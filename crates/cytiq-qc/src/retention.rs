//! Population retention against the original labelled acquisition, independently
//! of downstream gating accuracy among the events that survived filtering.

use cytiq_mask::EventMask;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
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
    let original_labelled = populations.values().map(Vec::len).sum();
    let mut keep = vec![true; labels.len()];
    let mut result = Vec::new();
    for &(name, mask) in stages {
        if mask.acquisition != acquisition || mask.source_event_ids.len() != mask.keep.len() {
            return Err(format!("{name}: incompatible acquisition or mask lengths"));
        }
        let mut seen = HashSet::new();
        let mut stage_keep = vec![false; labels.len()];
        for (&source, &value) in mask.source_event_ids.iter().zip(&mask.keep) {
            let row = usize::try_from(source).map_err(|_| "source ID overflow")?;
            if row >= labels.len() || !seen.insert(source) {
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
        let labelled = populations.values().flatten().filter(|&&i| keep[i]).count();
        let rows = populations
            .iter()
            .map(|(&population, ids)| {
                let original = ids.len();
                let eligible = ids.iter().filter(|&&i| prior[i]).count();
                let retained = ids.iter().filter(|&&i| keep[i]).count();
                let original_frequency = ratio(original, original_labelled);
                let retained_frequency = ratio(retained, labelled);
                let frequency_error = retained_frequency
                    .zip(original_frequency)
                    .map(|(a, b)| a - b);
                let good_original =
                    technical_good.map(|good| ids.iter().filter(|&&i| good[i]).count());
                let good_retained =
                    technical_good.map(|good| ids.iter().filter(|&&i| good[i] && keep[i]).count());
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
