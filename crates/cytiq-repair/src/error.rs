use std::io;
use std::path::PathBuf;

use crate::codes::Refusal;

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
    #[error("failed to serialize the repair plan: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to write the repair plan: {0}")]
    Write(#[source] io::Error),
}
