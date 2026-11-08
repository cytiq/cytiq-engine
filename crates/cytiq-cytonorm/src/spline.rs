//! Monotone cubic Hermite interpolation from Fritsch & Carlson (1980).
//! Implemented from the method; R `stats::splinefun` is a black-box test oracle.
#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::float_cmp
)]
use crate::{Result, invalid};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonotoneSpline {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub slopes: Vec<f64>,
}
impl MonotoneSpline {
    /// Fit nondecreasing points, averaging targets at identical source values.
    ///
    /// # Errors
    /// Requires two distinct finite sources and monotone finite targets.
    pub fn fit(x: &[f64], y: &[f64]) -> Result<Self> {
        if x.len() != y.len()
            || x.len() < 2
            || x.iter().chain(y).any(|v| !v.is_finite())
            || x.windows(2).any(|w| w[0] > w[1])
            || y.windows(2).any(|w| w[0] > w[1])
        {
            return Err(invalid("spline knots must be finite and nondecreasing"));
        }
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        let mut i = 0;
        while i < x.len() {
            let mut j = i + 1;
            while j < x.len() && x[j] == x[i] {
                j += 1;
            }
            xs.push(x[i]);
            ys.push(y[i..j].iter().sum::<f64>() / (j - i) as f64);
            i = j;
        }
        if xs.len() < 2 {
            return Err(invalid("spline requires two distinct source knots"));
        }
        let secants = xs
            .windows(2)
            .zip(ys.windows(2))
            .map(|(a, b)| secant(a[0], a[1], b[0], b[1]))
            .collect::<Result<Vec<_>>>()?;
        let mut slopes = vec![0.0; xs.len()];
        slopes[0] = secants[0];
        slopes[xs.len() - 1] = secants[secants.len() - 1];
        for i in 1..xs.len() - 1 {
            slopes[i] = secants[i - 1].midpoint(secants[i]);
        }
        for (i, &d) in secants.iter().enumerate() {
            if d == 0.0 {
                slopes[i] = 0.0;
                slopes[i + 1] = 0.0;
            } else {
                let a = slopes[i] / d;
                let b = slopes[i + 1] / d;
                // The radius-three circle is sufficient, but not necessary,
                // for monotonicity. R monoH.FC preserves the entire monotone
                // region before applying the Fritsch–Carlson radial correction.
                if minimum_derivative(a, b)? < 0.0 {
                    let factor = 3.0 / a.hypot(b);
                    slopes[i] = factor * a * d;
                    slopes[i + 1] = factor * b * d;
                }
            }
        }
        let spline = Self {
            x: xs,
            y: ys,
            slopes,
        };
        spline.validate()?;
        Ok(spline)
    }
    /// Validate stored coefficients, including the monotonicity constraint.
    ///
    /// # Errors
    /// Rejects malformed or non-monotone coefficient arrays.
    pub fn validate(&self) -> Result<()> {
        if self.x.len() < 2
            || self.x.len() != self.y.len()
            || self.x.len() != self.slopes.len()
            || self
                .x
                .iter()
                .chain(&self.y)
                .chain(&self.slopes)
                .any(|v| !v.is_finite())
            || self.x.windows(2).any(|w| w[0] >= w[1])
            || self.y.windows(2).any(|w| w[0] > w[1])
            || self.slopes.iter().any(|s| *s < 0.0)
        {
            return Err(invalid("invalid monotone spline coefficients"));
        }
        // Check the actual derivative, including monotone intervals outside
        // the radius-three circle. Stored legacy coefficients remain valid.
        for i in 0..self.x.len() - 1 {
            let d = secant(self.x[i], self.x[i + 1], self.y[i], self.y[i + 1])?;
            if (d == 0.0 && (self.slopes[i] != 0.0 || self.slopes[i + 1] != 0.0))
                || (d > 0.0
                    && minimum_derivative(self.slopes[i] / d, self.slopes[i + 1] / d)? < -1e-12)
            {
                return Err(invalid(
                    "monoH.FC slopes violate monotonicity after adjacent corrections",
                ));
            }
        }
        Ok(())
    }
    /// Evaluate with linear endpoint extrapolation. Call validate after loading.
    #[must_use]
    pub fn evaluate(&self, value: f64) -> f64 {
        if !value.is_finite() {
            return f64::NAN;
        }
        let n = self.x.len();
        if value <= self.x[0] {
            return self.y[0] + self.slopes[0] * (value - self.x[0]);
        }
        if value >= self.x[n - 1] {
            return self.y[n - 1] + self.slopes[n - 1] * (value - self.x[n - 1]);
        }
        let i = self.x.partition_point(|x| *x <= value) - 1;
        let width = self.x[i + 1] - self.x[i];
        let t = (value - self.x[i]) / width;
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * self.y[i]
            + (t3 - 2.0 * t2 + t) * width * self.slopes[i]
            + (-2.0 * t3 + 3.0 * t2) * self.y[i + 1]
            + (t3 - t2) * width * self.slopes[i + 1]
    }
}

fn secant(x0: f64, x1: f64, y0: f64, y1: f64) -> Result<f64> {
    let width = x1 - x0;
    let height = y1 - y0;
    let slope = height / width;
    if !width.is_finite() || width <= 0.0 || !height.is_finite() || !slope.is_finite() {
        return Err(invalid("nonfinite spline interval or secant"));
    }
    Ok(slope)
}

/// Minimum of the normalized Hermite derivative on [0, 1]. Scaling keeps
/// the quadratic bounded even when a finite slope/secant ratio is large.
fn minimum_derivative(a: f64, b: f64) -> Result<f64> {
    if !a.is_finite() || !b.is_finite() {
        return Err(invalid("nonfinite normalized spline slope"));
    }
    let scale = a.max(b).max(1.0);
    let (a, b, d) = (a / scale, b / scale, 1.0 / scale);
    let quadratic = 3.0 * (a + b - 2.0 * d);
    let linear = 2.0 * (3.0 * d - 2.0 * a - b);
    let mut minimum = a.min(b);
    if quadratic > 0.0 {
        let t = -linear / (2.0 * quadratic);
        if t > 0.0 && t < 1.0 {
            minimum = minimum.min(a + t * (linear + t * quadratic));
        }
    }
    Ok(minimum * scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_committed_r_spline_and_quantile_oracles() {
        #[derive(serde::Deserialize)]
        struct SplineCase {
            x: Vec<f64>,
            y: Vec<f64>,
            query: Vec<f64>,
            expected: Vec<f64>,
            slopes: Vec<f64>,
            monotone: bool,
        }
        #[derive(serde::Deserialize)]
        struct QuantileCase {
            sorted: Vec<f64>,
            probabilities: Vec<f64>,
            expected: Vec<f64>,
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            splines: Vec<SplineCase>,
            quantiles: Vec<QuantileCase>,
        }
        let fixture: Fixture =
            serde_json::from_str(include_str!("../../../data/fixtures/cytonorm/numeric.json"))
                .unwrap();
        for case in fixture.splines {
            let fitted = MonotoneSpline::fit(&case.x, &case.y);
            if !case.monotone {
                // The real CD19 oracle has a negative derivative after R's
                // adjacent corrections. Never silently publish that mapping.
                assert!(fitted.unwrap_err().to_string().contains("monotonicity"));
                continue;
            }
            let s = fitted.unwrap();
            for (actual, expected) in s.slopes.iter().zip(&case.slopes) {
                assert!((actual - expected).abs() <= 1e-10 + 1e-10 * expected.abs());
            }
            for (x, expected) in case.query.into_iter().zip(case.expected) {
                let actual = s.evaluate(x);
                assert!(
                    (actual - expected).abs() <= 1e-10 + 1e-10 * expected.abs(),
                    "x={x}: Rust={actual} R={expected}"
                );
            }
        }
        for case in fixture.quantiles {
            for (p, expected) in case.probabilities.into_iter().zip(case.expected) {
                let actual = cytiq_density::quantile_type7(&case.sorted, p);
                assert!((actual - expected).abs() <= 1e-10 + 1e-10 * expected.abs());
            }
        }
    }
    #[test]
    fn preserves_the_full_monotone_region_and_rejects_invalid_coefficients() {
        let s = MonotoneSpline::fit(&[0., 1., 2., 3.], &[0., 4., 5., 9.]).unwrap();
        assert_eq!(s.slopes, [4., 2.5, 2.5, 4.]);
        let mut malformed = s;
        malformed.slopes[1] = 10.0;
        assert!(malformed.validate().is_err());
    }

    #[test]
    fn stored_legacy_coefficients_are_validated_without_refitting() {
        let spline = MonotoneSpline {
            x: vec![0., 1., 2., 3.],
            y: vec![0., 4., 5., 9.],
            slopes: vec![4., 2.121_320_343_559_642_4, 2.121_320_343_559_642_4, 4.],
        };
        let stored = serde_json::to_string(&spline).unwrap();
        let loaded: MonotoneSpline = serde_json::from_str(&stored).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded.slopes, spline.slopes);
        assert!((loaded.evaluate(0.25) - 1.088_063_108_895_641_8).abs() < 1e-12);
    }

    #[test]
    fn nonfinite_queries_never_panic_and_overflow_is_rejected() {
        let s = MonotoneSpline::fit(&[0., 1.], &[0., 1.]).unwrap();
        for query in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(s.evaluate(query).is_nan());
        }
        for (x, y) in [
            ([-f64::MAX, f64::MAX], [0., 1.]),
            ([0., 1.], [-f64::MAX, f64::MAX]),
            ([0., f64::MIN_POSITIVE], [0., f64::MAX]),
        ] {
            assert!(MonotoneSpline::fit(&x, &y).is_err());
        }
    }
    #[test]
    fn interpolates_r_reference_with_linear_tails() {
        // R 4.5.3: splinefun(0:3,c(0,2,3,4),method="monoH.FC")(c(-1,0,.5,1,3,4))
        let s = MonotoneSpline::fit(&[0.0, 1.0, 2.0, 3.0], &[0.0, 2.0, 3.0, 4.0]).unwrap();
        for (x, y) in [
            (-1.0, -2.0),
            (0.0, 0.0),
            (0.5, 1.0625),
            (1.0, 2.0),
            (3.0, 4.0),
            (4.0, 5.0),
        ] {
            assert!((s.evaluate(x) - y).abs() < 1e-12);
        }
    }
    #[test]
    fn collapses_ties_and_handles_flat_segments() {
        let s = MonotoneSpline::fit(&[1.0, 1.0, 2.0], &[0.0, 2.0, 3.0]).unwrap();
        assert_eq!(s.y, vec![1.0, 3.0]);
        assert_eq!(s.evaluate(1.5), 2.0);
        assert!(MonotoneSpline::fit(&[1.0, 1.0], &[0.0, 2.0]).is_err());
        let s = MonotoneSpline::fit(&[0.0, 1.0, 2.0, 3.0], &[0.0, 0.0, 1.0, 1.0]).unwrap();
        let out = (0..301)
            .map(|i| s.evaluate(f64::from(i) / 100.0))
            .collect::<Vec<_>>();
        assert!(out.windows(2).all(|v| v[0] <= v[1]));
    }
}
