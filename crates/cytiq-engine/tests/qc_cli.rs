mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use cytiq_mask::EventMask;
use serde_json::Value;

const EVENTS: usize = 10_000;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures/clean")
        .join(name)
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

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

/// A manifest for `fixture("sample.fcs")` whose panel has `qc` (YAML lines
/// indented under `panel`).
fn write_manifest(directory: &Path, qc: &str) -> PathBuf {
    let path = directory.join("run.yaml");
    fs::write(
        &path,
        format!(
            "cytometry: conventional\n\
             panel:\n  name: test\n  markers:\n    - {{ marker: CD3 }}\n{qc}\
             samples:\n  - {{ id: S01, file: {} }}\n",
            utf8(&fixture("sample.fcs"))
        ),
    )
    .expect("manifest should write");
    path
}

/// A mask over every raw event: `peacoqc_pass` removes the first
/// `peacoqc_removed` events, `live` assessed nothing.
fn write_mask(directory: &Path, acquisition: String, peacoqc_removed: usize) -> PathBuf {
    let path = directory.join("analysis.parquet");
    let pass = (0..EVENTS)
        .map(|row| row >= peacoqc_removed)
        .collect::<Vec<_>>();
    EventMask::new(acquisition, pass.clone(), None)
        .with_filter("peacoqc_pass", pass.into_iter().map(Some).collect())
        .with_filter("live", vec![None; EVENTS])
        .write_parquet(&path)
        .expect("mask should write");
    path
}

/// Gate membership of every raw event: `T` is the first 2,000 events and
/// `rare` the last 10. With `masked`, the membership was computed under a
/// mask that removed event 0, so its gates are null there.
fn write_membership(directory: &Path, acquisition: String, masked: bool) -> PathBuf {
    let path = directory.join("membership.parquet");
    let mut t = (0..EVENTS).map(|row| Some(row < 2_000)).collect::<Vec<_>>();
    let mut keep = vec![true; EVENTS];
    if masked {
        t[0] = None;
        keep[0] = false;
    }
    EventMask::new(acquisition, keep, None)
        .with_filter("T", t)
        .with_filter(
            "rare",
            (0..EVENTS).map(|row| Some(row >= EVENTS - 10)).collect(),
        )
        .write_parquet(&path)
        .expect("membership should write");
    path
}

fn qc(directory: &Path, manifest: &Path, mask: &Path) -> (Output, Option<Value>) {
    qc_with(directory, manifest, mask, &[])
}

fn qc_with(
    directory: &Path,
    manifest: &Path,
    mask: &Path,
    flags: &[&str],
) -> (Output, Option<Value>) {
    let report = directory.join("qc.json");
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "qc",
            "--manifest",
            utf8(manifest),
            "--sample",
            "S01",
            "--mask",
            utf8(mask),
            "--output",
            utf8(&report),
            "--overwrite",
        ])
        .args(flags)
        .output()
        .expect("command should run");
    let report = fs::read_to_string(report)
        .ok()
        .map(|text| serde_json::from_str(&text).expect("report should be JSON"));
    (output, report)
}

fn failed(report: &Value) -> Vec<String> {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter(|check| check["status"] == "fail")
        .map(|check| format!("{} {}", check["scope"], check["check"]))
        .collect()
}

#[test]
fn reused_failed_qc_preserves_the_exit_code_and_report() {
    let directory = create_temp_dir("cytiq-engine-qc-reuse");
    let manifest = write_manifest(&directory, "  qc:\n    min_events: 20000\n");
    let mask = write_mask(
        &directory,
        common::acquisition_of(&fixture("sample.fcs")),
        1000,
    );
    let outcome = directory.join("outcome.json");
    let (first, _) = qc_with(
        &directory,
        &manifest,
        &mask,
        &["--reuse", "skip", "--outcome", utf8(&outcome)],
    );
    assert_eq!(first.status.code(), Some(3));
    let report = directory.join("qc.json");
    let stamp = fs::metadata(&report).unwrap().modified().unwrap();
    let (second, _) = qc_with(
        &directory,
        &manifest,
        &mask,
        &["--reuse", "skip", "--outcome", utf8(&outcome)],
    );
    assert_eq!(second.status.code(), Some(3));
    let result: Value = serde_json::from_slice(&fs::read(outcome).unwrap()).unwrap();
    assert_eq!(result["execution"]["reused"], true);
    assert_eq!(result["reason"], "checks_failed");
    assert_eq!(fs::metadata(report).unwrap().modified().unwrap(), stamp);
}

#[test]
fn population_retention_never_changes_status_or_exit_code() {
    let directory = create_temp_dir("cytiq-engine-qc-populations");
    // The policy fails: retention must not turn it into a pass, or the
    // reverse.
    let manifest = write_manifest(&directory, "  qc:\n    min_events: 20000\n");
    let acquisition = common::acquisition_of(&fixture("sample.fcs"));
    let mask = write_mask(&directory, acquisition.clone(), 1_000);
    let membership = write_membership(&directory, acquisition, false);

    let (plain_output, plain) = qc(&directory, &manifest, &mask);
    let (output, report) = qc_with(
        &directory,
        &manifest,
        &mask,
        &["--populations", utf8(&membership)],
    );

    let (plain, report) = (plain.expect("report"), report.expect("report"));
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.status.code(), plain_output.status.code());
    for key in [
        "status", "passed", "warnings", "failures", "metrics", "checks",
    ] {
        assert_eq!(report[key], plain[key], "{key}");
    }
    assert!(plain.get("population_retention").is_none());
    let retention = &report["population_retention"];
    assert_eq!(retention["status"], "assessed", "{retention}");
    assert!(
        retention["basis"]
            .as_str()
            .unwrap()
            .contains("never a QC check")
    );
    let combined = &retention["combined"]["populations"];
    assert_eq!(combined[0]["population"], "T");
    assert_eq!(combined[0]["original"], 2_000);
    assert_eq!(combined[0]["retained"], 1_000);
    assert_eq!(combined[0]["cumulative_retention"], 0.5);
    assert_eq!(combined[1]["population"], "rare");
    assert_eq!(combined[1]["retained"], 10);
    let filters = retention["filters"].as_array().unwrap();
    assert_eq!(filters[0]["stage"], "peacoqc_pass");
    assert_eq!(filters[0]["populations"][0]["retained"], 1_000);
    // `live` assessed nothing, so it removed nothing.
    assert_eq!(filters[1]["stage"], "live");
    assert_eq!(filters[1]["populations"][0]["retained"], 2_000);
    common::assert_provenance(
        &directory.join("qc.json.provenance.json"),
        "cytiq qc",
        &[&manifest, &fixture("sample.fcs"), &mask, &membership],
        &[&directory.join("qc.json")],
    );
}

#[test]
fn masked_membership_is_not_assessed() {
    let directory = create_temp_dir("cytiq-engine-qc-populations-masked");
    let manifest = write_manifest(&directory, "  qc:\n    min_events: 5000\n");
    let acquisition = common::acquisition_of(&fixture("sample.fcs"));
    let mask = write_mask(&directory, acquisition.clone(), 1_000);
    let membership = write_membership(&directory, acquisition, true);

    let (output, report) = qc_with(
        &directory,
        &manifest,
        &mask,
        &["--populations", utf8(&membership)],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let retention = &report.expect("report")["population_retention"];
    assert_eq!(retention["status"], "not_assessed");
    assert!(
        retention["reason"].as_str().unwrap().contains("--mask"),
        "{retention}"
    );
    assert!(retention.get("combined").is_none());
}

#[test]
fn changed_membership_invalidates_reuse() {
    let directory = create_temp_dir("cytiq-engine-qc-populations-reuse");
    let manifest = write_manifest(&directory, "  qc:\n    min_events: 5000\n");
    let acquisition = common::acquisition_of(&fixture("sample.fcs"));
    let mask = write_mask(&directory, acquisition.clone(), 1_000);
    let membership = write_membership(&directory, acquisition.clone(), false);
    let outcome = directory.join("outcome.json");
    let flags = [
        "--populations",
        utf8(&membership),
        "--reuse",
        "skip",
        "--outcome",
        utf8(&outcome),
    ];
    let reused = || -> bool {
        let result: Value = serde_json::from_slice(&fs::read(&outcome).unwrap()).unwrap();
        result["execution"]["reused"] == true
    };

    let (first, _) = qc_with(&directory, &manifest, &mask, &flags);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(!reused());
    qc_with(&directory, &manifest, &mask, &flags);
    assert!(reused());
    write_membership(&directory, acquisition, true);
    let (changed, report) = qc_with(&directory, &manifest, &mask, &flags);
    assert!(changed.status.success());
    assert!(!reused());
    assert_eq!(
        report.expect("report")["population_retention"]["status"],
        "not_assessed"
    );
}

#[test]
fn passes_a_mask_within_the_policy() {
    let directory = create_temp_dir("cytiq-engine-qc-pass");
    let manifest = write_manifest(
        &directory,
        "  qc:\n    min_events: 5000\n    max_removed_fraction: 0.5\n    filters:\n      peacoqc_pass: { required: true, max_removed_fraction: 0.3 }\n",
    );
    let mask = write_mask(
        &directory,
        common::acquisition_of(&fixture("sample.fcs")),
        1_000,
    );

    let (output, report) = qc(&directory, &manifest, &mask);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = report.expect("report");
    assert_eq!(report["status"], "pass");
    assert_eq!(report["panel"], "test");
    assert_eq!(report["metrics"]["kept"], 9_000);
    assert_eq!(report["metrics"]["filters"][0]["removed"], 1_000);
    common::assert_provenance(
        &directory.join("qc.json.provenance.json"),
        "cytiq qc",
        &[&manifest, &fixture("sample.fcs"), &mask],
        &[&directory.join("qc.json")],
    );
}

#[test]
fn fails_when_peacoqc_removes_too_much() {
    let directory = create_temp_dir("cytiq-engine-qc-peacoqc");
    let manifest = write_manifest(
        &directory,
        "  qc:\n    filters:\n      peacoqc_pass: { max_removed_fraction: 0.3 }\n",
    );
    let mask = write_mask(
        &directory,
        common::acquisition_of(&fixture("sample.fcs")),
        5_000,
    );

    let (output, report) = qc(&directory, &manifest, &mask);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("qc failed"));
    let report = report.expect("the report is written even on failure");
    assert_eq!(
        failed(&report),
        [r#""filter peacoqc_pass" "filter_removed_fraction""#]
    );
    let check = &report["checks"].as_array().expect("checks")[1];
    assert_eq!(
        (check["value"].as_f64(), check["limit"].as_f64()),
        (Some(0.5), Some(0.3))
    );
}

#[test]
fn fails_when_a_required_filter_was_not_assessed() {
    let directory = create_temp_dir("cytiq-engine-qc-required");
    let manifest = write_manifest(
        &directory,
        "  qc:\n    filters:\n      live: { required: true }\n      singlets: { required: true, severity: warn }\n",
    );
    let mask = write_mask(
        &directory,
        common::acquisition_of(&fixture("sample.fcs")),
        0,
    );

    let (output, report) = qc(&directory, &manifest, &mask);

    assert!(!output.status.success());
    let report = report.expect("report");
    assert_eq!(failed(&report), [r#""filter live" "filter_assessed""#]);
    assert_eq!(
        report["warnings"], 1,
        "singlets is missing, with severity warn"
    );
}

#[test]
fn fails_when_a_required_filter_assessed_few_events() {
    let directory = create_temp_dir("cytiq-engine-qc-assessed");
    let manifest = write_manifest(
        &directory,
        "  qc:\n    filters:\n      live: { required: true, min_assessed_fraction: 0.95 }\n",
    );
    // `live` ran on 100 of 10,000 events: enough for `required`, not for
    // the policy.
    let mask = directory.join("analysis.parquet");
    EventMask::new(
        common::acquisition_of(&fixture("sample.fcs")),
        vec![true; EVENTS],
        None,
    )
    .with_filter(
        "live",
        (0..EVENTS).map(|row| (row < 100).then_some(true)).collect(),
    )
    .write_parquet(&mask)
    .expect("mask should write");

    let (output, report) = qc(&directory, &manifest, &mask);

    assert!(!output.status.success());
    let report = report.expect("report");
    assert_eq!(
        failed(&report),
        [r#""filter live" "filter_assessed_fraction""#]
    );
    assert_eq!(report["metrics"]["filters"][0]["assessed_fraction"], 0.01);
}

#[test]
fn counts_a_later_filter_against_the_events_left_to_it() {
    let directory = create_temp_dir("cytiq-engine-qc-eligible");
    let rule = |of: &str| {
        format!(
            "  qc:\n    filters:\n      peacoqc_pass: {{ min_assessed_fraction: 0.99, assessed_of: {of} }}\n"
        )
    };
    // The margin mask removed the first 2,000 events; `PeacoQC` ran on the
    // other 8,000, all it saw.
    let mask = directory.join("analysis.parquet");
    let margin = (0..EVENTS).map(|row| row >= 2_000).collect::<Vec<_>>();
    EventMask::new(
        common::acquisition_of(&fixture("sample.fcs")),
        margin.clone(),
        None,
    )
    .with_filter("margin_pass", margin.iter().copied().map(Some).collect())
    .with_filter(
        "peacoqc_pass",
        margin.iter().map(|kept| kept.then_some(true)).collect(),
    )
    .write_parquet(&mask)
    .expect("mask should write");

    let (output, report) = qc(&directory, &write_manifest(&directory, &rule("raw")), &mask);
    assert!(!output.status.success(), "80% of raw events < 99%");
    assert_eq!(
        failed(&report.expect("report")),
        [r#""filter peacoqc_pass" "filter_assessed_fraction""#]
    );

    let (output, report) = qc(
        &directory,
        &write_manifest(&directory, &rule("eligible")),
        &mask,
    );
    let report = report.expect("report");
    assert!(output.status.success(), "{:?}", failed(&report));
    let peacoqc = &report["metrics"]["filters"][1];
    assert_eq!(peacoqc["eligible"], 8_000);
    assert_eq!(peacoqc["eligible_assessed_fraction"], 1.0);
}

#[test]
fn fails_when_the_mask_has_rows_beyond_the_raw_file() {
    let directory = create_temp_dir("cytiq-engine-qc-extra-rows");
    let manifest = write_manifest(
        &directory,
        "  qc:\n    filters:\n      live: { max_removed_fraction: 0.3 }\n",
    );
    // Every raw event is there, plus 5,000 rows `live` removed that belong to
    // no raw event: counted, they would double `live`'s removed fraction.
    let rows = EVENTS + 5_000;
    let mask = directory.join("analysis.parquet");
    EventMask::new(
        common::acquisition_of(&fixture("sample.fcs")),
        vec![true; rows],
        None,
    )
    .with_filter("live", (0..rows).map(|row| Some(row < 7_500)).collect())
    .write_parquet(&mask)
    .expect("mask should write");

    let (output, report) = qc(&directory, &manifest, &mask);

    assert!(!output.status.success());
    let report = report.expect("report");
    assert_eq!(failed(&report), [r#""sample S01" "mask_coverage""#]);
    assert!(report["metrics"].is_null());
    assert!(
        report["checks"][0]["message"]
            .as_str()
            .is_some_and(
                |message| message.contains("5000 row(s)") && message.contains("for example 10000")
            ),
        "{}",
        report["checks"][0]
    );
}

#[test]
fn fails_when_the_mask_belongs_to_another_sample() {
    let directory = create_temp_dir("cytiq-engine-qc-acquisition");
    let manifest = write_manifest(&directory, "");
    let mask = write_mask(
        &directory,
        common::acquisition_of(&fixture("control.fcs")),
        0,
    );

    let (output, report) = qc(&directory, &manifest, &mask);

    assert!(!output.status.success());
    let report = report.expect("report");
    assert_eq!(failed(&report), [r#""sample S01" "mask_acquisition""#]);
    assert!(report["metrics"].is_null());
}

#[test]
fn warns_without_a_policy_unless_one_is_required() {
    let directory = create_temp_dir("cytiq-engine-qc-no-policy");
    let manifest = write_manifest(&directory, "");
    let mask = write_mask(
        &directory,
        common::acquisition_of(&fixture("sample.fcs")),
        0,
    );

    // No threshold was checked, so the mask is never a plain pass.
    let (output, report) = qc(&directory, &manifest, &mask);
    assert!(output.status.success());
    let report = report.expect("report");
    assert_eq!(report["status"], "warn");
    assert_eq!(report["checks"][1]["check"], "policy");
    assert_eq!(report["checks"][1]["status"], "warn");

    let outcome = directory.join("outcome.json");
    let (output, report) = qc_with(
        &directory,
        &manifest,
        &mask,
        &["--require-policy", "--outcome", utf8(&outcome)],
    );
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        failed(&report.expect("report")),
        [r#""sample S01" "policy""#]
    );
    let outcome: Value =
        serde_json::from_str(&fs::read_to_string(outcome).expect("outcome")).expect("JSON");
    assert_eq!(outcome["reason"], "checks_failed");
    assert_eq!(outcome["exit_code"], 3);
    assert_eq!(outcome["completed"], true);
    assert_eq!(outcome["command"], "cytiq qc");
}

#[test]
fn rejects_an_unknown_sample() {
    let directory = create_temp_dir("cytiq-engine-qc-unknown-sample");
    let manifest = write_manifest(&directory, "");
    let mask = write_mask(
        &directory,
        common::acquisition_of(&fixture("sample.fcs")),
        0,
    );

    let unknown = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "qc",
            "--manifest",
            utf8(&manifest),
            "--sample",
            "S99",
            "--mask",
            utf8(&mask),
            "--output",
            utf8(&directory.join("unknown.json")),
        ])
        .output()
        .expect("command should run");
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("sample `S99` is not in manifest"));
    assert!(!directory.join("unknown.json").exists());
}
