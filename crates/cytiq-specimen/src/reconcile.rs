//! Reconciliation operates on summary observations; no FCS/event-level I/O.

use crate::{
    Result, SpecimenPlan,
    layout::{AnchorLimits, SourceRef, Strategy},
    require,
};
use cytiq_population::{
    EventAnnotations, Measure, Method, ObservationStatus, PopulationObservation, Target,
    annotations::AnnotationFormat, observation::Lineage, serialization,
};
use cytiq_qc::CheckStatus;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct SourceObservations {
    pub annotations: EventAnnotations,
    pub observations: Vec<PopulationObservation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecimenCheck {
    pub category: String,
    pub scope: String,
    pub check: String,
    pub status: CheckStatus,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecimenResult {
    pub schema_version: u32,
    pub specimen_id: String,
    pub protocol_id: String,
    pub complete: bool,
    pub status: CheckStatus,
    pub observations: Vec<PopulationObservation>,
    pub results: Vec<PopulationObservation>,
    pub checks: Vec<SpecimenCheck>,
}

impl SpecimenResult {
    /// Read a completed package, validating versions, quantities and JSON/Parquet agreement.
    /// # Errors
    /// Rejects corrupt or internally inconsistent result packages.
    pub fn load(directory: &std::path::Path) -> Result<Self> {
        let outcome = cytiq_population::verify_package(directory)?;
        require(
            outcome.command == "cytiq specimen run",
            "not a specimen result package",
        )?;
        let result: Self =
            serde_json::from_value(serialization::read(&directory.join("result.json"))?)?;
        result.validate()?;
        require(
            cytiq_population::table::read(&directory.join("results.parquet"))? == result.results,
            "result JSON/Parquet disagreement",
        )?;
        require(
            outcome.reason == result.outcome_reason(),
            "outcome and specimen result disagree",
        )?;
        Ok(result)
    }

    /// Validate a summary snapshot without reopening its original input files.
    /// # Errors
    /// Rejects invalid identities, quantities, source references and QC summaries.
    pub fn validate(&self) -> Result<()> {
        let result = self;
        require(
            result.schema_version == 1 && !result.specimen_id.is_empty(),
            "invalid specimen result identity/version",
        )?;
        let mut identities = BTreeSet::new();
        for observation in &result.observations {
            observation.validate()?;
            require(
                observation.specimen_id == result.specimen_id
                    && identities.insert(&observation.observation_id),
                "unexpected or repeated original observation",
            )?;
        }
        for observation in &result.results {
            observation.validate()?;
            require(
                observation.specimen_id == result.specimen_id
                    && observation
                        .method
                        .source_observation_ids
                        .iter()
                        .all(|id| identities.contains(id)),
                "missing reconciliation input observation",
            )?;
        }
        let status = result
            .checks
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(CheckStatus::Pass);
        require(status == result.status, "inconsistent specimen QC summary")?;
        Ok(())
    }

    #[must_use]
    pub fn outcome_reason(&self) -> cytiq_provenance::outcome::OutcomeReason {
        if !self.complete {
            cytiq_provenance::outcome::OutcomeReason::Incomplete
        } else if self.status == CheckStatus::Fail {
            cytiq_provenance::outcome::OutcomeReason::ChecksFailed
        } else {
            cytiq_provenance::outcome::OutcomeReason::Success
        }
    }
}

/// Assemble independent observations and explicitly requested derived results.
/// # Errors
/// Rejects duplicate, unexpected or incompatible source results.
pub fn reconcile(plan: &SpecimenPlan, sources: &[SourceObservations]) -> Result<SpecimenResult> {
    let provided = index_sources(plan, sources)?;
    let mut checks = Vec::new();
    let mut observations = Vec::new();
    let mut complete = true;
    for selection in &plan.selections {
        let panel = &plan.layout.panels[&selection.panel_id];
        for (source_id, definition) in &panel.sources {
            if let Some(source) = provided.get(&(selection.panel_id.as_str(), source_id.as_str())) {
                if source.annotations.technical_status != "fail"
                    && (source.annotations.unknown_events > 0
                        || source.annotations.abstained_events.unwrap_or(0) > 0)
                {
                    complete = false;
                    checks.push(SpecimenCheck {
                        category: "completeness".into(),
                        scope: format!("{}/{}", selection.panel_id, source_id),
                        check: "classification_coverage".into(),
                        status: CheckStatus::Fail,
                        message: "classifier abstained; full population totals are unavailable"
                            .into(),
                    });
                }
                observations.extend(source.observations.iter().cloned());
                checks.push(SpecimenCheck {
                    category: "technical".into(),
                    scope: format!("{}/{}", selection.panel_id, source_id),
                    check: "acquisition_qc".into(),
                    status: match source.annotations.technical_status.as_str() {
                        "pass" => CheckStatus::Pass,
                        "warn" => CheckStatus::Warn,
                        "fail" => CheckStatus::Fail,
                        _ => {
                            return Err(crate::SpecimenError::Invalid(
                                "invalid technical QC status".into(),
                            ));
                        }
                    },
                    message: "technical QC remains separate from biological consistency".into(),
                });
            } else {
                let required = panel.required || selection.selected.is_some();
                complete &= !required;
                checks.push(SpecimenCheck {
                    category: "completeness".into(),
                    scope: format!("{}/{}", selection.panel_id, source_id),
                    check: "source_available".into(),
                    status: if required {
                        CheckStatus::Fail
                    } else {
                        CheckStatus::Warn
                    },
                    message: if selection.selected.is_none() {
                        "missing_panel"
                    } else {
                        "missing_source_output"
                    }
                    .into(),
                });
                observations.extend(missing_observations(
                    plan, selection, source_id, definition,
                )?);
            }
        }
    }
    let mut results = observations
        .iter()
        .filter(|o| {
            !plan.layout.reconciliation.iter().any(|rule| {
                rule.target == o.target && !matches!(rule.strategy, Strategy::Independent)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    apply_rules(plan, &observations, &mut results, &mut checks)?;
    check_consistency(plan, &observations, &mut checks);
    let status = checks
        .iter()
        .map(|c| c.status)
        .max()
        .unwrap_or(CheckStatus::Pass);
    Ok(SpecimenResult {
        schema_version: 1,
        specimen_id: plan.specimen_id.clone(),
        protocol_id: plan.layout.protocol_id.clone(),
        complete,
        status,
        observations,
        results,
        checks,
    })
}

fn matches_source(observation: &PopulationObservation, source: &SourceRef) -> bool {
    observation.panel_id == source.panel && observation.source_id == source.source
}

fn find_count<'a>(
    observations: &'a [PopulationObservation],
    source: &SourceRef,
    target: &Target,
) -> Result<&'a PopulationObservation> {
    observations
        .iter()
        .find(|o| {
            matches_source(o, source) && &o.target == target && o.measure == Measure::ObservedCount
        })
        .ok_or_else(|| {
            crate::SpecimenError::Invalid("configured count is missing from observations".into())
        })
}

fn validate_observations(
    source: &SourceObservations,
    selection: &crate::plan::PanelSelection,
) -> Result<()> {
    let d = &source.annotations;
    let c = &d.context;
    require(
        source.observations.len() == c.source.bindings.len() * 2,
        "incomplete source observation set",
    )?;
    let mut keys = BTreeSet::new();
    for o in &source.observations {
        o.validate()?;
        let key = (
            &o.target,
            match o.measure {
                Measure::ObservedCount => 0,
                Measure::Fraction => 1,
                _ => 2,
            },
        );
        require(
            key.1 != 2
                && keys.insert(key)
                && c.source.bindings.iter().any(|b| b.target == o.target),
            "unexpected or duplicate source observation",
        )?;
        require(
            o.specimen_id == c.specimen_id
                && o.panel_id == c.panel_id
                && o.acquisition_id.as_deref() == Some(&c.acquisition_id)
                && o.source_id == c.source_id
                && o.n_events == d.assessed_events
                && o.lineage.acquisition_sha256.as_deref() == Some(&c.acquisition_sha256)
                && o.lineage.annotations_sha256.as_deref() == Some(&d.data.sha256)
                && o.lineage.producer_provenance_sha256.as_deref()
                    == Some(&d.producer_provenance.sha256)
                && o.lineage.analysis_mask_sha256.as_deref() == Some(&d.analysis_mask.sha256)
                && o.lineage.projection_sha256 == c.projection.sha256
                && o.lineage.tree_sha256 == selection.tree_sha256
                && o.lineage.panel_profile_sha256 == selection.panel_profile_sha256
                && o.method.definition_sha256 == c.source.definition.sha256
                && o.method.name
                    == match c.source.format {
                        AnnotationFormat::GateMembership => "gating",
                        AnnotationFormat::ClassifyPredictions => "classify",
                    }
                && o.method.version == "1"
                && !o.method.experimental
                && o.method.source_observation_ids.is_empty(),
            "observation and annotation lineage disagree",
        )?;
        require(
            (d.technical_status == "fail") == (o.status == ObservationStatus::QcRejected),
            "observation bypasses technical QC rejection",
        )?;
    }
    validate_quantities(source)?;
    Ok(())
}

fn validate_quantities(source: &SourceObservations) -> Result<()> {
    let d = &source.annotations;
    let c = &d.context;
    let counts: BTreeMap<_, _> = source
        .observations
        .iter()
        .filter(|o| o.measure == Measure::ObservedCount)
        .map(|o| (&o.target, o))
        .collect();
    let mut classified = 0_u64;
    for binding in &c.source.bindings {
        let count = counts[&binding.target];
        let fraction = source
            .observations
            .iter()
            .find(|o| o.target == binding.target && o.measure == Measure::Fraction)
            .ok_or_else(|| crate::SpecimenError::Invalid("missing source fraction".into()))?;
        let available = d.technical_status != "fail"
            && d.assessed_events > 0
            && d.unknown_events == 0
            && d.abstained_events.unwrap_or(0) == 0;
        require(
            count.status
                == if d.technical_status == "fail" {
                    ObservationStatus::QcRejected
                } else if available {
                    ObservationStatus::Measured
                } else {
                    ObservationStatus::NotMeasured
                },
            "invalid assessment status",
        )?;
        let denominator = binding
            .denominator
            .as_ref()
            .map_or(d.assessed_events, |label| {
                let target = &c
                    .source
                    .bindings
                    .iter()
                    .find(|b| &b.label == label)
                    .expect("planned binding")
                    .target;
                counts[target]
                    .value_u64
                    .or(counts[target].supporting_count)
                    .unwrap_or(0)
            });
        let expected_target = binding.denominator.as_ref().map(|label| {
            c.source
                .bindings
                .iter()
                .find(|b| &b.label == label)
                .expect("planned denominator")
                .target
                .clone()
        });
        for row in [count, fraction] {
            require(
                row.denominator.as_ref().is_some_and(|d| {
                    d.scope == binding.target.scope
                        && d.target == expected_target
                        && d.observed_count == denominator
                }),
                "source denominator mismatch",
            )?;
        }
        if available && denominator > 0 {
            #[allow(clippy::cast_precision_loss)]
            let expected = count.value_u64.expect("measured count") as f64 / denominator as f64;
            require(
                fraction.status == ObservationStatus::Measured
                    && fraction.value_f64 == Some(expected),
                "source fraction/count mismatch",
            )?;
        } else if available {
            require(
                fraction.status == ObservationStatus::NotApplicable,
                "zero denominator requires not_applicable",
            )?;
        }
        classified = classified
            .checked_add(count.value_u64.or(count.supporting_count).unwrap_or(0))
            .ok_or_else(|| crate::SpecimenError::Invalid("summary count overflow".into()))?;
    }
    if c.source.format == AnnotationFormat::ClassifyPredictions {
        require(
            classified
                .checked_add(d.unknown_events)
                .and_then(|n| n.checked_add(d.abstained_events.unwrap_or(0)))
                == Some(d.assessed_events),
            "classified and unknown counts do not cover assessed events",
        )?;
    }
    Ok(())
}

fn missing_observations(
    plan: &SpecimenPlan,
    selection: &crate::plan::PanelSelection,
    source_id: &str,
    definition: &cytiq_population::annotations::SourceDefinition,
) -> Result<Vec<PopulationObservation>> {
    let mut observations = Vec::new();
    for binding in &definition.bindings {
        for measure in [Measure::ObservedCount, Measure::Fraction] {
            let mut missing = PopulationObservation {
                schema_version: 1,
                observation_id: String::new(),
                specimen_id: plan.specimen_id.clone(),
                panel_id: selection.panel_id.clone(),
                acquisition_id: selection.selected.clone(),
                source_id: source_id.to_owned(),
                target: binding.target.clone(),
                measure,
                unit: if measure == Measure::Fraction {
                    "fraction"
                } else {
                    "events"
                }
                .into(),
                status: ObservationStatus::NotMeasured,
                value_u64: None,
                value_f64: None,
                denominator: None,
                method: Method {
                    name: match definition.format {
                        AnnotationFormat::GateMembership => "gating",
                        AnnotationFormat::ClassifyPredictions => "classify",
                    }
                    .into(),
                    version: "1".into(),
                    definition_sha256: definition.definition.sha256.clone(),
                    experimental: false,
                    source_observation_ids: Vec::new(),
                },
                lineage: Lineage {
                    acquisition_sha256: selection
                        .selected
                        .as_ref()
                        .and_then(|id| selection.acquisitions.iter().find(|a| &a.id == id))
                        .map(|a| a.file.sha256.clone()),
                    tree_sha256: selection.tree_sha256.clone(),
                    panel_profile_sha256: selection.panel_profile_sha256.clone(),
                    projection_sha256: selection.projection_sha256.clone(),
                    annotations_sha256: None,
                    producer_provenance_sha256: None,
                    analysis_mask_sha256: None,
                },
                n_events: 0,
                supporting_count: None,
                reason: Some(
                    if selection.selected.is_none() {
                        "missing_panel"
                    } else {
                        "missing_source_output"
                    }
                    .into(),
                ),
            };
            missing.seal()?;
            observations.push(missing);
        }
    }
    Ok(observations)
}

fn index_sources<'a>(
    plan: &SpecimenPlan,
    sources: &'a [SourceObservations],
) -> Result<BTreeMap<(&'a str, &'a str), &'a SourceObservations>> {
    let mut provided = BTreeMap::new();
    for source in sources {
        let descriptor = &source.annotations;
        let context = &descriptor.context;
        require(
            ((descriptor.schema_version == 1
                && descriptor.abstained_events.is_none()
                && descriptor.ood_events.is_none())
                || (descriptor.schema_version == 2
                    && descriptor.context.source.decision_policy.is_some()
                    && descriptor.abstained_events.is_some()
                    && descriptor.ood_events.is_some()))
                && descriptor.unknown_events <= descriptor.assessed_events
                && descriptor.abstained_events.unwrap_or(0) <= descriptor.assessed_events
                && descriptor.ood_events.unwrap_or(0) <= descriptor.abstained_events.unwrap_or(0)
                && descriptor.assessed_events <= context.raw_events,
            "invalid annotation descriptor",
        )?;
        require(
            plan.context(&context.acquisition_id, &context.source_id)? == *context,
            "source does not belong to this plan",
        )?;
        require(
            provided
                .insert(
                    (context.panel_id.as_str(), context.source_id.as_str()),
                    source,
                )
                .is_none(),
            "duplicate observation package",
        )?;
        let selection = plan
            .selections
            .iter()
            .find(|s| s.panel_id == context.panel_id)
            .ok_or_else(|| crate::SpecimenError::Invalid("source has no planned panel".into()))?;
        validate_observations(source, selection)?;
    }
    Ok(provided)
}

fn check_consistency(
    plan: &SpecimenPlan,
    observations: &[PopulationObservation],
    checks: &mut Vec<SpecimenCheck>,
) {
    for policy in &plan.layout.consistency {
        let fractions = observations
            .iter()
            .filter(|o| {
                o.target == policy.target
                    && o.measure == Measure::Fraction
                    && o.status == ObservationStatus::Measured
            })
            .collect::<Vec<_>>();
        let comparable = fractions.len() > 1
            && fractions.iter().all(|o| {
                let denominator = o.denominator.as_ref().expect("validated fraction");
                let reference = fractions[0]
                    .denominator
                    .as_ref()
                    .expect("validated fraction");
                denominator.target == reference.target && denominator.scope == reference.scope
            });
        let (status, message) = if comparable {
            let low = fractions
                .iter()
                .filter_map(|o| o.value_f64)
                .fold(f64::INFINITY, f64::min);
            let high = fractions
                .iter()
                .filter_map(|o| o.value_f64)
                .fold(f64::NEG_INFINITY, f64::max);
            (
                if high - low > policy.max_abs_fraction_difference {
                    policy.severity
                } else {
                    CheckStatus::Pass
                },
                format!(
                    "absolute fraction difference {} (limit {})",
                    high - low,
                    policy.max_abs_fraction_difference
                ),
            )
        } else {
            (
                CheckStatus::Warn,
                "insufficient comparable fractions; no biological exclusion applied".into(),
            )
        };
        checks.push(SpecimenCheck {
            category: "consistency".into(),
            scope: policy.target.id.clone(),
            check: "fraction_agreement".into(),
            status,
            message,
        });
    }
}

fn apply_rules(
    plan: &SpecimenPlan,
    observations: &[PopulationObservation],
    results: &mut Vec<PopulationObservation>,
    checks: &mut Vec<SpecimenCheck>,
) -> Result<()> {
    let definition_sha256 = serialization::digest(&serde_json::to_value(&plan.layout)?);
    for rule in &plan.layout.reconciliation {
        match &rule.strategy {
            Strategy::Independent => {}
            Strategy::PreferReference { reference } => {
                for observation in observations
                    .iter()
                    .filter(|o| matches_source(o, reference) && o.target == rule.target)
                {
                    let mut selected = observation.clone();
                    selected.method = Method {
                        name: "prefer_reference".into(),
                        version: "1".into(),
                        definition_sha256: definition_sha256.clone(),
                        experimental: false,
                        source_observation_ids: vec![observation.observation_id.clone()],
                    };
                    selected.seal()?;
                    results.push(selected);
                }
            }
            Strategy::ReferenceRatio {
                source,
                reference,
                anchor,
                anchor_limits,
            } => {
                let (estimate, ratio_checks) = reference_ratio(
                    observations,
                    &rule.target,
                    &RatioInputs {
                        source,
                        reference,
                        anchor,
                        limits: anchor_limits,
                    },
                    &definition_sha256,
                )?;
                results.push(estimate);
                checks.extend(ratio_checks);
            }
        }
    }
    Ok(())
}

struct RatioInputs<'a> {
    source: &'a SourceRef,
    reference: &'a SourceRef,
    anchor: &'a Target,
    limits: &'a AnchorLimits,
}

fn reference_ratio(
    observations: &[PopulationObservation],
    target: &Target,
    inputs: &RatioInputs<'_>,
    definition_sha256: &str,
) -> Result<(PopulationObservation, Vec<SpecimenCheck>)> {
    let population = find_count(observations, inputs.source, target)?;
    let source_anchor = find_count(observations, inputs.source, inputs.anchor)?;
    let reference_anchor = find_count(observations, inputs.reference, inputs.anchor)?;
    let mut estimate = population.clone();
    estimate.measure = Measure::EstimatedCount;
    estimate.value_u64 = None;
    estimate.method = Method {
        name: "reference_ratio".into(),
        // 2: anchor limits can reject an estimate version 1 accepted.
        version: "2".into(),
        definition_sha256: definition_sha256.to_owned(),
        experimental: true,
        source_observation_ids: vec![
            population.observation_id.clone(),
            source_anchor.observation_id.clone(),
            reference_anchor.observation_id.clone(),
        ],
    };
    let counts = AnchorCounts {
        population: measured_count(population),
        source: measured_count(source_anchor).filter(|&n| n > 0),
        reference: measured_count(reference_anchor).filter(|&n| n > 0),
    };
    let available =
        counts.population.is_some() && counts.source.is_some() && counts.reference.is_some();
    let (mut checks, rejection) = anchor_checks(
        &target.id,
        [source_anchor, reference_anchor],
        &counts,
        inputs.limits,
    );
    let rejection = if available {
        rejection
    } else {
        Some("invalid_or_unavailable_anchor")
    };
    if let (None, Some(population), Some(source), Some(reference)) = (
        rejection,
        counts.population,
        counts.source,
        counts.reference,
    ) {
        estimate.value_f64 = Some(as_f64(population) * (as_f64(reference) / as_f64(source)));
        estimate.status = ObservationStatus::Estimated;
        estimate.reason = None;
    } else {
        estimate.status = ObservationStatus::QcRejected;
        estimate.value_f64 = None;
        estimate.supporting_count = population.value_u64.or(population.supporting_count);
        estimate.reason = rejection.map(Into::into);
    }
    estimate.seal()?;

    checks.insert(
        0,
        SpecimenCheck {
            category: "consistency".into(),
            scope: target.id.clone(),
            check: "reference_ratio_anchor".into(),
            status: if available {
                CheckStatus::Pass
            } else {
                CheckStatus::Fail
            },
            message: format!(
                "reference_ratio is experimental; no fallback or averaging; {}",
                estimate_precision(&counts)
            ),
        },
    );
    Ok((estimate, checks))
}

/// Measured counts of a ratio; anchors are `None` unless measured and
/// positive.
struct AnchorCounts {
    population: Option<u64>,
    source: Option<u64>,
    reference: Option<u64>,
}

fn measured_count(observation: &PopulationObservation) -> Option<u64> {
    (observation.status == ObservationStatus::Measured)
        .then_some(observation.value_u64)
        .flatten()
}

#[allow(clippy::cast_precision_loss)]
fn as_f64(count: u64) -> f64 {
    count as f64
}

/// Poisson relative CV of `population × reference / source` for independent
/// counts (delta method); undefined without a positive population count.
fn estimate_precision(counts: &AnchorCounts) -> String {
    match (counts.population, counts.source, counts.reference) {
        (Some(population), Some(source), Some(reference)) if population > 0 => format!(
            "estimate Poisson CV {:.3}",
            (1.0 / as_f64(population) + 1.0 / as_f64(source) + 1.0 / as_f64(reference)).sqrt()
        ),
        _ => "estimate Poisson CV n/a".into(),
    }
}

/// Reliability checks of a reference-ratio anchor. Returns the checks and
/// the first rejection reason in fixed order: too few events, then scale.
fn anchor_checks(
    scope: &str,
    [source, reference]: [&PopulationObservation; 2],
    counts: &AnchorCounts,
    limits: &AnchorLimits,
) -> (Vec<SpecimenCheck>, Option<&'static str>) {
    let check = |name: &str, status: CheckStatus, message: String| SpecimenCheck {
        category: "consistency".into(),
        scope: scope.to_owned(),
        check: name.into(),
        status,
        message,
    };
    let mut rejection = None;
    let mut low = false;
    let describe = |observation: &PopulationObservation, count: Option<u64>| {
        let name = format!(
            "{}/{} anchor `{}`",
            observation.panel_id, observation.source_id, observation.target.id
        );
        match count {
            Some(n) => format!(
                "{name}: {n} event(s) (minimum {}, Poisson CV {:.3})",
                limits.min_events,
                1.0 / as_f64(n).sqrt()
            ),
            None => format!("{name}: unavailable (minimum {})", limits.min_events),
        }
    };
    let mut messages = Vec::new();
    for (observation, count) in [(source, counts.source), (reference, counts.reference)] {
        low |= count.is_none_or(|n| n < limits.min_events);
        messages.push(describe(observation, count));
    }
    if low {
        rejection = Some("anchor_below_min_events");
    }
    let mut checks = vec![check(
        "reference_ratio_anchor_events",
        if low {
            CheckStatus::Fail
        } else {
            CheckStatus::Pass
        },
        messages.join("; "),
    )];
    let (Some(source_count), Some(reference_count)) = (counts.source, counts.reference) else {
        return (checks, rejection);
    };
    if let Some(max_scale) = limits.max_scale {
        let scale = as_f64(reference_count) / as_f64(source_count);
        let excess = scale.max(1.0 / scale) > max_scale;
        if excess && rejection.is_none() {
            rejection = Some("anchor_scale_out_of_bounds");
        }
        checks.push(check(
            "reference_ratio_scale",
            if excess {
                CheckStatus::Fail
            } else {
                CheckStatus::Pass
            },
            format!(
                "scale {scale} (accepted [{}, {max_scale}])",
                1.0 / max_scale
            ),
        ));
    }
    if let Some(max_ratio) = limits.max_frequency_ratio {
        let frequency = |count: u64, observation: &PopulationObservation| {
            as_f64(count) / as_f64(observation.n_events)
        };
        let source_frequency = frequency(source_count, source);
        let reference_frequency = frequency(reference_count, reference);
        let ratio =
            source_frequency.max(reference_frequency) / source_frequency.min(reference_frequency);
        let diverges = ratio.is_nan() || ratio > max_ratio;
        checks.push(check(
            "reference_ratio_anchor_frequency",
            if diverges {
                CheckStatus::Warn
            } else {
                CheckStatus::Pass
            },
            format!(
                "anchor frequency {source_frequency} in {}/{} vs {reference_frequency} in {}/{}; ratio {ratio} (limit {max_ratio})",
                source.panel_id, source.source_id, reference.panel_id, reference.source_id
            ),
        ));
    }
    (checks, rejection)
}
