//! A two-panel dataset built from synthetic FCS files, shared by the
//! dataset tests.
#![allow(dead_code)]

use std::path::Path;

use cytiq_fcs::{FcsReadLimits, FcsReader, FcsWriteOptions, FcsWriteParameter};
use cytiq_manifest::RunManifest;
use cytiq_population::dataset::{
    self, AcquisitionRole, ArtifactEntry, ArtifactRole, DatasetManifest, SampleEntry, contracts,
};
use cytiq_population::events::{self, ExportOptions, LayerState, WriteSettings};
use cytiq_population::privacy::PrivacyPolicy;

const RUN: &str = "\
schema_version: 3
cytometry: conventional
experiment: { title: Two-panel test, purpose: Contract tests }
instruments:
  - id: canto-1
    manufacturer: BD Biosciences
    model: FACSCanto II
    detectors:
      - { id: B530, kind: pmt, excitation_nm: 488, emission_filter: 530/30 }
      - { id: B585, kind: pmt, excitation_nm: 488, emission_filter: 585/42 }
specimens:
  - { id: SP01, organism: Homo sapiens, material: peripheral blood }
panels:
  - name: T
    instrument: canto-1
    markers:
      - { marker: CD3, channel: FL1-A, reporter: FITC, detector: B530, reagent: { clone: SK7 } }
      - { marker: CD4, channel: FL2-A, reporter: PE, detector: B585 }
  - name: B
    instrument: canto-1
    markers:
      - { marker: CD19, channel: FL1-A, reporter: FITC, detector: B530 }
samples:
  - { id: SP01-T, file: t.fcs, panel: T, specimen: SP01 }
  - { id: SP01-B, file: b.fcs, panel: B, specimen: SP01 }
";

const SPILL: &str = "2,FL1-A,FL2-A,1,0.1,0.05,1";
const ARCSINH: &str = r#"{"method":"arcsinh","parameters":{"cofactor":150},"files":[{"channels":["FL1-A","FL2-A"]}]}"#;

pub struct Built {
    pub root: tempfile::TempDir,
    pub manifest: DatasetManifest,
}

fn write_fcs(path: &Path, names: &[(&str, Option<&str>)], keywords: &[(&str, String)]) {
    let parameters: Vec<FcsWriteParameter> = names
        .iter()
        .map(|(name, label)| match label {
            Some(label) => FcsWriteParameter::with_label(*name, *label),
            None => FcsWriteParameter::new(*name),
        })
        .collect();
    let columns: Vec<Vec<f32>> = (0..u8::try_from(names.len()).unwrap())
        .map(|c| {
            (0..5_u8)
                .map(|r| f32::from(r) + 10.0 * f32::from(c) + 0.5)
                .collect()
        })
        .collect();
    let mut options = FcsWriteOptions::new()
        .with_text_keyword("$CYT", "FACSCanto II")
        .with_text_keyword("$FIL", "Patient_Name.fcs")
        .with_text_keyword("$DATE", "01-JAN-2024");
    for (key, value) in keywords {
        options = options.with_text_keyword(*key, value.clone());
    }
    cytiq_fcs::write_fcs3_1_f32_with_options(path, &parameters, &columns, &options).unwrap();
}

fn metadata(path: &Path) -> cytiq_fcs::FcsMetadata {
    FcsReader::open(path)
        .unwrap()
        .stream(std::num::NonZeroUsize::MIN, FcsReadLimits::default())
        .unwrap()
        .metadata
}

/// How panel T's raw layer is stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Storage {
    #[default]
    Table,
    /// Parts of two events each, listed by `parts.json`.
    Parts,
    /// A table of two of the three channels.
    Projection,
}

fn layer(
    root: &Path,
    input: &Path,
    (location, state): (&str, LayerState),
    panel: &str,
    parents: Vec<String>,
) -> ArtifactEntry {
    stored_layer(
        root,
        input,
        (location, state),
        panel,
        parents,
        Storage::Table,
    )
}

fn stored_layer(
    root: &Path,
    input: &Path,
    (location, state): (&str, LayerState),
    panel: &str,
    parents: Vec<String>,
    storage: Storage,
) -> ArtifactEntry {
    let output = root.join(location);
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    let options = ExportOptions {
        layer: state,
        ..ExportOptions::default()
    };
    let limits = cytiq_mask::parquet::ParquetReadLimits::default();
    if storage == Storage::Parts {
        std::fs::create_dir_all(&output).unwrap();
        // 8 bytes of id and three float32 channels per event.
        let settings = WriteSettings {
            max_file_bytes: std::num::NonZeroU64::new(2 * (8 + 3 * 4)),
            ..WriteSettings::default()
        };
        let summary = events::export_with(input, &output, &options, &settings, limits).unwrap();
        let manifest = output.join(events::PARTS_FILE);
        return ArtifactEntry::signal_layer_parts(
            &summary.descriptor,
            &summary.values_sha256,
            Some(panel.into()),
            parents,
            format!("{location}/{}", events::PARTS_FILE),
            dataset::content_of(&manifest).unwrap(),
        );
    }
    let settings = WriteSettings {
        channels: (storage == Storage::Projection)
            .then(|| vec!["FSC-A".to_owned(), "FL1-A".to_owned()]),
        ..WriteSettings::default()
    };
    let summary = events::export_with(input, &output, &options, &settings, limits).unwrap();
    ArtifactEntry::signal_layer(
        &summary.descriptor,
        &summary.values_sha256,
        Some(panel.into()),
        parents,
        location.into(),
        dataset::content_of(&output).unwrap(),
    )
}

/// Write the raw files of both panels and panel T's derived files, as
/// `cytiq compensate` and `cytiq transform` record them; return T's
/// acquisition.
fn write_inputs(raw: &Path) -> String {
    let names = [
        ("FSC-A", None),
        ("FL1-A", Some("CD3 FITC")),
        ("FL2-A", Some("CD4 PE")),
    ];
    write_fcs(&raw.join("t.fcs"), &names, &[]);
    write_fcs(&raw.join("b.fcs"), &names[..2], &[("$SMNO", "B".into())]);
    let acquisition = cytiq_provenance::hash_file(&raw.join("t.fcs")).unwrap().1;
    let compensated = [
        ("CYTIQ_ACQUISITION", acquisition.clone()),
        (
            "CYTIQ_COMPENSATION",
            r#"{"source":"fcs","channels":["FL1-A","FL2-A"]}"#.into(),
        ),
        ("CYTIQ_SOURCE_SPILL", SPILL.into()),
    ];
    let mut transformed = compensated.to_vec();
    transformed.push(("CYTIQ_TRANSFORM", ARCSINH.into()));
    write_fcs(&raw.join("t.comp.fcs"), &names, &compensated);
    write_fcs(&raw.join("t.trans.fcs"), &names, &transformed);
    acquisition
}

/// Panels, acquisitions and samples from the run manifest and raw files.
fn entries(
    run: &RunManifest,
    raw: &Path,
    policy: &PrivacyPolicy,
) -> (
    Vec<dataset::PanelEntry>,
    Vec<dataset::AcquisitionEntry>,
    Vec<SampleEntry>,
) {
    let panels = run
        .panels
        .iter()
        .zip(["t.fcs", "b.fcs"])
        .map(|(panel, file)| {
            dataset::panel_from_run(run, panel, &metadata(&raw.join(file))).unwrap()
        })
        .collect();
    let acquisitions: Vec<_> = [("t.fcs", "T"), ("b.fcs", "B")]
        .iter()
        .map(|(file, panel)| {
            let path = raw.join(file);
            dataset::acquisition_entry(
                &path,
                &metadata(&path),
                panel,
                AcquisitionRole::Sample,
                policy,
            )
            .unwrap()
        })
        .collect();
    let samples = run
        .samples
        .iter()
        .zip(&acquisitions)
        .map(|(sample, acquisition)| SampleEntry {
            id: sample.id.clone(),
            panel: sample.panel.clone().unwrap(),
            specimen: sample.specimen.clone(),
            acquisition: acquisition.id.clone(),
        })
        .collect();
    (panels, acquisitions, samples)
}

/// Export every layer and write panel T's QC mask.
fn artifacts(
    root: &Path,
    raw: &Path,
    acquisition_t: String,
    storage: Storage,
) -> Vec<ArtifactEntry> {
    let t_raw = stored_layer(
        root,
        &raw.join("t.fcs"),
        ("events/T/SP01-T.raw.parquet", LayerState::Raw),
        "T",
        vec![],
        storage,
    );
    let t_comp = layer(
        root,
        &raw.join("t.comp.fcs"),
        (
            "events/T/SP01-T.compensated.parquet",
            LayerState::Compensated,
        ),
        "T",
        vec![t_raw.semantic_id.clone()],
    );
    let t_trans = layer(
        root,
        &raw.join("t.trans.fcs"),
        (
            "events/T/SP01-T.transformed.parquet",
            LayerState::Transformed,
        ),
        "T",
        vec![t_comp.semantic_id.clone()],
    );
    let b_raw = layer(
        root,
        &raw.join("b.fcs"),
        ("events/B/SP01-B.raw.parquet", LayerState::Raw),
        "B",
        vec![],
    );
    let mask_path = root.join("masks/T/SP01-T.mask.parquet");
    std::fs::create_dir_all(mask_path.parent().unwrap()).unwrap();
    cytiq_mask::EventMask::new(
        acquisition_t.clone(),
        vec![true, true, false, true, true],
        None,
    )
    .with_filter(
        "margin_pass",
        vec![Some(true), Some(true), Some(false), Some(true), Some(true)],
    )
    .write_parquet(&mask_path)
    .unwrap();
    let mask = ArtifactEntry::object(
        ArtifactRole::TechnicalMask,
        contracts::MASK,
        Some(acquisition_t),
        vec![t_raw.semantic_id.clone()],
        "masks/T/SP01-T.mask.parquet".into(),
        dataset::content_of(&mask_path).unwrap(),
    );
    vec![t_raw, t_comp, t_trans, b_raw, mask]
}

/// Panel T: raw, compensated and transformed layers and a QC mask; panel B:
/// a raw layer. Both acquisitions share specimen SP01 and count rows from 0.
pub fn build() -> Built {
    build_storing(Storage::Table)
}

/// [`build`] with panel T's raw layer stored as `storage`.
pub fn build_storing(storage: Storage) -> Built {
    let root = tempfile::tempdir().unwrap();
    let raw = root.path().join("raw");
    std::fs::create_dir_all(&raw).unwrap();
    std::fs::write(raw.join("run.yaml"), RUN).unwrap();
    let run = RunManifest::load(&raw.join("run.yaml")).unwrap();
    let acquisition_t = write_inputs(&raw);
    let policy = PrivacyPolicy::deidentified();
    let (panels, acquisitions, samples) = entries(&run, &raw, &policy);
    let artifacts = artifacts(root.path(), &raw, acquisition_t, storage);
    std::fs::remove_dir_all(&raw).unwrap();
    let manifest = DatasetManifest {
        schema_version: dataset::SCHEMA_VERSION,
        artifact_type: dataset::ARTIFACT_TYPE.into(),
        dataset_id: None,
        contracts: if storage == Storage::Parts {
            vec![
                contracts::EVENTS.into(),
                contracts::EVENTS_PARTS.into(),
                contracts::MASK.into(),
            ]
        } else {
            vec![contracts::EVENTS.into(), contracts::MASK.into()]
        },
        privacy: policy,
        study: run.experiment.clone(),
        instruments: run.instruments.clone(),
        panels,
        specimens: run.specimens.clone(),
        samples,
        acquisitions,
        controls: vec![],
        knowledge: vec![],
        artifacts,
        design: None,
    }
    .sealed();
    Built { root, manifest }
}
