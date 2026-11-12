use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CytoScanError {
    #[error(
        "FCS input `{path}` has preprocessing/channel mapping incompatible with `{reference}`; use one effective scale and panel for the cohort"
    )]
    IncompatiblePreprocessing { path: PathBuf, reference: PathBuf },
    #[error("at least one `--input` value is required")]
    EmptyInputs,
    #[error(
        "FCS input `{path}` has no spillover metadata (--strict); CytoScan would score its raw, uncompensated values"
    )]
    MissingSpillover { path: PathBuf },
    #[error("selected channel `{channel}` of `{path}` is not in its spillover matrix (--strict)")]
    ChannelNotCompensated { path: PathBuf, channel: String },
    #[error(
        "FCS input `{path}` was {state}; CytoScan compensates and transforms raw files itself, so pass the raw FCS"
    )]
    ProcessedInput { path: PathBuf, state: String },
    #[error("input path `{path}` does not exist")]
    MissingInput { path: PathBuf },
    #[error("input path `{path}` is not an FCS file")]
    NonFcsInput { path: PathBuf },
    #[error("glob pattern `{pattern}` matched no FCS files")]
    EmptyGlob { pattern: String },
    #[error(
        "glob pattern `{pattern}` is not supported; wildcard characters are only supported in the file name"
    )]
    UnsupportedGlob { pattern: String },
    #[error("failed to read directory `{path}`: {source}")]
    DirectoryRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("directory input `{path}` contains no `.fcs` files")]
    EmptyDirectory { path: PathBuf },
    #[error("failed to resolve input file `{path}`: {source}")]
    InputCanonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("input file `{path}` was provided more than once")]
    DuplicateInput { path: PathBuf },
    #[error("`--channels` must include at least one channel")]
    EmptyChannels,
    #[error("channel list must contain at least one channel")]
    EmptyChannelList,
    #[error("duplicate channel `{channel}`")]
    DuplicateChannel { channel: String },
    #[error(
        "channel `{channel}` was not found in `{path}`; available channels: {available_channels:?}"
    )]
    MissingChannel {
        path: PathBuf,
        channel: String,
        available_channels: Vec<String>,
    },
    #[error("channel `{channel}` in `{path}` contains non-finite values")]
    NonFiniteChannel { path: PathBuf, channel: String },
    #[error("failed to read FCS input `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("FCS input `{path}` has no events")]
    EmptyFcs { path: PathBuf },
    #[error("`--reference` is required when `--flag-method` is `{flag_method}`")]
    MissingReference { flag_method: &'static str },
    #[error("reference input was provided but no reference files were resolved")]
    EmptyReference,
    #[error("failed to parse spillover metadata in `{path}`: {reason}")]
    InvalidSpillover { path: PathBuf, reason: String },
    #[error("spillover matrix in `{path}` is singular")]
    SingularSpillover { path: PathBuf },
    #[error("feature matrix is empty")]
    EmptyFeatureMatrix,
    #[error("feature method `{method}` requires at least one aggregate event")]
    EmptyAggregate { method: &'static str },
    #[error("feature matrices have incompatible feature columns")]
    FeatureShapeMismatch,
    #[error(
        "fingerprint bin {bin} at level {level} has {events} event(s); the aggregate is too small for {recursions} recursions"
    )]
    FingerprintBinTooSmall {
        level: usize,
        bin: usize,
        events: usize,
        recursions: usize,
    },
    #[error("novelty model fitting failed: {reason}")]
    NoveltyFit { reason: String },
    #[error("failed to create output directory `{path}`: {source}")]
    OutputDirectoryCreate {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create debug output directory `{path}`: {source}")]
    DebugDirectoryCreate {
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
    #[error("failed to write HTML debug output `{path}`: {source}")]
    HtmlWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Publish(#[from] cytiq_publish::PublishError),
}

impl From<cytiq_fcs::inputs::InputError> for CytoScanError {
    fn from(error: cytiq_fcs::inputs::InputError) -> Self {
        use cytiq_fcs::inputs::InputError as E;
        match error {
            E::EmptyInputs => Self::EmptyInputs,
            E::MissingInput { path } => Self::MissingInput { path },
            E::NonFcsInput { path } => Self::NonFcsInput { path },
            E::EmptyDirectory { path } => Self::EmptyDirectory { path },
            E::EmptyGlob { pattern } => Self::EmptyGlob { pattern },
            E::UnsupportedGlob { pattern } => Self::UnsupportedGlob { pattern },
            E::DirectoryRead { path, source } => Self::DirectoryRead { path, source },
            E::InputCanonicalize { path, source } => Self::InputCanonicalize { path, source },
            E::DuplicateInput { path } => Self::DuplicateInput { path },
        }
    }
}
