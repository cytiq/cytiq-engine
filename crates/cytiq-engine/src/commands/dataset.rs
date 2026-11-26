use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use cytiq_population::analytics::{AnalyticsRunner, ExportDatasetConfig};
use cytiq_population::dataset::{
    Profile, ReportStatus, ValidateConfig, ValidateOptions, ValidateRunner,
};
use cytiq_population::issue::Severity;
use cytiq_provenance::outcome::OutcomeReason;
use cytiq_provenance::record_path_for_file;
use log::{error, info, warn};
use serde::Serialize;

#[derive(Debug, Serialize, Subcommand)]
pub(crate) enum DatasetCommand {
    /// Validate a dataset manifest: references, panels, signal states,
    /// event identities, channel order, metadata and privacy, and with
    /// `--verify-objects` every object's size, digest and content. Writes a
    /// JSON report; exits with 3 after writing it when the dataset has
    /// errors.
    Validate(DatasetValidateArgs),
    /// Write the analytics product of a dataset: path-free Parquet tables
    /// of its subjects, samples, design factors, panels, label tables with
    /// exact counts, and flattened population observations, plus
    /// `views.sql` (`DuckDB` views, cohort statistics per subject and inputs
    /// for diffcyt) and `product.json`. Observations must agree with the
    /// label tables of the same native output, or nothing is published.
    Export(DatasetExportArgs),
}

#[derive(Debug, Serialize, Args)]
pub(crate) struct DatasetExportArgs {
    /// Dataset manifest (`.json`).
    #[arg(long)]
    manifest: PathBuf,
    /// Product directory; its provenance record is written in it.
    #[arg(long)]
    output: PathBuf,
    /// Directory the manifest's object keys resolve against; the
    /// manifest's directory by default.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Replace an existing product directory.
    #[arg(long)]
    overwrite: bool,
}

impl From<DatasetExportArgs> for ExportDatasetConfig {
    fn from(args: DatasetExportArgs) -> Self {
        Self {
            manifest: args.manifest,
            root: args.root,
            output: args.output,
            overwrite: args.overwrite,
        }
    }
}

fn export(
    args: DatasetExportArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let config = ExportDatasetConfig::from(args);
    info!(
        "starting dataset export: manifest={}, output={}",
        config.manifest.display(),
        config.output.display()
    );
    recorder.snapshot_roles(&AnalyticsRunner::input_files(&config)?)?;
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::Dir(config.output.clone())
    );
    let prepared = AnalyticsRunner::prepare(&config)?;
    let product = prepared.summary;
    let outcome = crate::provenance::publish(
        recorder,
        prepared.publication,
        &AnalyticsRunner::outputs(&config, &product),
        &config.record_path(),
        OutcomeReason::Success,
    )?;
    info!(
        "dataset export: {} table(s), {} view(s), {} label table(s) checked against \
         observations, cohort views {}; wrote {}",
        product.tables.len(),
        product.views.names.len(),
        product.parity.len(),
        if product.cohort.available {
            "included"
        } else {
            "omitted"
        },
        config.output.display()
    );
    Ok(outcome)
}

#[derive(Debug, Serialize, Args)]
pub(crate) struct DatasetValidateArgs {
    /// Dataset manifest (`.json`).
    #[arg(long)]
    manifest: PathBuf,
    /// JSON report path.
    #[arg(long)]
    output: PathBuf,
    /// Directory the manifest's object keys resolve against; the
    /// manifest's directory by default.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Read every object the manifest lists.
    #[arg(long)]
    verify_objects: bool,
    /// Metadata the dataset must carry.
    #[arg(long, value_enum, default_value = "structural")]
    profile: CliProfile,
    /// Replace an existing report.
    #[arg(long)]
    overwrite: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliProfile {
    Structural,
    MiflowcytMinimal,
}

impl From<CliProfile> for Profile {
    fn from(value: CliProfile) -> Self {
        match value {
            CliProfile::Structural => Self::Structural,
            CliProfile::MiflowcytMinimal => Self::MiflowcytMinimal,
        }
    }
}

impl From<DatasetValidateArgs> for ValidateConfig {
    fn from(args: DatasetValidateArgs) -> Self {
        Self {
            manifest: args.manifest,
            output: args.output,
            options: ValidateOptions {
                profile: args.profile.into(),
                root: args.root,
                verify_objects: args.verify_objects,
            },
            overwrite: args.overwrite,
        }
    }
}

pub(crate) fn run(
    command: DatasetCommand,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let args = match command {
        DatasetCommand::Validate(args) => args,
        DatasetCommand::Export(args) => return export(args, recorder),
    };
    let config = ValidateConfig::from(args);
    info!(
        "starting dataset validate: manifest={}, profile={}, verify_objects={}",
        config.manifest.display(),
        config.options.profile.as_str(),
        config.options.verify_objects
    );
    recorder.snapshot_roles(&ValidateRunner::input_files(&config))?;
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let prepared = ValidateRunner::prepare(&config)?;
    let report = prepared.summary;
    for issue in &report.issues {
        let line = format!("{:?} at `{}`: {}", issue.code, issue.pointer, issue.message);
        match issue.severity {
            Severity::Error => error!("{line}"),
            Severity::Warning => warn!("{line}"),
        }
    }
    info!(
        "dataset validate: {} error(s), {} warning(s); wrote {}",
        report.errors,
        report.warnings,
        config.output.display()
    );
    let reason = match report.status {
        ReportStatus::Valid => OutcomeReason::Success,
        ReportStatus::Invalid => OutcomeReason::ChecksFailed,
    };
    let mut outcome = crate::provenance::publish(
        recorder,
        prepared.publication,
        std::slice::from_ref(&config.output),
        &record_path_for_file(&config.output),
        reason,
    )?;
    if let Some(outcome) = &mut outcome
        && report.status == ReportStatus::Invalid
    {
        outcome.message = format!(
            "dataset has {} error(s); see {}",
            report.errors,
            config.output.display()
        );
    }
    Ok(outcome)
}
