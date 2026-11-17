//! Readers of the native per-event outputs a label table is made from.
//!
//! Every source streams its native rows, which must name raw events in
//! strictly increasing order, into [`Rows`]; `Rows` gives the events a native
//! file omits their state and hands full batches to the writer.

pub(crate) mod classify;
pub(crate) mod flowsom;
pub(crate) mod mask;

use std::sync::Arc;

use arrow_array::ArrayRef;
use arrow_array::builder::{BooleanBuilder, Float32Builder, StringBuilder, UInt64Builder};

use super::{LabelBatch, LabelState, LabelType, LabelWriter};
use crate::bitmap::Bitmap;
use crate::{DEFAULT_BATCH_SIZE, PopulationError, Result, require};

/// One label column being built.
pub(crate) enum Builder {
    Boolean(BooleanBuilder),
    UInt64(UInt64Builder),
    Float32(Float32Builder),
    Utf8(StringBuilder),
}

// The sources build each column with the type the descriptor gives it; a
// mismatch is a bug in this module, not a property of the input.
impl Builder {
    fn new(kind: LabelType) -> Self {
        match kind {
            LabelType::Boolean => Self::Boolean(BooleanBuilder::with_capacity(DEFAULT_BATCH_SIZE)),
            LabelType::UInt64 => Self::UInt64(UInt64Builder::with_capacity(DEFAULT_BATCH_SIZE)),
            LabelType::Float32 => Self::Float32(Float32Builder::with_capacity(DEFAULT_BATCH_SIZE)),
            LabelType::Utf8 => Self::Utf8(StringBuilder::new()),
        }
    }

    fn null(&mut self) {
        match self {
            Self::Boolean(builder) => builder.append_null(),
            Self::UInt64(builder) => builder.append_null(),
            Self::Float32(builder) => builder.append_null(),
            Self::Utf8(builder) => builder.append_null(),
        }
    }

    pub(crate) fn boolean(&mut self, value: Option<bool>) {
        match self {
            Self::Boolean(builder) => builder.append_option(value),
            _ => unreachable!("a Boolean label column"),
        }
    }

    pub(crate) fn uint(&mut self, value: Option<u64>) {
        match self {
            Self::UInt64(builder) => builder.append_option(value),
            _ => unreachable!("a UInt64 label column"),
        }
    }

    pub(crate) fn float(&mut self, value: Option<f32>) {
        match self {
            Self::Float32(builder) => builder.append_option(value),
            _ => unreachable!("a Float32 label column"),
        }
    }

    pub(crate) fn text(&mut self, value: Option<&str>) {
        match self {
            Self::Utf8(builder) => builder.append_option(value),
            _ => unreachable!("a Utf8 label column"),
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Self::Boolean(builder) => Arc::new(builder.finish()),
            Self::UInt64(builder) => Arc::new(builder.finish()),
            Self::Float32(builder) => Arc::new(builder.finish()),
            Self::Utf8(builder) => Arc::new(builder.finish()),
        }
    }
}

/// The rows of a label table, every raw event in order.
pub(crate) struct Rows<'a> {
    writer: &'a mut LabelWriter,
    builders: Vec<Builder>,
    states: Vec<LabelState>,
    /// The first event of the batch being built.
    first: u64,
    /// The next raw event a row is due for.
    next: u64,
    raw_events: u64,
    /// Combined `keep` of the verified QC masks the producer read.
    eligible: Option<&'a Bitmap>,
}

impl<'a> Rows<'a> {
    pub(crate) fn new(
        writer: &'a mut LabelWriter,
        columns: &[(String, LabelType)],
        raw_events: u64,
        eligible: Option<&'a Bitmap>,
    ) -> Self {
        Self {
            writer,
            builders: columns
                .iter()
                .map(|(_, kind)| Builder::new(*kind))
                .collect(),
            states: Vec::with_capacity(DEFAULT_BATCH_SIZE),
            first: 0,
            next: 0,
            raw_events,
            eligible,
        }
    }

    /// Move to the native row of raw event `id`: every event skipped before
    /// it gets the state of an omitted event.
    ///
    /// # Errors
    /// Refuses ids out of increasing order or beyond the acquisition, and an
    /// omitted event the QC masks keep.
    pub(crate) fn at(&mut self, id: u64) -> Result<()> {
        require(
            id >= self.next && id < self.raw_events,
            format!(
                "native rows must name raw events in strictly increasing order within the \
                 acquisition's {} events (event {id})",
                self.raw_events
            ),
        )?;
        self.omit_until(id)
    }

    /// The state of an event the producer excluded or omitted: QC-rejected
    /// when verified masks exclude it, refused when they keep it.
    pub(crate) fn excluded(&self, id: u64) -> Result<LabelState> {
        match self.eligible {
            None => Ok(LabelState::NotAssessed),
            Some(eligible) => {
                require(
                    !keeps(eligible, id),
                    format!("the producer left out event {id}, which the QC masks keep"),
                )?;
                Ok(LabelState::QcRejected)
            }
        }
    }

    /// Refuse an event the producer assessed although the QC masks exclude
    /// it.
    pub(crate) fn assessed(&self, id: u64) -> Result<()> {
        require(
            self.eligible.is_none_or(|eligible| keeps(eligible, id)),
            format!("the producer assessed event {id}, which the QC masks exclude"),
        )
    }

    fn omitted(&self, id: u64) -> Result<LabelState> {
        self.excluded(id).map_err(|_| {
            PopulationError::Invalid(format!(
                "the native file omits event {id}, which the QC masks keep"
            ))
        })
    }

    /// Append the row of the next raw event.
    pub(crate) fn push(
        &mut self,
        state: LabelState,
        cells: impl FnOnce(&mut [Builder]),
    ) -> Result<()> {
        self.states.push(state);
        cells(&mut self.builders);
        self.next += 1;
        if self.states.len() == DEFAULT_BATCH_SIZE {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.states.is_empty() {
            return Ok(());
        }
        let batch = LabelBatch {
            first: self.first,
            states: std::mem::take(&mut self.states),
            columns: self.builders.iter_mut().map(Builder::finish).collect(),
        };
        self.first = self.next;
        self.writer.write(batch)
    }

    /// Give every remaining event its omitted state and write the rest.
    pub(crate) fn finish(mut self) -> Result<()> {
        self.omit_until(self.raw_events)?;
        self.flush()
    }

    /// Rows for the events before `end` the native file omits.
    fn omit_until(&mut self, end: u64) -> Result<()> {
        while self.next < end {
            let state = self.omitted(self.next)?;
            self.push(state, |builders| {
                builders.iter_mut().for_each(Builder::null);
            })?;
        }
        Ok(())
    }
}

fn keeps(eligible: &Bitmap, id: u64) -> bool {
    usize::try_from(id).is_ok_and(|id| id < eligible.len() && eligible.get(id))
}
