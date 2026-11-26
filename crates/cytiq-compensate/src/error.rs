use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CompensateError {
    #[error("FCS input file `{path}` does not exist")]
    MissingInput { path: PathBuf },
    #[error("output `{path}` must be an `.fcs` file")]
    UnsupportedOutputFormat { path: PathBuf },
    #[error("output `{path}` already exists; pass --overwrite to replace it")]
    OutputExists { path: PathBuf },
    #[error("failed to read FCS input `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("failed to write FCS output `{path}`: {source}")]
    FcsWrite {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("FCS input `{path}` has no events")]
    EmptyInput { path: PathBuf },
    #[error(
        "FCS input `{path}` has no `$SPILLOVER`/`$SPILL` keyword; pass --matrix with --matrix-kind"
    )]
    MissingSpillover { path: PathBuf },
    #[error(
        "FCS input `{path}` was already compensated by `cytiq compensate` (channels: {channels:?})"
    )]
    AlreadyCompensated {
        path: PathBuf,
        channels: Vec<String>,
    },
    #[error(
        "channel `{channel}` in `{path}` was transformed by `cytiq transform`; compensate the raw FCS before transforming"
    )]
    TransformedInput { path: PathBuf, channel: String },
    #[error(
        "`{path}` holds spectrally unmixed abundances ({components:?}); unmixing replaces compensation, so pass the raw FCS or skip `cytiq compensate`"
    )]
    UnmixedInput {
        path: PathBuf,
        components: Vec<String>,
    },
    #[error("matrix channel `{channel}` was not found in `{path}`")]
    MissingChannel { path: PathBuf, channel: String },
    #[error("failed to read matrix `{path}`: {source}")]
    MatrixRead {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("matrix `{path}` is invalid: {reason}")]
    InvalidMatrix { path: PathBuf, reason: String },
    #[error("spillover matrix of `{path}` is invalid: {source}")]
    Spillover {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("failed to write event mask: {0}")]
    Mask(#[from] cytiq_mask::MaskError),
    #[error("{0}")]
    Publish(#[from] cytiq_publish::PublishError),
}
