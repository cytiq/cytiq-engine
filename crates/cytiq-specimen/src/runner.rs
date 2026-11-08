//! Package workflows behind the CLI; all output layout lives in this library.

use crate::{
    Result, SpecimenLayout, SpecimenPlan,
    reconcile::{SourceObservations, reconcile},
    require,
};
use cytiq_manifest::RunManifest;
use cytiq_population::{
    EventAnnotations, ObserveConfig,
    artifact::{PackageBegin, begin_package, verify_package, write_json},
    serialization, table,
};
use cytiq_provenance::{
    RunRecorder,
    outcome::{CommandOutcome, OutcomeReason},
};
use cytiq_qc::CheckStatus;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct PlanConfig {
    pub manifest: PathBuf,
    pub layout: PathBuf,
    pub specimen: String,
    pub selections: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ObserveRequest {
    pub plan: PathBuf,
    pub acquisition: String,
    pub source: String,
    pub annotations: PathBuf,
    pub producer_provenance: PathBuf,
    pub mask: PathBuf,
    pub qc_report: PathBuf,
    pub qc_provenance: PathBuf,
}

fn selections(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut selections = BTreeMap::new();
    for value in values {
        let (panel, acquisition) = value.split_once('=').ok_or_else(|| {
            crate::SpecimenError::Invalid("selection must be PANEL=ACQUISITION".into())
        })?;
        require(
            !panel.is_empty()
                && !acquisition.is_empty()
                && selections
                    .insert(panel.to_owned(), acquisition.to_owned())
                    .is_none(),
            "empty or duplicate panel selection",
        )?;
    }
    Ok(selections)
}

fn build(config: &PlanConfig, recorder: &mut RunRecorder) -> Result<SpecimenPlan> {
    let manifest = RunManifest::load(&config.manifest)?;
    let layout = SpecimenLayout::load(&config.layout)?;
    let mut files = vec![config.manifest.clone(), config.layout.clone()];
    files.extend(layout.input_files());
    files.extend(
        manifest
            .samples
            .iter()
            .filter(|s| s.specimen.as_deref() == Some(&config.specimen))
            .map(|s| s.file.clone()),
    );
    recorder.snapshot_inputs("input", &files)?;
    SpecimenPlan::build(
        &config.manifest,
        &config.layout,
        &config.specimen,
        &selections(&config.selections)?,
    )
}

/// # Errors
/// Returns configuration, definition or acquisition validation failures.
pub fn validate_plan(config: &PlanConfig, mut recorder: RunRecorder) -> Result<CommandOutcome> {
    let plan = build(config, &mut recorder)?;
    recorder.prepare_staged(&[], cytiq_provenance::outcome::OutcomeReason::Success)?;
    Ok(CommandOutcome::new(
        "cytiq specimen validate",
        true,
        if plan.missing_required() {
            OutcomeReason::Incomplete
        } else {
            OutcomeReason::Success
        },
        String::new(),
    ))
}

/// # Errors
/// Rejects invalid input and output conflicts without replacing prior results.
pub fn write_plan(
    config: &PlanConfig,
    output: &Path,
    overwrite: bool,
    reuse: cytiq_provenance::reuse::ReusePolicy,
    mut recorder: RunRecorder,
) -> Result<CommandOutcome> {
    let plan = build(config, &mut recorder)?;
    let run = match begin_package(output, overwrite, "cytiq specimen plan", reuse, recorder)? {
        PackageBegin::Reused(outcome) => return Ok(outcome),
        PackageBegin::Compute(run) => run,
    };
    let reason = if plan.missing_required() {
        OutcomeReason::Incomplete
    } else {
        OutcomeReason::Success
    };
    Ok(run.write("cytiq specimen plan", reason, |staged| {
        write_json(&staged.join("plan.json"), &plan)?;
        Ok(vec!["plan.json".into()])
    })?)
}

fn prepare_observe(request: &ObserveRequest, recorder: &mut RunRecorder) -> Result<ObserveConfig> {
    recorder.snapshot_inputs("plan", &plan_files(&request.plan))?;
    let plan = SpecimenPlan::load(&request.plan)?;
    let context = plan.context(&request.acquisition, &request.source)?;
    let config = ObserveConfig {
        context,
        annotations: request.annotations.clone(),
        producer_provenance: request.producer_provenance.clone(),
        mask: request.mask.clone(),
        qc_report: request.qc_report.clone(),
        qc_provenance: request.qc_provenance.clone(),
    };
    recorder.snapshot_inputs("input", &config.input_files())?;
    Ok(config)
}

/// Validate a source using exactly the checks used by observe; publish nothing.
/// # Errors
/// Returns invalid annotation/lineage/provenance errors.
pub fn validate_population(
    request: &ObserveRequest,
    mut recorder: RunRecorder,
) -> Result<CommandOutcome> {
    let config = prepare_observe(request, &mut recorder)?;
    let (annotations, _) = cytiq_population::observe(&config)?;
    recorder.prepare_staged(&[], cytiq_provenance::outcome::OutcomeReason::Success)?;
    Ok(CommandOutcome::new(
        "cytiq population validate",
        true,
        if annotations.technical_status == "fail" {
            OutcomeReason::ChecksFailed
        } else if annotations.unknown_events > 0 || annotations.abstained_events.unwrap_or(0) > 0 {
            OutcomeReason::Incomplete
        } else {
            OutcomeReason::Success
        },
        String::new(),
    ))
}

/// # Errors
/// Rejects invalid sources and publication failures.
pub fn write_observations(
    request: &ObserveRequest,
    output: &Path,
    overwrite: bool,
    reuse: cytiq_provenance::reuse::ReusePolicy,
    mut recorder: RunRecorder,
) -> Result<CommandOutcome> {
    let config = prepare_observe(request, &mut recorder)?;
    let run = match begin_package(
        output,
        overwrite,
        "cytiq population observe",
        reuse,
        recorder,
    )? {
        PackageBegin::Reused(outcome) => return Ok(outcome),
        PackageBegin::Compute(run) => run,
    };
    let (annotations, observations, assessment) =
        cytiq_population::annotations::observe_with_release(&config)?;
    let reason = if annotations.technical_status == "fail" {
        OutcomeReason::ChecksFailed
    } else if annotations.unknown_events > 0 || annotations.abstained_events.unwrap_or(0) > 0 {
        OutcomeReason::Incomplete
    } else {
        OutcomeReason::Success
    };
    Ok(run.write("cytiq population observe", reason, |staged| {
        write_json(&staged.join("release-qc.json"), &assessment)?;
        write_json(&staged.join("event-annotations.json"), &annotations)?;
        table::write(&staged.join("observations.parquet"), &observations)?;
        Ok(vec![
            "event-annotations.json".into(),
            "observations.parquet".into(),
            "release-qc.json".into(),
        ])
    })?)
}

/// Assemble verified summaries; never opens FCS files or native annotation Parquet.
/// # Errors
/// Rejects invalid packages, conflicting sources and publication failures.
pub fn run_specimen(
    plan_path: &Path,
    packages: &[PathBuf],
    output: &Path,
    overwrite: bool,
    reuse: cytiq_provenance::reuse::ReusePolicy,
    mut recorder: RunRecorder,
) -> Result<CommandOutcome> {
    recorder.snapshot_inputs("plan", &plan_files(plan_path))?;
    let plan = SpecimenPlan::load(plan_path)?;
    let mut sources = Vec::new();
    let mut release_assessments = Vec::new();
    for package in packages {
        let names = [
            "event-annotations.json",
            "observations.parquet",
            "provenance.json",
            "outcome.json",
        ];
        recorder.snapshot_inputs("observations", &names.map(|name| package.join(name)))?;
        let outcome = verify_package(package)?;
        require(
            outcome.command == "cytiq population observe"
                && names[..2]
                    .iter()
                    .all(|name| outcome.artifacts.iter().any(|a| &a.path == name)),
            "not a population observation package",
        )?;
        let annotations: EventAnnotations = serde_json::from_value(serialization::read(
            &package.join("event-annotations.json"),
        )?)?;
        let observations = table::read(&package.join("observations.parquet"))?;
        let (status, message) = if outcome
            .artifacts
            .iter()
            .any(|artifact| artifact.path == "release-qc.json")
        {
            let path = package.join("release-qc.json");
            recorder.snapshot_inputs("release_qc", std::slice::from_ref(&path))?;
            let status = cytiq_population::annotations::validate_release_assessment(
                &serialization::read(&path)?,
                &annotations,
            )?;
            (
                status,
                "panel policy and mandatory assessments were checked before population publication",
            )
        } else {
            (
                CheckStatus::Fail,
                "source has no pinned release QC assessment; regenerate the population package",
            )
        };
        release_assessments.push(crate::reconcile::SpecimenCheck {
            category: "technical".into(),
            scope: format!(
                "{}/{}",
                annotations.context.panel_id, annotations.context.source_id
            ),
            check: "release_assessment".into(),
            status,
            message: message.into(),
        });
        sources.push(SourceObservations {
            annotations,
            observations,
        });
    }
    let run = match begin_package(output, overwrite, "cytiq specimen run", reuse, recorder)? {
        PackageBegin::Reused(outcome) => return Ok(outcome),
        PackageBegin::Compute(run) => run,
    };
    let mut result = reconcile(&plan, &sources)?;
    result.checks.extend(release_assessments);
    result.status = result
        .checks
        .iter()
        .map(|check| check.status)
        .max()
        .unwrap_or(CheckStatus::Pass);
    let reason = if !result.complete {
        OutcomeReason::Incomplete
    } else if result.status == CheckStatus::Fail {
        OutcomeReason::ChecksFailed
    } else {
        OutcomeReason::Success
    };
    Ok(run.write("cytiq specimen run", reason, |staged| {
        write_json(&staged.join("result.json"), &result)?;
        table::write(&staged.join("results.parquet"), &result.results)?;
        Ok(vec!["result.json".into(), "results.parquet".into()])
    })?)
}

fn plan_files(path: &Path) -> Vec<PathBuf> {
    let parent = path.parent().unwrap_or(Path::new("."));
    vec![
        path.to_owned(),
        parent.join("outcome.json"),
        parent.join("provenance.json"),
    ]
}

#[derive(Clone, Debug)]
pub struct CompareConfig {
    pub baseline: PathBuf,
    pub candidate: PathBuf,
    pub policy: Option<PathBuf>,
    pub output: PathBuf,
    pub overwrite: bool,
    pub reuse: cytiq_provenance::reuse::ReusePolicy,
}

pub(crate) fn snapshot_package(
    directory: &Path,
    role: &str,
    recorder: &mut RunRecorder,
) -> Result<CommandOutcome> {
    let outcome = verify_package(directory)?;
    let files: Vec<_> = outcome
        .artifacts
        .iter()
        .map(|a| directory.join(&a.path))
        .chain(std::iter::once(directory.join("outcome.json")))
        .collect();
    recorder.snapshot_inputs(role, &files)?;
    Ok(outcome)
}

pub(crate) fn package_identity(
    directory: &Path,
    outcome: &CommandOutcome,
) -> Result<crate::compare::PackageIdentity> {
    let record: cytiq_provenance::RunRecord =
        serde_json::from_value(serialization::read(&directory.join("provenance.json"))?)?;
    require(
        (cytiq_provenance::MIN_READ_SCHEMA_VERSION..=cytiq_provenance::SCHEMA_VERSION)
            .contains(&record.schema_version),
        "unsupported source provenance version",
    )?;
    Ok(crate::compare::PackageIdentity {
        artifacts: outcome.artifacts.clone(),
        version: record.version,
        build: record.build,
    })
}

/// Compare and publish standalone summaries, including failed or unavailable comparisons.
/// # Errors
/// Rejects invalid packages/policy, differing specimen identities and output conflicts.
pub fn write_comparison(
    config: &CompareConfig,
    mut recorder: RunRecorder,
) -> Result<CommandOutcome> {
    let a = snapshot_package(&config.baseline, "baseline", &mut recorder)?;
    let b = snapshot_package(&config.candidate, "candidate", &mut recorder)?;
    let baseline = crate::SpecimenResult::load(&config.baseline)?;
    let candidate = crate::SpecimenResult::load(&config.candidate)?;
    let policy = if let Some(path) = &config.policy {
        recorder.snapshot_inputs("policy", std::slice::from_ref(path))?;
        crate::ComparisonPolicy::load(path)?
    } else {
        crate::ComparisonPolicy::default()
    };
    let run = match begin_package(
        &config.output,
        config.overwrite,
        "cytiq specimen compare",
        config.reuse,
        recorder,
    )? {
        PackageBegin::Reused(outcome) => return Ok(outcome),
        PackageBegin::Compute(run) => run,
    };
    let mut comparison = crate::compare(&baseline, &candidate, &policy)?;
    comparison.baseline_package = Some(package_identity(&config.baseline, &a)?);
    comparison.candidate_package = Some(package_identity(&config.candidate, &b)?);
    Ok(run.write(
        "cytiq specimen compare",
        comparison.outcome_reason(),
        |staged| {
            let path = staged.join("comparison.json");
            write_json(&path, &comparison)?;
            // Do not publish a comparison that its bounded native reader cannot reopen.
            serialization::read(&path)?;
            Ok(vec!["comparison.json".into()])
        },
    )?)
}

/// Map specimen/population failures onto the exit contract shared by every
/// command: I/O follows its kind, unreadable or inconsistent content is
/// invalid input, an existing output a conflict.
#[must_use]
pub fn error_reason(error: &crate::SpecimenError) -> OutcomeReason {
    use crate::SpecimenError as S;
    match error {
        S::Json(source) => json_reason(source),
        S::Population(source) => population_reason(source),
        S::Manifest(cytiq_manifest::ManifestError::Read { source, .. })
        | S::Fcs(cytiq_fcs::Error::Io(source))
        | S::Io(source) => OutcomeReason::of_io(source),
        S::Invalid(_) | S::Manifest(_) | S::Fcs(_) => OutcomeReason::InvalidConfiguration,
        // CSV is only written (report tables).
        S::Csv(source) => match source.kind() {
            csv::ErrorKind::Io(source) => OutcomeReason::of_io(source),
            _ => OutcomeReason::RuntimeError,
        },
        S::Provenance(source) => source.outcome_reason(),
        S::Publish(source) => OutcomeReason::of_publish(source),
    }
}

fn population_reason(error: &cytiq_population::PopulationError) -> OutcomeReason {
    use cytiq_population::PopulationError as P;
    match error {
        P::Missing(_) => OutcomeReason::Incomplete,
        P::Invalid(_) | P::Arrow(_) | P::Parquet(_) => OutcomeReason::InvalidConfiguration,
        P::Json(source) => json_reason(source),
        P::Io(source) => OutcomeReason::of_io(source),
        P::Publish(source) => OutcomeReason::of_publish(source),
        P::Provenance(source) => source.outcome_reason(),
    }
}

fn json_reason(error: &serde_json::Error) -> OutcomeReason {
    error.io_error_kind().map_or(
        OutcomeReason::InvalidConfiguration,
        OutcomeReason::of_io_kind,
    )
}
