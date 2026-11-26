use std::io::Write as _;
use std::path::PathBuf;

use clap::Args;
use cytiq_repair::{PlanDocument, RepairError};
use serde::Serialize;

#[derive(Debug, Serialize, Args)]
pub(crate) struct RepairArgs {
    /// FCS file, as the instrument wrote it.
    #[arg(long)]
    input: PathBuf,
    /// Print the diagnosis as JSON and write nothing.
    #[arg(long)]
    plan: bool,
}

pub(crate) fn run(args: &RepairArgs) -> crate::provenance::CommandResult {
    if !args.plan {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "`cytiq repair` only diagnoses a file: pass `--plan`",
        )
        .into());
    }
    let document = PlanDocument::of(&args.input)?;
    let mut text = serde_json::to_vec_pretty(&document).map_err(RepairError::Serialize)?;
    text.push(b'\n');
    std::io::stdout()
        .lock()
        .write_all(&text)
        .map_err(RepairError::Write)?;
    Ok(None)
}
