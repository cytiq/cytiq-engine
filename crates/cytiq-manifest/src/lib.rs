//! Run manifest: one YAML or JSON file describing an acquisition run — the
//! cytometry type, the panels, the sample files and the controls.
//!
//! ```yaml
//! cytometry: conventional        # or spectral
//! panel:
//!   name: T-cell-8c
//!   markers:
//!     - { marker: CD3, channel: FL1-A }        # channel optional: matched by $PnS
//!     - { marker: Live/Dead, required: false }
//! samples:
//!   - { id: S01, file: fcs/s01.fcs }
//! controls:
//!   - { file: fcs/unstained.fcs, dye: Autofluorescence }
//!   - { file: fcs/cd3_fitc.fcs, dye: FITC, primary_channel: FL1-A }
//! ```
//!
//! A run with several panels lists them under `panels` (named) and links each
//! sample to one; a control without `panel` belongs to every panel:
//!
//! ```yaml
//! cytometry: conventional
//! panels:
//!   - name: T
//!     markers: [{ marker: CD3 }]
//!     compensation: { source: matrix, matrix: autospill/t.csv, kind: compensation }
//!   - name: B
//!     markers: [{ marker: CD19 }]
//! samples:
//!   - { id: S01-T, file: s01_t.fcs, panel: T, specimen: S01 }
//!   - { id: S01-B, file: s01_b.fcs, panel: B, specimen: S01 }
//! ```
//!
//! A panel whose files other software already compensated or unmixed says so
//! with `signal_state: compensated` or `unmixed` (`raw` by default).
//!
//! Relative file paths are resolved against the manifest's directory.

mod check;
mod metadata;
mod migrate;
mod run;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub use metadata::{
    ControlRole, Detector, DetectorKind, Experiment, Instrument, OntologyTerm, Reagent, Specimen,
};
pub use migrate::{SCHEMA_VERSION, SUPPORTED_SCHEMA_VERSIONS};
pub use run::{
    AssessedOf, CompensationSource, ControlEntry, Cytometry, FilterPolicy, Marker, MatrixKind,
    Panel, QcPolicy, RunManifest, SampleEntry, Severity, SignalState,
};

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("failed to read manifest `{path}`: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest `{path}` must be `.yaml`, `.yml` or `.json`")]
    UnsupportedFormat { path: PathBuf },
    #[error("failed to parse manifest `{path}`: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("manifest `{path}` is invalid: {reason}")]
    Invalid { path: PathBuf, reason: String },
}

/// Largest manifest read. Real run manifests are a few KiB; the bound keeps a
/// wrong or hostile path from being read whole before YAML/JSON parsing.
pub const MANIFEST_BYTES_MAX: u64 = 16 << 20;

/// Read at most [`MANIFEST_BYTES_MAX`] bytes, also when the file grows after
/// opening; more, or invalid UTF-8, is `InvalidData`.
fn read_bounded(path: &Path) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MANIFEST_BYTES_MAX + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MANIFEST_BYTES_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("manifest exceeds {MANIFEST_BYTES_MAX} bytes"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

impl RunManifest {
    /// Read, parse and validate a manifest, resolving relative file paths
    /// against its directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or parsed, or when the
    /// manifest is invalid (see [`Self::validate`]).
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let text = read_bounded(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut manifest = Self::parse(path, &text)?;
        let base = path.parent().unwrap_or_else(|| Path::new(""));
        for sample in &mut manifest.samples {
            sample.file = resolve(base, &sample.file);
        }
        for control in &mut manifest.controls {
            control.file = resolve(base, &control.file);
        }
        for panel in &mut manifest.panels {
            if let Some(CompensationSource::Matrix { matrix, .. }) = &mut panel.compensation {
                *matrix = resolve(base, matrix);
            }
        }
        manifest.validate(path)?;
        Ok(manifest)
    }

    fn parse(path: &Path, text: &str) -> Result<Self, ManifestError> {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        let parse_error = |message: String| ManifestError::Parse {
            path: path.to_path_buf(),
            message,
        };
        match extension.as_deref() {
            Some("yaml" | "yml") => {
                serde_saphyr::from_str(text).map_err(|error| parse_error(error.to_string()))
            }
            Some("json") => {
                serde_json::from_str(text).map_err(|error| parse_error(error.to_string()))
            }
            _ => Err(ManifestError::UnsupportedFormat {
                path: path.to_path_buf(),
            }),
        }
    }
}

fn resolve(base: &Path, file: &Path) -> PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        base.join(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = "\
cytometry: conventional
panel:
  name: T cells
  markers:
    - { marker: CD3, channel: FL1-A }
    - { marker: Live/Dead, required: false }
samples:
  - { id: S01, file: fcs/s01.fcs }
controls:
  - { file: /abs/unstained.fcs, dye: Autofluorescence }
";

    fn write(name: &str, text: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("cytiq-manifest-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn oversized_manifest_is_refused_before_parsing() {
        let path = write("huge.yaml", "");
        fs::File::create(&path)
            .unwrap()
            .set_len(MANIFEST_BYTES_MAX + 1)
            .unwrap();
        let error = RunManifest::load(&path).unwrap_err();
        assert!(
            matches!(&error, ManifestError::Read { source, .. }
                if source.kind() == std::io::ErrorKind::InvalidData),
            "{error}"
        );
    }

    #[test]
    fn yaml_and_json_load_the_same_manifest() {
        let yaml_path = write("run.yaml", YAML);
        let from_yaml = RunManifest::load(&yaml_path).unwrap();
        let json = serde_json::to_string(&from_yaml).unwrap();
        let from_json = RunManifest::load(&write("run.json", &json)).unwrap();
        assert_eq!(from_yaml, from_json);
        assert_eq!(
            from_yaml.samples[0].file,
            yaml_path.parent().unwrap().join("fcs/s01.fcs")
        );
        assert_eq!(
            from_yaml.controls[0].file,
            PathBuf::from("/abs/unstained.fcs")
        );
        assert!(from_yaml.panels[0].markers[0].required);
        assert!(!from_yaml.panels[0].markers[1].required);
        assert_eq!(
            from_yaml.compensation_of(&from_yaml.panels[0]),
            Some(&CompensationSource::Fcs)
        );
    }

    #[test]
    fn rejects_unknown_fields_and_duplicate_ids() {
        let unknown = RunManifest::load(&write(
            "unknown.yaml",
            "cytometry: conventional\nsampels: []\n",
        ));
        assert!(matches!(unknown, Err(ManifestError::Parse { .. })));
        let typo = MULTI.replace("kind: compensation", "kind: compensation, matrx: x.csv");
        assert!(matches!(
            RunManifest::load(&write("typo.yaml", &typo)),
            Err(ManifestError::Parse { .. })
        ));

        let duplicate = YAML.replace(
            "  - { id: S01, file: fcs/s01.fcs }\n",
            "  - { id: S01, file: a.fcs }\n  - { id: S01, file: b.fcs }\n",
        );
        let error = RunManifest::load(&write("duplicate.yaml", &duplicate)).unwrap_err();
        assert!(error.to_string().contains("more than once"), "{error}");
    }

    #[test]
    fn spectral_runs_need_an_unstained_control() {
        let spectral = YAML
            .replace("cytometry: conventional", "cytometry: spectral")
            .replace("dye: Autofluorescence", "dye: FITC");
        let error = RunManifest::load(&write("spectral.yaml", &spectral)).unwrap_err();
        assert!(error.to_string().contains("unstained"), "{error}");
    }

    const MULTI: &str = "\
cytometry: conventional
panels:
  - name: T
    markers: [{ marker: CD3 }]
    compensation: { source: matrix, matrix: autospill/t.csv, kind: compensation }
  - name: B
    markers: [{ marker: CD19 }]
    compensation: { source: controls }
samples:
  - { id: S01-T, file: s01_t.fcs, panel: T, specimen: S01 }
  - { id: S01-B, file: s01_b.fcs, panel: B, specimen: S01 }
controls:
  - { file: unstained.fcs, dye: Autofluorescence }
  - { file: cd19.fcs, dye: PE, panel: B }
";

    #[test]
    fn single_panel_manifest_still_loads() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/pipeline/run.yaml");
        let manifest = RunManifest::load(&fixture).unwrap();
        assert_eq!(manifest.panels.len(), 1);
        let sample = &manifest.samples[0];
        assert_eq!(manifest.panel_of(sample), Some(&manifest.panels[0]));
    }

    #[test]
    fn links_samples_and_controls_to_their_panels() {
        let path = write("multi.yaml", MULTI);
        let manifest = RunManifest::load(&path).unwrap();
        let t = &manifest.panels[0];
        let b = &manifest.panels[1];
        assert_eq!(manifest.panel_of(&manifest.samples[1]), Some(b));
        assert_eq!(
            manifest.controls_of(t).count(),
            1,
            "only the shared control"
        );
        assert_eq!(manifest.controls_of(b).count(), 2);
        // Matrix paths resolve against the manifest, like FCS files.
        assert_eq!(
            manifest.compensation_of(t),
            Some(&CompensationSource::Matrix {
                matrix: path.parent().unwrap().join("autospill/t.csv"),
                kind: MatrixKind::Compensation,
            })
        );
        let json = serde_json::to_string(&manifest).unwrap();
        assert_eq!(
            RunManifest::load(&write("multi.json", &json)).unwrap(),
            manifest
        );
    }

    #[test]
    fn samples_need_a_panel_when_there_are_several() {
        let unlinked = MULTI.replace(", panel: T, specimen: S01", "");
        let error = RunManifest::load(&write("unlinked.yaml", &unlinked)).unwrap_err();
        assert!(
            error.to_string().contains("must name its `panel`"),
            "{error}"
        );

        let unknown = MULTI.replace("panel: T, specimen", "panel: NK, specimen");
        let error = RunManifest::load(&write("unknown-panel.yaml", &unknown)).unwrap_err();
        assert!(
            error.to_string().contains("panel `NK` is not defined"),
            "{error}"
        );
    }

    #[test]
    fn rejects_panel_and_panels_together() {
        let both = YAML.replace("samples:", "panels: []\nsamples:");
        let error = RunManifest::load(&write("both.yaml", &both)).unwrap_err();
        assert!(error.to_string().contains("not both"), "{error}");
    }

    #[test]
    fn rejects_compensation_in_spectral_runs() {
        let spectral = YAML
            .replace("cytometry: conventional", "cytometry: spectral")
            .replace(
                "  name: T cells\n",
                "  name: T cells\n  compensation: { source: fcs }\n",
            );
        let error = RunManifest::load(&write("spectral-comp.yaml", &spectral)).unwrap_err();
        assert!(error.to_string().contains("conventional runs"), "{error}");
    }

    #[test]
    fn controls_source_needs_single_stain_controls() {
        let without = MULTI.replace("  - { file: cd19.fcs, dye: PE, panel: B }\n", "");
        let error = RunManifest::load(&write("no-stains.yaml", &without)).unwrap_err();
        assert!(
            error.to_string().contains("needs single-stain controls"),
            "{error}"
        );
    }

    #[test]
    fn a_specimen_has_one_file_per_panel() {
        let twice = MULTI.replace("panel: B, specimen: S01", "panel: T, specimen: S01");
        let error = RunManifest::load(&write("specimen.yaml", &twice)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("more than one file in panel `T`"),
            "{error}"
        );
    }

    #[test]
    fn v2_keeps_repeat_acquisition_ids_and_rejects_unknown_versions() {
        let twice = MULTI.replace("panel: B, specimen: S01", "panel: T, specimen: S01");
        let path = write("repeat-v2.yaml", &format!("schema_version: 2\n{twice}"));
        assert_eq!(RunManifest::load(&path).unwrap().samples.len(), 2);
        let path = write("future.yaml", &format!("schema_version: 99\n{MULTI}"));
        assert!(RunManifest::load(&path).is_err());
    }

    #[test]
    fn cytof_v2_does_not_allow_fluorescence_compensation() {
        let document = "schema_version: 2\ncytometry: cytof\npanel:\n  markers: [{marker: CD3}]\nsamples: [{id: A01, file: sample.fcs}]\n";
        let path = write("mass.yaml", document);
        assert_eq!(
            RunManifest::load(&path).unwrap().cytometry,
            Cytometry::Cytof
        );
        let path = write("mass-v1.yaml", &document.replace("schema_version: 2\n", ""));
        assert!(RunManifest::load(&path).is_err());
        let path = write(
            "mass-compensation.yaml",
            &document.replace("  markers:", "  compensation: {source: fcs}\n  markers:"),
        );
        assert!(RunManifest::load(&path).is_err());
    }

    #[test]
    fn compensated_samples_need_no_compensation_source() {
        let manifest = RunManifest::load(&write("raw.yaml", YAML)).unwrap();
        assert_eq!(manifest.panels[0].signal_state, SignalState::Raw);

        let compensated = YAML.replace(
            "  name: T cells\n",
            "  name: T cells\n  signal_state: compensated\n",
        );
        let manifest = RunManifest::load(&write("compensated.yaml", &compensated)).unwrap();
        assert_eq!(manifest.panels[0].signal_state, SignalState::Compensated);
        assert_eq!(manifest.compensation_of(&manifest.panels[0]), None);

        let twice = compensated.replace(
            "  signal_state: compensated\n",
            "  signal_state: compensated\n  compensation: { source: fcs }\n",
        );
        let error = RunManifest::load(&write("twice.yaml", &twice)).unwrap_err();
        assert!(
            error.to_string().contains("not compensated again"),
            "{error}"
        );
    }

    #[test]
    fn signal_state_fits_the_cytometry() {
        let unmixed = YAML
            .replace("cytometry: conventional", "cytometry: spectral")
            .replace("dye: Autofluorescence", "dye: FITC")
            .replace(
                "  name: T cells\n",
                "  name: T cells\n  signal_state: unmixed\n",
            );
        let manifest = RunManifest::load(&write("unmixed.yaml", &unmixed)).unwrap();
        assert_eq!(
            manifest.panels[0].signal_state,
            SignalState::Unmixed,
            "unmixed samples need no unstained control"
        );

        let conventional = unmixed.replace("cytometry: spectral", "cytometry: conventional");
        let error = RunManifest::load(&write("unmixed-conv.yaml", &conventional)).unwrap_err();
        assert!(
            error.to_string().contains("applies to spectral runs"),
            "{error}"
        );

        let spectral = unmixed.replace("signal_state: unmixed", "signal_state: compensated");
        let error = RunManifest::load(&write("comp-spectral.yaml", &spectral)).unwrap_err();
        assert!(
            error.to_string().contains("exports are `unmixed`"),
            "{error}"
        );
    }

    #[test]
    fn reads_a_qc_policy() {
        let with_qc = YAML.replace(
            "  name: T cells\n",
            "  name: T cells\n  qc:\n    min_events: 1000\n    max_removed_fraction: 0.5\n    filters:\n      peacoqc_pass: { required: true, max_removed_fraction: 0.3 }\n      live: { min_assessed_fraction: 0.95, max_removed_fraction: 0.4, severity: warn }\n      singlets: { min_assessed_fraction: 1.0, assessed_of: eligible }\n",
        );
        let manifest = RunManifest::load(&write("qc.yaml", &with_qc)).unwrap();
        let qc = manifest.panels[0].qc.as_ref().unwrap();
        assert_eq!(qc.min_events, Some(1000));
        assert!(qc.filters["peacoqc_pass"].required);
        assert_eq!(qc.filters["peacoqc_pass"].severity, Severity::Fail);
        assert_eq!(qc.filters["live"].severity, Severity::Warn);
        assert_eq!(qc.filters["live"].min_assessed_fraction, Some(0.95));
        assert_eq!(qc.filters["live"].assessed_of, AssessedOf::Raw);
        assert_eq!(qc.filters["singlets"].assessed_of, AssessedOf::Eligible);

        let unbounded = with_qc.replace("min_assessed_fraction: 1.0, ", "");
        let error = RunManifest::load(&write("qc-of.yaml", &unbounded)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`filters.singlets.assessed_of` needs `min_assessed_fraction`"),
            "{error}"
        );

        let out_of_range = with_qc.replace("max_removed_fraction: 0.5", "max_removed_fraction: 50");
        let error = RunManifest::load(&write("qc-range.yaml", &out_of_range)).unwrap_err();
        assert!(error.to_string().contains("between 0 and 1"), "{error}");

        let out_of_range =
            with_qc.replace("min_assessed_fraction: 0.95", "min_assessed_fraction: 95");
        let error = RunManifest::load(&write("qc-assessed.yaml", &out_of_range)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`filters.live.min_assessed_fraction` must be between 0 and 1"),
            "{error}"
        );
    }
}
