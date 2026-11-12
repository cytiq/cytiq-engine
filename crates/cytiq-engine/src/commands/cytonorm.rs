use std::path::PathBuf;

use clap::Subcommand;
use cytiq_flowsom::FlowSomConstants;

use log::info;
use serde::Serialize;

use crate::parsers::parse_threads;
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Subcommand)]
pub(crate) enum CytoNormCommand {
    /// Train a normalization bundle on controls or comparable batch aggregates.
    Train {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_parser = parse_threads)]
        threads: Option<usize>,
        #[arg(long)]
        debug_dir: Option<PathBuf>,
        #[arg(long)]
        overwrite: bool,
    },
    /// Normalize every event with the fixed model of a trained batch.
    Apply {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        model: PathBuf,
        #[arg(long)]
        batch: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_parser = parse_threads)]
        threads: Option<usize>,
        #[arg(long)]
        debug_dir: Option<PathBuf>,
        #[arg(long)]
        overwrite: bool,
    },
}

pub(crate) fn run(
    command: CytoNormCommand,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    use cytiq_cytonorm::{ApplyConfig, CytoNormRunner, TrainConfig};
    recorder.set_algorithms(cytiq_cytonorm::UPSTREAM);
    recorder.set_constants(&FlowSomConstants::default());
    let (result, recorder) = match command {
        CytoNormCommand::Train {
            manifest,
            output,
            threads,
            debug_dir,
            overwrite,
        } => {
            let design = cytiq_cytonorm::Manifest::load(&manifest)?;
            recorder.set_config(&serde_json::json!({"manifest":design,"output":output,"threads":threads,"debug_dir":debug_dir,"overwrite":overwrite}));
            recorder.snapshot_inputs("cohort_input", &CytoNormRunner::input_files(&manifest)?)?;
            let run = crate::provenance::begin!(
                recorder,
                cytiq_provenance::reuse::Destination::Dir(output.clone())
            );
            let result = CytoNormRunner::train(&TrainConfig {
                manifest,
                output,
                debug_dir,
                overwrite,
            })?;
            (result, run)
        }
        CytoNormCommand::Apply {
            input,
            model,
            batch,
            output,
            threads: _,
            debug_dir,
            overwrite,
        } => {
            recorder.snapshot_inputs("input", std::slice::from_ref(&input))?;
            recorder.snapshot_inputs(
                "normalization_model",
                &cytiq_cytonorm::CytoNormModel::input_files(&model),
            )?;
            let run = crate::provenance::begin!(
                recorder,
                cytiq_provenance::reuse::Destination::File(output.clone())
            );
            let result = CytoNormRunner::apply(&ApplyConfig {
                input,
                model,
                batch,
                output,
                debug_dir,
                overwrite,
            })?;
            (result, run)
        }
    };
    info!(
        "cytonorm processed {} events; normalization model {}",
        result.events, result.model_sha256
    );
    record_provenance(recorder, &result.outputs, &result.provenance)
}
