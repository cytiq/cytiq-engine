//! Population retention against the original labelled acquisition, independently
//! of downstream gating accuracy among the events that survived filtering.

use cytiq_mask::EventMask;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

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
    pub good_original: Option<usize>,
    pub good_retained: Option<usize>,
    pub good_retention: Option<f64>,
    pub technical_removed: Option<usize>,
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
        technical_good,
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
    retention(acquisition, events, &rows, None, None, stages)
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
/// must cover the same `events`. Anything that prevents an assessment is
/// reported as not assessed with its reason; this never fails.
#[must_use]
pub fn qc_population_retention(
    acquisition: &str,
    events: usize,
    qc: &EventMask,
    membership: &EventMask,
) -> PopulationRetentionReport {
    if membership.acquisition != acquisition {
        return PopulationRetentionReport::not_assessed(format!(
            "the population membership belongs to acquisition {}, not this sample's {acquisition}",
            membership.acquisition
        ));
    }
    // Raw rows of the membership, when they are each raw event exactly once.
    let mut seen = vec![false; events];
    let rows = membership
        .source_event_ids
        .iter()
        .map(|&source| {
            usize::try_from(source)
                .ok()
                .filter(|&row| row < events && !std::mem::replace(&mut seen[row], true))
        })
        .collect::<Option<Vec<_>>>()
        .filter(|rows| rows.len() == events);
    let Some(rows) = rows else {
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
    let measured = membership_retention(acquisition, events, &populations, &[("combined", qc)])
        .and_then(|mut combined| {
            let filters = qc
                .filters
                .iter()
                .map(|(name, values)| {
                    let keep = values.iter().map(|value| *value != Some(false)).collect();
                    let stage =
                        EventMask::new(acquisition, keep, Some(qc.source_event_ids.clone()));
                    membership_retention(acquisition, events, &populations, &[(name, &stage)])
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
    technical_good: Option<&[bool]>,
    stages: &[(&str, &EventMask)],
) -> Result<Vec<RetentionStage>, String> {
    let base_original = base.map_or(events, <[usize]>::len);
    let mut keep = vec![true; events];
    let mut result = Vec::new();
    for &(name, mask) in stages {
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
        let input_events = keep.iter().filter(|v| **v).count();
        let prior = keep.clone();
        for (keep, stage) in keep.iter_mut().zip(stage_keep) {
            *keep &= stage;
        }
        let kept_events = keep.iter().filter(|v| **v).count();
        let base_retained = base.map_or(kept_events, |rows| {
            rows.iter().filter(|&&row| keep[row]).count()
        });
        let rows = populations
            .iter()
            .map(|(population, ids)| {
                let original = ids.len();
                let eligible = ids.iter().filter(|&&i| prior[i]).count();
                let retained = ids.iter().filter(|&&i| keep[i]).count();
                let original_frequency = ratio(original, base_original);
                let retained_frequency = ratio(retained, base_retained);
                let frequency_error = retained_frequency
                    .zip(original_frequency)
                    .map(|(a, b)| a - b);
                let good_original =
                    technical_good.map(|good| ids.iter().filter(|&&i| good[i]).count());
                let good_retained =
                    technical_good.map(|good| ids.iter().filter(|&&i| good[i] && keep[i]).count());
                PopulationRetention {
                    population: (*population).into(),
                    original,
                    eligible,
                    retained,
                    conditional_retention: ratio(retained, eligible),
                    cumulative_retention: ratio(retained, original),
                    original_frequency,
                    retained_frequency,
                    frequency_error,
                    relative_frequency_error: frequency_error
                        .zip(original_frequency)
                        .and_then(|(e, f)| (f != 0.0).then_some(e / f)),
                    good_original,
                    good_retained,
                    good_retention: good_retained
                        .zip(good_original)
                        .and_then(|(n, d)| ratio(n, d)),
                    technical_removed: technical_good
                        .map(|good| ids.iter().filter(|&&i| !good[i] && !keep[i]).count()),
                }
            })
            .collect();
        result.push(RetentionStage {
            stage: name.into(),
            input_events,
            kept_events,
            newly_removed: input_events - kept_events,
            populations: rows,
        });
    }
    Ok(result)
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
}
