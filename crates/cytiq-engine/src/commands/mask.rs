use std::path::PathBuf;

use clap::Subcommand;
use cytiq_mask::{MaskCombineConfig, MaskCombineRunner};
use cytiq_provenance::outcome::OutcomeReason;
use cytiq_provenance::record_path_for_file;
use log::info;
use serde::Serialize;

use crate::provenance::publish;

#[derive(Debug, Serialize, Subcommand)]
#[serde(untagged)]
pub(crate) enum MaskCommand {
    /// Combine masks of one acquisition by `source_event_id`; an event is kept
    /// only when every mask keeps it.
    Combine {
        /// Event mask Parquet file; repeat for each mask to combine.
        #[arg(long = "mask", required = true)]
        masks: Vec<PathBuf>,
        /// Output Parquet path.
        #[arg(long)]
        output: PathBuf,
        /// Replace an existing output file.
        #[arg(long)]
        overwrite: bool,
    },
}

pub(crate) fn run(
    command: MaskCommand,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    match command {
        MaskCommand::Combine {
            masks,
            output,
            overwrite,
        } => {
            info!("starting mask combine: masks={}", masks.len());
            let config = MaskCombineConfig {
                masks,
                output,
                overwrite,
            };
            recorder.snapshot_inputs("mask", &config.masks)?;
            let recorder = crate::provenance::begin!(
                recorder,
                cytiq_provenance::reuse::Destination::File(config.output.clone())
            );
            let prepared = MaskCombineRunner::prepare(&config)?;
            let combined = prepared.summary;
            let outcome = publish(
                recorder,
                prepared.publication,
                std::slice::from_ref(&config.output),
                &record_path_for_file(&config.output),
                OutcomeReason::Success,
            )?;
            info!(
                "mask combine kept {} of {} event(s); wrote {}",
                combined.kept(),
                combined.len(),
                config.output.display()
            );
            Ok(outcome)
        }
    }
}
