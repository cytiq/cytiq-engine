use std::path::PathBuf;

use clap::Args;
use cytiq_provenance::record_path_for_dir;
use log::info;
use serde::Serialize;

use crate::parsers::{parse_non_negative_f64, parse_threads};
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Args)]
pub(crate) struct ClassifyArgs {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    model: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long = "mask")]
    masks: Vec<PathBuf>,
    #[arg(long, value_parser = parse_threads)]
    threads: Option<usize>,
    #[arg(long, default_value_t = cytiq_classify::DEFAULT_BATCH_SIZE, value_parser = parse_threads)]
    batch_size: usize,
    #[arg(long, default_value_t = 0.0, value_parser = parse_non_negative_f64)]
    abstain_margin: f64,
    /// Independently validated decision policy; no default rejection thresholds.
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_policy: Option<PathBuf>,
    #[arg(long)]
    overwrite: bool,
}

pub(crate) fn run(
    args: ClassifyArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let ClassifyArgs {
        input,
        model,
        output,
        masks,
        threads,
        batch_size,
        abstain_margin,
        decision_policy,
        overwrite,
    } = args;
    let config = cytiq_classify::ClassifyConfig {
        input,
        model,
        output,
        masks,
        threads,
        batch_size,
        #[allow(clippy::cast_possible_truncation)]
        abstain_margin: abstain_margin as f32,
        overwrite,
    };
    info!("starting classify: {}", config.input.display());
    recorder.snapshot_inputs("input", std::slice::from_ref(&config.input))?;
    recorder.snapshot_inputs("model", &cytiq_classify::input_files(&config.model)?)?;
    recorder.snapshot_inputs("mask", &config.masks)?;
    if let Some(path) = &decision_policy {
        let policy = cytiq_classify::DecisionPolicy::load(path)?;
        recorder.snapshot_inputs("decision_policy", &policy.input_files(path))?;
    }
    recorder.set_algorithms(cytiq_classify::UPSTREAM);
    recorder.set_parallelism(
        config
            .threads
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from)),
    );
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::Dir(config.output.clone())
    );
    let result =
        cytiq_classify::ClassifyRunner::run_with_policy(&config, decision_policy.as_deref())?;
    info!(
        "inferred {} of {} events; {} legacy abstentions",
        result.assessed, result.events, result.unknown
    );
    if let Some(decision) = &result.decision {
        info!(
            "accepted {}; abstained {}; OOD assessed: {}; OOD events: {}",
            result.assessed - decision.abstained,
            decision.abstained,
            decision.ood_assessed,
            decision.ood_events
        );
    }
    if result.assessed > 0
        && result
            .decision
            .as_ref()
            .is_none_or(|decision| !decision.ood_assessed)
    {
        log::warn!("OOD was not assessed; probabilities describe only the declared class set");
    }
    record_provenance(
        recorder,
        &[
            config.output.join("predictions.parquet"),
            config.output.join("summary.json"),
        ],
        &record_path_for_dir(&config.output),
    )
}
