use std::path::PathBuf;

use clap::Args;
use cytiq_autospill::{
    AutospreadConfig, AutospreadRunner, export_spread_matrix, read_controls_manifest,
    read_detectors_manifest,
};
use cytiq_provenance::record_path_for_file;
use log::{debug, info};
use serde::Serialize;

use crate::parsers::parse_threads;
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Args)]
pub(crate) struct AutospreadArgs {
    /// Controls manifest CSV with `filename,dye`; primary channels are inferred from FCS metadata.
    #[arg(long)]
    controls: PathBuf,
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
    /// Replace an existing output file.
    #[arg(long)]
    overwrite: bool,
}

pub(crate) fn run(
    args: AutospreadArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let AutospreadArgs {
        controls,
        input_dir,
        output,
        detectors,
        debug_dir,
        fsc,
        ssc,
        threads: _,
        overwrite,
    } = args;
    info!(
        "starting autospread: controls={}, input_dir={}, output={}",
        controls.display(),
        input_dir.display(),
        output.display()
    );
    debug!("autospread channels: FSC=`{fsc}`, SSC=`{ssc}`");
    let manifest = read_controls_manifest(&controls)?;
    info!(
        "loaded {} control rows from {}",
        manifest.len(),
        controls.display()
    );
    recorder.snapshot_inputs("controls_manifest", std::slice::from_ref(&controls))?;
    recorder.snapshot_inputs("detectors", detectors.as_slice())?;
    recorder.snapshot_inputs(
        "control",
        &AutospreadRunner::input_files(&manifest, &input_dir),
    )?;
    let detectors = detectors
        .map(read_detectors_manifest)
        .transpose()?
        .unwrap_or_default();
    if detectors.is_empty() {
        info!("no detector manifest provided; detector order will be inferred");
    } else {
        info!("loaded {} detector specs", detectors.len());
    }
    let config = AutospreadConfig {
        fsc,
        ssc,
        detectors,
        debug_dir,
    };
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(output.clone())
    );
    // Refuse an existing output before the computation, not after it.
    let existing = cytiq_publish::Existing::overwrite(overwrite);
    cytiq_publish::Publication::new(existing).check(&output)?;
    let result = AutospreadRunner::run(&manifest, &input_dir, &config)?;
    export_spread_matrix(&result, &output, existing)?;
    info!(
        "autospread finished computation for {} dyes across {} detectors",
        result.dyes.len(),
        result.detectors.len()
    );
    info!("wrote spread matrix to {}", output.display());
    if let Some(debug_dir) = config.debug_dir.as_ref() {
        info!("wrote debug artifacts to {}", debug_dir.display());
    }
    info!("autospread completed successfully");
    record_provenance(
        recorder,
        std::slice::from_ref(&output),
        &record_path_for_file(&output),
    )
}
