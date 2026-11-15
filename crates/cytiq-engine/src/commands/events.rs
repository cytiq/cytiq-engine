use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use cytiq_population::events::{BaseState, ExportConfig, ExportRunner};
use cytiq_provenance::record_path_for_file;
use log::info;
use serde::Serialize;

use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Subcommand)]
pub(crate) enum EventsCommand {
    /// Write one FCS file's events as a `cytiq.events.v1` Parquet table:
    /// acquisition, signal layer and raw event id in every row, then the
    /// channels in `$Pn` order, bit-exact. No identifying FCS keyword is
    /// written.
    Export(EventsExportArgs),
}

#[derive(Debug, Serialize, Args)]
pub(crate) struct EventsExportArgs {
    /// FCS file: raw, or written by a cytiq stage.
    #[arg(long)]
    input: PathBuf,
    /// Events table (`.parquet`); its provenance record is written beside it.
    #[arg(long)]
    output: PathBuf,
    /// What the values were before any cytiq stage, for files compensated
    /// or unmixed by other software (the panel's `signal_state`).
    #[arg(long, value_enum, default_value = "raw")]
    base_state: CliBaseState,
    /// The matrix file `cytiq compensate --matrix` applied to the input;
    /// required for such files, recorded only as its SHA-256.
    #[arg(long)]
    compensation_matrix: Option<PathBuf>,
    /// The reference `cytiq autospectral` unmixed the input with; required
    /// for unmixed files, recorded only as its SHA-256.
    #[arg(long)]
    unmixing_reference: Option<PathBuf>,
    /// Replace an existing table.
    #[arg(long)]
    overwrite: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliBaseState {
    Raw,
    Compensated,
    Unmixed,
}

impl From<CliBaseState> for BaseState {
    fn from(value: CliBaseState) -> Self {
        match value {
            CliBaseState::Raw => Self::Raw,
            CliBaseState::Compensated => Self::Compensated,
            CliBaseState::Unmixed => Self::Unmixed,
        }
    }
}

impl From<EventsExportArgs> for ExportConfig {
    fn from(args: EventsExportArgs) -> Self {
        Self {
            input: args.input,
            output: args.output,
            base_state: args.base_state.into(),
            compensation_matrix: args.compensation_matrix,
            unmixing_reference: args.unmixing_reference,
            overwrite: args.overwrite,
        }
    }
}

pub(crate) fn run(
    command: EventsCommand,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let EventsCommand::Export(args) = command;
    let config = ExportConfig::from(args);
    info!(
        "starting events export: input={}, output={}",
        config.input.display(),
        config.output.display()
    );
    recorder.snapshot_roles(&ExportRunner::input_files(&config))?;
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let summary = ExportRunner::run(&config)?;
    info!(
        "exported {} event(s) of acquisition {} as a {:?} layer ({} channel(s), layer {}); wrote {}",
        summary.rows,
        summary.descriptor.acquisition_id,
        summary.descriptor.state,
        summary.descriptor.channels.len(),
        summary.layer_id,
        config.output.display()
    );
    record_provenance(
        recorder,
        std::slice::from_ref(&config.output),
        &record_path_for_file(&config.output),
    )
}
