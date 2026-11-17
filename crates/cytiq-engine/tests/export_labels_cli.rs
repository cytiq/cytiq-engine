//! `cytiq export labels` on the outputs of the real producers: the pipeline
//! fixture's QC masks, gate membership and `FlowSOM` assignments, and the
//! binary and multiclass classifier fixtures.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arrow_array::{Array, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

const FLUORESCENCE: &str = "FL1-A,FL2-A,FL3-A,FL4-A,FL5-A";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures")
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

fn cytiq(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .current_dir(directory)
        .args(args)
        .output()
        .expect("command should run")
}

fn succeed(directory: &Path, args: &[&str]) {
    let output = cytiq(directory, args);
    assert!(
        output.status.success(),
        "`cytiq {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The pipeline fixture's QC chain, transformed layer, gates and `FlowSOM`.
fn pipeline(directory: &Path) {
    let sample = fixtures().join("pipeline/sample.fcs");
    let gates = fixtures().join("pipeline/gates.xml");
    let steps: [&[&str]; 9] = [
        &[
            "compensate",
            "--input",
            utf8(&sample),
            "--output",
            "s.comp.fcs",
        ],
        &["transform", "--input", "s.comp.fcs", "--output", "s.t.fcs"],
        &["clean", "--input", utf8(&sample), "--output", "clean"],
        &[
            "mask",
            "combine",
            "--mask",
            "s.comp.masks.parquet",
            "--mask",
            "clean/masks.parquet",
            "--output",
            "analysis.parquet",
        ],
        &[
            "export",
            "events",
            "--input",
            "s.t.fcs",
            "--output",
            "s.t.parquet",
            "--layer",
            "transformed",
        ],
        &[
            "gate",
            "--input",
            "s.t.fcs",
            "--gating-ml",
            utf8(&gates),
            "--output",
            "counts.json",
            "--format",
            "json",
            "--mask",
            "analysis.parquet",
            "--membership",
            "membership.parquet",
        ],
        &[
            "flowsom",
            "--input",
            "s.t.fcs",
            "--output",
            "flowsom.csv",
            "--channels",
            FLUORESCENCE,
            "--mask",
            "analysis.parquet",
            "--metaclusters",
            "7",
        ],
        &[
            "flowsom",
            "train",
            "--input",
            "s.t.fcs",
            "--output",
            "model.json",
            "--channels",
            FLUORESCENCE,
            "--mask",
            "analysis.parquet",
            "--metaclusters",
            "7",
        ],
        &[
            "flowsom",
            "apply",
            "--input",
            "s.t.fcs",
            "--model",
            "model.json",
            "--output",
            "applied.csv",
            "--mask",
            "analysis.parquet",
        ],
    ];
    for step in steps {
        succeed(directory, step);
    }
}

fn export_labels(
    directory: &Path,
    kind: &str,
    input: &str,
    record: &str,
    extra: &[&str],
) -> Output {
    let output = format!("{kind}.labels.parquet");
    let mut args = vec![
        "export",
        "labels",
        "--kind",
        kind,
        "--input",
        input,
        "--producer-provenance",
        record,
        "--events",
        "s.t.parquet",
        "--output",
        &output,
    ];
    args.extend_from_slice(extra);
    cytiq(directory, &args)
}

/// Rows per state of a label table, read with the standard arrow-rs reader.
fn states(path: &Path) -> std::collections::BTreeMap<String, usize> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let mut states = std::collections::BTreeMap::new();
    for batch in reader {
        let batch = batch.unwrap();
        let column = batch
            .column_by_name("state")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..column.len() {
            *states.entry(column.value(i).to_owned()).or_default() += 1;
        }
    }
    states
}

fn record(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(cytiq_provenance::record_path_for_file(path)).unwrap())
        .unwrap()
}

#[test]
fn every_kind_of_the_pipeline_exports_labels_with_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    pipeline(root);
    let gates = fixtures().join("pipeline/gates.xml");
    let runs: [(&str, &str, &str, Vec<&str>); 3] = [
        (
            "qc",
            "analysis.parquet",
            "analysis.parquet.provenance.json",
            vec![],
        ),
        (
            "gate",
            "membership.parquet",
            "counts.json.provenance.json",
            vec!["--qc-mask", "analysis.parquet", "--gating-ml", utf8(&gates)],
        ),
        (
            "cluster",
            "flowsom.csv",
            "flowsom.csv.provenance.json",
            vec!["--qc-mask", "analysis.parquet"],
        ),
    ];
    for (kind, input, producer, extra) in &runs {
        let output = export_labels(root, kind, input, producer, extra);
        assert!(
            output.status.success(),
            "{kind}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let table = root.join(format!("{kind}.labels.parquet"));
        let states = states(&table);
        assert_eq!(states.values().sum::<usize>(), 15_000, "{kind}: {states:?}");
        let record = record(&table);
        assert_eq!(record["tool"], "cytiq export labels");
        let roles: Vec<&str> = record["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| input["role"].as_str().unwrap())
            .collect();
        assert!(roles.contains(&"producer_provenance") && roles.contains(&"events"));
        assert_eq!(
            record["outputs"][0]["sha256"],
            cytiq_provenance::hash_file(&table).unwrap().1
        );
    }
    // The QC mask, the gates and the clusters reject the same events.
    let qc = states(&root.join("qc.labels.parquet"));
    let gate = states(&root.join("gate.labels.parquet"));
    let cluster = states(&root.join("cluster.labels.parquet"));
    assert_eq!(qc["qc_rejected"], gate["qc_rejected"]);
    assert_eq!(qc["qc_rejected"], cluster["qc_rejected"]);
    assert_eq!(qc["accepted"], gate["assessed"]);
    assert_eq!(qc["accepted"], cluster["classified"]);

    // A saved model's clusters are comparable: they carry a definition.
    let model = root.join("model.json");
    let output = cytiq(
        root,
        &[
            "export",
            "labels",
            "--kind",
            "cluster",
            "--input",
            "applied.csv",
            "--producer-provenance",
            "applied.csv.provenance.json",
            "--events",
            "s.t.parquet",
            "--flowsom-model",
            utf8(&model),
            "--qc-mask",
            "analysis.parquet",
            "--output",
            "applied.labels.parquet",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        footer(
            &root.join("applied.labels.parquet"),
            "cytiq.labels.definition_id"
        )
        .is_some()
    );
    assert!(
        footer(
            &root.join("cluster.labels.parquet"),
            "cytiq.labels.definition_id"
        )
        .is_none()
    );
}

fn footer(path: &Path, key: &str) -> Option<String> {
    ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap())
        .unwrap()
        .metadata()
        .file_metadata()
        .key_value_metadata()?
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.clone())
}

#[test]
fn classifier_predictions_keep_abstentions() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let raw = fixtures().join("classify/raw.fcs");
    let model = fixtures().join("classify/v2/binary");
    succeed(
        root,
        &[
            "export",
            "events",
            "--input",
            utf8(&raw),
            "--output",
            "s.t.parquet",
            "--layer",
            "raw",
        ],
    );
    succeed(
        root,
        &[
            "classify",
            "--input",
            utf8(&raw),
            "--model",
            utf8(&model),
            "--output",
            "classified",
            "--abstain-margin",
            "0.2",
            "--threads",
            "1",
        ],
    );
    let output = export_labels(
        root,
        "classifier",
        "classified/predictions.parquet",
        "classified/provenance.json",
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let states = states(&root.join("classifier.labels.parquet"));
    assert!(states.contains_key("classified"), "{states:?}");
    assert!(states.contains_key("abstained"), "{states:?}");
}

#[test]
fn missing_inputs_and_invalid_flags_are_classified() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let missing = export_labels(root, "qc", "absent.parquet", "absent.json", &[]);
    assert_eq!(
        missing.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );

    let gates = fixtures().join("pipeline/gates.xml");
    let invalid = export_labels(
        root,
        "qc",
        "absent.parquet",
        "absent.json",
        &["--gating-ml", utf8(&gates)],
    );
    assert_eq!(invalid.status.code(), Some(2));
    let unknown = cytiq(root, &["export", "labels", "--kind", "cells"]);
    assert_eq!(unknown.status.code(), Some(2));
}

#[test]
fn refusals_publish_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    pipeline(root);
    // The membership is not an output of the QC mask's run.
    let output = export_labels(
        root,
        "gate",
        "membership.parquet",
        "analysis.parquet.provenance.json",
        &["--gating-ml", utf8(&fixtures().join("pipeline/gates.xml"))],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("gate.labels.parquet").exists());
    // The membership names gates, but not the raw file's Gating-ML.
    let output = export_labels(
        root,
        "gate",
        "membership.parquet",
        "counts.json.provenance.json",
        &[
            "--gating-ml",
            utf8(&fixtures().join("pipeline/gates_raw.xml")),
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("gate.labels.parquet").exists());
    // Existing outputs are kept without `--overwrite`.
    let first = export_labels(
        root,
        "qc",
        "analysis.parquet",
        "analysis.parquet.provenance.json",
        &[],
    );
    assert!(first.status.success());
    let again = export_labels(
        root,
        "qc",
        "analysis.parquet",
        "analysis.parquet.provenance.json",
        &[],
    );
    assert_eq!(again.status.code(), Some(4));
}

/// The output-conflict contract of `tests/overwrite_cli.rs`, which runs
/// commands without prerequisites: refused with exit 4 and untouched,
/// replaced with `--overwrite`, reused with `--reuse skip`.
#[test]
fn labels_follow_the_output_conflict_contract() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let sample = fixtures().join("pipeline/sample.fcs");
    succeed(
        root,
        &["clean", "--input", utf8(&sample), "--output", "clean"],
    );
    succeed(
        root,
        &[
            "export",
            "events",
            "--input",
            utf8(&sample),
            "--output",
            "s.t.parquet",
            "--layer",
            "raw",
        ],
    );
    let run = |extra: &[&str]| {
        export_labels(
            root,
            "qc",
            "clean/masks.parquet",
            "clean/provenance.json",
            extra,
        )
    };
    let table = root.join("qc.labels.parquet");
    assert!(run(&[]).status.success());
    let first = std::fs::read(&table).unwrap();
    let conflict = run(&[]);
    assert_eq!(conflict.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("--overwrite"));
    assert_eq!(std::fs::read(&table).unwrap(), first);
    assert!(run(&["--overwrite"]).status.success());
    assert_eq!(
        std::fs::read(&table).unwrap(),
        first,
        "the export is deterministic"
    );
    assert_eq!(
        run(&["--overwrite", "--reuse", "reject"]).status.code(),
        Some(2)
    );
    let outcome = root.join("outcome.json");
    let reused = run(&["--reuse", "skip", "--outcome", utf8(&outcome)]);
    assert!(
        reused.status.success(),
        "{}",
        String::from_utf8_lossy(&reused.stderr)
    );
    let outcome: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&outcome).unwrap()).unwrap();
    assert_eq!(outcome["execution"]["reused"], true, "{outcome}");
}
