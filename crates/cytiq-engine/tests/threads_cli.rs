//! `--threads` changes speed, never results: every threaded command writes
//! byte-identical outputs with one and with four worker threads.
//!
//! Each run uses its own working directory and relative output paths, so
//! paths recorded in outputs are equal too. Provenance records differ by
//! design (arguments, times) and are not compared.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const FLUORESCENCE: &str = "FL1-A,FL2-A,FL3-A,FL4-A,FL5-A";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures")
}

fn create_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&directory).expect("temp dir should be created");
    directory
}

fn utf8(path: &Path) -> String {
    path.to_str().expect("path should be UTF-8").to_owned()
}

fn cytiq(directory: &Path, args: &[String]) {
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .current_dir(directory)
        .args(args)
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "`cytiq {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Every file below `directory` except provenance records, by relative path.
fn outputs(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).expect("directory should be readable") {
            let path = entry.expect("entry should be readable").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name == "provenance.json" || name.ends_with(".provenance.json") {
                continue;
            }
            let mut bytes = fs::read(&path).expect("output should be readable");
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            {
                bytes = without_varying_fields(&bytes);
            }
            files.insert(
                path.strip_prefix(directory)
                    .expect("below root")
                    .to_path_buf(),
                bytes,
            );
        }
    }
    files
}

/// JSON outputs without the fields that legitimately vary: the requested
/// thread count that `CytoScan` echoes in `cytoscan_config.json`, and the
/// wall-clock `run_id` (`autospectral-<unix seconds>`) of `AutoSpectral`'s
/// `diagnostics/run.json`.
fn without_varying_fields(bytes: &[u8]) -> Vec<u8> {
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                object.remove("threads");
                object.values_mut().for_each(strip);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut value: serde_json::Value = serde_json::from_slice(bytes).expect("output is JSON");
    if let Some(object) = value.as_object_mut() {
        object.remove("run_id");
    }
    strip(&mut value);
    serde_json::to_vec(&value).expect("JSON serializes")
}

/// Run `steps` once with each thread count, in fresh directories, and require
/// identical outputs. `{threads}` in a step is replaced by the count.
fn assert_thread_invariant(name: &str, steps: &[Vec<String>]) {
    let mut runs = Vec::new();
    for threads in ["1", "4"] {
        let directory = create_temp_dir(&format!("cytiq-threads-{name}-{threads}"));
        for step in steps {
            let args = step
                .iter()
                .map(|arg| arg.replace("{threads}", threads))
                .collect::<Vec<_>>();
            cytiq(&directory, &args);
        }
        runs.push(outputs(&directory));
        fs::remove_dir_all(&directory).expect("temp dir should be removed");
    }
    let (one, four) = (&runs[0], &runs[1]);
    assert!(!one.is_empty(), "{name}: no outputs");
    assert_eq!(
        one.keys().collect::<Vec<_>>(),
        four.keys().collect::<Vec<_>>(),
        "{name}: different output files"
    );
    for (path, bytes) in one {
        assert!(
            bytes == &four[path],
            "{name}: {} differs between 1 and 4 threads",
            path.display()
        );
    }
}

fn step(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
#[allow(clippy::too_many_lines)] // one literal argument list per stage
fn pipeline_stages_do_not_depend_on_the_thread_count() {
    let pipeline = fixtures().join("pipeline");
    let sample = utf8(&pipeline.join("sample.fcs"));
    let gates = utf8(&pipeline.join("gates.xml"));
    let steps = [
        step(&["compensate", "--input", &sample, "--output", "s.comp.fcs"]),
        step(&[
            "transform",
            "--input",
            "s.comp.fcs",
            "--output",
            "s.t.fcs",
            "--threads",
            "{threads}",
        ]),
        step(&[
            "transform",
            "--input",
            "s.comp.fcs",
            "--output",
            "s.logicle.fcs",
            "--method",
            "logicle",
            "--threads",
            "{threads}",
        ]),
        step(&[
            "peacoqc",
            "--input",
            "s.t.fcs",
            "--output",
            "s.peacoqc.fcs",
            "--channels",
            FLUORESCENCE,
            "--threads",
            "{threads}",
            "--debug-dir",
            "peacoqc-debug",
        ]),
        step(&[
            "clean",
            "--input",
            &sample,
            "--output",
            "clean",
            "--threads",
            "{threads}",
        ]),
        step(&[
            "gate",
            "--input",
            "s.t.fcs",
            "--gating-ml",
            &gates,
            "--output",
            "counts.json",
            "--format",
            "json",
            "--mask",
            "s.peacoqc.masks.parquet",
            "--membership",
            "membership.parquet",
            "--threads",
            "{threads}",
        ]),
        step(&[
            "flowsom",
            "--input",
            "s.t.fcs",
            "--output",
            "flowsom.csv",
            "--channels",
            FLUORESCENCE,
            "--mask",
            "s.peacoqc.masks.parquet",
            "--threads",
            "{threads}",
            "--debug-dir",
            "flowsom-debug",
        ]),
        step(&[
            "flowsom",
            "train",
            "--input",
            "s.t.fcs",
            "--input",
            "s.peacoqc.fcs",
            "--output",
            "som.json",
            "--channels",
            FLUORESCENCE,
            "--metaclustering",
            "consensus",
            "--threads",
            "{threads}",
        ]),
        step(&[
            "flowsom",
            "apply",
            "--input",
            "s.t.fcs",
            "--model",
            "som.json",
            "--output",
            "assigned.csv",
            "--threads",
            "{threads}",
        ]),
    ];
    assert_thread_invariant("pipeline", &steps);
}

#[test]
fn spillover_and_cohort_commands_do_not_depend_on_the_thread_count() {
    let parity = fixtures().join("parity");
    let autospill = parity.join("autospill");
    let autospectral = parity.join("autospectral");
    let cytoscan = parity.join("cytoscan_exact");
    let channels = fs::read_to_string(cytoscan.join("channels.txt")).expect("channels");
    let steps = [
        step(&[
            "autospill",
            "--controls",
            &utf8(&autospill.join("controls.csv")),
            "--input-dir",
            &utf8(&autospill),
            "--output",
            "spillover.csv",
            "--matrix-kind",
            "spillover",
            "--debug-dir",
            "autospill-debug",
            "--threads",
            "{threads}",
        ]),
        step(&[
            "autospectral",
            "--controls",
            &utf8(&autospectral.join("controls.csv")),
            "--samples",
            &utf8(&autospectral.join("samples.csv")),
            "--input-dir",
            &utf8(&autospectral),
            "--output",
            "autospectral",
            "--threads",
            "{threads}",
        ]),
        step(&[
            "cytoscan",
            "--input",
            &utf8(&cytoscan.join("test")),
            "--reference",
            &utf8(&cytoscan.join("reference")),
            "--output",
            "cytoscan",
            "--channels",
            channels.trim(),
            "--method",
            "quantiles",
            "--flag-method",
            "both",
            "--threads",
            "{threads}",
        ]),
        step(&[
            "spillqc",
            "--input",
            &utf8(&parity.join("spillqc")),
            "--output",
            "spillqc",
            "--threads",
            "{threads}",
        ]),
    ];
    assert_thread_invariant("cohorts", &steps);
}
