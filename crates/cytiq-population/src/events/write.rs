//! Stream an FCS file into a `cytiq.events.v1` table with bounded memory.

use std::collections::HashMap;
use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, Float32Array, Float64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{Field, Schema, SchemaRef};
use cytiq_fcs::{ColumnData, EventColumns, FcsDataset, FcsReadLimits, FcsReader, Parameter};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

use super::{
    ACQUISITION_COLUMN, ACQUISITION_KEY, CREATED_BY, ColumnValues, EVENT_ID_BASE_KEY,
    ExportOptions, FIELD_KEY_PREFIX, LAYER_COLUMN, LAYER_ID_KEY, LAYER_KEY, LayerDescriptor,
    RAW_EVENTS_KEY, ROW_SET_KEY, SCHEMA_VERSION, SCHEMA_VERSION_KEY, SOURCE_EVENT_ID_COLUMN,
    VALUES_SHA256_KEY, ValuesHasher,
};
use crate::{PopulationError, Result, require};

/// Bytes of decoded events and keys held per batch, whatever the event
/// count; the Parquet encoder holds about as much again for the row group.
const BATCH_BYTES: usize = 16 << 20;
/// Most events per batch and row group.
const BATCH_ROWS_MAX: usize = 65_536;

/// What [`export`] wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSummary {
    pub descriptor: LayerDescriptor,
    pub layer_id: String,
    pub recipe_id: String,
    pub rows: u64,
    /// Raw event count when the rows are the raw rows; `None` for a subset.
    pub raw_events: Option<u64>,
    pub values_sha256: String,
}

/// Write `input`'s events and channels to the events table `output`.
///
/// Memory holds one batch of at most 16 MiB of decoded events. The input is
/// read twice (metadata, then events) and refused if it changes in between.
///
/// # Errors
/// Returns FCS read errors, descriptor refusals (see
/// [`LayerDescriptor::from_fcs`]), non-increasing `Original_ID`s and write
/// errors.
pub fn export(input: &Path, output: &Path, options: &ExportOptions) -> Result<ExportSummary> {
    let probe = FcsReader::open(input)?.stream(NonZeroUsize::MIN, FcsReadLimits::default())?;
    let metadata = probe.metadata;
    let empty = probe.batches.empty_batch();
    let acquisition = cytiq_fcs::acquisition_id(input, &metadata.text)?;
    let descriptor = LayerDescriptor::from_fcs(&metadata, &empty.columns, &acquisition, options)?;
    let layer_id = descriptor.layer_id();

    // Values, `source_event_id`, and both 64-character keys with offsets.
    let row_bytes = descriptor
        .channels
        .iter()
        .map(|channel| channel.value_type.bytes())
        .sum::<usize>()
        + 8
        + 2 * (64 + 4);
    let rows = (BATCH_BYTES / row_bytes).clamp(1, BATCH_ROWS_MAX);
    let stream = FcsReader::open(input)?.stream(
        NonZeroUsize::new(rows).unwrap_or(NonZeroUsize::MIN),
        FcsReadLimits::default(),
    )?;
    require(
        stream.metadata == metadata,
        "the input changed while it was exported",
    )?;
    let lineage = metadata
        .parameters
        .iter()
        .position(|p| p.name == cytiq_fcs::ORIGINAL_ID_PARAMETER);
    let row_set_all = lineage.is_none();
    let raw_events = row_set_all.then_some(metadata.event_count as u64);

    let schema = schema(&descriptor, &metadata.parameters);
    let properties = properties(&descriptor, &layer_id, raw_events, rows);
    let mut writer =
        ArrowWriter::try_new(File::create(output)?, Arc::clone(&schema), Some(properties))?;
    let mut hasher = ValuesHasher::new(&layer_id);
    let mut window = FcsDataset {
        version: metadata.version,
        header: metadata.header.clone(),
        text: metadata.text.clone(),
        parameters: metadata.parameters.clone(),
        data: EventColumns {
            columns: Vec::new(),
        },
        analysis: None,
    };
    let mut written = 0_u64;
    let mut previous: Option<u64> = None;
    for batch in stream.batches {
        window.data = batch?;
        let count = window.event_count();
        let ids = source_ids(&window, written, lineage)?;
        for id in &ids {
            require(
                previous.is_none_or(|last| *id > last),
                "`Original_ID` must increase strictly: events are out of raw order or repeated",
            )?;
            previous = Some(*id);
        }
        let columns: Vec<ColumnData> = std::mem::take(&mut window.data.columns)
            .into_iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != lineage)
            .map(|(_, column)| column)
            .collect();
        let values: Vec<ColumnValues<'_>> = columns.iter().map(borrow).collect();
        hasher.update(&ids, &values);
        let mut arrays: Vec<ArrayRef> = Vec::with_capacity(columns.len() + 3);
        arrays.push(Arc::new(StringArray::from_iter_values(
            std::iter::repeat_n(acquisition.as_str(), count),
        )));
        arrays.push(Arc::new(StringArray::from_iter_values(
            std::iter::repeat_n(layer_id.as_str(), count),
        )));
        arrays.push(Arc::new(UInt64Array::from(ids)));
        arrays.extend(columns.into_iter().map(array));
        writer.write(&RecordBatch::try_new(Arc::clone(&schema), arrays)?)?;
        written += count as u64;
    }
    require(
        written == metadata.event_count as u64,
        "the input's events disagree with its `$TOT`",
    )?;
    let values_sha256 = hasher.finish();
    writer.append_key_value_metadata(KeyValue::new(
        VALUES_SHA256_KEY.to_owned(),
        values_sha256.clone(),
    ));
    writer.close()?;
    Ok(ExportSummary {
        recipe_id: descriptor.recipe_id(),
        descriptor,
        layer_id,
        rows: written,
        raw_events,
        values_sha256,
    })
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

fn borrow(column: &ColumnData) -> ColumnValues<'_> {
    match column {
        ColumnData::UInt(values) => ColumnValues::Uint64(values),
        ColumnData::F32(values) => ColumnValues::Float32(values),
        ColumnData::F64(values) => ColumnValues::Float64(values),
    }
}

fn array(column: ColumnData) -> ArrayRef {
    match column {
        ColumnData::UInt(values) => Arc::new(UInt64Array::from(values)),
        ColumnData::F32(values) => Arc::new(Float32Array::from(values)),
        ColumnData::F64(values) => Arc::new(Float64Array::from(values)),
    }
}

/// `$Pn` keywords recorded per channel, by field metadata key suffix.
const CHANNEL_KEYWORDS: &[(&str, &str)] = &[
    ("label", "S"),
    ("analyte", "ANALYTE"),
    ("reporter", "TAG"),
    ("detector", "DET"),
    ("feature", "FEATURE"),
    ("excitation", "L"),
    ("filter", "F"),
    ("voltage", "V"),
    ("gain", "G"),
    ("range", "R"),
    ("amplification", "E"),
    ("bits", "B"),
];

fn parameter_keyword<'a>(parameter: &'a Parameter, suffix: &str) -> Option<&'a str> {
    let key = format!("$P{}{suffix}", parameter.index);
    parameter
        .keywords
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&key))
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.trim().is_empty())
}

fn field_metadata(
    descriptor: &LayerDescriptor,
    parameter: &Parameter,
    value_type: &str,
) -> HashMap<String, String> {
    let key = |suffix: &str| format!("{FIELD_KEY_PREFIX}{suffix}");
    let mut metadata = HashMap::from([
        (key("index"), parameter.index.to_string()),
        (key("name"), parameter.name.clone()),
        (key("value_type"), value_type.to_owned()),
    ]);
    for (suffix, keyword) in CHANNEL_KEYWORDS {
        if let Some(value) = parameter_keyword(parameter, keyword) {
            metadata.insert(key(suffix), value.to_owned());
        }
    }
    let unmixed = descriptor.steps.iter().any(|step| {
        step.kind == super::StepKind::Unmixing && step.channels.contains(&parameter.name)
    });
    let kind = if unmixed {
        "unmixed_component".to_owned()
    } else {
        parameter_keyword(parameter, "TYPE").map_or_else(|| "unspecified".to_owned(), str::to_owned)
    };
    metadata.insert(key("kind"), kind);
    let steps = descriptor.steps_of(&parameter.name);
    if !steps.is_empty() {
        let names: Vec<&str> = steps.iter().map(|kind| kind.as_str()).collect();
        metadata.insert(key("steps"), names.join(","));
    }
    metadata
}

fn schema(descriptor: &LayerDescriptor, parameters: &[Parameter]) -> SchemaRef {
    let mut fields = vec![
        Field::new(ACQUISITION_COLUMN, arrow_schema::DataType::Utf8, false),
        Field::new(LAYER_COLUMN, arrow_schema::DataType::Utf8, false),
        Field::new(
            SOURCE_EVENT_ID_COLUMN,
            arrow_schema::DataType::UInt64,
            false,
        ),
    ];
    for channel in &descriptor.channels {
        let parameter = parameters
            .iter()
            .find(|p| p.index == channel.index)
            .expect("descriptor channels come from the parameters");
        fields.push(
            Field::new(&channel.name, channel.value_type.arrow(), false).with_metadata(
                field_metadata(descriptor, parameter, channel.value_type.as_str()),
            ),
        );
    }
    Arc::new(Schema::new(fields))
}

fn properties(
    descriptor: &LayerDescriptor,
    layer_id: &str,
    raw_events: Option<u64>,
    rows: usize,
) -> WriterProperties {
    let mut metadata = vec![
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
        KeyValue::new(
            ROW_SET_KEY.to_owned(),
            if raw_events.is_some() {
                "all"
            } else {
                "subset"
            }
            .to_owned(),
        ),
    ];
    if let Some(raw_events) = raw_events {
        metadata.push(KeyValue::new(
            RAW_EVENTS_KEY.to_owned(),
            raw_events.to_string(),
        ));
    }
    WriterProperties::builder()
        .set_created_by(CREATED_BY.to_owned())
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_max_row_group_row_count(Some(rows))
        .set_dictionary_enabled(false)
        .set_column_dictionary_enabled(ColumnPath::from(ACQUISITION_COLUMN), true)
        .set_column_dictionary_enabled(ColumnPath::from(LAYER_COLUMN), true)
        .set_key_value_metadata(Some(metadata))
        .build()
}
