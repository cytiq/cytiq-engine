//! Reference-based spectral unmixing for `cytiq autospectral`.

mod error;
mod export;
mod manifest;
mod runner;
mod spectrum;

pub use crate::error::AutoSpectralError;
pub use crate::export::{masks_path, write_outputs};
pub use crate::manifest::{
    ControlManifestRow, DetectorSpec, SampleManifestRow, read_controls_manifest,
    read_detectors_manifest, read_samples_manifest,
};
pub use crate::runner::{
    AF_COMPONENT_COUNT, AUTO_EXCLUDED_CHANNELS, AutoSpectralConfig, AutoSpectralRunner,
    AutoSpectralSummary, CONDITION_NUMBER_LIMIT, DEFAULT_FSC_CHANNEL, DEFAULT_SSC_CHANNEL,
    MIN_POSITIVE_EVENTS, MIN_RETAINED_EVENTS, NEGATIVE_EVENT_LIMIT, POSITIVE_EVENT_LIMIT,
    POSITIVITY_THRESHOLD, ReferenceDiagnostics, SCATTER_GATE_LOWER_QUANTILE,
    SCATTER_GATE_UPPER_QUANTILE, SOLVER, SOLVER_METHOD,
};
pub use crate::spectrum::{RLM_MAX_ITERATIONS, SCATTER_NEIGHBOURS};

/// Upstream implementations this crate is validated against
/// (`parity/versions.txt`), recorded in provenance.
pub const UPSTREAM: &[(&str, &str)] = &[("AutoSpectral", "1.8.7@292dfea")];
