use std::path::PathBuf;

use cytiq_publish::{Existing, Prepared, Publication};
use log::debug;

use crate::error::MaskError;
use crate::mask::{EventMask, read_combined};

/// Configuration of `cytiq mask combine`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskCombineConfig {
    pub masks: Vec<PathBuf>,
    pub output: PathBuf,
    pub overwrite: bool,
}

pub struct MaskCombineRunner;

impl MaskCombineRunner {
    /// [`Self::prepare`], then publish, without a provenance record.
    ///
    /// # Errors
    ///
    /// As [`Self::prepare`], and what publishing returns.
    pub fn run(config: &MaskCombineConfig) -> Result<EventMask, MaskError> {
        Ok(Self::prepare(config)?.commit()?)
    }

    /// Combine the configured masks and stage the result; returns the
    /// combined mask. Nothing is published.
    ///
    /// # Errors
    ///
    /// Returns an error when the output exists and `overwrite` is false, or
    /// when a mask cannot be read or staged.
    pub fn prepare(config: &MaskCombineConfig) -> Result<Prepared<EventMask>, MaskError> {
        if config.output.exists() && !config.overwrite {
            return Err(MaskError::OutputExists {
                path: config.output.clone(),
            });
        }
        let combined = read_combined(&config.masks)?.ok_or(MaskError::Empty)?;
        let mut publication = Publication::new(Existing::overwrite(config.overwrite));
        combined.stage_parquet(&mut publication, &config.output)?;
        debug!(
            "combined {} mask(s): kept {} of {} event(s)",
            config.masks.len(),
            combined.kept(),
            combined.len()
        );
        Ok(Prepared {
            summary: combined,
            publication,
        })
    }
}
