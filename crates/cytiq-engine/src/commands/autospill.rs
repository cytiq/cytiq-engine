use std::path::PathBuf;

use clap::Args;
use cytiq_autospill::{
    AutospillConfig, AutospillRunner, export_matrix, read_controls_manifest,
    read_detectors_manifest, read_transform_manifest,
};
use cytiq_provenance::record_path_for_file;
use log::{debug, info};
use serde::Serialize;

use crate::cli::CliMatrixKind;
use crate::parsers::parse_threads;
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Args)]
pub(crate) struct AutospillArgs {
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
    transform_file: Option<PathBuf>,
    #[arg(long)]
    debug_dir: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = CliMatrixKind::Compensation)]
    matrix_kind: CliMatrixKind,
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
    args: AutospillArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let AutospillArgs {
        controls,
        input_dir,
        output,
        detectors,
        transform_file,
        debug_dir,
        matrix_kind,
        fsc,
        ssc,
        threads: _,
        overwrite,
    } = args;
    info!(
        "starting autospill: controls={}, input_dir={}, output={}, matrix_kind={:?}",
        controls.display(),
        input_dir.display(),
        output.display(),
        matrix_kind
    );
    debug!("autospill channels: FSC=`{fsc}`, SSC=`{ssc}`");
    let manifest = read_controls_manifest(&controls)?;
    info!(
        "loaded {} control rows from {}",
        manifest.len(),
        controls.display()
    );
    recorder.snapshot_inputs("controls_manifest", std::slice::from_ref(&controls))?;
    recorder.snapshot_inputs("detectors", detectors.as_slice())?;
    recorder.snapshot_inputs("transforms", transform_file.as_slice())?;
    recorder.snapshot_inputs(
        "control",
        &AutospillRunner::input_files(&manifest, &input_dir),
    )?;
    recorder.set_algorithms(cytiq_autospill::UPSTREAM);
    let detectors = detectors
        .map(read_detectors_manifest)
        .transpose()?
        .unwrap_or_default();
    if detectors.is_empty() {
        info!("no detector manifest provided; detector order will be inferred");
    } else {
        info!("loaded {} detector specs", detectors.len());
    }
    let transforms = transform_file
        .map(read_transform_manifest)
        .transpose()?
        .unwrap_or_default();
    if transforms.is_empty() {
        debug!("no transform manifest provided");
    } else {
        info!("loaded {} transform specs", transforms.len());
    }
    let config = AutospillConfig {
        fsc,
        ssc,
        matrix_kind: matrix_kind.into(),
        detectors,
        transforms,
        debug_dir,
    };
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(output.clone())
    );
    // Refuse an existing output before the computation, not after it.
    let existing = cytiq_publish::Existing::overwrite(overwrite);
    cytiq_publish::Publication::new(existing).check(&output)?;
    let result = AutospillRunner::run(&manifest, &input_dir, &config)?;
    info!(
        "autospill finished computation for {} dyes across {} detectors",
        result.dyes.len(),
        result.detectors.len()
    );
    export_matrix(&result, config.matrix_kind, &output, existing)?;
    info!(
        "wrote {:?} matrix to {}",
        config.matrix_kind,
        output.display()
    );
    if let Some(debug_dir) = config.debug_dir.as_ref() {
        info!("wrote debug artifacts to {}", debug_dir.display());
    }
    info!("autospill completed successfully");
    record_provenance(
        recorder,
        std::slice::from_ref(&output),
        &record_path_for_file(&output),
    )
}
