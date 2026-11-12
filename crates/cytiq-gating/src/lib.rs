//! `cytiq-gating` evaluates `GatingML` 2.0 gating strategies on FCS events.
//!
//! This crate applies existing gate boundaries; it does not estimate them.
//! Strategies built in code can be serialized back with [`to_gatingml`].
//! Supported: rectangle (range/hyper-rectangle), polygon, ellipsoid and
//! Boolean gates, gate hierarchies, `FCS`/spectrum-matrix compensation and
//! the `flin`, `flog`, `fasinh`, `logicle` and `hyperlog` transformations.
//! Quadrant gates, `fratio` and new dimensions are rejected.
//!
//! Specification: <http://flowcyt.sourceforge.net/gating/latest.pdf>
//!
//! Reference implementation (results are checked against it):
//! <https://github.com/whitews/FlowKit>

mod error;
mod evaluate;
mod geometry;
mod model;
mod parse;
mod runner;
mod transforms;
mod write;

pub use crate::error::GatingError;
pub use crate::evaluate::{GateCount, GatingResults};
pub use crate::model::{
    BooleanOp, CompensationRef, Dimension, Gate, GateKind, GateRef, GatingStrategy, Quadrant,
    QuadrantDivider, QuadrantGate, SpectrumMatrix,
};
pub use crate::parse::{GATINGML_BYTES_MAX, parse_gatingml, read_gatingml};
pub use crate::runner::{GatingConfig, GatingRunResult, GatingRunner, OutputFormat};
pub use crate::transforms::Transform;
pub use crate::write::to_gatingml;

/// Upstream implementations this crate is validated against
/// (`parity/versions.txt`), recorded in provenance.
pub const UPSTREAM: &[(&str, &str)] = &[("Gating-ML", "2.0"), ("FlowKit", "1.3.0")];
