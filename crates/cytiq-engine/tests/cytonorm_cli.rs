mod common;
#[path = "../../cytiq-cytonorm/tests/support/mod.rs"]
mod support;

use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str], root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A normalized file is never a raw layer of its acquisition.
fn assert_never_raw(root: &Path, file: &str) {
    let raw = run(
        &[
            "export",
            "events",
            "--input",
            file,
            "--output",
            "raw.parquet",
            "--layer",
            "raw",
        ],
        root,
    );
    assert_eq!(raw.status.code(), Some(2));
    assert!(!root.join("raw.parquet").exists());
}

#[test]
fn trains_applies_and_records_all_artifacts_and_original_acquisition() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    support::design(root, "controls");
    success(&run(
        &[
            "cytonorm",
            "train",
            "--manifest",
            "batches.json",
            "--output",
            "model",
            "--threads",
            "1",
            "--debug-dir",
            "diagnostics",
        ],
        root,
    ));
    for name in [
        "model.json",
        "som.json",
        "markers.json",
        "splines.parquet",
        "training-qc.json",
        "provenance.json",
    ] {
        assert!(root.join("model").join(name).is_file());
    }
    assert!(root.join("diagnostics/report.html").is_file());
    let record: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(root.join("model/provenance.json")).unwrap())
            .unwrap();
    assert_eq!(record["tool"], "cytiq cytonorm train");
    assert_eq!(record["inputs"].as_array().unwrap().len(), 5);
    assert_eq!(record["algorithms"]["CytoNorm"], "2.0.12");
    success(&run(
        &[
            "cytonorm",
            "apply",
            "--input",
            "vb.fcs",
            "--model",
            "model",
            "--batch",
            "b",
            "--output",
            "sample.norm.fcs",
        ],
        root,
    ));
    assert!(root.join("sample.norm.fcs.cytonorm-qc.json").is_file());
    let record: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(root.join("sample.norm.fcs.provenance.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["tool"], "cytiq cytonorm apply");
    assert_eq!(record["inputs"].as_array().unwrap().len(), 5);
    assert!(
        !run(
            &[
                "cytonorm",
                "apply",
                "--input",
                "vb.fcs",
                "--model",
                "model",
                "--batch",
                "b",
                "--output",
                "sample.norm.fcs"
            ],
            root
        )
        .status
        .success()
    );
    assert!(
        !run(
            &[
                "cytonorm", "apply", "--input", "vb.fcs", "--model", "model", "--batch", "unknown",
                "--output", "bad.fcs"
            ],
            root
        )
        .status
        .success()
    );
    assert!(!root.join("bad.fcs").exists());
    assert_never_raw(root, "sample.norm.fcs");
}

#[test]
fn validates_flags_and_missing_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(
        !run(
            &[
                "cytonorm",
                "train",
                "--manifest",
                "missing.yaml",
                "--output",
                "model"
            ],
            root
        )
        .status
        .success()
    );
    assert!(
        !run(
            &[
                "cytonorm",
                "train",
                "--manifest",
                "missing.yaml",
                "--output",
                "model",
                "--threads",
                "0"
            ],
            root
        )
        .status
        .success()
    );
    assert!(!root.join("model").exists());
}

#[test]
fn flowsom_model_commands_preserve_legacy_flags_and_reuse_fixed_models() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    support::design(root, "controls");
    success(&run(
        &[
            "flowsom",
            "train",
            "--input",
            "a.fcs",
            "--input",
            "b.fcs",
            "--output",
            "som.json",
            "--channels",
            "CD3",
            "--xdim",
            "4",
            "--ydim",
            "2",
            "--metaclusters",
            "2",
            "--metaclustering",
            "consensus",
        ],
        root,
    ));
    success(&run(
        &[
            "flowsom",
            "apply",
            "--input",
            "vb.fcs",
            "--model",
            "som.json",
            "--output",
            "assigned.csv",
        ],
        root,
    ));
    let reader = csv::Reader::from_path(root.join("assigned.csv")).unwrap();
    assert_eq!(reader.into_records().count(), 2000);
    success(&run(
        &[
            "flowsom",
            "--input",
            "a.fcs",
            "--output",
            "legacy.csv",
            "--channels",
            "CD3",
            "--xdim",
            "4",
            "--ydim",
            "2",
            "--metaclusters",
            "2",
        ],
        root,
    ));
}
