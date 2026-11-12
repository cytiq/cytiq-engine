use std::path::Path;

use cytiq_publish::{Existing, Publication, Staging};
use log::debug;

use crate::error::AutoSpectralError;
use crate::runner::{AutoSpectralResult, ComponentKind};

/// `unmixed/sample_01.unmixed.fcs` -> `unmixed/sample_01.unmixed.masks.parquet`.
#[must_use]
pub fn masks_path(output: &Path) -> std::path::PathBuf {
    output.with_extension("masks.parquet")
}

/// Write references, unmixed FCS files, diagnostics, and optional debug output.
///
/// The outputs are published together, `diagnostics/run.json` last: a run
/// directory either has all of them or keeps its previous contents.
///
/// # Errors
///
/// Returns an error if any output directory or file cannot be created.
pub fn write_outputs(
    result: &AutoSpectralResult,
    output_dir: &Path,
    existing: Existing,
    debug_dir: Option<&Path>,
) -> Result<(), AutoSpectralError> {
    let references_dir = output_dir.join("references");
    let mut publication = Publication::new(existing);
    let spectra = references_dir.join("fluorophore_spectra.csv");
    publication.stage_file(&spectra, |writer| {
        write_reference_csv(writer, &spectra, result)
    })?;
    stage_json(
        &mut publication,
        &references_dir.join("autospectral-references.json"),
        &result.references,
    )?;
    for sample in &result.samples {
        debug!("writing unmixed FCS to {}", sample.output_path.display());
        let options =
            cytiq_fcs::FcsWriteOptions::new().with_text_keywords(sample.text_keywords.clone());
        publication.stage_path(&sample.output_path, |path| {
            cytiq_fcs::write_fcs3_1_exact_with_options(
                path,
                &sample.parameters,
                &sample.columns,
                &options,
            )
            .map_err(|source| AutoSpectralError::FcsWrite {
                path: sample.output_path.clone(),
                source,
            })
        })?;
        sample
            .mask
            .stage_parquet(&mut publication, &masks_path(&sample.output_path))?;
    }
    stage_json(
        &mut publication,
        &output_dir.join("diagnostics").join("run.json"),
        &result.summary,
    )?;
    publication.commit()?;

    if let Some(debug_dir) = debug_dir {
        let mut publication = Publication::new(Existing::Replace);
        stage_json(
            &mut publication,
            &debug_dir.join("summary.json"),
            &result.summary,
        )?;
        publication.commit()?;
    }

    Ok(())
}

fn write_reference_csv(
    file: &mut Staging<'_>,
    path: &Path,
    result: &AutoSpectralResult,
) -> Result<(), AutoSpectralError> {
    let mut writer = csv::Writer::from_writer(file);
    let mut header = vec!["component".to_owned(), "kind".to_owned()];
    header.extend(result.references.detectors.iter().cloned());
    writer
        .write_record(header)
        .map_err(|source| AutoSpectralError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for component in &result.references.components {
        let mut record = vec![
            component.name.clone(),
            match component.kind {
                ComponentKind::Fluorophore => "fluorophore".to_owned(),
                ComponentKind::Autofluorescence => "autofluorescence".to_owned(),
            },
        ];
        record.extend(
            component
                .spectrum
                .iter()
                .map(|value| format!("{value:.12}")),
        );
        writer
            .write_record(record)
            .map_err(|source| AutoSpectralError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer
        .flush()
        .map_err(|source| AutoSpectralError::CsvFlush {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(())
}

fn stage_json<T: serde::Serialize>(
    publication: &mut Publication,
    path: &Path,
    value: &T,
) -> Result<(), AutoSpectralError> {
    publication.stage_file(path, |writer| {
        serde_json::to_writer_pretty(writer, value).map_err(|source| {
            AutoSpectralError::JsonSerialize {
                path: path.to_path_buf(),
                source,
            }
        })
    })
}
