//! `cytiq-peacoqc` implements a PeacoQC-compatible acquisition quality-control
//! workflow for cytometry FCS files.
//!
//! Primary reference:
//! <https://doi.org/10.1002/cyto.a.24501>
//!
//! Upstream R reference implementation:
//! <https://github.com/saeyslab/PeacoQC>

mod error;
mod export;
mod runner;

pub use crate::error::PeacoQcError;
pub use crate::runner::{
    DEFAULT_CONSECUTIVE_BINS, DEFAULT_FORCE_IT_BINS, DEFAULT_IT_LIMIT, DEFAULT_MAD_THRESHOLD,
    DEFAULT_MAX_BINS, DEFAULT_MIN_CELLS, DEFAULT_MIN_PEAK_BIN_PERCENT, DEFAULT_PEAK_REMOVAL,
    DEFAULT_STEP, DEFAULT_TIME_CHANNEL, EventMaskRecord, PeacoQcConfig, PeacoQcConstants,
    PeacoQcMethod, PeacoQcMetrics, PeacoQcParameters, PeacoQcResult, PeacoQcRunner, PeacoQcSummary,
    masks_path, parse_channel_list, peacoqc_mask, summary_path,
};

/// Upstream implementations this crate is validated against
/// (`parity/versions.txt`), recorded in provenance.
pub const UPSTREAM: &[(&str, &str)] = &[("PeacoQC", "1.20.0")];
