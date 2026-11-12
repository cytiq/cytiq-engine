//! `cytiq-flowsom` implements a deterministic FlowSOM-style clustering
//! workflow for cytometry FCS files.
//!
//! Primary reference:
//! <https://doi.org/10.1002/cyto.a.22625>
//!
//! Upstream R reference implementation:
//! <https://github.com/saeyslab/FlowSOM>

mod error;
mod export;
mod runner;
pub use runner::{FlowSomModel, Metaclustering, PopulationStatistics};

pub use crate::error::FlowSomError;
pub use crate::runner::{
    FLOWSOM_LEARNING_RATE_END, FLOWSOM_LEARNING_RATE_START, FLOWSOM_METACLUSTERS,
    FLOWSOM_RADIUS_END, FLOWSOM_RADIUS_START_QUANTILE, FLOWSOM_RANDOM_SEED, FLOWSOM_SCALE_INPUT,
    FLOWSOM_TRAINING_ITERATIONS, FLOWSOM_XDIM, FLOWSOM_YDIM, FlowSomAssignment, FlowSomConfig,
    FlowSomConstants, FlowSomMetacluster, FlowSomMetrics, FlowSomNode, FlowSomParameters,
    FlowSomResult, FlowSomRunner, FlowSomScaleSummary, FlowSomSummary,
};

/// Upstream implementations this crate is validated against
/// (`parity/versions.txt`), recorded in provenance.
pub const UPSTREAM: &[(&str, &str)] = &[("FlowSOM", "2.18.0")];
