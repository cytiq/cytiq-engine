//! Outlier detection within one cohort, as in
//! `CytoScan::Flag(flagMethod = "outlier")`: an isolation forest
//! (`isotree::isolation.forest`) is fitted on the test cohort's own feature
//! rows and scores each of them. The reference cohort is never used, so the
//! score of a file depends on the other files of that cohort, and the fixed
//! 0.5 cut-off is a heuristic, not a calibrated false-positive rate.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, Uniform};
use rayon::prelude::*;

use crate::runner::DEFAULT_ISOLATION_TREES;

/// Whether [`score`] builds trees for `rows` files with `dimensions`
/// features; otherwise every score is 0 and the seed has no effect.
pub(crate) const fn uses_trees(rows: usize, dimensions: usize) -> bool {
    rows >= 2 && dimensions >= 1
}

#[derive(Clone, Debug)]
enum IsolationNode {
    Leaf {
        size: usize,
    },
    Split {
        feature_index: usize,
        threshold: f64,
        left: Box<IsolationNode>,
        right: Box<IsolationNode>,
    },
}

/// `isotree::isolation.forest(sample_size = 1, ntrees = 1000, ndim = 1)` scored
/// with `predict()`: every tree sees all files, depth is limited to
/// `ceiling(log2(n))`, each split picks a non-constant column uniformly and a
/// threshold uniformly between that column's node minimum and maximum, and the
/// standardized score is `2^(-mean depth / c(n))` with `c(n) = 2 (H_n - 1)`.
/// Tree `t` uses `seed + t` (wrapping), as the upstream default seed 42 does.
pub(crate) fn score(features: &[Vec<f64>], seed: u64) -> Vec<f64> {
    if !uses_trees(features.len(), features.first().map_or(0, Vec::len)) {
        return vec![0.0; features.len()];
    }
    let max_depth = (features.len() as f64).log2().ceil() as usize;
    let normalization = expected_average_depth(features.len());
    let indexes = (0..features.len()).collect::<Vec<_>>();
    let depth_sums = (0..DEFAULT_ISOLATION_TREES)
        .into_par_iter()
        .map(|tree_index| {
            let mut rng = StdRng::seed_from_u64(seed.wrapping_add(tree_index as u64));
            let tree = build_isolation_tree(features, &indexes, 0, max_depth, &mut rng);
            features
                .iter()
                .map(|row| tree.path_length(row, 0.0))
                .collect::<Vec<_>>()
        })
        // collected in tree order and summed sequentially so the result does
        // not depend on rayon's scheduling
        .collect::<Vec<_>>()
        .into_iter()
        .fold(vec![0.0; features.len()], |mut totals, depths| {
            for (total, value) in totals.iter_mut().zip(depths) {
                *total += value;
            }
            totals
        });
    depth_sums
        .into_iter()
        .map(|sum| 2_f64.powf(-(sum / DEFAULT_ISOLATION_TREES as f64) / normalization))
        .collect()
}

fn build_isolation_tree(
    features: &[Vec<f64>],
    indexes: &[usize],
    depth: usize,
    max_depth: usize,
    rng: &mut StdRng,
) -> IsolationNode {
    if indexes.len() <= 1 || depth >= max_depth {
        return IsolationNode::Leaf {
            size: indexes.len(),
        };
    }
    let mut candidates = (0..features.first().map_or(0, Vec::len)).collect::<Vec<_>>();
    while !candidates.is_empty() {
        let feature_index =
            candidates.swap_remove(crate::sampling::random_index(rng, candidates.len()));
        let (minimum, maximum) = feature_min_max(features, indexes, feature_index);
        if maximum <= minimum {
            // constant within this node: isotree drops the column and retries
            continue;
        }
        let threshold = Uniform::new(minimum, maximum)
            .expect("nonconstant feature range must be finite")
            .sample(rng);
        let (left, right): (Vec<usize>, Vec<usize>) = indexes
            .iter()
            .partition(|index| features[**index][feature_index] <= threshold);
        if left.is_empty() || right.is_empty() {
            continue;
        }
        return IsolationNode::Split {
            feature_index,
            threshold,
            left: Box::new(build_isolation_tree(
                features,
                &left,
                depth + 1,
                max_depth,
                rng,
            )),
            right: Box::new(build_isolation_tree(
                features,
                &right,
                depth + 1,
                max_depth,
                rng,
            )),
        };
    }
    IsolationNode::Leaf {
        size: indexes.len(),
    }
}

impl IsolationNode {
    fn path_length(&self, row: &[f64], depth: f64) -> f64 {
        match self {
            Self::Leaf { size } => depth + expected_average_depth(*size),
            Self::Split {
                feature_index,
                threshold,
                left,
                right,
            } => {
                if row[*feature_index] <= *threshold {
                    left.path_length(row, depth + 1.0)
                } else {
                    right.path_length(row, depth + 1.0)
                }
            }
        }
    }
}

fn feature_min_max(features: &[Vec<f64>], indexes: &[usize], feature_index: usize) -> (f64, f64) {
    indexes.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(minimum, maximum), index| {
            let value = features[*index][feature_index];
            (minimum.min(value), maximum.max(value))
        },
    )
}

/// isotree's `expected_avg_depth`: `2 (H_n - 1)` with the exact harmonic number
/// (isotree tabulates it up to 512 and uses an asymptotic series above).
fn expected_average_depth(size: usize) -> f64 {
    if size <= 1 {
        return 0.0;
    }
    let harmonic = (1..=size).rev().map(|k| 1.0 / k as f64).sum::<f64>();
    2.0 * (harmonic - 1.0)
}

#[cfg(test)]
#[allow(
    dead_code,
    clippy::approx_constant,
    clippy::unreadable_literal,
    clippy::excessive_precision
)]
mod tests {
    use super::*;
    use crate::runner::{DEFAULT_OUTLIER_THRESHOLD, DEFAULT_RANDOM_SEED};

    include!("../../../data/fixtures/cytoscan/reference.rs");

    fn rows<const N: usize>(matrix: &[[f64; N]]) -> Vec<Vec<f64>> {
        matrix.iter().map(|row| row.to_vec()).collect()
    }

    #[test]
    fn expected_depth_matches_isotree_table() {
        // isotree exp_depth_table[n - 1] = 2 (H_n - 1)
        for (size, expected) in [
            (1, 0.0),
            (2, 1.0),
            (3, 1.666_666_666_666_666_5),
            (10, 3.857_936_507_936_507_5),
            (20, 5.195_479_314_287_363),
        ] {
            assert!(
                (expected_average_depth(size) - expected).abs() < 1e-12,
                "n = {size}"
            );
        }
    }

    #[test]
    fn outlier_scores_match_isotree_within_monte_carlo_noise() {
        // 1000-tree isotree scores vary by ~0.014 between independent seeds
        let scores = score(&rows(&ISOTREE_POINTS), DEFAULT_RANDOM_SEED);
        for (actual, expected) in scores.iter().zip(ISOTREE_SCORES) {
            assert!((actual - expected).abs() < 0.03, "{actual} vs {expected}");
            assert_eq!(
                *actual >= DEFAULT_OUTLIER_THRESHOLD,
                expected >= DEFAULT_OUTLIER_THRESHOLD
            );
        }
        assert_eq!(scores, score(&rows(&ISOTREE_POINTS), DEFAULT_RANDOM_SEED));
    }

    #[test]
    fn seed_changes_the_trees() {
        let points = rows(&ISOTREE_POINTS);
        assert_ne!(score(&points, 7), score(&points, DEFAULT_RANDOM_SEED));
        // Tree seeds wrap instead of overflowing.
        assert_eq!(score(&points, u64::MAX).len(), points.len());
    }

    #[test]
    fn fewer_than_two_files_or_no_features_build_no_trees() {
        assert!(!uses_trees(0, 3));
        assert!(!uses_trees(1, 3));
        assert!(!uses_trees(5, 0));
        assert!(uses_trees(2, 1));
        assert_eq!(score(&[vec![1.0, 2.0]], 7), vec![0.0]);
        assert_eq!(score(&[Vec::new(), Vec::new()], 7), vec![0.0, 0.0]);
    }
}
