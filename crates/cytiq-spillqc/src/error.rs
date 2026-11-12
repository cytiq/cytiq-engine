use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpillQcError {
    #[error("at least one FCS input is required")]
    EmptyInputs,
    #[error("FCS input `{path}` does not exist")]
    MissingInput { path: PathBuf },
    #[error("input `{path}` is not an `.fcs` file")]
    NonFcsInput { path: PathBuf },
    #[error("input directory `{path}` holds no `.fcs` files")]
    EmptyDirectory { path: PathBuf },
    #[error("glob `{pattern}` matched no `.fcs` files")]
    EmptyGlob { pattern: String },
    #[error("glob `{pattern}` is not supported; only the file name may hold `*` or `?`")]
    UnsupportedGlob { pattern: String },
    #[error("failed to read directory `{path}`: {source}")]
    DirectoryRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to resolve input `{path}`: {source}")]
    InputCanonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("input `{path}` is listed more than once")]
    DuplicateInput { path: PathBuf },
    #[error("inputs `{first}` and `{second}` share the output name `{stem}`")]
    DuplicateStem {
        stem: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("invalid `{name}`: {reason}")]
    InvalidParameter { name: &'static str, reason: String },
    #[error("output `{path}` already exists; pass --overwrite to replace it")]
    OutputExists { path: PathBuf },
    #[error("output `{path}` exists and is not a directory")]
    OutputNotDirectory { path: PathBuf },
    #[error("failed to read FCS input `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error(
        "FCS input `{path}` was already compensated by `cytiq compensate` (channels: {channels:?}); pass the raw FCS, or --preprocessed to assess the stored values as they are"
    )]
    AlreadyCompensated {
        path: PathBuf,
        channels: Vec<String>,
    },
    #[error(
        "`{path}` holds spectrally unmixed abundances ({components:?}); spillqc assesses compensated conventional data"
    )]
    UnmixedInput {
        path: PathBuf,
        components: Vec<String>,
    },
    #[error(
        "channel `{channel}` in `{path}` was transformed by `cytiq transform`; pass the raw FCS, or --preprocessed"
    )]
    TransformedInput { path: PathBuf, channel: String },
    #[error(transparent)]
    Matrix(#[from] cytiq_compensate::CompensateError),
    #[error("failed to write `{path}`: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write CSV `{path}`: {source}")]
    Csv {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to serialize `{path}`: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to render report `{path}`: {reason}")]
    Report { path: PathBuf, reason: String },
    #[error("{0}")]
    Publish(#[from] cytiq_publish::PublishError),
    /// The run completed and wrote its report, but flagged pairs.
    #[error("spillqc flagged {pairs} pair(s); review {}", report.display())]
    Flagged { pairs: usize, report: PathBuf },
}

impl From<cytiq_fcs::inputs::InputError> for SpillQcError {
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
