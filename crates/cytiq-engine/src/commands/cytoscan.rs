use std::path::PathBuf;

use clap::Args;
use cytiq_cytoscan::{CytoScanConfig, CytoScanConstants, CytoScanRunner};
use cytiq_provenance::outcome::OutcomeReason;
use cytiq_provenance::record_path_for_dir;
use log::{info, warn};
use serde::Serialize;

use crate::cli::{CliCytoScanFeatureMethod, CliCytoScanFlagMethod, CliNoveltyMode};
use crate::parsers::{parse_channels, parse_stability_seeds, parse_threads};
use crate::provenance::{publish, staged_under};

#[derive(Debug, Serialize, Args)]
pub(crate) struct CytoscanArgs {
    /// Input .fcs file, glob pattern, or directory. Can be passed multiple times.
    #[arg(long, required = true)]
    input: Vec<PathBuf>,
    /// Reference .fcs file, glob pattern, or directory for novelty detection.
    #[arg(long)]
    reference: Vec<PathBuf>,
    /// Output directory for summary, feature, and configuration tables.
    #[arg(long)]
    output: PathBuf,
    /// Comma-separated channel names used for feature generation.
    #[arg(long)]
    channels: Option<String>,
    /// Channel name used for feature generation; can be repeated.
    #[arg(long = "channel")]
    channel: Vec<String>,
    #[arg(long, value_enum, default_value_t = CliCytoScanFeatureMethod::Quantiles)]
    method: CliCytoScanFeatureMethod,
    #[arg(long, value_enum, default_value_t = CliCytoScanFlagMethod::Outlier)]
    flag_method: CliCytoScanFlagMethod,
    #[arg(long)]
    debug_dir: Option<PathBuf>,
    /// Worker thread limit; defaults to all available logical CPUs.
    #[arg(long, value_parser = parse_threads)]
    pub(crate) threads: Option<usize>,
    /// Fail instead of warning when a file has no spillover metadata or a
    /// selected channel is not in it.
    #[arg(long)]
    strict: bool,
    /// Write into an output directory that already holds files, replacing
    /// the outputs of an earlier run.
    #[arg(long)]
    overwrite: bool,
    /// Seed of the per-file event subsample and the isolation trees
    /// (upstream's 42 by default). Recorded under provenance `constants`;
    /// vary it to measure how much scores depend on it.
    #[arg(long, default_value_t = cytiq_cytoscan::DEFAULT_RANDOM_SEED)]
    #[serde(skip)]
    seed: u64,
    /// Repeat the analysis with N seeds in total (2 to 64): --seed and
    /// seed + j * 2^32 for j = 1..N-1, so no two share an isolation tree.
    /// Writes each file's flag agreement and every seed's values next to the
    /// summary, which does not change.
    #[arg(long, value_name = "N", value_parser = parse_stability_seeds)]
    #[serde(skip_serializing_if = "Option::is_none")]
    stability_seeds: Option<usize>,
    /// How the novelty flag is decided. `upstream` reproduces the original tool.
    /// `conformal-ledoit-wolf-v1` is the mode the novelty benchmark
    /// validated: a calibrated p-value per test file, written next to the
    /// upstream columns; it abstains below 19 references.
    #[arg(long, value_enum, default_value_t = CliNoveltyMode::Upstream)]
    #[serde(skip_serializing_if = "CliNoveltyMode::is_upstream")]
    novelty_mode: CliNoveltyMode,
}

pub(crate) fn run(
    args: CytoscanArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let CytoscanArgs {
        input,
        reference,
        output,
        channels,
        channel,
        method,
        flag_method,
        debug_dir,
        threads: _,
        strict,
        overwrite,
        seed,
        stability_seeds,
        novelty_mode,
    } = args;
    info!(
        "starting cytoscan: inputs={}, references={}, output={}, method={:?}, flag_method={:?}",
        input.len(),
        reference.len(),
        output.display(),
        method,
        flag_method
    );
    let channels = parse_channels(channels.as_deref(), &channel)?;
    let config = CytoScanConfig {
        inputs: input,
        reference_inputs: reference,
        output_dir: output,
        channels,
        feature_method: method.into(),
        flag_method: flag_method.into(),
        debug_dir,
        strict,
        overwrite,
        random_seed: seed,
        events_per_file: cytiq_cytoscan::DEFAULT_EVENTS_PER_FILE,
        stability_seeds,
        novelty_mode: novelty_mode.into(),
    };
    let (test, reference) = CytoScanRunner::input_files(&config)?;
    recorder.snapshot_inputs("input", &test)?;
    recorder.snapshot_inputs("reference", &reference)?;
    recorder.set_constants(&CytoScanConstants::for_config(&config));
    recorder.set_algorithms(cytiq_cytoscan::UPSTREAM);
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::Dir(config.output_dir.clone())
    );
    let prepared = CytoScanRunner::prepare(&config)?;
    let result = prepared.summary;
    let outputs = staged_under(&prepared.publication, &config.output_dir)?;
    let outcome = publish(
        recorder,
        prepared.publication,
        &outputs,
        &record_path_for_dir(&config.output_dir),
        OutcomeReason::Success,
    )?;
    for warning in &result.diagnostics.warnings {
        warn!("cytoscan: {warning}");
    }
    info!(
        "cytoscan wrote {} summary row(s) and {} feature column(s)",
        result.summary_rows.len(),
        result.test_features.feature_names.len()
    );
    let flag_status = |flag: Option<&cytiq_cytoscan::FlagAssessment>| {
        flag.map_or_else(
            || "not computed".to_owned(),
            |flag| {
                format!(
                    "{:?} calibration, {:?}, stability {:?}",
                    flag.calibration_status, flag.seed_dependence, flag.stability_status
                )
            },
        )
    };
    info!(
        "cytoscan flags: outlier (within test cohort): {}; novelty (against reference): {}",
        flag_status(result.diagnostics.outlier_flag.as_ref()),
        flag_status(result.diagnostics.novelty_flag.as_ref())
    );
    if let Some(debug_dir) = config.debug_dir.as_ref() {
        info!("wrote cytoscan debug artifacts to {}", debug_dir.display());
    }
    info!("cytoscan completed successfully");
    Ok(outcome)
}
