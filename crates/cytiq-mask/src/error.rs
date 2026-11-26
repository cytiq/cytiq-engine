use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MaskError {
    #[error("failed to open event mask `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read or write event mask `{path}`: {source}")]
    Parquet {
        path: PathBuf,
        /// Keeps the cause: a full disk is not an invalid mask.
        #[source]
        source: parquet::errors::ParquetError,
    },
    #[error("event mask `{path}` is invalid: {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error("event mask columns must all have {expected} rows; `{column}` has {actual}")]
    LengthMismatch {
        column: String,
        expected: usize,
        actual: usize,
    },
    #[error("at least one event mask is required")]
    Empty,
    #[error("output `{path}` already exists; pass --overwrite to replace it")]
    OutputExists { path: PathBuf },
    #[error(
        "event mask has no row for raw event {source_event_id}; it was computed on a different acquisition"
    )]
    MissingEvent { source_event_id: u64 },
    #[error(
        "event mask `{path}` does not record its acquisition; it was written by an older cytiq, regenerate it with this version"
    )]
    MissingAcquisition { path: PathBuf },
    #[error("event mask acquisition `{value}` is not a lowercase hex SHA-256")]
    InvalidAcquisition { value: String },
    #[error(
        "{what} was computed on acquisition {found}, not {expected}; masks only apply to files derived from the same raw FCS"
    )]
    AcquisitionMismatch {
        expected: String,
        found: String,
        what: String,
    },
    #[error("failed to read event lineage of the FCS input: {0}")]
    Lineage(#[source] cytiq_fcs::Error),
    #[error("failed to write event mask: {0}")]
    Publish(#[from] cytiq_publish::PublishError),
}
