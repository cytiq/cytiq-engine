use std::path::PathBuf;

use clap::Args;
use cytiq_provenance::record_path_for_file;
use cytiq_qc::{CheckStatus as QcStatus, QcConfig, QcRunner};
use log::{debug, error, info, warn};
use serde::Serialize;

#[derive(Debug, Serialize, Args)]
pub(crate) struct QcArgs {
    /// Run manifest (`.yaml`, `.yml` or `.json`) with the sample.
    #[arg(long)]
    manifest: PathBuf,
    /// Sample id in the manifest.
    #[arg(long)]
    sample: String,
    /// The sample's final event mask (`cytiq mask combine` output).
    #[arg(long)]
    mask: PathBuf,
    /// JSON report path.
    #[arg(long)]
    output: PathBuf,
    /// Replace an existing report.
    #[arg(long)]
    overwrite: bool,
    /// Fail when the sample's panel has no `qc` policy instead of
    /// warning; use it where the report decides whether a sample is
    /// released.
    #[arg(long)]
    require_policy: bool,
    /// Gate membership of every raw event (`cytiq gate --membership`
    /// without `--mask`). Adds each gate's retention after QC filtering to
    /// the report; descriptive only, it never changes a check or the exit
    /// code.
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    populations: Option<PathBuf>,
}

pub(crate) fn run(
    args: QcArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let QcArgs {
        manifest,
        sample,
        mask,
        output,
        overwrite,
        require_policy,
        populations,
    } = args;
    info!(
        "starting qc: manifest={}, sample={sample}, mask={}",
        manifest.display(),
        mask.display()
    );
    let config = QcConfig {
        manifest,
        sample,
        mask,
        output,
        overwrite,
        require_policy,
        populations,
    };
    recorder.snapshot_roles(&QcRunner::input_files(&config)?)?;
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let report = QcRunner::run(&config)?;
    for check in &report.checks {
        match check.status {
            QcStatus::Fail => {
                error!("{}: {}: {}", check.scope, check.check, check.message);
            }
            QcStatus::Warn => {
                warn!("{}: {}: {}", check.scope, check.check, check.message);
            }
            QcStatus::Pass => {
                debug!("{}: {}: {}", check.scope, check.check, check.message);
            }
        }
    }
    if let Some(retention) = &report.population_retention {
        match (&retention.reason, &retention.combined) {
            (Some(reason), _) => warn!("qc: population retention not assessed: {reason}"),
            (None, Some(combined)) => {
                for population in &combined.populations {
                    info!(
                        "qc: population `{}` retained {}/{} event(s)",
                        population.population, population.retained, population.original
                    );
                }
            }
            (None, None) => {}
        }
    }
    info!(
        "qc: {} passed, {} warning(s), {} failure(s); wrote {}",
        report.passed,
        report.warnings,
        report.failures,
        config.output.display()
    );
    let failure = report.ensure_passed(&config.output).err();
    let reason = if failure.is_some() {
        cytiq_provenance::outcome::OutcomeReason::ChecksFailed
    } else {
        cytiq_provenance::outcome::OutcomeReason::Success
    };
    let mut outcome = crate::provenance::record_with_reason(
        recorder,
        std::slice::from_ref(&config.output),
        &record_path_for_file(&config.output),
        reason,
    )?;
    if let (Some(outcome), Some(failure)) = (&mut outcome, failure) {
        outcome.message = failure.to_string();
    }
    Ok(outcome)
}
