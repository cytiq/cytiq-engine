use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatingError {
    #[error("failed to parse GatingML XML: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("invalid GatingML: {0}")]
    InvalidGatingMl(String),
    #[error("unsupported GatingML element `{element}`")]
    UnsupportedElement { element: String },
    #[error("duplicate gate id `{id}`")]
    DuplicateGate { id: String },
    #[error("gate `{gate}` references unknown gate `{reference}`")]
    UnknownGateRef { gate: String, reference: String },
    #[error("gate `{gate}` references unknown transformation `{reference}`")]
    UnknownTransform { gate: String, reference: String },
    #[error("gate `{gate}` references unknown compensation matrix `{reference}`")]
    UnknownCompensation { gate: String, reference: String },
    #[error("gate hierarchy contains a cycle through `{gate}`")]
    CyclicHierarchy { gate: String },
    #[error("unknown gate `{id}`")]
    UnknownGate { id: String },
    #[error("dimension `{name}` was not found in the FCS parameters")]
    MissingDimension { name: String },
    #[error("invalid transformation `{id}`: {reason}")]
    InvalidTransform { id: String, reason: String },
    #[error("invalid gate `{id}`: {reason}")]
    InvalidGate { id: String, reason: String },
    #[error("event mask has {actual} rows but the FCS has {expected} events")]
    MaskLength { expected: usize, actual: usize },
    #[error("failed to read or write an event mask: {0}")]
    Mask(#[from] cytiq_mask::MaskError),
    #[error("gate id `{id}` clashes with a reserved membership column name")]
    ReservedGateId { id: String },
    #[error("invalid signal state metadata: {0}")]
    SignalState(#[source] cytiq_fcs::Error),
    #[error(
        "gate `{gate}` would {action} channel `{channel}` a second time: the FCS was already {state}; gate the raw FCS or drop the step from the GatingML"
    )]
    AlreadyProcessed {
        gate: String,
        channel: String,
        action: &'static str,
        state: &'static str,
    },
    #[error("compensation `{reference}` is invalid: {source}")]
    Compensation {
        reference: String,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error("failed to read FCS input `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
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
    #[error("failed to write CSV `{path}`: {source}")]
    CsvWrite {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
    #[error("failed to serialize JSON `{path}`: {source}")]
    JsonWrite {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{0}")]
    Publish(#[from] cytiq_publish::PublishError),
}
