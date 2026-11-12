//! Spectrum estimation as upstream `AutoSpectral` does it after
//! `clean.controls`: each detector's robust slope against the peak detector
//! (`fit.robust.linear.model`, `MASS::rlm`) over the positive events and
//! negatives matched to them on scatter (`FNN::knnx.index`).

/// `MASS::psi.huber`'s default tuning constant.
const HUBER_K: f64 = 1.345;
/// `rlm`'s default convergence tolerance on the residuals (`acc`).
const RLM_ACCURACY: f64 = 1e-4;
/// `asp$rlm.iter.max`.
pub const RLM_MAX_ITERATIONS: usize = 100;
/// `k.neighbors` of `clean.controls`: negatives per positive on scatter.
pub const SCATTER_NEIGHBOURS: usize = 3;

/// The peak-normalised spectrum of `rows` (event indices) in `columns` (one
/// per detector): `1` at `peak`, every other detector the `rlm` slope of
/// that detector on the peak detector, then divided by the largest value
/// (`get.fluorophore.spectra`). `None` when no value is positive or a slope
/// is undefined (no spread on the peak detector).
pub(crate) fn spectrum(columns: &[&[f64]], peak: usize) -> Option<Vec<f64>> {
    let x = columns[peak];
    let coefficients = columns
        .iter()
        .enumerate()
        .map(|(detector, y)| {
            if detector == peak {
                1.0
            } else {
                robust_slope(x, y)
            }
        })
        .collect::<Vec<_>>();
    if coefficients.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let largest = coefficients
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    (largest > 0.0).then(|| coefficients.iter().map(|value| value / largest).collect())
}

/// `fit.robust.linear.model`: the slope of `rlm(y ~ x)` (Huber weights, MAD
/// scale, IRLS from least squares), or of `lm(y ~ x)` when `rlm` does not
/// converge in `RLM_MAX_ITERATIONS`.
pub(crate) fn robust_slope(x: &[f64], y: &[f64]) -> f64 {
    let ones = vec![1.0; x.len()];
    let (mut intercept, mut slope) = weighted_line(x, y, &ones);
    let ols = slope;
    let mut residuals = residuals(x, y, intercept, slope);
    for _ in 0..RLM_MAX_ITERATIONS {
        let previous = residuals.clone();
        let scale = cytiq_density::median_mut(
            &mut residuals
                .iter()
                .map(|value| value.abs())
                .collect::<Vec<_>>(),
        ) / 0.6745;
        if scale == 0.0 {
            return slope;
        }
        let weights = residuals
            .iter()
            .map(|residual| (HUBER_K / (residual / scale).abs()).min(1.0))
            .collect::<Vec<_>>();
        (intercept, slope) = weighted_line(x, y, &weights);
        residuals = self::residuals(x, y, intercept, slope);
        // `irls.delta` on the residuals.
        let change = previous
            .iter()
            .zip(&residuals)
            .map(|(old, new)| (old - new).powi(2))
            .sum::<f64>();
        let size = previous
            .iter()
            .map(|old| old.powi(2))
            .sum::<f64>()
            .max(1e-20);
        if (change / size).sqrt() <= RLM_ACCURACY {
            return slope;
        }
    }
    ols
}

/// Weighted least-squares line `y = a + b x`.
fn weighted_line(x: &[f64], y: &[f64], weights: &[f64]) -> (f64, f64) {
    let total = weights.iter().sum::<f64>();
    let mean_x = x.iter().zip(weights).map(|(x, w)| w * x).sum::<f64>() / total;
    let mean_y = y.iter().zip(weights).map(|(y, w)| w * y).sum::<f64>() / total;
    let (mut covariance, mut variance) = (0.0, 0.0);
    for ((x, y), w) in x.iter().zip(y).zip(weights) {
        covariance += w * (x - mean_x) * (y - mean_y);
        variance += w * (x - mean_x).powi(2);
    }
    let slope = covariance / variance;
    (mean_y - slope * mean_x, slope)
}

fn residuals(x: &[f64], y: &[f64], intercept: f64, slope: f64) -> Vec<f64> {
    x.iter()
        .zip(y)
        .map(|(x, y)| y - intercept - slope * x)
        .collect()
}

/// `unique(as.vector(FNN::knnx.index(negative, positive, k)))`: the
/// `SCATTER_NEIGHBOURS` nearest negatives (Euclidean on raw FSC/SSC) of every
/// positive, first neighbours of all positives first, without repeats.
pub(crate) fn scatter_neighbours(positive: &[[f64; 2]], negative: &[[f64; 2]]) -> Vec<usize> {
    let k = SCATTER_NEIGHBOURS.min(negative.len());
    let nearest = positive
        .iter()
        .map(|point| {
            let mut best = Vec::<(f64, usize)>::with_capacity(k + 1);
            for (index, candidate) in negative.iter().enumerate() {
                let distance =
                    (point[0] - candidate[0]).powi(2) + (point[1] - candidate[1]).powi(2);
                if best.len() < k || distance < best[k - 1].0 {
                    let at = best.partition_point(|(other, _)| *other <= distance);
                    best.insert(at, (distance, index));
                    best.truncate(k);
                }
            }
            best.into_iter().map(|(_, index)| index).collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut seen = vec![false; negative.len()];
    let mut matched = Vec::new();
    for rank in 0..k {
        for neighbours in &nearest {
            let index = neighbours[rank];
            if !seen[index] {
                seen[index] = true;
                matched.push(index);
            }
        }
    }
    matched
}

/// At most `limit` of `values`, evenly spaced: upstream draws them at random
/// (`sample`), cytiq picks a fixed subset so runs are reproducible.
pub(crate) fn evenly_spaced<T: Copy>(values: &[T], limit: usize) -> Vec<T> {
    if values.len() <= limit {
        return values.to_vec();
    }
    (0..limit)
        .map(|position| values[position * values.len() / limit])
        .collect()
}

#[cfg(test)]
#[allow(clippy::unreadable_literal, clippy::excessive_precision)]
mod tests {
    use super::*;

    include!("../../../data/fixtures/autospectral/rlm_reference.rs");

    #[test]
    fn robust_slope_matches_mass_rlm() {
        const { assert!(RLM_CONVERGED) };
        let slope = robust_slope(&RLM_X, &RLM_Y);
        assert!(
            (slope - RLM_SLOPE).abs() < 1e-6 * RLM_SLOPE.abs(),
            "{slope} vs {RLM_SLOPE}"
        );
    }

    #[test]
    fn robust_slope_falls_back_to_least_squares_like_autospectral() {
        // rlm does not converge in 100 steps here.
        const { assert!(!RLM_DIVERGENT_CONVERGED) };
        let slope = robust_slope(&RLM_DIVERGENT_X, &RLM_DIVERGENT_Y);
        assert!(
            (slope - RLM_DIVERGENT_SLOPE).abs() < 1e-9 * RLM_DIVERGENT_SLOPE.abs(),
            "{slope} vs lm {RLM_DIVERGENT_SLOPE}"
        );
    }

    #[test]
    fn spectrum_is_one_at_the_peak_and_peak_normalised() {
        let x = (0..50).map(f64::from).collect::<Vec<_>>();
        let half = x.iter().map(|value| 0.5 * value + 3.0).collect::<Vec<_>>();
        let double = x.iter().map(|value| 2.0 * value).collect::<Vec<_>>();
        let spectrum = spectrum(&[&half, &x, &double], 1).expect("positive spectrum");
        let expected = [0.25, 0.5, 1.0];
        for (value, expected) in spectrum.iter().zip(expected) {
            assert!((value - expected).abs() < 1e-12, "{spectrum:?}");
        }
    }

    #[test]
    fn a_constant_peak_detector_has_no_spectrum() {
        let flat = [5.0; 10];
        let other = (0..10).map(f64::from).collect::<Vec<_>>();
        assert_eq!(spectrum(&[&flat, &other], 0), None);
    }

    #[test]
    fn scatter_neighbours_take_first_neighbours_first() {
        let negative = [[0.0, 0.0], [10.0, 0.0], [1.0, 0.0], [11.0, 0.0], [5.0, 5.0]];
        let matched = scatter_neighbours(&[[0.2, 0.0], [10.2, 0.0]], &negative);
        assert_eq!(matched, [0, 1, 2, 3, 4]);
        assert_eq!(evenly_spaced(&matched, 2), [0, 2]);
    }
}
