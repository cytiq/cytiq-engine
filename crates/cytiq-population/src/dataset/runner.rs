//! `cytiq dataset validate`: validate a manifest and publish its report.

use std::io::Write as _;
use std::path::PathBuf;

use cytiq_publish::{Existing, PublishError};

use super::{ValidateOptions, ValidationReport, parse, validate};
use crate::identity::ObjectKey;
use crate::issue::Issues;
use crate::object::ObjectRoot;
use crate::{PopulationError, Result, serialization};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidateConfig {
    pub manifest: PathBuf,
    /// The JSON report.
    pub output: PathBuf,
    pub options: ValidateOptions,
    pub overwrite: bool,
}

pub struct ValidateRunner;

impl ValidateRunner {
    /// Files the validation reads, by provenance role: the manifest and,
    /// when objects are verified, every object it lists that exists, so a
    /// changed object invalidates a reused report.
    #[must_use]
    pub fn input_files(config: &ValidateConfig) -> Vec<(&'static str, PathBuf)> {
        let mut files = vec![("manifest", config.manifest.clone())];
        if !config.options.verify_objects {
            return files;
        }
        let Ok(bytes) = cytiq_provenance::input::document_bytes(&config.manifest) else {
            return files;
        };
        let format = serialization::DocumentFormat::of(&config.manifest);
        let Some(manifest) = parse(&bytes, format, &mut Issues::new()) else {
            return files;
        };
        let root = config.options.root.clone().unwrap_or_else(|| {
            config
                .manifest
                .parent()
                .map_or_else(PathBuf::new, std::path::Path::to_path_buf)
        });
        let locations = manifest
            .artifacts
            .iter()
            .map(|artifact| artifact.location.as_str())
            .chain(
                manifest
                    .acquisitions
                    .iter()
                    .filter_map(|a| a.source.as_ref().map(|s| s.location.as_str())),
            );
        let Ok(objects) = ObjectRoot::open(&root) else {
            return files;
        };
        for location in locations {
            if let Ok(key) = ObjectKey::parse(location)
                && objects.object(&key).is_ok()
            {
                files.push(("object", key.resolve(&root)));
            }
        }
        files
    }

    /// Validate the manifest and publish the report, refusing an existing
    /// report without `overwrite` before any work. An invalid dataset is a
    /// report with errors, not an error.
    ///
    /// # Errors
    /// Returns [`PublishError::Exists`], a missing or unreadable manifest and
    /// write errors.
    pub fn run(config: &ValidateConfig) -> Result<ValidationReport> {
        if config.output.exists() && !config.overwrite {
            return Err(PublishError::Exists {
                path: config.output.clone(),
            }
            .into());
        }
        let report = validate(&config.manifest, &config.options)?;
        let mut bytes = serde_json::to_vec_pretty(&report)?;
        bytes.push(b'\n');
        cytiq_publish::write_file(
            &config.output,
            Existing::overwrite(config.overwrite),
            |writer| writer.write_all(&bytes).map_err(PopulationError::Io),
        )?;
        Ok(report)
    }
}
