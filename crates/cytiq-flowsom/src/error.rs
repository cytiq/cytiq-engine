use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FlowSomError {
    #[error("model I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid model JSON: {0}")]
    ModelJson(#[from] serde_json::Error),
    #[error("FCS input file `{path}` does not exist")]
    MissingInputFile { path: PathBuf },
    #[error("failed to read FCS input `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("FCS input `{path}` has no events")]
    EmptyInput { path: PathBuf },
    #[error("the event mask keeps no events of `{path}`")]
    NoMaskedEvents { path: PathBuf },
    #[error("failed to read the event mask: {0}")]
    Mask(#[from] cytiq_mask::MaskError),
    #[error("`--channels` must include at least one channel")]
    EmptyChannels,
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
    #[error("failed to write JSON debug output `{path}`: {source}")]
    JsonWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize JSON debug output `{path}`: {source}")]
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
