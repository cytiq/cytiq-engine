mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use cytiq_fcs::{ColumnData, FcsReader, FcsWriteOptions, FcsWriteParameter};

#[test]
fn cli_runs_transform_default_arcsinh_with_metadata() {
    let directory = create_temp_dir("cytiq-engine-transform-default");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    let metadata_path = directory.join("metadata.json");
    let debug_dir = directory.join("debug");
    write_transform_fixture_fcs(&input, 0.0);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output_path),
            "--metadata-json",
            utf8(&metadata_path),
            "--debug-dir",
            utf8(&debug_dir),
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dataset = FcsReader::open(&output_path)
        .and_then(FcsReader::read)
        .expect("transformed FCS should be readable");
    assert!(dataset.text.get("CYTIQ_TRANSFORM").is_some());
    assert_f32_column(dataset.data.column(0), &[0.0, 1.0, 2.0]);
    assert_f32_column(dataset.data.column(1), &[10.0, 11.0, 12.0]);
    assert_f32_column_close(
        dataset.data.column(3),
        &[
            (0.0_f64 / 150.0).asinh(),
            (150.0_f64 / 150.0).asinh(),
            (300.0_f64 / 150.0).asinh(),
        ],
    );

    let metadata = fs::read_to_string(metadata_path).expect("metadata should exist");
    assert!(metadata.contains("\"method\": \"arcsinh\""));
    assert!(metadata.contains("\"channels\""));
    assert!(debug_dir.join("summary.json").is_file());
    assert!(debug_dir.join("metrics.csv").is_file());
    let report = fs::read_to_string(debug_dir.join("report.html")).expect("report should exist");
    assert!(report.contains("plotly"));
    let metrics = fs::read_to_string(debug_dir.join("metrics.csv")).expect("metrics should exist");
    assert!(metrics.contains("raw_mean"));
    assert!(metrics.contains("transformed_mean"));
}

#[test]
fn cli_transforms_repeated_inputs_to_output_directory() {
    let directory = create_temp_dir("cytiq-engine-transform-repeated");
    let input_a = directory.join("sample-a.fcs");
    let input_b = directory.join("sample-b.fcs");
    let output_dir = directory.join("out");
    write_transform_fixture_fcs(&input_a, 0.0);
    write_transform_fixture_fcs(&input_b, 10.0);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&input_a),
            "--input",
            utf8(&input_b),
            "--output",
            utf8(&output_dir),
            "--method",
            "none",
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_dir.join("sample-a.transformed.fcs").is_file());
    assert!(output_dir.join("sample-b.transformed.fcs").is_file());
}

#[test]
fn cli_transforms_glob_inputs_to_output_directory() {
    let directory = create_temp_dir("cytiq-engine-transform-glob");
    write_transform_fixture_fcs(&directory.join("sample-1.fcs"), 0.0);
    write_transform_fixture_fcs(&directory.join("sample-2.fcs"), 10.0);
    fs::write(directory.join("notes.txt"), "ignored").expect("notes should write");
    let output_dir = directory.join("out");
    let glob = directory.join("sample-*.fcs");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&glob),
            "--output",
            utf8(&output_dir),
            "--method",
            "none",
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_dir.join("sample-1.transformed.fcs").is_file());
    assert!(output_dir.join("sample-2.transformed.fcs").is_file());
}

#[test]
fn cli_applies_channels_and_exclude_channels() {
    let directory = create_temp_dir("cytiq-engine-transform-channels");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    write_transform_fixture_fcs(&input, 0.0);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output_path),
            "--channels",
            "CD3,CD4",
            "--exclude-channels",
            "CD4",
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dataset = FcsReader::open(&output_path)
        .and_then(FcsReader::read)
        .expect("transformed FCS should be readable");
    assert_f32_column_close(
        dataset.data.column(3),
        &[
            (0.0_f64 / 150.0).asinh(),
            (150.0_f64 / 150.0).asinh(),
            (300.0_f64 / 150.0).asinh(),
        ],
    );
    assert_f32_column(dataset.data.column(4), &[30.0, 180.0, 330.0]);
}

#[test]
fn cli_runs_logicle_biexponential_and_hyperlog_methods() {
    for method in ["logicle", "biexponential", "hyperlog"] {
        let directory = create_temp_dir(&format!("cytiq-engine-transform-{method}"));
        let input = directory.join("sample.fcs");
        let output_path = directory.join(format!("sample.{method}.fcs"));
        write_transform_fixture_fcs(&input, 0.0);

        let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
            .args([
                "transform",
                "--input",
                utf8(&input),
                "--output",
                utf8(&output_path),
                "--method",
                method,
                "--channels",
                "CD3,CD4",
                "--threads",
                "2",
            ])
            .output()
            .expect("command should run");

        assert!(
            output.status.success(),
            "method {method} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let dataset = FcsReader::open(&output_path)
            .and_then(FcsReader::read)
            .expect("transformed FCS should be readable");
        let Some(ColumnData::F32(values)) = dataset.data.column(3) else {
            panic!("expected f32 column");
        };
        assert!(values.iter().all(|value| value.is_finite()));
        assert_ne!(values, &[0.0, 150.0, 300.0]);
    }
}

#[test]
fn cli_dry_run_writes_no_outputs() {
    let directory = create_temp_dir("cytiq-engine-transform-dry-run");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    let metadata_path = directory.join("metadata.json");
    write_transform_fixture_fcs(&input, 0.0);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output_path),
            "--metadata-json",
            utf8(&metadata_path),
            "--dry-run",
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cytiq transform dry run"));
    assert!(stdout.contains("channels: FL1-A,FL2-A"));
    assert!(!output_path.exists());
    assert!(!metadata_path.exists());
}

#[test]
fn cli_rejects_existing_transform_output_without_overwrite() {
    let directory = create_temp_dir("cytiq-engine-transform-output-exists");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    write_transform_fixture_fcs(&input, 0.0);
    write_transform_fixture_fcs(&output_path, 0.0);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output_path),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already exists"));
}

#[test]
fn cli_rejects_existing_metadata_before_writing_transform_output() {
    let directory = create_temp_dir("cytiq-engine-transform-metadata-exists");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    let metadata_path = directory.join("metadata.json");
    write_transform_fixture_fcs(&input, 0.0);
    fs::write(&metadata_path, "{}").expect("metadata placeholder should write");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output_path),
            "--metadata-json",
            utf8(&metadata_path),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("metadata output"));
    assert!(!output_path.exists());
}

#[test]
fn cli_requires_transform_input() {
    let directory = create_temp_dir("cytiq-engine-transform-requires-input");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--output",
            utf8(&directory.join("sample.transformed.fcs")),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--input"));
}

#[test]
fn cli_rejects_unsupported_transform_format() {
    let directory = create_temp_dir("cytiq-engine-transform-format");
    let input = directory.join("sample.fcs");
    write_transform_fixture_fcs(&input, 0.0);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&input),
            "--output",
            utf8(&directory.join("sample.csv")),
            "--format",
            "csv",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not supported"));
}

#[test]
fn cli_transform_carries_metadata_and_moves_spillover() {
    let directory = create_temp_dir("cytiq-engine-transform-metadata");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    let parameters = vec![
        FcsWriteParameter::new("Time"),
        FcsWriteParameter::new("FSC-A").with_range(262_144.0),
        FcsWriteParameter::with_label("FL1-A", "CD3")
            .with_range(262_144.0)
            .with_keyword("V", "450"),
    ];
    let columns = vec![
        vec![0.0, 1.0, 2.0],
        vec![10.0, 11.0, 12.0],
        vec![0.0, 150.0, 262_143.0],
    ];
    let spillover = "1,FL1-A,1";
    let options = FcsWriteOptions::new()
        .with_text_keyword("$CYT", "Aurora")
        .with_text_keyword("$SPILLOVER", spillover);
    cytiq_fcs::write_fcs3_1_f32_with_options(&input, &parameters, &columns, &options)
        .expect("fixture FCS should write");

    let output = run_transform(&[
        utf8(&input),
        "--output",
        utf8(&output_path),
        "--allow-uncompensated",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dataset = FcsReader::open(&output_path)
        .and_then(FcsReader::read)
        .expect("transformed FCS should be readable");
    assert_eq!(dataset.text.get("$CYT"), Some("Aurora"));
    assert_eq!(dataset.text.get("$SPILLOVER"), None);
    assert_eq!(dataset.text.get("CYTIQ_SOURCE_SPILL"), Some(spillover));
    assert_eq!(dataset.text.get("$P2R"), Some("262144"));
    assert_eq!(dataset.text.get("$P3V"), Some("450"));
    assert_ne!(dataset.text.get("$P3R"), Some("262144"));

    let margins = cytiq_fcs::recorded_margin_bounds(&dataset.text)
        .expect("margins should parse")
        .expect("margins should be recorded");
    let upper = margins["FL1-A"].upper.expect("FL1-A has a range");
    // Bounds are rounded to f32 like the transformed values.
    assert!((upper - (262_143.0_f64 / 150.0).asinh()).abs() <= 1.0e-6);
    assert_eq!(margins["FSC-A"].upper, Some(262_143.0));
    common::assert_provenance(
        &directory.join("sample.transformed.fcs.provenance.json"),
        "cytiq transform",
        &[&input],
        &[&output_path],
    );
}

#[test]
fn cli_rejects_already_transformed_input() {
    let directory = create_temp_dir("cytiq-engine-transform-twice");
    let input = directory.join("sample.fcs");
    let once = directory.join("once.fcs");
    let twice = directory.join("twice.fcs");
    write_transform_fixture_fcs(&input, 0.0);

    let first = run_transform(&[utf8(&input), "--output", utf8(&once)]);
    assert!(first.status.success());

    let second = run_transform(&[utf8(&once), "--output", utf8(&twice)]);
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("already transformed"), "stderr: {stderr}");
    assert!(!twice.exists());

    let forced = run_transform(&[utf8(&once), "--output", utf8(&twice), "--allow-retransform"]);
    assert!(
        forced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
}

#[test]
fn cli_accepts_negative_logicle_a() {
    let directory = create_temp_dir("cytiq-engine-transform-negative-a");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    write_transform_fixture_fcs(&input, 0.0);

    let output = run_transform(&[
        utf8(&input),
        "--output",
        utf8(&output_path),
        "--method",
        "logicle",
        "--logicle-a",
        "-0.25",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let out_of_range = run_transform(&[
        utf8(&input),
        "--output",
        utf8(&directory.join("rejected.fcs")),
        "--method",
        "logicle",
        "--logicle-a",
        "-1",
    ]);
    assert!(!out_of_range.status.success());
    let stderr = String::from_utf8_lossy(&out_of_range.stderr);
    assert!(stderr.contains("A is out of range"), "stderr: {stderr}");
}

/// FL1-A and FL2-A markers plus an `Original_ID` beyond f32 precision (2^24 + 1
/// is the first integer f32 cannot represent).
fn write_lineage_fixture_fcs(path: &Path) -> Vec<f64> {
    let ids = vec![16_777_217.0, 16_777_219.0, 16_777_221.0];
    let parameters = vec![
        FcsWriteParameter::with_label("FL1-A", "CD3"),
        FcsWriteParameter::with_label("FL2-A", "CD4"),
        FcsWriteParameter::new("Original_ID"),
    ];
    let columns = vec![
        vec![0.0, 150.0, 300.0],
        vec![30.0, 180.0, 330.0],
        ids.clone(),
    ];
    cytiq_fcs::write_fcs3_1_f64_with_options(path, &parameters, &columns, &FcsWriteOptions::new())
        .expect("fixture FCS should write");
    ids
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn transform_leaves_original_id_exact() {
    let directory = create_temp_dir("cytiq-engine-transform-original-id");
    let input = directory.join("sample.peacoqc.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    let ids = write_lineage_fixture_fcs(&input);

    let output = run_transform(&[utf8(&input), "--output", utf8(&output_path)]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dataset = FcsReader::open(&output_path)
        .and_then(FcsReader::read)
        .expect("transformed FCS should be readable");
    let Some(ColumnData::F64(written)) = dataset.data.column(2) else {
        panic!("identifiers beyond 2^24 need a double column");
    };
    assert_eq!(written, &ids);
    let Some(ColumnData::F64(cd3)) = dataset.data.column(0) else {
        panic!("expected a double column");
    };
    for (actual, raw) in cd3.iter().zip([0.0_f64, 150.0, 300.0]) {
        // Transformed values keep f32 precision whatever the file encoding.
        assert_eq!(*actual, f64::from((raw / 150.0).asinh() as f32));
    }
    let state = cytiq_fcs::transform_state(&dataset.text)
        .expect("state should parse")
        .expect("transform state");
    assert_eq!(state.channels, ["FL1-A", "FL2-A"]);
    assert_eq!(
        dataset.text.get(cytiq_fcs::ACQUISITION_KEYWORD),
        Some(common::sha256_hex(&input).as_str())
    );
}

#[test]
fn rejects_transforming_original_id() {
    let directory = create_temp_dir("cytiq-engine-transform-reserved");
    let input = directory.join("sample.peacoqc.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    write_lineage_fixture_fcs(&input);

    let output = run_transform(&[
        utf8(&input),
        "--output",
        utf8(&output_path),
        "--channels",
        "FL1-A,Original_ID",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("event lineage column"), "stderr: {stderr}");
    assert!(!output_path.exists());
}

#[test]
fn in_place_transform_records_the_original_input_hash() {
    let directory = create_temp_dir("cytiq-engine-transform-in-place");
    let file = directory.join("sample.fcs");
    write_transform_fixture_fcs(&file, 0.0);
    let before = common::sha256_hex(&file);

    let output = run_transform(&[utf8(&file), "--output", utf8(&file), "--overwrite"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = common::sha256_hex(&file);
    assert_ne!(before, after);
    let record: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(directory.join("sample.fcs.provenance.json"))
            .expect("provenance should exist"),
    )
    .expect("provenance should be JSON");
    assert_eq!(record["inputs"][0]["sha256"], before.as_str());
    assert_eq!(record["outputs"][0]["sha256"], after.as_str());
    // The rewritten file still names the original as its acquisition.
    assert_eq!(common::acquisition_of(&file), before);
}

#[test]
fn transform_requires_compensation_first() {
    let directory = create_temp_dir("cytiq-engine-transform-uncompensated");
    let raw =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/gating/sample.fcs");
    let output_path = directory.join("sample.t.fcs");

    let refused = run_transform(&[utf8(&raw), "--output", utf8(&output_path)]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("run `cytiq compensate` first") && stderr.contains("--allow-uncompensated"),
        "stderr: {stderr}"
    );
    assert!(!output_path.exists());

    let compensated = directory.join("sample.comp.fcs");
    let compensate = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "compensate",
            "--input",
            utf8(&raw),
            "--output",
            utf8(&compensated),
        ])
        .output()
        .expect("command should run");
    assert!(compensate.status.success());
    let output = run_transform(&[utf8(&compensated), "--output", utf8(&output_path)]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_transform(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(["transform", "--input"])
        .args(args)
        .output()
        .expect("command should run")
}

fn write_transform_fixture_fcs(path: &Path, offset: f32) {
    let columns = vec![
        vec![0.0, 1.0, 2.0],
        vec![10.0 + offset, 11.0 + offset, 12.0 + offset],
        vec![20.0 + offset, 21.0 + offset, 22.0 + offset],
        vec![0.0 + offset, 150.0 + offset, 300.0 + offset],
        vec![30.0 + offset, 180.0 + offset, 330.0 + offset],
    ];
    let parameters = vec![
        FcsWriteParameter::new("Time"),
        FcsWriteParameter::new("FSC-A"),
        FcsWriteParameter::new("SSC-A"),
        FcsWriteParameter::with_label("FL1-A", "CD3"),
        FcsWriteParameter::with_label("FL2-A", "CD4"),
    ];
    cytiq_fcs::write_fcs3_1_f32(path, &parameters, &columns).expect("fixture FCS should write");
}

fn assert_f32_column(column: Option<&ColumnData>, expected: &[f32]) {
    let Some(ColumnData::F32(values)) = column else {
        panic!("expected f32 column");
    };
    assert_eq!(values, expected);
}

fn assert_f32_column_close(column: Option<&ColumnData>, expected: &[f64]) {
    let Some(ColumnData::F32(values)) = column else {
        panic!("expected f32 column");
    };
    assert_eq!(values.len(), expected.len());
    for (actual, expected) in values.iter().zip(expected) {
        assert!(
            (f64::from(*actual) - expected).abs() <= 1.0e-5,
            "expected {expected}, got {actual}"
        );
    }
}

/// A Navios names its scatter `FS INT`, `SS PEAK`, `SS INT` and `SS TOF`; they
/// were transformed like fluorescence when only `FSC-A/H/W` and `SSC-A/H/W`
/// counted as scatter, so gates and plots saw asinh scatter.
#[test]
fn transform_leaves_instrument_scatter_channels_linear() {
    let directory = create_temp_dir("cytiq-engine-transform-navios");
    let raw =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/flowcyt/Case12_P.fcs");
    let compensated = directory.join("case12.comp.fcs");
    let transformed = directory.join("case12.t.fcs");
    let compensate = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "compensate",
            "--input",
            utf8(&raw),
            "--output",
            utf8(&compensated),
        ])
        .output()
        .expect("command should run");
    assert!(
        compensate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&compensate.stderr)
    );
    let output = run_transform(&[utf8(&compensated), "--output", utf8(&transformed)]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let read = |path: &Path| {
        FcsReader::open(path)
            .and_then(FcsReader::read)
            .expect("FCS should be readable")
    };
    let (before, after) = (read(&compensated), read(&transformed));
    let record: serde_json::Value =
        serde_json::from_str(after.text.get("CYTIQ_TRANSFORM").expect("transform record"))
            .expect("transform record is JSON");
    let channels = record["files"][0]["channels"]
        .as_array()
        .expect("transformed channels")
        .iter()
        .map(|channel| channel.as_str().expect("channel name"))
        .collect::<Vec<_>>();
    let fluorescence = (1..=10).map(|n| format!("FL{n} INT")).collect::<Vec<_>>();
    assert_eq!(channels, fluorescence, "only fluorescence is transformed");
    for scatter in ["FS INT", "SS PEAK", "SS INT", "SS TOF"] {
        let index = after.find_parameter(scatter).expect("scatter channel");
        assert_eq!(
            after.data.column(index).map(ColumnData::to_f64_vec),
            before.data.column(index).map(ColumnData::to_f64_vec),
            "{scatter} keeps its values"
        );
    }
}

fn create_temp_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("cytiq-{label}-{unique}"));
    fs::create_dir_all(&directory).expect("temporary directory should be created");
    directory
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be valid utf-8")
}

#[test]
fn cli_rejects_log_amplified_input() {
    let directory = create_temp_dir("cytiq-engine-transform-log");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.transformed.fcs");
    write_transform_fixture_fcs(&input, 0.0);
    common::set_log_amplified(&input, "FL1-A");

    let output = run_transform(&[utf8(&input), "--output", utf8(&output_path)]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`FL1-A` store log-amplified values"),
        "stderr: {stderr}"
    );
    assert!(!output_path.exists());
}

mod biexp_reference {
    #![allow(dead_code, clippy::unreadable_literal, clippy::excessive_precision)]
    include!("../../../data/fixtures/transform/flowjo_biexp_params.rs");
}

/// `--biexponential-*` reach `flowjo_biexp` and are recorded in the transform
/// contract (reference: `parity/gen/flowjo_biexp_params.R`).
#[test]
fn cli_biexponential_parameters_match_flowworkspace() {
    let directory = create_temp_dir("cytiq-engine-transform-biexp-params");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.biexp.fcs");
    let metadata_path = directory.join("metadata.json");
    #[allow(clippy::cast_possible_truncation)]
    let values = biexp_reference::BIEXP_PARAMS_INPUT
        .iter()
        .map(|value| *value as f32)
        .collect::<Vec<_>>();
    let columns = vec![(0_u16..12).map(f32::from).collect(), values];
    let parameters = vec![
        FcsWriteParameter::new("Time"),
        FcsWriteParameter::with_label("FL1-A", "CD3"),
    ];
    cytiq_fcs::write_fcs3_1_f32(&input, &parameters, &columns).expect("fixture FCS");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output_path),
            "--method",
            "biexponential",
            "--channels",
            "CD3",
            "--biexponential-length",
            "1024",
            "--biexponential-max-range",
            "100000",
            "--biexponential-pos",
            "4",
            "--biexponential-neg",
            "0.5",
            "--biexponential-width",
            "-50",
            "--metadata-json",
            utf8(&metadata_path),
        ])
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dataset = FcsReader::open(&output_path)
        .and_then(FcsReader::read)
        .expect("transformed FCS should be readable");
    let Some(ColumnData::F32(values)) = dataset.data.column(1) else {
        panic!("expected f32 column");
    };
    for (actual, expected) in values.iter().zip(biexp_reference::BIEXP_NEG) {
        // Output is float32: compare at its precision.
        assert!(
            (f64::from(*actual) - expected).abs() <= 1e-6 * expected.abs().max(1.0),
            "{actual} vs {expected}"
        );
    }
    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(metadata_path).expect("metadata"))
            .expect("metadata JSON");
    assert_eq!(
        metadata["parameters"]["biexponential"],
        serde_json::json!({"length": 1024.0, "max_range": 100_000.0, "pos": 4.0, "neg": 0.5, "width": -50.0})
    );
}
