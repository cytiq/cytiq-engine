use std::io::{self};
use std::path::PathBuf;

use clap::Args;
use cytiq_peacoqc::{
    PeacoQcConfig, PeacoQcConstants, PeacoQcRunner, masks_path as peacoqc_masks_path,
};
use cytiq_provenance::record_path_for_file;
use log::info;
use serde::Serialize;

use crate::cli::CliPeacoQcMethod;
use crate::parsers::{parse_channels, parse_threads};
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Args)]
pub(crate) struct PeacoqcArgs {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    /// Comma-separated channel names used for peak detection and QC.
    #[arg(long)]
    channels: Option<String>,
    /// Channel name used for peak detection and QC; can be repeated.
    #[arg(long = "channel")]
    channel: Vec<String>,
    #[arg(long, value_enum, default_value_t = CliPeacoQcMethod::All)]
    method: CliPeacoQcMethod,
    /// Apply legacy `PeacoQC` margins (lower bounds and observed extrema);
    /// can remove negative fluorescence. Disabled by default.
    #[arg(long)]
    remove_margins: bool,
    /// Ignore zero values during peak detection; useful for mass cytometry.
    #[arg(long)]
    remove_zeros: bool,
    #[arg(long)]
    debug_dir: Option<PathBuf>,
    /// Worker thread limit; defaults to all available logical CPUs.
    #[arg(long, value_parser = parse_threads)]
    pub(crate) threads: Option<usize>,
}

pub(crate) fn run(
    args: PeacoqcArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let PeacoqcArgs {
        input,
        output,
        channels,
        channel,
        method,
        remove_margins,
        remove_zeros,
        debug_dir,
        threads: _,
    } = args;
    info!(
        "starting peacoqc: input={}, output={}, method={:?}",
        input.display(),
        output.display(),
        method
    );
    let channels = parse_channels(channels.as_deref(), &channel)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let config = PeacoQcConfig {
        channels,
        method: method.into(),
        remove_margins,
        remove_zeros,
        debug_dir,
    };
    recorder.snapshot_inputs("input", std::slice::from_ref(&input))?;
    recorder.set_constants(&PeacoQcConstants::default());
    recorder.set_algorithms(cytiq_peacoqc::UPSTREAM);
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(output.clone())
    );
    let result = PeacoQcRunner::run(&input, &output, &config)?;
    let outputs = [
        output.clone(),
        peacoqc_masks_path(&output),
        cytiq_peacoqc::summary_path(&output),
    ];
    info!(
        "peacoqc retained {} of {} events",
        result.summary.metrics.events_after, result.summary.metrics.events_before
    );
    if let Some(debug_dir) = config.debug_dir.as_ref() {
        info!("wrote peacoqc debug artifacts to {}", debug_dir.display());
    }
    info!("peacoqc completed successfully");
    record_provenance(recorder, &outputs, &record_path_for_file(&output))
}
