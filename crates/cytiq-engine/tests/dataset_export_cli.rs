//! `cytiq dataset export`: the analytics product of the reference dataset,
//! and of a dataset whose observations `cytiq population observe` computed
//! from the same gate membership `cytiq export labels` converted: their
//! counts and fractions must agree exactly.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arrow_array::{Array, StringArray, UInt64Array};
use cytiq_mask::EventMask;
use cytiq_population::dataset::{
    self, AcquisitionRole, ArtifactEntry, ArtifactRole, ChannelEntry, DatasetManifest, Design,
    PanelEntry, SampleEntry, SpecimenDesign, SubjectEntry, contracts,
};
use cytiq_population::privacy::{Disclosure, PrivacyPolicy, PrivacyProfile};
use cytiq_population::{ArtifactRef, artifact::write_json, events, knowledge::KnowledgeArtifact};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{Value, json};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures")
        .join(name)
}

fn text(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

fn cytiq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run")
}

fn succeed(args: &[&str]) {
    let output = cytiq(args);
    assert!(
        output.status.success(),
        "`cytiq {}`: {}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn product(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path.join("product.json")).unwrap()).unwrap()
}

/// `(label_column, label_value, state, count)` rows of one label table.
fn counts(product: &Path, annotation: &str) -> Vec<(String, Option<String>, String, u64)> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(
        File::open(product.join("tables/label_counts.parquet")).unwrap(),
    )
    .unwrap()
    .build()
    .unwrap();
    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let text = |name: &str| {
            batch
                .column_by_name(name)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .clone()
        };
        let (ids, columns, values, states) = (
            text("annotation_id"),
            text("label_column"),
            text("label_value"),
            text("state"),
        );
        let n = batch
            .column_by_name("count")
            .unwrap()
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap()
            .clone();
        for i in 0..batch.num_rows() {
            if ids.value(i) == annotation {
                rows.push((
                    columns.value(i).to_owned(),
                    (!values.is_null(i)).then(|| values.value(i).to_owned()),
                    states.value(i).to_owned(),
                    n.value(i),
                ));
            }
        }
    }
    rows
}

#[test]
fn the_reference_dataset_exports_its_analytics_product() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("product");
    let manifest = fixture("dataset/reference/manifest.json");
    succeed(&[
        "dataset",
        "export",
        "--manifest",
        text(&manifest),
        "--output",
        text(&output),
    ]);
    let product = product(&output);
    assert_eq!(product["contract"], "cytiq.analytics.product.v1");
    assert_eq!(product["cohort"]["available"], true);
    let views = std::fs::read_to_string(output.join("views.sql")).unwrap();
    for view in [
        "\"events_M_raw\"",
        "\"labels_gate\"",
        "\"gate_membership\"",
        "\"cohort_labels\"",
    ] {
        assert!(views.contains(view), "{view}");
    }
    let committed: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(product["dataset_id"], committed["dataset_id"]);
    let gates = committed["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["location"] == "labels/M/S12-M.gates.parquet")
        .unwrap()["semantic_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let counts = counts(&output, &gates);
    let count = |column: &str, value: &str| {
        counts
            .iter()
            .find(|(c, v, s, _)| c == column && v.as_deref() == Some(value) && s == "assessed")
            .map_or(0, |row| row.3)
    };
    // Nested, overlapping and empty gates count independently.
    assert_eq!(count("cells", "true"), 3);
    assert_eq!(count("CD45+", "true"), 2);
    assert_eq!(count("CD34+", "true"), 2);
    assert_eq!(count("blasts", "true"), 1);
    assert_eq!(count("large", "true"), 0);
    assert_eq!(count("large", "false"), 3);
    let record: Value =
        serde_json::from_slice(&std::fs::read(output.join("provenance.json")).unwrap()).unwrap();
    assert_eq!(record["tool"], "cytiq dataset export");
    assert!(record["outputs"].as_array().unwrap().len() > 17);
}

#[test]
fn missing_invalid_and_existing_are_classified() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("product");
    let missing = cytiq(&[
        "dataset",
        "export",
        "--manifest",
        text(&directory.path().join("absent.json")),
        "--output",
        text(&output),
    ]);
    assert_eq!(missing.status.code(), Some(4));
    // A manifest that does not validate publishes nothing.
    let reference = fixture("dataset/reference");
    let mut broken: Value =
        serde_json::from_slice(&std::fs::read(reference.join("manifest.json")).unwrap()).unwrap();
    broken["samples"][0]["acquisition"] = json!("f".repeat(64));
    let broken_path = directory.path().join("broken.json");
    std::fs::write(&broken_path, broken.to_string()).unwrap();
    let invalid = cytiq(&[
        "dataset",
        "export",
        "--manifest",
        text(&broken_path),
        "--root",
        text(&reference),
        "--output",
        text(&output),
    ]);
    assert_eq!(
        invalid.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&invalid.stderr)
    );
    assert!(!output.exists());
    let manifest = reference.join("manifest.json");
    let args = [
        "dataset",
        "export",
        "--manifest",
        text(&manifest),
        "--output",
        text(&output),
    ];
    succeed(&args);
    assert_eq!(cytiq(&args).status.code(), Some(4));
}

/// `from` and everything under it, copied to `to`.
#[cfg(unix)]
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn objects_are_not_read_through_a_link_out_of_the_root() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("dataset");
    copy_tree(&fixture("dataset/reference"), &root);
    let output = directory.path().join("product");
    let report = directory.path().join("report.json");
    let outside = directory.path().join("outside.parquet");
    let labels = root.join("labels/M/S12-M.gates.parquet");
    std::fs::rename(&labels, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &labels).unwrap();
    // From the root, so that the default root is the empty path.
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_cytiq"))
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap()
    };
    let export = [
        "dataset",
        "export",
        "--manifest",
        "manifest.json",
        "--output",
    ];
    let refused = run(&[&export[..], &[text(&output)]].concat());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("symbolic link"), "{stderr}");
    assert!(!output.exists());

    let validate = [
        "dataset",
        "validate",
        "--manifest",
        "manifest.json",
        "--verify-objects",
    ];
    let invalid = run(&[&validate[..], &["--output", text(&report)]].concat());
    assert_eq!(invalid.status.code(), Some(3));
    let document: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    let issues = document["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|issue| issue["code"] == "invalid_reference"
                && issue["message"].as_str().unwrap().contains("symbolic link"))
    );
    // The record names what was read, and the link's target was not.
    let record = cytiq_provenance::record_path_for_file(&report);
    let record: Value = serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
    let inputs = record["inputs"].as_array().unwrap();
    assert!(inputs.len() > 1);
    assert!(
        !inputs
            .iter()
            .any(|input| input["path"].as_str().unwrap().contains("S12-M.gates"))
    );

    std::fs::remove_file(&labels).unwrap();
    std::fs::rename(&outside, &labels).unwrap();
    let accepted = run(&[&export[..], &[text(&output)]].concat());
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

fn digest(path: &Path) -> String {
    KnowledgeArtifact::load(path).unwrap().sha256().into()
}

/// The specimen layout of `specimen_cli.rs`: four gates, bound to
/// populations of the P1 projection.
fn layout() -> Value {
    let projection = fixture("specimen/P1.projection.json");
    let target = |id: &str| json!({"kind": "population", "id": id, "scope": "cells"});
    json!({"schema_version": 1, "protocol_id": "synthetic-gates", "panels": {"P1": {
    "projection": {"path": projection, "sha256": digest(&projection)},
    "sources": {"gates": {
        "format": "gate_membership",
        "definition": ArtifactRef::of(&fixture("specimen/gates.xml")).unwrap(),
        "bindings": [
            {"label": "All", "target": target("cells")},
            {"label": "T", "target": target("t_cells"), "denominator": "All"},
            {"label": "Activated", "target": target("activated"), "denominator": "All"},
            {"label": "Zero", "target": target("zero"), "denominator": "All"}
        ]}}}}})
}

/// Panel P1 with the channels of its raw layer.
fn panel(layer: &events::EventsSummary) -> PanelEntry {
    PanelEntry {
        id: "P1".into(),
        cytometry: cytiq_manifest::Cytometry::Conventional,
        signal_state: cytiq_manifest::SignalState::Raw,
        instrument: None,
        channels: layer
            .channels
            .iter()
            .map(|channel| ChannelEntry {
                index: channel.index,
                name: channel.name.clone(),
                kind: None,
                detector: None,
                marker: None,
                reporter: None,
                reagent: None,
            })
            .collect(),
    }
}

/// Subject P01 of specimen S01.
fn design() -> Design {
    Design {
        factors: vec![],
        subjects: vec![SubjectEntry {
            id: "P01".into(),
            factors: std::collections::BTreeMap::default(),
        }],
        specimens: vec![SpecimenDesign {
            specimen: "S01".into(),
            subject: "P01".into(),
            factors: std::collections::BTreeMap::default(),
        }],
        samples: vec![],
    }
}

/// A dataset of one acquisition with its raw layer, the gate labels and the
/// observation package of the same membership, under a disclosure for the
/// package's annotation descriptor.
fn dataset(directory: &Path, raw: &Path) -> DatasetManifest {
    let mut issues = cytiq_population::issue::Issues::new();
    let layer = events::verify_file(&directory.join("raw.parquet"), "", &mut issues).unwrap();
    let labels = cytiq_population::labels::verify_file(
        &directory.join("gates.labels.parquet"),
        "",
        &mut issues,
    )
    .unwrap();
    let metadata = cytiq_fcs::FcsReader::open(raw)
        .unwrap()
        .stream(
            std::num::NonZeroUsize::MIN,
            cytiq_fcs::FcsReadLimits::default(),
        )
        .unwrap()
        .metadata;
    let policy = PrivacyPolicy {
        profile: PrivacyProfile::Controlled,
        disclosures: vec![Disclosure {
            contract: "DUA-2026-014".into(),
            purpose: "cohort analysis".into(),
            keywords: vec![],
            source_locations: false,
            provenance_records: true,
        }],
    };
    let acquisition =
        dataset::acquisition_entry(raw, &metadata, "P1", AcquisitionRole::Sample, &policy).unwrap();
    let id = acquisition.id.clone();
    let object = |role, contract: &str, location: &str| {
        ArtifactEntry::object(
            role,
            contract,
            Some(id.clone()),
            vec![],
            location.into(),
            dataset::content_of(&directory.join(location)).unwrap(),
        )
    };
    DatasetManifest {
        schema_version: dataset::SCHEMA_VERSION,
        artifact_type: dataset::ARTIFACT_TYPE.into(),
        dataset_id: None,
        contracts: vec![
            contracts::EVENTS.into(),
            contracts::LABELS.into(),
            contracts::OBSERVATIONS.into(),
            contracts::ANNOTATIONS[0].into(),
        ],
        privacy: policy,
        study: None,
        instruments: vec![],
        panels: vec![panel(&layer)],
        specimens: vec![serde_json::from_value(json!({"id": "S01"})).unwrap()],
        samples: vec![SampleEntry {
            id: "S01-P1".into(),
            panel: "P1".into(),
            specimen: Some("S01".into()),
            acquisition: id.clone(),
        }],
        acquisitions: vec![acquisition],
        controls: vec![],
        knowledge: vec![],
        artifacts: vec![
            ArtifactEntry::signal_layer(
                &layer.descriptor,
                &layer.values_sha256,
                Some("P1".into()),
                vec![],
                "raw.parquet".into(),
                dataset::content_of(&directory.join("raw.parquet")).unwrap(),
            ),
            ArtifactEntry::labels(
                &labels,
                Some("P1".into()),
                vec![],
                "gates.labels.parquet".into(),
                dataset::content_of(&directory.join("gates.labels.parquet")).unwrap(),
            ),
            object(
                ArtifactRole::Observations,
                contracts::OBSERVATIONS,
                "package/observations.parquet",
            ),
            object(
                ArtifactRole::Annotation,
                contracts::ANNOTATIONS[0],
                "package/event-annotations.json",
            ),
        ],
        design: Some(design()),
    }
    .sealed()
}

#[test]
#[allow(clippy::too_many_lines)] // One workflow: observe and label the same membership.
fn population_observe_counts_match_the_label_tables() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    let raw = fixture("pipeline/sample.fcs");
    let manifest = directory.join("run.json");
    write_json(
        &manifest,
        &json!({"schema_version": 2, "cytometry": "conventional",
        "panels": [{"name": "P1", "markers": [{"marker": "CD3"}], "qc": {"min_events": 1}}],
        "samples": [{"id": "A02", "file": raw, "panel": "P1", "specimen": "S01"}]}),
    )
    .unwrap();
    let layout_path = directory.join("layout.json");
    write_json(&layout_path, &layout()).unwrap();
    let planned = directory.join("planned");
    succeed(&[
        "specimen",
        "plan",
        "--manifest",
        text(&manifest),
        "--layout",
        text(&layout_path),
        "--specimen",
        "S01",
        "--output",
        text(&planned),
    ]);
    // Every tenth event fails technical QC.
    let mask = directory.join("mask.parquet");
    let events = cytiq_fcs::FcsReader::open(&raw).unwrap().read().unwrap();
    let acquisition = cytiq_fcs::acquisition_id(&raw, &events.text).unwrap();
    EventMask::new(
        acquisition,
        (0..events.event_count()).map(|i| i % 10 != 0).collect(),
        None,
    )
    .write_parquet(&mask)
    .unwrap();
    let qc = directory.join("qc.json");
    succeed(&[
        "qc",
        "--manifest",
        text(&manifest),
        "--sample",
        "A02",
        "--mask",
        text(&mask),
        "--output",
        text(&qc),
    ]);
    let gates = fixture("specimen/gates.xml");
    let counts_json = directory.join("counts.json");
    let membership = directory.join("membership.parquet");
    succeed(&[
        "gate",
        "--input",
        text(&raw),
        "--gating-ml",
        text(&gates),
        "--mask",
        text(&mask),
        "--membership",
        text(&membership),
        "--output",
        text(&counts_json),
        "--format",
        "json",
    ]);
    let producer = cytiq_provenance::record_path_for_file(&counts_json);
    succeed(&[
        "population",
        "observe",
        "--plan",
        text(&planned.join("plan.json")),
        "--acquisition",
        "A02",
        "--source",
        "gates",
        "--annotations",
        text(&membership),
        "--producer-provenance",
        text(&producer),
        "--mask",
        text(&mask),
        "--qc-report",
        text(&qc),
        "--qc-provenance",
        text(&cytiq_provenance::record_path_for_file(&qc)),
        "--output",
        text(&directory.join("package")),
    ]);
    succeed(&[
        "export",
        "events",
        "--input",
        text(&raw),
        "--output",
        text(&directory.join("raw.parquet")),
        "--layer",
        "raw",
    ]);
    succeed(&[
        "export",
        "labels",
        "--kind",
        "gate",
        "--input",
        text(&membership),
        "--producer-provenance",
        text(&producer),
        "--events",
        text(&directory.join("raw.parquet")),
        "--qc-mask",
        text(&mask),
        "--gating-ml",
        text(&gates),
        "--output",
        text(&directory.join("gates.labels.parquet")),
    ]);
    let manifest = dataset(directory, &raw);
    std::fs::write(directory.join("dataset.json"), manifest.canonical_json()).unwrap();
    let output = directory.join("product");
    succeed(&[
        "dataset",
        "export",
        "--manifest",
        text(&directory.join("dataset.json")),
        "--output",
        text(&output),
    ]);
    let product = product(&output);
    assert_eq!(
        product["parity"],
        json!([{"annotation_id": manifest.artifacts[1].semantic_id,
                "observations": 8, "level": "counts"}])
    );
    let states: Vec<_> = counts(&output, &manifest.artifacts[1].semantic_id);
    assert!(states.iter().any(|(c, v, s, n)| c == "Zero"
        && v.as_deref() == Some("false")
        && s == "assessed"
        && *n > 0));
}
