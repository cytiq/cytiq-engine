//! What a flag can and cannot be read as. The outlier flag ranks a file
//! within its own test cohort; the novelty flag compares it with a reference
//! cohort. Neither carries a validated false-positive rate: these statuses
//! travel with every run so that an unvalidated flag is never mistaken for a
//! calibrated decision.

use serde::Serialize;

use crate::runner::{DEFAULT_NOVELTY_QUANTILE, DEFAULT_OUTLIER_THRESHOLD};

/// Which cohort a flag is relative to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagScope {
    /// Isolation forest fitted on the test files themselves: a file's score
    /// depends on the other files of the same cohort.
    WithinTestCohort,
    /// Mixture fitted on the reference files; each test file is scored on
    /// its own against it.
    AgainstReference,
}

/// How the flag's cut-off is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdRule {
    /// A fixed score (`threshold_parameter`, 0.5), as upstream.
    FixedScore,
    /// The `threshold_parameter` quantile of the reference files' own
    /// log-densities under the model fitted on them, as upstream.
    InSampleQuantile,
}

/// What the cut-off's false-positive rate is known to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationStatus {
    /// The flag was not computed.
    NotAssessed,
    /// Reference size does not exceed the feature dimension, the nominal 5%
    /// tail holds less than one reference file, or a mixture component has no
    /// more effective files than features.
    InsufficientReference,
    /// Enough reference files for the model, but the threshold is set on the
    /// same files the model was fitted on; held-out normal files exceed it
    /// more often than the nominal rate.
    InSampleOnly,
    /// A fixed score cut-off without any reference: not a rate at all.
    UncalibratedHeuristic,
}

impl CalibrationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotAssessed => "not_assessed",
            Self::InsufficientReference => "insufficient_reference",
            Self::InSampleOnly => "in_sample_only",
            Self::UncalibratedHeuristic => "uncalibrated_heuristic",
        }
    }
}

/// Status of a quantity no run can measure on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Assessment {
    /// Needs independent normal acquisitions, which a run does not have; see
    /// the novelty benchmark in `parity/scientific`.
    NotAssessed,
}

/// Whether the run's seed can change the flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedDependence {
    /// No random number is drawn for this flag: every seed gives the same
    /// values bit for bit.
    SeedInvariant,
    /// The flag depends on random draws (event subsample, aggregate subsample
    /// or isolation trees); one seed is one Monte Carlo draw.
    SeedDependent,
}

/// What `--stability-seeds` measured for a flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StabilityStatus {
    /// No stability run (`stability_seeds` unset).
    NotAssessed,
    /// The flag is seed-invariant; the stability run confirmed identical
    /// values for every seed.
    SeedInvariant,
    /// Every file got the same decision under every seed.
    Stable,
    /// At least one file's decision changed with the seed, or a seed failed.
    Unstable,
}

/// How to read one flag of this run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FlagAssessment {
    pub scope: FlagScope,
    pub threshold_rule: ThresholdRule,
    /// The score cut-off (outlier) or the reference quantile (novelty).
    pub threshold_parameter: f64,
    pub calibration_status: CalibrationStatus,
    /// The false-positive rate on a population of normal files.
    pub population_false_positive_rate: Assessment,
    pub seed_dependence: SeedDependence,
    pub stability_status: StabilityStatus,
}

impl FlagAssessment {
    pub(crate) const fn outlier(seed_dependence: SeedDependence) -> Self {
        Self {
            scope: FlagScope::WithinTestCohort,
            threshold_rule: ThresholdRule::FixedScore,
            threshold_parameter: DEFAULT_OUTLIER_THRESHOLD,
            calibration_status: CalibrationStatus::UncalibratedHeuristic,
            population_false_positive_rate: Assessment::NotAssessed,
            seed_dependence,
            stability_status: StabilityStatus::NotAssessed,
        }
    }

    pub(crate) const fn novelty(
        calibration_status: CalibrationStatus,
        seed_dependence: SeedDependence,
    ) -> Self {
        Self {
            scope: FlagScope::AgainstReference,
            threshold_rule: ThresholdRule::InSampleQuantile,
            threshold_parameter: DEFAULT_NOVELTY_QUANTILE,
            calibration_status,
            population_false_positive_rate: Assessment::NotAssessed,
            seed_dependence,
            stability_status: StabilityStatus::NotAssessed,
        }
    }
}

/// The outlier flag draws random numbers only when it builds trees.
pub(crate) const fn outlier_seed_dependence(test_files: usize, features: usize) -> SeedDependence {
    if crate::outlier::uses_trees(test_files, features) {
        SeedDependence::SeedDependent
    } else {
        SeedDependence::SeedInvariant
    }
}

/// The novelty flag draws random numbers only to subsample: a file longer
/// than `events_per_file`, or, for an aggregate method, an aggregate file
/// longer than its share `ceil(aggregate_events / files)`.
pub(crate) fn novelty_seed_dependence(
    event_counts: impl IntoIterator<Item = usize>,
    events_per_file: usize,
    aggregate_counts: Option<&[usize]>,
    aggregate_events: usize,
) -> SeedDependence {
    let subsampled = event_counts
        .into_iter()
        .any(|count| count > events_per_file);
    let aggregate_subsampled = aggregate_counts.is_some_and(|counts| {
        !counts.is_empty() && {
            let share = aggregate_events.div_ceil(counts.len());
            counts.iter().any(|&count| count > share)
        }
    });
    if subsampled || aggregate_subsampled {
        SeedDependence::SeedDependent
    } else {
        SeedDependence::SeedInvariant
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn novelty_is_seed_invariant_only_when_nothing_is_subsampled() {
        let invariant = SeedDependence::SeedInvariant;
        let dependent = SeedDependence::SeedDependent;
        assert_eq!(
            novelty_seed_dependence([1000, 999], 1000, None, 0),
            invariant
        );
        assert_eq!(
            novelty_seed_dependence([1001, 10], 1000, None, 0),
            dependent
        );
        // ceil(10000 / 3) = 3334 per aggregate file
        assert_eq!(
            novelty_seed_dependence([800], 1000, Some(&[3334, 3334, 3334]), 10_000),
            invariant
        );
        assert_eq!(
            novelty_seed_dependence([800], 1000, Some(&[3335, 10, 10]), 10_000),
            dependent
        );
        assert_eq!(
            novelty_seed_dependence([800], 1000, Some(&[]), 10_000),
            invariant
        );
    }

    #[test]
    fn outlier_is_seed_dependent_when_trees_are_built() {
        assert_eq!(outlier_seed_dependence(1, 4), SeedDependence::SeedInvariant);
        assert_eq!(outlier_seed_dependence(3, 0), SeedDependence::SeedInvariant);
        assert_eq!(outlier_seed_dependence(2, 1), SeedDependence::SeedDependent);
    }

    #[test]
    fn statuses_serialize_in_snake_case() {
        let assessment = FlagAssessment::outlier(SeedDependence::SeedDependent);
        let json = serde_json::to_value(&assessment).expect("json");
        assert_eq!(json["scope"], "within_test_cohort");
        assert_eq!(json["threshold_rule"], "fixed_score");
        assert_eq!(json["threshold_parameter"], 0.5);
        assert_eq!(json["calibration_status"], "uncalibrated_heuristic");
        assert_eq!(json["population_false_positive_rate"], "not_assessed");
        assert_eq!(json["seed_dependence"], "seed_dependent");
        assert_eq!(json["stability_status"], "not_assessed");
        for status in [
            CalibrationStatus::NotAssessed,
            CalibrationStatus::InsufficientReference,
            CalibrationStatus::InSampleOnly,
            CalibrationStatus::UncalibratedHeuristic,
        ] {
            assert_eq!(serde_json::to_value(status).expect("json"), status.as_str());
        }
    }
}
