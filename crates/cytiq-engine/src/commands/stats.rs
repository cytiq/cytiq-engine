use std::path::PathBuf;

use clap::Args;
use cytiq_inspect::stats::{DEFAULT_BINS, StatsConfig, StatsRunner};
use cytiq_provenance::outcome::OutcomeReason;
use cytiq_provenance::record_path_for_file;
use log::info;
use serde::Serialize;

use crate::provenance::publish;

#[derive(Debug, Serialize, Args)]
pub(crate) struct StatsArgs {
    /// Input FCS file.
    #[arg(long)]
    input: PathBuf,
    /// Output JSON path.
    #[arg(long)]
    output: PathBuf,
    /// Histogram bins per channel (2 to 4096).
    #[arg(long, default_value_t = DEFAULT_BINS)]
    bins: usize,
    /// Replace an existing output file.
    #[arg(long)]
    overwrite: bool,
}

pub(crate) fn run(
    args: StatsArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let StatsArgs {
        input,
        output,
        bins,
        overwrite,
    } = args;
    info!("starting stats: input={}", input.display());
    recorder.snapshot_roles(&[("input", input.clone())])?;
    let config = StatsConfig {
        input,
        output,
        bins,
        overwrite,
    };
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let prepared = StatsRunner::prepare(&config)?;
    let report = prepared.summary;
    let outcome = publish(
        recorder,
        prepared.publication,
        std::slice::from_ref(&config.output),
        &record_path_for_file(&config.output),
        OutcomeReason::Success,
    )?;
    info!(
        "stats: {} event(s), {} channel(s); wrote {}",
        report.event_count,
        report.channels.len(),
        config.output.display()
    );
    Ok(outcome)
}
