//! The 0–4 exit contract and `--outcome` across command families.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures")
        .join(name)
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

fn cytiq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("JSON file")).expect("valid JSON")
}

fn assert_outcome(outcome: &Path, command: &str, reason: &str, code: i64, completed: bool) {
    let outcome = read_json(outcome);
    assert_eq!(outcome["schema_version"], 1);
    assert_eq!(outcome["command"], command);
    assert_eq!(outcome["reason"], reason);
    assert_eq!(outcome["exit_code"], code);
    assert_eq!(outcome["completed"], completed);
    // A failure publishes nothing; a success lists what it published.
    if code != 0 {
        assert_eq!(outcome["artifacts"], Value::Array(Vec::new()));
    }
}

fn artifact_paths(outcome: &Path) -> Vec<String> {
    read_json(outcome)["artifacts"]
        .as_array()
        .expect("artifacts")
        .iter()
        .map(|artifact| artifact["path"].as_str().expect("path").to_owned())
        .collect()
}

#[test]
fn success_writes_a_completed_outcome_and_leaves_stdout_alone() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let outcome = directory.path().join("outcome.json");
    let output = cytiq(&[
        "meta",
        "--input",
        utf8(&fixture("gating/sample.fcs")),
        "--output",
        "-",
        "--outcome",
        utf8(&outcome),
    ]);

    assert_eq!(output.status.code(), Some(0));
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("metadata JSON");
    assert!(
        metadata.get("reason").is_none(),
        "stdout holds only metadata"
    );
    assert_outcome(&outcome, "cytiq meta", "success", 0, true);
    assert!(
        artifact_paths(&outcome).is_empty(),
        "stdout is not an artifact"
    );
    assert_eq!(read_json(&outcome)["message"], "");
}

#[test]
fn corrupt_fcs_exits_two_not_one() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("broken.fcs");
    fs::write(&input, b"FCS3.1    garbage").expect("write");
    let outcome = directory.path().join("outcome.json");

    let output = cytiq(&[
        "stats",
        "--input",
        utf8(&input),
        "--output",
        utf8(&directory.path().join("stats.json")),
        "--outcome",
        utf8(&outcome),
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert_outcome(&outcome, "cytiq stats", "invalid_configuration", 2, false);
    let message = read_json(&outcome)["message"].clone();
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .ends_with(message.as_str().expect("message")),
        "the error stays the last stderr line"
    );
}

#[test]
fn missing_input_exits_four_as_incomplete() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let outcome = directory.path().join("outcome.json");

    let output = cytiq(&[
        "stats",
        "--input",
        utf8(&directory.path().join("absent.fcs")),
        "--output",
        utf8(&directory.path().join("stats.json")),
        "--outcome",
        utf8(&outcome),
    ]);

    assert_eq!(output.status.code(), Some(4));
    assert_outcome(&outcome, "cytiq stats", "incomplete", 4, false);
}

#[test]
fn existing_output_exits_four_as_output_conflict() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let stats = directory.path().join("stats.json");
    let outcome = directory.path().join("outcome.json");
    let input = fixture("gating/sample.fcs");
    let args = [
        "stats",
        "--input",
        utf8(&input),
        "--output",
        utf8(&stats),
        "--outcome",
        utf8(&outcome),
    ];

    assert_eq!(cytiq(&args).status.code(), Some(0));
    assert_outcome(&outcome, "cytiq stats", "success", 0, true);
    assert_eq!(artifact_paths(&outcome), [utf8(&stats)]);
    let output = cytiq(&args);

    assert_eq!(output.status.code(), Some(4));
    assert_outcome(&outcome, "cytiq stats", "output_conflict", 4, false);
}

#[test]
fn outcome_flag_is_accepted_before_and_after_the_subcommand() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let before = directory.path().join("before.json");
    let output = cytiq(&[
        "--outcome",
        utf8(&before),
        "meta",
        "--input",
        utf8(&fixture("gating/sample.fcs")),
        "--output",
        "-",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert_outcome(&before, "cytiq meta", "success", 0, true);

    let mask = directory.path().join("mask.json");
    let output = cytiq(&[
        "mask",
        "combine",
        "--mask",
        utf8(&directory.path().join("absent.parquet")),
        "--output",
        utf8(&directory.path().join("combined.parquet")),
        "--outcome",
        utf8(&mask),
    ]);
    assert_eq!(output.status.code(), Some(4));
    assert_outcome(&mask, "cytiq mask combine", "incomplete", 4, false);

    let flowsom = directory.path().join("flowsom.json");
    let output = cytiq(&[
        "flowsom",
        "train",
        "--input",
        utf8(&fixture("gating/sample.fcs")),
        "--output",
        utf8(&directory.path().join("model.json")),
        "--channels",
        ",",
        "--outcome",
        utf8(&flowsom),
    ]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_outcome(
        &flowsom,
        "cytiq flowsom train",
        "invalid_configuration",
        2,
        false,
    );
}

#[test]
fn outcome_is_in_arguments_but_not_in_config() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let stats = directory.path().join("stats.json");
    let outcome = directory.path().join("outcome.json");

    let output = cytiq(&[
        "stats",
        "--input",
        utf8(&fixture("gating/sample.fcs")),
        "--output",
        utf8(&stats),
        "--outcome",
        utf8(&outcome),
    ]);

    assert_eq!(output.status.code(), Some(0));
    let record = read_json(&cytiq_provenance::record_path_for_file(&stats));
    let arguments = record["arguments"].as_array().expect("arguments");
    assert!(arguments.iter().any(|argument| argument == "--outcome"));
    assert!(record["config"].get("outcome").is_none());
    assert!(
        record["outputs"]
            .as_array()
            .expect("outputs")
            .iter()
            .all(|output| !output["path"]
                .as_str()
                .unwrap_or("")
                .ends_with("outcome.json")),
        "the outcome is not a recorded output"
    );
}

#[test]
fn an_unwritable_outcome_keeps_the_exit_code() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let file = directory.path().join("file");
    fs::write(&file, "not a directory").expect("write");
    let outcome = file.join("outcome.json");

    let success = cytiq(&[
        "meta",
        "--input",
        utf8(&fixture("gating/sample.fcs")),
        "--output",
        "-",
        "--outcome",
        utf8(&outcome),
    ]);
    assert_eq!(success.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&success.stderr).contains("failed to write outcome"));

    let failure = cytiq(&[
        "stats",
        "--input",
        utf8(&fixture("gating/sample.fcs")),
        "--output",
        utf8(&directory.path().join("stats.json")),
        "--bins",
        "1",
        "--outcome",
        utf8(&outcome),
    ]);
    assert_eq!(failure.status.code(), Some(2));
}

#[test]
fn argument_errors_exit_two_without_an_outcome() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let outcome = directory.path().join("outcome.json");

    let output = cytiq(&["stats", "--no-such-flag", "--outcome", utf8(&outcome)]);

    assert_eq!(output.status.code(), Some(2));
    assert!(!outcome.exists());
}

#[test]
fn dash_is_not_an_outcome_path() {
    let output = cytiq(&[
        "meta",
        "--input",
        utf8(&fixture("gating/sample.fcs")),
        "--output",
        "-",
        "--outcome",
        "-",
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("stdout is reserved"));
}

#[test]
fn specimen_outcome_file_matches_the_stdout_outcome() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let outcome = directory.path().join("outcome.json");

    let output = cytiq(&[
        "specimen",
        "validate",
        "--manifest",
        utf8(&directory.path().join("absent.yaml")),
        "--layout",
        utf8(&directory.path().join("layout.yaml")),
        "--specimen",
        "S01",
        "--outcome",
        utf8(&outcome),
    ]);

    let printed: Value = serde_json::from_slice(&output.stdout).expect("stdout outcome");
    assert_eq!(read_json(&outcome), printed);
    assert_eq!(
        output.status.code(),
        Some(i32::try_from(printed["exit_code"].as_i64().expect("code")).expect("small code"))
    );
    assert_eq!(printed["completed"], false);
}
