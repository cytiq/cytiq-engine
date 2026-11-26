//! A vendor keyword a file repeats: read past, shown, and never carried on.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cytiq_fcs::{FcsReader, FcsWriteOptions, FcsWriteParameter};
use serde_json::{Value, json};

fn cytiq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run")
}

fn text(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

/// Two channels, a spillover matrix and `keywords`.
fn write(path: &Path, keywords: &[(&str, &str)]) {
    let parameters = [
        FcsWriteParameter::new("FL1-A").with_range(1024.0),
        FcsWriteParameter::new("FL2-A").with_range(1024.0),
    ];
    let columns = [vec![1.0_f32, 2.0, 3.0], vec![4.0, 5.0, 6.0]];
    let mut options =
        FcsWriteOptions::new().with_text_keyword("$SPILLOVER", "2,FL1-A,FL2-A,1,0.1,0.05,1");
    for (key, value) in keywords {
        options = options.with_text_keyword(*key, *value);
    }
    cytiq_fcs::write_fcs3_1_f32_with_options(path, &parameters, &columns, &options).unwrap();
}

/// Rename keywords in place. The writer keeps one value per keyword, so a
/// repeated one has to be made afterwards.
fn rename(path: &Path, names: &[(&str, &str)]) {
    let mut bytes = fs::read(path).unwrap();
    for (from, to) in names {
        assert_eq!(from.len(), to.len(), "offsets must not move");
        let (from, to) = (format!("|{from}|"), format!("|{to}|"));
        let at = bytes
            .windows(from.len())
            .position(|window| window == from.as_bytes())
            .unwrap();
        bytes[at..at + to.len()].copy_from_slice(to.as_bytes());
    }
    fs::write(path, bytes).unwrap();
}

fn record(output: &Path) -> Value {
    let path = cytiq_provenance::record_path_for_file(output);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("tube.fcs");
    write(
        &input,
        &[
            ("NA", "empty"),
            ("Kept", "yes"),
            ("N1", "CD3-AA750-A"),
            ("N2", "x"),
        ],
    );
    rename(&input, &[("N1", "NA"), ("N2", "na")]);
    (root, input)
}

#[test]
fn meta_shows_the_pairs_and_names_them() {
    let (root, input) = workspace();
    let output = cytiq(&["meta", "--input", text(&input)]);
    assert!(output.status.success());
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["quarantined_keywords"], json!(["NA"]));
    let pairs = document["text"].as_array().unwrap();
    let values: Vec<&str> = pairs
        .iter()
        .filter(|pair| pair[0].as_str().unwrap().eq_ignore_ascii_case("NA"))
        .map(|pair| pair[1].as_str().unwrap())
        .collect();
    assert_eq!(values, ["empty", "CD3-AA750-A", "x"]);

    // A file without repeats gives the document it always gave.
    let clean = root.path().join("clean.fcs");
    write(&clean, &[("NA", "empty")]);
    let output = cytiq(&["meta", "--input", text(&clean)]);
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(document.get("quarantined_keywords").is_none());
}

#[test]
fn derived_files_and_tables_leave_it_out_and_records_name_it() {
    let (root, input) = workspace();
    let sha256 = cytiq_provenance::hash_file(&input).unwrap().1;
    let table = root.path().join("raw.parquet");
    let compensated = root.path().join("tube.comp.fcs");
    for args in [
        vec![
            "export",
            "events",
            "--input",
            text(&input),
            "--output",
            text(&table),
            "--layer",
            "raw",
        ],
        vec![
            "compensate",
            "--input",
            text(&input),
            "--output",
            text(&compensated),
        ],
    ] {
        let output = cytiq(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for output in [&table, &compensated] {
        let facts = &record(output)["constants"]["input_facts"];
        assert_eq!(facts["quarantined_keywords"][&sha256], json!(["NA"]));
    }
    let derived = FcsReader::open(&compensated).unwrap().read().unwrap();
    assert_eq!(derived.text.get("Kept"), Some("yes"));
    assert!(
        derived
            .text
            .pairs()
            .all(|(key, _)| !key.eq_ignore_ascii_case("NA"))
    );
    assert!(
        record(&compensated)["constants"]["input_facts"]
            .as_object()
            .is_some_and(|facts| facts.len() == 1)
    );

    // The same file again is the same execution: `--reuse` still matches.
    let reused = cytiq(&[
        "--reuse",
        "reject",
        "export",
        "events",
        "--input",
        text(&input),
        "--output",
        text(&table),
        "--layer",
        "raw",
    ]);
    assert_eq!(reused.status.code(), Some(0));
    // An input without repeats records nothing new.
    let clean = root.path().join("clean.fcs");
    write(&clean, &[]);
    let other = root.path().join("clean.parquet");
    let output = cytiq(&[
        "export",
        "events",
        "--input",
        text(&clean),
        "--output",
        text(&other),
        "--layer",
        "raw",
    ]);
    assert!(output.status.success());
    assert_eq!(record(&other)["constants"], Value::Null);
}

#[test]
fn preflight_warns_and_a_repeated_spillover_is_still_refused() {
    let (root, _) = workspace();
    let manifest = root.path().join("run.yaml");
    fs::write(
        &manifest,
        "schema_version: 3\ncytometry: conventional\npanels:\n  - name: T\n    signal_state: raw\n    \
         markers:\n      - { marker: CD3, channel: FL1-A }\nsamples:\n  - { id: S1, file: tube.fcs, \
         panel: T }\n",
    )
    .unwrap();
    let report = root.path().join("preflight.json");
    let output = cytiq(&[
        "preflight",
        "--manifest",
        text(&manifest),
        "--output",
        text(&report),
    ]);
    let document: Value = serde_json::from_slice(
        &fs::read(&report)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr))),
    )
    .unwrap();
    let warning = document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "repeated_keywords")
        .expect("preflight names the repeated keyword");
    assert_eq!(warning["status"], "warn");
    assert!(warning["message"].as_str().unwrap().contains("`NA`"));

    let refused = root.path().join("refused.fcs");
    write(&refused, &[("SPILL", "1"), ("SPILX", "2")]);
    rename(&refused, &[("SPILX", "spill")]);
    let output = cytiq(&["meta", "--input", text(&refused)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate keyword"));
}
