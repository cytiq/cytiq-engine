use std::io::{self};
use std::path::PathBuf;

use clap::Args;
use cytiq_cytoscan::{CytoScanConfig, CytoScanConstants, CytoScanRunner};
use cytiq_provenance::{files_under, record_path_for_dir};
use log::{info, warn};
use serde::Serialize;

use crate::cli::{CliCytoScanFeatureMethod, CliCytoScanFlagMethod};
use crate::parsers::{parse_channels, parse_threads};
use crate::provenance::record_provenance;

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
    } = args;
    info!(
        "starting cytoscan: inputs={}, references={}, output={}, method={:?}, flag_method={:?}",
        input.len(),
        reference.len(),
        output.display(),
        method,
        flag_method
    );
    let channels = parse_channels(channels.as_deref(), &channel)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let config = CytoScanConfig {
        inputs: input,
        reference_inputs: reference,
        output_dir: output,
        channels,
        feature_method: method.into(),
        flag_method: flag_method.into(),
        debug_dir,
        strict,
    };
    let (test, reference) = CytoScanRunner::input_files(&config)?;
    recorder.snapshot_inputs("input", &test)?;
    recorder.snapshot_inputs("reference", &reference)?;
    recorder.set_constants(&CytoScanConstants::default());
    recorder.set_algorithms(cytiq_cytoscan::UPSTREAM);
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::Dir(config.output_dir.clone())
    );
    let result = CytoScanRunner::run(&config)?;
    for warning in &result.diagnostics.warnings {
        warn!("cytoscan: {warning}");
    }
    info!(
        "cytoscan wrote {} summary row(s) and {} feature column(s)",
        result.summary_rows.len(),
        result.test_features.feature_names.len()
    );
    if let Some(debug_dir) = config.debug_dir.as_ref() {
        info!("wrote cytoscan debug artifacts to {}", debug_dir.display());
    }
    info!("cytoscan completed successfully");
    record_provenance(
        recorder,
        &files_under(&config.output_dir),
        &record_path_for_dir(&config.output_dir),
    )
}
