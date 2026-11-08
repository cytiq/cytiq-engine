//! Reuse is a verified execution decision, observable through outcomes and mtimes.
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/gating/sample.fcs")
}
fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}
fn cytiq(args: &[&str], code: i32) {
    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(code),
        "{}: {}",
        args.join(" "),
        String::from_utf8_lossy(&result.stderr)
    );
}
fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn skip_reuses_without_rewriting_and_config_changes_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("stats.json");
    let outcome = temp.path().join("outcome.json");
    let input = fixture();
    let args = [
        "stats",
        "--input",
        text(&input),
        "--output",
        text(&output),
        "--reuse",
        "skip",
        "--outcome",
        text(&outcome),
    ];
    cytiq(&args, 0);
    assert_eq!(json(&outcome)["execution"]["reused"], false);
    let before = fs::read(&output).unwrap();
    let time = fs::metadata(&output).unwrap().modified().unwrap();
    let record = cytiq_provenance::record_path_for_file(&output);
    let record_before = fs::read(&record).unwrap();
    cytiq(&args, 0);
    assert_eq!(json(&outcome)["execution"]["reused"], true);
    assert_eq!(fs::read(&output).unwrap(), before);
    assert_eq!(fs::metadata(&output).unwrap().modified().unwrap(), time);
    assert_eq!(fs::read(&record).unwrap(), record_before);
    let mut changed = args.to_vec();
    changed.extend(["--bins", "128"]);
    changed[6] = "reject";
    cytiq(&changed, 4);
    assert_eq!(fs::read(&output).unwrap(), before);
    let mut overwritten = args.to_vec();
    overwritten.push("--overwrite");
    cytiq(&overwritten, 0);
    assert_eq!(json(&outcome)["execution"]["reused"], true);
}

#[test]
fn tampering_requires_overwrite_and_reject_preserves_the_damaged_output() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("stats.json");
    let outcome = temp.path().join("outcome.json");
    let input = fixture();
    let args = [
        "stats",
        "--input",
        text(&input),
        "--output",
        text(&output),
        "--reuse",
        "skip",
        "--outcome",
        text(&outcome),
    ];
    cytiq(&args, 0);
    fs::write(&output, b"tampered").unwrap();
    let mut rejected = args;
    rejected[6] = "reject";
    cytiq(&rejected, 4);
    assert_eq!(fs::read(&output).unwrap(), b"tampered");
    cytiq(&args, 4);
    let mut overwrite = args.to_vec();
    overwrite.push("--overwrite");
    cytiq(&overwrite, 0);
    assert_eq!(json(&outcome)["execution"]["reused"], false);
    assert!(json(&output).is_object());
    rejected[6] = "reject";
    let mut invalid = rejected.to_vec();
    invalid.push("--overwrite");
    cytiq(&invalid, 2);
}

#[test]
fn unrecorded_outputs_are_never_reused_and_a_held_lock_exits_four() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("stats.json");
    let input = fixture();
    let args = [
        "stats",
        "--input",
        text(&input),
        "--output",
        text(&output),
        "--reuse",
        "skip",
    ];
    let lock = cytiq_publish::OutputLock::acquire(&output).unwrap();
    cytiq(&args, 4);
    assert!(!output.exists());
    drop(lock);
    fs::write(&output, b"unrecorded").unwrap();
    cytiq(&args, 4);
    assert_eq!(fs::read(&output).unwrap(), b"unrecorded");
    let mut overwrite = args.to_vec();
    overwrite.push("--overwrite");
    cytiq(&overwrite, 0);
    assert!(!temp.path().join(".stats.json.cytiq-lock").exists());
}

#[test]
fn changed_input_recomputes_and_own_abandoned_temporaries_are_cleaned() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("stats.json");
    let outcome = temp.path().join("outcome.json");
    let input = temp.path().join("input.fcs");
    fs::copy(fixture(), &input).unwrap();
    let args = [
        "stats",
        "--input",
        text(&input),
        "--output",
        text(&output),
        "--reuse",
        "skip",
        "--overwrite",
        "--outcome",
        text(&outcome),
    ];
    cytiq(&args, 0);
    let first = json(&outcome)["execution"]["fingerprint"].clone();
    let mut bytes = fs::read(&input).unwrap();
    bytes.extend(b"\n");
    fs::write(&input, bytes).unwrap();
    let leftover = temp.path().join(".stats.json.abc123.tmp");
    fs::write(&leftover, b"incomplete").unwrap();
    let unrelated = temp.path().join(".other.abc123.tmp");
    fs::write(&unrelated, b"keep").unwrap();
    cytiq(&args, 0);
    assert_eq!(json(&outcome)["execution"]["reused"], false);
    assert_ne!(json(&outcome)["execution"]["fingerprint"], first);
    assert!(!leftover.exists());
    assert!(unrelated.exists());
}

#[test]
fn debug_artifacts_force_recomputation_and_nonsemantic_threads_do_not() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("membership.parquet");
    let outcome = temp.path().join("outcome.json");
    let input = fixture();
    let debug = temp.path().join("debug");
    let args = [
        "flowsom",
        "--input",
        text(&input),
        "--output",
        text(&output),
        "--channels",
        "FSC-A,SSC-A",
        "--xdim",
        "2",
        "--ydim",
        "2",
        "--metaclusters",
        "2",
        "--reuse",
        "skip",
        "--outcome",
        text(&outcome),
    ];
    cytiq(&args, 0);
    let mut threaded = args.to_vec();
    threaded.extend(["--threads", "2"]);
    cytiq(&threaded, 0);
    assert_eq!(json(&outcome)["execution"]["reused"], true);
    let mut debugged = args.to_vec();
    debugged.extend(["--debug-dir", text(&debug)]);
    cytiq(&debugged, 0);
    assert_eq!(json(&outcome)["execution"]["reused"], false);
    let fresh = temp.path().join("fresh.parquet");
    let mut rejected = args.to_vec();
    rejected[4] = text(&fresh);
    rejected[14] = "reject";
    rejected.extend(["--debug-dir", text(&debug)]);
    cytiq(&rejected, 0);
    assert_eq!(json(&outcome)["execution"]["reused"], false);
}

#[test]
fn compensate_and_gate_reuse_every_output_and_reject_a_damaged_sidecar() {
    for command in ["compensate", "gate"] {
        let temp = tempfile::tempdir().unwrap();
        let input = fixture();
        let output = temp.path().join(if command == "gate" {
            "counts.csv"
        } else {
            "comp.fcs"
        });
        let outcome = temp.path().join("outcome.json");
        let membership = temp.path().join("membership.parquet");
        let gates = input.with_file_name("gates.xml");
        let mut args = vec![
            command,
            "--input",
            text(&input),
            "--output",
            text(&output),
            "--reuse",
            "skip",
            "--outcome",
            text(&outcome),
        ];
        if command == "gate" {
            args.extend([
                "--gating-ml",
                text(&gates),
                "--membership",
                text(&membership),
            ]);
        }
        cytiq(&args, 0);
        let record = json(&cytiq_provenance::record_path_for_file(&output));
        let outputs: Vec<_> = record["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| PathBuf::from(file["path"].as_str().unwrap()))
            .collect();
        assert_eq!(outputs.len(), if command == "compensate" { 3 } else { 2 });
        let stamps: Vec<_> = outputs
            .iter()
            .map(|path| fs::metadata(path).unwrap().modified().unwrap())
            .collect();
        cytiq(&args, 0);
        assert_eq!(json(&outcome)["execution"]["reused"], true);
        for (path, stamp) in outputs.iter().zip(stamps) {
            assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), stamp);
        }
        let primary = fs::read(&output).unwrap();
        for sidecar in outputs.iter().skip(1) {
            let original = fs::read(sidecar).unwrap();
            fs::write(sidecar, b"damaged sidecar").unwrap();
            args[6] = "reject";
            cytiq(&args, 4);
            assert_eq!(fs::read(&output).unwrap(), primary);
            assert_eq!(fs::read(sidecar).unwrap(), b"damaged sidecar");
            fs::write(sidecar, original).unwrap();
        }
    }
}
