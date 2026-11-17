//! Stream an FCS file into `cytiq.events.v1` tables with bounded memory.

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{
    Array, ArrayRef, Float32Array, Float64Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::{Field, Schema, SchemaRef};
use cytiq_fcs::{
    ACQUISITION_KEYWORD, ColumnData, EventColumns, FcsDataset, FcsMetadata, FcsReadLimits,
    FcsReader, ORIGINAL_ID_PARAMETER, SignalState, Text as FcsText,
};
use cytiq_mask::parquet::ParquetReadLimits;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

use super::channels::{self, ChannelEntry};
use super::parts::{PARTS_CONTRACT, PARTS_FILE, PartEntry, PartsManifest, RowSet, part_name};
use super::read;
use super::settings::{Plan, ROW_GROUP_BYTES_MAX, WriteSettings};
use super::{
    ACQUISITION_COLUMN, ACQUISITION_KEY, CHANNELS_KEY, COLUMN_SET_KEY, CREATED_BY, ColumnValues,
    EVENT_ID_BASE_KEY, ExportOptions, LAYER_COLUMN, LAYER_ID_KEY, LAYER_KEY, LayerDescriptor,
    RAW_EVENTS_KEY, ROW_SET_KEY, SCHEMA_VERSION, SCHEMA_VERSION_KEY, SOURCE_EVENT_ID_COLUMN,
    SOURCE_SHA256_KEY, VALUES_SHA256_KEY, ValuesHasher,
};
use crate::issue::Issues;
use crate::{PopulationError, Result, require};

/// What [`export`] wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSummary {
    pub descriptor: LayerDescriptor,
    pub layer_id: String,
    pub recipe_id: String,
    pub rows: u64,
    /// Raw event count when the rows are the raw rows; `None` for a subset.
    pub raw_events: Option<u64>,
    /// Digest of the whole layer's rows, whatever the parts.
    pub values_sha256: String,
    /// SHA-256 of the exported FCS file.
    pub source_sha256: String,
    /// The stored channels, in column order.
    pub channels: Vec<ChannelEntry>,
    /// The parts manifest when the layer was split, as written to
    /// [`PARTS_FILE`] in the output directory.
    pub parts: Option<PartsManifest>,
}

/// Write `input`'s events and channels to the events table `output`, with
/// the default [`WriteSettings`].
///
/// # Errors
/// As [`export_with`].
pub fn export(input: &Path, output: &Path, options: &ExportOptions) -> Result<ExportSummary> {
    export_with(
        input,
        output,
        options,
        &WriteSettings::default(),
        ParquetReadLimits::default(),
    )
}

/// Write `input`'s events to the events table `output`, or, with
/// [`WriteSettings::max_file_bytes`], to part tables and a parts manifest
/// in the existing empty directory `output`.
///
/// Memory holds one batch of decoded events, the row group being encoded
/// and one batch read back. Every table is read back through the bounded
/// reader within `limits` and must hold exactly the exported values. The
/// input is hashed before the export and again after it, and refused if its
/// TEXT or its bytes changed in between.
///
/// # Errors
/// Returns FCS read errors, descriptor refusals (see
/// [`LayerDescriptor::from_fcs`]), a layer other than
/// [`ExportOptions::layer`], a subset without
/// [`ExportOptions::allow_subset`], settings refusals, non-increasing
/// `Original_ID`s, write errors and a table that does not verify.
///
/// # Panics
/// Never: stored channels come from the file's parameters.
pub fn export_with(
    input: &Path,
    output: &Path,
    options: &ExportOptions,
    settings: &WriteSettings,
    limits: ParquetReadLimits,
) -> Result<ExportSummary> {
    let source_sha256 = cytiq_provenance::hash_file(input)?.1;
    let layout = Layout::describe(input, options, settings, &limits, &source_sha256)?;
    let mut sink = Sink::new(
        output,
        Target {
            plan: &layout.plan,
            footer: &layout.footer,
            schema: schema(&layout.channels),
            layer_id: &layout.layer_id,
            acquisition: &layout.acquisition,
            raw_events: layout.raw_events,
            events: layout.events,
            limits,
        },
    )?;
    let written = layout.stream(input, &mut sink)?;
    // A modification time can stay the same through a rewrite (coarse
    // clocks, network shares), so the bytes themselves are compared.
    require(
        cytiq_provenance::hash_file(input)?.1 == source_sha256,
        "the input changed while it was exported",
    )?;
    let (values_sha256, parts) = sink.finish()?;
    Ok(ExportSummary {
        recipe_id: layout.descriptor.recipe_id(),
        descriptor: layout.descriptor,
        layer_id: layout.layer_id,
        rows: written,
        raw_events: layout.raw_events,
        values_sha256,
        source_sha256,
        channels: layout.channels,
        parts,
    })
}

/// What an export writes, decided before any event is read.
struct Layout {
    metadata: FcsMetadata,
    acquisition: String,
    descriptor: LayerDescriptor,
    layer_id: String,
    /// Column of `Original_ID`, when the rows are a subset.
    lineage: Option<usize>,
    /// The stored channels, and the batch column of each.
    channels: Vec<ChannelEntry>,
    columns: Vec<usize>,
    events: u64,
    raw_events: Option<u64>,
    footer: Footer,
    plan: Plan,
}

impl Layout {
    /// Read the file's metadata, describe its layer and check it against
    /// the options, then plan the tables against `limits`.
    fn describe(
        input: &Path,
        options: &ExportOptions,
        settings: &WriteSettings,
        limits: &ParquetReadLimits,
        source_sha256: &str,
    ) -> Result<Self> {
        let probe = FcsReader::open(input)?.stream(NonZeroUsize::MIN, FcsReadLimits::default())?;
        let metadata = probe.metadata;
        let empty = probe.batches.empty_batch();
        let acquisition = acquisition(input, &metadata.text, source_sha256)?;
        let descriptor =
            LayerDescriptor::from_fcs(&metadata, &empty.columns, &acquisition, options)?;
        require(
            descriptor.state == options.layer,
            format!(
                "the file holds a {} layer, not a {} one",
                descriptor.state.as_str(),
                options.layer.as_str()
            ),
        )?;
        let lineage = metadata
            .parameters
            .iter()
            .position(|p| p.name == ORIGINAL_ID_PARAMETER);
        require(
            lineage.is_none() || options.allow_subset,
            "the file's rows are a subset of the acquisition (it has `Original_ID`): export the \
             unfiltered layer and keep the filter as a mask, or allow a subset explicitly",
        )?;
        let layer_id = descriptor.layer_id();
        let stored = settings.select(&descriptor)?;
        let channels = channels::dictionary(&descriptor, &metadata.parameters, &stored);
        let events = metadata.event_count as u64;
        let footer = Footer::new(
            &descriptor,
            &layer_id,
            &channels,
            stored.len() == descriptor.channels.len(),
            source_sha256,
        );
        let plan = settings.plan(&descriptor, &stored, events, footer.bytes, limits)?;
        let columns = channels
            .iter()
            .map(|channel| {
                metadata
                    .parameters
                    .iter()
                    .position(|p| p.index == channel.index)
                    .expect("stored channels come from the parameters")
            })
            .collect();
        Ok(Self {
            raw_events: lineage.is_none().then_some(events),
            metadata,
            acquisition,
            descriptor,
            layer_id,
            lineage,
            channels,
            columns,
            events,
            footer,
            plan,
        })
    }

    /// Decode the events batch by batch into `sink`; the number written.
    fn stream(&self, input: &Path, sink: &mut Sink<'_>) -> Result<u64> {
        let stream =
            FcsReader::open(input)?.stream(self.plan.batch_rows, FcsReadLimits::default())?;
        require(
            stream.metadata == self.metadata,
            "the input changed while it was exported",
        )?;
        let mut window = FcsDataset {
            version: self.metadata.version,
            header: self.metadata.header.clone(),
            text: self.metadata.text.clone(),
            parameters: self.metadata.parameters.clone(),
            data: EventColumns {
                columns: Vec::new(),
            },
            analysis: None,
        };
        let mut written = 0_u64;
        let mut previous: Option<u64> = None;
        for batch in stream.batches {
            window.data = batch?;
            let ids = source_ids(&window, written, self.lineage)?;
            for id in &ids {
                require(
                    previous.is_none_or(|last| *id > last),
                    "`Original_ID` must increase strictly: events are out of raw order or \
                     repeated",
                )?;
                previous = Some(*id);
            }
            let mut decoded: Vec<Option<ColumnData>> = std::mem::take(&mut window.data.columns)
                .into_iter()
                .map(Some)
                .collect();
            let arrays: Vec<ArrayRef> = self
                .columns
                .iter()
                .map(|&column| array(decoded[column].take().expect("each column is stored once")))
                .collect();
            written += ids.len() as u64;
            sink.write(&UInt64Array::from(ids), &arrays)?;
        }
        require(
            written == self.events,
            "the input's events disagree with its `$TOT`",
        )?;
        Ok(written)
    }
}

/// The acquisition of a file: its recorded `CYTIQ_ACQUISITION`, else the
/// file itself, unless a cytiq stage processed it without recording one.
fn acquisition(input: &Path, text: &FcsText, source_sha256: &str) -> Result<String> {
    if text.get(ACQUISITION_KEYWORD).is_some() {
        return Ok(cytiq_fcs::acquisition_id(input, text)?);
    }
    let state = SignalState::of(text)?;
    require(
        state.compensation.is_none()
            && state.unmixing.is_none()
            && state.transform.is_none()
            && state.normalization.is_none(),
        format!(
            "the file records cytiq processing without `{ACQUISITION_KEYWORD}`: its raw \
             acquisition is unknown"
        ),
    )?;
    Ok(source_sha256.to_owned())
}

/// Raw rows of a batch: `Original_ID` when the file has it, else the rows.
fn source_ids(window: &FcsDataset, start: u64, lineage: Option<usize>) -> Result<Vec<u64>> {
    let count = window.event_count() as u64;
    match lineage {
        None => Ok((start..start + count).collect()),
        Some(_) => cytiq_fcs::source_event_ids(window)?
            .ok_or_else(|| PopulationError::Invalid("`Original_ID` column vanished".into())),
    }
}

fn array(column: ColumnData) -> ArrayRef {
    match column {
        ColumnData::UInt(values) => Arc::new(UInt64Array::from(values)),
        ColumnData::F32(values) => Arc::new(Float32Array::from(values)),
        ColumnData::F64(values) => Arc::new(Float64Array::from(values)),
    }
}

/// Borrow channel arrays as hashable values.
fn values(arrays: &[ArrayRef]) -> Vec<ColumnValues<'_>> {
    arrays
        .iter()
        .map(|array| {
            let any = array.as_any();
            if let Some(values) = any.downcast_ref::<UInt64Array>() {
                ColumnValues::Uint64(values.values())
            } else if let Some(values) = any.downcast_ref::<Float32Array>() {
                ColumnValues::Float32(values.values())
            } else {
                ColumnValues::Float64(
                    any.downcast_ref::<Float64Array>()
                        .expect("channel arrays are UInt64, Float32 or Float64")
                        .values(),
                )
            }
        })
        .collect()
}

fn schema(channels: &[ChannelEntry]) -> SchemaRef {
    let mut fields = vec![
        Field::new(ACQUISITION_COLUMN, arrow_schema::DataType::Utf8, false),
        Field::new(LAYER_COLUMN, arrow_schema::DataType::Utf8, false),
        Field::new(
            SOURCE_EVENT_ID_COLUMN,
            arrow_schema::DataType::UInt64,
            false,
        ),
    ];
    fields.extend(channels.iter().map(|channel| {
        Field::new(&channel.name, channel.value_type.arrow(), false)
            .with_metadata(channel.field_metadata())
    }));
    Arc::new(Schema::new(fields))
}

/// Footer key-value metadata every table of a layer shares.
struct Footer {
    common: Vec<KeyValue>,
    /// Estimate of the footer's key-value bytes, including the Arrow schema
    /// that repeats the channel dictionary as field metadata.
    bytes: u64,
}

impl Footer {
    fn new(
        descriptor: &LayerDescriptor,
        layer_id: &str,
        channels: &[ChannelEntry],
        all_columns: bool,
        source_sha256: &str,
    ) -> Self {
        let dictionary = serde_json::to_string(channels).expect("channel dictionary serializes");
        let dictionary_bytes = dictionary.len() as u64;
        let common = vec![
            KeyValue::new(SCHEMA_VERSION_KEY.to_owned(), SCHEMA_VERSION.to_string()),
            KeyValue::new(
                ACQUISITION_KEY.to_owned(),
                descriptor.acquisition_id.clone(),
            ),
            KeyValue::new(EVENT_ID_BASE_KEY.to_owned(), "0".to_owned()),
            KeyValue::new(
                LAYER_KEY.to_owned(),
                serde_json::to_string(descriptor).expect("descriptor serializes"),
            ),
            KeyValue::new(LAYER_ID_KEY.to_owned(), layer_id.to_owned()),
            KeyValue::new(CHANNELS_KEY.to_owned(), dictionary),
            KeyValue::new(
                COLUMN_SET_KEY.to_owned(),
                if all_columns { "all" } else { "subset" }.to_owned(),
            ),
            KeyValue::new(SOURCE_SHA256_KEY.to_owned(), source_sha256.to_owned()),
        ];
        let text: u64 = common
            .iter()
            .map(|kv| (kv.key.len() + kv.value.as_ref().map_or(0, String::len)) as u64)
            .sum();
        Self {
            common,
            bytes: text + 3 * dictionary_bytes + 4096,
        }
    }

    fn properties(&self, plan: &Plan, raw_events: Option<u64>) -> WriterProperties {
        let mut metadata = self.common.clone();
        metadata.push(KeyValue::new(
            ROW_SET_KEY.to_owned(),
            if raw_events.is_some() {
                "all"
            } else {
                "subset"
            }
            .to_owned(),
        ));
        if let Some(raw_events) = raw_events {
            metadata.push(KeyValue::new(
                RAW_EVENTS_KEY.to_owned(),
                raw_events.to_string(),
            ));
        }
        WriterProperties::builder()
            .set_created_by(CREATED_BY.to_owned())
            .set_compression(plan.compression)
            .set_max_row_group_row_count(Some(plan.row_group_rows.get()))
            .set_max_row_group_bytes(Some(ROW_GROUP_BYTES_MAX))
            .set_dictionary_enabled(false)
            .set_column_dictionary_enabled(ColumnPath::from(ACQUISITION_COLUMN), true)
            .set_column_dictionary_enabled(ColumnPath::from(LAYER_COLUMN), true)
            .set_key_value_metadata(Some(metadata))
            .build()
    }
}

/// What every table of one export shares.
struct Target<'a> {
    plan: &'a Plan,
    footer: &'a Footer,
    schema: SchemaRef,
    layer_id: &'a str,
    acquisition: &'a str,
    raw_events: Option<u64>,
    events: u64,
    limits: ParquetReadLimits,
}

/// One table being written.
struct Table {
    path: PathBuf,
    writer: ArrowWriter<File>,
    hasher: ValuesHasher,
    /// Rows the table may still take.
    room: u64,
    rows: u64,
    first: Option<u64>,
    last: Option<u64>,
}

/// A written and verified table.
struct Written {
    path: PathBuf,
    rows: u64,
    first: Option<u64>,
    last: Option<u64>,
    values_sha256: String,
}

impl Table {
    fn create(
        path: PathBuf,
        target: &Target<'_>,
        room: u64,
        raw_events: Option<u64>,
    ) -> Result<Self> {
        let properties = target.footer.properties(target.plan, raw_events);
        let writer = ArrowWriter::try_new(
            File::create(&path)?,
            Arc::clone(&target.schema),
            Some(properties),
        )?;
        Ok(Self {
            path,
            writer,
            hasher: ValuesHasher::new(target.layer_id),
            room,
            rows: 0,
            first: None,
            last: None,
        })
    }

    fn write(
        &mut self,
        target: &Target<'_>,
        ids: &UInt64Array,
        channels: &[ArrayRef],
    ) -> Result<()> {
        let count = ids.len();
        if count == 0 {
            return Ok(());
        }
        self.hasher.update(ids.values(), &values(channels));
        let mut arrays: Vec<ArrayRef> = Vec::with_capacity(channels.len() + 3);
        arrays.push(Arc::new(StringArray::from_iter_values(
            std::iter::repeat_n(target.acquisition, count),
        )));
        arrays.push(Arc::new(StringArray::from_iter_values(
            std::iter::repeat_n(target.layer_id, count),
        )));
        arrays.push(Arc::new(ids.clone()));
        arrays.extend(channels.iter().cloned());
        self.writer
            .write(&RecordBatch::try_new(Arc::clone(&target.schema), arrays)?)?;
        self.first = self.first.or(Some(ids.value(0)));
        self.last = Some(ids.value(count - 1));
        self.rows += count as u64;
        self.room -= count as u64;
        Ok(())
    }

    /// Close the table and read it back through the bounded reader: it must
    /// hold exactly the rows and values written.
    fn finish(mut self, target: &Target<'_>) -> Result<Written> {
        let values_sha256 = self.hasher.finish();
        self.writer.append_key_value_metadata(KeyValue::new(
            VALUES_SHA256_KEY.to_owned(),
            values_sha256.clone(),
        ));
        self.writer.close()?;
        let mut issues = Issues::new();
        let verified =
            read::verify_with_limits(File::open(&self.path)?, target.limits, "", &mut issues);
        match verified {
            Some(summary)
                if summary.rows == self.rows
                    && summary.layer_id == target.layer_id
                    && summary.values_sha256 == values_sha256 => {}
            Some(_) => {
                return Err(PopulationError::Invalid(
                    "the written table reads back other rows than were exported".into(),
                ));
            }
            None => {
                let reason = issues
                    .items()
                    .first()
                    .map_or("no reason given", |issue| issue.message.as_str());
                return Err(PopulationError::Invalid(format!(
                    "the written table does not verify: {reason}"
                )));
            }
        }
        Ok(Written {
            path: self.path,
            rows: self.rows,
            first: self.first,
            last: self.last,
            values_sha256,
        })
    }
}

/// Where batches go: one table, or parts rolled at the planned row count.
struct Sink<'a> {
    target: Target<'a>,
    /// The table, or the parts directory when split.
    output: &'a Path,
    /// The parts directory, when split.
    directory: Option<&'a Path>,
    parts_total: u64,
    table: Option<Table>,
    written: Vec<Written>,
    /// Digest of the whole layer across parts.
    layer: Option<ValuesHasher>,
}

impl<'a> Sink<'a> {
    fn new(output: &'a Path, target: Target<'a>) -> Result<Self> {
        let (directory, parts_total) = match target.plan.part_rows {
            None => (None, 1),
            Some(rows) => {
                require(output.is_dir(), "the parts directory does not exist")?;
                require(
                    std::fs::read_dir(output)?.next().is_none(),
                    "the parts directory is not empty",
                )?;
                (Some(output), target.events.div_ceil(rows.get()).max(1))
            }
        };
        let layer = directory.map(|_| ValuesHasher::new(target.layer_id));
        let mut sink = Self {
            target,
            output,
            directory,
            parts_total,
            table: None,
            written: Vec::new(),
            layer,
        };
        // Even an empty layer has one table.
        sink.table = Some(sink.open()?);
        Ok(sink)
    }

    fn open(&self) -> Result<Table> {
        let index = self.written.len() as u64;
        let (path, room) = match (self.directory, self.target.plan.part_rows) {
            (Some(directory), Some(rows)) => (directory.join(part_name(index)), rows.get()),
            _ => (self.output.to_path_buf(), u64::MAX),
        };
        // A part is a whole layer only when it is the only one.
        let raw_events = self.target.raw_events.filter(|_| self.parts_total == 1);
        Table::create(path, &self.target, room, raw_events)
    }

    fn write(&mut self, ids: &UInt64Array, channels: &[ArrayRef]) -> Result<()> {
        if let Some(layer) = &mut self.layer {
            layer.update(ids.values(), &values(channels));
        }
        let count = ids.len();
        let mut offset = 0;
        while offset < count {
            let mut table = match self.table.take() {
                Some(table) => table,
                None => self.open()?,
            };
            let take = usize::try_from(table.room)
                .unwrap_or(usize::MAX)
                .min(count - offset);
            let slices: Vec<ArrayRef> = channels.iter().map(|a| a.slice(offset, take)).collect();
            table.write(&self.target, &ids.slice(offset, take), &slices)?;
            offset += take;
            if table.room == 0 {
                self.written.push(table.finish(&self.target)?);
            } else {
                self.table = Some(table);
            }
        }
        Ok(())
    }

    /// Close the last table; with parts, hash each and write the manifest.
    fn finish(mut self) -> Result<(String, Option<PartsManifest>)> {
        if let Some(table) = self.table.take() {
            self.written.push(table.finish(&self.target)?);
        }
        let Some(directory) = self.directory else {
            let table = self.written.pop().expect("a single table was written");
            return Ok((table.values_sha256, None));
        };
        let values_sha256 = self
            .layer
            .take()
            .expect("split layers have a layer digest")
            .finish();
        let mut parts = Vec::with_capacity(self.written.len());
        for (index, table) in self.written.iter().enumerate() {
            let (bytes, sha256) = cytiq_provenance::hash_file(&table.path)?;
            parts.push(PartEntry {
                location: part_name(index as u64),
                sha256,
                bytes,
                rows: table.rows,
                first_source_event_id: table.first,
                last_source_event_id: table.last,
                values_sha256: table.values_sha256.clone(),
            });
        }
        let manifest = PartsManifest {
            contract: PARTS_CONTRACT.to_owned(),
            acquisition_id: self.target.acquisition.to_owned(),
            layer_id: self.target.layer_id.to_owned(),
            row_set: if self.target.raw_events.is_some() {
                RowSet::All
            } else {
                RowSet::Subset
            },
            raw_events: self.target.raw_events,
            rows: parts.iter().map(|part| part.rows).sum(),
            values_sha256: values_sha256.clone(),
            parts,
        };
        manifest.write(&directory.join(PARTS_FILE))?;
        Ok((values_sha256, Some(manifest)))
    }
}
