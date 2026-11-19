//! Population retention against the original labelled acquisition, independently
//! of downstream gating accuracy among the events that survived filtering.

use cytiq_mask::EventMask;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// What independent technical truth says about one raw event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventTruth {
    /// Verified free of every labelled artifact class.
    Clean,
    /// At least one labelled artifact class.
    Artifact,
    /// No class says artifact, and at least one class is unlabelled.
    Undetermined,
}

impl EventTruth {
    /// `true` (technically good) is clean, `false` an artifact.
    #[must_use]
    pub const fn of_good(good: bool) -> Self {
        if good { Self::Clean } else { Self::Artifact }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PopulationRetention {
    pub population: String,
    pub original: usize,
    pub eligible: usize,
    pub retained: usize,
    pub conditional_retention: Option<f64>,
    pub cumulative_retention: Option<f64>,
    pub original_frequency: Option<f64>,
    pub retained_frequency: Option<f64>,
    pub frequency_error: Option<f64>,
    pub relative_frequency_error: Option<f64>,
    /// Members verified clean by technical truth.
    pub good_original: Option<usize>,
    pub good_retained: Option<usize>,
    pub good_retention: Option<f64>,
    /// Members that are labelled artifacts and were removed.
    pub technical_removed: Option<usize>,
    /// Members whose technical truth is undetermined and were removed.
    pub undetermined_removed: Option<usize>,
    /// The population's frequency among the clean events of the base: its
    /// frequency in artifact-free data.
    pub clean_frequency: Option<f64>,
    /// `retained_frequency - clean_frequency`.
    pub frequency_error_vs_clean: Option<f64>,
    pub relative_frequency_error_vs_clean: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RetentionStage {
    pub stage: String,
    pub input_events: usize,
    pub kept_events: usize,
    pub newly_removed: usize,
    pub populations: Vec<PopulationRetention>,
}

fn ratio(n: usize, d: usize) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    (d != 0).then(|| n as f64 / d as f64)
}

/// Labels and optional technical-good truth are indexed by raw source event ID.
/// Null labels are excluded from population frequencies. Stages are applied in
/// order; an absent source row cannot reintroduce an event removed earlier.
///
/// # Errors
/// Rejects mismatched acquisitions, lengths, duplicate IDs and IDs outside the
/// original acquisition. No classification or technical truth is inferred.
pub fn population_retention(
    acquisition: &str,
    labels: &[Option<String>],
    technical_good: Option<&[bool]>,
    stages: &[(&str, &EventMask)],
) -> Result<Vec<RetentionStage>, String> {
    if technical_good.is_some_and(|good| good.len() != labels.len()) {
        return Err("technical truth length differs from labels".into());
    }
    let technical = technical_good.map(|good| {
        good.iter()
            .map(|&good| EventTruth::of_good(good))
            .collect::<Vec<_>>()
    });
    let mut populations: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (row, label) in labels.iter().enumerate() {
        if let Some(label) = label {
            populations.entry(label).or_default().push(row);
        }
    }
    let labelled = populations.values().flatten().copied().collect::<Vec<_>>();
    retention(
        acquisition,
        labels.len(),
        &populations.into_iter().collect::<Vec<_>>(),
        Some(&labelled),
        technical.as_deref(),
        stages,
    )
}

/// Retention of possibly overlapping populations, such as the gates of a
/// `cytiq gate --membership` file: `populations` pairs a name with one flag
/// per raw event. Frequencies are relative to all raw events, not to a
/// parent gate, so nested gates are reported independently. Stages are
/// applied in order, as in [`population_retention`].
///
/// # Errors
/// Rejects a membership whose length differs from `events`, and the stage
/// errors of [`population_retention`].
pub fn membership_retention(
    acquisition: &str,
    events: usize,
    populations: &[(&str, &[bool])],
    stages: &[(&str, &EventMask)],
) -> Result<Vec<RetentionStage>, String> {
    membership_retention_with_truth(acquisition, events, populations, None, stages)
}

/// [`membership_retention`] with technical truth per raw event, which adds
/// the clean-event columns of every population.
fn membership_retention_with_truth(
    acquisition: &str,
    events: usize,
    populations: &[(&str, &[bool])],
    technical: Option<&[EventTruth]>,
    stages: &[(&str, &EventMask)],
) -> Result<Vec<RetentionStage>, String> {
    if technical.is_some_and(|truth| truth.len() != events) {
        return Err(format!("technical truth does not have {events} events"));
    }
    let rows = populations
        .iter()
        .map(|&(name, members)| {
            if members.len() == events {
                Ok((
                    name,
                    (0..events).filter(|&row| members[row]).collect::<Vec<_>>(),
                ))
            } else {
                Err(format!(
                    "population `{name}` has {} events, not {events}",
                    members.len()
                ))
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    retention(acquisition, events, &rows, None, technical, stages)
}

/// The raw row of each `source_event_ids` entry, when they are each of the
/// `events` raw events exactly once.
pub(crate) fn raw_rows(source_event_ids: &[u64], events: usize) -> Option<Vec<usize>> {
    if source_event_ids.len() != events {
        return None;
    }
    let mut seen = vec![false; events];
    source_event_ids
        .iter()
        .map(|&source| {
            usize::try_from(source)
                .ok()
                .filter(|&row| row < events && !std::mem::replace(&mut seen[row], true))
        })
        .collect()
}

/// The `basis` every [`PopulationRetentionReport`] states.
pub const RETENTION_BASIS: &str = "descriptive: retention of the supplied populations after QC filtering, relative to every raw event; never a QC check, and it cannot show that a filter removed only artifacts";

/// Whether population retention could be measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionStatus {
    Assessed,
    NotAssessed,
}

/// Population retention reported by `cytiq qc --populations`. It describes
/// what filtering did to each supplied population and never changes a check,
/// the report status or the exit code.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PopulationRetentionReport {
    pub status: RetentionStatus,
    /// Why retention was not assessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub basis: String,
    /// Retention under the mask's final `keep`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined: Option<RetentionStage>,
    /// Each filter column of the mask on its own, against every raw event:
    /// `false` removes an event, null (not assessed) keeps it.
    pub filters: Vec<RetentionStage>,
}

impl PopulationRetentionReport {
    #[must_use]
    pub fn not_assessed(reason: impl Into<String>) -> Self {
        Self {
            status: RetentionStatus::NotAssessed,
            reason: Some(reason.into()),
            basis: RETENTION_BASIS.to_owned(),
            combined: None,
            filters: Vec::new(),
        }
    }
}

/// Retention of the gates in `membership` (a `cytiq gate --membership` file
/// over every raw event of `acquisition`) under the QC mask `qc`, which
/// must cover the same `events`. `technical`, independent artifact truth
/// per raw event, adds the clean-event columns. Anything that prevents an
/// assessment is reported as not assessed with its reason; this never fails.
#[must_use]
pub fn qc_population_retention(
    acquisition: &str,
    events: usize,
    qc: &EventMask,
    membership: &EventMask,
    technical: Option<&[EventTruth]>,
) -> PopulationRetentionReport {
    if membership.acquisition != acquisition {
        return PopulationRetentionReport::not_assessed(format!(
            "the population membership belongs to acquisition {}, not this sample's {acquisition}",
            membership.acquisition
        ));
    }
    let Some(rows) = raw_rows(&membership.source_event_ids, events) else {
        return PopulationRetentionReport::not_assessed(
            "the population membership must have one row per raw event: run `cytiq gate --membership` on the raw acquisition",
        );
    };
    if !membership.keep.iter().all(|keep| *keep)
        || membership
            .filters
            .iter()
            .any(|(_, values)| values.iter().any(Option::is_none))
    {
        return PopulationRetentionReport::not_assessed(
            "the population membership was computed under an event mask, so removed events have no population: run `cytiq gate --membership` without `--mask`",
        );
    }
    if membership.filters.is_empty() {
        return PopulationRetentionReport::not_assessed("the population membership has no gate");
    }
    let populations = membership
        .filters
        .iter()
        .map(|(name, values)| {
            let mut members = vec![false; events];
            for (&row, value) in rows.iter().zip(values) {
                members[row] = *value == Some(true);
            }
            (name.as_str(), members)
        })
        .collect::<Vec<_>>();
    let populations = populations
        .iter()
        .map(|(name, members)| (*name, members.as_slice()))
        .collect::<Vec<_>>();
    let measured = membership_retention_with_truth(
        acquisition,
        events,
        &populations,
        technical,
        &[("combined", qc)],
    )
    .and_then(|mut combined| {
        let filters = qc
            .filters
            .iter()
            .map(|(name, values)| {
                let keep = values.iter().map(|value| *value != Some(false)).collect();
                let stage = EventMask::new(acquisition, keep, Some(qc.source_event_ids.clone()));
                membership_retention_with_truth(
                    acquisition,
                    events,
                    &populations,
                    technical,
                    &[(name, &stage)],
                )
                .map(|mut stages| stages.remove(0))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok((combined.remove(0), filters))
    });
    match measured {
        Ok((combined, filters)) => PopulationRetentionReport {
            status: RetentionStatus::Assessed,
            reason: None,
            basis: RETENTION_BASIS.to_owned(),
            combined: Some(combined),
            filters,
        },
        Err(reason) => PopulationRetentionReport::not_assessed(reason),
    }
}

/// The stage loop shared by both reports. `base` lists the raw rows whose
/// count is the frequency denominator; `None` means every raw event.
fn retention(
    acquisition: &str,
    events: usize,
    populations: &[(&str, Vec<usize>)],
    base: Option<&[usize]>,
    technical: Option<&[EventTruth]>,
    stages: &[(&str, &EventMask)],
) -> Result<Vec<RetentionStage>, String> {
    let base_original = base.map_or(events, <[usize]>::len);
    // Clean events of the base: the denominator of every clean frequency.
    let technical = technical.map(|truth| {
        let clean = |row: &usize| truth[*row] == EventTruth::Clean;
        let base_clean = base.map_or_else(
            || (0..events).filter(clean).count(),
            |rows| rows.iter().filter(|row| clean(row)).count(),
        );
        (truth, base_clean)
    });
    let mut keep = vec![true; events];
    let mut result = Vec::new();
    for &(name, mask) in stages {
        let stage_keep = stage_keep(name, acquisition, events, mask)?;
        let input_events = keep.iter().filter(|v| **v).count();
        let prior = keep.clone();
        for (keep, stage) in keep.iter_mut().zip(stage_keep) {
            *keep &= stage;
        }
        let kept_events = keep.iter().filter(|v| **v).count();
        let base_retained = base.map_or(kept_events, |rows| {
            rows.iter().filter(|&&row| keep[row]).count()
        });
        let state = StageState {
            prior: &prior,
            keep: &keep,
            base_original,
            base_retained,
        };
        result.push(RetentionStage {
            stage: name.into(),
            input_events,
            kept_events,
            newly_removed: input_events - kept_events,
            populations: populations
                .iter()
                .map(|(population, ids)| population_row(population, ids, &state, technical))
                .collect(),
        });
    }
    Ok(result)
}

/// One stage's `keep` per raw event: an event without a row is removed.
fn stage_keep(
    name: &str,
    acquisition: &str,
    events: usize,
    mask: &EventMask,
) -> Result<Vec<bool>, String> {
    if mask.acquisition != acquisition || mask.source_event_ids.len() != mask.keep.len() {
        return Err(format!("{name}: incompatible acquisition or mask lengths"));
    }
    let mut seen = HashSet::new();
    let mut stage_keep = vec![false; events];
    for (&source, &value) in mask.source_event_ids.iter().zip(&mask.keep) {
        let row = usize::try_from(source).map_err(|_| "source ID overflow")?;
        if row >= events || !seen.insert(source) {
            return Err(format!("{name}: invalid or repeated source ID {source}"));
        }
        stage_keep[row] = value;
    }
    Ok(stage_keep)
}

/// Cumulative `keep` before and after a stage, with the base counts.
struct StageState<'a> {
    prior: &'a [bool],
    keep: &'a [bool],
    base_original: usize,
    base_retained: usize,
}

/// One population's members by technical truth after a stage.
#[derive(Default)]
struct TechnicalCounts {
    clean: usize,
    clean_kept: usize,
    artifact_removed: usize,
    undetermined_removed: usize,
}

impl TechnicalCounts {
    fn of(ids: &[usize], truth: &[EventTruth], keep: &[bool]) -> Self {
        let mut counts = Self::default();
        for &row in ids {
            match (truth[row], keep[row]) {
                (EventTruth::Clean, kept) => {
                    counts.clean += 1;
                    counts.clean_kept += usize::from(kept);
                }
                (EventTruth::Artifact, false) => counts.artifact_removed += 1,
                (EventTruth::Undetermined, false) => counts.undetermined_removed += 1,
                (EventTruth::Artifact | EventTruth::Undetermined, true) => {}
            }
        }
        counts
    }
}

fn relative(error: Option<f64>, of: Option<f64>) -> Option<f64> {
    error.zip(of).and_then(|(e, f)| (f != 0.0).then_some(e / f))
}

fn population_row(
    population: &str,
    ids: &[usize],
    state: &StageState<'_>,
    technical: Option<(&[EventTruth], usize)>,
) -> PopulationRetention {
    let original = ids.len();
    let eligible = ids.iter().filter(|&&i| state.prior[i]).count();
    let retained = ids.iter().filter(|&&i| state.keep[i]).count();
    let original_frequency = ratio(original, state.base_original);
    let retained_frequency = ratio(retained, state.base_retained);
    let frequency_error = retained_frequency
        .zip(original_frequency)
        .map(|(a, b)| a - b);
    let counts = technical.map(|(truth, _)| TechnicalCounts::of(ids, truth, state.keep));
    let clean_frequency = technical
        .zip(counts.as_ref())
        .and_then(|((_, base_clean), counts)| ratio(counts.clean, base_clean));
    let frequency_error_vs_clean = retained_frequency.zip(clean_frequency).map(|(a, b)| a - b);
    PopulationRetention {
        population: population.into(),
        original,
        eligible,
        retained,
        conditional_retention: ratio(retained, eligible),
        cumulative_retention: ratio(retained, original),
        original_frequency,
        retained_frequency,
        frequency_error,
        relative_frequency_error: relative(frequency_error, original_frequency),
        good_original: counts.as_ref().map(|c| c.clean),
        good_retained: counts.as_ref().map(|c| c.clean_kept),
        good_retention: counts.as_ref().and_then(|c| ratio(c.clean_kept, c.clean)),
        technical_removed: counts.as_ref().map(|c| c.artifact_removed),
        undetermined_removed: counts.as_ref().map(|c| c.undetermined_removed),
        clean_frequency,
        frequency_error_vs_clean,
        relative_frequency_error_vs_clean: relative(frequency_error_vs_clean, clean_frequency),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_complete_rare_loss_despite_small_overall_removal() {
        let acquisition = "a".repeat(64);
        let mut labels = vec![Some("common".into()); 1000];
        labels[999] = Some("rare_dim".into());
        let mask = EventMask::new(&acquisition, (0..1000).map(|i| i != 999).collect(), None);
        let report = population_retention(
            &acquisition,
            &labels,
            Some(&vec![true; 1000]),
            &[("filter", &mask)],
        )
        .unwrap();
        let rare = &report[0].populations[1];
        assert_eq!(report[0].newly_removed, 1);
        assert_eq!(rare.cumulative_retention, Some(0.0));
        assert_eq!(rare.good_retention, Some(0.0));
        assert_eq!(rare.relative_frequency_error, Some(-1.0));
    }
    #[test]
    fn overlapping_gates_keep_independent_retention() {
        let acquisition = "c".repeat(64);
        // Events 0-3 are T cells, 2-5 are CD4 (overlapping); the filter
        // removes events 0 and 5.
        let t = [true, true, true, true, false, false];
        let cd4 = [false, false, true, true, true, true];
        let mask = EventMask::new(
            &acquisition,
            vec![false, true, true, true, true, false],
            None,
        );
        let report = membership_retention(
            &acquisition,
            6,
            &[("T", &t), ("CD4", &cd4)],
            &[("filter", &mask)],
        )
        .unwrap();
        let [t, cd4] = &report[0].populations[..] else {
            panic!("two populations in input order")
        };
        assert_eq!(
            (t.population.as_str(), cd4.population.as_str()),
            ("T", "CD4")
        );
        assert_eq!((t.original, t.retained), (4, 3));
        assert_eq!((cd4.original, cd4.retained), (4, 3));
        assert_eq!(t.cumulative_retention, Some(0.75));
        assert_eq!(t.good_retention, None);
    }

    #[test]
    fn nested_gate_frequencies_are_relative_to_all_events() {
        let acquisition = "d".repeat(64);
        let cells = [true; 4];
        let t = [true, true, false, false];
        let mask = EventMask::new(&acquisition, vec![true, false, true, true], None);
        let report = membership_retention(
            &acquisition,
            4,
            &[("cells", &cells), ("T", &t)],
            &[("filter", &mask)],
        )
        .unwrap();
        let t = &report[0].populations[1];
        assert_eq!(t.original_frequency, Some(0.5));
        assert_eq!(t.retained_frequency, Some(1.0 / 3.0));
        assert_eq!(report[0].populations[0].retained_frequency, Some(1.0));
        let short = [true; 4];
        assert!(membership_retention(&acquisition, 5, &[("T", &short)], &[]).is_err());
    }

    #[test]
    fn matches_source_ids_and_separates_technical_removal() {
        let acquisition = "b".repeat(64);
        let labels = vec![Some("dim".into()); 3];
        let first = EventMask::new(&acquisition, vec![true, false, true], Some(vec![2, 0, 1]));
        let second = EventMask::new(&acquisition, vec![true], Some(vec![1]));
        let r = population_retention(
            &acquisition,
            &labels,
            Some(&[false, true, true]),
            &[("a", &first), ("b", &second)],
        )
        .unwrap();
        assert_eq!(r[0].populations[0].good_retention, Some(1.0));
        assert_eq!(r[0].populations[0].technical_removed, Some(1));
        assert_eq!(r[1].populations[0].conditional_retention, Some(0.5));
        assert_eq!(r[1].populations[0].good_retention, Some(0.5));
    }

    #[test]
    fn undetermined_events_are_neither_clean_nor_artifacts() {
        use EventTruth::{Artifact, Clean, Undetermined};
        let acquisition = "e".repeat(64);
        let members = [true; 6];
        let truth = [Clean, Clean, Artifact, Artifact, Undetermined, Undetermined];
        // Removes one clean, one artifact and one undetermined event.
        let mask = EventMask::new(
            &acquisition,
            vec![true, false, true, false, true, false],
            None,
        );
        let report = membership_retention_with_truth(
            &acquisition,
            6,
            &[("all", &members)],
            Some(&truth),
            &[("filter", &mask)],
        )
        .unwrap();
        let all = &report[0].populations[0];
        assert_eq!((all.good_original, all.good_retained), (Some(2), Some(1)));
        assert_eq!(all.good_retention, Some(0.5));
        assert_eq!(all.technical_removed, Some(1));
        assert_eq!(all.undetermined_removed, Some(1));
        let without =
            membership_retention(&acquisition, 6, &[("all", &members)], &[("f", &mask)]).unwrap();
        let all = &without[0].populations[0];
        assert_eq!(all.good_original, None);
        assert_eq!(all.undetermined_removed, None);
        assert_eq!(all.clean_frequency, None);
        assert_eq!(all.frequency_error_vs_clean, None);
    }

    #[test]
    fn frequency_error_is_also_measured_against_clean_events() {
        use EventTruth::{Artifact, Clean};
        let acquisition = "f".repeat(64);
        // Events 0-3 clean (0-1 in `t`), 4-7 artifacts (4-7 in `t`).
        let t = [true, true, false, false, true, true, true, true];
        let truth = [
            Clean, Clean, Clean, Clean, Artifact, Artifact, Artifact, Artifact,
        ];
        // Removes every artifact: `t` falls from 6/8 overall to 2/4, its
        // frequency among clean events.
        let mask = EventMask::new(&acquisition, (0..8).map(|i| i < 4).collect(), None);
        let report = membership_retention_with_truth(
            &acquisition,
            8,
            &[("t", &t)],
            Some(&truth),
            &[("filter", &mask)],
        )
        .unwrap();
        let t = &report[0].populations[0];
        assert_eq!(t.original_frequency, Some(0.75));
        assert_eq!(t.retained_frequency, Some(0.5));
        assert_eq!(t.frequency_error, Some(-0.25));
        assert_eq!(t.relative_frequency_error, Some(-0.25 / 0.75));
        assert_eq!(t.clean_frequency, Some(0.5));
        assert_eq!(t.frequency_error_vs_clean, Some(0.0));
        assert_eq!(t.relative_frequency_error_vs_clean, Some(0.0));
        assert_eq!(t.technical_removed, Some(4));
        assert_eq!(t.good_retention, Some(1.0));
    }

    #[test]
    fn clean_frequency_is_relative_to_the_labelled_base() {
        let acquisition = "g".repeat(64);
        // Event 3 is unlabelled and clean: it is outside the base.
        let labels = vec![Some("a".into()), Some("b".into()), Some("b".into()), None];
        let mask = EventMask::new(&acquisition, vec![true, true, false, true], None);
        let report = population_retention(
            &acquisition,
            &labels,
            Some(&[true, true, false, true]),
            &[("filter", &mask)],
        )
        .unwrap();
        let [a, b] = &report[0].populations[..] else {
            panic!("two populations")
        };
        assert_eq!(a.clean_frequency, Some(0.5));
        assert_eq!(b.clean_frequency, Some(0.5));
        assert_eq!(a.retained_frequency, Some(0.5));
        assert_eq!(b.frequency_error_vs_clean, Some(0.0));
        assert_eq!(b.undetermined_removed, Some(0));
    }

    #[test]
    fn clean_frequency_is_null_without_clean_events() {
        let acquisition = "h".repeat(64);
        let members = [true, true];
        let truth = [EventTruth::Artifact, EventTruth::Undetermined];
        let mask = EventMask::new(&acquisition, vec![true, true], None);
        let report = membership_retention_with_truth(
            &acquisition,
            2,
            &[("p", &members)],
            Some(&truth),
            &[("filter", &mask)],
        )
        .unwrap();
        let p = &report[0].populations[0];
        assert_eq!(p.clean_frequency, None);
        assert_eq!(p.good_retention, None);
        assert_eq!(p.relative_frequency_error_vs_clean, None);
        assert!(
            membership_retention_with_truth(
                &acquisition,
                2,
                &[("p", &members)],
                Some(&truth[..1]),
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn a_lost_rare_clean_event_shows_while_total_clean_loss_is_tiny() {
        let acquisition = "i".repeat(64);
        let events = 2_000;
        let rare = (0..events).map(|i| i == 1_999).collect::<Vec<_>>();
        let common = rare.iter().map(|r| !r).collect::<Vec<_>>();
        let truth = vec![EventTruth::Clean; events];
        let mask = EventMask::new(
            &acquisition,
            (0..events).map(|i| i != 1_999).collect(),
            None,
        );
        let report = membership_retention_with_truth(
            &acquisition,
            events,
            &[("common", &common), ("rare", &rare)],
            Some(&truth),
            &[("filter", &mask)],
        )
        .unwrap();
        assert_eq!(report[0].newly_removed, 1);
        let [common, rare] = &report[0].populations[..] else {
            panic!("two populations")
        };
        assert_eq!(common.good_retention, Some(1.0));
        assert_eq!(rare.good_retention, Some(0.0));
        assert_eq!(rare.relative_frequency_error_vs_clean, Some(-1.0));
    }

    #[test]
    fn raw_rows_requires_each_raw_event_exactly_once() {
        assert_eq!(raw_rows(&[2, 0, 1], 3), Some(vec![2, 0, 1]));
        assert_eq!(raw_rows(&[], 0), Some(vec![]));
        assert_eq!(raw_rows(&[0, 1], 3), None);
        assert_eq!(raw_rows(&[0, 1, 2, 3], 3), None);
        assert_eq!(raw_rows(&[0, 0, 1], 3), None);
        assert_eq!(raw_rows(&[0, 1, 3], 3), None);
        assert_eq!(raw_rows(&[0, 1, u64::MAX], 3), None);
    }

    #[test]
    fn qc_retention_passes_technical_truth_to_every_stage() {
        let acquisition = "j".repeat(64);
        let qc = EventMask::new(&acquisition, vec![true, false, true], None)
            .with_filter("live", vec![Some(true), Some(false), None]);
        let membership = EventMask::new(&acquisition, vec![true; 3], None)
            .with_filter("cells", vec![Some(true); 3]);
        let truth = [EventTruth::Clean, EventTruth::Artifact, EventTruth::Clean];
        let report = qc_population_retention(&acquisition, 3, &qc, &membership, Some(&truth));
        assert_eq!(report.status, RetentionStatus::Assessed);
        let combined = &report.combined.unwrap().populations[0];
        assert_eq!(combined.technical_removed, Some(1));
        assert_eq!(combined.good_retention, Some(1.0));
        assert_eq!(report.filters[0].populations[0].technical_removed, Some(1));
        let plain = qc_population_retention(&acquisition, 3, &qc, &membership, None);
        assert_eq!(
            plain.combined.unwrap().populations[0].technical_removed,
            None
        );
    }

    #[test]
    fn newly_removed_counts_the_events_each_stage_dropped() {
        let acquisition = "k".repeat(64);
        let labels = vec![Some("p".into()); 6];
        let first = EventMask::new(
            &acquisition,
            vec![true, true, false, true, false, true],
            None,
        );
        let second = EventMask::new(
            &acquisition,
            vec![true, false, true, false, true, false],
            None,
        );
        let report = population_retention(
            &acquisition,
            &labels,
            None,
            &[("first", &first), ("second", &second)],
        )
        .unwrap();
        assert_eq!(
            (
                report[0].input_events,
                report[0].kept_events,
                report[0].newly_removed
            ),
            (6, 4, 2)
        );
        assert_eq!(
            (
                report[1].input_events,
                report[1].kept_events,
                report[1].newly_removed
            ),
            (4, 1, 3)
        );
    }

    #[test]
    fn stages_reject_foreign_or_malformed_masks() {
        let acquisition = "l".repeat(64);
        let labels = vec![Some("p".into()); 3];
        let rejects = |mask: &EventMask| {
            population_retention(&acquisition, &labels, None, &[("stage", mask)]).is_err()
        };
        assert!(rejects(&EventMask::new(
            "m".repeat(64),
            vec![true; 3],
            None
        )));
        let mut ragged = EventMask::new(&acquisition, vec![true; 3], None);
        ragged.source_event_ids.pop();
        assert!(rejects(&ragged));
        assert!(rejects(&EventMask::new(
            &acquisition,
            vec![true; 3],
            Some(vec![0, 1, 3])
        )));
        assert!(rejects(&EventMask::new(
            &acquisition,
            vec![true; 3],
            Some(vec![0, 1, 1])
        )));
        assert!(!rejects(&EventMask::new(
            &acquisition,
            vec![true; 3],
            Some(vec![2, 1, 0])
        )));
    }

    #[test]
    fn a_membership_computed_under_a_mask_is_not_assessed_either_way() {
        let acquisition = "n".repeat(64);
        let qc = EventMask::new(&acquisition, vec![true; 2], None);
        // Every event kept but one gate value unassessed.
        let unassessed = EventMask::new(&acquisition, vec![true; 2], None)
            .with_filter("cells", vec![Some(true), None]);
        // Every gate value assessed but one event removed.
        let removed = EventMask::new(&acquisition, vec![true, false], None)
            .with_filter("cells", vec![Some(true), Some(false)]);
        for membership in [unassessed, removed] {
            let report = qc_population_retention(&acquisition, 2, &qc, &membership, None);
            assert_eq!(report.status, RetentionStatus::NotAssessed);
            assert!(report.reason.unwrap().contains("--mask"));
        }
    }
}
