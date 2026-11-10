//! `cytiq qc`: check one sample's final event mask against its panel's QC
//! policy from the run manifest — raw and kept event counts, the overall
//! fraction removed, and per filter whether it ran and how much it removed.
//!
//! Masks are the common contract of the preprocessing stages, so the checks
//! read the combined mask (`cytiq mask combine`) instead of each stage's own
//! report.
//!
//! With the `preflight` feature, `preflight` checks run manifests and their
//! FCS inputs before computation. The feature is disabled by default.

mod error;
mod policy;
pub mod retention;
mod runner;

#[cfg(feature = "preflight")]
pub mod preflight;

pub use crate::error::QcError;
pub use crate::policy::{
    CheckStatus, FilterMetrics, QcCheck, QcMetrics, evaluate, evaluate_release,
};
pub use crate::runner::{QcConfig, QcReport, QcRunner};
