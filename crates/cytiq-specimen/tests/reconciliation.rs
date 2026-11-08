use cytiq_population::{
    ArtifactRef, EventAnnotations, Measure, ObservationStatus, PopulationObservation,
    annotations::KnowledgeRef, knowledge::KnowledgeArtifact,
};
use cytiq_qc::CheckStatus;
use cytiq_specimen::{
    SpecimenLayout, SpecimenPlan, SpecimenResult,
    layout::{AnchorLimits, ReconciliationRule, SourceRef, Strategy},
    plan::{Acquisition, PanelSelection},
    reconcile::{SourceObservations, reconcile},
};
use serde_json::json;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures/specimen")
        .join(name)
}

fn plan() -> SpecimenPlan {
    let definition = ArtifactRef::of(&fixture("gates.xml")).unwrap();
    let mut panels = serde_json::Map::new();
    let mut selections = Vec::new();
    for (name, events) in [("P1", 10000_u64), ("P2", 20000_u64)] {
        let projection =
            KnowledgeArtifact::load(&fixture(&format!("{name}.projection.json"))).unwrap();
        panels.insert(name.into(),json!({"projection":KnowledgeRef {path:fixture(&format!("{name}.projection.json")),sha256:projection.sha256().into()},"sources":{"gates":{
            "format":"gate_membership","definition":definition,"bindings":[
                {"label":"All","target":{"kind":"population","id":"cells","scope":"cells"}},
                {"label":"T","target":{"kind":"population","id":"t_cells","scope":"cells"},"denominator":"All"}
            ]}}}));
        let id = format!("{name}-A");
        selections.push(PanelSelection {
            panel_id: name.into(),
            tree_sha256: projection.value()["tree_sha256"].as_str().unwrap().into(),
            panel_profile_sha256: projection.value()["panel_sha256"].as_str().unwrap().into(),
            projection_sha256: projection.sha256().into(),
            acquisitions: vec![Acquisition {
                id: id.clone(),
                file: ArtifactRef {
                    path: PathBuf::from("unused.fcs"),
                    sha256: "a".repeat(64),
                },
                raw_events: events,
            }],
            selected: Some(id),
            excluded: std::collections::BTreeMap::default(),
        });
    }
    let mut layout: SpecimenLayout =
        serde_json::from_value(json!({"schema_version":1,"protocol_id":"test","panels":panels}))
            .unwrap();
    layout.validate().unwrap();
    SpecimenPlan {
        schema_version: 1,
        specimen_id: "S01".into(),
        manifest: definition.clone(),
        layout_file: definition,
        cytometry: cytiq_manifest::Cytometry::Conventional,
        layout,
        selections,
    }
}

fn source(plan: &SpecimenPlan, panel: &str, population: u64, anchor: u64) -> SourceObservations {
    let context = plan.context(&format!("{panel}-A"), "gates").unwrap();
    let sha = "b".repeat(64);
    let reference = ArtifactRef {
        path: Path::new("unused.parquet").into(),
        sha256: sha.clone(),
    };
    let descriptor = EventAnnotations {
        schema_version: 1,
        context: context.clone(),
        data: reference.clone(),
        producer_provenance: reference.clone(),
        analysis_mask: reference.clone(),
        technical_qc: reference.clone(),
        technical_qc_provenance: reference,
        technical_status: "pass".into(),
        assessed_events: context.raw_events,
        unknown_events: 0,
        abstained_events: None,
        ood_events: None,
    };
    let mut observations = Vec::new();
    let selection = plan
        .selections
        .iter()
        .find(|s| s.panel_id == panel)
        .unwrap();
    for (index, binding) in context.source.bindings.iter().enumerate() {
        let count = if index == 0 { anchor } else { population };
        let denominator = if index == 0 {
            context.raw_events
        } else {
            anchor
        };
        for measure in ["observed_count", "fraction"] {
            let fraction = measure == "fraction";
            #[allow(clippy::cast_precision_loss)]
            let value =
                (fraction && denominator > 0).then_some(count as f64 / denominator.max(1) as f64);
            let mut row: PopulationObservation = serde_json::from_value(json!({
                "schema_version":1,"observation_id":"","specimen_id":"S01","panel_id":panel,"acquisition_id":context.acquisition_id,"source_id":"gates","target":binding.target,
                "measure":measure,"unit":if fraction {"fraction"} else {"events"},"status":if fraction && denominator == 0 {"not_applicable"} else {"measured"},
                "value_u64":if fraction {None} else {Some(count)},"value_f64":value,
                "denominator":{"target":if index == 0 {None} else {Some(&context.source.bindings[0].target)},"scope":"cells","observed_count":denominator},
                "method":{"name":"gating","version":"1","definition_sha256":context.source.definition.sha256,"experimental":false,"source_observation_ids":[]},
                "lineage":{"acquisition_sha256":context.acquisition_sha256,"tree_sha256":selection.tree_sha256,"panel_profile_sha256":selection.panel_profile_sha256,"projection_sha256":selection.projection_sha256,
                    "annotations_sha256":sha,"producer_provenance_sha256":sha,"analysis_mask_sha256":sha},"n_events":context.raw_events,"supporting_count":null,
                "reason":if fraction && denominator == 0 {Some("zero_denominator")} else {None}
            })).unwrap();
            row.seal().unwrap();
            observations.push(row);
        }
    }
    SourceObservations {
        annotations: descriptor,
        observations,
    }
}

fn rule(plan: &SpecimenPlan, strategy: Strategy) -> ReconciliationRule {
    ReconciliationRule {
        target: plan.layout.panels["P1"].sources["gates"].bindings[1]
            .target
            .clone(),
        strategy,
    }
}

fn limits(
    min_events: u64,
    max_scale: Option<f64>,
    max_frequency_ratio: Option<f64>,
) -> AnchorLimits {
    AnchorLimits {
        min_events,
        max_scale,
        max_frequency_ratio,
    }
}

/// P2's T cells scaled by the `cells` anchor of P1 over that of P2.
fn ratio_rule(plan: &SpecimenPlan, anchor_limits: AnchorLimits) -> ReconciliationRule {
    rule(
        plan,
        Strategy::ReferenceRatio {
            source: SourceRef {
                panel: "P2".into(),
                source: "gates".into(),
            },
            reference: SourceRef {
                panel: "P1".into(),
                source: "gates".into(),
            },
            anchor: plan.layout.panels["P1"].sources["gates"].bindings[0]
                .target
                .clone(),
            anchor_limits,
        },
    )
}

/// Reconcile P1 (`p1` T cells of `p1_anchor`) and P2 under one ratio rule.
fn ratio(
    anchor_limits: AnchorLimits,
    (p1, p1_anchor): (u64, u64),
    (p2, p2_anchor): (u64, u64),
) -> SpecimenResult {
    let mut plan = plan();
    plan.layout.reconciliation = vec![ratio_rule(&plan, anchor_limits)];
    let sources = [
        source(&plan, "P1", p1, p1_anchor),
        source(&plan, "P2", p2, p2_anchor),
    ];
    reconcile(&plan, &sources).unwrap()
}

fn estimate(result: &SpecimenResult) -> &PopulationObservation {
    result
        .results
        .iter()
        .find(|o| o.measure == Measure::EstimatedCount)
        .unwrap()
}

fn check<'a>(
    result: &'a SpecimenResult,
    name: &str,
) -> Option<&'a cytiq_specimen::reconcile::SpecimenCheck> {
    result.checks.iter().find(|c| c.check == name)
}

fn assert_measured_counts_unchanged(result: &SpecimenResult, p1: u64, p2: u64) {
    let measured = result
        .observations
        .iter()
        .filter(|o| o.target.id == "t_cells" && o.measure == Measure::ObservedCount)
        .map(|o| (o.status, o.value_u64))
        .collect::<Vec<_>>();
    assert_eq!(
        measured,
        [
            (ObservationStatus::Measured, Some(p1)),
            (ObservationStatus::Measured, Some(p2))
        ]
    );
}

#[test]
fn reference_ratio_keeps_both_measured_counts_and_emits_a_distinct_estimate() {
    let mut plan = plan();
    plan.layout.reconciliation = vec![ratio_rule(&plan, limits(1, None, None))];
    let sources = vec![
        source(&plan, "P1", 3000, 10000),
        source(&plan, "P2", 6400, 20000),
    ];
    let result = reconcile(&plan, &sources).unwrap();
    assert!(result.complete);
    let measured = result
        .observations
        .iter()
        .filter(|o| o.target.id == "t_cells" && o.measure == Measure::ObservedCount)
        .map(|o| o.value_u64.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(measured, vec![3000, 6400]);
    let estimate = result
        .results
        .iter()
        .find(|o| o.measure == Measure::EstimatedCount)
        .unwrap();
    assert_eq!(estimate.value_f64, Some(3200.0));
    assert_eq!(estimate.status, ObservationStatus::Estimated);
    assert!(estimate.method.experimental);
    assert_eq!(estimate.method.version, "2");
    assert_eq!(estimate.method.source_observation_ids.len(), 3);
}

#[test]
fn missing_required_panel_is_incomplete_while_optional_panel_remains_explicitly_missing() {
    let mut plan = plan();
    let sources = vec![source(&plan, "P1", 3000, 10000)];
    let result = reconcile(&plan, &sources).unwrap();
    assert!(!result.complete);
    assert!(
        result
            .observations
            .iter()
            .filter(|o| o.panel_id == "P2")
            .all(|o| o.status == ObservationStatus::NotMeasured && o.value_u64.is_none())
    );
    plan.layout.panels.get_mut("P2").unwrap().required = false;
    plan.selections[1].selected = None;
    assert!(reconcile(&plan, &sources).unwrap().complete);
}

#[test]
fn prefer_reference_has_no_fallback_and_never_adds_children() {
    let mut plan = plan();
    plan.layout.reconciliation = vec![rule(
        &plan,
        Strategy::PreferReference {
            reference: SourceRef {
                panel: "P2".into(),
                source: "gates".into(),
            },
        },
    )];
    let result = reconcile(
        &plan,
        &[
            source(&plan, "P1", 3000, 10000),
            source(&plan, "P2", 6400, 20000),
        ],
    )
    .unwrap();
    assert_eq!(
        result
            .results
            .iter()
            .find(|o| o.target.id == "t_cells" && o.measure == Measure::ObservedCount)
            .unwrap()
            .value_u64,
        Some(6400)
    );
    let missing = reconcile(&plan, &[source(&plan, "P1", 3000, 10000)]).unwrap();
    assert!(
        missing
            .results
            .iter()
            .filter(|o| o.target.id == "t_cells")
            .all(|o| o.status == ObservationStatus::NotMeasured)
    );
}

#[test]
fn zero_anchors_reject_only_the_derived_estimate() {
    let mut plan = plan();
    plan.layout.reconciliation = vec![ratio_rule(&plan, limits(1, None, None))];
    let result = reconcile(
        &plan,
        &[source(&plan, "P1", 3000, 10000), source(&plan, "P2", 0, 0)],
    )
    .unwrap();
    let estimate = result
        .results
        .iter()
        .find(|o| o.measure == Measure::EstimatedCount)
        .unwrap();
    assert_eq!(estimate.status, ObservationStatus::QcRejected);
    assert_eq!(estimate.value_f64, None);
    assert!(result.observations.iter().any(|o| o.panel_id == "P2"
        && o.value_u64 == Some(0)
        && o.status == ObservationStatus::Measured));
}

#[test]
fn biological_disagreement_does_not_delete_or_reject_measurements() {
    let mut plan = plan();
    plan.layout
        .consistency
        .push(cytiq_specimen::layout::ConsistencyPolicy {
            target: plan.layout.panels["P1"].sources["gates"].bindings[1]
                .target
                .clone(),
            max_abs_fraction_difference: 0.01,
            severity: cytiq_qc::CheckStatus::Fail,
        });
    let result = reconcile(
        &plan,
        &[
            source(&plan, "P1", 100, 10000),
            source(&plan, "P2", 19900, 20000),
        ],
    )
    .unwrap();
    assert_eq!(result.status, cytiq_qc::CheckStatus::Fail);
    assert!(result.complete);
    assert!(
        result
            .observations
            .iter()
            .all(|o| o.status == ObservationStatus::Measured)
    );
    let duplicate = vec![
        source(&plan, "P1", 100, 10000),
        source(&plan, "P1", 100, 10000),
    ];
    assert!(reconcile(&plan, &duplicate).is_err());
}

#[test]
fn anchor_below_min_events_rejects_only_the_derived_estimate() {
    // P2's anchor holds 49 events; the rule demands 50.
    let result = ratio(limits(50, None, None), (30, 100), (20, 49));
    let estimate = estimate(&result);
    assert_eq!(estimate.status, ObservationStatus::QcRejected);
    assert_eq!(estimate.reason.as_deref(), Some("anchor_below_min_events"));
    assert_eq!(estimate.value_f64, None);
    assert_eq!(estimate.supporting_count, Some(20));
    let events = check(&result, "reference_ratio_anchor_events").unwrap();
    assert_eq!(events.status, CheckStatus::Fail);
    assert!(
        events
            .message
            .contains("P2/gates anchor `cells`: 49 event(s) (minimum 50")
    );
    assert_eq!(
        check(&result, "reference_ratio_anchor").unwrap().status,
        CheckStatus::Pass
    );
    assert_eq!(result.status, CheckStatus::Fail);
    assert_measured_counts_unchanged(&result, 30, 20);
}

#[test]
fn anchor_count_equal_to_min_events_is_accepted() {
    let result = ratio(limits(50, None, None), (30, 100), (20, 50));
    let estimate = estimate(&result);
    assert_eq!(estimate.status, ObservationStatus::Estimated);
    assert_eq!(estimate.value_f64, Some(40.0));
    assert_eq!(
        check(&result, "reference_ratio_anchor_events")
            .unwrap()
            .status,
        CheckStatus::Pass
    );
    let reference_low = ratio(limits(101, None, None), (30, 100), (20, 200));
    assert_eq!(
        estimate_status(&reference_low),
        ObservationStatus::QcRejected,
        "the reference anchor is checked too"
    );
}

fn estimate_status(result: &SpecimenResult) -> ObservationStatus {
    estimate(result).status
}

#[test]
fn scale_beyond_bound_rejects_estimate_in_both_directions() {
    // Reference (P1) over source (P2) anchor: scale 0.5, then 2.
    for (p1_anchor, p2_anchor) in [(5000, 10000), (10000, 5000)] {
        let result = ratio(
            limits(1, Some(1.5), None),
            (3000, p1_anchor),
            (2000, p2_anchor),
        );
        let estimate = estimate(&result);
        assert_eq!(estimate.status, ObservationStatus::QcRejected);
        assert_eq!(
            estimate.reason.as_deref(),
            Some("anchor_scale_out_of_bounds")
        );
        assert_eq!(
            check(&result, "reference_ratio_scale").unwrap().status,
            CheckStatus::Fail
        );
        assert_measured_counts_unchanged(&result, 3000, 2000);
    }
}

#[test]
fn scale_at_bound_is_accepted() {
    for (p1_anchor, p2_anchor) in [(5000, 10000), (10000, 5000)] {
        let result = ratio(
            limits(1, Some(2.0), None),
            (3000, p1_anchor),
            (2000, p2_anchor),
        );
        assert_eq!(estimate_status(&result), ObservationStatus::Estimated);
        assert_eq!(
            check(&result, "reference_ratio_scale").unwrap().status,
            CheckStatus::Pass
        );
    }
}

#[test]
fn events_are_reported_before_scale_when_both_fail() {
    let result = ratio(limits(50, Some(1.5), None), (30, 100), (5, 10));
    assert_eq!(
        estimate(&result).reason.as_deref(),
        Some("anchor_below_min_events")
    );
    assert_eq!(
        check(&result, "reference_ratio_scale").unwrap().status,
        CheckStatus::Fail
    );
}

#[test]
fn divergent_anchor_frequencies_warn_without_rejecting() {
    // Anchor frequencies 4000/10000 = 0.4 in P1 and 16000/20000 = 0.8 in P2.
    let result = ratio(limits(1, None, Some(1.5)), (1000, 4000), (2000, 16000));
    let estimate = estimate(&result);
    assert_eq!(estimate.status, ObservationStatus::Estimated);
    assert_eq!(estimate.value_f64, Some(500.0));
    let frequency = check(&result, "reference_ratio_anchor_frequency").unwrap();
    assert_eq!(frequency.status, CheckStatus::Warn);
    assert!(
        frequency.message.contains("ratio 2"),
        "{}",
        frequency.message
    );
    assert_eq!(result.status, CheckStatus::Warn);

    let at_limit = ratio(limits(1, None, Some(2.0)), (1000, 4000), (2000, 16000));
    assert_eq!(
        check(&at_limit, "reference_ratio_anchor_frequency")
            .unwrap()
            .status,
        CheckStatus::Pass
    );
}

#[test]
fn optional_checks_are_absent_unless_configured_or_anchors_are_unavailable() {
    let result = ratio(limits(1, None, None), (3000, 10000), (6400, 20000));
    assert!(check(&result, "reference_ratio_scale").is_none());
    assert!(check(&result, "reference_ratio_anchor_frequency").is_none());
    let unavailable = ratio(limits(1, Some(2.0), Some(2.0)), (3000, 10000), (0, 0));
    assert_eq!(
        estimate(&unavailable).reason.as_deref(),
        Some("invalid_or_unavailable_anchor")
    );
    assert!(check(&unavailable, "reference_ratio_scale").is_none());
    assert!(check(&unavailable, "reference_ratio_anchor_frequency").is_none());
    let events = check(&unavailable, "reference_ratio_anchor_events").unwrap();
    assert_eq!(events.status, CheckStatus::Fail);
    assert!(events.message.contains("unavailable"), "{}", events.message);
}

#[test]
fn anchor_check_messages_report_poisson_cv() {
    let result = ratio(limits(1, None, None), (100, 400), (100, 100));
    let events = check(&result, "reference_ratio_anchor_events").unwrap();
    assert!(
        events.message.contains("Poisson CV 0.100") && events.message.contains("Poisson CV 0.050"),
        "{}",
        events.message
    );
    // √(1/100 + 1/100 + 1/400) = 0.15
    let anchor = check(&result, "reference_ratio_anchor").unwrap();
    assert!(
        anchor.message.ends_with("estimate Poisson CV 0.150"),
        "{}",
        anchor.message
    );
    let empty = ratio(limits(1, None, None), (100, 400), (0, 100));
    assert!(
        check(&empty, "reference_ratio_anchor")
            .unwrap()
            .message
            .ends_with("estimate Poisson CV n/a")
    );
}

#[test]
fn reference_ratio_without_anchor_limits_is_rejected() {
    let plan = plan();
    let mut rule = serde_json::to_value(ratio_rule(&plan, limits(1, None, None))).unwrap();
    rule["strategy"]
        .as_object_mut()
        .unwrap()
        .remove("anchor_limits");
    assert!(serde_json::from_value::<ReconciliationRule>(rule).is_err());
}

#[test]
fn invalid_anchor_limits_are_rejected() {
    for invalid in [
        limits(0, None, None),
        limits(1, Some(f64::NAN), None),
        limits(1, Some(0.5), None),
        limits(1, None, Some(f64::INFINITY)),
        limits(1, None, Some(0.99)),
    ] {
        let mut plan = plan();
        plan.layout.reconciliation = vec![ratio_rule(&plan, invalid.clone())];
        assert!(plan.layout.validate().is_err(), "{invalid:?}");
    }
    let mut plan = plan();
    plan.layout.reconciliation = vec![ratio_rule(&plan, limits(1, Some(1.0), Some(1.0)))];
    assert!(plan.layout.validate().is_ok());
}

#[test]
fn optional_anchor_limits_are_omitted_from_the_layout_digest() {
    let plan = plan();
    let rule = serde_json::to_value(ratio_rule(&plan, limits(5, None, None))).unwrap();
    assert_eq!(rule["strategy"]["anchor_limits"], json!({"min_events": 5}));
}

#[test]
fn layout_identity_and_policy_fields_are_checked_individually() {
    let base = plan().layout;
    for (pointer, replacement) in [
        ("/schema_version", json!(2)),
        ("/protocol_id", json!(" ")),
        ("/panels", json!({})),
        ("/panels/P1/sources", json!({})),
    ] {
        let mut value = serde_json::to_value(&base).unwrap();
        *value.pointer_mut(pointer).unwrap() = replacement;
        let mut layout: SpecimenLayout = serde_json::from_value(value).unwrap();
        assert!(layout.validate().is_err(), "accepted {pointer}");
    }
    let mut layout = base.clone();
    let panel = layout.panels.remove("P1").unwrap();
    layout.panels.insert(String::new(), panel);
    assert!(layout.validate().is_err());
    let target = base.panels["P1"].sources["gates"].bindings[1]
        .target
        .clone();
    for (threshold, severity, valid) in [
        (0.0, CheckStatus::Warn, true),
        (1.0, CheckStatus::Fail, true),
        (-0.1, CheckStatus::Fail, false),
        (1.1, CheckStatus::Fail, false),
        (f64::NAN, CheckStatus::Fail, false),
        (0.1, CheckStatus::Pass, false),
    ] {
        let mut layout = base.clone();
        layout
            .consistency
            .push(cytiq_specimen::layout::ConsistencyPolicy {
                target: target.clone(),
                max_abs_fraction_difference: threshold,
                severity,
            });
        assert_eq!(layout.validate().is_ok(), valid, "{threshold}/{severity:?}");
    }
    let mut p = plan();
    let mut r = ratio_rule(&p, limits(1, None, None));
    if let Strategy::ReferenceRatio {
        source, reference, ..
    } = &mut r.strategy
    {
        source.panel.clone_from(&reference.panel);
    }
    p.layout.reconciliation = vec![r];
    assert!(p.layout.validate().is_err());
    let mut p = plan();
    let mut r = ratio_rule(&p, limits(1, None, None));
    if let Strategy::ReferenceRatio { anchor, .. } = &mut r.strategy {
        anchor.scope = "other".into();
    }
    p.layout.reconciliation = vec![r];
    assert!(p.layout.validate().is_err());
    let mut p = plan();
    p.layout.reconciliation = vec![rule(&p, Strategy::Independent)];
    p.layout.validate().unwrap();
    p.layout.reconciliation[0].target.id = "unknown".into();
    assert!(p.layout.validate().is_err());
}

#[test]
fn result_identity_and_source_references_are_checked_independently() {
    let p = plan();
    let valid = reconcile(&p, &[source(&p, "P1", 30, 100)]).unwrap();
    valid.validate().unwrap();
    let mut bad = valid.clone();
    bad.schema_version = 2;
    assert!(bad.validate().is_err());
    let mut bad = valid.clone();
    bad.specimen_id.clear();
    bad.observations.clear();
    bad.results.clear();
    assert!(bad.validate().is_err());
    for original in [true, false] {
        let mut bad = valid.clone();
        let rows = if original {
            &mut bad.observations
        } else {
            &mut bad.results
        };
        rows[0].specimen_id = "other".into();
        rows[0].seal().unwrap();
        assert!(bad.validate().is_err());
    }
    let mut bad = valid;
    bad.results[0]
        .method
        .source_observation_ids
        .push("f".repeat(64));
    bad.results[0].seal().unwrap();
    assert!(bad.validate().is_err());
}

#[test]
fn annotation_descriptor_bounds_are_independent() {
    let p = plan();
    for field in ["version", "unknown", "assessed"] {
        let mut s = source(&p, "P1", 30, 100);
        match field {
            "version" => s.annotations.schema_version = 2,
            "unknown" => s.annotations.unknown_events = s.annotations.assessed_events + 1,
            _ => s.annotations.assessed_events = s.annotations.context.raw_events + 1,
        }
        assert!(reconcile(&p, &[s]).is_err(), "{field}");
    }
}

#[test]
fn source_denominators_and_fractions_must_match_their_counts() {
    let p = plan();
    for field in [
        "scope",
        "target",
        "count",
        "fraction_status",
        "fraction_value",
    ] {
        let mut s = source(&p, "P1", 30, 100);
        let row = &mut s.observations[3];
        match field {
            "scope" => row.denominator.as_mut().unwrap().scope = "other".into(),
            "target" => row.denominator.as_mut().unwrap().target = None,
            "count" => row.denominator.as_mut().unwrap().observed_count = 101,
            "fraction_status" => row.status = ObservationStatus::Estimated,
            _ => row.value_f64 = Some(0.2),
        }
        row.seal().unwrap();
        assert!(reconcile(&p, &[s]).is_err(), "{field}");
    }
    let mut s = source(&p, "P1", 30, 100);
    s.annotations.technical_status = "fail".into();
    for row in &mut s.observations {
        row.supporting_count = row.value_u64;
        row.value_u64 = None;
        row.value_f64 = None;
        row.status = ObservationStatus::QcRejected;
        row.reason = Some("technical_qc_failed".into());
        row.seal().unwrap();
    }
    let result = reconcile(&p, &[s]).unwrap();
    assert_eq!(result.status, CheckStatus::Fail);
    let mut empty = source(&p, "P1", 0, 0);
    empty.annotations.assessed_events = 0;
    for row in &mut empty.observations {
        row.n_events = 0;
        row.denominator.as_mut().unwrap().observed_count = 0;
        row.value_u64 = None;
        row.value_f64 = None;
        row.status = ObservationStatus::NotMeasured;
        row.reason = Some("no_assessed_events".into());
        row.seal().unwrap();
    }
    reconcile(&p, &[empty]).unwrap();
    let mut p = plan();
    p.layout
        .panels
        .get_mut("P1")
        .unwrap()
        .sources
        .get_mut("gates")
        .unwrap()
        .format = cytiq_population::annotations::AnnotationFormat::ClassifyPredictions;
    let mut s = source(&p, "P1", 30, 100);
    for row in &mut s.observations {
        row.method.name = "classify".into();
        row.seal().unwrap();
    }
    assert!(
        reconcile(&p, &[s]).is_err(),
        "classification counts must cover assessed events"
    );
}

#[test]
fn consistency_limits_are_inclusive_and_require_comparable_measured_fractions() {
    let mut p = plan();
    let target = p.layout.panels["P1"].sources["gates"].bindings[1]
        .target
        .clone();
    p.layout
        .consistency
        .push(cytiq_specimen::layout::ConsistencyPolicy {
            target,
            max_abs_fraction_difference: 0.25,
            severity: CheckStatus::Fail,
        });
    for (p1, p2, expected) in [
        (0, 0, CheckStatus::Pass),
        (25, 50, CheckStatus::Pass),
        (25, 51, CheckStatus::Fail),
    ] {
        let result =
            reconcile(&p, &[source(&p, "P1", p1, 100), source(&p, "P2", p2, 100)]).unwrap();
        assert_eq!(
            check(&result, "fraction_agreement").unwrap().status,
            expected
        );
    }
    let result = reconcile(&p, &[source(&p, "P1", 25, 100)]).unwrap();
    assert_eq!(
        check(&result, "fraction_agreement").unwrap().status,
        CheckStatus::Warn
    );
    // Both panels can validly choose different denominator definitions.
    // Such fractions cannot establish biological agreement.
    p.layout
        .panels
        .get_mut("P2")
        .unwrap()
        .sources
        .get_mut("gates")
        .unwrap()
        .bindings[1]
        .denominator = None;
    let mut other = source(&p, "P2", 50, 100);
    for row in &mut other.observations {
        if row.target.id == "t_cells" {
            row.denominator.as_mut().unwrap().target = None;
            row.denominator.as_mut().unwrap().observed_count = 20000;
            if row.measure == Measure::Fraction {
                row.value_f64 = Some(50.0 / 20000.0);
            }
            row.seal().unwrap();
        }
    }
    let result = reconcile(&p, &[source(&p, "P1", 25, 100), other]).unwrap();
    assert_eq!(
        check(&result, "fraction_agreement").unwrap().status,
        CheckStatus::Warn
    );
}

#[test]
fn zero_reference_anchor_is_reported_as_unavailable() {
    let result = ratio(limits(1, None, None), (0, 0), (20, 100));
    assert_eq!(
        estimate(&result).reason.as_deref(),
        Some("invalid_or_unavailable_anchor")
    );
    assert!(
        check(&result, "reference_ratio_anchor_events")
            .unwrap()
            .message
            .contains("unavailable")
    );
}
