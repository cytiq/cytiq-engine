#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RegressionResult {
    pub intercept: f64,
    pub slope: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LinearModelSummary {
    pub intercept: f64,
    pub slope: f64,
    pub slope_t: f64,
}

// AutoSpill estimates each spillover term from `secondary ~ primary` with
// `MASS::rlm(y ~ x, maxit)`: Huber psi (k = 1.345), scale re-estimated each
// iteration as `median(|resid|) / 0.6745`, IRLS until the relative change of
// the residuals is <= 1e-4. `fit.robust.linear.model` falls back to OLS
// (`lm(y ~ x)`) when rlm does not converge; we do the same.
pub(crate) fn robust_linear_regression(
    x: &[f64],
    y: &[f64],
    max_iterations: usize,
) -> Option<RegressionResult> {
    const HUBER_K: f64 = 1.345;
    const IRLS_ACCURACY: f64 = 1e-4;

    if x.len() != y.len() || x.len() < 3 {
        return None;
    }
    let ordinary = || {
        ordinary_linear_regression(x, y).map(|summary| RegressionResult {
            intercept: summary.intercept,
            slope: summary.slope,
        })
    };
    let residuals_for = |intercept: f64, slope: f64| {
        x.iter()
            .zip(y)
            .map(|(x_value, y_value)| y_value - (intercept + slope * x_value))
            .collect::<Vec<_>>()
    };

    let mut weights = vec![1.0; x.len()];
    let Some((mut intercept, mut slope)) = weighted_fit(x, y, &weights) else {
        return ordinary();
    };
    let mut residuals = residuals_for(intercept, slope);

    for _ in 0..max_iterations {
        let scale = median_absolute(&residuals) / 0.6745;
        if scale == 0.0 {
            return Some(RegressionResult { intercept, slope });
        }
        for (weight, residual) in weights.iter_mut().zip(&residuals) {
            *weight = (HUBER_K / (residual / scale).abs()).min(1.0);
        }
        let Some((next_intercept, next_slope)) = weighted_fit(x, y, &weights) else {
            return ordinary();
        };
        let next_residuals = residuals_for(next_intercept, next_slope);
        let change = residuals
            .iter()
            .zip(&next_residuals)
            .map(|(old, new)| (old - new) * (old - new))
            .sum::<f64>();
        let previous = residuals.iter().map(|old| old * old).sum::<f64>();
        intercept = next_intercept;
        slope = next_slope;
        residuals = next_residuals;
        if (change / previous.max(1e-20)).sqrt() <= IRLS_ACCURACY {
            return Some(RegressionResult { intercept, slope });
        }
    }

    ordinary()
}

fn median_absolute(values: &[f64]) -> f64 {
    let mut absolute = values.iter().map(|value| value.abs()).collect::<Vec<_>>();
    absolute.sort_by(f64::total_cmp);
    let middle = absolute.len() / 2;
    if absolute.len() % 2 == 0 {
        f64::midpoint(absolute[middle - 1], absolute[middle])
    } else {
        absolute[middle]
    }
}

pub(crate) fn ordinary_linear_regression(x: &[f64], y: &[f64]) -> Option<LinearModelSummary> {
    if x.len() != y.len() || x.len() < 3 {
        return None;
    }

    let mean_x = x.iter().sum::<f64>() / (x.len() as f64);
    let mean_y = y.iter().sum::<f64>() / (y.len() as f64);
    let sxx = x
        .iter()
        .map(|value| {
            let centered = value - mean_x;
            centered * centered
        })
        .sum::<f64>();
    if sxx <= f64::EPSILON {
        return None;
    }
    let sxy = x
        .iter()
        .zip(y)
        .map(|(x_value, y_value)| (x_value - mean_x) * (y_value - mean_y))
        .sum::<f64>();
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;

    let rss = x
        .iter()
        .zip(y)
        .map(|(x_value, y_value)| {
            let residual = y_value - (intercept + slope * x_value);
            residual * residual
        })
        .sum::<f64>();
    let degrees_of_freedom = (x.len() as f64) - 2.0;
    let variance = if degrees_of_freedom > 0.0 {
        rss / degrees_of_freedom
    } else {
        0.0
    };
    let slope_standard_error = (variance / sxx).sqrt();
    let slope_t = if slope_standard_error <= f64::EPSILON {
        f64::INFINITY
    } else {
        slope / slope_standard_error
    };

    Some(LinearModelSummary {
        intercept,
        slope,
        slope_t,
    })
}

pub(crate) fn ordinary_linear_regression_without_intercept(
    x: &[f64],
    y: &[f64],
) -> Option<LinearModelSummary> {
    if x.len() != y.len() || x.len() < 2 {
        return None;
    }

    let sxx = x.iter().map(|value| value * value).sum::<f64>();
    if sxx <= f64::EPSILON {
        return None;
    }
    let sxy = x
        .iter()
        .zip(y)
        .map(|(x_value, y_value)| x_value * y_value)
        .sum::<f64>();
    let slope = sxy / sxx;
    let rss = x
        .iter()
        .zip(y)
        .map(|(x_value, y_value)| {
            let residual = y_value - slope * x_value;
            residual * residual
        })
        .sum::<f64>();
    let degrees_of_freedom = (x.len() as f64) - 1.0;
    let variance = if degrees_of_freedom > 0.0 {
        rss / degrees_of_freedom
    } else {
        0.0
    };
    let slope_standard_error = (variance / sxx).sqrt();
    let slope_t = if slope_standard_error <= f64::EPSILON {
        f64::INFINITY
    } else {
        slope / slope_standard_error
    };

    Some(LinearModelSummary {
        intercept: 0.0,
        slope,
        slope_t,
    })
}

pub(crate) fn trim_joint_pairs(
    primary: &[f64],
    secondary: &[f64],
    trim_factor: f64,
) -> (Vec<f64>, Vec<f64>) {
    if primary.len() != secondary.len() || primary.len() < 3 {
        return (Vec::new(), Vec::new());
    }

    let trim_n = ((primary.len() as f64) * trim_factor).round_ties_even() as usize;
    let trim_n = trim_n.clamp(1, primary.len());
    let primary_bounds = sorted_trim_bounds(primary, trim_n - 1);
    let secondary_bounds = sorted_trim_bounds(secondary, trim_n - 1);
    let (primary_low, primary_high) = primary_bounds;
    let (secondary_low, secondary_high) = secondary_bounds;

    let mut trimmed_primary = Vec::new();
    let mut trimmed_secondary = Vec::new();
    for (&primary_value, &secondary_value) in primary.iter().zip(secondary) {
        if primary_value > primary_low
            && primary_value < primary_high
            && secondary_value > secondary_low
            && secondary_value < secondary_high
        {
            trimmed_primary.push(primary_value);
            trimmed_secondary.push(secondary_value);
        }
    }

    if trimmed_primary.len() < 3 {
        (primary.to_vec(), secondary.to_vec())
    } else {
        (trimmed_primary, trimmed_secondary)
    }
}

pub(crate) fn skewness(values: &[f64]) -> f64 {
    if values.len() < 3 {
        return 0.0;
    }

    let mean = values.iter().sum::<f64>() / (values.len() as f64);
    let variance = values
        .iter()
        .map(|value| {
            let centered = value - mean;
            centered * centered
        })
        .sum::<f64>()
        / (values.len() as f64);
    let standard_deviation = variance.sqrt();
    if standard_deviation <= f64::EPSILON {
        return 0.0;
    }

    let third_moment = values
        .iter()
        .map(|value| {
            let centered = value - mean;
            centered * centered * centered
        })
        .sum::<f64>()
        / (values.len() as f64);
    third_moment / standard_deviation.powi(3)
}

/// R's `median`, with `0` for no values.
pub(crate) fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        cytiq_density::median(values)
    }
}

pub(crate) fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let clamped = fraction.clamp(0.0, 1.0);
    let position = clamped * ((sorted.len() - 1) as f64);
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let weight = position - (lower as f64);
        (1.0 - weight) * sorted[lower] + weight * sorted[upper]
    }
}

pub(crate) fn robust_standard_deviation(values: &[f64]) -> f64 {
    percentile(values, 0.84) - median(values)
}

pub(crate) fn sample_standard_deviation_iter<I>(values: I) -> f64
where
    I: IntoIterator<Item = f64>,
{
    let mut count = 0_usize;
    let mut mean = 0.0;
    let mut squared_distance_sum = 0.0;

    for value in values {
        count += 1;
        let delta = value - mean;
        mean += delta / (count as f64);
        let delta_next = value - mean;
        squared_distance_sum += delta * delta_next;
    }

    if count < 2 {
        0.0
    } else {
        (squared_distance_sum / ((count as f64) - 1.0)).sqrt()
    }
}

fn weighted_fit(x: &[f64], y: &[f64], weights: &[f64]) -> Option<(f64, f64)> {
    let weight_sum = weights.iter().sum::<f64>();
    if weight_sum <= f64::EPSILON {
        return None;
    }

    let mean_x = x
        .iter()
        .zip(weights)
        .map(|(value, weight)| value * weight)
        .sum::<f64>()
        / weight_sum;
    let mean_y = y
        .iter()
        .zip(weights)
        .map(|(value, weight)| value * weight)
        .sum::<f64>()
        / weight_sum;

    let covariance = x
        .iter()
        .zip(y)
        .zip(weights)
        .map(|((x_value, y_value), weight)| weight * (x_value - mean_x) * (y_value - mean_y))
        .sum::<f64>();
    let variance = x
        .iter()
        .zip(weights)
        .map(|(value, weight)| weight * (value - mean_x) * (value - mean_x))
        .sum::<f64>();
    if variance <= f64::EPSILON {
        return None;
    }

    let slope = covariance / variance;
    let intercept = mean_y - slope * mean_x;
    Some((intercept, slope))
}

fn sorted_trim_bounds(values: &[f64], index: usize) -> (f64, f64) {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let high_index = sorted.len().saturating_sub(index + 1);
    (sorted[index], sorted[high_index])
}

#[cfg(test)]
mod tests {
    use super::{
        ordinary_linear_regression_without_intercept, robust_linear_regression,
        robust_standard_deviation, trim_joint_pairs,
    };

    #[test]
    fn robust_regression_handles_outliers() {
        let x = (1..=40).map(f64::from).collect::<Vec<_>>();
        let mut y = x.iter().map(|value| 2.0 * value + 5.0).collect::<Vec<_>>();
        y[5] = 500.0;
        y[22] = -300.0;

        let result = robust_linear_regression(&x, &y, 100)
            .expect("regression should converge with outliers");

        assert!((result.intercept - 5.0).abs() < 1.0);
        assert!((result.slope - 2.0).abs() < 0.05);
    }

    #[test]
    fn trim_joint_pairs_removes_extremes_from_both_axes() {
        let primary = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10_000.0];
        let secondary = vec![2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0, -2_000.0];
        let (trimmed_primary, trimmed_secondary) = trim_joint_pairs(&primary, &secondary, 0.2);

        assert_eq!(trimmed_primary, vec![3.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(trimmed_secondary, vec![6.0, 8.0, 10.0, 12.0, 14.0]);
    }

    #[test]
    fn no_intercept_regression_fits_expected_slope() {
        let x = vec![1.0, 2.0, 3.0, 4.0];
        let y = vec![0.5, 1.0, 1.5, 2.0];
        let result = ordinary_linear_regression_without_intercept(&x, &y)
            .expect("no-intercept regression should fit");
        assert!((result.slope - 0.5).abs() < 1e-9);
        assert!(result.slope_t.is_infinite() || result.slope_t.abs() >= 1.96);
    }

    #[test]
    fn robust_standard_deviation_uses_percentile84_minus_median() {
        let values = vec![1.0, 2.0, 2.0, 3.0, 10.0];
        let sigma = robust_standard_deviation(&values);
        assert!(sigma.is_finite());
        assert!(sigma > 0.0);
    }
}
