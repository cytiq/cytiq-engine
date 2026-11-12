//! Display scales ported from the reference cytometry implementations.
//!
//! - [`Logicle`]: Moore & Parks (2012) logicle, as in `flowutils`/`FlowKit`
//!   (`[0, 1]` scale) and `flowCore::logicleTransform` (the same curve times
//!   `M`).
//! - [`Hyperlog`]: Bagwell hyperlog as defined by Gating-ML 2.0, as in
//!   `flowCore::hyperlogtGml2` and `FlowKit`.
//! - [`FlowJoBiexp`]: `FlowJo` biexponential, as `flowWorkspace::flowjo_biexp`:
//!   cytolib's calibration table evaluated with R's natural cubic spline.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::manual_midpoint,
    clippy::suboptimal_flops
)]
// Reference ports keep the upstream arithmetic (exact float comparisons,
// `(lo + hi) / 2`) so results match the original implementations bit for bit.

use std::f64::consts::LN_10;

const TAYLOR_LENGTH: usize = 16;

/// Errors raised when scale parameters violate the reference constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScaleParameterError(pub String);

impl std::fmt::Display for ScaleParameterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScaleParameterError {}

fn check_logicle_parameters(t: f64, w: f64, m: f64, a: f64) -> Result<(), ScaleParameterError> {
    if t <= 0.0 {
        return Err(ScaleParameterError("T must be positive".to_owned()));
    }
    if w < 0.0 {
        return Err(ScaleParameterError("W must not be negative".to_owned()));
    }
    if m <= 0.0 {
        return Err(ScaleParameterError("M must be positive".to_owned()));
    }
    if 2.0 * w > m {
        return Err(ScaleParameterError(
            "W is too large (2W must be <= M)".to_owned(),
        ));
    }
    if -a > w || a + w > m - w {
        return Err(ScaleParameterError(
            "A is out of range (-W <= A <= M - 2W)".to_owned(),
        ));
    }
    Ok(())
}

/// `flowCore::estimateLogicle`'s width for one channel (`.lgclTrans`,
/// `type = "instrument"`): `w = (m - log10(t / |r|)) / 2` with
/// `r = .Machine$double.eps + quantile(x[x < 0], q)` (type 7), and `w = 0`
/// when no value is negative. flowCore passes `t = maxRange`, `m = 4.5`,
/// `q = 0.05`.
///
/// # Errors
///
/// Returns an error when `w` comes out negative, as flowCore does ("w is
/// negative! Try to increase 'm'").
pub fn estimate_logicle_width(
    values: &[f64],
    t: f64,
    m: f64,
    q: f64,
) -> Result<f64, ScaleParameterError> {
    let mut negative = values
        .iter()
        .copied()
        .filter(|value| *value < 0.0)
        .collect::<Vec<_>>();
    if negative.is_empty() {
        return Ok(0.0);
    }
    negative.sort_by(f64::total_cmp);
    let r = f64::EPSILON + cytiq_density::quantile_type7(&negative, q);
    let w = (m - (t / r.abs()).log10()) / 2.0;
    if w < 0.0 {
        return Err(ScaleParameterError(format!(
            "estimated logicle W is negative ({w}); increase M"
        )));
    }
    Ok(w)
}

/// Moore & Parks logicle with output on the `[0, 1]` Gating-ML scale.
#[derive(Clone, Debug, PartialEq)]
pub struct Logicle {
    /// `[T, W, M, A]` as given to [`Logicle::new`]; they determine every
    /// coefficient below and let the scale be written back losslessly.
    parameters: [f64; 4],
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    f: f64,
    x1: f64,
    x_taylor: f64,
    taylor: [f64; TAYLOR_LENGTH],
}

impl Logicle {
    /// # Errors
    ///
    /// Returns an error when the parameters violate the logicle constraints.
    pub fn new(t: f64, w: f64, m: f64, a: f64) -> Result<Self, ScaleParameterError> {
        check_logicle_parameters(t, w, m, a)?;
        let w_n = w / (m + a);
        let x2 = a / (m + a);
        let x1 = x2 + w_n;
        let x0 = x2 + 2.0 * w_n;
        let b = (m + a) * LN_10;
        let d = solve_logicle_d(b, w_n)?;
        let c_a = (x0 * (b + d)).exp();
        let mf_a = (b * x1).exp() - c_a / (d * x1).exp();
        let a_coef = t / ((b.exp() - mf_a) - c_a / d.exp());
        let c = c_a * a_coef;
        let f = -mf_a * a_coef;

        let mut taylor = [0.0; TAYLOR_LENGTH];
        let mut positive = a_coef * (b * x1).exp();
        let mut negative = -c / (d * x1).exp();
        for (index, coefficient) in taylor.iter_mut().enumerate() {
            positive *= b / (index as f64 + 1.0);
            negative *= -d / (index as f64 + 1.0);
            *coefficient = positive + negative;
        }
        // exact zero by the logicle condition
        taylor[1] = 0.0;

        Ok(Self {
            parameters: [t, w, m, a],
            a: a_coef,
            b,
            c,
            d,
            f,
            x1,
            x_taylor: x1 + w_n / 4.0,
            taylor,
        })
    }

    /// `[T, W, M, A]` as given to [`Self::new`].
    #[must_use]
    pub const fn parameters(&self) -> [f64; 4] {
        self.parameters
    }

    fn series(&self, scale: f64) -> f64 {
        let x = scale - self.x1;
        let mut sum = self.taylor[TAYLOR_LENGTH - 1] * x;
        for index in (2..=TAYLOR_LENGTH - 2).rev() {
            sum = (sum + self.taylor[index]) * x;
        }
        (sum * x + self.taylor[0]) * x
    }

    /// Forward transform onto `[0, 1]` (data value `T` maps to 1).
    #[must_use]
    pub fn scale(&self, value: f64) -> f64 {
        if value == 0.0 {
            return self.x1;
        }
        if !value.is_finite() {
            return value;
        }
        let negative = value < 0.0;
        let value = value.abs();
        let mut x = if value < self.f {
            self.x1 + value / self.taylor[0]
        } else {
            (value / self.a).ln() / self.b
        };
        let tolerance = 3.0 * f64::EPSILON * x.max(1.0);
        for _ in 0..20 {
            let ae2bx = self.a * (self.b * x).exp();
            let ce2mdx = self.c / (self.d * x).exp();
            let y = if x < self.x_taylor {
                self.series(x) - value
            } else {
                (ae2bx + self.f) - (ce2mdx + value)
            };
            let abe2bx = self.b * ae2bx;
            let cde2mdx = self.d * ce2mdx;
            let dy = abe2bx + cde2mdx;
            let ddy = self.b * abe2bx - self.d * cde2mdx;
            let delta = y / (dy * (1.0 - y * ddy / (2.0 * dy * dy)));
            x -= delta;
            if delta.abs() < tolerance {
                return if negative { 2.0 * self.x1 - x } else { x };
            }
        }
        f64::NAN
    }

    /// Inverse of [`Logicle::scale`].
    #[must_use]
    pub fn inverse(&self, scale: f64) -> f64 {
        let negative = scale < self.x1;
        let scale = if negative {
            2.0 * self.x1 - scale
        } else {
            scale
        };
        let value = if scale < self.x_taylor {
            self.series(scale)
        } else {
            (self.a * (self.b * scale).exp() + self.f) - self.c / (self.d * scale).exp()
        };
        if negative { -value } else { value }
    }
}

/// Parks' `solve`: root `d` of `2 ln d + w d = 2 ln b - w b`.
fn solve_logicle_d(b: f64, w: f64) -> Result<f64, ScaleParameterError> {
    if w == 0.0 {
        return Ok(b);
    }
    let tolerance = 2.0 * b * f64::EPSILON;
    let (mut low, mut high) = (0.0_f64, b);
    let mut d = (low + high) / 2.0;
    let mut last_delta = high - low;
    let f_b = -2.0 * b.ln() + w * b;
    let mut f = 2.0 * d.ln() + w * d + f_b;
    let mut last_f = f64::NAN;
    for _ in 0..20 {
        let df = 2.0 / d + w;
        let delta;
        if ((d - high) * df - f) * ((d - low) * df - f) >= 0.0
            || (1.9 * f).abs() > (last_delta * df).abs()
        {
            delta = (high - low) / 2.0;
            d = low + delta;
            if d == low {
                return Ok(d);
            }
        } else {
            delta = f / df;
            let previous = d;
            d -= delta;
            if d == previous {
                return Ok(d);
            }
        }
        if delta.abs() < tolerance {
            return Ok(d);
        }
        last_delta = delta;
        f = 2.0 * d.ln() + w * d + f_b;
        if f == 0.0 || f == last_f {
            return Ok(d);
        }
        last_f = f;
        if f < 0.0 {
            low = d;
        } else {
            high = d;
        }
    }
    Err(ScaleParameterError(
        "logicle parameter solve did not converge".to_owned(),
    ))
}

/// Gating-ML 2.0 hyperlog with output on the `[0, 1]` scale.
#[derive(Clone, Debug, PartialEq)]
pub struct Hyperlog {
    /// `[T, W, M, A]` as given to [`Hyperlog::new`].
    parameters: [f64; 4],
    a: f64,
    b: f64,
    c: f64,
    f: f64,
    w: f64,
    x0: f64,
    x1: f64,
    x_taylor: f64,
    inverse_x0: f64,
    taylor: [f64; TAYLOR_LENGTH],
}

impl Hyperlog {
    /// # Errors
    ///
    /// Returns an error when the parameters violate the hyperlog constraints.
    pub fn new(t: f64, w: f64, m: f64, a: f64) -> Result<Self, ScaleParameterError> {
        check_logicle_parameters(t, w, m, a)?;
        if w <= 0.0 {
            return Err(ScaleParameterError("hyperlog requires W > 0".to_owned()));
        }
        let w_n = w / (m + a);
        let x2 = a / (m + a);
        let x1 = x2 + w_n;
        let x0 = x2 + 2.0 * w_n;
        let b = (m + a) * LN_10;
        let e0 = (b * x0).exp();
        let c_a = e0 / w_n;
        let f_a = (b * x1).exp() + c_a * x1;
        let a_coef = t / ((b.exp() + c_a) - f_a);
        let c = c_a * a_coef;
        let f = f_a * a_coef;

        let mut taylor = [0.0; TAYLOR_LENGTH];
        let mut positive = a_coef * (b * x1).exp();
        for (index, coefficient) in taylor.iter_mut().enumerate() {
            positive *= b / (index as f64 + 1.0);
            *coefficient = positive;
        }
        taylor[0] += c;

        let mut hyperlog = Self {
            parameters: [t, w, m, a],
            a: a_coef,
            b,
            c,
            f,
            w: w_n,
            x0,
            x1,
            x_taylor: x1 + w_n / 4.0,
            inverse_x0: 0.0,
            taylor,
        };
        hyperlog.inverse_x0 = hyperlog.inverse(x0);
        Ok(hyperlog)
    }

    /// `[T, W, M, A]` as given to [`Self::new`].
    #[must_use]
    pub const fn parameters(&self) -> [f64; 4] {
        self.parameters
    }

    fn series(&self, scale: f64) -> f64 {
        let x = scale - self.x1;
        let mut sum = self.taylor[TAYLOR_LENGTH - 1] * x;
        for index in (0..=TAYLOR_LENGTH - 2).rev() {
            sum = (sum + self.taylor[index]) * x;
        }
        sum
    }

    /// Forward transform onto `[0, 1]` (data value `T` maps to 1).
    #[must_use]
    pub fn scale(&self, value: f64) -> f64 {
        if value == 0.0 {
            return self.x1;
        }
        if !value.is_finite() {
            return value;
        }
        let negative = value < 0.0;
        let value = value.abs();
        let mut x = if value < self.inverse_x0 {
            self.x1 + value * self.w / self.inverse_x0
        } else {
            (value / self.a).ln() / self.b
        };
        let tolerance = 3.0 * f64::EPSILON * x.max(1.0);
        for _ in 0..20 {
            let ae2bx = self.a * (self.b * x).exp();
            let y = if x < self.x_taylor {
                self.series(x) - value
            } else {
                (ae2bx + self.c * x) - (self.f + value)
            };
            let abe2bx = self.b * ae2bx;
            let dy = abe2bx + self.c;
            let ddy = self.b * abe2bx;
            let delta = y / (dy * (1.0 - y * ddy / (2.0 * dy * dy)));
            x -= delta;
            if delta.abs() < tolerance {
                return if negative { 2.0 * self.x1 - x } else { x };
            }
        }
        f64::NAN
    }

    /// Inverse of [`Hyperlog::scale`].
    #[must_use]
    pub fn inverse(&self, scale: f64) -> f64 {
        let negative = scale < self.x1;
        let scale = if negative {
            2.0 * self.x1 - scale
        } else {
            scale
        };
        let value = if scale < self.x_taylor {
            self.series(scale)
        } else {
            (self.a * (self.b * scale).exp() + self.c * scale) - self.f
        };
        if negative { -value } else { value }
    }
}

/// `FlowJo` biexponential as `flowWorkspace::flowjo_biexp(channelRange,
/// maxValue, pos, neg, widthBasis)`.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowJoBiexp {
    forward: NaturalSpline,
    inverse: NaturalSpline,
}

impl FlowJoBiexp {
    /// # Errors
    ///
    /// Returns an error when the parameters produce an invalid calibration
    /// table (cytolib raises `invalid zeroChan` in the same cases).
    pub fn new(
        channel_range: usize,
        max_value: f64,
        pos: f64,
        neg: f64,
        width_basis: f64,
    ) -> Result<Self, ScaleParameterError> {
        let (values, channels) =
            flowjo_calibration_table(channel_range, max_value, pos, neg, width_basis)?;
        Ok(Self {
            forward: NaturalSpline::new(&values, &channels),
            inverse: NaturalSpline::new(&channels, &values),
        })
    }

    /// Data value to channel in `[0, channel_range]`.
    #[must_use]
    pub fn scale(&self, value: f64) -> f64 {
        if value.is_finite() {
            self.forward.eval(value)
        } else {
            value
        }
    }

    /// Channel to data value.
    #[must_use]
    pub fn inverse(&self, channel: f64) -> f64 {
        if channel.is_finite() {
            self.inverse.eval(channel)
        } else {
            channel
        }
    }
}

/// cytolib `biexpTrans::computCalTbl` (translated from the `TreeStar` Java
/// routine), returning `(data values, channels)` with `channel_range + 1`
/// points. The `float` divisions match the C++ source.
fn flowjo_calibration_table(
    channel_range: usize,
    max_value: f64,
    pos: f64,
    neg: f64,
    width_basis: f64,
) -> Result<(Vec<f64>, Vec<f64>), ScaleParameterError> {
    let mut decades = pos;
    let mut width = (-width_basis).log10();
    if !(0.5..=3.0).contains(&width) {
        width = 0.5;
    }
    decades -= width / 2.0;
    let extra = neg.max(0.0) + width / 2.0;
    let channel_range_f = channel_range as f64;
    let zero_channel =
        ((extra * channel_range_f / (extra + decades)) as i64).min((channel_range / 2) as i64);
    if zero_channel > 0 {
        decades = extra * channel_range_f / zero_channel as f64;
    }
    width /= 2.0 * decades;

    let positive_range = LN_10 * decades;
    let minimum = max_value / positive_range.exp();
    let negative_range = flowjo_log_root(positive_range, width);

    let points = channel_range + 1;
    if zero_channel < 0 || zero_channel as usize >= points {
        return Err(ScaleParameterError(format!(
            "invalid zeroChan: {zero_channel}"
        )));
    }
    let zero_channel = zero_channel as usize;
    let mut positive = Vec::with_capacity(points);
    let mut negative = Vec::with_capacity(points);
    for index in 0..points {
        let fraction = f64::from(index as f32 / points as f32);
        positive.push((fraction * positive_range).exp());
        negative.push((fraction * -negative_range).exp());
    }
    let s = ((positive_range + negative_range) * (width + extra / decades)).exp();
    for value in &mut negative {
        *value *= s;
    }
    let s = positive[zero_channel] - negative[zero_channel];
    for index in zero_channel..points {
        positive[index] = minimum * (positive[index] - negative[index] - s);
    }
    for index in 0..zero_channel {
        let mirror = 2 * zero_channel - index;
        if mirror >= points {
            return Err(ScaleParameterError(format!(
                "invalid value from '2 * zeroChan - j': {mirror}"
            )));
        }
        positive[index] = -positive[mirror];
    }
    let channels = (0..points).map(|index| index as f64).collect();
    Ok((positive, channels))
}

/// cytolib `biexpTrans::logRoot`, including its integer truncations.
fn flowjo_log_root(b: f64, w: f64) -> f64 {
    let truncated_abs = |value: f64| (value as i64).abs() as f64;
    let mut low = 0.0;
    let mut high = b;
    let mut d = (low + high) / 2.0;
    let mut last_delta = truncated_abs(low - high);
    let f_b = -2.0 * b.ln() + w * b;
    let mut f = 2.0 * d.ln() + w * b + f_b;
    let mut df = 2.0 / d + w;
    if w == 0.0 {
        return b;
    }
    for _ in 0..100 {
        let delta;
        if ((d - high) * df - f) * ((d - low) * df - f) >= 0.0
            || truncated_abs(2.0 * f) > truncated_abs(last_delta * df)
        {
            delta = (high - low) / 2.0;
            d = low + delta;
            if d == low {
                return d;
            }
        } else {
            delta = f / df;
            let previous = d;
            d -= delta;
            if d == previous {
                return d;
            }
        }
        if truncated_abs(delta) < 1.0e-12 {
            return d;
        }
        last_delta = delta;
        f = 2.0 * d.ln() + w * d + f_b;
        df = 2.0 / d + w;
        if f < 0.0 {
            low = d;
        } else {
            high = d;
        }
    }
    d
}

/// R's `natural_spline` coefficients evaluated as `spline_eval(method = 2)`.
#[derive(Clone, Debug, PartialEq)]
struct NaturalSpline {
    x: Vec<f64>,
    y: Vec<f64>,
    b: Vec<f64>,
    c: Vec<f64>,
    d: Vec<f64>,
}

impl NaturalSpline {
    fn new(x: &[f64], y: &[f64]) -> Self {
        let n = x.len();
        let mut b = vec![0.0; n];
        let mut c = vec![0.0; n];
        let mut d = vec![0.0; n];
        if n < 3 {
            let slope = (y[1] - y[0]) / (x[1] - x[0]);
            return Self {
                x: x.to_vec(),
                y: y.to_vec(),
                b: vec![slope; n],
                c,
                d,
            };
        }
        // 0-based transcription of splines.c::natural_spline
        let nm1 = n - 1;
        d[0] = x[1] - x[0];
        c[1] = (y[1] - y[0]) / d[0];
        for i in 1..nm1 {
            d[i] = x[i + 1] - x[i];
            b[i] = 2.0 * (d[i - 1] + d[i]);
            c[i + 1] = (y[i + 1] - y[i]) / d[i];
            c[i] = c[i + 1] - c[i];
        }
        for i in 2..nm1 {
            let t = d[i - 1] / b[i - 1];
            b[i] -= t * d[i - 1];
            c[i] -= t * c[i - 1];
        }
        c[nm1 - 1] /= b[nm1 - 1];
        for i in (1..nm1 - 1).rev() {
            c[i] = (c[i] - d[i] * c[i + 1]) / b[i];
        }
        c[0] = 0.0;
        c[nm1] = 0.0;
        b[0] = (y[1] - y[0]) / d[0] - d[0] * c[1];
        d[0] = c[1] / d[0];
        b[nm1] = (y[nm1] - y[nm1 - 1]) / d[nm1 - 1] + d[nm1 - 1] * c[nm1 - 1];
        for i in 1..nm1 {
            b[i] = (y[i + 1] - y[i]) / d[i] - d[i] * (c[i + 1] + 2.0 * c[i]);
            d[i] = (c[i + 1] - c[i]) / d[i];
            c[i] *= 3.0;
        }
        c[nm1] = 0.0;
        d[nm1] = 0.0;
        Self {
            x: x.to_vec(),
            y: y.to_vec(),
            b,
            c,
            d,
        }
    }

    fn eval(&self, value: f64) -> f64 {
        let n = self.x.len();
        // binary search for x[i] <= value <= x[i + 1] (i = 0 below the range)
        let mut i = 0;
        let mut j = n;
        while j > i + 1 {
            let k = (i + j) / 2;
            if value < self.x[k] {
                j = k;
            } else {
                i = k;
            }
        }
        let dx = value - self.x[i];
        // natural splines extrapolate linearly to the left
        let cubic = if value < self.x[0] { 0.0 } else { self.d[i] };
        self.y[i] + dx * (self.b[i] + dx * (self.c[i] + dx * cubic))
    }
}

#[cfg(test)]
#[allow(clippy::unreadable_literal, clippy::excessive_precision)]
mod tests {
    use super::*;

    include!("../../../data/fixtures/transform/flowcore_reference.rs");
    include!("../../../data/fixtures/transform/estimate_logicle_reference.rs");

    #[test]
    fn logicle_width_matches_flowcore_estimate_logicle() {
        let w = estimate_logicle_width(&ESTIMATE_LOGICLE_VALUES, 262_144.0, 4.5, 0.05)
            .expect("width is positive");
        assert!(
            (w - ESTIMATE_LOGICLE_W).abs() < 1e-12,
            "{w} vs {ESTIMATE_LOGICLE_W}"
        );
    }

    #[test]
    fn logicle_width_is_zero_without_negative_values() {
        assert_eq!(
            estimate_logicle_width(&[0.0, 5.0, 1e4], 262_144.0, 4.5, 0.05),
            Ok(0.0)
        );
    }

    #[test]
    fn logicle_width_rejects_wide_negative_tails() {
        // |r| = 1 needs w = (4.5 - log10(262144)) / 2 < 0
        assert!(estimate_logicle_width(&[-1.0, 10.0], 262_144.0, 4.5, 0.05).is_err());
    }

    #[test]
    fn logicle_matches_flowcore_logicle_transform() {
        let logicle = Logicle::new(262_144.0, 0.5, 4.5, 0.0).expect("valid parameters");
        for (value, expected) in R_INPUT.iter().zip(R_LOGICLE) {
            let actual = 4.5 * logicle.scale(*value);
            assert!(
                (actual - expected).abs() < 1e-9,
                "x = {value}: {actual} vs {expected}"
            );
            let round_trip = logicle.inverse(actual / 4.5);
            assert!((round_trip - value).abs() <= 1e-8 * value.abs().max(1.0));
        }
    }

    #[test]
    fn hyperlog_matches_flowcore_hyperlog_gml2() {
        let hyperlog = Hyperlog::new(262_144.0, 0.5, 4.5, 0.0).expect("valid parameters");
        for (value, expected) in R_INPUT.iter().zip(R_HYPERLOG) {
            let actual = hyperlog.scale(*value);
            assert!(
                (actual - expected).abs() < 1e-9,
                "x = {value}: {actual} vs {expected}"
            );
            let round_trip = hyperlog.inverse(actual);
            assert!((round_trip - value).abs() <= 1e-8 * value.abs().max(1.0));
        }
    }

    #[test]
    fn flowjo_biexp_matches_flowworkspace() {
        let biexp = FlowJoBiexp::new(256, 262_144.0, 4.418_539_922, 0.0, -100.0).expect("valid");
        for (value, expected) in R_INPUT.iter().zip(R_BIEXP_256) {
            let actual = biexp.scale(*value);
            assert!(
                (actual - expected).abs() < 1e-9,
                "x = {value}: {actual} vs {expected}"
            );
        }
        for (channel, expected) in R_CHANNELS.iter().zip(R_BIEXP_256_INVERSE) {
            let actual = biexp.inverse(*channel);
            assert!(
                (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0),
                "channel = {channel}: {actual} vs {expected}"
            );
        }
        let biexp = FlowJoBiexp::new(4096, 262_144.0, 4.5, 0.0, -10.0).expect("valid");
        for (value, expected) in R_INPUT.iter().zip(R_BIEXP_4096) {
            let actual = biexp.scale(*value);
            assert!(
                (actual - expected).abs() < 1e-8,
                "x = {value}: {actual} vs {expected}"
            );
        }
    }
}
