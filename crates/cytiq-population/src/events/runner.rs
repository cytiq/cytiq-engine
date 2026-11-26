//! `cytiq export events`: one FCS file to one events table, or to a
//! directory of part tables, published atomically.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use cytiq_fcs::{DeclaredTypes, FcsReadLimits, FcsReader};

use cytiq_mask::parquet::ParquetReadLimits;
use cytiq_provenance::{RunRecord, hash_file, record_path_for_dir, record_path_for_file};
use cytiq_publish::{Existing, Publication, PublishError};

use super::{
    BaseState, Definition, ExportOptions, ExportSummary, LayerBasis, LayerState, PARTS_FILE,
    WriteSettings, export_with,
};
use crate::{PopulationError, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportConfig {
    pub input: PathBuf,
    /// The table, or the parts directory with
    /// [`WriteSettings::max_file_bytes`].
    pub output: PathBuf,
    /// The layer the input must hold.
    pub layer: LayerState,
    pub base_state: BaseState,
    /// The matrix file `cytiq compensate --matrix` applied to the input.
    pub compensation_matrix: Option<PathBuf>,
    /// The reference `cytiq autospectral` unmixed the input with.
    pub unmixing_reference: Option<PathBuf>,
    /// Accept an input whose rows are a subset of the acquisition.
    pub allow_subset: bool,
    pub settings: WriteSettings,
    pub overwrite: bool,
}

impl ExportConfig {
    /// The output is a directory of parts.
    #[must_use]
    pub const fn splits(&self) -> bool {
        self.settings.max_file_bytes.is_some()
    }

    /// Where the provenance record of the export goes.
    #[must_use]
    pub fn record_path(&self) -> PathBuf {
        if self.splits() {
            record_path_for_dir(&self.output)
        } else {
            record_path_for_file(&self.output)
        }
    }
}

pub struct ExportRunner;

impl ExportRunner {
    /// Files the export reads, by provenance role: the input, the supplied
    /// definitions and the input's own provenance record when it has one.
    #[must_use]
    pub fn input_files(config: &ExportConfig) -> Vec<(&'static str, PathBuf)> {
        let mut files = vec![("input", config.input.clone())];
        if let Some(matrix) = &config.compensation_matrix {
            files.push(("compensation_matrix", matrix.clone()));
        }
        if let Some(reference) = &config.unmixing_reference {
            files.push(("unmixing_reference", reference.clone()));
        }
        if let Some(record) = producer_record_path(&config.input) {
            files.push(("producer_provenance", record));
        }
        files
    }

    /// Who stated the input's base state: the file through `$PnTYPE`, or
    /// the caller. `None` when the input cannot be read; the export itself
    /// then reports why.
    #[must_use]
    pub fn layer_basis(config: &ExportConfig) -> Option<LayerBasis> {
        let stream = FcsReader::open(&config.input)
            .and_then(|reader| reader.stream(NonZeroUsize::MIN, FcsReadLimits::default()))
            .ok()?;
        let declared = DeclaredTypes::of(&stream.metadata.parameters);
        Some(LayerBasis::of(&declared, config.base_state))
    }

    /// The files an export published: the table, or the parts manifest and
    /// every part.
    #[must_use]
    pub fn outputs(config: &ExportConfig, summary: &ExportSummary) -> Vec<PathBuf> {
        match &summary.parts {
            None => vec![config.output.clone()],
            Some(manifest) => std::iter::once(config.output.join(PARTS_FILE))
                .chain(
                    manifest
                        .parts
                        .iter()
                        .map(|part| config.output.join(&part.location)),
                )
                .collect(),
        }
    }

    /// Export the input, refusing an existing output without `overwrite`
    /// before any work, and publish the table or the parts directory
    /// atomically.
    ///
    /// # Errors
    /// Returns [`PublishError::Exists`] for an existing output, a mismatch
    /// between a supplied definition and the input's provenance record, and
    /// every error of [`export_with`].
    pub fn run(config: &ExportConfig) -> Result<ExportSummary> {
        if config.output.exists() && !config.overwrite {
            return Err(PublishError::Exists {
                path: config.output.clone(),
            }
            .into());
        }
        let options = ExportOptions {
            base_state: config.base_state,
            compensation_matrix: definition(
                config.compensation_matrix.as_deref(),
                &config.input,
                "compensation matrix",
            )?,
            unmixing_reference: definition(
                config.unmixing_reference.as_deref(),
                &config.input,
                "unmixing reference",
            )?,
            layer: config.layer,
            allow_subset: config.allow_subset,
        };
        let limits = ParquetReadLimits::default();
        let mut publication = Publication::new(Existing::overwrite(config.overwrite));
        let summary = if config.splits() {
            let staged = publication.stage_dir(&config.output)?;
            export_with(&config.input, &staged, &options, &config.settings, limits)?
        } else {
            let mut summary = None;
            publication.stage_path(&config.output, |staged| {
                summary = Some(export_with(
                    &config.input,
                    staged,
                    &options,
                    &config.settings,
                    limits,
                )?);
                Ok::<(), PopulationError>(())
            })?;
            summary.ok_or_else(|| PopulationError::Invalid("export wrote nothing".into()))?
        };
        publication.commit()?;
        Ok(summary)
    }
}

/// The provenance record a cytiq stage wrote for the input: its sidecar, or
/// the record of the directory it was published in.
fn producer_record_path(input: &Path) -> Option<PathBuf> {
    let sidecar = record_path_for_file(input);
    if sidecar.is_file() {
        return Some(sidecar);
    }
    let directory = input.parent()?.join("provenance.json");
    directory.is_file().then_some(directory)
}

/// The producing run's record when it lists `input` as an output.
fn producer(input: &Path) -> Result<Option<RunRecord>> {
    let Some(path) = producer_record_path(input) else {
        return Ok(None);
    };
    let record = crate::native::producer_record(&path)?;
    let input_sha = hash_file(input)?.1;
    Ok(record
        .outputs
        .iter()
        .any(|output| output.sha256 == input_sha)
        .then_some(record))
}

/// The digest of a supplied definition, verified against the producing
/// run's record when the input has one: the record must name the same file.
fn definition(path: Option<&Path>, input: &Path, what: &str) -> Result<Option<Definition>> {
    let Some(path) = path else {
        return Ok(None);
    };
    if !path.is_file() {
        return Err(PopulationError::Missing(path.to_owned()));
    }
    let sha256 = hash_file(path)?.1;
    let verified = match producer(input)? {
        None => false,
        Some(record)
            if record
                .inputs
                .iter()
                .chain(&record.outputs)
                .any(|f| f.sha256 == sha256) =>
        {
            true
        }
        Some(record) => {
            return Err(PopulationError::Invalid(format!(
                "the {what} is not the one `{}` used to produce the input",
                record.tool
            )));
        }
    };
    Ok(Some(Definition { sha256, verified }))
}
