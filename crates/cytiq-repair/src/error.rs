use std::io;
use std::path::PathBuf;

use crate::codes::{Refusal, RepairCode};

fn listed(codes: &[RepairCode]) -> String {
    codes
        .iter()
        .map(|code| format!("--allow {code}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    #[error("input file does not exist: {}", path.display())]
    MissingInput { path: PathBuf },
    #[error("{}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{}` is not repaired: {refusal}", path.display())]
    Refused { path: PathBuf, refusal: Refusal },
    #[error("`{}` conforms: there is nothing to repair", path.display())]
    NothingToRepair { path: PathBuf },
    #[error("`{}` needs repairs that were not allowed: {}", path.display(), listed(missing))]
    NotAllowed {
        path: PathBuf,
        missing: Vec<RepairCode>,
    },
    #[error("the output `{}` must be an `.fcs` file other than the input", path.display())]
    InvalidOutput { path: PathBuf },
    #[error("output `{}` already exists; pass --overwrite to replace it", path.display())]
    OutputExists { path: PathBuf },
    #[error("`{}` changed while it was repaired", path.display())]
    InputChanged { path: PathBuf },
    /// What was written is not what was planned: nothing is published.
    #[error("the repaired file does not verify: {reason}")]
    NotVerified { reason: String },
    #[error("failed to write the repaired file: {0}")]
    Publish(#[from] cytiq_publish::PublishError),
    #[error("failed to serialize the repair plan: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to write the repair plan: {0}")]
    Write(#[source] io::Error),
}
