//! Removal of labelled technical artifacts by each QC mask column and by the
//! final `keep`, against independent per-event truth
//! ([`crate::truth::ArtifactTruth`]). Descriptive only: nothing here is a
//! check, a status or a release decision.
//!
//! A column's outcome per event is removed (`false`), passed (`true`) or not
//! assessed (null), which keeps the event. Each event is clean, an artifact
//! or undetermined ([`EventTruth`]). Every ratio has a point value over the
//! events whose truth is known and sharp bounds over all assignments of the
//! undetermined (or unlabelled) events; the bounds hold for each metric on
//! its own, not jointly. A zero denominator gives null, never 0 or 1.

use std::collections::BTreeMap;

use cytiq_mask::EventMask;
use serde::Serialize;

use crate::policy::base_name;
use crate::retention::{EventTruth, RetentionStatus, raw_rows};
use crate::truth::{
    Annotation, ArtifactClass, ArtifactTruth, ClassLabels, TRUTH_CONTRACT, TruthClass,
};

/// The `basis` every [`ArtifactRemovalReport`] states.
pub const ARTIFACT_BASIS: &str = "descriptive: removal of labelled technical artifacts and loss of events verified clean of the labelled classes, against truth whose evidence its descriptor declares; never a QC check, status, exit code or release decision";

/// Why a report without truth is not assessed.
pub const NO_TRUTH: &str = "no artifact truth supplied (`--artifact-truth`); `status` covers the policy checks only, not artifact removal";

/// The class a mask column is meant to remove. Combine prefixes (`2.live`)
/// are ignored; `margin_lower_pass` and unknown columns have none.
#[must_use]
pub fn target_class(column: &str) -> Option<ArtifactClass> {
    match base_name(column) {
        "non_debris" => Some(ArtifactClass::Debris),
        "singlets" => Some(ArtifactClass::Doublet),
        "live" => Some(ArtifactClass::Dead),
        "peacoqc_pass" => Some(ArtifactClass::TimeAnomaly),
        "margin_pass" | "margin_upper_pass" => Some(ArtifactClass::Saturated),
        _ => None,
    }
}

/// A ratio over known truth with its bounds over unknown truth.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Estimate {
    pub value: Option<f64>,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
}

fn ratio(part: usize, whole: usize) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    (whole != 0).then(|| part as f64 / whole as f64)
}

impl Estimate {
    /// `hit / total` over known events, where each unknown event counts as a
    /// hit (`unknown_hit`) or a miss (`unknown_miss`) depending on its truth.
    /// The lower bound makes every unknown miss a member of `total`; the
    /// upper bound every unknown hit.
    fn share(hit: usize, total: usize, unknown_hit: usize, unknown_miss: usize) -> Self {
        Self {
            value: ratio(hit, total),
            lower: ratio(hit, total + unknown_miss),
            upper: ratio(hit + unknown_hit, total + unknown_hit),
        }
    }

    /// The share of `wanted` among `wanted + other` events, with `unknown`
    /// events of either kind.
    fn purity(wanted: usize, other: usize, unknown: usize) -> Self {
        let all = wanted + other + unknown;
        Self {
            value: ratio(wanted, wanted + other),
            lower: ratio(wanted, all),
            upper: ratio(wanted + unknown, all),
        }
    }
}

/// What a column (or the final `keep`) did to a set of events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Outcomes {
    pub removed: usize,
    pub passed: usize,
    /// Null in the column; always 0 for the final `keep`.
    pub not_assessed: usize,
}

impl Outcomes {
    fn add(&mut self, value: Option<bool>) {
        match value {
            Some(false) => self.removed += 1,
            Some(true) => self.passed += 1,
            None => self.not_assessed += 1,
        }
    }

    /// Events left in the data: passed or not assessed.
    #[must_use]
    pub const fn kept(&self) -> usize {
        self.passed + self.not_assessed
    }

    #[must_use]
    pub const fn total(&self) -> usize {
        self.removed + self.kept()
    }
}

/// Outcomes by event truth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StateOutcomes {
    pub clean: Outcomes,
    pub artifact: Outcomes,
    pub undetermined: Outcomes,
}

impl StateOutcomes {
    fn add(&mut self, state: EventTruth, value: Option<bool>) {
        match state {
            EventTruth::Clean => self.clean.add(value),
            EventTruth::Artifact => self.artifact.add(value),
            EventTruth::Undetermined => self.undetermined.add(value),
        }
    }

    /// Share of clean events removed: the harm.
    fn clean_loss(&self) -> Estimate {
        let (clean, unknown) = (&self.clean, &self.undetermined);
        Estimate::share(
            clean.removed,
            clean.total(),
            unknown.removed,
            unknown.kept(),
        )
    }

    /// Share of clean events kept.
    fn clean_retention(&self) -> Estimate {
        let (clean, unknown) = (&self.clean, &self.undetermined);
        Estimate::share(clean.kept(), clean.total(), unknown.kept(), unknown.removed)
    }

    /// Share of artifacts removed.
    fn artifact_recall(&self) -> Estimate {
        let (artifact, unknown) = (&self.artifact, &self.undetermined);
        Estimate::share(
            artifact.removed,
            artifact.total(),
            unknown.removed,
            unknown.kept(),
        )
    }

    /// Share of removed events that are artifacts.
    fn removed_artifact_share(&self) -> Estimate {
        Estimate::purity(
            self.artifact.removed,
            self.clean.removed,
            self.undetermined.removed,
        )
    }

    const fn total(&self) -> usize {
        self.clean.total() + self.artifact.total() + self.undetermined.total()
    }
}

/// Outcomes by one class's labels: positives are artifacts of the class,
/// negatives verified free of it, unlabelled are null.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ClassOutcomes {
    pub positive: Outcomes,
    pub negative: Outcomes,
    pub unlabelled: Outcomes,
}

impl ClassOutcomes {
    fn of(label: &[Option<bool>], values: impl Iterator<Item = (usize, Option<bool>)>) -> Self {
        let mut outcomes = Self::default();
        for (row, value) in values {
            match label[row] {
                Some(true) => outcomes.positive.add(value),
                Some(false) => outcomes.negative.add(value),
                None => outcomes.unlabelled.add(value),
            }
        }
        outcomes
    }
}

/// A column against the class it targets.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TargetRemoval {
    pub class: ArtifactClass,
    pub status: RetentionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcomes: Option<ClassOutcomes>,
    /// Positives removed over all positives: a positive the column did not
    /// assess stays in the data, so it counts as missed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<Estimate>,
    /// Positives removed over positives the column assessed: diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessed_sensitivity: Option<f64>,
    /// Negatives kept over all negatives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub specificity: Option<Estimate>,
    /// Positives among the removed events labelled for the class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<Estimate>,
}

impl TargetRemoval {
    fn not_assessed(class: ArtifactClass, reason: String) -> Self {
        Self {
            class,
            status: RetentionStatus::NotAssessed,
            reason: Some(reason),
            outcomes: None,
            sensitivity: None,
            assessed_sensitivity: None,
            specificity: None,
            precision: None,
        }
    }

    fn assessed(class: ArtifactClass, outcomes: ClassOutcomes) -> Self {
        let ClassOutcomes {
            positive,
            negative,
            unlabelled,
        } = outcomes;
        Self {
            class,
            status: RetentionStatus::Assessed,
            reason: None,
            outcomes: Some(outcomes),
            sensitivity: Some(Estimate::share(
                positive.removed,
                positive.total(),
                unlabelled.removed,
                unlabelled.kept(),
            )),
            assessed_sensitivity: ratio(positive.removed, positive.removed + positive.passed),
            specificity: Some(Estimate::share(
                negative.kept(),
                negative.total(),
                unlabelled.kept(),
                unlabelled.removed,
            )),
            precision: Some(Estimate::purity(
                positive.removed,
                negative.removed,
                unlabelled.removed,
            )),
        }
    }
}

/// One mask column against the truth. Columns are never merged by filter
/// name: compensate's physical `margin_pass` and `PeacoQC`'s legacy
/// `2.margin_pass` mean different things.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FilterRemoval {
    pub column: String,
    /// Events the column assessed (non-null).
    pub assessed: usize,
    pub events: StateOutcomes,
    /// Removed artifacts of each labelled class; an event in two classes
    /// counts for both.
    pub removed_by_class: BTreeMap<ArtifactClass, usize>,
    pub clean_loss: Estimate,
    pub removed_artifact_share: Estimate,
    /// The class the column is meant to remove, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetRemoval>,
}

/// The final `keep` against the truth.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CombinedRemoval {
    pub kept: usize,
    pub total_retention: Option<f64>,
    pub events: StateOutcomes,
    pub artifact_recall: Estimate,
    pub clean_retention: Estimate,
    pub removed_artifact_share: Estimate,
    pub classes: BTreeMap<ArtifactClass, ClassOutcomes>,
}

/// One labelled class as its descriptor declares it, with its label counts.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClassSummary {
    pub class: ArtifactClass,
    pub truth_class: TruthClass,
    #[serde(flatten)]
    pub labels: ClassLabels,
    pub artifacts: usize,
    pub verified_absent: usize,
    pub unlabelled: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TruthSummary {
    pub contract: &'static str,
    pub annotation: Annotation,
    pub classes: Vec<ClassSummary>,
}

/// Raw events by truth. `clean_of` lists the classes an event was verified
/// free of to count as clean: it says nothing about the others.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EventCounts {
    pub raw: usize,
    pub clean: usize,
    pub artifact: usize,
    pub undetermined: usize,
    pub clean_of: Vec<ArtifactClass>,
}

/// The `artifact_removal` section of `qc.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ArtifactRemovalReport {
    pub status: RetentionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub basis: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truth: Option<TruthSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<EventCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined: Option<CombinedRemoval>,
    pub filters: Vec<FilterRemoval>,
}

impl ArtifactRemovalReport {
    #[must_use]
    pub fn not_assessed(reason: impl Into<String>) -> Self {
        Self {
            status: RetentionStatus::NotAssessed,
            reason: Some(reason.into()),
            basis: ARTIFACT_BASIS.to_owned(),
            truth: None,
            events: None,
            combined: None,
            filters: Vec::new(),
        }
    }
}

/// Artifact removal by `qc`, a mask over every one of the `events` raw
/// events of `acquisition`, against `truth`. Anything that prevents an
/// assessment is reported as not assessed with its reason; this never fails.
#[must_use]
pub fn artifact_removal(
    acquisition: &str,
    events: usize,
    qc: &EventMask,
    truth: &ArtifactTruth,
) -> ArtifactRemovalReport {
    if truth.acquisition != acquisition {
        return ArtifactRemovalReport::not_assessed(format!(
            "the artifact truth belongs to acquisition {}, not this sample's {acquisition}",
            truth.acquisition
        ));
    }
    if truth.events() != events {
        return ArtifactRemovalReport::not_assessed(format!(
            "the artifact truth has {} event(s), the raw acquisition {events}",
            truth.events()
        ));
    }
    if qc.acquisition != acquisition {
        return ArtifactRemovalReport::not_assessed("the QC mask belongs to another acquisition");
    }
    let Some(rows) = raw_rows(&qc.source_event_ids, events) else {
        return ArtifactRemovalReport::not_assessed("the QC mask must have one row per raw event");
    };
    let states = truth.event_states();
    let filters = qc
        .filters
        .iter()
        .map(|(column, values)| filter_removal(column, values, &rows, &states, truth))
        .collect();
    let keep = qc.keep.iter().map(|&keep| Some(keep)).collect::<Vec<_>>();
    ArtifactRemovalReport {
        status: RetentionStatus::Assessed,
        reason: None,
        basis: ARTIFACT_BASIS.to_owned(),
        truth: Some(truth_summary(truth)),
        events: Some(event_counts(&states, truth)),
        combined: Some(combined_removal(&keep, &rows, &states, truth)),
        filters,
    }
}

fn state_outcomes(values: &[Option<bool>], rows: &[usize], states: &[EventTruth]) -> StateOutcomes {
    let mut outcomes = StateOutcomes::default();
    for (&row, &value) in rows.iter().zip(values) {
        outcomes.add(states[row], value);
    }
    debug_assert_eq!(outcomes.total(), states.len());
    outcomes
}

/// Mask values paired with their raw rows.
fn by_row<'a>(
    values: &'a [Option<bool>],
    rows: &'a [usize],
) -> impl Iterator<Item = (usize, Option<bool>)> + 'a {
    rows.iter().copied().zip(values.iter().copied())
}

fn filter_removal(
    column: &str,
    values: &[Option<bool>],
    rows: &[usize],
    states: &[EventTruth],
    truth: &ArtifactTruth,
) -> FilterRemoval {
    let events = state_outcomes(values, rows, states);
    let removed_by_class = truth
        .classes
        .iter()
        .map(|(class, label)| {
            let removed = by_row(values, rows)
                .filter(|&(row, value)| value == Some(false) && label[row] == Some(true))
                .count();
            (*class, removed)
        })
        .collect();
    let assessed = values.iter().filter(|value| value.is_some()).count();
    FilterRemoval {
        column: column.to_owned(),
        assessed,
        events,
        removed_by_class,
        clean_loss: events.clean_loss(),
        removed_artifact_share: events.removed_artifact_share(),
        target: target_class(column)
            .map(|class| target_removal(column, class, assessed, values, rows, truth)),
    }
}

fn target_removal(
    column: &str,
    class: ArtifactClass,
    assessed: usize,
    values: &[Option<bool>],
    rows: &[usize],
    truth: &ArtifactTruth,
) -> TargetRemoval {
    let name = class.column();
    let Some(label) = truth.class(class) else {
        return TargetRemoval::not_assessed(
            class,
            format!("the artifact truth does not label `{name}`"),
        );
    };
    if label.iter().all(Option::is_none) {
        return TargetRemoval::not_assessed(class, format!("no event is labelled for `{name}`"));
    }
    if assessed == 0 {
        return TargetRemoval::not_assessed(class, format!("`{column}` assessed no event"));
    }
    TargetRemoval::assessed(class, ClassOutcomes::of(label, by_row(values, rows)))
}

fn combined_removal(
    keep: &[Option<bool>],
    rows: &[usize],
    states: &[EventTruth],
    truth: &ArtifactTruth,
) -> CombinedRemoval {
    let events = state_outcomes(keep, rows, states);
    let kept = keep.iter().filter(|keep| **keep == Some(true)).count();
    CombinedRemoval {
        kept,
        total_retention: ratio(kept, keep.len()),
        events,
        artifact_recall: events.artifact_recall(),
        clean_retention: events.clean_retention(),
        removed_artifact_share: events.removed_artifact_share(),
        classes: truth
            .classes
            .iter()
            .map(|(class, label)| (*class, ClassOutcomes::of(label, by_row(keep, rows))))
            .collect(),
    }
}

fn truth_summary(truth: &ArtifactTruth) -> TruthSummary {
    let classes = truth
        .classes
        .iter()
        .filter_map(|(class, label)| {
            let labels = truth.descriptor.classes.get(*class)?.clone();
            let count = |wanted| label.iter().filter(|value| **value == wanted).count();
            Some(ClassSummary {
                class: *class,
                truth_class: labels.truth_class(),
                labels,
                artifacts: count(Some(true)),
                verified_absent: count(Some(false)),
                unlabelled: count(None),
            })
        })
        .collect();
    TruthSummary {
        contract: TRUTH_CONTRACT,
        annotation: truth.descriptor.annotation.clone(),
        classes,
    }
}

fn event_counts(states: &[EventTruth], truth: &ArtifactTruth) -> EventCounts {
    let count = |wanted| states.iter().filter(|state| **state == wanted).count();
    EventCounts {
        raw: states.len(),
        clean: count(EventTruth::Clean),
        artifact: count(EventTruth::Artifact),
        undetermined: count(EventTruth::Undetermined),
        clean_of: truth.classes.iter().map(|(class, _)| *class).collect(),
    }
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
