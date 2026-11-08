use std::path::PathBuf;

use clap::Args;
use cytiq_compensate::{CompensateConfig, CompensateRunner, MatrixKind as CompensateMatrixKind};
use cytiq_provenance::record_path_for_file;
use log::info;
use serde::Serialize;

use crate::cli::CliMatrixKind;
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Args)]
pub(crate) struct CompensateArgs {
    /// Input FCS file with raw (uncompensated, untransformed) values.
    #[arg(long)]
    input: PathBuf,
    /// Output FCS file; `<output stem>.masks.parquet` is written beside it.
    #[arg(long)]
    output: PathBuf,
    /// Square matrix CSV/TSV as written by `cytiq autospill`; defaults to
    /// the input's `$SPILLOVER` keyword.
    #[arg(long, requires = "matrix_kind")]
    matrix: Option<PathBuf>,
    /// What `--matrix` holds.
    #[arg(long, value_enum, requires = "matrix")]
    matrix_kind: Option<CliMatrixKind>,
    /// Replace existing output files.
    #[arg(long)]
    overwrite: bool,
}

pub(crate) fn run(
    args: CompensateArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let CompensateArgs {
        input,
        output,
        matrix,
        matrix_kind,
        overwrite,
    } = args;
    info!(
        "starting compensate: input={}, output={}",
        input.display(),
        output.display()
    );
    let config = CompensateConfig {
        input,
        output,
        matrix: matrix.zip(matrix_kind.map(CompensateMatrixKind::from)),
        overwrite,
    };
    recorder.snapshot_inputs("input", std::slice::from_ref(&config.input))?;
    if let Some((matrix, _)) = &config.matrix {
        recorder.snapshot_inputs("matrix", std::slice::from_ref(matrix))?;
    }
    recorder.set_algorithms(cytiq_compensate::UPSTREAM);
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let result = CompensateRunner::run(&config)?;
    info!(
        "compensated {} channel(s) over {} event(s); {} on a raw detector margin; wrote {} and {}",
        result.channels.len(),
        result.event_count,
        result.margin_event_count,
        result.output.display(),
        result.masks.display()
    );
    record_provenance(
        recorder,
        &[
            result.output.clone(),
            result.masks.clone(),
            cytiq_compensate::margin_diagnostics_path(&result.output),
        ],
        &record_path_for_file(&result.output),
    )
}
