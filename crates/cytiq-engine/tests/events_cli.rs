mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use cytiq_population::events::{self, EventsSummary, LayerState, Origin, StepKind};
use cytiq_population::issue::Issues;

fn fixture(directory: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures")
        .join(directory)
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

fn cytiq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run")
}

fn succeed(args: &[&str]) {
    let output = cytiq(args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn export(input: &Path, output: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "events",
        "export",
        "--input",
        utf8(input),
        "--output",
        utf8(output),
    ];
    args.extend_from_slice(extra);
    cytiq(&args)
}

fn verified(path: &Path) -> EventsSummary {
    let mut issues = Issues::new();
    events::verify_file(path, "", &mut issues)
        .unwrap_or_else(|| panic!("{} should verify: {:?}", path.display(), issues.items()))
}

#[test]
fn exports_raw_events_with_provenance_and_no_identifying_text() {
    let directory = create_temp_dir("cytiq-events-raw");
    let input = fixture("flowcyt", "Case12_P.fcs");
    let output = directory.join("case12.raw.parquet");
    let result = export(&input, &output, &[]);
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    common::assert_provenance(
        &cytiq_provenance::record_path_for_file(&output),
        "cytiq events export",
        &[&input],
        &[&output],
    );
    let summary = verified(&output);
    assert_eq!(
        summary.descriptor.acquisition_id,
        common::sha256_hex(&input)
    );
    assert_eq!(summary.descriptor.state, LayerState::Raw);
    assert_eq!(summary.rows, 3);
    assert_eq!(summary.descriptor.channels.len(), 15);
    let bytes = fs::read(&output).unwrap();
    for needle in [
        "Case12_P",
        "Kaluza Cytobank",
        "04-Dec-15",
        "11:47:17",
        "Navios",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "`{needle}` leaked into the table"
        );
    }
}

#[test]
fn compensated_and_transformed_layers_chain_on_one_acquisition() {
    let directory = create_temp_dir("cytiq-events-chain");
    let input = fixture("flowcyt", "Case12_P.fcs");
    let compensated = directory.join("case12.comp.fcs");
    let transformed = directory.join("case12.trans.fcs");
    succeed(&[
        "compensate",
        "--input",
        utf8(&input),
        "--output",
        utf8(&compensated),
    ]);
    succeed(&[
        "transform",
        "--input",
        utf8(&compensated),
        "--output",
        utf8(&transformed),
    ]);
    let mut layers = Vec::new();
    for (index, file) in [&input, &compensated, &transformed].into_iter().enumerate() {
        let output = directory.join(format!("layer{index}.parquet"));
        let result = export(file, &output, &[]);
        assert_eq!(
            result.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        layers.push(verified(&output).descriptor);
    }
    let states: Vec<LayerState> = layers.iter().map(|layer| layer.state).collect();
    assert_eq!(
        states,
        [
            LayerState::Raw,
            LayerState::Compensated,
            LayerState::Transformed
        ]
    );
    assert!(
        layers
            .iter()
            .all(|l| l.acquisition_id == layers[0].acquisition_id)
    );
    assert!(layers[0].is_parent_of(&layers[1]) && layers[1].is_parent_of(&layers[2]));
    let compensation = &layers[1].steps[0];
    assert_eq!(
        (compensation.kind, compensation.origin),
        (StepKind::Compensation, Origin::Recorded)
    );
    assert_eq!(layers[2].steps[1].method, "arcsinh");
    // The derived file's own producer record is an input of the export.
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(cytiq_provenance::record_path_for_file(
            &directory.join("layer1.parquet"),
        ))
        .unwrap(),
    )
    .unwrap();
    let roles: Vec<&str> = record["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|input| input["role"].as_str())
        .collect();
    assert_eq!(roles, ["input", "producer_provenance"]);
}

fn write_matrix(path: &Path, spillover: &cytiq_fcs::Spillover, scale: f64) {
    let channels = spillover.channels();
    let mut text = format!("channel,{}\n", channels.join(","));
    for (row, channel) in channels.iter().enumerate() {
        let cells: Vec<String> = spillover.matrix()
            [row * channels.len()..(row + 1) * channels.len()]
            .iter()
            .enumerate()
            .map(|(column, value)| {
                let value = if row == column { *value } else { value * scale };
                format!("{value:.17}")
            })
            .collect();
        writeln!(text, "{channel},{}", cells.join(",")).unwrap();
    }
    fs::write(path, text).unwrap();
}

#[test]
fn a_compensation_matrix_is_verified_against_the_producing_run() {
    let directory = create_temp_dir("cytiq-events-matrix");
    let input = fixture("flowcyt", "Case12_P.fcs");
    let dataset = cytiq_fcs::FcsReader::open(&input)
        .and_then(cytiq_fcs::FcsReader::read)
        .unwrap();
    let spillover = cytiq_fcs::Spillover::from_dataset(&dataset)
        .unwrap()
        .unwrap();
    let matrix = directory.join("spillover.csv");
    let other = directory.join("other.csv");
    write_matrix(&matrix, &spillover, 1.0);
    write_matrix(&other, &spillover, 0.5);
    let compensated = directory.join("case12.comp.fcs");
    succeed(&[
        "compensate",
        "--input",
        utf8(&input),
        "--output",
        utf8(&compensated),
        "--matrix",
        utf8(&matrix),
        "--matrix-kind",
        "spillover",
    ]);
    let output = directory.join("layer.parquet");
    let missing = export(&compensated, &output, &[]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("supply the compensation matrix"));
    let wrong = export(
        &compensated,
        &output,
        &["--compensation-matrix", utf8(&other)],
    );
    assert_eq!(wrong.status.code(), Some(2));
    assert!(!output.exists(), "a refused export publishes nothing");
    let result = export(
        &compensated,
        &output,
        &["--compensation-matrix", utf8(&matrix)],
    );
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let step = &verified(&output).descriptor.steps[0];
    assert_eq!(step.origin, Origin::Verified);
    assert_eq!(
        step.definition_sha256.as_deref(),
        Some(common::sha256_hex(&matrix).as_str())
    );
    let bytes = fs::read(&output).unwrap();
    assert!(
        !bytes.windows(14).any(|w| w == b"spillover.csv\""),
        "the matrix path leaked"
    );
}

#[test]
fn externally_compensated_files_declare_their_state() {
    let directory = create_temp_dir("cytiq-events-external");
    let input = fixture("flowcyt", "Case30_P.fcs");
    let output = directory.join("case30.parquet");
    let result = export(&input, &output, &["--base-state", "compensated"]);
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let descriptor = verified(&output).descriptor;
    assert_eq!(descriptor.state, LayerState::Compensated);
    assert_eq!(descriptor.steps[0].origin, Origin::External);
}

#[test]
fn existing_tables_are_refused_without_overwrite() {
    let directory = create_temp_dir("cytiq-events-overwrite");
    let input = fixture("fcs", "fcs3_2_mixed.fcs");
    let output = directory.join("mixed.parquet");
    assert_eq!(export(&input, &output, &[]).status.code(), Some(0));
    let before = fs::read(&output).unwrap();
    assert_eq!(export(&input, &output, &[]).status.code(), Some(4));
    assert_eq!(fs::read(&output).unwrap(), before);
    assert_eq!(
        export(&input, &output, &["--overwrite"]).status.code(),
        Some(0)
    );
    assert_eq!(export(&input, &output, &[]).status.code(), Some(4));
    let reused = cytiq(&[
        "--reuse",
        "skip",
        "events",
        "export",
        "--input",
        utf8(&input),
        "--output",
        utf8(&output),
    ]);
    assert_eq!(
        reused.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&reused.stderr)
    );
}

#[test]
fn missing_inputs_and_invalid_flags_are_classified() {
    let directory = create_temp_dir("cytiq-events-errors");
    let output = directory.join("x.parquet");
    assert_eq!(
        export(&directory.join("absent.fcs"), &output, &[])
            .status
            .code(),
        Some(4)
    );
    let input = fixture("fcs", "fcs3_2_mixed.fcs");
    assert_eq!(
        export(&input, &output, &["--base-state", "normalized"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        export(&input, &output, &["--unmixing-reference", utf8(&input)])
            .status
            .code(),
        Some(2),
        "a reference for a file that is not unmixed"
    );
    assert!(!output.exists());
}
