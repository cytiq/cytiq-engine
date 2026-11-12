//! The output-conflict contract of every command that writes results: an
//! existing output is refused with exit 4 before any work and left untouched,
//! `--overwrite` replaces it, `--reuse skip` reuses a verified match, and
//! `--reuse reject --overwrite` is an invalid combination (exit 2).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const FLUORESCENCE: &str = "FL1-A,FL2-A,FL3-A,FL4-A,FL5-A";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures")
}

fn utf8(path: &Path) -> String {
    path.to_str().expect("path should be UTF-8").to_owned()
}

fn cytiq(directory: &Path, args: &[String], extra: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .current_dir(directory)
        .args(args)
        .args(extra)
        .output()
        .expect("command should run");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Bytes of `path`, or of every file below it, except provenance records.
fn contents(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        if current.is_dir() {
            for entry in fs::read_dir(&current).expect("directory should be readable") {
                pending.push(entry.expect("entry").path());
            }
        } else if !current.to_string_lossy().ends_with("provenance.json") {
            files.insert(current.clone(), fs::read(&current).expect("readable"));
        }
    }
    files
}

struct Case {
    name: &'static str,
    output: &'static str,
    args: Vec<String>,
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

#[allow(clippy::too_many_lines)] // one literal argument list per command
fn cases() -> Vec<Case> {
    let pipeline = fixtures().join("pipeline");
    let parity = fixtures().join("parity");
    let sample = utf8(&pipeline.join("sample.fcs"));
    let gates = utf8(&pipeline.join("gates.xml"));
    let peacoqc = parity.join("peacoqc");
    let peacoqc_channels = fs::read_to_string(peacoqc.join("channels.txt")).expect("channels");
    let autospill = parity.join("autospill");
    let autospectral = parity.join("autospectral");
    let cytoscan = parity.join("cytoscan_exact");
    let cytoscan_channels = fs::read_to_string(cytoscan.join("channels.txt")).expect("channels");
    vec![
        Case {
            name: "gate",
            output: "counts.json",
            args: args(&[
                "gate",
                "--input",
                &sample,
                "--gating-ml",
                &gates,
                "--output",
                "counts.json",
                "--format",
                "json",
                "--membership",
                "membership.parquet",
            ]),
        },
        Case {
            name: "peacoqc",
            output: "cleaned.fcs",
            args: args(&[
                "peacoqc",
                "--input",
                &utf8(&peacoqc.join("sample.fcs")),
                "--output",
                "cleaned.fcs",
                "--channels",
                peacoqc_channels.trim(),
            ]),
        },
        Case {
            name: "clean",
            output: "clean",
            args: args(&["clean", "--input", &sample, "--output", "clean"]),
        },
        Case {
            name: "flowsom",
            output: "flowsom.csv",
            args: args(&[
                "flowsom",
                "--input",
                &sample,
                "--output",
                "flowsom.csv",
                "--channels",
                FLUORESCENCE,
                "--xdim",
                "3",
                "--ydim",
                "3",
                "--metaclusters",
                "3",
            ]),
        },
        Case {
            name: "autospill",
            output: "spillover.csv",
            args: args(&[
                "autospill",
                "--controls",
                &utf8(&autospill.join("controls.csv")),
                "--input-dir",
                &utf8(&autospill),
                "--output",
                "spillover.csv",
            ]),
        },
        Case {
            name: "autospread",
            output: "spread.csv",
            args: args(&[
                "autospread",
                "--controls",
                &utf8(&autospill.join("controls.csv")),
                "--input-dir",
                &utf8(&autospill),
                "--output",
                "spread.csv",
            ]),
        },
        Case {
            name: "autospectral",
            output: "autospectral",
            args: args(&[
                "autospectral",
                "--controls",
                &utf8(&autospectral.join("controls.csv")),
                "--samples",
                &utf8(&autospectral.join("samples.csv")),
                "--input-dir",
                &utf8(&autospectral),
                "--output",
                "autospectral",
            ]),
        },
        Case {
            name: "cytoscan",
            output: "cytoscan",
            args: args(&[
                "cytoscan",
                "--input",
                &utf8(&cytoscan.join("test")),
                "--output",
                "cytoscan",
                "--channels",
                cytoscan_channels.trim(),
            ]),
        },
    ]
}

#[test]
fn every_command_refuses_existing_outputs_unless_overwriting() {
    for case in cases() {
        let directory = tempfile::tempdir().expect("temp dir");
        let directory = directory.path();
        let output = directory.join(case.output);
        let name = case.name;

        let (code, stderr) = cytiq(directory, &case.args, &[]);
        assert_eq!(code, Some(0), "{name} first run: {stderr}");
        let first = contents(&output);
        assert!(!first.is_empty(), "{name} wrote nothing");

        let (code, stderr) = cytiq(directory, &case.args, &[]);
        assert_eq!(code, Some(4), "{name} rerun must conflict: {stderr}");
        assert!(stderr.contains("--overwrite"), "{name}: {stderr}");
        assert_eq!(
            contents(&output),
            first,
            "{name}: a refused run changed outputs"
        );

        let (code, stderr) = cytiq(directory, &case.args, &["--overwrite"]);
        assert_eq!(code, Some(0), "{name} --overwrite: {stderr}");

        let (code, stderr) = cytiq(directory, &case.args, &["--overwrite", "--reuse", "reject"]);
        assert_eq!(code, Some(2), "{name} reject + overwrite: {stderr}");

        let outcome = directory.join("outcome.json");
        let (code, stderr) = cytiq(
            directory,
            &case.args,
            &["--reuse", "skip", "--outcome", &utf8(&outcome)],
        );
        assert_eq!(code, Some(0), "{name} --reuse skip: {stderr}");
        let outcome: Value =
            serde_json::from_slice(&fs::read(&outcome).expect("outcome")).expect("JSON");
        assert_eq!(outcome["execution"]["reused"], true, "{name}: {outcome}");
    }
}

/// A directory output may hold an earlier run's files only with
/// `--overwrite`, and the record lists what this run published, never the
/// leftovers.
#[test]
fn directory_outputs_record_only_what_the_run_published() {
    let directory = tempfile::tempdir().expect("temp dir");
    let directory = directory.path();
    let clean = directory.join("clean");
    fs::create_dir(&clean).expect("output dir");
    fs::write(clean.join("notes.txt"), "kept by the user").expect("leftover");
    let sample = utf8(&fixtures().join("pipeline/sample.fcs"));
    let run = args(&["clean", "--input", &sample, "--output", "clean"]);

    let (code, stderr) = cytiq(directory, &run, &[]);
    assert_eq!(code, Some(4), "{stderr}");
    assert_eq!(fs::read_dir(&clean).expect("dir").count(), 1);

    let (code, stderr) = cytiq(directory, &run, &["--overwrite"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        fs::read_to_string(clean.join("notes.txt")).expect("leftover"),
        "kept by the user"
    );
    let record: Value =
        serde_json::from_slice(&fs::read(clean.join("provenance.json")).expect("record"))
            .expect("JSON");
    let outputs = record["outputs"]
        .as_array()
        .expect("outputs")
        .iter()
        .map(|entry| entry["path"].as_str().expect("path").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 4, "{outputs:?}");
    assert!(
        outputs.iter().all(|path| !path.ends_with("notes.txt")),
        "{outputs:?}"
    );
}

/// Saved-model commands report an existing output as an output conflict
/// (exit 4), like every other command, not as invalid input.
#[test]
fn saved_model_commands_report_existing_outputs_as_conflicts() {
    let directory = tempfile::tempdir().expect("temp dir");
    let directory = directory.path();
    let sample = utf8(&fixtures().join("pipeline/sample.fcs"));
    let train = args(&[
        "flowsom",
        "train",
        "--input",
        &sample,
        "--output",
        "som.json",
        "--channels",
        FLUORESCENCE,
        "--xdim",
        "3",
        "--ydim",
        "3",
        "--metaclusters",
        "3",
    ]);
    let apply = args(&[
        "flowsom",
        "apply",
        "--input",
        &sample,
        "--model",
        "som.json",
        "--output",
        "assigned.csv",
    ]);
    for step in [&train, &apply] {
        let (code, stderr) = cytiq(directory, step, &[]);
        assert_eq!(code, Some(0), "{stderr}");
        let (code, stderr) = cytiq(directory, step, &[]);
        assert_eq!(code, Some(4), "{stderr}");
        assert!(stderr.contains("--overwrite"), "{stderr}");
        let (code, stderr) = cytiq(directory, step, &["--overwrite"]);
        assert_eq!(code, Some(0), "{stderr}");
    }
}
