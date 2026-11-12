//! `cytiq-autospill` implements the core `AutoSpill` and `AutoSpread`
//! workflows for flow-cytometry compensation and unmixing.
//!
//! Primary reference:
//! <https://www.nature.com/articles/s41467-021-23126-8>
//!
//! Upstream R reference implementation:
//! <https://github.com/carlosproca/autospill>
//!
//! The goal of this crate is not to introduce Rust-specific tuning, but to keep
//! the same high-level stages and defaults visible in code:
//! FSC/SSC auto-gating, robust spillover estimation, linear-to-transformed
//! iterative refinement, matrix export, `AutoSpread` estimation, and optional
//! Plotly debug artifacts.

mod error;
mod export;
mod manifest;
mod plotting;
mod regression;
mod runner;
mod transform;

pub use crate::error::AutospillError;
pub use crate::export::{export_matrix, export_spread_matrix};
pub use crate::manifest::{
    ControlsManifestRow, DetectorSpec, TransformSpec, read_controls_manifest,
    read_detectors_manifest, read_transform_manifest,
};
pub use crate::runner::{
    AutospillConfig, AutospillResult, AutospillRunner, AutospreadConfig, AutospreadResult,
    AutospreadRunner, ConvergenceStep, ConvergenceSummary, GateStats, MatrixKind, SpreadFit,
    SpreadFitStatus, SpreadStats,
};

/// Upstream implementations this crate is validated against
/// (`parity/versions.txt`), recorded in provenance.
pub const UPSTREAM: &[(&str, &str)] = &[("autospill", "0.2.0@1e60e86"), ("MASS", "7.3.66")];
