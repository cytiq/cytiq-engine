use std::path::PathBuf;

use clap::Args;
use cytiq_inspect::meta::{MetaConfig, MetaRunner};
use cytiq_provenance::outcome::OutcomeReason;
use cytiq_provenance::record_path_for_file;
use log::{debug, info};
use serde::Serialize;

use crate::provenance::publish;

#[derive(Debug, Serialize, Args)]
pub(crate) struct MetaArgs {
    /// Input FCS file.
    #[arg(long)]
    input: PathBuf,
    /// Output JSON path; omit or pass `-` to write to stdout.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Replace an existing output file.
    #[arg(long)]
    overwrite: bool,
}

pub(crate) fn run(
    args: MetaArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let MetaArgs {
        input,
        output,
        overwrite,
    } = args;
    let output = output.filter(|path| path.as_os_str() != "-");
    debug!("starting meta: input={}", input.display());
    let config = MetaConfig {
        input,
        output,
        overwrite,
    };
    // A file output gets its provenance record like every other command's
    // output; the document on stdout already carries the input's hash.
    if config.output.is_some() {
        recorder.snapshot_roles(&[("input", config.input.clone())])?;
    }
    if let Some(output) = config.output.as_ref() {
        let recorder = crate::provenance::begin!(
            recorder,
            cytiq_provenance::reuse::Destination::File(output.clone())
        );
        let prepared = MetaRunner::prepare(&config, output)?;
        let outcome = publish(
            recorder,
            prepared.publication,
            std::slice::from_ref(output),
            &record_path_for_file(output),
            OutcomeReason::Success,
        )?;
        info!("wrote FCS metadata to {}", output.display());
        return Ok(outcome);
    }
    MetaRunner::run(&config)?;
    Ok(None)
}
