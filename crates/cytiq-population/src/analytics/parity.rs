//! Observations against the label tables of the same native output: the
//! counts `cytiq population observe` computed must equal the label tables'
//! exact counts, and fractions must be bit-equal to count / denominator.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::inputs::{Descriptor, Inputs, LabelTable};
use crate::labels::{LabelKind, LabelState};
use crate::{Measure, PopulationError, PopulationObservation, Result};

/// How far the observations of one label table were checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParityLevel {
    /// Only the assessed event count: no descriptor names the bindings.
    AssessedEvents,
    /// Every count, denominator and fraction, through the listed
    /// descriptor's bindings.
    Counts,
}

/// The observations checked against one label table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParityEntry {
    pub annotation_id: String,
    pub observations: u64,
    pub level: ParityLevel,
}

/// Exact counts of one label table.
struct Counts<'a> {
    table: &'a LabelTable,
    cells: HashMap<(&'a str, Option<&'a str>, LabelState), u64>,
}

impl<'a> Counts<'a> {
    fn of(table: &'a LabelTable) -> Self {
        Self {
            table,
            cells: table
                .summary
                .counts
                .iter()
                .map(|c| ((c.column.as_str(), c.value.as_deref(), c.state), c.count))
                .collect(),
        }
    }

    fn state(&self, state: LabelState) -> u64 {
        self.table.summary.states.get(&state).copied().unwrap_or(0)
    }

    /// Events the producer assessed, as `observe` counts them.
    fn assessed(&self) -> Option<u64> {
        match self.table.summary.descriptor.kind {
            LabelKind::Gate => Some(self.state(LabelState::Assessed)),
            LabelKind::Classifier => {
                Some(self.state(LabelState::Classified) + self.state(LabelState::Abstained))
            }
            LabelKind::Qc | LabelKind::Cluster => None,
        }
    }

    /// Events of the bound `label`: gate members, or accepted assignments of
    /// a class.
    fn label(&self, label: &str) -> u64 {
        let key = match self.table.summary.descriptor.kind {
            LabelKind::Gate => (label, Some("true"), LabelState::Assessed),
            _ => ("label", Some(label), LabelState::Classified),
        };
        self.cells.get(&key).copied().unwrap_or(0)
    }
}

/// Check every observation whose native output has a label table.
///
/// # Errors
/// Names the first observation whose counts disagree with its label table.
pub(crate) fn check(inputs: &Inputs) -> Result<Vec<ParityEntry>> {
    let tables: HashMap<&str, Counts<'_>> = inputs
        .labels
        .iter()
        .map(|t| {
            (
                t.summary.descriptor.producer.native_sha256.as_str(),
                Counts::of(t),
            )
        })
        .collect();
    let descriptors: HashMap<&str, &Descriptor> = inputs
        .descriptors
        .iter()
        .map(|d| (d.value.data.sha256.as_str(), d))
        .collect();
    let mut entries: Vec<ParityEntry> = Vec::new();
    for observation in inputs.observations.iter().flat_map(|t| &t.rows) {
        let Some(native) = observation.lineage.annotations_sha256.as_deref() else {
            continue;
        };
        let Some(counts) = tables.get(native) else {
            continue;
        };
        let Some(assessed) = counts.assessed() else {
            continue;
        };
        let descriptor = descriptors.get(native).copied();
        check_one(observation, counts, assessed, descriptor)?;
        let annotation_id = &counts.table.summary.annotation_id;
        let level = if descriptor.is_some() {
            ParityLevel::Counts
        } else {
            ParityLevel::AssessedEvents
        };
        match entries
            .iter_mut()
            .find(|e| &e.annotation_id == annotation_id)
        {
            Some(entry) => entry.observations += 1,
            None => entries.push(ParityEntry {
                annotation_id: annotation_id.clone(),
                observations: 1,
                level,
            }),
        }
    }
    entries.sort_by(|a, b| a.annotation_id.cmp(&b.annotation_id));
    Ok(entries)
}

fn check_one(
    observation: &PopulationObservation,
    counts: &Counts<'_>,
    assessed: u64,
    descriptor: Option<&Descriptor>,
) -> Result<()> {
    let disagree = |what: &str, held: String, counted: String| {
        Err(PopulationError::Invalid(format!(
            "observation `{}` ({} of `{}`) holds {what} {held}, but label table `{}` counts \
             {counted}",
            observation.observation_id,
            super::dimensions::name(&observation.measure),
            observation.target.id,
            counts.table.summary.annotation_id
        )))
    };
    if observation.n_events != assessed {
        return disagree(
            "assessed events",
            observation.n_events.to_string(),
            assessed.to_string(),
        );
    }
    let Some(descriptor) = descriptor else {
        return Ok(());
    };
    let bindings = &descriptor.value.context.source.bindings;
    let Some(binding) = bindings.iter().find(|b| b.target == observation.target) else {
        return Ok(());
    };
    let count = counts.label(&binding.label);
    let denominator = binding
        .denominator
        .as_deref()
        .map_or(assessed, |label| counts.label(label));
    let held = observation.denominator.as_ref().map(|d| d.observed_count);
    if held != Some(denominator) {
        return disagree("denominator", format!("{held:?}"), denominator.to_string());
    }
    let evidence = observation.value_u64.or(observation.supporting_count);
    if matches!(observation.measure, Measure::ObservedCount) && evidence != Some(count) {
        return disagree("count", format!("{evidence:?}"), count.to_string());
    }
    if matches!(observation.measure, Measure::Fraction)
        && let Some(fraction) = observation.value_f64
    {
        #[allow(clippy::cast_precision_loss)] // As observe computes it.
        let expected = count as f64 / denominator as f64;
        if fraction.to_bits() != expected.to_bits() {
            return disagree("fraction", fraction.to_string(), expected.to_string());
        }
    }
    Ok(())
}
