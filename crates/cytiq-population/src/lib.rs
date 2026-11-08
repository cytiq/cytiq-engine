//! Population contracts independent of specimen reconciliation and model inference.

pub mod annotations;
pub mod artifact;
pub mod decision;
pub mod knowledge;
pub mod observation;
pub mod serialization;
pub mod table;

pub use annotations::{AnnotationFormat, Binding, EventAnnotations, ObserveConfig, observe};
pub use artifact::{ArtifactRef, verify_package, write_package};
pub use observation::{
    Denominator, Measure, Method, ObservationStatus, PopulationObservation, Target, TargetKind,
};

/// Every public document in this crate has its own explicit schema version.
pub const SCHEMA_VERSION: u32 = 1;
/// Bound event batches independently of the acquisition's number of events.
pub const DEFAULT_BATCH_SIZE: usize = 65_536;

#[derive(Debug, thiserror::Error)]
pub enum PopulationError {
    #[error("{0}")]
    Invalid(String),
    #[error("required input is missing: {0}")]
    Missing(std::path::PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Arrow(#[from] arrow_schema::ArrowError),
    #[error(transparent)]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error(transparent)]
    Publish(#[from] cytiq_publish::PublishError),
    #[error(transparent)]
    Provenance(#[from] cytiq_provenance::ProvenanceError),
}

pub type Result<T> = std::result::Result<T, PopulationError>;

pub(crate) fn require(condition: bool, reason: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(PopulationError::Invalid(reason.into()))
    }
}
