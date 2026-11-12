//! What a workflow engine reads from a step: the outcome's artifacts, JSON log
//! lines and the reason token on stderr.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/gating/sample.fcs")
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

fn stats(output: &Path, outcome: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "stats",
            "--input",
            utf8(&sample()),
            "--output",
            utf8(output),
            "--outcome",
            utf8(outcome),
        ])
        .args(extra)
        .env("CYTIQ_LOG", "info")
        .env("CYTIQ_LOG_FORMAT", "json")
        .output()
        .expect("command should run")
}

#[test]
fn outcomes_list_published_and_reused_artifacts_with_their_digests() {
    let temp = tempfile::tempdir().expect("temp dir");
    let output = temp.path().join("stats.json");
    let outcome = temp.path().join("outcome.json");
    for (extra, reused) in [(&[][..], false), (&["--reuse", "skip"][..], true)] {
        let result = stats(&output, &outcome, extra);
        assert!(result.status.success(), "{result:?}");
        let document: Value =
            serde_json::from_slice(&fs::read(&outcome).expect("outcome")).expect("JSON");
        assert_eq!(document["execution"]["reused"], reused);
        let artifacts = document["artifacts"].as_array().expect("artifacts");
        assert_eq!(artifacts.len(), 1, "{document}");
        assert_eq!(artifacts[0]["path"], utf8(&output));
        assert_eq!(artifacts[0]["sha256"], common::sha256_hex(&output));
        assert_eq!(
            artifacts[0]["bytes"],
            fs::metadata(&output).expect("output").len()
        );
    }
}

#[test]
fn json_logs_are_one_object_per_line_and_failures_name_their_reason() {
    let temp = tempfile::tempdir().expect("temp dir");
    let output = temp.path().join("stats.json");
    let outcome = temp.path().join("outcome.json");
    let first = stats(&output, &outcome, &[]);
    assert!(first.status.success());
    let lines = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(!lines.trim().is_empty(), "info logs expected");
    for line in lines.lines() {
        let entry: Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("`{line}` is not JSON: {error}"));
        assert_eq!(
            entry
                .as_object()
                .expect("object")
                .keys()
                .collect::<Vec<_>>(),
            ["level", "message", "target"]
        );
    }

    let conflict = stats(&output, &outcome, &[]);
    assert_eq!(conflict.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&conflict.stderr);
    assert!(
        stderr
            .lines()
            .last()
            .is_some_and(|line| line.contains("already exists")),
        "{stderr}"
    );
    let document: Value =
        serde_json::from_slice(&fs::read(&outcome).expect("outcome")).expect("JSON");
    assert_eq!(document["reason"], "output_conflict");
    assert_eq!(document["artifacts"], serde_json::json!([]));
}
