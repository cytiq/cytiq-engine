mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

fn create_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&directory).expect("temp dir should be created");
    directory
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

fn validate(manifest: &Path, report: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "dataset",
        "validate",
        "--manifest",
        utf8(manifest),
        "--output",
        utf8(report),
    ];
    args.extend_from_slice(extra);
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(&args)
        .output()
        .expect("command should run")
}

/// The reference dataset in `<directory>/dataset`.
fn reference(directory: &Path) -> PathBuf {
    let root = directory.join("dataset");
    let work = directory.join("work");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&work).unwrap();
    common::dataset::build(&root, &work);
    root
}

fn read_report(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn codes(report: &Value) -> Vec<String> {
    report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|issue| issue["severity"] == "error")
        .map(|issue| issue["code"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_reference_dataset_is_valid_with_every_object_verified() {
    let directory = create_temp_dir("cytiq-dataset-valid");
    let root = reference(&directory);
    let manifest = root.join("manifest.json");
    let report = directory.join("report.json");
    let output = validate(
        &manifest,
        &report,
        &["--verify-objects", "--profile", "miflowcyt-minimal"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document = read_report(&report);
    assert_eq!(document["status"], "valid", "{document:#}");
    assert_eq!(document["errors"], 0);
    assert_eq!(document["manifest_sha256"], common::sha256_hex(&manifest));
    let parquet = root.join("events/M/S12-M.raw.parquet");
    common::assert_provenance(
        &cytiq_provenance::record_path_for_file(&report),
        "cytiq dataset validate",
        &[&manifest, &parquet],
        &[&report],
    );
    // Panels B and M hold source_event_id 0..3 of different acquisitions.
    assert!(!codes(&document).contains(&"duplicate_event_identity".to_owned()));
}

#[test]
fn invalid_datasets_exit_3_after_writing_the_report() {
    let directory = create_temp_dir("cytiq-dataset-invalid");
    let root = reference(&directory);
    let layer = root.join("events/B/S30-B.compensated.parquet");
    let mut bytes = fs::read(&layer).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    fs::write(&layer, bytes).unwrap();
    let report = directory.join("report.json");
    let outcome = directory.join("outcome.json");
    let output = validate(
        &root.join("manifest.json"),
        &report,
        &["--verify-objects", "--outcome", utf8(&outcome)],
    );
    assert_eq!(output.status.code(), Some(3));
    let document = read_report(&report);
    assert_eq!(document["status"], "invalid");
    assert_eq!(codes(&document), ["corrupt_artifact_hash"]);
    let outcome: Value = serde_json::from_slice(&fs::read(&outcome).unwrap()).unwrap();
    assert_eq!(outcome["exit_code"], 3);
    assert!(outcome["message"].as_str().unwrap().contains("1 error"));
    // Without object verification the document alone is valid.
    let document_only = directory.join("document.json");
    assert_eq!(
        validate(&root.join("manifest.json"), &document_only, &[])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn malformed_manifests_are_reported_not_crashed_on() {
    let directory = create_temp_dir("cytiq-dataset-malformed");
    let manifest = directory.join("manifest.json");
    fs::write(&manifest, "{\"schema_version\": 1,").unwrap();
    let report = directory.join("report.json");
    assert_eq!(validate(&manifest, &report, &[]).status.code(), Some(3));
    assert_eq!(codes(&read_report(&report)), ["malformed_document"]);
}

#[test]
fn missing_manifests_existing_reports_and_bad_flags_are_classified() {
    let directory = create_temp_dir("cytiq-dataset-errors");
    let report = directory.join("report.json");
    assert_eq!(
        validate(&directory.join("absent.json"), &report, &[])
            .status
            .code(),
        Some(4)
    );
    assert!(!report.exists());
    let manifest = directory.join("manifest.json");
    fs::write(&manifest, "{}").unwrap();
    assert_eq!(
        validate(&manifest, &report, &["--profile", "complete"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(validate(&manifest, &report, &[]).status.code(), Some(3));
    assert_eq!(
        validate(&manifest, &report, &[]).status.code(),
        Some(4),
        "existing report"
    );
    assert_eq!(
        validate(&manifest, &report, &["--overwrite"]).status.code(),
        Some(3)
    );
}
