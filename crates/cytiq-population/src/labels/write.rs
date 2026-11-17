//! Write a label table batch by batch, then read it back through the
//! bounded verifier before anyone may publish it.

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use cytiq_mask::parquet::ParquetReadLimits;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

use super::digest::LabelsHasher;
use super::read::{self, LabelsSummary};
use super::{
    ACQUISITION_COLUMN, ACQUISITION_KEY, ANNOTATION_COLUMN, ANNOTATION_ID_KEY, CREATED_BY,
    DEFINITION_ID_KEY, DESCRIPTOR_KEY, EVENT_ID_BASE_KEY, KIND_KEY, LabelDescriptor, LabelState,
    LabelType, RAW_EVENTS_KEY, SCHEMA_VERSION, SCHEMA_VERSION_KEY, SOURCE_EVENT_ID_COLUMN,
    STATE_COLUMN, VALUES_SHA256_KEY,
};
use crate::events::{Codec, DEFAULT_ROW_GROUP_ROWS, ROW_GROUP_BYTES_MAX, TableShape, check_table};
use crate::issue::Issues;
use crate::{PopulationError, Result, require};

/// Decoded bytes of both key strings per row: 64 characters and an offset.
const KEY_BYTES: u64 = 2 * (64 + 4);
/// Decoded bytes of `source_event_id` and a `state` of at most 12 bytes.
const ROW_BYTES: u64 = 8 + 4 + 12;
/// Longest `abstain_reason`.
const REASON_BYTES: u64 = 22;

/// How a label table is written. No setting changes a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LabelSettings {
    /// Most events per row group; a row group is also flushed at
    /// [`ROW_GROUP_BYTES_MAX`] encoded bytes.
    pub row_group_rows: NonZeroUsize,
    pub codec: Codec,
}

impl Default for LabelSettings {
    fn default() -> Self {
        Self {
            row_group_rows: NonZeroUsize::new(DEFAULT_ROW_GROUP_ROWS).unwrap_or(NonZeroUsize::MIN),
            codec: Codec::default(),
        }
    }
}

/// Consecutive rows of a label table: events `first..first + states.len()`.
#[derive(Clone, Debug)]
pub struct LabelBatch {
    pub first: u64,
    pub states: Vec<LabelState>,
    /// One array per column after `state`, in table order
    /// ([`LabelDescriptor::columns`]).
    pub columns: Vec<ArrayRef>,
}

/// What [`LabelWriter::finish`] wrote and verified.
#[derive(Clone, Debug, PartialEq)]
pub struct WrittenLabels {
    pub annotation_id: String,
    pub definition_id: Option<String>,
    pub rows: u64,
    pub values_sha256: String,
    pub summary: LabelsSummary,
}

/// A label table being written to `path`.
pub struct LabelWriter {
    path: PathBuf,
    writer: ArrowWriter<File>,
    schema: SchemaRef,
    descriptor: LabelDescriptor,
    annotation_id: String,
    columns: Vec<(String, LabelType)>,
    hasher: LabelsHasher,
    next: u64,
    limits: ParquetReadLimits,
}

impl LabelWriter {
    /// Create the table for `descriptor`'s labels.
    ///
    /// # Errors
    /// Refuses an invalid descriptor, a table the bounded verifier could
    /// not read within `limits`, invalid settings and I/O errors.
    pub fn create(
        path: &Path,
        descriptor: LabelDescriptor,
        settings: &LabelSettings,
        limits: ParquetReadLimits,
    ) -> Result<Self> {
        descriptor.validate()?;
        let columns = descriptor.columns();
        let descriptor_json = serde_json::to_string(&descriptor)?;
        check_table(
            shape(
                &descriptor,
                &columns,
                settings,
                descriptor_json.len() as u64,
            ),
            &limits,
            "the acquisition has too many events or labels for one label table",
        )?;
        let annotation_id = descriptor.annotation_id();
        let schema = schema(&columns);
        let mut metadata = vec![
            KeyValue::new(SCHEMA_VERSION_KEY.to_owned(), SCHEMA_VERSION.to_string()),
            KeyValue::new(KIND_KEY.to_owned(), descriptor.kind.as_str().to_owned()),
            KeyValue::new(DESCRIPTOR_KEY.to_owned(), descriptor_json),
            KeyValue::new(ANNOTATION_ID_KEY.to_owned(), annotation_id.clone()),
            KeyValue::new(
                ACQUISITION_KEY.to_owned(),
                descriptor.acquisition_id.clone(),
            ),
            KeyValue::new(EVENT_ID_BASE_KEY.to_owned(), "0".to_owned()),
            KeyValue::new(RAW_EVENTS_KEY.to_owned(), descriptor.raw_events.to_string()),
        ];
        if let Some(definition_id) = descriptor.definition_id() {
            metadata.push(KeyValue::new(DEFINITION_ID_KEY.to_owned(), definition_id));
        }
        let mut properties = WriterProperties::builder()
            .set_created_by(CREATED_BY.to_owned())
            .set_compression(settings.codec.parquet()?)
            .set_max_row_group_row_count(Some(settings.row_group_rows.get()))
            .set_max_row_group_bytes(Some(ROW_GROUP_BYTES_MAX))
            .set_dictionary_enabled(false)
            .set_key_value_metadata(Some(metadata));
        // Strings repeat a few values: dictionary pages (plain-encoded) and
        // RLE indices, both of which the bounded reader decodes. Nothing
        // else needs a dictionary.
        for field in schema.fields() {
            if field.data_type() == &DataType::Utf8 {
                properties = properties
                    .set_column_dictionary_enabled(ColumnPath::from(field.name().as_str()), true);
            }
        }
        let writer = ArrowWriter::try_new(
            File::create(path)?,
            Arc::clone(&schema),
            Some(properties.build()),
        )?;
        Ok(Self {
            path: path.to_path_buf(),
            writer,
            schema,
            hasher: LabelsHasher::new(&annotation_id),
            annotation_id,
            columns,
            descriptor,
            next: 0,
            limits,
        })
    }

    /// Append the next rows.
    ///
    /// # Errors
    /// Refuses rows out of raw order, beyond the acquisition, or columns of
    /// another shape than the descriptor's.
    pub fn write(&mut self, batch: LabelBatch) -> Result<()> {
        let count = batch.states.len();
        require(
            batch.first == self.next,
            "label rows must be every raw event in raw order",
        )?;
        let end = self.next.saturating_add(count as u64);
        require(
            end <= self.descriptor.raw_events,
            "more label rows than the acquisition has events",
        )?;
        require(
            batch.columns.len() == self.columns.len()
                && batch
                    .columns
                    .iter()
                    .zip(&self.columns)
                    .all(|(array, (_, kind))| {
                        array.len() == count && array.data_type() == &kind.arrow()
                    }),
            "label columns disagree with the descriptor",
        )?;
        if count == 0 {
            return Ok(());
        }
        let ids = UInt64Array::from_iter_values(self.next..end);
        let states = StringArray::from_iter_values(batch.states.iter().map(|s| s.as_str()));
        self.hasher
            .update(&ids, &states, &batch.columns)
            .ok_or_else(|| PopulationError::Invalid("label columns are not hashable".into()))?;
        let mut arrays: Vec<ArrayRef> = Vec::with_capacity(batch.columns.len() + 4);
        arrays.push(Arc::new(StringArray::from_iter_values(
            std::iter::repeat_n(self.descriptor.acquisition_id.as_str(), count),
        )));
        arrays.push(Arc::new(StringArray::from_iter_values(
            std::iter::repeat_n(self.annotation_id.as_str(), count),
        )));
        arrays.push(Arc::new(ids));
        arrays.push(Arc::new(states));
        arrays.extend(batch.columns);
        self.writer
            .write(&RecordBatch::try_new(Arc::clone(&self.schema), arrays)?)?;
        self.next = end;
        Ok(())
    }

    /// Close the table and read it back through the bounded verifier: it
    /// must hold exactly the rows written.
    ///
    /// # Errors
    /// Refuses a table missing raw events, one that does not verify, and
    /// I/O errors.
    pub fn finish(mut self) -> Result<WrittenLabels> {
        require(
            self.next == self.descriptor.raw_events,
            format!(
                "{} label rows for an acquisition of {} events",
                self.next, self.descriptor.raw_events
            ),
        )?;
        let values_sha256 = self.hasher.finish();
        self.writer.append_key_value_metadata(KeyValue::new(
            VALUES_SHA256_KEY.to_owned(),
            values_sha256.clone(),
        ));
        self.writer.close()?;
        let mut issues = Issues::new();
        let summary =
            read::verify_with_limits(File::open(&self.path)?, self.limits, "", &mut issues);
        let Some(summary) = summary else {
            let reason = issues
                .items()
                .first()
                .map_or("no reason given", |issue| issue.message.as_str());
            return Err(PopulationError::Invalid(format!(
                "the written label table does not verify: {reason}"
            )));
        };
        require(
            summary.rows == self.next
                && summary.annotation_id == self.annotation_id
                && summary.values_sha256 == values_sha256,
            "the written label table reads back other rows than were written",
        )?;
        Ok(WrittenLabels {
            annotation_id: self.annotation_id,
            definition_id: summary.definition_id.clone(),
            rows: self.next,
            values_sha256,
            summary,
        })
    }
}

fn schema(columns: &[(String, LabelType)]) -> SchemaRef {
    let mut fields = vec![
        Field::new(ACQUISITION_COLUMN, DataType::Utf8, false),
        Field::new(ANNOTATION_COLUMN, DataType::Utf8, false),
        Field::new(SOURCE_EVENT_ID_COLUMN, DataType::UInt64, false),
        Field::new(STATE_COLUMN, DataType::Utf8, false),
    ];
    fields.extend(
        columns
            .iter()
            .map(|(name, kind)| Field::new(name, kind.arrow(), true)),
    );
    Arc::new(Schema::new(fields))
}

/// The table's size as the bounded verifier will see it.
fn shape(
    descriptor: &LabelDescriptor,
    columns: &[(String, LabelType)],
    settings: &LabelSettings,
    descriptor_bytes: u64,
) -> TableShape {
    let class_bytes = descriptor
        .labels
        .iter()
        .filter_map(|label| label.class.as_ref())
        .map(|class| class.len() as u64)
        .max()
        .unwrap_or(0);
    let mut value_bytes = 8 + 3 * 4;
    let mut decoded_row = KEY_BYTES + ROW_BYTES;
    let mut dictionaries = 3;
    for (name, kind) in columns {
        let (stored, decoded) = match kind {
            LabelType::Boolean => (1, 1),
            LabelType::UInt64 => (8, 9),
            LabelType::Float32 => (4, 5),
            LabelType::Utf8 => {
                dictionaries += 1;
                let text = if name == "abstain_reason" {
                    REASON_BYTES
                } else {
                    class_bytes
                };
                (4 + text, 5 + text)
            }
        };
        value_bytes += stored;
        decoded_row += decoded;
    }
    TableShape {
        rows: descriptor.raw_events,
        columns: (columns.len() + 4) as u64,
        dictionaries,
        value_bytes,
        decoded_row,
        row_group_rows: settings.row_group_rows.get() as u64,
        // The schema repeats the descriptor once more in the Arrow schema.
        metadata_bytes: 2 * descriptor_bytes + 4096,
    }
}
