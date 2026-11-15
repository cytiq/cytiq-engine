//! Run manifest versions and migration to the current one.
//!
//! | Version | Adds |
//! | --- | --- |
//! | 1 (omitted) | Panels, samples, controls; one file per specimen and panel |
//! | 2 | Repeat acquisitions with unique ids; `cytometry: cytof` |
//! | 3 | `MIFlowCyt` metadata: `experiment`, `instruments`, `specimens`, marker `reporter`/`detector`/`reagent`, control `role` |
//!
//! Each version only adds optional fields or relaxes a rule, so a valid
//! document of an earlier version describes the same run under the current
//! one once its `schema_version` is raised.

use std::ops::RangeInclusive;
use std::path::Path;

use crate::{ManifestError, RunManifest};

/// The version new manifests should declare.
pub const SCHEMA_VERSION: u32 = 3;
/// Every version `RunManifest::load` reads.
pub const SUPPORTED_SCHEMA_VERSIONS: RangeInclusive<u32> = 1..=SCHEMA_VERSION;

impl RunManifest {
    /// The same run declared at [`SCHEMA_VERSION`], validated under its rules.
    /// Migration is idempotent and changes nothing but `schema_version`.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Invalid`] when the document is invalid under
    /// the current rules, for example a v1 sample with an empty `specimen`,
    /// which v2 began to reject.
    pub fn migrate(mut self, path: &Path) -> Result<Self, ManifestError> {
        self.schema_version = SCHEMA_VERSION;
        self.validate(path)?;
        Ok(self)
    }
}
