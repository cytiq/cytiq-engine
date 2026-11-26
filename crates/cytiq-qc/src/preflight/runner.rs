use std::io::Write as _;
use std::path::{Path, PathBuf};

use cytiq_manifest::{
    CompensationSource, ControlEntry, Panel, RunManifest, SampleEntry, SignalState,
};
use cytiq_publish::{Existing, Prepared, Publication};
use log::{debug, info};
use serde::Serialize;

use super::checks::{
    Check, LoadedFile, check_control_coverage, check_control_dyes, check_detectors, check_file,
    check_linear_scale, check_markers, check_matrix, check_primary_channel, check_signal_state,
    check_spillover, panel_scope,
};
use super::error::PreflightError;
use crate::CheckStatus;

/// Configuration of `cytiq preflight`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightConfig {
    /// Run manifest (`.yaml`, `.yml` or `.json`).
    pub manifest: PathBuf,
    /// JSON report path.
    pub output: PathBuf,
    pub overwrite: bool,
}

/// The `preflight.json` report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreflightReport {
    pub tool: String,
    pub version: String,
    pub manifest: String,
    /// FCS files the manifest lists, resolved.
    pub files: Vec<String>,
    pub panels: Vec<PanelSummary>,
    /// The worst check status.
    pub status: CheckStatus,
    pub passed: usize,
    pub warnings: usize,
    pub failures: usize,
    pub checks: Vec<Check>,
}

/// What the report found for one panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PanelSummary {
    pub name: String,
    /// Sample ids.
    pub samples: Vec<String>,
    /// Control dyes, including controls shared by every panel.
    pub controls: Vec<String>,
    /// What the samples hold, as the manifest declares it.
    pub signal_state: SignalState,
    /// Compensation source of a conventional panel of raw files; `None` for
    /// spectral panels and compensated samples.
    pub compensation: Option<CompensationSource>,
}

pub struct PreflightRunner;

impl PreflightRunner {
    /// The files a run with `config` reads, with their provenance role: the
    /// manifest, the sample and control FCS files it lists, then the panels'
    /// compensation matrices.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest is invalid.
    pub fn input_files(
        config: &PreflightConfig,
    ) -> Result<Vec<(&'static str, PathBuf)>, PreflightError> {
        let manifest = RunManifest::load(&config.manifest)?;
        let mut files = vec![("manifest", config.manifest.clone())];
        files.extend(
            manifest
                .samples
                .iter()
                .map(|sample| ("input", sample.file.clone())),
        );
        files.extend(
            manifest
                .controls
                .iter()
                .map(|control| ("control", control.file.clone())),
        );
        files.extend(
            manifest
                .panels
                .iter()
                .filter_map(|panel| match &panel.compensation {
                    Some(CompensationSource::Matrix { matrix, .. }) => {
                        Some(("matrix", matrix.clone()))
                    }
                    _ => None,
                }),
        );
        Ok(files)
    }

    /// [`Self::prepare`], then publish, without a provenance record.
    ///
    /// # Errors
    ///
    /// As [`Self::prepare`], and what publishing returns.
    pub fn run(config: &PreflightConfig) -> Result<PreflightReport, PreflightError> {
        Ok(Self::prepare(config)?.commit()?)
    }

    /// Check the manifest and its files and stage the report, whatever the
    /// checks found; [`PreflightReport::ensure_passed`] turns failed checks
    /// into an error.
    ///
    /// Nothing is published.
    ///
    /// # Errors
    ///
    /// Returns an error when the output exists without `overwrite`, the
    /// manifest is invalid, or the report cannot be written.
    pub fn prepare(config: &PreflightConfig) -> Result<Prepared<PreflightReport>, PreflightError> {
        if config.output.exists() && !config.overwrite {
            return Err(PreflightError::OutputExists {
                path: config.output.clone(),
            });
        }
        let manifest = RunManifest::load(&config.manifest)?;
        let files = manifest_files(&manifest);
        info!(
            "preflight: {} sample(s), {} control(s), {} panel(s)",
            manifest.samples.len(),
            manifest.controls.len(),
            manifest.panels.len()
        );
        let checks = run_checks(&manifest);
        let count = |status| checks.iter().filter(|check| check.status == status).count();
        let report = PreflightReport {
            tool: "cytiq preflight".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            manifest: config.manifest.display().to_string(),
            files: files
                .iter()
                .map(|file| file.display().to_string())
                .collect(),
            panels: manifest
                .panels
                .iter()
                .map(|panel| PanelSummary {
                    name: panel.label().to_owned(),
                    samples: samples_of(&manifest, panel)
                        .map(|sample| sample.id.clone())
                        .collect(),
                    controls: manifest
                        .controls_of(panel)
                        .map(|control| control.dye.clone())
                        .collect(),
                    signal_state: panel.signal_state,
                    compensation: manifest.compensation_of(panel).cloned(),
                })
                .collect(),
            status: checks
                .iter()
                .map(|check| check.status)
                .max()
                .unwrap_or(CheckStatus::Pass),
            passed: count(CheckStatus::Pass),
            warnings: count(CheckStatus::Warn),
            failures: count(CheckStatus::Fail),
            checks,
        };
        let publication = stage_report(
            &config.output,
            &report,
            Existing::overwrite(config.overwrite),
        )?;
        Ok(Prepared {
            summary: report,
            publication,
        })
    }
}

impl PreflightReport {
    /// `Ok` when no check failed.
    ///
    /// # Errors
    ///
    /// Returns [`PreflightError::Failed`] naming the report when any check
    /// failed.
    pub fn ensure_passed(&self, report_path: &Path) -> Result<(), PreflightError> {
        if self.failures == 0 {
            Ok(())
        } else {
            Err(PreflightError::Failed {
                failed: self.failures,
                report: report_path.to_path_buf(),
            })
        }
    }
}

fn run_checks(manifest: &RunManifest) -> Vec<Check> {
    let mut checks = Vec::new();
    // Each file is read once; panels refer to them by position.
    let mut samples = Vec::with_capacity(manifest.samples.len());
    for sample in &manifest.samples {
        let scope = format!("sample {}", sample.id);
        let declared = manifest
            .panel_of(sample)
            .map_or(SignalState::Raw, |panel| panel.signal_state);
        let (file_checks, loaded) = check_file(&scope, &sample.file, declared);
        checks.extend(file_checks);
        samples.push(loaded);
    }
    let mut controls = Vec::with_capacity(manifest.controls.len());
    for control in &manifest.controls {
        let scope = format!("control {}", control.dye);
        // Controls calibrate raw detector values, whatever the samples hold.
        let (file_checks, loaded) = check_file(&scope, &control.file, SignalState::Raw);
        checks.extend(file_checks);
        if let Some(dataset) = &loaded.dataset {
            checks.push(check_signal_state(&scope, dataset, SignalState::Raw));
            checks.push(check_linear_scale(&scope, dataset, CheckStatus::Fail));
            if let Some(channel) = &control.primary_channel {
                checks.push(check_primary_channel(&scope, dataset, channel));
            }
        }
        controls.push(loaded);
    }
    for panel in &manifest.panels {
        checks.extend(check_panel(manifest, panel, &samples, &controls));
    }
    checks.extend(duplicate_files(manifest));
    checks
}

fn samples_of<'a>(
    manifest: &'a RunManifest,
    panel: &'a Panel,
) -> impl Iterator<Item = &'a SampleEntry> {
    manifest.samples.iter().filter(move |sample| {
        manifest
            .panel_of(sample)
            .is_some_and(|own| std::ptr::eq(own, panel))
    })
}

/// Signal state, markers, calibration and detector configuration of one
/// panel's files.
fn check_panel(
    manifest: &RunManifest,
    panel: &Panel,
    samples: &[LoadedFile],
    controls: &[LoadedFile],
) -> Vec<Check> {
    let mut checks = Vec::new();
    let panel_samples = manifest
        .samples
        .iter()
        .zip(samples)
        .filter(|(sample, _)| {
            manifest
                .panel_of(sample)
                .is_some_and(|own| std::ptr::eq(own, panel))
        })
        .map(|(_, loaded)| loaded)
        .collect::<Vec<_>>();
    let panel_controls = manifest
        .controls
        .iter()
        .zip(controls)
        .filter(|(control, _)| {
            control.panel.is_none() || control.panel.as_deref() == panel.name.as_deref()
        })
        .collect::<Vec<(&ControlEntry, &LoadedFile)>>();
    // `cytiq compensate` linearises the samples it compensates; every other
    // stage reads stored values.
    let log_amplified = if manifest.compensation_of(panel).is_some() {
        CheckStatus::Warn
    } else {
        CheckStatus::Fail
    };
    for loaded in &panel_samples {
        if let Some(dataset) = &loaded.dataset {
            checks.push(check_signal_state(
                &loaded.scope,
                dataset,
                panel.signal_state,
            ));
            checks.push(check_linear_scale(&loaded.scope, dataset, log_amplified));
            checks.extend(check_markers(&loaded.scope, dataset, panel));
        }
    }
    let reference = panel_samples
        .iter()
        .find_map(|loaded| loaded.dataset.as_ref());
    let scope = panel_scope(panel);
    if !panel_controls.is_empty() {
        checks.extend(check_control_dyes(&scope, &panel_controls));
    }
    // Samples compensated or unmixed elsewhere need no calibration, and their
    // channels need not match the raw controls.
    let raw = panel.signal_state == SignalState::Raw;
    match manifest.compensation_of(panel) {
        Some(CompensationSource::Fcs) => {
            for loaded in &panel_samples {
                if let Some(dataset) = &loaded.dataset {
                    checks.push(check_spillover(&loaded.scope, dataset));
                }
            }
        }
        Some(CompensationSource::Matrix { matrix, kind }) => {
            checks.extend(check_matrix(&scope, matrix, *kind, &panel_samples));
        }
        Some(CompensationSource::Controls) => {
            checks.push(check_control_coverage(
                &scope,
                panel,
                &panel_controls,
                reference,
                CheckStatus::Fail,
            ));
        }
        None if raw && manifest.cytometry == cytiq_manifest::Cytometry::Spectral => {
            checks.push(check_control_coverage(
                &scope,
                panel,
                &panel_controls,
                reference,
                CheckStatus::Warn,
            ));
        }
        None => {}
    }
    let files = panel_samples
        .iter()
        .copied()
        .chain(
            panel_controls
                .iter()
                .filter(|_| raw)
                .map(|(_, loaded)| *loaded),
        )
        .collect::<Vec<_>>();
    checks.extend(check_detectors(&files));
    checks
}

fn duplicate_files(manifest: &RunManifest) -> Vec<Check> {
    let mut paths = manifest
        .samples
        .iter()
        .map(|sample| sample.file.as_path())
        .chain(
            manifest
                .controls
                .iter()
                .map(|control| control.file.as_path()),
        )
        .collect::<Vec<&Path>>();
    paths.sort();
    paths
        .windows(2)
        .filter(|pair| pair[0] == pair[1])
        .map(|pair| Check {
            scope: "manifest".to_owned(),
            check: "unique_files",
            status: CheckStatus::Warn,
            message: format!("`{}` is listed more than once", pair[0].display()),
        })
        .collect()
}

fn stage_report(
    path: &Path,
    report: &PreflightReport,
    existing: Existing,
) -> Result<Publication, PreflightError> {
    debug!("staging preflight report to {}", path.display());
    let write_error = |source| PreflightError::Write {
        path: path.to_path_buf(),
        source,
    };
    let mut bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| write_error(std::io::Error::other(error)))?;
    bytes.push(b'\n');
    let mut publication = Publication::new(existing);
    publication.stage_file(path, |writer| writer.write_all(&bytes).map_err(write_error))?;
    Ok(publication)
}

/// Sample then control FCS files of a manifest, resolved.
fn manifest_files(manifest: &RunManifest) -> Vec<PathBuf> {
    manifest
        .samples
        .iter()
        .map(|sample| sample.file.clone())
        .chain(manifest.controls.iter().map(|control| control.file.clone()))
        .collect()
}
