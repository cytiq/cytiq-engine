use std::path::PathBuf;

use clap::Args;
use cytiq_gating::{GatingConfig, GatingRunner};
use cytiq_provenance::record_path_for_file;
use log::{debug, info};
use serde::Serialize;

use crate::cli::CliGateOutputFormat;
use crate::parsers::parse_threads;
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Args)]
pub(crate) struct GateArgs {
    /// Input FCS file.
    #[arg(long)]
    input: PathBuf,
    /// Gating-ML 2.0 XML file with the gate definitions.
    #[arg(long)]
    gating_ml: PathBuf,
    /// Output path for the per-gate counts report.
    #[arg(long)]
    output: PathBuf,
    #[arg(long, value_enum, default_value_t = CliGateOutputFormat::Csv)]
    format: CliGateOutputFormat,
    /// Worker thread limit; defaults to all available logical CPUs.
    #[arg(long, value_parser = parse_threads)]
    pub(crate) threads: Option<usize>,
    /// Event mask Parquet (`masks.parquet`) restricting the gated events;
    /// repeat to combine several masks with AND.
    #[arg(long = "mask")]
    masks: Vec<PathBuf>,
    /// Optional Parquet path for per-event gate membership.
    #[arg(long)]
    membership: Option<PathBuf>,
}

pub(crate) fn run(
    args: GateArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let GateArgs {
        input,
        gating_ml,
        output,
        format,
        threads: _,
        masks,
        membership,
    } = args;
    info!(
        "starting gate: input={}, gating_ml={}, output={}",
        input.display(),
        gating_ml.display(),
        output.display()
    );
    let config = GatingConfig {
        input,
        gating_ml,
        output,
        format: format.into(),
        masks,
        membership,
    };
    recorder.snapshot_inputs("input", std::slice::from_ref(&config.input))?;
    recorder.snapshot_inputs("gating_ml", std::slice::from_ref(&config.gating_ml))?;
    recorder.snapshot_inputs("mask", &config.masks)?;
    recorder.set_algorithms(cytiq_gating::UPSTREAM);
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let result = GatingRunner::run(&config)?;
    let mut outputs = vec![config.output.clone()];
    outputs.extend(config.membership.clone());
    for gate in &result.gates {
        debug!(
            "gate `{}`: {} event(s), {:.2}% of parent",
            gate.gate_path, gate.count, gate.percent_of_parent
        );
    }
    info!(
        "gate evaluated {} gate(s) over {} event(s); wrote {}",
        result.gates.len(),
        result.event_count,
        result.output.display()
    );
    record_provenance(recorder, &outputs, &record_path_for_file(&config.output))
}
