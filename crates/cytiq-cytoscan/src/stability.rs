//! Seed stability of the flags (`--stability-seeds N`): the run is repeated
//! with `N - 1` derived seeds on the same loaded files, and every test file's
//! decisions and scores are compared across the `N` seeds. Only the
//! algorithm seed varies (per-file event subsample, aggregate subsample,
//! isolation trees); files, reference membership and channels are fixed.
//! Seed `j` reproduces a separate run with `--seed stability_seed(seed, j)`
//! exactly.

#![allow(clippy::cast_precision_loss, clippy::module_name_repetitions)]

use std::path::PathBuf;

use serde::Serialize;

use crate::assessment::{SeedDependence, StabilityStatus};
use crate::error::CytoScanError;
use crate::runner::{
    CytoScanConfig, DEFAULT_ISOLATION_TREES, DEFAULT_OUTLIER_THRESHOLD, LoadedFile, SeedAnalysis,
    analyse_features, fit_novelty,
};

/// Upper bound of `--stability-seeds`: each seed repeats the whole analysis.
pub const MAX_STABILITY_SEEDS: usize = 64;

/// Distance between consecutive stability seeds. Tree `t` of seed `s` uses
/// generator seed `s + t`, so seeds closer than the tree count would share
/// trees (42 and 43 share 999 of 1000); `2^32` keeps every seed's generator
/// seeds disjoint from every other's (see [`stability_seed`]).
pub const STABILITY_SEED_STRIDE: u64 = 1 << 32;

const _: () = assert!((DEFAULT_ISOLATION_TREES as u64) < STABILITY_SEED_STRIDE);
const _: () = assert!((MAX_STABILITY_SEEDS as u64) < (1 << 32));

/// Seed `index` of a stability run: `primary + index * 2^32` (wrapping);
/// index 0 is the primary seed itself.
///
/// Seed `j` draws from generator seeds `s_j + t` for `t < 1000` (trees; the
/// subsamples use `s_j`). For `i < j < 64`, `(j - i) * 2^32` lies in
/// `[2^32, 2^64 - 2^32]` without wrapping, while a difference of two tree
/// offsets lies in `[0, 1000) ∪ (2^64 - 1000, 2^64)` modulo `2^64`; the
/// ranges are disjoint because `1000 < 2^32`, so no two seeds of one
/// stability run share a generator seed.
///
/// # Panics
///
/// When `index >= MAX_STABILITY_SEEDS`.
#[must_use]
pub fn stability_seed(primary: u64, index: usize) -> u64 {
    assert!(index < MAX_STABILITY_SEEDS, "stability seed index {index}");
    primary.wrapping_add(index as u64 * STABILITY_SEED_STRIDE)
}

/// What one seed of a stability run produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedStatus {
    Ok,
    /// The features could not be computed (an empty fingerprint bin or
    /// aggregate): neither flag was assessed at this seed.
    FeatureFailed,
    /// No mixture could be fitted: the outlier flag was assessed, the
    /// novelty flag was not.
    NoveltyFitFailed,
}

impl SeedStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::FeatureFailed => "feature_failed",
            Self::NoveltyFitFailed => "novelty_fit_failed",
        }
    }
}

/// A test file's decision over all seeds of a stability run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDecision {
    /// Flagged under every seed.
    StableFlag,
    /// Flagged under no seed.
    StableNoFlag,
    /// Flagged under some seeds only, or not assessed under some seed.
    Unstable,
    /// The flag is not computed by this run.
    NotAssessed,
}

impl FileDecision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StableFlag => "stable_flag",
            Self::StableNoFlag => "stable_no_flag",
            Self::Unstable => "unstable",
            Self::NotAssessed => "not_assessed",
        }
    }
}

/// One test file at one seed (`cytoscan_stability_seeds.csv`). Values use
/// the summary's rules, so the `seed_index` 0 rows repeat
/// `cytoscan_summary.csv`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanStabilitySeedRow {
    pub seed_index: usize,
    pub seed: u64,
    pub status: SeedStatus,
    pub filename: String,
    pub path: PathBuf,
    pub is_outlier: Option<bool>,
    pub outlier_score: Option<f64>,
    pub is_novelty: Option<bool>,
    pub novelty_log_likelihood: Option<f64>,
    pub novelty_threshold: Option<f64>,
}

/// One test file over every seed (`cytoscan_stability.csv`). The novelty
/// margin is `log-likelihood - threshold` at each seed; the file is flagged
/// where it is `<= 0`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanStabilityFileRow {
    pub filename: String,
    pub path: PathBuf,
    pub seeds: usize,
    pub outlier_seeds_assessed: usize,
    pub outlier_flag_count: usize,
    pub outlier_score_min: Option<f64>,
    pub outlier_score_median: Option<f64>,
    pub outlier_score_max: Option<f64>,
    pub outlier_decision: FileDecision,
    pub novelty_seeds_assessed: usize,
    pub novelty_flag_count: usize,
    pub novelty_margin_min: Option<f64>,
    pub novelty_margin_median: Option<f64>,
    pub novelty_margin_max: Option<f64>,
    pub novelty_decision: FileDecision,
}

/// A seed whose analysis failed; kept as an observation, never dropped.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FailedSeed {
    pub seed_index: usize,
    pub seed: u64,
    pub status: SeedStatus,
    pub reason: String,
}

/// Agreement of one flag across the seeds.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FlagStability {
    pub status: StabilityStatus,
    /// Seeds at which this flag was computed.
    pub seeds_assessed: usize,
    /// Share of test files with a stable decision.
    pub unanimous_fraction: f64,
    pub unstable_files: usize,
    /// Probability that two different assessed seeds disagree on a test
    /// file, averaged over files: `sum f_i (S - f_i) / (n S (S - 1) / 2)`
    /// with `f_i` flags in `S` assessed seeds. `None` below two seeds.
    pub pairwise_disagreement: Option<f64>,
    /// Largest per-file range of the outlier score, or of the novelty margin.
    pub max_range: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_max: Option<f64>,
}

/// `diagnostics.stability`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StabilitySummary {
    pub seeds: Vec<u64>,
    pub seed_rule: String,
    pub scope: String,
    pub failed_seeds: Vec<FailedSeed>,
    pub outlier: Option<FlagStability>,
    pub novelty: Option<FlagStability>,
}

/// Per-file and per-seed rows of a stability run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanStability {
    pub files: Vec<CytoScanStabilityFileRow>,
    pub seed_rows: Vec<CytoScanStabilitySeedRow>,
}

/// What one seed decided.
#[derive(Clone, Debug)]
struct SeedOutcome {
    status: SeedStatus,
    reason: Option<String>,
    outlier_scores: Option<Vec<f64>>,
    /// Test log-likelihoods and the threshold.
    novelty: Option<(Vec<f64>, f64)>,
}

impl SeedOutcome {
    fn from_analysis(analysis: &SeedAnalysis) -> Self {
        Self {
            status: SeedStatus::Ok,
            reason: None,
            outlier_scores: analysis.outlier_scores.clone(),
            novelty: analysis
                .novelty
                .as_ref()
                .map(|(model, values)| (values.clone(), model.threshold)),
        }
    }
}

/// Analyse the loaded files at every derived seed and compare them with the
/// primary analysis. Seeds run one after another: the isolation forest is
/// already parallel over its trees, and only one seed's features are held at
/// a time.
pub(crate) fn run(
    config: &CytoScanConfig,
    test_files: &[LoadedFile],
    reference_files: &[LoadedFile],
    primary: &SeedAnalysis,
    seeds: usize,
    dependence: (SeedDependence, SeedDependence),
) -> Result<(StabilitySummary, CytoScanStability), CytoScanError> {
    assert!((2..=MAX_STABILITY_SEEDS).contains(&seeds), "{seeds} seeds");
    let seed_values = (0..seeds)
        .map(|index| stability_seed(config.random_seed, index))
        .collect::<Vec<_>>();
    let mut outcomes = Vec::with_capacity(seeds);
    outcomes.push(SeedOutcome::from_analysis(primary));
    for &seed in &seed_values[1..] {
        outcomes.push(evaluate(config, test_files, reference_files, seed)?);
    }
    let (outlier_dependence, novelty_dependence) = dependence;
    if config.flag_method.includes_outlier() && outlier_dependence == SeedDependence::SeedInvariant
    {
        assert_identical(&outcomes, |outcome| {
            outcome.outlier_scores.as_ref().map(|scores| bits(scores))
        });
    }
    if config.flag_method.includes_novelty() && novelty_dependence == SeedDependence::SeedInvariant
    {
        assert_identical(&outcomes, |outcome| {
            outcome.novelty.as_ref().map(|(values, threshold)| {
                let mut all = bits(values);
                all.push(threshold.to_bits());
                all
            })
        });
    }
    let files = primary
        .test_features
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| file_row(index, &row.filename, &row.path, &outcomes))
        .collect::<Vec<_>>();
    let seed_rows = seed_rows(primary, &seed_values, &outcomes);
    let failed_seeds = outcomes
        .iter()
        .zip(&seed_values)
        .enumerate()
        .filter(|(_, (outcome, _))| outcome.status != SeedStatus::Ok)
        .map(|(seed_index, (outcome, &seed))| FailedSeed {
            seed_index,
            seed,
            status: outcome.status,
            reason: outcome.reason.clone().unwrap_or_default(),
        })
        .collect();
    let summary = StabilitySummary {
        seeds: seed_values,
        seed_rule: "seed + index * 2^32 (wrapping); index 0 is --seed".to_owned(),
        scope: "algorithm seed only (per-file event subsample, aggregate subsample, isolation trees); files, reference membership and channels fixed".to_owned(),
        failed_seeds,
        outlier: config.flag_method.includes_outlier().then(|| {
            flag_stability(
                &files,
                outcomes.iter().filter(|o| o.outlier_scores.is_some()).count(),
                outlier_dependence,
                |row| (row.outlier_decision, row.outlier_flag_count, row.outlier_seeds_assessed),
                |row| range(row.outlier_score_min, row.outlier_score_max),
                None,
            )
        }),
        novelty: config.flag_method.includes_novelty().then(|| {
            let thresholds = outcomes
                .iter()
                .filter_map(|o| o.novelty.as_ref().map(|(_, threshold)| *threshold))
                .collect::<Vec<_>>();
            flag_stability(
                &files,
                thresholds.len(),
                novelty_dependence,
                |row| (row.novelty_decision, row.novelty_flag_count, row.novelty_seeds_assessed),
                |row| range(row.novelty_margin_min, row.novelty_margin_max),
                Some(&thresholds),
            )
        }),
    };
    Ok((summary, CytoScanStability { files, seed_rows }))
}

/// Analyse one derived seed. Failures that a different subsample can cause
/// are observations of that seed; anything else fails the run.
fn evaluate(
    config: &CytoScanConfig,
    test_files: &[LoadedFile],
    reference_files: &[LoadedFile],
    seed: u64,
) -> Result<SeedOutcome, CytoScanError> {
    let analysis = match analyse_features(config, test_files, reference_files, seed) {
        Ok(analysis) => analysis,
        Err(
            error @ (CytoScanError::FingerprintBinTooSmall { .. }
            | CytoScanError::EmptyAggregate { .. }),
        ) => {
            return Ok(SeedOutcome {
                status: SeedStatus::FeatureFailed,
                reason: Some(error.to_string()),
                outlier_scores: None,
                novelty: None,
            });
        }
        Err(error) => return Err(error),
    };
    let (status, reason, novelty) = match fit_novelty(config, &analysis) {
        Ok(novelty) => (
            SeedStatus::Ok,
            None,
            novelty.map(|(model, values)| (values, model.threshold)),
        ),
        Err(error @ CytoScanError::NoveltyFit { .. }) => {
            (SeedStatus::NoveltyFitFailed, Some(error.to_string()), None)
        }
        Err(error) => return Err(error),
    };
    Ok(SeedOutcome {
        status,
        reason,
        outlier_scores: analysis.outlier_scores,
        novelty,
    })
}

fn bits(values: &[f64]) -> Vec<u64> {
    values.iter().map(|value| value.to_bits()).collect()
}

/// A seed-invariant flag must come out bit-identical at every seed: the
/// seed-dependence rule claims that no random number was drawn.
fn assert_identical(outcomes: &[SeedOutcome], key: impl Fn(&SeedOutcome) -> Option<Vec<u64>>) {
    let first = key(&outcomes[0]);
    assert!(first.is_some(), "the primary seed assessed the flag");
    for (index, outcome) in outcomes.iter().enumerate().skip(1) {
        assert_eq!(
            outcome.status,
            SeedStatus::Ok,
            "seed {index} of a seed-invariant flag failed"
        );
        assert!(
            key(outcome) == first,
            "seed {index} changed a seed-invariant flag"
        );
    }
}

/// `log-likelihood - threshold`; equal values, including two infinities of
/// the same sign, give 0, as the `<=` flag rule treats them.
#[allow(clippy::float_cmp)]
fn margin(log_likelihood: f64, threshold: f64) -> f64 {
    if log_likelihood == threshold {
        0.0
    } else {
        log_likelihood - threshold
    }
}

/// Minimum, median (midpoint of the two middle values for an even count) and
/// maximum, ordered by `f64::total_cmp`.
fn min_median_max(mut values: Vec<f64>) -> (Option<f64>, Option<f64>, Option<f64>) {
    if values.is_empty() {
        return (None, None, None);
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let median = if values.len().is_multiple_of(2) {
        f64::midpoint(values[middle - 1], values[middle])
    } else {
        values[middle]
    };
    (Some(values[0]), Some(median), values.last().copied())
}

#[allow(clippy::float_cmp)]
fn range(minimum: Option<f64>, maximum: Option<f64>) -> Option<f64> {
    let (minimum, maximum) = (minimum?, maximum?);
    Some(if minimum == maximum {
        0.0
    } else {
        maximum - minimum
    })
}

fn decision(enabled: bool, seeds: usize, assessed: usize, flagged: usize) -> FileDecision {
    if !enabled {
        FileDecision::NotAssessed
    } else if assessed < seeds || (flagged > 0 && flagged < assessed) {
        FileDecision::Unstable
    } else if flagged == assessed {
        FileDecision::StableFlag
    } else {
        FileDecision::StableNoFlag
    }
}

fn file_row(
    index: usize,
    filename: &str,
    path: &std::path::Path,
    outcomes: &[SeedOutcome],
) -> CytoScanStabilityFileRow {
    let seeds = outcomes.len();
    let scores = outcomes
        .iter()
        .filter_map(|outcome| outcome.outlier_scores.as_ref().map(|scores| scores[index]))
        .collect::<Vec<_>>();
    let novelty = outcomes
        .iter()
        .filter_map(|outcome| {
            outcome
                .novelty
                .as_ref()
                .map(|(values, threshold)| (values[index], *threshold))
        })
        .collect::<Vec<_>>();
    let outlier_enabled = outcomes[0].outlier_scores.is_some();
    let novelty_enabled = outcomes[0].novelty.is_some();
    let outlier_flag_count = scores
        .iter()
        .filter(|&&score| score >= DEFAULT_OUTLIER_THRESHOLD)
        .count();
    let novelty_flag_count = novelty
        .iter()
        .filter(|(value, threshold)| value <= threshold)
        .count();
    let (outlier_score_min, outlier_score_median, outlier_score_max) =
        min_median_max(scores.clone());
    let (novelty_margin_min, novelty_margin_median, novelty_margin_max) = min_median_max(
        novelty
            .iter()
            .map(|&(value, threshold)| margin(value, threshold))
            .collect(),
    );
    CytoScanStabilityFileRow {
        filename: filename.to_owned(),
        path: path.to_path_buf(),
        seeds,
        outlier_seeds_assessed: scores.len(),
        outlier_flag_count,
        outlier_score_min,
        outlier_score_median,
        outlier_score_max,
        outlier_decision: decision(outlier_enabled, seeds, scores.len(), outlier_flag_count),
        novelty_seeds_assessed: novelty.len(),
        novelty_flag_count,
        novelty_margin_min,
        novelty_margin_median,
        novelty_margin_max,
        novelty_decision: decision(novelty_enabled, seeds, novelty.len(), novelty_flag_count),
    }
}

fn seed_rows(
    primary: &SeedAnalysis,
    seed_values: &[u64],
    outcomes: &[SeedOutcome],
) -> Vec<CytoScanStabilitySeedRow> {
    let mut rows = Vec::with_capacity(seed_values.len() * primary.test_features.rows.len());
    for (seed_index, (outcome, &seed)) in outcomes.iter().zip(seed_values).enumerate() {
        for (index, file) in primary.test_features.rows.iter().enumerate() {
            let outlier_score = outcome.outlier_scores.as_ref().map(|scores| scores[index]);
            let novelty = outcome
                .novelty
                .as_ref()
                .map(|(values, threshold)| (values[index], *threshold));
            rows.push(CytoScanStabilitySeedRow {
                seed_index,
                seed,
                status: outcome.status,
                filename: file.filename.clone(),
                path: file.path.clone(),
                is_outlier: outlier_score.map(|score| score >= DEFAULT_OUTLIER_THRESHOLD),
                outlier_score,
                is_novelty: novelty.map(|(value, threshold)| value <= threshold),
                novelty_log_likelihood: novelty.map(|(value, _)| value),
                novelty_threshold: novelty.map(|(_, threshold)| threshold),
            });
        }
    }
    rows
}

fn flag_stability(
    files: &[CytoScanStabilityFileRow],
    seeds_assessed: usize,
    dependence: SeedDependence,
    decision_of: impl Fn(&CytoScanStabilityFileRow) -> (FileDecision, usize, usize),
    range_of: impl Fn(&CytoScanStabilityFileRow) -> Option<f64>,
    thresholds: Option<&[f64]>,
) -> FlagStability {
    let decisions = files.iter().map(&decision_of).collect::<Vec<_>>();
    let unstable_files = decisions
        .iter()
        .filter(|(decision, _, _)| *decision == FileDecision::Unstable)
        .count();
    let stable_files = decisions
        .iter()
        .filter(|(decision, _, _)| {
            matches!(
                decision,
                FileDecision::StableFlag | FileDecision::StableNoFlag
            )
        })
        .count();
    // A file with a failed seed is `Unstable` already (`decision`).
    let status = if dependence == SeedDependence::SeedInvariant {
        StabilityStatus::SeedInvariant
    } else if unstable_files > 0 {
        StabilityStatus::Unstable
    } else {
        StabilityStatus::Stable
    };
    let (threshold_min, _, threshold_max) =
        thresholds.map_or((None, None, None), |values| min_median_max(values.to_vec()));
    FlagStability {
        status,
        seeds_assessed,
        unanimous_fraction: if files.is_empty() {
            1.0
        } else {
            stable_files as f64 / files.len() as f64
        },
        unstable_files,
        pairwise_disagreement: pairwise_disagreement(
            &decisions
                .iter()
                .map(|&(_, flagged, assessed)| (flagged, assessed))
                .collect::<Vec<_>>(),
        ),
        max_range: files.iter().filter_map(&range_of).max_by(f64::total_cmp),
        threshold_min,
        threshold_max,
    }
}

/// `sum f_i (S_i - f_i) / sum S_i (S_i - 1) / 2`: the share of disagreeing
/// seed pairs over all files, `f_i` flags in `S_i` assessed seeds. With the
/// same `S` for every file this is the mean per-file disagreement.
fn pairwise_disagreement(counts: &[(usize, usize)]) -> Option<f64> {
    let pairs = counts
        .iter()
        .map(|&(_, assessed)| assessed * assessed.saturating_sub(1) / 2)
        .sum::<usize>();
    if pairs == 0 {
        return None;
    }
    let disagreeing = counts
        .iter()
        .map(|&(flagged, assessed)| flagged * (assessed - flagged))
        .sum::<usize>();
    Some(disagreeing as f64 / pairs as f64)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn index_zero_is_the_primary_seed() {
        for primary in [0, 42, u64::MAX] {
            assert_eq!(stability_seed(primary, 0), primary);
        }
        assert_eq!(stability_seed(42, 1), 42 + (1 << 32));
        assert_eq!(stability_seed(u64::MAX, 1), (1 << 32) - 1);
    }

    #[test]
    fn stability_seeds_never_share_a_generator_seed() {
        // Every generator seed of every tree of every stability seed.
        for primary in [
            0,
            42,
            1 << 63,
            u64::MAX,
            u64::MAX - 999,
            0_u64.wrapping_sub(1 << 32),
        ] {
            let mut generator_seeds = HashSet::new();
            for index in 0..MAX_STABILITY_SEEDS {
                let first = stability_seed(primary, index);
                for tree in 0..DEFAULT_ISOLATION_TREES as u64 {
                    assert!(
                        generator_seeds.insert(first.wrapping_add(tree)),
                        "{primary} {index} {tree}"
                    );
                }
            }
            assert_eq!(
                generator_seeds.len(),
                MAX_STABILITY_SEEDS * DEFAULT_ISOLATION_TREES
            );
        }
    }

    #[test]
    #[should_panic(expected = "stability seed index 64")]
    fn seed_index_is_bounded() {
        let _ = stability_seed(42, MAX_STABILITY_SEEDS);
    }

    fn outcome(scores: &[f64], novelty: Option<(&[f64], f64)>) -> SeedOutcome {
        SeedOutcome {
            status: SeedStatus::Ok,
            reason: None,
            outlier_scores: Some(scores.to_vec()),
            novelty: novelty.map(|(values, threshold)| (values.to_vec(), threshold)),
        }
    }

    #[test]
    fn flags_use_the_summary_boundaries() {
        // score == 0.5 is an outlier; log-likelihood == threshold a novelty.
        let outcomes = [
            outcome(&[0.5], Some((&[-3.0], -3.0))),
            outcome(&[0.499_999], Some((&[-2.999], -3.0))),
        ];
        let row = file_row(0, "a.fcs", std::path::Path::new("a.fcs"), &outcomes);
        assert_eq!(row.outlier_flag_count, 1);
        assert_eq!(row.novelty_flag_count, 1);
        assert_eq!(row.outlier_decision, FileDecision::Unstable);
        assert_eq!(row.novelty_decision, FileDecision::Unstable);
        assert_eq!(row.novelty_margin_min, Some(0.0));
    }

    #[test]
    fn medians_and_infinite_margins() {
        assert_eq!(
            min_median_max(vec![3.0, 1.0, 2.0]),
            (Some(1.0), Some(2.0), Some(3.0))
        );
        assert_eq!(
            min_median_max(vec![4.0, 1.0, 2.0, 3.0]),
            (Some(1.0), Some(2.5), Some(4.0))
        );
        assert_eq!(min_median_max(Vec::new()), (None, None, None));
        assert_eq!(margin(f64::NEG_INFINITY, f64::NEG_INFINITY), 0.0);
        assert_eq!(margin(f64::NEG_INFINITY, -5.0), f64::NEG_INFINITY);
        assert_eq!(
            range(Some(f64::NEG_INFINITY), Some(1.0)),
            Some(f64::INFINITY)
        );
        assert_eq!(
            range(Some(f64::NEG_INFINITY), Some(f64::NEG_INFINITY)),
            Some(0.0)
        );
        let outcomes = [
            outcome(&[0.1], Some((&[f64::NEG_INFINITY], -3.0))),
            outcome(&[0.1], Some((&[-1.0], -3.0))),
        ];
        let row = file_row(0, "a.fcs", std::path::Path::new("a.fcs"), &outcomes);
        assert_eq!(row.novelty_margin_min, Some(f64::NEG_INFINITY));
        assert_eq!(row.novelty_margin_max, Some(2.0));
        assert_eq!(row.outlier_decision, FileDecision::StableNoFlag);
    }

    #[test]
    fn failed_seeds_make_the_flag_unstable() {
        let failed = SeedOutcome {
            status: SeedStatus::NoveltyFitFailed,
            reason: Some("singular".to_owned()),
            outlier_scores: Some(vec![0.6]),
            novelty: None,
        };
        let outcomes = [outcome(&[0.7], Some((&[-9.0], -3.0))), failed];
        let row = file_row(0, "a.fcs", std::path::Path::new("a.fcs"), &outcomes);
        assert_eq!(row.outlier_decision, FileDecision::StableFlag);
        assert_eq!(row.novelty_seeds_assessed, 1);
        assert_eq!(row.novelty_decision, FileDecision::Unstable);
        let stability = flag_stability(
            &[row],
            1,
            SeedDependence::SeedDependent,
            |row| {
                (
                    row.novelty_decision,
                    row.novelty_flag_count,
                    row.novelty_seeds_assessed,
                )
            },
            |row| range(row.novelty_margin_min, row.novelty_margin_max),
            Some(&[-3.0]),
        );
        assert_eq!(stability.status, StabilityStatus::Unstable);
        assert_eq!(stability.pairwise_disagreement, None);
    }

    /// Three files over three seeds: always flagged, never flagged, and
    /// flagged at one seed only.
    fn three_files() -> Vec<CytoScanStabilityFileRow> {
        let outcomes = [
            outcome(&[0.9, 0.1, 0.6], Some((&[-9.0, 1.0, -4.0], -3.0))),
            outcome(&[0.8, 0.2, 0.4], Some((&[-8.0, 2.0, 0.0], -3.0))),
            outcome(&[0.7, 0.3, 0.4], Some((&[-7.0, 3.0, 1.0], -3.0))),
        ];
        (0..3)
            .map(|index| file_row(index, "f.fcs", std::path::Path::new("f.fcs"), &outcomes))
            .collect()
    }

    fn novelty_of(row: &CytoScanStabilityFileRow) -> (FileDecision, usize, usize) {
        (
            row.novelty_decision,
            row.novelty_flag_count,
            row.novelty_seeds_assessed,
        )
    }

    fn novelty_range(row: &CytoScanStabilityFileRow) -> Option<f64> {
        range(row.novelty_margin_min, row.novelty_margin_max)
    }

    #[test]
    fn file_rows_count_flags_and_ranges() {
        let rows = three_files();
        let counts = rows
            .iter()
            .map(|row| (row.novelty_flag_count, row.outlier_flag_count))
            .collect::<Vec<_>>();
        assert_eq!(counts, [(3, 3), (0, 0), (1, 1)]);
        assert_eq!(rows[0].novelty_decision, FileDecision::StableFlag);
        assert_eq!(rows[1].novelty_decision, FileDecision::StableNoFlag);
        assert_eq!(rows[2].novelty_decision, FileDecision::Unstable);
        // margins of the last file: -1, 3, 4
        assert_eq!(rows[2].novelty_margin_min, Some(-1.0));
        assert_eq!(rows[2].novelty_margin_median, Some(3.0));
        assert_eq!(rows[2].novelty_margin_max, Some(4.0));
    }

    #[test]
    fn flag_stability_summarises_the_files() {
        let rows = three_files();
        let stability = flag_stability(
            &rows,
            3,
            SeedDependence::SeedDependent,
            novelty_of,
            novelty_range,
            Some(&[-3.0, -2.0, -5.0]),
        );
        assert_eq!(stability.status, StabilityStatus::Unstable);
        assert_eq!(stability.seeds_assessed, 3);
        assert_eq!(stability.unstable_files, 1);
        assert!((stability.unanimous_fraction - 2.0 / 3.0).abs() < 1e-15);
        // flags 3, 0, 1 of 3: 0 + 0 + 2 disagreeing of 9 pairs
        assert_eq!(stability.pairwise_disagreement, Some(2.0 / 9.0));
        // margin ranges 2, 2 and 5
        assert_eq!(stability.max_range, Some(5.0));
        assert_eq!(stability.threshold_min, Some(-5.0));
        assert_eq!(stability.threshold_max, Some(-2.0));

        let stable = flag_stability(
            &rows[..2],
            3,
            SeedDependence::SeedDependent,
            novelty_of,
            novelty_range,
            None,
        );
        assert_eq!(stable.status, StabilityStatus::Stable);
        assert_eq!(stable.unstable_files, 0);
        assert!((stable.unanimous_fraction - 1.0).abs() < 1e-15);
        assert_eq!(stable.threshold_min, None);

        let invariant = flag_stability(
            &rows,
            3,
            SeedDependence::SeedInvariant,
            novelty_of,
            novelty_range,
            None,
        );
        assert_eq!(invariant.status, StabilityStatus::SeedInvariant);
        let empty = flag_stability(
            &[],
            3,
            SeedDependence::SeedDependent,
            novelty_of,
            novelty_range,
            None,
        );
        assert!((empty.unanimous_fraction - 1.0).abs() < 1e-15);
        assert_eq!(empty.pairwise_disagreement, None);
    }

    #[test]
    fn decisions_need_every_seed() {
        assert_eq!(decision(false, 5, 0, 0), FileDecision::NotAssessed);
        assert_eq!(decision(true, 5, 5, 5), FileDecision::StableFlag);
        assert_eq!(decision(true, 5, 5, 0), FileDecision::StableNoFlag);
        assert_eq!(decision(true, 5, 5, 2), FileDecision::Unstable);
        assert_eq!(decision(true, 5, 4, 0), FileDecision::Unstable);
    }

    #[test]
    fn pairwise_disagreement_counts_disagreeing_seed_pairs() {
        // 5 seeds: 2 of 5 flagged -> 2 * 3 = 6 of 10 pairs disagree; 0 and 5
        // flags disagree on none.
        assert_eq!(
            pairwise_disagreement(&[(2, 5), (0, 5), (5, 5), (1, 5)]),
            Some((6.0 + 0.0 + 0.0 + 4.0) / 40.0)
        );
        assert_eq!(pairwise_disagreement(&[(0, 1)]), None);
        assert_eq!(pairwise_disagreement(&[]), None);
    }
}
