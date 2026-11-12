use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AutospillError {
    #[error("failed to read controls manifest `{path}`: {source}")]
    ManifestRead {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to read detectors manifest `{path}`: {source}")]
    DetectorRead {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to read transform manifest `{path}`: {source}")]
    TransformRead {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("controls manifest is empty")]
    EmptyManifest,
    #[error("detectors manifest is empty")]
    EmptyDetectorsManifest,
    #[error("manifest row {row} has an empty `{field}`")]
    EmptyManifestField { row: usize, field: &'static str },
    #[error("detector row {row} has an empty `{field}`")]
    EmptyDetectorField { row: usize, field: &'static str },
    #[error("transform row {row} has an empty `{field}`")]
    EmptyTransformField { row: usize, field: &'static str },
    #[error("manifest row {row} must use a relative `filename`, got `{filename}`")]
    AbsoluteManifestFilename { row: usize, filename: String },
    #[error("duplicate `dye` value `{value}` in controls manifest")]
    DuplicateDye { value: String },
    #[error("duplicate inferred `primary_channel` value `{value}` across controls")]
    DuplicatePrimaryChannel { value: String },
    #[error("duplicate detector channel `{value}` in detectors manifest")]
    DuplicateDetectorChannel { value: String },
    #[error("duplicate transform channel `{value}` in transform manifest")]
    DuplicateTransformChannel { value: String },
    #[error("control file `{path}` does not exist")]
    MissingControlFile { path: PathBuf },
    #[error("failed to read FCS control `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("control `{path}` has no events")]
    EmptyControl { path: PathBuf },
    #[error("control `{path}` was {state}; single-stain controls must be raw acquisition files")]
    ProcessedControl { path: PathBuf, state: String },
    #[error(
        "could not infer `primary_channel` for dye `{dye}` in `{path}`; available channels: {available_channels:?}"
    )]
    PrimaryChannelNotFound {
        path: PathBuf,
        dye: String,
        available_channels: Vec<String>,
    },
    #[error(
        "dye `{dye}` in `{path}` matched multiple channels while inferring `primary_channel`: {matches:?}"
    )]
    AmbiguousPrimaryChannel {
        path: PathBuf,
        dye: String,
        matches: Vec<String>,
    },
    #[error(
        "autofluorescence control `{path}` requires `--detectors` so a synthetic `primary_channel` can be inferred"
    )]
    AutofluorescenceRequiresDetectors { path: PathBuf },
    #[error(
        "autofluorescence control `{path}` requires exactly one synthetic detector absent from the FCS metadata, found {candidates:?}"
    )]
    AutofluorescenceDetectorResolution {
        path: PathBuf,
        candidates: Vec<String>,
    },
    #[error("channel `{channel}` was not found in `{path}`")]
    MissingChannel { path: PathBuf, channel: String },
    #[error("detector channel `{channel}` was not declared")]
    MissingDetector { channel: String },
    #[error("transform parameters for `{channel}` were not found")]
    MissingTransform { channel: String },
    #[error("control `{path}` produced a degenerate gate: {reason}")]
    DegenerateGate { path: PathBuf, reason: &'static str },
    #[error(
        "robust regression failed for control `{path}` between `{primary_channel}` and `{secondary_channel}`"
    )]
    RegressionFailed {
        path: PathBuf,
        primary_channel: String,
        secondary_channel: String,
    },
    #[error("spillover matrix became singular during refinement")]
    SingularMatrix,
    #[error("rectangular unmixing matrix is ill-conditioned")]
    IllConditionedPseudoInverse,
    #[error("autospill did not converge after {iterations} iterations (delta.max={delta_max:e})")]
    NonConvergent { iterations: usize, delta_max: f64 },
    #[error("autospread could not estimate a spreading matrix")]
    AutospreadFailed,
    #[error("debug output could not be created at `{path}`: {source}")]
    DebugOutputCreate {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write debug artifact `{path}`: {source}")]
    DebugWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("matrix export format for `{path}` is not supported")]
    UnsupportedOutputFormat { path: PathBuf },
    #[error("failed to write matrix to `{path}`: {source}")]
    MatrixWrite {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to create output file `{path}`: {source}")]
    CreateOutputFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write parquet output `{path}`: {source}")]
    ParquetWrite {
        path: PathBuf,
        #[source]
        source: parquet::errors::ParquetError,
    },
    #[error("failed to build parquet batch for `{path}`: {message}")]
    ParquetBatch { path: PathBuf, message: String },
    #[error("{0}")]
    Publish(#[from] cytiq_publish::PublishError),
}
