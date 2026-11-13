//! Verify a `cytiq.events.v1` table through the bounded Parquet reader.

use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

use arrow_array::{Array, Float32Array, Float64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use cytiq_mask::parquet::{ParquetReadLimits, ScalarParquetReader};
use parquet::file::reader::ChunkReader;

use super::{
    ACQUISITION_COLUMN, ACQUISITION_KEY, ColumnValues, EVENT_ID_BASE_KEY, FIELD_KEY_PREFIX,
    LAYER_COLUMN, LAYER_CONTRACT, LAYER_ID_KEY, LAYER_KEY, LayerDescriptor, RAW_EVENTS_KEY,
    ROW_SET_KEY, SCHEMA_VERSION, SCHEMA_VERSION_KEY, SOURCE_EVENT_ID_COLUMN, VALUES_SHA256_KEY,
    ValueType, ValuesHasher, channel_name_problem,
};
use crate::issue::{IssueCode, Issues};
use crate::serialization::{self, DocumentFormat};

/// What a verified events table holds.
#[derive(Clone, Debug, PartialEq)]
pub struct EventsSummary {
    pub descriptor: LayerDescriptor,
    pub layer_id: String,
    pub rows: u64,
    /// Raw event count when the rows are every raw event in order.
    pub raw_events: Option<u64>,
    /// Largest `source_event_id`; `None` for an empty table.
    pub max_source_event_id: Option<u64>,
    pub values_sha256: String,
}

/// Verify the events table at `path`; issues point at `at`.
pub fn verify_file(path: &Path, at: &str, issues: &mut Issues) -> Option<EventsSummary> {
    match File::open(path) {
        Ok(file) => verify(file, at, issues),
        Err(error) => {
            issues.error(
                IssueCode::MissingObject,
                at,
                format!("cannot open events table: {error}"),
            );
            None
        }
    }
}

/// Verify an events table: version, descriptor digest, key and channel
/// columns, row identities and the values digest. Returns the summary when
/// the table has no error.
pub fn verify<R: ChunkReader + 'static>(
    reader: R,
    at: &str,
    issues: &mut Issues,
) -> Option<EventsSummary> {
    verify_with_limits(reader, ParquetReadLimits::default(), at, issues)
}

/// [`verify`] within explicit read limits.
pub fn verify_with_limits<R: ChunkReader + 'static>(
    reader: R,
    limits: ParquetReadLimits,
    at: &str,
    issues: &mut Issues,
) -> Option<EventsSummary> {
    let errors = issues.errors();
    let reader = match ScalarParquetReader::new(reader, limits) {
        Ok(reader) => reader,
        Err(error) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!("unreadable events table: {error}"),
            );
            return None;
        }
    };
    let schema = reader.schema().clone();
    let header = header(&schema, at, issues)?;
    check_fields(&schema, &header.descriptor, at, issues);
    let rows = u64::try_from(reader.metadata().file_metadata().num_rows()).unwrap_or(0);
    if let Some(raw_events) = header.raw_events
        && raw_events != rows
    {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            format!("{rows} rows, but `{RAW_EVENTS_KEY}` is {raw_events}"),
        );
    }
    if issues.errors() > errors {
        return None;
    }
    let scan = scan(reader, &header, at, issues)?;
    if scan.rows != rows {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            "decoded rows disagree with the footer",
        );
    }
    if header.values_sha256.as_deref() != Some(scan.values_sha256.as_str()) {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            "decoded values disagree with `cytiq.events.values_sha256`",
        );
    }
    (issues.errors() == errors).then_some(EventsSummary {
        layer_id: header.layer_id,
        descriptor: header.descriptor,
        rows,
        raw_events: header.raw_events,
        max_source_event_id: scan.max_source_event_id,
        values_sha256: scan.values_sha256,
    })
}

struct Header {
    descriptor: LayerDescriptor,
    layer_id: String,
    raw_events: Option<u64>,
    values_sha256: Option<String>,
}

fn header(schema: &Schema, at: &str, issues: &mut Issues) -> Option<Header> {
    let metadata = schema.metadata();
    match metadata.get(SCHEMA_VERSION_KEY).map(String::as_str) {
        Some(version) if version == SCHEMA_VERSION.to_string() => {}
        Some(version) => {
            issues.error(
                IssueCode::UnsupportedSchema,
                at,
                format!("events schema version {version} is not supported"),
            );
            return None;
        }
        None => {
            issues.error(
                IssueCode::UnsupportedSchema,
                at,
                format!("not a cytiq.events table: `{SCHEMA_VERSION_KEY}` is missing"),
            );
            return None;
        }
    }
    let corrupt = |issues: &mut Issues, message: String| {
        issues.error(IssueCode::CorruptArtifactHash, at, message);
    };
    let Some(text) = metadata.get(LAYER_KEY) else {
        corrupt(issues, format!("`{LAYER_KEY}` is missing"));
        return None;
    };
    let descriptor = match serialization::parse(text.as_bytes(), DocumentFormat::Json)
        .and_then(|value| Ok(serde_json::from_value::<LayerDescriptor>(value)?))
    {
        Ok(descriptor) => descriptor,
        Err(error) => {
            corrupt(issues, format!("malformed layer descriptor: {error}"));
            return None;
        }
    };
    if descriptor.contract != LAYER_CONTRACT {
        issues.error(
            IssueCode::UnsupportedSchema,
            at,
            format!("layer contract `{}` is not supported", descriptor.contract),
        );
        return None;
    }
    let layer_id = descriptor.layer_id();
    if metadata.get(LAYER_ID_KEY) != Some(&layer_id) {
        corrupt(
            issues,
            "the layer descriptor does not match `cytiq.events.layer_id`".into(),
        );
    }
    if metadata.get(ACQUISITION_KEY) != Some(&descriptor.acquisition_id) {
        issues.error(
            IssueCode::InvalidReference,
            at,
            "`cytiq.acquisition_sha256` differs from the layer's acquisition",
        );
    }
    if metadata.get(EVENT_ID_BASE_KEY).map(String::as_str) != Some("0") {
        corrupt(issues, format!("`{EVENT_ID_BASE_KEY}` must be 0"));
    }
    let raw_events = match (
        metadata.get(ROW_SET_KEY).map(String::as_str),
        metadata.get(RAW_EVENTS_KEY),
    ) {
        (Some("all"), Some(count)) => {
            if let Ok(count) = count.parse::<u64>() {
                Some(count)
            } else {
                corrupt(issues, format!("`{RAW_EVENTS_KEY}` is not a count"));
                None
            }
        }
        (Some("subset"), None) => None,
        _ => {
            corrupt(
                issues,
                format!("`{ROW_SET_KEY}` must be `all` with `{RAW_EVENTS_KEY}`, or `subset`"),
            );
            None
        }
    };
    Some(Header {
        descriptor,
        layer_id,
        raw_events,
        values_sha256: metadata.get(VALUES_SHA256_KEY).cloned(),
    })
}

fn check_key(field: &Field, name: &str, data_type: &DataType, at: &str, issues: &mut Issues) {
    if field.name() != name || field.data_type() != data_type || field.is_nullable() {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            format!(
                "key column `{}` must be a non-null {data_type} named `{name}`",
                field.name()
            ),
        );
    }
}

fn check_fields(schema: &Schema, descriptor: &LayerDescriptor, at: &str, issues: &mut Issues) {
    let fields = schema.fields();
    if fields.len() != descriptor.channels.len() + 3 {
        issues.error(
            IssueCode::InconsistentChannelOrder,
            at,
            format!(
                "{} columns, but the layer has {} channels and 3 keys",
                fields.len(),
                descriptor.channels.len()
            ),
        );
        return;
    }
    check_key(&fields[0], ACQUISITION_COLUMN, &DataType::Utf8, at, issues);
    check_key(&fields[1], LAYER_COLUMN, &DataType::Utf8, at, issues);
    check_key(
        &fields[2],
        SOURCE_EVENT_ID_COLUMN,
        &DataType::UInt64,
        at,
        issues,
    );
    let mut previous = 0_u16;
    let mut names = HashSet::new();
    for (field, channel) in fields[3..].iter().zip(&descriptor.channels) {
        if channel.index <= previous || !names.insert(channel.name.to_ascii_lowercase()) {
            issues.error(
                IssueCode::InconsistentChannelOrder,
                at,
                format!(
                    "channel `{}` is out of `$Pn` order or repeated",
                    channel.name
                ),
            );
        }
        previous = channel.index;
        if let Some(problem) = channel_name_problem(&channel.name) {
            issues.error(
                IssueCode::InconsistentChannelOrder,
                at,
                format!("{problem}: `{}`", channel.name),
            );
        }
        if field.name() != &channel.name {
            issues.error(
                IssueCode::InconsistentChannelOrder,
                at,
                format!(
                    "column `{}` where the layer has channel `{}`",
                    field.name(),
                    channel.name
                ),
            );
            continue;
        }
        if ValueType::of_arrow(field.data_type()) != Some(channel.value_type) || field.is_nullable()
        {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!(
                    "channel `{}` is not a non-null {}",
                    channel.name,
                    channel.value_type.as_str()
                ),
            );
        }
        let metadata = field.metadata();
        let index = metadata.get(&format!("{FIELD_KEY_PREFIX}index"));
        let name = metadata.get(&format!("{FIELD_KEY_PREFIX}name"));
        if index.is_some_and(|index| *index != channel.index.to_string())
            || name.is_some_and(|name| *name != channel.name)
        {
            issues.error(
                IssueCode::InconsistentChannelOrder,
                at,
                format!(
                    "field metadata of `{}` disagrees with the layer descriptor",
                    channel.name
                ),
            );
        }
    }
}

struct Scan {
    rows: u64,
    max_source_event_id: Option<u64>,
    values_sha256: String,
}

/// Row-level problems are reported once per kind, not once per row.
#[derive(Default)]
struct Reported {
    acquisition: bool,
    layer: bool,
    order: bool,
}

fn scan<R: ChunkReader + 'static>(
    reader: ScalarParquetReader<R>,
    header: &Header,
    at: &str,
    issues: &mut Issues,
) -> Option<Scan> {
    let mut hasher = ValuesHasher::new(&header.layer_id);
    let mut rows = 0_u64;
    let mut previous: Option<u64> = None;
    let mut reported = Reported::default();
    for batch in reader {
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => {
                issues.error(
                    IssueCode::CorruptArtifactHash,
                    at,
                    format!("unreadable events batch: {error}"),
                );
                return None;
            }
        };
        let Some((acquisitions, layers, ids)) = keys(&batch) else {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                "key columns are not decodable",
            );
            return None;
        };
        if !reported.acquisition
            && (0..batch.num_rows())
                .any(|i| acquisitions.value(i) != header.descriptor.acquisition_id)
        {
            reported.acquisition = true;
            issues.error(
                IssueCode::InvalidReference,
                at,
                "rows carry another acquisition than the layer's",
            );
        }
        if !reported.layer && (0..batch.num_rows()).any(|i| layers.value(i) != header.layer_id) {
            reported.layer = true;
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                "rows carry another layer id than the table's",
            );
        }
        for (offset, id) in ids.values().iter().enumerate() {
            let ordered = previous.is_none_or(|last| *id > last)
                && (header.raw_events.is_none() || *id == rows + offset as u64);
            if !ordered && !reported.order {
                reported.order = true;
                issues.error(
                    IssueCode::DuplicateEventIdentity,
                    at,
                    "`source_event_id` is not strictly increasing (or not the raw row)",
                );
            }
            previous = Some(*id);
        }
        let Some(columns) = channel_values(&batch) else {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                "channel columns are not decodable",
            );
            return None;
        };
        hasher.update(ids.values(), &columns);
        rows += batch.num_rows() as u64;
    }
    Some(Scan {
        rows,
        max_source_event_id: previous,
        values_sha256: hasher.finish(),
    })
}

fn keys(batch: &RecordBatch) -> Option<(&StringArray, &StringArray, &UInt64Array)> {
    let acquisitions = batch.column(0).as_any().downcast_ref::<StringArray>()?;
    let layers = batch.column(1).as_any().downcast_ref::<StringArray>()?;
    let ids = batch.column(2).as_any().downcast_ref::<UInt64Array>()?;
    (acquisitions.null_count() == 0 && layers.null_count() == 0 && ids.null_count() == 0)
        .then_some((acquisitions, layers, ids))
}

fn channel_values(batch: &RecordBatch) -> Option<Vec<ColumnValues<'_>>> {
    batch.columns()[3..]
        .iter()
        .map(|column| {
            if column.null_count() != 0 {
                return None;
            }
            let any = column.as_any();
            if let Some(values) = any.downcast_ref::<UInt64Array>() {
                Some(ColumnValues::Uint64(values.values()))
            } else if let Some(values) = any.downcast_ref::<Float32Array>() {
                Some(ColumnValues::Float32(values.values()))
            } else {
                any.downcast_ref::<Float64Array>()
                    .map(|values| ColumnValues::Float64(values.values()))
            }
        })
        .collect()
}
