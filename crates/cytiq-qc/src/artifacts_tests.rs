use cytiq_mask::EventMask;
use serde_json::json;

use super::*;
use crate::truth::{
    Annotation, ArtifactClass, ArtifactTruth, ClassLabels, Evidence, Selection, TruthClasses,
    TruthDescriptor,
};

fn sha() -> String {
    "b".repeat(64)
}

fn labels() -> ClassLabels {
    ClassLabels {
        evidence: Evidence::Generative,
        selection: Selection::AllEvents,
        method: "generator".into(),
        channels: Vec::new(),
        annotators: None,
        agreement: None,
    }
}

fn truth(classes: Vec<(ArtifactClass, Vec<Option<bool>>)>) -> ArtifactTruth {
    let mut described = TruthClasses::default();
    for (class, _) in &classes {
        let entry = Some(labels());
        match class {
            ArtifactClass::Debris => described.debris = entry,
            ArtifactClass::Doublet => described.doublet = entry,
            ArtifactClass::Dead => described.dead = entry,
            ArtifactClass::TimeAnomaly => described.time_anomaly = entry,
            ArtifactClass::Saturated => described.saturated = entry,
        }
    }
    let descriptor = TruthDescriptor {
        annotation: Annotation {
            producer: "p".into(),
            protocol: "q".into(),
            reference: None,
        },
        classes: described,
    };
    ArtifactTruth::new(sha(), descriptor, classes).unwrap()
}

/// Labels and column values from `(label, value, count)` runs.
fn runs(spec: &[(Option<bool>, Option<bool>, usize)]) -> (Vec<Option<bool>>, Vec<Option<bool>>) {
    let mut labels = Vec::new();
    let mut values = Vec::new();
    for &(label, value, count) in spec {
        labels.extend(std::iter::repeat_n(label, count));
        values.extend(std::iter::repeat_n(value, count));
    }
    (labels, values)
}

fn mask_of(columns: Vec<(&str, Vec<Option<bool>>)>) -> EventMask {
    let events = columns[0].1.len();
    let keep = (0..events)
        .map(|row| columns.iter().all(|(_, values)| values[row] != Some(false)))
        .collect();
    columns
        .into_iter()
        .fold(EventMask::new(sha(), keep, None), |mask, (name, values)| {
            mask.with_filter(name, values)
        })
}

/// Artifacts: 5 removed, 2 passed, 1 not assessed; clean: 3, 11, 2;
/// unlabelled (undetermined): 4 removed, 6 passed.
fn asymmetric() -> (Vec<Option<bool>>, Vec<Option<bool>>) {
    let (yes, no) = (Some(true), Some(false));
    runs(&[
        (yes, no, 5),
        (yes, yes, 2),
        (yes, None, 1),
        (no, no, 3),
        (no, yes, 11),
        (no, None, 2),
        (None, no, 4),
        (None, yes, 6),
    ])
}

#[test]
fn filter_metrics_use_their_own_denominators_and_sharp_bounds() {
    let (label, values) = asymmetric();
    let truth = truth(vec![(ArtifactClass::Debris, label)]);
    let report = artifact_removal(&sha(), 34, &mask_of(vec![("non_debris", values)]), &truth);
    assert_eq!(report.status, RetentionStatus::Assessed);
    let filter = &report.filters[0];
    assert_eq!(filter.assessed, 31);
    assert_eq!(
        filter.events.artifact,
        Outcomes {
            removed: 5,
            passed: 2,
            not_assessed: 1
        }
    );
    assert_eq!(filter.events.clean.kept(), 13);
    assert_eq!(filter.removed_by_class[&ArtifactClass::Debris], 5);
    let clean_loss = filter.clean_loss;
    assert_eq!(
        (clean_loss.value, clean_loss.lower, clean_loss.upper),
        (Some(3.0 / 16.0), Some(3.0 / 22.0), Some(7.0 / 20.0))
    );
    let share = filter.removed_artifact_share;
    assert_eq!(
        (share.value, share.lower, share.upper),
        (Some(5.0 / 8.0), Some(5.0 / 12.0), Some(9.0 / 12.0))
    );
    let target = filter.target.as_ref().unwrap();
    assert_eq!(
        (target.class, target.status),
        (ArtifactClass::Debris, RetentionStatus::Assessed)
    );
    let sensitivity = target.sensitivity.unwrap();
    assert_eq!(
        (sensitivity.value, sensitivity.lower, sensitivity.upper),
        (Some(5.0 / 8.0), Some(5.0 / 14.0), Some(9.0 / 12.0))
    );
    assert_eq!(target.assessed_sensitivity, Some(5.0 / 7.0));
    let specificity = target.specificity.unwrap();
    assert_eq!(
        (specificity.value, specificity.lower, specificity.upper),
        (Some(13.0 / 16.0), Some(13.0 / 20.0), Some(19.0 / 22.0))
    );
    let precision = target.precision.unwrap();
    assert_eq!(
        (precision.value, precision.lower, precision.upper),
        (Some(5.0 / 8.0), Some(5.0 / 12.0), Some(9.0 / 12.0))
    );
    let outcomes = target.outcomes.unwrap();
    assert_eq!(
        outcomes.unlabelled,
        Outcomes {
            removed: 4,
            passed: 6,
            not_assessed: 0
        }
    );
}

#[test]
fn combined_keep_counts_unassessed_events_as_kept() {
    let (label, values) = asymmetric();
    let truth = truth(vec![(ArtifactClass::Debris, label)]);
    let report = artifact_removal(&sha(), 34, &mask_of(vec![("non_debris", values)]), &truth);
    let combined = report.combined.unwrap();
    assert_eq!(combined.kept, 22);
    assert_eq!(combined.total_retention, Some(22.0 / 34.0));
    assert_eq!(
        combined.events.artifact,
        Outcomes {
            removed: 5,
            passed: 3,
            not_assessed: 0
        }
    );
    let recall = combined.artifact_recall;
    assert_eq!(
        (recall.value, recall.lower, recall.upper),
        (Some(5.0 / 8.0), Some(5.0 / 14.0), Some(9.0 / 12.0))
    );
    let retention = combined.clean_retention;
    assert_eq!(
        (retention.value, retention.lower, retention.upper),
        (Some(13.0 / 16.0), Some(13.0 / 20.0), Some(19.0 / 22.0))
    );
    let debris = combined.classes[&ArtifactClass::Debris];
    assert_eq!(debris.positive.removed, 5);
    assert_eq!(debris.negative.kept(), 13);
    let events = report.events.unwrap();
    assert_eq!(
        (
            events.raw,
            events.clean,
            events.artifact,
            events.undetermined
        ),
        (34, 16, 8, 10)
    );
    assert_eq!(events.clean_of, vec![ArtifactClass::Debris]);
}

#[test]
fn bounds_collapse_without_unknown_truth_and_extremes_are_reached() {
    let (yes, no) = (Some(true), Some(false));
    // No unlabelled event: the bounds equal the value.
    let (label, values) = runs(&[(yes, no, 3), (yes, yes, 1), (no, no, 1), (no, yes, 5)]);
    let truth_known = truth(vec![(ArtifactClass::Dead, label)]);
    let report = artifact_removal(&sha(), 10, &mask_of(vec![("live", values)]), &truth_known);
    let target = report.filters[0].target.clone().unwrap();
    for estimate in [target.sensitivity, target.specificity, target.precision] {
        let estimate = estimate.unwrap();
        assert_eq!(estimate.lower, estimate.value);
        assert_eq!(estimate.upper, estimate.value);
    }
    // Every unlabelled event removed: recall can only rise, clean loss too.
    let (label, values) = runs(&[(yes, no, 1), (yes, yes, 1), (no, yes, 2), (None, no, 2)]);
    let removed = truth(vec![(ArtifactClass::Dead, label)]);
    let report = artifact_removal(&sha(), 6, &mask_of(vec![("live", values)]), &removed);
    let sensitivity = report.filters[0]
        .target
        .clone()
        .unwrap()
        .sensitivity
        .unwrap();
    assert_eq!(
        (sensitivity.value, sensitivity.lower, sensitivity.upper),
        (Some(0.5), Some(0.5), Some(3.0 / 4.0))
    );
    let loss = report.filters[0].clean_loss;
    assert_eq!(
        (loss.value, loss.lower, loss.upper),
        (Some(0.0), Some(0.0), Some(0.5))
    );
    // Every unlabelled event kept: recall can only fall.
    let (label, values) = runs(&[(yes, no, 1), (yes, yes, 1), (no, yes, 2), (None, yes, 2)]);
    let kept = truth(vec![(ArtifactClass::Dead, label)]);
    let report = artifact_removal(&sha(), 6, &mask_of(vec![("live", values)]), &kept);
    let sensitivity = report.filters[0]
        .target
        .clone()
        .unwrap()
        .sensitivity
        .unwrap();
    assert_eq!(
        (sensitivity.value, sensitivity.lower, sensitivity.upper),
        (Some(0.5), Some(0.25), Some(0.5))
    );
}

#[test]
fn zero_denominators_are_null_never_zero_or_one() {
    // Only negatives: sensitivity and precision have nothing to measure.
    let (label, values) = runs(&[(Some(false), Some(true), 4)]);
    let truth = truth(vec![(ArtifactClass::Doublet, label)]);
    let report = artifact_removal(&sha(), 4, &mask_of(vec![("singlets", values)]), &truth);
    let filter = &report.filters[0];
    let target = filter.target.clone().unwrap();
    let sensitivity = target.sensitivity.unwrap();
    assert_eq!(
        (sensitivity.value, sensitivity.lower, sensitivity.upper),
        (None, None, None)
    );
    assert_eq!(target.assessed_sensitivity, None);
    assert_eq!(target.precision.unwrap().value, None);
    assert_eq!(target.specificity.unwrap().value, Some(1.0));
    assert_eq!(filter.removed_artifact_share.value, None);
    assert_eq!(report.combined.unwrap().artifact_recall.value, None);
}

#[test]
fn overlapping_classes_count_for_each_class() {
    let (yes, no) = (Some(true), Some(false));
    // Event 0 is a dead doublet, 1 a doublet, 2 dead, 3 clean.
    let truth = truth(vec![
        (ArtifactClass::Doublet, vec![yes, yes, no, no]),
        (ArtifactClass::Dead, vec![yes, no, yes, no]),
    ]);
    let mask = mask_of(vec![
        ("singlets", vec![no, no, yes, yes]),
        ("live", vec![no, yes, yes, yes]),
    ]);
    let report = artifact_removal(&sha(), 4, &mask, &truth);
    let singlets = &report.filters[0];
    assert_eq!(singlets.removed_by_class[&ArtifactClass::Doublet], 2);
    assert_eq!(singlets.removed_by_class[&ArtifactClass::Dead], 1);
    assert_eq!(singlets.events.artifact.removed, 2);
    let live = report.filters[1].target.clone().unwrap();
    assert_eq!(live.class, ArtifactClass::Dead);
    assert_eq!(
        live.outcomes.unwrap().positive,
        Outcomes {
            removed: 1,
            passed: 1,
            not_assessed: 0
        }
    );
    assert_eq!(report.events.unwrap().artifact, 3);
}

#[test]
fn repeated_and_unmapped_columns_are_reported_separately() {
    let (yes, no) = (Some(true), Some(false));
    let truth = truth(vec![(ArtifactClass::Saturated, vec![yes, no, no])]);
    let mask = mask_of(vec![
        ("margin_pass", vec![no, yes, yes]),
        ("2.margin_pass", vec![None, None, None]),
        ("margin_lower_pass", vec![yes, no, yes]),
        ("cd3", vec![yes, yes, no]),
    ]);
    let report = artifact_removal(&sha(), 3, &mask, &truth);
    let columns = report
        .filters
        .iter()
        .map(|filter| filter.column.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        columns,
        ["margin_pass", "2.margin_pass", "margin_lower_pass", "cd3"]
    );
    assert_eq!(
        report.filters[0].target.clone().unwrap().status,
        RetentionStatus::Assessed
    );
    let unassessed = report.filters[1].target.clone().unwrap();
    assert_eq!(unassessed.status, RetentionStatus::NotAssessed);
    assert_eq!(
        unassessed.reason.as_deref(),
        Some("`2.margin_pass` assessed no event")
    );
    assert_eq!(unassessed.sensitivity, None);
    assert_eq!(report.filters[1].assessed, 0);
    assert!(report.filters[2].target.is_none());
    assert!(report.filters[3].target.is_none());
    assert_eq!(report.filters[2].events.clean.removed, 1);
}

#[test]
fn a_target_class_without_labels_is_not_assessed() {
    let (yes, no) = (Some(true), Some(false));
    let truth = truth(vec![
        (ArtifactClass::Debris, vec![yes, no]),
        (ArtifactClass::Dead, vec![None, None]),
    ]);
    let mask = mask_of(vec![("singlets", vec![no, yes]), ("live", vec![no, yes])]);
    let report = artifact_removal(&sha(), 2, &mask, &truth);
    let singlets = report.filters[0].target.clone().unwrap();
    assert_eq!(
        (singlets.status, singlets.reason.as_deref()),
        (
            RetentionStatus::NotAssessed,
            Some("the artifact truth does not label `doublet`")
        )
    );
    let live = report.filters[1].target.clone().unwrap();
    assert_eq!(
        live.reason.as_deref(),
        Some("no event is labelled for `dead`")
    );
    let value = serde_json::to_value(&live).unwrap();
    assert_eq!(
        value,
        json!({"class": "dead", "status": "not_assessed", "reason": "no event is labelled for `dead`"}),
        "a class that was not assessed carries no number"
    );
}

#[test]
fn masks_are_aligned_by_source_event_id() {
    let (yes, no) = (Some(true), Some(false));
    let truth = truth(vec![(ArtifactClass::Debris, vec![yes, no, no])]);
    // Mask rows in raw order 2, 0, 1: only raw event 0 is removed.
    let mask = EventMask::new(sha(), vec![true, false, true], Some(vec![2, 0, 1]))
        .with_filter("non_debris", vec![yes, no, yes]);
    let report = artifact_removal(&sha(), 3, &mask, &truth);
    let combined = report.combined.unwrap();
    assert_eq!(combined.artifact_recall.value, Some(1.0));
    assert_eq!(combined.clean_retention.value, Some(1.0));
    assert_eq!(
        report.filters[0]
            .target
            .clone()
            .unwrap()
            .precision
            .unwrap()
            .value,
        Some(1.0)
    );
}

#[test]
fn incompatible_inputs_are_not_assessed_with_a_reason() {
    let truth = truth(vec![(ArtifactClass::Debris, vec![Some(true), None])]);
    let mask = EventMask::new(sha(), vec![true, true], None);
    let foreign = artifact_removal(&"c".repeat(64), 2, &mask, &truth);
    assert!(foreign.reason.unwrap().contains("belongs to acquisition"));
    let short = artifact_removal(&sha(), 3, &mask, &truth);
    assert_eq!(
        short.reason.as_deref(),
        Some("the artifact truth has 2 event(s), the raw acquisition 3")
    );
    let other_mask = EventMask::new("c".repeat(64), vec![true, true], None);
    let report = artifact_removal(&sha(), 2, &other_mask, &truth);
    assert!(report.reason.unwrap().contains("another acquisition"));
    let partial = EventMask::new(sha(), vec![true, true], Some(vec![0, 0]));
    let report = artifact_removal(&sha(), 2, &partial, &truth);
    assert_eq!(report.status, RetentionStatus::NotAssessed);
    assert!(report.truth.is_none() && report.combined.is_none() && report.filters.is_empty());
    let none = serde_json::to_value(ArtifactRemovalReport::not_assessed(NO_TRUTH)).unwrap();
    assert_eq!(
        none,
        json!({"status": "not_assessed", "reason": NO_TRUTH, "basis": ARTIFACT_BASIS, "filters": []})
    );
}

#[test]
fn the_summary_echoes_declared_evidence_and_label_counts() {
    let truth = truth(vec![(
        ArtifactClass::TimeAnomaly,
        vec![Some(true), Some(false), None, Some(false)],
    )]);
    let report = artifact_removal(
        &sha(),
        4,
        &EventMask::new(sha(), vec![true; 4], None),
        &truth,
    );
    let summary = report.truth.unwrap();
    assert_eq!(summary.contract, "cytiq.qc.truth.v1");
    let class = &summary.classes[0];
    assert_eq!(
        (class.artifacts, class.verified_absent, class.unlabelled),
        (1, 2, 1)
    );
    let value = serde_json::to_value(class).unwrap();
    assert_eq!(value["truth_class"], "synthetic");
    assert_eq!(value["evidence"], "generative");
    assert_eq!(value["class"], "time_anomaly");
    assert_eq!(report.filters, Vec::<FilterRemoval>::new());
}

#[test]
fn every_stage_column_maps_to_the_class_it_removes() {
    let cases = [
        ("non_debris", Some(ArtifactClass::Debris)),
        ("singlets", Some(ArtifactClass::Doublet)),
        ("live", Some(ArtifactClass::Dead)),
        ("peacoqc_pass", Some(ArtifactClass::TimeAnomaly)),
        ("margin_pass", Some(ArtifactClass::Saturated)),
        ("margin_upper_pass", Some(ArtifactClass::Saturated)),
        ("2.2.live", Some(ArtifactClass::Dead)),
        ("margin_lower_pass", None),
        ("debris", None),
        ("3x.live", None),
    ];
    for (column, class) in cases {
        assert_eq!(target_class(column), class, "{column}");
    }
}
