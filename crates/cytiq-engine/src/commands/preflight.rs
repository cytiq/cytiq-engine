use std::path::PathBuf;

use clap::Args;
use cytiq_provenance::record_path_for_file;
use cytiq_qc::preflight::{CheckStatus, PreflightConfig, PreflightRunner};
use log::{debug, error, info, warn};
use serde::Serialize;

#[derive(Debug, Serialize, Args)]
pub(crate) struct PreflightArgs {
    /// Run manifest (`.yaml`, `.yml` or `.json`).
    #[arg(long)]
    manifest: PathBuf,
    /// JSON report path.
    #[arg(long)]
    output: PathBuf,
    /// Replace an existing report.
    #[arg(long)]
    overwrite: bool,
}

pub(crate) fn run(
    args: PreflightArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let PreflightArgs {
        manifest,
        output,
        overwrite,
    } = args;
    info!(
        "starting preflight: manifest={}, output={}",
        manifest.display(),
        output.display()
    );
    let config = PreflightConfig {
        manifest,
        output,
        overwrite,
    };
    // An invalid manifest fails the run below with the same error.
    let inputs = PreflightRunner::input_files(&config)
        .unwrap_or_else(|_| vec![("manifest", config.manifest.clone())]);
    recorder.snapshot_roles(&inputs)?;
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let prepared = PreflightRunner::prepare(&config)?;
    let report = prepared.summary;
    for check in &report.checks {
        match check.status {
            CheckStatus::Fail => {
                error!("{}: {}: {}", check.scope, check.check, check.message);
            }
            CheckStatus::Warn => {
                warn!("{}: {}: {}", check.scope, check.check, check.message);
            }
            CheckStatus::Pass => {
                debug!("{}: {}: {}", check.scope, check.check, check.message);
            }
        }
    }
    info!(
        "preflight: {} passed, {} warning(s), {} failure(s); wrote {}",
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
    let mut outcome = crate::provenance::publish(
        recorder,
        prepared.publication,
        std::slice::from_ref(&config.output),
        &record_path_for_file(&config.output),
        reason,
    )?;
    if let (Some(outcome), Some(failure)) = (&mut outcome, failure) {
        outcome.message = failure.to_string();
    }
    Ok(outcome)
}
