#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::HashSet;

use crate::kde::quantile_type7;

// Gating defaults are intentionally centralized here because they are part of
// the published `AutoSpill` behavior rather than ad-hoc implementation knobs.
//
// Paper:
// https://www.nature.com/articles/s41467-021-23126-8
//
// Matching R defaults live in `get_autospill_param_minimal.r` in the upstream
// `autospill` package. When changing any value below, treat it as an algorithm
// compatibility change, not a local refactor.

// The first pass trims away the outer 1% tails on FSC/SSC before any density
// estimate. This mirrors the paper/R workflow and keeps extreme debris from
// dominating the coarse KDE used to locate the main population.
const TRIM_X_MIN: f64 = 0.01;
const TRIM_X_MAX: f64 = 0.99;
const TRIM_Y_MIN: f64 = 0.01;
const TRIM_Y_MAX: f64 = 0.99;

// First-pass KDE / maxima search. This is the coarse grid used to find density
// peaks before assigning points to the peak-specific regions.
const BOUND_BW_FACTOR: f64 = 3.0;
const BOUND_GRID: usize = 100;
const BOUND_NEIGHBORHOOD: usize = 3;

// The first tessellation ignores peaks in the lower-left corner of the FSC/SSC
// space because these are often debris or acquisition artefacts rather than the
// target cell/bead population.
const LOWER_LEFT_EXCLUSION: f64 = 0.05;

// After the first peak is chosen, the paper narrows the search region around
// that peak using median +/- 3 * MAD on FSC and SSC independently.
const REGION_MAD_FACTOR: f64 = 3.0;

// Second-pass KDE / maxima search inside the MAD-bounded region. This pass
// separates close local peaks that the first coarse partition may have merged.
const REGION_BW_FACTOR: f64 = 2.0;
const REGION_GRID: usize = 100;
const REGION_NEIGHBORHOOD: usize = 2;

// Final local density estimate used to keep only the dense interior of the
// selected population before wrapping it in a convex hull.
const FINAL_BW_FACTOR: f64 = 1.0;
const FINAL_GRID: usize = 100;

// The final threshold is taken at 33% of the local density range, matching the
// default used by the reference implementation.
const DENSITY_THRESHOLD: f64 = 0.33;

/// Result of [`gate_events`]: the events inside the final convex-hull gate.
#[derive(Clone, Debug)]
pub struct GateComputation {
    pub gated_indices: Vec<usize>,
    /// Convex hull vertices of the dense core, in input coordinates.
    pub hull: Vec<[f64; 2]>,
    pub bound_maxima: usize,
    pub region_maxima: usize,
    pub strict_points: usize,
}

/// `AutoSpill` FSC/SSC auto-gate: two-pass KDE peak search that ignores
/// lower-left debris, followed by a convex hull around the dense core.
///
/// # Errors
///
/// Returns a static reason when the scatter cloud is too small or degenerate.
#[allow(clippy::too_many_lines)]
pub fn gate_events(
    points: &[[f64; 2]],
    data_max: Option<f64>,
) -> Result<GateComputation, &'static str> {
    if points.len() < 3 {
        return Err("not enough FSC/SSC events");
    }

    // do.gate: clamp the scatter range to [expr.data.min = 0, expr.data.max = max($P1R)]
    let clamp = |(low, high): (f64, f64)| {
        (
            low.max(0.0),
            data_max.map_or(high, |maximum| high.min(maximum)),
        )
    };
    let (x_min, x_max) = range(points.iter().map(|point| point[0]))
        .map(clamp)
        .ok_or("missing FSC values")?;
    let (y_min, y_max) = range(points.iter().map(|point| point[1]))
        .map(clamp)
        .ok_or("missing SSC values")?;
    if (x_max - x_min).abs() <= f64::EPSILON || (y_max - y_min).abs() <= f64::EPSILON {
        return Err("scatter values are constant");
    }

    // Step 1: trim the scatter domain before the first coarse density estimate.
    let trimmed_bounds = Bounds {
        x_low: (1.0 - TRIM_X_MIN) * x_min + TRIM_X_MIN * x_max,
        x_high: (1.0 - TRIM_X_MAX) * x_min + TRIM_X_MAX * x_max,
        y_low: (1.0 - TRIM_Y_MIN) * y_min + TRIM_Y_MIN * y_max,
        y_high: (1.0 - TRIM_Y_MAX) * y_min + TRIM_Y_MAX * y_max,
    };

    let bound_indices = points
        .iter()
        .enumerate()
        .filter_map(|(index, point)| {
            if point[0] > trimmed_bounds.x_low
                && point[0] < trimmed_bounds.x_high
                && point[1] > trimmed_bounds.y_low
                && point[1] < trimmed_bounds.y_high
            {
                Some(index)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if bound_indices.len() < 3 {
        return Err("trimmed FSC/SSC region is empty");
    }

    let bound_points = bound_indices
        .iter()
        .map(|&index| points[index])
        .collect::<Vec<_>>();
    // Step 2: coarse KDE + maxima search over the trimmed bounds. The maxima
    // play the role of tessellation seeds, so the exact density values matter
    // less than finding the right dominant peak ordering.
    let bound_density = DensityGrid::from_points(&bound_points, BOUND_BW_FACTOR, BOUND_GRID);
    let bound_maxima = bound_density.local_maxima(BOUND_NEIGHBORHOOD);
    let bound_target =
        select_target_peak(&bound_maxima, trimmed_bounds).ok_or("failed to find a density peak")?;
    let bound_cluster = select_cluster_points(&bound_points, &bound_maxima, bound_target);
    if bound_cluster.len() < 3 {
        return Err("tessellation did not isolate a valid FSC/SSC region");
    }

    let region_x = bound_cluster
        .iter()
        .map(|&index| bound_points[index][0])
        .collect::<Vec<_>>();
    let region_y = bound_cluster
        .iter()
        .map(|&index| bound_points[index][1])
        .collect::<Vec<_>>();
    let median_x = median(&region_x);
    let median_y = median(&region_y);
    let mad_x = mad(&region_x, median_x);
    let mad_y = mad(&region_y, median_y);
    // Step 3: build the second-pass search window from median +/- 3 * MAD
    // around the events assigned to the first selected peak.
    let region_bounds = Bounds {
        x_low: (median_x - REGION_MAD_FACTOR * mad_x).max(trimmed_bounds.x_low),
        x_high: (median_x + REGION_MAD_FACTOR * mad_x).min(trimmed_bounds.x_high),
        y_low: (median_y - REGION_MAD_FACTOR * mad_y).max(trimmed_bounds.y_low),
        y_high: (median_y + REGION_MAD_FACTOR * mad_y).min(trimmed_bounds.y_high),
    };
    if region_bounds.x_low >= region_bounds.x_high || region_bounds.y_low >= region_bounds.y_high {
        return Err("automatic FSC/SSC region collapsed");
    }

    let region_indices = points
        .iter()
        .enumerate()
        .filter_map(|(index, point)| {
            if point[0] > region_bounds.x_low
                && point[0] < region_bounds.x_high
                && point[1] > region_bounds.y_low
                && point[1] < region_bounds.y_high
            {
                Some(index)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if region_indices.len() < 3 {
        return Err("refined FSC/SSC region is empty");
    }

    let region_points = region_indices
        .iter()
        .map(|&index| points[index])
        .collect::<Vec<_>>();
    // Step 4: rerun density estimation inside the narrowed region. The second
    // tessellation matters when two nearby populations survived the first pass.
    let region_density = DensityGrid::from_points(&region_points, REGION_BW_FACTOR, REGION_GRID);
    let region_maxima = region_density.local_maxima(REGION_NEIGHBORHOOD);
    if region_maxima.is_empty() {
        return Err("refined FSC/SSC tessellation found no maxima");
    }
    let region_cluster = select_cluster_points(&region_points, &region_maxima, 0);
    if region_cluster.len() < 3 {
        return Err("refined FSC/SSC tessellation isolated too few points");
    }

    let strict_source = region_cluster
        .iter()
        .map(|&index| region_points[index])
        .collect::<Vec<_>>();
    if strict_source.is_empty() {
        return Err("strict FSC/SSC region is empty");
    }
    let final_density = DensityGrid::from_points(&strict_source, FINAL_BW_FACTOR, FINAL_GRID);
    let densities = strict_source
        .iter()
        .map(|point| final_density.interpolate(point[0], point[1]))
        .collect::<Vec<_>>();
    let density_min = densities
        .iter()
        .copied()
        .reduce(f64::min)
        .ok_or("strict FSC/SSC density is empty")?;
    let density_max = densities
        .iter()
        .copied()
        .reduce(f64::max)
        .ok_or("strict FSC/SSC density is empty")?;
    // Step 5: keep the dense interior at 33% of the local density range. Using
    // a relative threshold instead of an absolute density makes the gate track
    // the local shape of the chosen population.
    let density_threshold =
        (1.0 - DENSITY_THRESHOLD) * density_min + DENSITY_THRESHOLD * density_max;

    let mut seen = HashSet::new();
    let strict_points = strict_source
        .iter()
        .zip(&densities)
        .filter_map(|(point, density)| {
            if *density > density_threshold && seen.insert((point[0].to_bits(), point[1].to_bits()))
            {
                Some(*point)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if strict_points.len() < 3 {
        return Err("density threshold left too few gated points");
    }

    // Step 6: the strict interior points define the final polygon via a convex
    // hull. This turns the dense core into an inclusive gate over the full
    // event cloud, which is how the paper/R workflow presents the final gate.
    let hull = convex_hull(&strict_points);
    if hull.len() < 3 {
        return Err("convex hull for FSC/SSC gate is degenerate");
    }

    let gated_indices = points
        .iter()
        .enumerate()
        .filter_map(|(index, point)| {
            if point_in_polygon(*point, &hull) {
                Some(index)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if gated_indices.is_empty() {
        return Err("final FSC/SSC gate is empty");
    }

    Ok(GateComputation {
        gated_indices,
        hull,
        bound_maxima: bound_maxima.len(),
        region_maxima: region_maxima.len(),
        strict_points: strict_points.len(),
    })
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    x_low: f64,
    x_high: f64,
    y_low: f64,
    y_high: f64,
}

#[derive(Clone, Copy, Debug)]
struct Peak {
    x: f64,
    y: f64,
    density: f64,
}

#[derive(Clone, Debug)]
struct DensityGrid {
    x: Vec<f64>,
    y: Vec<f64>,
    z: Vec<f64>,
    width: usize,
    height: usize,
}

impl DensityGrid {
    /// `MASS::kde2d(x, y, h = factor * bandwidth.nrd, n = grid_size)`: the grid
    /// spans the points' own range and the Gaussian sd is `h / 4`.
    fn from_points(points: &[[f64; 2]], bandwidth_factor: f64, grid_size: usize) -> Self {
        let bounds = bounds_from_points(points).unwrap_or(Bounds {
            x_low: 0.0,
            x_high: 0.0,
            y_low: 0.0,
            y_high: 0.0,
        });
        let x_values = points.iter().map(|point| point[0]).collect::<Vec<_>>();
        let y_values = points.iter().map(|point| point[1]).collect::<Vec<_>>();
        let bandwidth_x =
            (bandwidth_factor * bandwidth_nrd(&x_values) / 4.0).max(f64::MIN_POSITIVE);
        let bandwidth_y =
            (bandwidth_factor * bandwidth_nrd(&y_values) / 4.0).max(f64::MIN_POSITIVE);
        let x = linspace(bounds.x_low, bounds.x_high, grid_size);
        let y = linspace(bounds.y_low, bounds.y_high, grid_size);
        let mut z = vec![0.0; grid_size * grid_size];

        for (row_index, y_value) in y.iter().enumerate() {
            for (column_index, x_value) in x.iter().enumerate() {
                let density = points.iter().fold(0.0, |accumulator, point| {
                    let dx = (x_value - point[0]) / bandwidth_x;
                    let dy = (y_value - point[1]) / bandwidth_y;
                    accumulator + (-0.5 * (dx * dx + dy * dy)).exp()
                });
                z[row_index * grid_size + column_index] = density;
            }
        }

        Self {
            x,
            y,
            z,
            width: grid_size,
            height: grid_size,
        }
    }

    fn local_maxima(&self, neighborhood: usize) -> Vec<Peak> {
        let mut peaks = Vec::new();
        for row in 0..self.height {
            for column in 0..self.width {
                let candidate = self.value(column, row);
                let x_start = column.saturating_sub(neighborhood);
                let x_end = (column + neighborhood).min(self.width - 1);
                let y_start = row.saturating_sub(neighborhood);
                let y_end = (row + neighborhood).min(self.height - 1);
                let mut is_maximum = true;
                'outer: for neighbor_row in y_start..=y_end {
                    for neighbor_column in x_start..=x_end {
                        if neighbor_row == row && neighbor_column == column {
                            continue;
                        }
                        if self.value(neighbor_column, neighbor_row) > candidate {
                            is_maximum = false;
                            break 'outer;
                        }
                    }
                }
                if is_maximum {
                    peaks.push(Peak {
                        x: self.x[column],
                        y: self.y[row],
                        density: candidate,
                    });
                }
            }
        }

        if peaks.is_empty() {
            let (index, density) = self
                .z
                .iter()
                .copied()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(&right.1))
                .expect("density grid is never empty");
            let row = index / self.width;
            let column = index % self.width;
            peaks.push(Peak {
                x: self.x[column],
                y: self.y[row],
                density,
            });
        }

        peaks.sort_by(|left, right| right.density.total_cmp(&left.density));
        peaks
    }

    fn interpolate(&self, x_value: f64, y_value: f64) -> f64 {
        let x_position = locate_segment(&self.x, x_value);
        let y_position = locate_segment(&self.y, y_value);
        let x0 = self.x[x_position];
        let x1 = self.x[x_position + 1];
        let y0 = self.y[y_position];
        let y1 = self.y[y_position + 1];
        let tx = if (x1 - x0).abs() <= f64::EPSILON {
            0.0
        } else {
            (x_value - x0) / (x1 - x0)
        };
        let ty = if (y1 - y0).abs() <= f64::EPSILON {
            0.0
        } else {
            (y_value - y0) / (y1 - y0)
        };
        let z00 = self.value(x_position, y_position);
        let z10 = self.value(x_position + 1, y_position);
        let z01 = self.value(x_position, y_position + 1);
        let z11 = self.value(x_position + 1, y_position + 1);

        (1.0 - tx) * (1.0 - ty) * z00
            + tx * (1.0 - ty) * z10
            + (1.0 - tx) * ty * z01
            + tx * ty * z11
    }

    fn value(&self, x_index: usize, y_index: usize) -> f64 {
        self.z[y_index * self.width + x_index]
    }
}

fn select_target_peak(peaks: &[Peak], bounds: Bounds) -> Option<usize> {
    let x_range = (bounds.x_high - bounds.x_low).max(f64::EPSILON);
    let y_range = (bounds.y_high - bounds.y_low).max(f64::EPSILON);
    peaks.iter().position(|peak| {
        ((peak.x - bounds.x_low) / x_range) > LOWER_LEFT_EXCLUSION
            || ((peak.y - bounds.y_low) / y_range) > LOWER_LEFT_EXCLUSION
    })
}

fn select_cluster_points(points: &[[f64; 2]], peaks: &[Peak], target_peak: usize) -> Vec<usize> {
    let target = peaks[target_peak];
    points
        .iter()
        .enumerate()
        .filter_map(|(index, point)| {
            let closest = peaks
                .iter()
                .enumerate()
                .min_by(|left, right| {
                    distance_squared(*point, left.1).total_cmp(&distance_squared(*point, right.1))
                })
                .map(|(peak_index, _)| peak_index)
                .expect("peaks are guaranteed to be non-empty");
            if closest == target_peak && distance_squared(*point, &target).is_finite() {
                Some(index)
            } else {
                None
            }
        })
        .collect()
}

fn distance_squared(point: [f64; 2], peak: &Peak) -> f64 {
    let dx = point[0] - peak.x;
    let dy = point[1] - peak.y;
    dx * dx + dy * dy
}

/// Andrew's monotone chain convex hull (counter-clockwise, no repeated end).
#[must_use]
pub fn convex_hull(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut sorted = points.to_vec();
    sorted.sort_by(|left, right| {
        left[0]
            .total_cmp(&right[0])
            .then_with(|| left[1].total_cmp(&right[1]))
    });
    sorted.dedup_by(|left, right| {
        left[0].to_bits() == right[0].to_bits() && left[1].to_bits() == right[1].to_bits()
    });
    if sorted.len() <= 1 {
        return sorted;
    }

    let mut lower = Vec::new();
    for point in &sorted {
        while lower.len() >= 2
            && cross(lower[lower.len() - 2], lower[lower.len() - 1], *point) <= 0.0
        {
            lower.pop();
        }
        lower.push(*point);
    }

    let mut upper = Vec::new();
    for point in sorted.iter().rev() {
        while upper.len() >= 2
            && cross(upper[upper.len() - 2], upper[upper.len() - 1], *point) <= 0.0
        {
            upper.pop();
        }
        upper.push(*point);
    }

    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Even-odd point-in-polygon test; points on an edge count as inside.
#[must_use]
pub fn point_in_polygon(point: [f64; 2], polygon: &[[f64; 2]]) -> bool {
    if polygon.len() < 3 {
        return false;
    }

    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        if point_on_segment(point, previous, current) {
            return true;
        }
        let intersects = ((current[1] > point[1]) != (previous[1] > point[1]))
            && (point[0]
                < (previous[0] - current[0]) * (point[1] - current[1])
                    / (previous[1] - current[1])
                    + current[0]);
        if intersects {
            inside = !inside;
        }
        previous = current;
    }

    inside
}

fn point_on_segment(point: [f64; 2], start: [f64; 2], end: [f64; 2]) -> bool {
    let area = cross(start, end, point).abs();
    if area > 1e-9 {
        return false;
    }
    let x_min = start[0].min(end[0]) - 1e-9;
    let x_max = start[0].max(end[0]) + 1e-9;
    let y_min = start[1].min(end[1]) - 1e-9;
    let y_max = start[1].max(end[1]) + 1e-9;
    point[0] >= x_min && point[0] <= x_max && point[1] >= y_min && point[1] <= y_max
}

fn cross(origin: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - origin[0]) * (b[1] - origin[1]) - (a[1] - origin[1]) * (b[0] - origin[0])
}

fn bounds_from_points(points: &[[f64; 2]]) -> Option<Bounds> {
    let (x_low, x_high) = range(points.iter().map(|point| point[0]))?;
    let (y_low, y_high) = range(points.iter().map(|point| point[1]))?;
    Some(Bounds {
        x_low,
        x_high,
        y_low,
        y_high,
    })
}

fn range(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    values.fold(None, |current, value| match current {
        None => Some((value, value)),
        Some((low, high)) => Some((low.min(value), high.max(value))),
    })
}

fn bandwidth_nrd(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 1.0;
    }
    let standard_deviation = standard_deviation(values);
    let interquartile = quantile(values, 0.75) - quantile(values, 0.25);
    let scale = standard_deviation.min(interquartile / 1.34).max(1e-6);
    4.0 * 1.06 * scale * (values.len() as f64).powf(-0.2)
}

fn standard_deviation(values: &[f64]) -> f64 {
    let mean = values.iter().sum::<f64>() / (values.len() as f64);
    let variance = values
        .iter()
        .map(|value| {
            let centered = value - mean;
            centered * centered
        })
        .sum::<f64>()
        / ((values.len() - 1) as f64);
    variance.sqrt()
}

fn quantile(values: &[f64], probability: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let position = probability * ((sorted.len() - 1) as f64);
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let fraction = position - (lower as f64);
        sorted[lower] * (1.0 - fraction) + sorted[upper] * fraction
    }
}

fn linspace(start: f64, end: f64, count: usize) -> Vec<f64> {
    if count <= 1 {
        return vec![start];
    }
    let step = (end - start) / ((count - 1) as f64);
    (0..count)
        .map(|index| start + (index as f64) * step)
        .collect()
}

fn locate_segment(axis: &[f64], value: f64) -> usize {
    if value <= axis[0] {
        return 0;
    }
    if value >= axis[axis.len() - 1] {
        return axis.len() - 2;
    }
    match axis.binary_search_by(|candidate| candidate.total_cmp(&value)) {
        Ok(index) => index.min(axis.len() - 2),
        Err(index) => index.saturating_sub(1).min(axis.len() - 2),
    }
}

/// Median of unsorted values.
///
/// # Panics
///
/// When `values` is empty; callers choose what an empty input means.
#[must_use]
pub fn median(values: &[f64]) -> f64 {
    median_mut(&mut values.to_vec())
}

/// [`median`] of values the caller no longer needs in order: sorts them in
/// place instead of copying.
///
/// # Panics
///
/// When `values` is empty.
pub fn median_mut(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        f64::midpoint(values[middle - 1], values[middle])
    } else {
        values[middle]
    }
}

/// R's `mad`: `1.4826 * median(|x - center|)`.
#[must_use]
pub fn mad(values: &[f64], center: f64) -> f64 {
    let deviations = values
        .iter()
        .map(|value| (value - center).abs())
        .collect::<Vec<_>>();
    1.4826 * median(&deviations)
}

/// Which estimate [`robust_spread`] returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpreadSource {
    /// `mad`, the normal case.
    Mad,
    /// `IQR / 1.349`: more than half of the values equal the center, as with
    /// heavily discretised data or beads.
    Iqr,
    /// A small fraction of `|center|`: the values are (nearly) constant.
    Relative,
}

impl SpreadSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mad => "mad",
            Self::Iqr => "iqr",
            Self::Relative => "relative",
        }
    }
}

/// A spread that is never zero, so `center ± k·spread` gates never collapse.
/// Falls back from the MAD to `IQR / 1.349` (equal to the standard deviation
/// for normal data, like the MAD), then to `1e-3·|center|`, then to a few
/// ULPs.
#[must_use]
pub fn robust_spread(values: &[f64], center: f64) -> (f64, SpreadSource) {
    let spread = mad(values, center);
    if spread > 0.0 {
        return (spread, SpreadSource::Mad);
    }
    if values.len() > 1 {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let iqr = quantile_type7(&sorted, 0.75) - quantile_type7(&sorted, 0.25);
        if iqr > 0.0 {
            return (iqr / 1.349, SpreadSource::Iqr);
        }
    }
    let relative = 1.0e-3 * center.abs();
    if relative > 0.0 {
        (relative, SpreadSource::Relative)
    } else {
        (f64::EPSILON * center.abs().max(1.0), SpreadSource::Relative)
    }
}

#[cfg(test)]
mod tests {
    use super::{SpreadSource, gate_events, robust_spread};

    #[test]
    fn robust_spread_falls_back_when_mad_is_zero() {
        let normal = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(robust_spread(&normal, 3.0).1, SpreadSource::Mad);

        // 60% of the values sit on the median: MAD = 0, IQR > 0.
        let discrete = [2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 1.0, 3.0, 4.0, 5.0];
        let (spread, source) = robust_spread(&discrete, 2.0);
        assert_eq!(source, SpreadSource::Iqr);
        assert!(spread > 0.0);

        let constant = [7.0; 10];
        assert_eq!(
            robust_spread(&constant, 7.0),
            (7.0e-3, SpreadSource::Relative)
        );
        let (spread, source) = robust_spread(&[0.0; 10], 0.0);
        assert_eq!(source, SpreadSource::Relative);
        assert!(spread > 0.0);
    }

    #[test]
    fn gate_events_prefers_main_cloud_over_lower_left_debris() {
        let mut points = Vec::new();
        for index in 0..120 {
            points.push([
                900.0 + f64::from(index % 10) * 12.0,
                1000.0 + f64::from(index / 10) * 9.0,
            ]);
        }
        for index in 0..40 {
            points.push([20.0 + f64::from(index), 25.0 + f64::from(index % 7)]);
        }

        let gate = gate_events(&points, None).expect("gate should succeed");
        let retained_main = gate
            .gated_indices
            .iter()
            .filter(|index| **index < 120)
            .count();
        // autospill:::do.gate keeps 27 lattice points and no debris here; the
        // regular lattice produces exactly tied KDE values, so the retained
        // count depends on floating-point tie-breaking and is not asserted.
        assert!(retained_main >= 20);
        assert_eq!(retained_main, gate.gated_indices.len());
        assert!(gate.bound_maxima >= 1);
        assert!(gate.region_maxima >= 1);
    }
}
