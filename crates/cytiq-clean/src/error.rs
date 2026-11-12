use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CleanError {
    #[error("failed to read FCS input `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("failed to read the event mask: {0}")]
    Mask(#[from] cytiq_mask::MaskError),
    #[error("channel `{name}` was not found in `{path}`")]
    MissingChannel { name: String, path: PathBuf },
    #[error(
        "several viability channel candidates found ({candidates}); \
         pass --viability-channel or --no-viability"
    )]
    AmbiguousViabilityChannel { candidates: String },
    #[error("--require {filter}: the filter is {status}, not applied ({reason})")]
    RequiredFilterNotApplied {
        filter: &'static str,
        status: &'static str,
        reason: String,
    },
    #[error(
        "viability channel `{channel}` was transformed by `cytiq transform`; run `cytiq clean` on the raw FCS (or pass --no-viability) and combine its masks with `cytiq mask combine`"
    )]
    TransformedViability { channel: String },
    #[error(
        "scatter channel `{channel}` was transformed by `cytiq transform`; debris and singlet filters need raw linear scatter, so run `cytiq clean` on the raw FCS and combine its masks with `cytiq mask combine`"
    )]
    TransformedScatter { channel: String },
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Gating(#[from] cytiq_gating::GatingError),
    #[error("failed to read GatingML `{path}`: {source}")]
    GatingMlRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write `{path}`: {source}")]
    OutputWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize JSON `{path}`: {source}")]
    JsonWrite {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to write parquet `{path}`: {message}")]
    ParquetWrite { path: PathBuf, message: String },
    #[error("{0}")]
    Publish(#[from] cytiq_publish::PublishError),
}
