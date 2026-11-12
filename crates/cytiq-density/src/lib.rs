//! `cytiq-density` holds the density-estimation and scatter auto-gating
//! primitives shared by the `cytiq` workflow crates.
//!
//! - [`density`]: port of R's `stats::density.default` (Gaussian kernel,
//!   `bw.nrd0`, 512 points), as used by `PeacoQC`
//!   (<https://github.com/saeyslab/PeacoQC>).
//! - [`smooth_spline_fitted`], [`smooth_spline_fitted_xy`]: port of R's
//!   `stats::smooth.spline(spar = ...)` fitted values.
//! - [`degate_upper_cuts`]: `flowDensity::deGate(all.cuts = TRUE, upper = TRUE)`
//!   (<https://bioconductor.org/packages/flowDensity>), as used by `CompensAID`.
//! - [`gate_events`]: the `AutoSpill` FSC/SSC auto-gate
//!   (<https://www.nature.com/articles/s41467-021-23126-8>).
//! - [`find_valleys`]: density valleys separating the populations of a 1-D
//!   kernel density estimate.
//! - [`sample_indices`]: deterministic, evenly spaced event subsampling.

mod degate;
mod kde;
mod sampling;
mod scatter_gate;
mod smooth_spline;
mod valley;

pub use crate::degate::{DEFAULT_TINYPEAK_REMOVAL, DeGate, degate_upper_cuts};
pub use crate::kde::{Density, bandwidth_nrd0, density, quantile_type7};
pub use crate::sampling::sample_indices;
pub use crate::scatter_gate::{
    GateComputation, SpreadSource, convex_hull, gate_events, mad, median, median_mut,
    point_in_polygon, robust_spread,
};
pub use crate::smooth_spline::{smooth_spline_fitted, smooth_spline_fitted_xy};
pub use crate::valley::{
    DEFAULT_MAX_VALLEY_RATIO, DEFAULT_MIN_PEAK_FRACTION, Valley, find_valleys,
};
