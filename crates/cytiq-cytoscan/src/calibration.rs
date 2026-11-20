//! `--novelty-mode conformal-ledoit-wolf-v1`: the novelty decision that the
//! pre-registered benchmark (`parity/scientific/novelty_benchmark.py`, rule
//! v2, confirmation profile) made eligible. Not upstream `CytoScan`.
//!
//! The reference features are modelled by one Gaussian with Ledoit–Wolf
//! shrunk covariance (as `sklearn.covariance.LedoitWolf`). The threshold is
//! cross-conformal: the references are split into `K = min(n, 10)` folds by
//! index (`i mod K`, in resolved reference order); each fold's model, fitted
//! without it, scores its held-out references and every test file, and
//! `c = sum_k #{i in F_k : ll_k(i) <= ll_k(t)}`. The p-value is
//! `(1 + c) / (n + 1)` and a test file is novel when `p <= 1/20`, decided in
//! integers as `20 (1 + c) <= n + 1`. Fewer than 19 references can never
//! reach 1/20, so the mode abstains there; a fold whose covariance is not
//! positive definite makes it abstain too. Under exchangeable references the
//! guarantee is marginal (over reference cohorts), not per cohort.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::module_name_repetitions
)]

use nalgebra::{DMatrix, DVector};
use serde::Serialize;

/// Nominal novelty rate as an exact fraction, `DEFAULT_NOVELTY_QUANTILE`.
pub const CONFORMAL_ALPHA: (u64, u64) = (1, 20);
/// Largest number of cross-conformal folds.
pub const CONFORMAL_MAX_FOLDS: usize = 10;
/// Fewest references at which `(1 + 0) / (n + 1) <= 1/20`.
pub const MIN_CONFORMAL_REFERENCE: usize = 19;

// A count of 0 reaches the nominal rate at 19 references and not at 18.
const _: () = assert!(conformal_novel(0, MIN_CONFORMAL_REFERENCE));
const _: () = assert!(!conformal_novel(0, MIN_CONFORMAL_REFERENCE - 1));

/// How the novelty flag is decided.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoveltyMode {
    /// Upstream `CytoScan`: mclust mixture, in-sample 5% quantile threshold.
    #[default]
    Upstream,
    /// Ledoit–Wolf Gaussian with a cross-conformal p-value at 1/20; summary
    /// and quantiles features only, abstains below 19 references.
    ConformalLedoitWolfV1,
}

impl NoveltyMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Upstream => "upstream",
            Self::ConformalLedoitWolfV1 => "conformal-ledoit-wolf-v1",
        }
    }

    #[must_use]
    pub const fn is_upstream(&self) -> bool {
        matches!(self, Self::Upstream)
    }
}

/// Outcome of the conformal decision for a whole run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformalStatus {
    Decided,
    /// Fewer than `MIN_CONFORMAL_REFERENCE` references.
    AbstainedInsufficientReference,
    /// A fold's (or the full) shrunk covariance is not positive definite.
    AbstainedFitFailed,
}

/// A Gaussian with Ledoit–Wolf shrunk covariance, as scikit-learn's
/// `LedoitWolf().fit(X)` (`assume_centered=False`): the data are centred on
/// their mean, the shrinkage is `min(beta, delta) / delta` with
/// `beta = (sum_i (sum_j x_ij^2)^2 / n - delta_) / (d n)`,
/// `delta_ = sum_jk (sum_i x_ij x_ik)^2 / n^2` and
/// `delta = (delta_ - 2 mu sum_j s_jj + d mu^2) / d`, `mu = tr(S) / d`, and
/// the covariance is `(1 - shrinkage) S + shrinkage mu I` with `S = X'X / n`.
/// One feature takes `S` unshrunk.
#[derive(Clone, Debug)]
pub(crate) struct LedoitWolf {
    mean: DVector<f64>,
    cholesky: DMatrix<f64>,
    log_normaliser: f64,
    pub(crate) shrinkage: f64,
}

impl LedoitWolf {
    /// `None` when the shrunk covariance is not positive definite (fewer than
    /// two files, or no variance at all).
    pub(crate) fn fit(rows: &[&[f64]]) -> Option<Self> {
        let n = rows.len();
        let d = rows.first().map_or(0, |row| row.len());
        if n < 2 || d == 0 {
            return None;
        }
        assert!(rows.iter().all(|row| row.len() == d), "ragged features");
        assert!(
            rows.iter()
                .flat_map(|row| row.iter())
                .all(|v| v.is_finite()),
            "non-finite feature"
        );
        let data = DMatrix::from_fn(n, d, |i, j| rows[i][j]);
        let mean = data.row_mean().transpose();
        let centred = DMatrix::from_fn(n, d, |i, j| data[(i, j)] - mean[j]);
        let samples = n as f64;
        let features = d as f64;
        let empirical = centred.transpose() * &centred / samples;
        let shrinkage = if d == 1 {
            0.0
        } else {
            let squared = centred.map(|value| value * value);
            let trace_terms = squared.row_sum() / samples;
            let mu = trace_terms.sum() / features;
            let beta_sum = (squared.transpose() * &squared).sum();
            let cross = centred.transpose() * &centred;
            let delta_sum = cross.map(|value| value * value).sum() / (samples * samples);
            let beta = (beta_sum / samples - delta_sum) / (features * samples);
            let delta = (delta_sum - 2.0 * mu * trace_terms.sum() + features * mu * mu) / features;
            let beta = beta.min(delta);
            // sklearn: `0 if beta == 0 else beta / delta`
            #[allow(clippy::float_cmp)]
            if beta == 0.0 { 0.0 } else { beta / delta }
        };
        let mu = empirical.trace() / features;
        let mut covariance = empirical * (1.0 - shrinkage);
        for j in 0..d {
            covariance[(j, j)] += shrinkage * mu;
        }
        let cholesky = covariance.cholesky()?.l();
        let log_determinant = 2.0 * cholesky.diagonal().map(f64::ln).sum();
        if !log_determinant.is_finite() {
            return None;
        }
        Some(Self {
            mean,
            cholesky,
            log_normaliser: -0.5 * (log_determinant + features * (2.0 * std::f64::consts::PI).ln()),
            shrinkage,
        })
    }

    pub(crate) fn log_density(&self, row: &[f64]) -> f64 {
        assert_eq!(row.len(), self.mean.len(), "feature count");
        let centred = DVector::from_fn(row.len(), |j, _| row[j] - self.mean[j]);
        let solved = self
            .cholesky
            .solve_lower_triangular(&centred)
            .expect("a Cholesky factor has a non-zero diagonal");
        self.log_normaliser - 0.5 * solved.norm_squared()
    }
}

/// Reference indices of each fold: `i mod K` with `K = min(n, 10)`.
pub(crate) fn folds(n: usize) -> Vec<Vec<usize>> {
    let k = n.min(CONFORMAL_MAX_FOLDS);
    assert!(k >= 2, "{n} references cannot be split into folds");
    (0..k).map(|fold| (fold..n).step_by(k).collect()).collect()
}

/// `p = (1 + c) / (n + 1) <= alpha`, in integers.
pub(crate) const fn conformal_novel(count: u64, n: usize) -> bool {
    CONFORMAL_ALPHA.1 * (1 + count) <= CONFORMAL_ALPHA.0 * (n as u64 + 1)
}

pub(crate) fn conformal_p_value(count: u64, n: usize) -> f64 {
    (1 + count) as f64 / (n + 1) as f64
}

/// The conformal decision for every test file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConformalNovelty {
    pub(crate) status: ConformalStatus,
    pub(crate) reason: Option<String>,
    pub(crate) reference: usize,
    pub(crate) tests: usize,
    pub(crate) folds: usize,
    /// Log-density of each test file under the model fitted on every
    /// reference (`None` when that fit fails).
    pub(crate) log_likelihoods: Option<Vec<f64>>,
    /// The cross-conformal count `c` of each test file (`None` unless decided).
    pub(crate) counts: Option<Vec<u64>>,
    pub(crate) shrinkage: Option<f64>,
}

impl ConformalNovelty {
    pub(crate) fn decisions(&self) -> Vec<Option<bool>> {
        self.counts.as_ref().map_or_else(
            || vec![None; self.tests],
            |counts| {
                counts
                    .iter()
                    .map(|&count| Some(conformal_novel(count, self.reference)))
                    .collect()
            },
        )
    }

    pub(crate) fn p_values(&self) -> Option<Vec<f64>> {
        self.counts.as_ref().map(|counts| {
            counts
                .iter()
                .map(|&count| conformal_p_value(count, self.reference))
                .collect()
        })
    }
}

/// Fit on `reference` (rows in resolved reference order) and decide every
/// row of `tests`.
pub(crate) fn decide(reference: &[Vec<f64>], tests: &[Vec<f64>]) -> ConformalNovelty {
    let n = reference.len();
    let all = reference.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let full = LedoitWolf::fit(&all);
    let mut outcome = ConformalNovelty {
        status: ConformalStatus::Decided,
        reason: None,
        reference: n,
        tests: tests.len(),
        folds: 0,
        log_likelihoods: full
            .as_ref()
            .map(|model| tests.iter().map(|row| model.log_density(row)).collect()),
        counts: None,
        shrinkage: full.as_ref().map(|model| model.shrinkage),
    };
    if n < MIN_CONFORMAL_REFERENCE {
        outcome.status = ConformalStatus::AbstainedInsufficientReference;
        outcome.reason = Some(format!(
            "{n} reference file(s): a cross-conformal p-value needs at least {MIN_CONFORMAL_REFERENCE} to reach 1/20"
        ));
        return outcome;
    }
    let folds = folds(n);
    outcome.folds = folds.len();
    let mut counts = vec![0_u64; tests.len()];
    for (index, held) in folds.iter().enumerate() {
        let training = (0..n)
            .filter(|i| i % folds.len() != index)
            .map(|i| reference[i].as_slice())
            .collect::<Vec<_>>();
        let Some(model) = LedoitWolf::fit(&training) else {
            outcome.status = ConformalStatus::AbstainedFitFailed;
            outcome.reason = Some(format!(
                "the shrunk covariance of fold {index} ({} references) is not positive definite",
                training.len()
            ));
            return outcome;
        };
        let held_out = held
            .iter()
            .map(|&i| model.log_density(&reference[i]))
            .collect::<Vec<_>>();
        for (count, test) in counts.iter_mut().zip(tests) {
            let value = model.log_density(test);
            assert!(!value.is_nan(), "NaN log-density");
            *count += held_out.iter().filter(|&&held| held <= value).count() as u64;
        }
    }
    if full.is_none() {
        outcome.status = ConformalStatus::AbstainedFitFailed;
        outcome.reason =
            Some("the shrunk covariance of every reference is not positive definite".to_owned());
        return outcome;
    }
    outcome.counts = Some(counts);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(values: &[&[f64]]) -> Vec<Vec<f64>> {
        values.iter().map(|row| row.to_vec()).collect()
    }

    #[test]
    fn ledoit_wolf_matches_scikit_learn() {
        let data = rows(&[
            &[1.0, 2.0],
            &[2.0, 3.5],
            &[0.5, 0.0],
            &[3.0, 4.0],
            &[1.5, 1.0],
            &[2.5, 4.5],
            &[0.0, 1.5],
        ]);
        let slices = data.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let model = LedoitWolf::fit(&slices).expect("fit");
        assert!(
            (model.shrinkage - SKLEARN_SHRINKAGE).abs() < 1e-12,
            "{}",
            model.shrinkage
        );
        let value = model.log_density(&[0.5, 1.0]);
        assert!((value - SKLEARN_LOG_DENSITY).abs() < 1e-10, "{value}");
    }

    // The pinned parity environment (scikit-learn 1.9.1, SciPy 1.18.1):
    // m = sklearn.covariance.LedoitWolf().fit(X) on the rows above;
    // m.shrinkage_ and
    // scipy.stats.multivariate_normal(m.location_, m.covariance_).logpdf([0.5, 1.0])
    const SKLEARN_SHRINKAGE: f64 = 0.203_583_715_794_792_9;
    const SKLEARN_LOG_DENSITY: f64 = -2.570_426_228_710_26;

    #[test]
    fn one_feature_is_not_shrunk_and_constant_data_has_no_model() {
        let data = rows(&[&[1.0], &[2.0], &[4.0]]);
        let slices = data.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let model = LedoitWolf::fit(&slices).expect("fit");
        assert!(model.shrinkage.abs() < f64::EPSILON);
        // variance 14/9 (divided by n), mean 7/3
        let expected = -0.5 * ((14.0_f64 / 9.0).ln() + (2.0 * std::f64::consts::PI).ln());
        assert!((model.log_density(&[7.0 / 3.0]) - expected).abs() < 1e-12);
        let constant = rows(&[&[1.0, 2.0], &[1.0, 2.0], &[1.0, 2.0]]);
        let slices = constant.iter().map(Vec::as_slice).collect::<Vec<_>>();
        assert!(LedoitWolf::fit(&slices).is_none());
        assert!(LedoitWolf::fit(&slices[..1]).is_none());
    }

    #[test]
    fn two_files_suffice_and_zero_features_have_no_model() {
        let pair = rows(&[&[1.0], &[3.0]]);
        let slices = pair.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let model = LedoitWolf::fit(&slices).expect("two distinct files");
        // variance 1 (divided by n), mean 2
        let expected = -0.5 * (2.0 * std::f64::consts::PI).ln();
        assert!((model.log_density(&[2.0]) - expected).abs() < 1e-12);
        let empty: [&[f64]; 2] = [&[], &[]];
        assert!(LedoitWolf::fit(&empty).is_none());
    }

    #[test]
    fn folds_partition_by_index() {
        let twenty = folds(20);
        assert_eq!(twenty.len(), 10);
        assert_eq!(twenty[3], vec![3, 13]);
        let mut all = twenty.concat();
        all.sort_unstable();
        assert_eq!(all, (0..20).collect::<Vec<_>>());
        assert_eq!(folds(5).len(), 5);
    }

    #[test]
    fn integer_rule_at_the_boundaries() {
        for (n, count, novel) in [
            (18, 0, false),
            (19, 0, true),
            (19, 1, false),
            (39, 0, true),
            (39, 1, true),
            (39, 2, false),
        ] {
            assert_eq!(conformal_novel(count, n), novel, "{n} {count}");
            assert_eq!(conformal_p_value(count, n) <= 0.05, novel, "{n} {count}");
        }
    }

    fn cohort(n: usize) -> Vec<Vec<f64>> {
        // deterministic, non-degenerate 3-feature rows
        (0..n)
            .map(|i| {
                let x = i as f64;
                vec![
                    (x * 0.37).sin(),
                    (x * 0.73).cos() + 0.1 * x / n as f64,
                    (x * 1.31).sin() * 0.5,
                ]
            })
            .collect()
    }

    #[test]
    fn abstains_below_nineteen_references_and_decides_above() {
        let tests = vec![vec![0.0, 0.0, 0.0], vec![25.0, -30.0, 40.0]];
        let small = decide(&cohort(18), &tests);
        assert_eq!(
            small.status,
            ConformalStatus::AbstainedInsufficientReference
        );
        assert_eq!(small.decisions(), vec![None, None]);
        assert!(small.log_likelihoods.is_some());
        let large = decide(&cohort(40), &tests);
        assert_eq!(large.status, ConformalStatus::Decided);
        assert_eq!(large.folds, 10);
        // A far outlier has count 0: below every held-out reference.
        assert_eq!(large.counts.as_ref().expect("counts")[1], 0);
        assert_eq!(large.decisions()[1], Some(true));
        assert_eq!(large.decisions()[0], Some(false));
        assert_eq!(large.p_values().expect("p")[1], 1.0 / 41.0);
    }

    #[test]
    fn counts_include_ties() {
        // Every reference identical but one feature varies: the test row equal
        // to the references ties with every held-out score.
        let mut reference = cohort(20);
        let tests = vec![reference[0].clone()];
        let outcome = decide(&reference, &tests);
        let count = outcome.counts.as_ref().expect("counts")[0];
        assert!(count >= 1, "the test row ties with its own copy");
        reference.truncate(19);
        assert_eq!(decide(&reference, &tests).folds, 10);
    }

    #[test]
    fn each_fold_is_scored_by_a_model_fitted_without_it() {
        let reference = cohort(23);
        let tests = vec![
            vec![0.1, 0.2, -0.1],
            vec![0.9, -0.4, 0.3],
            vec![-0.6, 1.0, 0.2],
        ];
        let mut expected = vec![0_u64; tests.len()];
        for held in folds(reference.len()) {
            let training = (0..reference.len())
                .filter(|i| !held.contains(i))
                .map(|i| reference[i].as_slice())
                .collect::<Vec<_>>();
            let model = LedoitWolf::fit(&training).expect("fit");
            for (count, test) in expected.iter_mut().zip(&tests) {
                let value = model.log_density(test);
                *count += held
                    .iter()
                    .filter(|&&i| model.log_density(&reference[i]) <= value)
                    .count() as u64;
            }
        }
        assert_eq!(decide(&reference, &tests).counts, Some(expected));
    }

    #[test]
    fn a_singular_fold_abstains() {
        let constant = vec![vec![1.0, 2.0]; 20];
        let outcome = decide(&constant, &[vec![1.0, 2.0]]);
        assert_eq!(outcome.status, ConformalStatus::AbstainedFitFailed);
        assert_eq!(outcome.decisions(), vec![None]);
        assert!(outcome.reason.expect("reason").contains("fold 0"));
    }
}
