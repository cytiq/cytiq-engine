//! Long Arrow tables with exact `UInt64` counts and typed metadata.

use crate::observation::Lineage;
use crate::{
    Denominator, Measure, Method, ObservationStatus, PopulationError, PopulationObservation,
    Result, Target, TargetKind, require,
};
use arrow_schema::{DataType, Field, Schema};
pub use cytiq_mask::parquet::ParquetReadLimits;
use cytiq_mask::parquet::ScalarParquetReader;
use parquet::arrow::ArrowWriter;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs::File, path::Path, sync::Arc};

const COLUMNS: &[(&str, DataType, bool)] = &[
    ("schema_version", DataType::UInt64, false),
    ("observation_id", DataType::Utf8, true),
    ("specimen_id", DataType::Utf8, true),
    ("panel_id", DataType::Utf8, true),
    ("acquisition_id", DataType::Utf8, true),
    ("source_id", DataType::Utf8, true),
    ("target_id", DataType::Utf8, true),
    ("target_kind", DataType::Utf8, true),
    ("measure", DataType::Utf8, true),
    ("unit", DataType::Utf8, true),
    ("status", DataType::Utf8, true),
    ("reason", DataType::Utf8, true),
    ("target", DataType::Utf8, true),
    ("denominator", DataType::Utf8, true),
    ("method", DataType::Utf8, true),
    ("lineage", DataType::Utf8, true),
    ("value_u64", DataType::UInt64, true),
    ("n_events", DataType::UInt64, true),
    ("supporting_count", DataType::UInt64, true),
    ("value_f64", DataType::Float64, true),
];
pub const MAX_OBSERVATIONS: usize = 1_000_000;

/// # Errors
/// Rejects invalid observations and failed Arrow/filesystem writes.
pub fn write(path: &Path, observations: &[PopulationObservation]) -> Result<()> {
    require(
        observations.len() <= MAX_OBSERVATIONS,
        "too many observations",
    )?;
    let fields = COLUMNS
        .iter()
        .map(|(name, kind, nullable)| Field::new(*name, kind.clone(), *nullable))
        .collect::<Vec<_>>();
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        HashMap::from([("cytiq.population.schema_version".into(), "1".into())]),
    ));
    let mut writer = ArrowWriter::try_new(File::create(path)?, schema.clone(), None)?;
    for rows in observations.chunks(crate::DEFAULT_BATCH_SIZE) {
        for row in rows {
            row.validate()?;
        }
        let rows = rows
            .iter()
            .map(|observation| ObservationRow {
                observation,
                target_id: &observation.target.id,
                target_kind: observation.target.kind,
            })
            .collect::<Vec<_>>();
        let batch = serde_arrow::to_record_batch(schema.fields(), &rows)
            .map_err(|e| PopulationError::Invalid(e.to_string()))?;
        writer.write(&batch.with_schema(schema.clone())?)?;
    }
    writer.close()?;
    Ok(())
}

/// # Errors
/// Rejects missing/changed schemas, inconsistent identities and excessive rows.
pub fn read(path: &Path) -> Result<Vec<PopulationObservation>> {
    read_from(File::open(path)?)
}

/// [`read`] from any Parquet source, such as `bytes::Bytes` in memory.
/// # Errors
/// Rejects missing/changed schemas, inconsistent identities and excessive rows.
pub fn read_from<R: parquet::file::reader::ChunkReader + 'static>(
    reader: R,
) -> Result<Vec<PopulationObservation>> {
    read_from_with_limits(reader, ParquetReadLimits::default())
}

/// Read with caller-supplied resource budgets, still enforcing the observation
/// table's fixed maximum row count. Useful for restricted ingestion and fuzzing.
/// # Errors
/// Rejects invalid tables and inputs exceeding either budget or schema limits.
pub fn read_from_with_limits<R: parquet::file::reader::ChunkReader + 'static>(
    reader: R,
    mut limits: ParquetReadLimits,
) -> Result<Vec<PopulationObservation>> {
    limits.rows = limits.rows.min(MAX_OBSERVATIONS as u64);
    let builder = ScalarParquetReader::new(reader, limits)?;
    require(
        builder.schema().fields().len() == COLUMNS.len(),
        "unexpected observation columns",
    )?;
    require(
        builder
            .schema()
            .metadata()
            .get("cytiq.population.schema_version")
            .map(String::as_str)
            == Some("1"),
        "unsupported population table schema",
    )?;
    require(
        builder.metadata().file_metadata().num_rows() >= 0
            && u64::try_from(builder.metadata().file_metadata().num_rows()).unwrap_or(u64::MAX)
                <= MAX_OBSERVATIONS as u64,
        "too many observation rows",
    )?;
    let mut result = Vec::new();
    for batch in builder {
        let batch = batch?;
        // serde_arrow can coerce compatible numeric Arrow types. The v1 table
        // contract deliberately accepts only the original physical types.
        for (name, expected, _) in COLUMNS {
            require(
                batch.schema().field_with_name(name)?.data_type() == expected,
                format!("invalid observation column: {name}"),
            )?;
        }
        let rows: Vec<ObservationValue> = serde_arrow::from_record_batch(&batch)
            .map_err(|e| PopulationError::Invalid(e.to_string()))?;
        for ObservationValue {
            observation,
            target_id,
            target_kind,
        } in rows
        {
            observation.validate()?;
            require(
                target_id == observation.target.id && target_kind == observation.target.kind,
                "target columns disagree",
            )?;
            require(
                result.len() < MAX_OBSERVATIONS,
                "too many decoded observation rows",
            )?;
            result.push(observation);
        }
    }
    Ok(result)
}

// Remote derive keeps the typed observation fields while representing its four
// structured fields as the JSON strings required by the v1 Parquet contract.
#[derive(Serialize, Deserialize)]
#[serde(remote = "PopulationObservation")]
struct FlatObservation {
    schema_version: u32,
    observation_id: String,
    specimen_id: String,
    panel_id: String,
    acquisition_id: Option<String>,
    source_id: String,
    #[serde(with = "json_text")]
    target: Target,
    measure: Measure,
    unit: String,
    status: ObservationStatus,
    value_u64: Option<u64>,
    value_f64: Option<f64>,
    #[serde(with = "json_text")]
    denominator: Option<Denominator>,
    #[serde(with = "json_text")]
    method: Method,
    #[serde(with = "json_text")]
    lineage: Lineage,
    n_events: u64,
    /// Rejected/below-detection counts are evidence, never a usable value.
    supporting_count: Option<u64>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct ObservationRow<'a> {
    #[serde(flatten, with = "FlatObservation")]
    observation: &'a PopulationObservation,
    target_id: &'a str,
    target_kind: TargetKind,
}

#[derive(Deserialize)]
struct ObservationValue {
    #[serde(flatten, with = "FlatObservation")]
    observation: PopulationObservation,
    target_id: String,
    target_kind: TargetKind,
}

mod json_text {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
    pub fn serialize<T: Serialize, S: Serializer>(
        value: &T,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let json = serde_json::to_value(value)
            .and_then(|value| serde_json::to_string(&value))
            .map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&json)
    }
    pub fn deserialize<'de, T: DeserializeOwned, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<T, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        serde_json::from_str(&encoded).map_err(serde::de::Error::custom)
    }
}
