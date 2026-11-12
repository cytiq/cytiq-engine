use std::path::PathBuf;

use clap::Args;
use cytiq_autospectral::{
    AutoSpectralConfig, AutoSpectralRunner,
    read_controls_manifest as read_autospectral_controls_manifest,
    read_detectors_manifest as read_autospectral_detectors_manifest, read_samples_manifest,
};
use cytiq_provenance::{files_under, record_path_for_dir};
use log::{debug, info};
use serde::Serialize;

use crate::parsers::parse_threads;
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Args)]
pub(crate) struct AutospectralArgs {
    /// Controls manifest CSV with `filename,dye[,primary_channel]`; include one `dye=Autofluorescence` row.
    #[arg(long)]
    controls: PathBuf,
    /// Samples manifest CSV with `filename` rows for fully stained FCS files.
    #[arg(long)]
    samples: PathBuf,
    #[arg(long)]
    input_dir: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    detectors: Option<PathBuf>,
    #[arg(long)]
    debug_dir: Option<PathBuf>,
    #[arg(long, default_value = "FSC-A")]
    fsc: String,
    #[arg(long, default_value = "SSC-A")]
    ssc: String,
    /// Worker thread limit; defaults to all available logical CPUs.
    #[arg(long, value_parser = parse_threads)]
    pub(crate) threads: Option<usize>,
    /// Gate control FSC/SSC with the 1-99% quantile box instead of
    /// cleaning them (drop raw detector-margin events, then the
    /// `autospill` KDE gate), for controls too small for the KDE gate. On
    /// real cell controls the box keeps debris and saturated events that
    /// bias the spectra.
    #[arg(long, conflicts_with = "clean_controls")]
    quantile_gate: bool,
    /// Clean controls before building spectra (the default; kept for
    /// existing scripts).
    #[arg(long)]
    clean_controls: bool,
}

pub(crate) fn run(
    args: AutospectralArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let AutospectralArgs {
        controls,
        samples,
        input_dir,
        output,
        detectors,
        debug_dir,
        fsc,
        ssc,
        threads: _,
        quantile_gate,
        clean_controls: _,
    } = args;
    info!(
        "starting autospectral: controls={}, samples={}, input_dir={}, output={}",
        controls.display(),
        samples.display(),
        input_dir.display(),
        output.display()
    );
    debug!("autospectral channels: FSC=`{fsc}`, SSC=`{ssc}`");
    let control_manifest = read_autospectral_controls_manifest(&controls)?;
    let sample_manifest = read_samples_manifest(&samples)?;
    recorder.snapshot_inputs("controls_manifest", std::slice::from_ref(&controls))?;
    recorder.snapshot_inputs("samples_manifest", std::slice::from_ref(&samples))?;
    recorder.snapshot_inputs("detectors", detectors.as_slice())?;
    let (control_files, sample_files) =
        AutoSpectralRunner::input_files(&control_manifest, &sample_manifest, &input_dir);
    recorder.snapshot_inputs("control", &control_files)?;
    recorder.snapshot_inputs("input", &sample_files)?;
    recorder.set_algorithms(cytiq_autospectral::UPSTREAM);
    info!(
        "loaded {} control rows and {} sample rows",
        control_manifest.len(),
        sample_manifest.len()
    );
    let detectors = detectors
        .map(read_autospectral_detectors_manifest)
        .transpose()?
        .unwrap_or_default();
    if detectors.is_empty() {
        info!("no detector manifest provided; spectral detector order will be inferred");
    } else {
        info!("loaded {} detector specs", detectors.len());
    }
    let config = AutoSpectralConfig {
        fsc,
        ssc,
        detectors,
        output_dir: output.clone(),
        debug_dir,
        clean_controls: !quantile_gate,
    };
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::Dir(output.clone())
    );
    let summary =
        AutoSpectralRunner::run(&control_manifest, &sample_manifest, &input_dir, &config)?;
    info!(
        "autospectral finished {} samples across {} components; condition_number={:.6e}",
        summary.samples.len(),
        summary.components.len(),
        summary.condition_number
    );
    info!("wrote autospectral outputs to {}", output.display());
    info!("autospectral completed successfully");
    record_provenance(
        recorder,
        &files_under(&output),
        &record_path_for_dir(&output),
    )
}
