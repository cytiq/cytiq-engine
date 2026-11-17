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

fn export(input: &Path, output: &Path, layer: &str, extra: &[&str]) -> Output {
    let mut args = vec![
        "export",
        "events",
        "--input",
        utf8(input),
        "--output",
        utf8(output),
        "--layer",
        layer,
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
    let result = export(&input, &output, "raw", &[]);
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    common::assert_provenance(
        &cytiq_provenance::record_path_for_file(&output),
        "cytiq export events",
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
    let files = [
        (&input, "raw"),
        (&compensated, "compensated"),
        (&transformed, "transformed"),
    ];
    for (index, (file, layer)) in files.into_iter().enumerate() {
        let output = directory.join(format!("layer{index}.parquet"));
        let result = export(file, &output, layer, &[]);
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
    let missing = export(&compensated, &output, "compensated", &[]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("supply the compensation matrix"));
    let wrong = export(
        &compensated,
        &output,
        "compensated",
        &["--compensation-matrix", utf8(&other)],
    );
    assert_eq!(wrong.status.code(), Some(2));
    assert!(!output.exists(), "a refused export publishes nothing");
    let result = export(
        &compensated,
        &output,
        "compensated",
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
    let result = export(
        &input,
        &output,
        "compensated",
        &["--base-state", "compensated"],
    );
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
    assert_eq!(export(&input, &output, "raw", &[]).status.code(), Some(0));
    let before = fs::read(&output).unwrap();
    assert_eq!(export(&input, &output, "raw", &[]).status.code(), Some(4));
    assert_eq!(fs::read(&output).unwrap(), before);
    assert_eq!(
        export(&input, &output, "raw", &["--overwrite"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        fs::read(&output).unwrap(),
        before,
        "exports are deterministic"
    );
    assert_eq!(export(&input, &output, "raw", &[]).status.code(), Some(4));
    let reused = cytiq(&[
        "--reuse",
        "skip",
        "export",
        "events",
        "--input",
        utf8(&input),
        "--output",
        utf8(&output),
        "--layer",
        "raw",
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
        export(&directory.join("absent.fcs"), &output, "raw", &[])
            .status
            .code(),
        Some(4)
    );
    let input = fixture("fcs", "fcs3_2_mixed.fcs");
    assert_eq!(
        export(&input, &output, "raw", &["--base-state", "normalized"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        export(
            &input,
            &output,
            "raw",
            &["--unmixing-reference", utf8(&input)]
        )
        .status
        .code(),
        Some(2),
        "a reference for a file that is not unmixed"
    );
    assert!(!output.exists());
}

/// Every entry of `directory`, by name.
fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_file_holding_another_layer_is_refused() {
    let directory = create_temp_dir("cytiq-events-layer");
    let input = fixture("flowcyt", "Case12_P.fcs");
    let compensated = directory.join("case12.comp.fcs");
    succeed(&[
        "compensate",
        "--input",
        utf8(&input),
        "--output",
        utf8(&compensated),
    ]);
    let output = directory.join("layer.parquet");
    let result = export(&compensated, &output, "raw", &[]);
    assert_eq!(result.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("holds a compensated layer, not a raw"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.exists());
    let missing = cytiq(&[
        "export",
        "events",
        "--input",
        utf8(&input),
        "--output",
        utf8(&output),
    ]);
    assert_eq!(missing.status.code(), Some(2), "`--layer` is required");
}

#[test]
fn malformed_and_truncated_files_publish_nothing() {
    let directory = create_temp_dir("cytiq-events-malformed");
    let garbage = directory.join("garbage.fcs");
    fs::write(&garbage, b"FCS3.1    not an FCS file at all").unwrap();
    let source = fs::read(fixture("flowcyt", "Case12_P.fcs")).unwrap();
    // Cut inside DATA, whose last byte the HEADER records at bytes 34..42.
    let data_end: usize = std::str::from_utf8(&source[34..42])
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let truncated = directory.join("truncated.fcs");
    fs::write(&truncated, &source[..data_end - 10]).unwrap();
    for input in [&garbage, &truncated] {
        let output = directory.join("events.parquet");
        let result = export(input, &output, "raw", &[]);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let parts = directory.join("parts");
        let split = export(input, &parts, "raw", &["--max-file-bytes", "1KiB"]);
        assert_eq!(split.status.code(), Some(2));
    }
    assert_eq!(
        entries(&directory),
        ["garbage.fcs", "truncated.fcs"],
        "no table, part, record or staged file is left behind"
    );
}

#[test]
fn split_layers_publish_parts_with_their_checksums() {
    let directory = create_temp_dir("cytiq-events-parts");
    let input = fixture("flowcyt", "Case30_P.fcs");
    let single = directory.join("single.parquet");
    let parts = directory.join("parts");
    let base = ["--base-state", "compensated"];
    assert_eq!(
        export(&input, &single, "compensated", &base).status.code(),
        Some(0)
    );
    // 149 events of 8-byte ids and float32 channels; ~50 events a part.
    let whole = verified(&single);
    let row_bytes = 8 + 4 * whole.channels.len();
    let max = (50 * row_bytes).to_string();
    let result = export(
        &input,
        &parts,
        "compensated",
        &[
            "--base-state",
            "compensated",
            "--max-file-bytes",
            &max,
            "--compression",
            "snappy",
            "--row-group-size",
            "16",
        ],
    );
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        entries(&parts),
        [
            "part-00000.parquet",
            "part-00001.parquet",
            "part-00002.parquet",
            "parts.json",
            "provenance.json",
        ]
    );
    let mut issues = Issues::new();
    let layer = events::verify_parts(&parts.join(events::PARTS_FILE), "", &mut issues)
        .unwrap_or_else(|| panic!("{:?}", issues.items()));
    assert_eq!(layer.layer_id, whole.layer_id);
    assert_eq!(layer.values_sha256, whole.values_sha256);
    assert_eq!((layer.rows, layer.raw_events), (149, Some(149)));
    let outputs: Vec<PathBuf> = ["parts.json", "part-00000.parquet", "part-00001.parquet"]
        .iter()
        .map(|name| parts.join(name))
        .chain(std::iter::once(parts.join("part-00002.parquet")))
        .collect();
    let outputs: Vec<&Path> = outputs.iter().map(PathBuf::as_path).collect();
    common::assert_provenance(
        &parts.join("provenance.json"),
        "cytiq export events",
        &[&input],
        &outputs,
    );
    // A parts directory is refused like a table, and replaced as a whole.
    let again = export(&input, &parts, "compensated", &base);
    assert_eq!(again.status.code(), Some(4));
    let replaced = export(
        &input,
        &parts,
        "compensated",
        &[
            "--base-state",
            "compensated",
            "--max-file-bytes",
            "1MiB",
            "--overwrite",
        ],
    );
    assert_eq!(replaced.status.code(), Some(0));
    assert_eq!(
        entries(&parts),
        ["part-00000.parquet", "parts.json", "provenance.json"]
    );
}

#[test]
fn selected_channels_are_stored_in_pn_order() {
    let directory = create_temp_dir("cytiq-events-columns");
    let input = fixture("fcs", "fcs3_2_mixed.fcs");
    let full = directory.join("full.parquet");
    assert_eq!(export(&input, &full, "raw", &[]).status.code(), Some(0));
    let all = verified(&full);
    let names: Vec<&str> = all.channels.iter().map(|c| c.name.as_str()).collect();
    let selected = directory.join("selected.parquet");
    let result = export(
        &input,
        &selected,
        "raw",
        &["--channel", names[2], "--channels", names[0]],
    );
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let projection = verified(&selected);
    let stored: Vec<&str> = projection
        .channels
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(stored, [names[0], names[2]]);
    assert!(!projection.all_columns);
    assert_eq!(
        projection.layer_id, all.layer_id,
        "a projection keeps its layer"
    );
    let unknown = export(
        &input,
        &directory.join("x.parquet"),
        "raw",
        &["--channel", "nope"],
    );
    assert_eq!(unknown.status.code(), Some(2));
}

#[test]
fn write_settings_are_checked_before_any_work() {
    let directory = create_temp_dir("cytiq-events-settings");
    let input = fixture("fcs", "fcs3_2_mixed.fcs");
    let output = directory.join("x.parquet");
    for extra in [
        &["--compression", "snappy", "--compression-level", "3"][..],
        &["--compression-level", "0"],
        &["--compression-level", "23"],
        &["--row-group-size", "0"],
        &["--batch-size", "0"],
        &["--batch-size", "100000000"],
        &["--max-file-bytes", "0"],
        &["--max-file-bytes", "12XB"],
        &["--max-file-bytes", "4"],
    ] {
        let result = export(&input, &output, "raw", extra);
        assert_eq!(result.status.code(), Some(2), "{extra:?}");
    }
    assert_eq!(entries(&directory), Vec::<String>::new());
    for extra in [
        &["--compression", "snappy"][..],
        &["--compression-level", "9", "--batch-size", "1"],
    ] {
        let output = directory.join("ok.parquet");
        let result = export(&input, &output, "raw", &[&["--overwrite"], extra].concat());
        assert_eq!(
            result.status.code(),
            Some(0),
            "{extra:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        verified(&output);
    }
}
