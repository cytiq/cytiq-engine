//! Novelty detection as in `CytoScan::Flag(flagMethod = "novelty")`:
//! a Gaussian mixture is fitted on the reference features with
//! `mclust::Mclust` defaults ([`crate::mixture`]), the threshold is the 5%
//! quantile of the reference log-densities, and test files at or below it
//! are novelties.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use crate::error::CytoScanError;
use crate::mixture::{self, MclustModel, Mixture};

#[derive(Clone, Debug)]
pub(crate) struct NoveltyModel {
    mixture: Mixture,
    pub(crate) threshold: f64,
}

impl NoveltyModel {
    pub(crate) fn effective_counts(&self, rows: usize) -> Vec<f64> {
        #[allow(clippy::cast_precision_loss)]
        self.mixture
            .components
            .iter()
            .map(|component| component.log_weight.exp() * rows as f64)
            .collect()
    }
    pub(crate) fn components(&self) -> usize {
        self.mixture.components.len()
    }

    pub(crate) const fn model(&self) -> MclustModel {
        self.mixture.model
    }

    pub(crate) const fn bic(&self) -> f64 {
        self.mixture.bic
    }

    /// Log-density of one feature row under the mixture (`log(mclust::dens)`).
    pub(crate) fn log_density(&self, row: &[f64]) -> f64 {
        self.mixture.log_density(row)
    }
}

/// Fit the novelty model on reference features and set the threshold at
/// `quantile` of the reference log-densities.
pub(crate) fn fit(features: &[Vec<f64>], quantile: f64) -> Result<NoveltyModel, CytoScanError> {
    let rows = features.len();
    let dimensions = features.first().map_or(0, Vec::len);
    if rows == 0 || dimensions == 0 {
        return Err(CytoScanError::EmptyFeatureMatrix);
    }
    let Some(mixture) = mixture::mclust(features).best else {
        return Err(CytoScanError::NoveltyFit {
            reason: format!(
                "no mclust covariance model fits the {rows} reference file(s): every one is singular (constant features, or files with identical features)"
            ),
        });
    };
    let mut model = NoveltyModel {
        mixture,
        threshold: f64::NAN,
    };
    let mut reference = features
        .iter()
        .map(|row| model.log_density(row))
        .collect::<Vec<_>>();
    model.threshold = quantile_type7(&mut reference, quantile);
    Ok(model)
}

fn quantile_type7(values: &mut [f64], probability: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    let position = probability * (values.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    values[lower] + (values[upper] - values[lower]) * (position - lower as f64)
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

    include!("../../../data/fixtures/cytoscan/reference.rs");

    fn points(range: std::ops::Range<usize>) -> Vec<Vec<f64>> {
        GMM_POINTS[range].iter().map(|row| row.to_vec()).collect()
    }

    #[test]
    fn single_cluster_fit_matches_mclust() {
        let features = points(0..120);
        let model = fit(&features, 0.05).expect("fit");
        assert_eq!(model.components(), GMM_ONE_G);
        assert_eq!(model.model().name(), GMM_ONE_MODEL);
        for (row, expected) in features.iter().zip(GMM_ONE_LOG_DENSITY) {
            assert!((model.log_density(row) - expected).abs() < 1e-8);
        }
    }

    #[test]
    fn fit_is_deterministic() {
        let features = points(0..GMM_POINTS.len());
        let first = fit(&features, 0.05).expect("fit");
        let second = fit(&features, 0.05).expect("fit");
        assert_eq!(first.threshold.to_bits(), second.threshold.to_bits());
    }

    #[test]
    fn constant_references_have_no_model() {
        let features = vec![vec![1.0, 2.0, 3.0]; 4];
        let error = fit(&features, 0.05).expect_err("every model is singular");
        assert!(
            error.to_string().contains("no mclust covariance model"),
            "{error}"
        );
    }
}
