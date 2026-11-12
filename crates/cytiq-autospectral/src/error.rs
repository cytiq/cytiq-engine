use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AutoSpectralError {
    #[error("failed to read controls manifest `{path}`: {source}")]
    ControlsManifestRead {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to read samples manifest `{path}`: {source}")]
    SamplesManifestRead {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to read detectors manifest `{path}`: {source}")]
    DetectorsManifestRead {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("controls manifest is empty")]
    EmptyControlsManifest,
    #[error("samples manifest is empty")]
    EmptySamplesManifest,
    #[error("detectors manifest is empty")]
    EmptyDetectorsManifest,
    #[error("{manifest} manifest row {row} has an empty `{field}`")]
    EmptyManifestField {
        manifest: &'static str,
        row: usize,
        field: &'static str,
    },
    #[error("{manifest} manifest row {row} must use a relative `filename`, got `{filename}`")]
    AbsoluteManifestFilename {
        manifest: &'static str,
        row: usize,
        filename: String,
    },
    #[error("duplicate `dye` value `{value}` in controls manifest")]
    DuplicateDye { value: String },
    #[error("duplicate detector channel `{value}` in detectors manifest")]
    DuplicateDetectorChannel { value: String },
    #[error("`{path}` was {state}; autospectral unmixes raw acquisition files")]
    ProcessedInput { path: PathBuf, state: String },
    #[error("`{value}` is the event lineage column, not a detector")]
    ReservedDetectorChannel { value: String },
    #[error("controls manifest must contain exactly one `dye=Autofluorescence` row")]
    MissingAutofluorescenceControl,
    #[error("controls manifest contains multiple `dye=Autofluorescence` rows")]
    DuplicateAutofluorescenceControl,
    #[error("controls manifest must contain at least one single-stain control")]
    NoSingleStainControls,
    #[error("FCS file `{path}` does not exist")]
    MissingFcsFile { path: PathBuf },
    #[error("failed to read FCS file `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("failed to write FCS file `{path}`: {source}")]
    FcsWrite {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("FSC/SSC gate of control `{path}` failed: {reason}")]
    DegenerateScatterGate { path: PathBuf, reason: &'static str },
    #[error("failed to write event mask: {0}")]
    Mask(#[from] cytiq_mask::MaskError),
    #[error("FCS file `{path}` has no events")]
    EmptyFcs { path: PathBuf },
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
    #[error("channel `{channel}` was not found in `{path}`")]
    MissingChannel { path: PathBuf, channel: String },
    #[error("detector channel `{channel}` was not found in `{path}`")]
    MissingDetectorChannel { path: PathBuf, channel: String },
    #[error("could not infer spectral detector channels")]
    DetectorInferenceFailed,
    #[error("detector set for `{path}` does not contain inferred channel `{channel}`")]
    DetectorMismatch { path: PathBuf, channel: String },
    #[error(
        "reference matrix needs at least as many detectors ({detectors}) as components ({components})"
    )]
    TooFewDetectors { detectors: usize, components: usize },
    #[error("control `{path}` retained {retained} events, below required minimum {minimum}")]
    TooFewRetainedEvents {
        path: PathBuf,
        retained: usize,
        minimum: usize,
    },
    #[error(
        "control `{path}` retained {positive} positive events, below required minimum {minimum}"
    )]
    TooFewPositiveEvents {
        path: PathBuf,
        positive: usize,
        minimum: usize,
    },
    #[error(
        "control `{path}` produced no reference spectrum: the slopes against its peak detector are zero or undefined (no spread on the peak detector)"
    )]
    ZeroSpectrum { path: PathBuf },
    #[error("reference matrix is singular")]
    SingularReferenceMatrix,
    #[error("reference matrix condition number {condition:.6e} exceeds limit {limit:.6e}")]
    IllConditionedReferenceMatrix { condition: f64, limit: f64 },
    #[error("sample `{path}` contains non-finite detector values")]
    NonFiniteSampleValues { path: PathBuf },
    #[error("failed to create output directory `{path}`: {source}")]
    OutputDirectoryCreate {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write CSV output `{path}`: {source}")]
    CsvWrite {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to flush CSV output `{path}`: {source}")]
    CsvFlush {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write JSON output `{path}`: {source}")]
    JsonWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize JSON output `{path}`: {source}")]
    JsonSerialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{0}")]
    Publish(#[from] cytiq_publish::PublishError),
}
