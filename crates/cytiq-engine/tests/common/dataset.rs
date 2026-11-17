//! The reference dataset of `data/fixtures/dataset`, built with the real
//! commands: `cytiq export events` for every layer, `cytiq compensate` and
//! `cytiq transform` for panel M's chain, and the library for the manifest.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cytiq_fcs::{FcsReadLimits, FcsReader};
use cytiq_manifest::{RunManifest, SignalState};
use cytiq_population::dataset::{
    self, AcquisitionRole, ArtifactEntry, ArtifactRole, DatasetManifest, KnowledgeEntry,
    PanelEntry, SampleEntry, contracts,
};
use cytiq_population::events;
use cytiq_population::issue::Issues;
use cytiq_population::knowledge::KnowledgeArtifact;
use cytiq_population::privacy::PrivacyPolicy;

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures")
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

fn cytiq(args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn metadata(path: &Path) -> cytiq_fcs::FcsMetadata {
    FcsReader::open(path)
        .unwrap()
        .stream(std::num::NonZeroUsize::MIN, FcsReadLimits::default())
        .unwrap()
        .metadata
}

/// Copy `source` to `root/location` and describe it.
fn place(root: &Path, source: &Path, location: &str) -> PathBuf {
    let target = root.join(location);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(source, &target).unwrap();
    target
}

/// Export `input` with the CLI into `work`, place the table under `root` and
/// describe it.
fn layer(
    root: &Path,
    work: &Path,
    input: &Path,
    location: &str,
    panel: &str,
    (base_state, state): (&str, &str),
    parents: Vec<String>,
) -> ArtifactEntry {
    let staged = work.join(location.replace('/', "_"));
    cytiq(&[
        "export",
        "events",
        "--input",
        utf8(input),
        "--output",
        utf8(&staged),
        "--layer",
        state,
        "--base-state",
        base_state,
    ]);
    let placed = place(root, &staged, location);
    let mut issues = Issues::new();
    let summary = events::verify_file(&placed, "", &mut issues)
        .unwrap_or_else(|| panic!("{location}: {:?}", issues.items()));
    ArtifactEntry::signal_layer(
        &summary.descriptor,
        &summary.values_sha256,
        Some(panel.into()),
        parents,
        location.into(),
        dataset::content_of(&placed).unwrap(),
    )
}

/// Panel M's chain: compensated with the file's own spillover, then
/// arcsinh-transformed, plus the raw detector-margin mask `cytiq compensate`
/// writes.
fn chain(
    root: &Path,
    work: &Path,
    input: &Path,
    id: &str,
    raw: &ArtifactEntry,
) -> Vec<ArtifactEntry> {
    let compensated = work.join(format!("{id}.comp.fcs"));
    let transformed = work.join(format!("{id}.trans.fcs"));
    cytiq(&[
        "compensate",
        "--input",
        utf8(input),
        "--output",
        utf8(&compensated),
    ]);
    cytiq(&[
        "transform",
        "--input",
        utf8(&compensated),
        "--output",
        utf8(&transformed),
        "--method",
        "arcsinh",
        "--cofactor",
        "150",
    ]);
    let comp = layer(
        root,
        work,
        &compensated,
        &format!("events/M/{id}.compensated.parquet"),
        "M",
        ("raw", "compensated"),
        vec![raw.semantic_id.clone()],
    );
    let trans = layer(
        root,
        work,
        &transformed,
        &format!("events/M/{id}.transformed.parquet"),
        "M",
        ("raw", "transformed"),
        vec![comp.semantic_id.clone()],
    );
    let location = format!("masks/M/{id}.margins.parquet");
    let mask = place(
        root,
        &work.join(format!("{id}.comp.masks.parquet")),
        &location,
    );
    let margins = ArtifactEntry::object(
        ArtifactRole::TechnicalMask,
        contracts::MASK,
        raw.acquisition.clone(),
        vec![raw.semantic_id.clone()],
        location,
        dataset::content_of(&mask).unwrap(),
    );
    vec![comp, trans, margins]
}

fn base_state(state: SignalState) -> &'static str {
    match state {
        SignalState::Raw => "raw",
        SignalState::Compensated => "compensated",
        SignalState::Unmixed => "unmixed",
    }
}

/// Knowledge and gating definitions the dataset refers to.
fn definitions(root: &Path) -> (Vec<KnowledgeEntry>, ArtifactEntry) {
    let specimen = fixtures().join("specimen");
    let knowledge = ["tree.json", "flowcyt-bm.projection.json"]
        .iter()
        .map(|name| {
            let artifact = KnowledgeArtifact::load(&specimen.join(name)).unwrap();
            KnowledgeEntry {
                kind: artifact.value()["artifact_type"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                sha256: artifact.sha256().to_owned(),
            }
        })
        .collect();
    let gates = place(root, &specimen.join("gates.xml"), "definitions/gates.xml");
    let gating = ArtifactEntry::object(
        ArtifactRole::Gating,
        contracts::GATING_ML,
        None,
        vec![],
        "definitions/gates.xml".into(),
        dataset::content_of(&gates).unwrap(),
    );
    (knowledge, gating)
}

/// Build the reference dataset under `root` (objects and `manifest.json`),
/// using `work` for intermediate files and provenance records.
pub fn build(root: &Path, work: &Path) -> DatasetManifest {
    let run = RunManifest::load(&fixtures().join("dataset/run.yaml")).unwrap();
    let policy = PrivacyPolicy::deidentified();
    let mut panels: Vec<PanelEntry> = Vec::new();
    let mut acquisitions = Vec::new();
    let mut samples = Vec::new();
    let mut artifacts = Vec::new();
    for sample in &run.samples {
        let panel = run.panel_of(sample).unwrap();
        let label = panel.label();
        let raw_metadata = metadata(&sample.file);
        if !panels.iter().any(|p| p.id == label) {
            panels.push(dataset::panel_from_run(&run, panel, &raw_metadata).unwrap());
        }
        let acquisition = dataset::acquisition_entry(
            &sample.file,
            &raw_metadata,
            label,
            AcquisitionRole::Sample,
            &policy,
        )
        .unwrap();
        samples.push(SampleEntry {
            id: sample.id.clone(),
            panel: label.to_owned(),
            specimen: sample.specimen.clone(),
            acquisition: acquisition.id.clone(),
        });
        acquisitions.push(acquisition);
        let state = base_state(panel.signal_state);
        let raw = layer(
            root,
            work,
            &sample.file,
            &format!("events/{label}/{}.{state}.parquet", sample.id),
            label,
            (state, state),
            vec![],
        );
        if label == "M" {
            let derived = chain(root, work, &sample.file, &sample.id, &raw);
            artifacts.push(raw);
            artifacts.extend(derived);
        } else {
            artifacts.push(raw);
        }
    }
    let (knowledge, gating) = definitions(root);
    artifacts.push(gating);
    let manifest = DatasetManifest {
        schema_version: dataset::SCHEMA_VERSION,
        artifact_type: dataset::ARTIFACT_TYPE.into(),
        dataset_id: None,
        contracts: vec![
            contracts::EVENTS.into(),
            contracts::MASK.into(),
            contracts::GATING_ML.into(),
        ],
        privacy: policy,
        study: run.experiment.clone(),
        instruments: run.instruments.clone(),
        panels,
        specimens: run.specimens.clone(),
        samples,
        acquisitions,
        controls: vec![],
        knowledge,
        artifacts,
        design: None,
    }
    .sealed();
    fs::write(root.join("manifest.json"), manifest.canonical_json()).unwrap();
    manifest
}
