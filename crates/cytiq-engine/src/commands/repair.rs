use std::io::{self, Write as _};
use std::path::PathBuf;

use clap::{Args, ValueEnum};
use cytiq_provenance::outcome::OutcomeReason;
use cytiq_provenance::record_path_for_file;
use cytiq_repair::{
    PlanDocument, RepairCode, RepairConfig, RepairError, RepairRunner, report_path,
};
use log::info;
use serde::Serialize;

use crate::provenance::publish;

#[derive(Debug, Serialize, Args)]
pub(crate) struct RepairArgs {
    /// FCS file, as the instrument wrote it.
    #[arg(long)]
    input: PathBuf,
    /// Repaired FCS file; its report and provenance record are written beside
    /// it.
    #[arg(long)]
    output: Option<PathBuf>,
    /// A repair to make; can be repeated. Without it nothing is repaired.
    #[arg(long, value_enum)]
    allow: Vec<CliRepairCode>,
    /// Print the diagnosis as JSON and write nothing.
    #[arg(long, conflicts_with_all = ["output", "allow", "overwrite"])]
    plan: bool,
    /// Replace an existing output.
    #[arg(long)]
    overwrite: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub(crate) enum CliRepairCode {
    TextTrailingPadding,
    TextOverlapsData,
    TextEmptyValues,
    DataEndExclusive,
    SupplementalTextInsideText,
    AnalysisOffsetsNegative,
    UndeclaredBytes,
}

impl From<CliRepairCode> for RepairCode {
    fn from(code: CliRepairCode) -> Self {
        match code {
            CliRepairCode::TextTrailingPadding => Self::TextTrailingPadding,
            CliRepairCode::TextOverlapsData => Self::TextOverlapsData,
            CliRepairCode::TextEmptyValues => Self::TextEmptyValues,
            CliRepairCode::DataEndExclusive => Self::DataEndExclusive,
            CliRepairCode::SupplementalTextInsideText => Self::SupplementalTextInsideText,
            CliRepairCode::AnalysisOffsetsNegative => Self::AnalysisOffsetsNegative,
            CliRepairCode::UndeclaredBytes => Self::UndeclaredBytes,
        }
    }
}

pub(crate) fn run(
    args: RepairArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    if args.plan {
        return plan(&args.input);
    }
    let Some(output) = args.output else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pass `--output` to repair the file, or `--plan` to diagnose it",
        )
        .into());
    };
    let config = RepairConfig {
        input: args.input,
        output,
        allow: args.allow.into_iter().map(RepairCode::from).collect(),
        overwrite: args.overwrite,
    };
    info!(
        "starting repair: input={}, output={}",
        config.input.display(),
        config.output.display()
    );
    recorder.snapshot_roles(&[("input", config.input.clone())])?;
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let prepared = RepairRunner::prepare(&config)?;
    let outputs = [config.output.clone(), report_path(&config.output)];
    let outcome = publish(
        recorder,
        prepared.publication,
        &outputs,
        &record_path_for_file(&config.output),
        OutcomeReason::Success,
    )?;
    info!(
        "repaired {} event(s) of acquisition {}; wrote {}",
        prepared.summary.data.events,
        prepared.summary.acquisition_id,
        config.output.display()
    );
    Ok(outcome)
}

fn plan(input: &std::path::Path) -> crate::provenance::CommandResult {
    let document = PlanDocument::of(input)?;
    let mut text = serde_json::to_vec_pretty(&document).map_err(RepairError::Serialize)?;
    text.push(b'\n');
    io::stdout()
        .lock()
        .write_all(&text)
        .map_err(RepairError::Write)?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_repair_can_be_allowed_by_its_code() {
        let mut allowed = Vec::new();
        for code in CliRepairCode::value_variants() {
            let name = code.to_possible_value().unwrap().get_name().to_owned();
            let repair = RepairCode::from(*code);
            assert_eq!(name, repair.as_str());
            allowed.push(repair);
        }
        assert_eq!(allowed, RepairCode::ALL);
    }
}
