//! `cytiq qc`: check one sample's final event mask against its panel's QC
//! policy from the run manifest — raw and kept event counts, the overall
//! fraction removed, and per filter whether it ran and how much it removed.
//!
//! Masks are the common contract of the preprocessing stages, so the checks
//! read the combined mask (`cytiq mask combine`) instead of each stage's own
//! report.
//!
//! With independent artifact truth (`cytiq.qc.truth.v1`, [`truth`]) the
//! report also measures how well the masks removed labelled artifacts and
//! how many clean events they lost ([`artifacts`]). Like population
//! retention, this is descriptive: it never changes a check or the status.
//!
//! With the `preflight` feature, `preflight` checks run manifests and their
//! FCS inputs before computation. The feature is disabled by default.

pub mod artifacts;
mod error;
mod policy;
pub mod retention;
mod runner;
pub mod truth;

#[cfg(feature = "preflight")]
pub mod preflight;

pub use crate::artifacts::{ArtifactRemovalReport, artifact_removal};
pub use crate::error::QcError;
pub use crate::policy::{
    CheckStatus, FilterMetrics, QcCheck, QcMetrics, evaluate, evaluate_release,
};
pub use crate::runner::{QcConfig, QcReport, QcRunner};
pub use crate::truth::{ArtifactTruth, TruthError};
