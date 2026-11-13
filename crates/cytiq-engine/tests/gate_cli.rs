mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures/gating")
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

fn run_gate(output: &Path, format: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "gate",
            "--input",
            utf8(&fixture("sample.fcs")),
            "--gating-ml",
            utf8(&fixture("gates.xml")),
            "--output",
            utf8(output),
            "--format",
            format,
            "--threads",
            "2",
        ])
        .output()
        .expect("command should run")
}

#[test]
fn cli_writes_gate_counts_csv() {
    let directory = create_temp_dir("cytiq-engine-gate-csv");
    let output_path = directory.join("nested").join("counts.csv");

    let output = run_gate(&output_path, "csv");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = csv::Reader::from_path(&output_path).expect("counts CSV should exist");
    let headers = reader.headers().expect("CSV should have headers").clone();
    assert_eq!(
        headers.iter().collect::<Vec<_>>(),
        [
            "gate_id",
            "gate_path",
            "parent_id",
            "gate_type",
            "count",
            "parent_count",
            "percent_of_parent",
            "percent_of_total"
        ]
    );
    let rows = reader
        .records()
        .collect::<Result<Vec<_>, _>>()
        .expect("CSV rows should parse");
    assert_eq!(rows.len(), 11);
    let cd3 = rows
        .iter()
        .find(|row| &row[0] == "CD3pos")
        .expect("CD3pos row should exist");
    assert_eq!(&cd3[1], "Cells/Singlets/CD3pos");
    assert_eq!(&cd3[2], "Singlets");
    assert_eq!(&cd3[4], "1637");
    assert_eq!(&cd3[5], "2762");

    fs::remove_dir_all(directory).ok();
}

#[test]
fn cli_writes_gate_counts_json() {
    let directory = create_temp_dir("cytiq-engine-gate-json");
    let output_path = directory.join("counts.json");

    let output = run_gate(&output_path, "json");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = fs::read_to_string(&output_path).expect("counts JSON should exist");
    assert!(json.trim_start().starts_with('['));
    assert!(json.contains(r#""gate_id": "Ellipse_or_log""#));
    assert!(json.contains(r#""parent_id": null"#));

    fs::remove_dir_all(directory).ok();
}

#[test]
fn cli_fails_when_gating_ml_is_missing() {
    let directory = create_temp_dir("cytiq-engine-gate-missing");
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "gate",
            "--input",
            utf8(&fixture("sample.fcs")),
            "--gating-ml",
            utf8(&directory.join("missing.xml")),
            "--output",
            utf8(&directory.join("counts.csv")),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing.xml"));

    fs::remove_dir_all(directory).ok();
}

#[test]
fn cli_refuses_an_oversized_gating_ml_before_parsing() {
    let directory = create_temp_dir("cytiq-engine-gate-oversized");
    let gating_ml = directory.join("huge.xml");
    fs::File::create(&gating_ml)
        .expect("create sparse file")
        .set_len(cytiq_gating::GATINGML_BYTES_MAX + 1)
        .expect("extend sparse file");
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(["gate", "--input", utf8(&fixture("sample.fcs"))])
        .args(["--gating-ml", utf8(&gating_ml)])
        .args(["--output", utf8(&directory.join("counts.csv"))])
        .output()
        .expect("command should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("exceeds"), "{stderr}");
    assert!(!directory.join("counts.csv").exists());
    fs::remove_dir_all(directory).ok();
}

#[test]
fn cli_refuses_to_transform_already_transformed_channels() {
    let directory = create_temp_dir("cytiq-engine-gate-transformed");
    let transformed = directory.join("sample.transformed.fcs");
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--allow-uncompensated",
            "--input",
            utf8(&fixture("sample.fcs")),
            "--output",
            utf8(&transformed),
        ])
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "gate",
            "--input",
            utf8(&transformed),
            "--gating-ml",
            utf8(&fixture("gates.xml")),
            "--output",
            utf8(&directory.join("counts.csv")),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("a second time"), "stderr: {stderr}");

    fs::remove_dir_all(directory).ok();
}

fn every_third_removed(events: usize) -> Vec<bool> {
    (0..events).map(|row| row % 3 != 0).collect()
}

#[test]
#[allow(clippy::too_many_lines)]
fn cli_gates_only_events_inside_the_mask() {
    let directory = create_temp_dir("cytiq-engine-gate-mask");
    let input = fixture("sample.fcs");
    let dataset = cytiq_fcs::FcsReader::open(&input)
        .and_then(cytiq_fcs::FcsReader::read)
        .expect("fixture should be readable");
    let keep = every_third_removed(dataset.event_count());
    let mask_path = directory.join("mask.parquet");
    cytiq_mask::EventMask::new(common::acquisition_of(&input), keep.clone(), None)
        .write_parquet(&mask_path)
        .expect("mask should write");

    // The same events as a physically filtered copy of the file.
    let filtered = directory.join("filtered.fcs");
    let parameters = dataset
        .parameters
        .iter()
        .map(cytiq_fcs::FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    let columns = dataset
        .data
        .columns
        .iter()
        .map(|column| {
            column
                .to_f64_vec()
                .into_iter()
                .zip(&keep)
                .filter_map(|(value, kept)| kept.then_some(value))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    cytiq_fcs::write_fcs3_1_f64_with_options(
        &filtered,
        &parameters,
        &columns,
        &cytiq_fcs::FcsWriteOptions::new()
            .with_text_keywords(cytiq_fcs::carried_keywords(&dataset.text)),
    )
    .expect("filtered copy should write");

    let gates = fixture("gates.xml");
    let counts = |input: &Path, extra: &[&str], name: &str| {
        let output = directory.join(name);
        let mut args = vec![
            "gate",
            "--input",
            utf8(input),
            "--gating-ml",
            utf8(&gates),
            "--output",
            utf8(&output),
            "--format",
            "json",
        ];
        args.extend_from_slice(extra);
        let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
            .args(&args)
            .output()
            .expect("command should run");
        assert!(
            result.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::read_to_string(output).expect("counts should exist")
    };

    let membership_path = directory.join("membership.parquet");
    let masked = counts(
        &input,
        &[
            "--mask",
            utf8(&mask_path),
            "--membership",
            utf8(&membership_path),
        ],
        "masked.json",
    );
    assert_eq!(masked, counts(&filtered, &[], "filtered.json"));

    let strategy = cytiq_gating::parse_gatingml(
        &fs::read_to_string(fixture("gates.xml")).expect("GatingML should be readable"),
    )
    .expect("GatingML should parse");
    let expected = strategy
        .gate_sample_masked(&dataset, Some(&keep))
        .expect("gating should succeed");
    common::assert_provenance(
        &directory.join("masked.json.provenance.json"),
        "cytiq gate",
        &[&input, &gates, &mask_path],
        &[&directory.join("masked.json"), &membership_path],
    );
    let membership = cytiq_mask::EventMask::read_parquet(&membership_path)
        .expect("membership should be a valid mask file");
    assert_eq!(membership.keep, keep);
    assert_eq!(
        membership
            .filters
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        expected.gate_ids().collect::<Vec<_>>()
    );
    for (id, column) in &membership.filters {
        let members = expected.membership(id).expect("gate should exist");
        for ((value, member), kept) in column.iter().zip(members).zip(&keep) {
            assert_eq!(*value, kept.then_some(*member), "gate {id}");
        }
    }

    fs::remove_dir_all(directory).ok();
}

#[test]
fn fails_when_mask_belongs_to_another_file() {
    let directory = create_temp_dir("cytiq-engine-gate-foreign-mask");
    let input = fixture("sample.fcs");
    let events = cytiq_fcs::FcsReader::open(&input)
        .and_then(cytiq_fcs::FcsReader::read)
        .expect("fixture should be readable")
        .event_count();
    // Enough rows for every event, but computed on another raw file.
    let mask_path = directory.join("mask.parquet");
    cytiq_mask::EventMask::new("0".repeat(64), vec![true; events], None)
        .write_parquet(&mask_path)
        .expect("mask should write");
    let output = directory.join("counts.json");

    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "gate",
            "--input",
            utf8(&input),
            "--gating-ml",
            utf8(&fixture("gates.xml")),
            "--output",
            utf8(&output),
            "--mask",
            utf8(&mask_path),
        ])
        .output()
        .expect("command should run");

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("was computed on acquisition"), "{stderr}");
    assert!(!output.exists());
}
