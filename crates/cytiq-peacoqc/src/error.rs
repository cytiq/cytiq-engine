use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PeacoQcError {
    #[error("FCS input file `{path}` does not exist")]
    MissingInputFile { path: PathBuf },
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
    #[error(
        "margin bounds of channel `{channel}` in `{path}` are unknown: it was transformed or compensated without recorded `CYTIQ_MARGINS`; remove margins on the raw file, or use `margin_pass` from the masks `cytiq compensate` writes"
    )]
    UnknownMargins { path: PathBuf, channel: String },
    #[error("output FCS values for `{path}` contain non-finite values")]
    NonFiniteOutputValues { path: PathBuf },
    #[error("PeacoQC removed all events from `{path}`; cleaned FCS output would be empty")]
    AllEventsRemoved { path: PathBuf },
    #[error("could not build any peak features from the selected channels in `{path}`")]
    NoPeakFeatures { path: PathBuf },
    #[error("PeacoQC output format for `{path}` is not supported; MVP supports `.fcs`")]
    UnsupportedOutputFormat { path: PathBuf },
    #[error("failed to create debug output directory `{path}`: {source}")]
    DebugDirectoryCreate {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write CSV debug output `{path}`: {source}")]
    CsvWrite {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to flush CSV debug output `{path}`: {source}")]
    CsvFlush {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write event masks `{path}`: {message}")]
    ParquetWrite { path: PathBuf, message: String },
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
