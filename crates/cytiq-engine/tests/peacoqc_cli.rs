mod common;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow_array::{Array, BooleanArray, UInt64Array};
use cytiq_fcs::{ColumnData, FcsReader, FcsWriteOptions, FcsWriteParameter};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

const PEACOQC_FIXTURE_EVENTS: &str = "sample_events.csv";

#[test]
fn cli_runs_peacoqc_with_debug_artifacts() {
    let directory = create_temp_dir("cytiq-engine-peacoqc");
    let input = directory.join(r#"sample<&"'>.fcs"#);
    let output_path = directory.join("sample.peacoqc.fcs");
    let debug_dir = directory.join("debug");
    write_peacoqc_fixture_fcs(&input);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "peacoqc",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output_path),
            "--channel",
            "355_378/28_uH-A [CD3 BUV395]",
            "--channel",
            "405_750/40_vB-A [CD4 BV750]",
            "--channel",
            "355_515/20_uF-A [CD8 BUV496]",
            "--debug-dir",
            utf8(&debug_dir),
            "--threads",
            "1",
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(debug_dir.join("summary.json").is_file());
    assert!(debug_dir.join("mask.csv").is_file());
    let report = fs::read_to_string(debug_dir.join("report.html")).expect("report should exist");
    assert!(report.contains("plotly"));
    assert!(!report.contains(input.file_name().unwrap().to_str().unwrap()));
    assert!(report.contains("sample&#60;") || report.contains("sample&lt;"));
    assert!(
        report.contains("Removal summary")
            && report.contains("Removed bins")
            && report.contains("Event mask")
    );

    let dataset = FcsReader::open(&output_path)
        .and_then(FcsReader::read)
        .expect("cleaned FCS should be readable");
    let names = dataset
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Original_ID"));
}

#[test]
fn cli_fails_when_peacoqc_channel_is_missing() {
    let directory = create_temp_dir("cytiq-engine-peacoqc-missing-channel");
    let input = directory.join("sample.fcs");
    write_peacoqc_fixture_fcs(&input);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "peacoqc",
            "--input",
            utf8(&input),
            "--output",
            utf8(&directory.join("sample.peacoqc.fcs")),
            "--channels",
            "CD45",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CD45"));
}

#[test]
fn cli_rejects_non_fcs_peacoqc_output() {
    let directory = create_temp_dir("cytiq-engine-peacoqc-output-format");
    let input = directory.join("sample.fcs");
    write_peacoqc_fixture_fcs(&input);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "peacoqc",
            "--input",
            utf8(&input),
            "--output",
            utf8(&directory.join("sample.csv")),
            "--channels",
            "355_378/28_uH-A",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("MVP supports `.fcs`"));
}

#[test]
fn cli_peacoqc_output_preserves_source_metadata() {
    let directory = create_temp_dir("cytiq-engine-peacoqc-metadata");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.peacoqc.fcs");
    write_margin_fixture_fcs(&input);

    let output = run_peacoqc(&input, &output_path, None);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dataset = FcsReader::open(&output_path)
        .and_then(FcsReader::read)
        .expect("cleaned FCS should be readable");
    assert_eq!(dataset.text.get("$CYT"), Some("FACSymphony"));
    assert_eq!(
        dataset.text.get("$SPILLOVER"),
        Some("2,355_378/28_uH-A,405_750/40_vB-A,1,0.1,0,1")
    );
    assert_eq!(dataset.text.get("$P4R"), Some("9001"));
    assert_eq!(dataset.text.get("$P4V"), Some("520"));
    assert!(dataset.text.get("CYTIQ_MARGINS").is_some());
    common::assert_provenance(
        &directory.join("sample.peacoqc.fcs.provenance.json"),
        "cytiq peacoqc",
        &[&input],
        &[
            &output_path,
            &directory.join("sample.peacoqc.masks.parquet"),
        ],
    );
}

#[test]
fn cli_margins_on_transformed_input_match_raw_margins() {
    let directory = create_temp_dir("cytiq-engine-peacoqc-transformed-margins");
    let raw = directory.join("sample.fcs");
    let transformed = directory.join("sample.transformed.fcs");
    write_margin_fixture_fcs(&raw);
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--allow-uncompensated",
            "--input",
            utf8(&raw),
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

    let mut margin_counts = Vec::new();
    for (label, input) in [("raw", &raw), ("transformed", &transformed)] {
        let debug_dir = directory.join(format!("debug-{label}"));
        let output = run_peacoqc(
            input,
            &directory.join(format!("{label}.peacoqc.fcs")),
            Some(&debug_dir),
        );
        assert!(
            output.status.success(),
            "{label} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let summary: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(debug_dir.join("summary.json")).expect("summary should exist"),
        )
        .expect("summary should be JSON");
        margin_counts.push(summary["metrics"]["per_channel_margin_counts"].clone());
    }

    for (raw, transformed) in margin_counts[0]
        .as_array()
        .unwrap()
        .iter()
        .zip(margin_counts[1].as_array().unwrap())
    {
        for key in ["channel", "min_count", "max_count", "removed_count"] {
            assert_eq!(raw[key], transformed[key]);
        }
        // Thresholds are reported in each file's signal space, including FCS f32 rounding.
        for key in ["min_threshold", "max_threshold"] {
            #[allow(clippy::cast_possible_truncation)]
            let expected = f64::from((raw[key].as_f64().unwrap() / 150.0).asinh() as f32);
            assert_eq!(transformed[key].as_f64().unwrap(), expected);
        }
    }
    let removed = margin_counts[0]
        .as_array()
        .expect("margin counts should be a list")
        .iter()
        .map(|channel| {
            channel["min_count"].as_u64().unwrap_or_default()
                + channel["max_count"].as_u64().unwrap_or_default()
        })
        .sum::<u64>();
    assert!(removed > 0, "fixture should have margin events");
}

#[test]
fn cli_writes_exact_original_ids_above_f32_precision() {
    let directory = create_temp_dir("cytiq-engine-peacoqc-original-id");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.peacoqc.fcs");
    write_margin_fixture_fcs(&input);
    // Re-write the fixture with an upstream Original_ID column past 2^24.
    let dataset = FcsReader::open(&input)
        .and_then(FcsReader::read)
        .expect("fixture should be readable");
    let mut parameters = dataset
        .parameters
        .iter()
        .map(FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    let mut columns = dataset
        .data
        .columns
        .iter()
        .map(|column| match column {
            ColumnData::F32(values) => values.iter().copied().map(f64::from).collect::<Vec<_>>(),
            other => panic!("expected f32 fixture column, got {other:?}"),
        })
        .collect::<Vec<_>>();
    let first_id = 16_777_217.0_f64;
    parameters.push(FcsWriteParameter::new("Original_ID"));
    columns.push(
        (0..columns[0].len())
            .map(|index| first_id + f64::from(u32::try_from(index).expect("fits")))
            .collect(),
    );
    cytiq_fcs::write_fcs3_1_f64_with_options(
        &input,
        &parameters,
        &columns,
        &FcsWriteOptions::new().with_text_keywords(cytiq_fcs::carried_keywords(&dataset.text)),
    )
    .expect("fixture should write");

    let output = run_peacoqc(&input, &output_path, None);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let cleaned = FcsReader::open(&output_path)
        .and_then(FcsReader::read)
        .expect("cleaned FCS should be readable");
    let id_index = cleaned
        .parameters
        .iter()
        .position(|parameter| parameter.name == "Original_ID")
        .expect("Original_ID should be kept");
    let Some(ColumnData::F64(ids)) = cleaned.data.column(id_index) else {
        panic!("Original_ID above 2^24 should be written as f64");
    };
    assert!(ids.iter().all(|id| *id >= first_id && id.fract() == 0.0));
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));

    let masks = File::open(directory.join("sample.peacoqc.masks.parquet"))
        .expect("masks should be written next to the FCS output");
    let reader = ParquetRecordBatchReaderBuilder::try_new(masks)
        .and_then(ParquetRecordBatchReaderBuilder::build)
        .expect("masks should be Parquet");
    let mut source_ids = Vec::new();
    let mut kept = Vec::new();
    for batch in reader {
        let batch = batch.expect("batch should decode");
        let column = |name: &str| batch.column_by_name(name).expect("column should exist");
        source_ids.extend(
            column("source_event_id")
                .as_any()
                .downcast_ref::<UInt64Array>()
                .expect("source_event_id is UInt64")
                .values()
                .iter(),
        );
        kept.extend(
            column("keep")
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("keep is Boolean")
                .iter()
                .map(Option::unwrap_or_default),
        );
    }
    // The input already carried Original_ID, so source ids continue it.
    assert_eq!(source_ids.first().copied(), Some(16_777_216));
    let kept_ids = source_ids
        .iter()
        .zip(&kept)
        .filter_map(|(id, keep)| keep.then_some(*id))
        .collect::<Vec<_>>();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let written_ids = ids.iter().map(|id| *id as u64 - 1).collect::<Vec<_>>();
    assert_eq!(kept_ids, written_ids);
}

#[test]
fn cli_publishes_no_fcs_when_its_mask_cannot_be_written() {
    let directory = create_temp_dir("cytiq-engine-peacoqc-blocked-mask");
    let input = directory.join("sample.fcs");
    let output = directory.join("sample.peacoqc.fcs");
    write_peacoqc_fixture_fcs(&input);
    // A non-empty directory where the mask goes: the FCS must not appear alone.
    let masks = directory.join("sample.peacoqc.masks.parquet");
    fs::create_dir(&masks).unwrap();
    fs::write(masks.join("keep"), "keep").unwrap();

    let result = run_peacoqc(&input, &output, None);
    assert!(!result.status.success());
    assert!(!output.exists(), "the FCS was published without its mask");
    assert_eq!(fs::read_to_string(masks.join("keep")).unwrap(), "keep");
    let leftovers = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn debug_publication_failure_leaves_no_fcs_mask_or_summary() {
    let root = create_temp_dir("cytiq-peacoqc-debug-rollback");
    let input = root.join("sample.fcs");
    write_margin_fixture_fcs(&input);
    let debug = root.join("debug");
    fs::write(&debug, b"existing file").unwrap();
    let output = root.join("cleaned.fcs");
    let result = run_peacoqc(&input, &output, Some(&debug));
    assert!(!result.status.success());
    for path in [
        output.clone(),
        cytiq_peacoqc::masks_path(&output),
        cytiq_peacoqc::summary_path(&output),
    ] {
        assert!(!path.exists());
    }
    assert_eq!(fs::read(&debug).unwrap(), b"existing file");
}

fn run_peacoqc(input: &Path, output: &Path, debug_dir: Option<&Path>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cytiq"));
    command.args([
        "peacoqc",
        "--input",
        utf8(input),
        "--output",
        utf8(output),
        "--channel",
        "355_378/28_uH-A",
        "--channel",
        "405_750/40_vB-A",
        "--threads",
        "1",
    ]);
    if let Some(debug_dir) = debug_dir {
        command.args(["--remove-margins", "--debug-dir", utf8(debug_dir)]);
    }
    command.output().expect("command should run")
}

/// The `PeacoQC` fixture scaled to raw detector units, with a few CD3 events
/// saturated at `$PnR - 1` and a few CD3/CD4 events below flowCore's -111
/// floor.
fn write_margin_fixture_fcs(path: &Path) {
    write_peacoqc_fixture_fcs(path);
    let dataset = FcsReader::open(path)
        .and_then(FcsReader::read)
        .expect("fixture should be readable");
    let columns = dataset
        .data
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let ColumnData::F32(values) = column else {
                panic!("expected f32 fixture column");
            };
            let scale = if index >= 3 { 1000.0 } else { 1.0 };
            values
                .iter()
                .enumerate()
                .map(|(event, value)| match (index, event % 97, event % 89) {
                    (3, 0, _) => 9000.0,
                    (3 | 4, _, 0) => -200.0,
                    _ => (value * scale).round(),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let parameters = vec![
        FcsWriteParameter::new("Time"),
        FcsWriteParameter::new("FSC-A"),
        FcsWriteParameter::new("SSC-A"),
        FcsWriteParameter::with_label("355_378/28_uH-A", "CD3 BUV395")
            .with_range(9001.0)
            .with_keyword("V", "520"),
        FcsWriteParameter::with_label("405_750/40_vB-A", "CD4 BV750").with_range(262_144.0),
        FcsWriteParameter::with_label("355_515/20_uF-A", "CD8 BUV496").with_range(262_144.0),
    ];
    let options = FcsWriteOptions::new()
        .with_text_keyword("$CYT", "FACSymphony")
        .with_text_keyword("$SPILLOVER", "2,355_378/28_uH-A,405_750/40_vB-A,1,0.1,0,1");
    cytiq_fcs::write_fcs3_1_f32_with_options(path, &parameters, &columns, &options)
        .expect("fixture FCS should write");
}

/// Repeats of the fixture rows: `PeacoQC` needs at least `consecutive_bins`
/// (5) bins of `min_cells` events, otherwise every bin is a short region.
const PEACOQC_FIXTURE_REPEATS: u16 = 50;

#[test]
fn warns_on_untransformed_input() {
    let directory = create_temp_dir("cytiq-engine-peacoqc-untransformed");
    let input = directory.join("sample.fcs");
    let debug_dir = directory.join("debug");
    write_peacoqc_fixture_fcs(&input);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "peacoqc",
            "--input",
            utf8(&input),
            "--output",
            utf8(&directory.join("sample.peacoqc.fcs")),
            "--channel",
            "355_378/28_uH-A [CD3 BUV395]",
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
    let summary: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(debug_dir.join("summary.json")).expect("summary should exist"),
    )
    .expect("summary should be JSON");
    let warnings = summary["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("were not transformed"))),
        "{warnings:?}"
    );
}

fn write_peacoqc_fixture_fcs(path: &Path) {
    let base = read_peacoqc_fixture_columns();
    let rows = f32::from(u16::try_from(base[0].len()).expect("fixture rows fit in u16"));
    let columns = base
        .iter()
        .enumerate()
        .map(|(column_index, column)| {
            (0..PEACOQC_FIXTURE_REPEATS)
                .flat_map(|repeat| {
                    column.iter().map(move |value| {
                        // keep acquisition time increasing across repeats
                        if column_index == 0 {
                            value + f32::from(repeat) * rows
                        } else {
                            *value
                        }
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let parameters = vec![
        FcsWriteParameter::new("Time"),
        FcsWriteParameter::new("FSC-A"),
        FcsWriteParameter::new("SSC-A"),
        FcsWriteParameter::with_label("355_378/28_uH-A", "CD3 BUV395"),
        FcsWriteParameter::with_label("405_750/40_vB-A", "CD4 BV750"),
        FcsWriteParameter::with_label("355_515/20_uF-A", "CD8 BUV496"),
    ];
    cytiq_fcs::write_fcs3_1_f32(path, &parameters, &columns).expect("fixture FCS should write");
}

fn read_peacoqc_fixture_columns() -> Vec<Vec<f32>> {
    let path = peacoqc_fixture_dir().join(PEACOQC_FIXTURE_EVENTS);
    let mut reader = csv::Reader::from_path(&path).expect("PeacoQC fixture CSV should open");
    let header_len = reader
        .headers()
        .expect("PeacoQC fixture header should parse")
        .len();
    let mut columns = vec![Vec::new(); header_len];
    for record in reader.records() {
        let record = record.expect("PeacoQC fixture row should parse");
        assert_eq!(record.len(), columns.len());
        for (index, column) in columns.iter_mut().enumerate() {
            column.push(
                record[index]
                    .parse::<f32>()
                    .expect("PeacoQC fixture value should be numeric"),
            );
        }
    }
    columns
}

fn peacoqc_fixture_dir() -> PathBuf {
    fixture_dir("peacoqc")
}

fn fixture_dir(name: &str) -> PathBuf {
    for start in fixture_search_roots() {
        for candidate in start
            .ancestors()
            .map(|ancestor| ancestor.join("data").join("fixtures").join(name))
        {
            if candidate.is_dir() {
                return candidate;
            }
        }
    }

    panic!("fixture directory should exist: data/fixtures/{name}");
}

fn fixture_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(current_dir) = std::env::current_dir() {
        roots.push(current_dir);
    }

    if let Ok(current_exe) = std::env::current_exe() {
        roots.push(current_exe);
    }

    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    roots
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

/// The cleaned FCS would declare `$PnE=0,0` over log channel numbers.
#[test]
fn cli_rejects_log_amplified_peacoqc_input() {
    let directory = create_temp_dir("cytiq-engine-peacoqc-log");
    let input = directory.join("sample.fcs");
    let output_path = directory.join("sample.peacoqc.fcs");
    write_peacoqc_fixture_fcs(&input);
    common::set_log_amplified(&input, "355_378/28_uH-A");

    let output = run_peacoqc(&input, &output_path, None);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`355_378/28_uH-A` store log-amplified values"),
        "stderr: {stderr}"
    );
    assert!(!output_path.exists());
}
