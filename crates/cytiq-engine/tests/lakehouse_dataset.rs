//! The cohort dataset `parity/lakehouse/labels.py` queries with `DuckDB`,
//! built with the real commands: four subjects in two groups, one with a
//! second panel, each acquisition with its QC mask, gate membership,
//! `FlowSOM` clusters of one shared model and `population observe` package,
//! plus a classifier acquisition with abstentions; then its analytics
//! product. `just lakehouse-interop` runs it with `CYTIQ_LAKEHOUSE_DATASET`
//! set to the directory to write.

use std::path::{Path, PathBuf};
use std::process::Command;

use cytiq_fcs::{ColumnData, FcsReader, FcsWriteOptions, FcsWriteParameter};
use cytiq_population::dataset::{
    self, AcquisitionRole, ArtifactEntry, ArtifactRole, ChannelEntry, DatasetManifest, Design,
    Factor, FactorLevel, PanelEntry, SampleDesign, SampleEntry, SpecimenDesign, SubjectEntry,
    contracts,
};
use cytiq_population::issue::Issues;
use cytiq_population::privacy::{Disclosure, PrivacyPolicy, PrivacyProfile};
use cytiq_population::{ArtifactRef, artifact::write_json, events, knowledge::KnowledgeArtifact};
use serde_json::{Value, json};

const FLUORESCENCE: &str = "FL1-A,FL2-A,FL3-A,FL4-A,FL5-A";
/// Events of each subsampled acquisition.
const EVENTS: usize = 3_000;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures")
        .join(name)
}

fn text(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

fn cytiq(directory: &Path, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .current_dir(directory)
        .args(args)
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "`cytiq {}`: {}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One acquisition of the cohort.
struct Acquisition {
    /// Run manifest sample id, also the file stem.
    run_id: &'static str,
    /// Dataset sample id (a pseudonym).
    sample: &'static str,
    panel: &'static str,
    specimen: &'static str,
    /// Rows of the pipeline fixture it holds.
    offset: usize,
}

const ACQUISITIONS: [Acquisition; 5] = [
    Acquisition {
        run_id: "A1",
        sample: "S1-P1",
        panel: "P1",
        specimen: "SP1",
        offset: 0,
    },
    Acquisition {
        run_id: "A2",
        sample: "S2-P1",
        panel: "P1",
        specimen: "SP2",
        offset: 3_000,
    },
    Acquisition {
        run_id: "A3",
        sample: "S3-P1",
        panel: "P1",
        specimen: "SP3",
        offset: 9_000,
    },
    Acquisition {
        run_id: "A4",
        sample: "S4-P1",
        panel: "P1",
        specimen: "SP4",
        offset: 12_000,
    },
    Acquisition {
        run_id: "A5",
        sample: "S1-P2",
        panel: "P2",
        specimen: "SP1",
        offset: 6_000,
    },
];

/// Rows `offset..offset + EVENTS` of the pipeline fixture as a raw file of
/// their own: a distinct acquisition with the fixture's channels and
/// spillover.
fn subsample(path: &Path, offset: usize) {
    let source = FcsReader::open(fixture("pipeline/sample.fcs"))
        .unwrap()
        .read()
        .unwrap();
    let parameters: Vec<FcsWriteParameter> = source
        .parameters
        .iter()
        .map(FcsWriteParameter::from_source)
        .collect();
    let columns: Vec<Vec<f32>> = source
        .data
        .columns
        .iter()
        .map(|column| match column {
            ColumnData::F32(values) => values[offset..offset + EVENTS].to_vec(),
            _ => panic!("the pipeline fixture stores float32"),
        })
        .collect();
    let mut options = FcsWriteOptions::new();
    for key in ["$SPILLOVER", "$CYT"] {
        if let Some(value) = source.text.get(key) {
            options = options.with_text_keyword(key, value.to_owned());
        }
    }
    cytiq_fcs::write_fcs3_1_f32_with_options(path, &parameters, &columns, &options).unwrap();
}

fn digest(path: &Path) -> String {
    KnowledgeArtifact::load(path).unwrap().sha256().into()
}

/// Gates `All`, `T`, `Activated` and `Zero` of `specimen/gates.xml`, bound
/// to the projection's populations.
fn gate_panel(name: &str) -> Value {
    let projection = fixture(&format!("specimen/{name}.projection.json"));
    let target = |id: &str| json!({"kind": "population", "id": id, "scope": "cells"});
    json!({"projection": {"path": projection, "sha256": digest(&projection)},
    "sources": {"gates": {
        "format": "gate_membership",
        "definition": ArtifactRef::of(&fixture("specimen/gates.xml")).unwrap(),
        "bindings": [
            {"label": "All", "target": target("cells")},
            {"label": "T", "target": target("t_cells"), "denominator": "All"},
            {"label": "Activated", "target": target("activated"), "denominator": "All"},
            {"label": "Zero", "target": target("zero"), "denominator": "All"}
        ]}}})
}

/// QC, gates, layers and the observation package of one acquisition.
fn process(dir: &Path, a: &Acquisition) {
    let id = a.run_id;
    let gates = fixture("specimen/gates.xml");
    let (raw, comp, trans) = (
        format!("{id}.fcs"),
        format!("{id}.comp.fcs"),
        format!("{id}.trans.fcs"),
    );
    let (analysis, clean) = (format!("{id}.analysis.parquet"), format!("{id}.clean"));
    let steps: Vec<Vec<String>> = vec![
        vec![
            "compensate".into(),
            "--input".into(),
            raw.clone(),
            "--output".into(),
            comp.clone(),
        ],
        vec![
            "transform".into(),
            "--input".into(),
            comp.clone(),
            "--output".into(),
            trans.clone(),
        ],
        vec![
            "clean".into(),
            "--input".into(),
            raw.clone(),
            "--output".into(),
            clean.clone(),
        ],
        vec![
            "mask".into(),
            "combine".into(),
            "--mask".into(),
            format!("{id}.comp.masks.parquet"),
            "--mask".into(),
            format!("{clean}/masks.parquet"),
            "--output".into(),
            analysis.clone(),
        ],
        vec![
            "qc".into(),
            "--manifest".into(),
            "run.json".into(),
            "--sample".into(),
            id.into(),
            "--mask".into(),
            analysis.clone(),
            "--output".into(),
            format!("{id}.qc.json"),
        ],
        vec![
            "gate".into(),
            "--input".into(),
            raw.clone(),
            "--gating-ml".into(),
            text(&gates).into(),
            "--mask".into(),
            analysis.clone(),
            "--membership".into(),
            format!("{id}.membership.parquet"),
            "--output".into(),
            format!("{id}.counts.json"),
            "--format".into(),
            "json".into(),
        ],
        vec![
            "export".into(),
            "events".into(),
            "--input".into(),
            raw,
            "--output".into(),
            format!("{id}.raw.parquet"),
            "--layer".into(),
            "raw".into(),
        ],
        vec![
            "export".into(),
            "events".into(),
            "--input".into(),
            trans,
            "--output".into(),
            format!("{id}.transformed.parquet"),
            "--layer".into(),
            "transformed".into(),
        ],
    ];
    for step in &steps {
        cytiq(dir, &step.iter().map(String::as_str).collect::<Vec<_>>());
    }
}

/// Labels of one acquisition, after `FlowSOM` and `observe` ran.
fn label(dir: &Path, a: &Acquisition) {
    let id = a.run_id;
    let gates = fixture("specimen/gates.xml");
    let analysis = format!("{id}.analysis.parquet");
    let runs: [Vec<String>; 3] = [
        vec![
            "--kind".into(),
            "qc".into(),
            "--input".into(),
            analysis.clone(),
            "--producer-provenance".into(),
            format!("{analysis}.provenance.json"),
            "--events".into(),
            format!("{id}.raw.parquet"),
            "--output".into(),
            format!("{id}.qc.labels.parquet"),
        ],
        vec![
            "--kind".into(),
            "gate".into(),
            "--input".into(),
            format!("{id}.membership.parquet"),
            "--producer-provenance".into(),
            format!("{id}.counts.json.provenance.json"),
            "--events".into(),
            format!("{id}.raw.parquet"),
            "--qc-mask".into(),
            analysis.clone(),
            "--gating-ml".into(),
            text(&gates).into(),
            "--output".into(),
            format!("{id}.gate.labels.parquet"),
        ],
        vec![
            "--kind".into(),
            "cluster".into(),
            "--input".into(),
            format!("{id}.clusters.csv"),
            "--producer-provenance".into(),
            format!("{id}.clusters.csv.provenance.json"),
            "--events".into(),
            format!("{id}.transformed.parquet"),
            "--qc-mask".into(),
            analysis,
            "--flowsom-model".into(),
            "model.json".into(),
            "--output".into(),
            format!("{id}.cluster.labels.parquet"),
        ],
    ];
    for run in &runs {
        let mut args = vec!["export", "labels"];
        args.extend(run.iter().map(String::as_str));
        cytiq(dir, &args);
    }
}

/// Run every command of the cohort in `dir`.
#[allow(clippy::too_many_lines)] // One literal argument list per command.
fn commands(dir: &Path) {
    for a in &ACQUISITIONS {
        subsample(&dir.join(format!("{}.fcs", a.run_id)), a.offset);
    }
    let samples: Vec<Value> = ACQUISITIONS
        .iter()
        .map(|a| {
            json!({"id": a.run_id, "file": dir.join(format!("{}.fcs", a.run_id)),
                   "panel": a.panel, "specimen": a.specimen})
        })
        .collect();
    write_json(
        &dir.join("run.json"),
        &json!({"schema_version": 2, "cytometry": "conventional",
        "panels": [
            {"name": "P1", "markers": [{"marker": "CD3"}], "qc": {"min_events": 1}},
            {"name": "P2", "markers": [{"marker": "CD3"}], "qc": {"min_events": 1}}],
        "samples": samples}),
    )
    .unwrap();
    // SP1 was measured on both panels, the others on P1 only.
    write_json(
        &dir.join("layout.json"),
        &json!({"schema_version": 1,
        "protocol_id": "lakehouse-cohort",
        "panels": {"P1": gate_panel("P1"), "P2": gate_panel("P2")}}),
    )
    .unwrap();
    write_json(
        &dir.join("layout-p1.json"),
        &json!({"schema_version": 1,
        "protocol_id": "lakehouse-cohort-p1",
        "panels": {"P1": gate_panel("P1")}}),
    )
    .unwrap();
    for a in &ACQUISITIONS {
        process(dir, a);
    }
    cytiq(
        dir,
        &[
            "flowsom",
            "train",
            "--input",
            "A1.trans.fcs",
            "--output",
            "model.json",
            "--channels",
            FLUORESCENCE,
            "--mask",
            "A1.analysis.parquet",
            "--metaclusters",
            "7",
        ],
    );
    for a in &ACQUISITIONS {
        let id = a.run_id;
        let (trans, csv, analysis) = (
            format!("{id}.trans.fcs"),
            format!("{id}.clusters.csv"),
            format!("{id}.analysis.parquet"),
        );
        cytiq(
            dir,
            &[
                "flowsom",
                "apply",
                "--input",
                &trans,
                "--model",
                "model.json",
                "--output",
                &csv,
                "--mask",
                &analysis,
            ],
        );
    }
    for specimen in ["SP1", "SP2", "SP3", "SP4"] {
        let layout = if specimen == "SP1" {
            "layout.json"
        } else {
            "layout-p1.json"
        };
        cytiq(
            dir,
            &[
                "specimen",
                "plan",
                "--manifest",
                "run.json",
                "--layout",
                layout,
                "--specimen",
                specimen,
                "--output",
                &format!("{specimen}.plan"),
            ],
        );
    }
    for a in &ACQUISITIONS {
        let id = a.run_id;
        cytiq(
            dir,
            &[
                "population",
                "observe",
                "--plan",
                &format!("{}.plan/plan.json", a.specimen),
                "--acquisition",
                id,
                "--source",
                "gates",
                "--annotations",
                &format!("{id}.membership.parquet"),
                "--producer-provenance",
                &format!("{id}.counts.json.provenance.json"),
                "--mask",
                &format!("{id}.analysis.parquet"),
                "--qc-report",
                &format!("{id}.qc.json"),
                "--qc-provenance",
                &format!("{id}.qc.json.provenance.json"),
                "--output",
                &format!("{id}.package"),
            ],
        );
        label(dir, a);
    }
    let classify = fixture("classify/raw.fcs");
    let bundle = fixture("classify/v2/binary");
    cytiq(
        dir,
        &[
            "export",
            "events",
            "--input",
            text(&classify),
            "--output",
            "C1.raw.parquet",
            "--layer",
            "raw",
        ],
    );
    cytiq(
        dir,
        &[
            "classify",
            "--input",
            text(&classify),
            "--model",
            text(&bundle),
            "--output",
            "C1.classified",
            "--abstain-margin",
            "0.2",
            "--threads",
            "1",
        ],
    );
    cytiq(
        dir,
        &[
            "export",
            "labels",
            "--kind",
            "classifier",
            "--input",
            "C1.classified/predictions.parquet",
            "--producer-provenance",
            "C1.classified/provenance.json",
            "--events",
            "C1.raw.parquet",
            "--output",
            "C1.classifier.labels.parquet",
        ],
    );
}

fn verified_layer(dir: &Path, location: &str) -> events::EventsSummary {
    let mut issues = Issues::new();
    events::verify_file(&dir.join(location), "", &mut issues)
        .unwrap_or_else(|| panic!("{location}: {:?}", issues.items()))
}

/// A panel with the channels of `layer`, markers from `$PnS`.
fn panel(id: &str, layer: &events::EventsSummary) -> PanelEntry {
    PanelEntry {
        id: id.into(),
        cytometry: cytiq_manifest::Cytometry::Conventional,
        signal_state: cytiq_manifest::SignalState::Raw,
        instrument: None,
        channels: layer
            .channels
            .iter()
            .map(|c| ChannelEntry {
                index: c.index,
                name: c.name.clone(),
                kind: None,
                detector: None,
                marker: c.label.clone(),
                reporter: None,
                reagent: None,
            })
            .collect(),
    }
}

fn object(
    dir: &Path,
    role: ArtifactRole,
    contract: &str,
    acquisition: &str,
    location: &str,
) -> ArtifactEntry {
    ArtifactEntry::object(
        role,
        contract,
        Some(acquisition.into()),
        vec![],
        location.into(),
        dataset::content_of(&dir.join(location)).unwrap(),
    )
}

fn labels(dir: &Path, panel: &str, location: &str) -> ArtifactEntry {
    let mut issues = Issues::new();
    let summary = cytiq_population::labels::verify_file(&dir.join(location), "", &mut issues)
        .unwrap_or_else(|| panic!("{location}: {:?}", issues.items()));
    ArtifactEntry::labels(
        &summary,
        Some(panel.into()),
        vec![],
        location.into(),
        dataset::content_of(&dir.join(location)).unwrap(),
    )
}

fn layer(dir: &Path, panel: &str, location: &str, parents: Vec<String>) -> ArtifactEntry {
    let summary = verified_layer(dir, location);
    ArtifactEntry::signal_layer(
        &summary.descriptor,
        &summary.values_sha256,
        Some(panel.into()),
        parents,
        location.into(),
        dataset::content_of(&dir.join(location)).unwrap(),
    )
}

fn design() -> Design {
    let factors = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    let subjects = [
        ("P01", "case"),
        ("P02", "case"),
        ("P03", "control"),
        ("P04", "control"),
        ("P05", "control"),
    ];
    Design {
        factors: vec![
            Factor {
                name: "group".into(),
                level: FactorLevel::Subject,
            },
            Factor {
                name: "timepoint".into(),
                level: FactorLevel::Specimen,
            },
            Factor {
                name: "batch".into(),
                level: FactorLevel::Sample,
            },
        ],
        subjects: subjects
            .iter()
            .map(|(id, group)| SubjectEntry {
                id: (*id).into(),
                factors: factors(&[("group", group)]),
            })
            .collect(),
        specimens: ["SP1", "SP2", "SP3", "SP4", "SP5"]
            .iter()
            .zip(subjects)
            .map(|(specimen, (subject, _))| SpecimenDesign {
                specimen: (*specimen).into(),
                subject: subject.into(),
                factors: factors(&[("timepoint", "D0")]),
            })
            .collect(),
        samples: ACQUISITIONS
            .iter()
            .map(|a| a.sample)
            .chain(["S5-C"])
            .enumerate()
            .map(|(i, sample)| SampleDesign {
                sample: sample.into(),
                factors: factors(&[("batch", if i % 2 == 0 { "B1" } else { "B2" })]),
            })
            .collect(),
    }
}

/// The dataset manifest of what [`commands`] wrote.
#[allow(clippy::too_many_lines)] // Every entry of one manifest.
fn manifest(dir: &Path) -> DatasetManifest {
    let policy = PrivacyPolicy {
        profile: PrivacyProfile::Controlled,
        disclosures: vec![Disclosure {
            contract: "LAKEHOUSE-TEST".into(),
            purpose: "SQL integration tests".into(),
            keywords: vec![],
            source_locations: false,
            provenance_records: true,
        }],
    };
    let mut panels: Vec<PanelEntry> = Vec::new();
    let mut acquisitions = Vec::new();
    let mut samples = Vec::new();
    let mut artifacts = Vec::new();
    let entries = ACQUISITIONS
        .iter()
        .map(|a| {
            (
                a.run_id,
                a.sample,
                a.panel,
                a.specimen,
                dir.join(format!("{}.fcs", a.run_id)),
            )
        })
        .chain([("C1", "S5-C", "C", "SP5", fixture("classify/raw.fcs"))]);
    for (id, sample, panel_id, specimen, raw) in entries {
        let metadata = FcsReader::open(&raw)
            .unwrap()
            .stream(
                std::num::NonZeroUsize::MIN,
                cytiq_fcs::FcsReadLimits::default(),
            )
            .unwrap()
            .metadata;
        let acquisition =
            dataset::acquisition_entry(&raw, &metadata, panel_id, AcquisitionRole::Sample, &policy)
                .unwrap();
        let raw_layer = layer(dir, panel_id, &format!("{id}.raw.parquet"), vec![]);
        if !panels.iter().any(|p| p.id == panel_id) {
            panels.push(panel(
                panel_id,
                &verified_layer(dir, &format!("{id}.raw.parquet")),
            ));
        }
        samples.push(SampleEntry {
            id: sample.into(),
            panel: panel_id.into(),
            specimen: Some(specimen.into()),
            acquisition: acquisition.id.clone(),
        });
        let acq = acquisition.id.clone();
        acquisitions.push(acquisition);
        if id == "C1" {
            artifacts.push(raw_layer);
            artifacts.push(labels(dir, panel_id, "C1.classifier.labels.parquet"));
            continue;
        }
        let transformed = layer(
            dir,
            panel_id,
            &format!("{id}.transformed.parquet"),
            vec![raw_layer.semantic_id.clone()],
        );
        artifacts.push(raw_layer);
        artifacts.push(transformed);
        for kind in ["qc", "gate", "cluster"] {
            artifacts.push(labels(
                dir,
                panel_id,
                &format!("{id}.{kind}.labels.parquet"),
            ));
        }
        artifacts.push(object(
            dir,
            ArtifactRole::Observations,
            contracts::OBSERVATIONS,
            &acq,
            &format!("{id}.package/observations.parquet"),
        ));
        artifacts.push(object(
            dir,
            ArtifactRole::Annotation,
            contracts::ANNOTATIONS[0],
            &acq,
            &format!("{id}.package/event-annotations.json"),
        ));
    }
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
        panels,
        specimens: ["SP1", "SP2", "SP3", "SP4", "SP5"]
            .iter()
            .map(|id| serde_json::from_value(json!({"id": id})).unwrap())
            .collect(),
        samples,
        acquisitions,
        controls: vec![],
        knowledge: vec![],
        artifacts,
        design: Some(design()),
    }
    .sealed()
}

#[test]
#[ignore = "builds the cohort parity/lakehouse/labels.py queries (just lakehouse-interop)"]
fn writes_the_lakehouse_analytics_dataset() {
    let temporary = tempfile::tempdir().unwrap();
    let dir = std::env::var_os("CYTIQ_LAKEHOUSE_DATASET")
        .map_or_else(|| temporary.path().to_path_buf(), PathBuf::from);
    std::fs::create_dir_all(&dir).unwrap();
    commands(&dir);
    let manifest = manifest(&dir);
    std::fs::write(dir.join("dataset.json"), manifest.canonical_json()).unwrap();
    cytiq(
        &dir,
        &[
            "dataset",
            "validate",
            "--manifest",
            "dataset.json",
            "--output",
            "validation.json",
            "--verify-objects",
        ],
    );
    cytiq(
        &dir,
        &[
            "dataset",
            "export",
            "--manifest",
            "dataset.json",
            "--output",
            "product",
        ],
    );
}
