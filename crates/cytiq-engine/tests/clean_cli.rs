mod common;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow_array::{Array, BooleanArray, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::Value;

const LIVE_SINGLET: u8 = b'0';
const DEBRIS: u8 = b'1';
const DOUBLET: u8 = b'2';
const DEAD: u8 = b'3';

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

fn run(args: &[&str]) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn run_clean(input: &str, output: &Path, extra: &[&str]) {
    let input = fixture(input);
    let mut args = vec![
        "clean",
        "--input",
        utf8(&input),
        "--output",
        utf8(output),
        "--threads",
        "2",
    ];
    args.extend_from_slice(extra);
    run(&args);
}

fn metrics(output: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(output.join("metrics.json")).unwrap()).unwrap()
}

struct Masks {
    event_id: Vec<u64>,
    source_event_id: Vec<u64>,
    columns: Vec<(String, Vec<Option<bool>>)>,
}

impl Masks {
    fn read(output: &Path) -> Self {
        let file = File::open(output.join("masks.parquet")).expect("masks should exist");
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let mut masks = Self {
            event_id: Vec::new(),
            source_event_id: Vec::new(),
            columns: Vec::new(),
        };
        for batch in reader {
            let batch = batch.unwrap();
            let schema = batch.schema();
            for (field, column) in schema.fields().iter().zip(batch.columns()) {
                if let Some(target) = match field.name().as_str() {
                    "event_id" => Some(&mut masks.event_id),
                    "source_event_id" => Some(&mut masks.source_event_id),
                    _ => None,
                } {
                    let ids = column.as_any().downcast_ref::<UInt64Array>().unwrap();
                    target.extend(ids.values().iter());
                    continue;
                }
                let values = column.as_any().downcast_ref::<BooleanArray>().unwrap();
                let position = masks
                    .columns
                    .iter()
                    .position(|(name, _)| name == field.name())
                    .unwrap_or_else(|| {
                        masks.columns.push((field.name().clone(), Vec::new()));
                        masks.columns.len() - 1
                    });
                masks.columns[position].1.extend(values.iter());
            }
        }
        masks
    }

    fn column(&self, name: &str) -> &[Option<bool>] {
        &self
            .columns
            .iter()
            .find(|(column, _)| column == name)
            .unwrap_or_else(|| panic!("missing column {name}"))
            .1
    }
}

fn truth_classes() -> Vec<u8> {
    let truth: Value =
        serde_json::from_str(&fs::read_to_string(fixture("truth.json")).unwrap()).unwrap();
    truth["classes"].as_str().unwrap().as_bytes().to_vec()
}

/// Share of events of class `positive` (expected to fail the filter) and of
/// live singlets (expected to pass) that the filter classified correctly.
#[allow(clippy::cast_precision_loss)]
fn accuracy(mask: &[Option<bool>], classes: &[u8], removed_class: u8) -> f64 {
    let (correct, total) = mask
        .iter()
        .zip(classes)
        .filter(|(_, class)| **class == LIVE_SINGLET || **class == removed_class)
        .fold((0, 0), |(correct, total), (passed, class)| {
            let expected = *class == LIVE_SINGLET;
            (correct + usize::from(*passed == Some(expected)), total + 1)
        });
    correct as f64 / f64::from(total)
}

#[test]
fn cli_writes_clean_outputs() {
    let output = create_temp_dir("cytiq-engine-clean-outputs");

    run_clean("sample.fcs", &output, &[]);

    for file in [
        "masks.parquet",
        "metrics.json",
        "gates.gatingml.xml",
        "report.html",
    ] {
        assert!(output.join(file).is_file(), "{file} should be written");
    }
    let masks = Masks::read(&output);
    assert_eq!(masks.event_id, (0..10_000).collect::<Vec<u64>>());
    assert_eq!(masks.source_event_id, masks.event_id);
    assert_eq!(
        masks
            .columns
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["non_debris", "singlets", "live", "keep"]
    );
    let metrics = metrics(&output);
    let kept = masks
        .column("keep")
        .iter()
        .filter(|keep| **keep == Some(true))
        .count();
    assert_eq!(metrics["kept"].as_u64().unwrap(), kept as u64);
    assert_eq!(metrics["channels"]["viability"], "FL3-A");
    assert_eq!(metrics["channels"]["viability_source"], "detected");
    for filter in ["debris", "singlets", "viability"] {
        assert_eq!(metrics["filters"][filter]["status"], "applied", "{filter}");
    }
}

#[test]
fn emits_source_event_ids_from_original_id() {
    let directory = create_temp_dir("cytiq-engine-clean-original-id");
    let input = directory.join("sample.fcs");
    let output = directory.join("clean");
    // The fixture as if an earlier stage had kept every other raw event.
    let dataset = cytiq_fcs::FcsReader::open(fixture("sample.fcs"))
        .and_then(cytiq_fcs::FcsReader::read)
        .expect("fixture should be readable");
    let mut parameters = dataset
        .parameters
        .iter()
        .map(cytiq_fcs::FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    let mut columns = dataset
        .data
        .columns
        .iter()
        .map(|column| match column {
            cytiq_fcs::ColumnData::F32(values) => values.clone(),
            other => panic!("expected f32 fixture column, got {other:?}"),
        })
        .collect::<Vec<_>>();
    parameters.push(cytiq_fcs::FcsWriteParameter::new("Original_ID"));
    columns.push(
        (0..10_000_u16)
            .map(|row| f32::from(row) * 2.0 + 1.0)
            .collect(),
    );
    cytiq_fcs::write_fcs3_1_f32_with_options(
        &input,
        &parameters,
        &columns,
        &cytiq_fcs::FcsWriteOptions::new()
            .with_text_keywords(cytiq_fcs::carried_keywords(&dataset.text)),
    )
    .expect("fixture should write");

    run(&["clean", "--input", utf8(&input), "--output", utf8(&output)]);

    let masks = Masks::read(&output);
    assert_eq!(masks.event_id, (0..10_000).collect::<Vec<u64>>());
    assert_eq!(
        masks.source_event_id,
        (0..10_000).map(|row| row * 2).collect::<Vec<u64>>()
    );
    assert!(metrics(&output)["source_event_id"].is_string());
}

#[test]
fn recovers_known_debris_doublets_and_dead_cells() {
    let output = create_temp_dir("cytiq-engine-clean-truth");

    run_clean("sample.fcs", &output, &[]);

    let classes = truth_classes();
    let masks = Masks::read(&output);
    for (column, class) in [
        ("non_debris", DEBRIS),
        ("singlets", DOUBLET),
        ("live", DEAD),
    ] {
        let score = accuracy(masks.column(column), &classes, class);
        assert!(score > 0.98, "{column} accuracy {score}");
    }
    let keep = masks.column("keep");
    let kept_live = keep
        .iter()
        .zip(&classes)
        .filter(|(keep, class)| **keep == Some(true) && **class == LIVE_SINGLET)
        .count();
    let kept = keep.iter().filter(|keep| **keep == Some(true)).count();
    let live = classes
        .iter()
        .zip(keep)
        .filter(|(class, _)| **class == LIVE_SINGLET)
        .count();
    assert!(kept_live * 100 > live * 98, "kept {kept_live} of {live}");
    assert!(
        kept_live * 100 > kept * 98,
        "{kept_live} of {kept} kept are live"
    );
}

#[test]
fn marks_viability_not_assessed_without_dye() {
    let output = create_temp_dir("cytiq-engine-clean-no-viability");

    run_clean("no_viability.fcs", &output, &[]);

    let masks = Masks::read(&output);
    assert!(masks.column("live").iter().all(Option::is_none));
    assert!(masks.column("non_debris").iter().all(Option::is_some));
    let metrics = metrics(&output);
    let viability = &metrics["filters"]["viability"];
    assert_eq!(viability["status"], "not_assessed");
    assert!(
        viability["reason"]
            .as_str()
            .unwrap()
            .contains("no viability channel")
    );
    assert!(viability["removed"].is_null());
    assert_eq!(metrics["keep_rule"], "gate `clean`");
    let gates = fs::read_to_string(output.join("gates.gatingml.xml")).unwrap();
    assert!(!gates.contains("gating:id=\"live\""));
}

#[test]
fn reports_viability_disabled_on_request() {
    let output = create_temp_dir("cytiq-engine-clean-disabled");

    run_clean("sample.fcs", &output, &["--no-viability"]);

    assert_eq!(
        metrics(&output)["filters"]["viability"]["status"],
        "disabled"
    );
}

#[test]
fn reports_debris_disabled_with_debris_mode_off() {
    let output = create_temp_dir("cytiq-engine-clean-debris-off");

    run_clean("sample.fcs", &output, &["--debris-mode", "off"]);

    let masks = Masks::read(&output);
    assert!(masks.column("non_debris").iter().all(Option::is_none));
    assert!(masks.column("singlets").iter().all(Option::is_some));
    let metrics = metrics(&output);
    let debris = &metrics["filters"]["debris"];
    assert_eq!(debris["status"], "disabled");
    assert_eq!(debris["reason"], "disabled with --debris-mode off");
    assert!(debris["removed"].is_null());
    assert_eq!(metrics["filters"]["singlets"]["status"], "applied");
    let gates = fs::read_to_string(output.join("gates.gatingml.xml")).unwrap();
    assert!(!gates.contains("gating:id=\"non_debris\""));
}

#[test]
fn rejects_debris_mode_off_with_required_debris_or_gates() {
    let directory = create_temp_dir("cytiq-engine-clean-debris-off-conflicts");
    let input = fixture("sample.fcs");
    let gates = directory.join("gates.gatingml.xml");
    fs::write(&gates, "").unwrap();

    for (extra, expected) in [
        (
            ["--require", "debris"],
            "--require debris and --debris-mode off",
        ),
        (["--gates", utf8(&gates)], "--debris-mode off and --gates"),
    ] {
        let output = directory.join(extra[0].trim_start_matches('-'));
        let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
            .args([
                "clean",
                "--input",
                utf8(&input),
                "--output",
                utf8(&output),
                "--debris-mode",
                "off",
            ])
            .args(extra)
            .output()
            .expect("command should run");

        assert!(!result.status.success(), "{extra:?} should be rejected");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains(expected), "stderr: {stderr}");
        assert!(!output.join("masks.parquet").exists());
    }
}

#[test]
fn exported_gates_reproduce_masks() {
    let output = create_temp_dir("cytiq-engine-clean-gates");
    run_clean("sample.fcs", &output, &[]);
    let counts = output.join("counts.csv");
    let sample = fixture("sample.fcs");
    let gates = output.join("gates.gatingml.xml");

    run(&[
        "gate",
        "--input",
        utf8(&sample),
        "--gating-ml",
        utf8(&gates),
        "--output",
        utf8(&counts),
    ]);

    let metrics = metrics(&output);
    let mut reader = csv::Reader::from_path(&counts).unwrap();
    let rows = reader
        .records()
        .map(|record| {
            let record = record.unwrap();
            (record[0].to_owned(), record[4].parse::<u64>().unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            (
                "non_debris".to_owned(),
                metrics["filters"]["debris"]["passed"].as_u64().unwrap()
            ),
            (
                "singlets".to_owned(),
                metrics["filters"]["singlets"]["passed"].as_u64().unwrap()
            ),
            (
                "live".to_owned(),
                metrics["filters"]["viability"]["passed"].as_u64().unwrap()
            ),
            ("clean".to_owned(), metrics["kept"].as_u64().unwrap()),
        ]
    );
}

#[test]
fn reapplies_edited_gates() {
    let fitted = create_temp_dir("cytiq-engine-clean-fitted");
    run_clean("sample.fcs", &fitted, &[]);
    let fitted_metrics = metrics(&fitted);
    // Hand-edit: move the live/dead threshold to the very top of the scale,
    // so every event counts as live.
    let threshold = fitted_metrics["filters"]["viability"]["parameters"]["threshold"]
        .as_f64()
        .unwrap();
    let xml = fs::read_to_string(fitted.join("gates.gatingml.xml"))
        .unwrap()
        .replace(
            &format!("gating:max=\"{threshold}\""),
            "gating:max=\"1000\"",
        );
    let edited = fitted.join("edited.xml");
    fs::write(&edited, xml).unwrap();
    let reapplied = create_temp_dir("cytiq-engine-clean-reapplied");

    run_clean("sample.fcs", &reapplied, &["--gates", utf8(&edited)]);

    let metrics = metrics(&reapplied);
    assert_eq!(metrics["gates_source"], utf8(&edited));
    assert_eq!(metrics["filters"]["viability"]["removed"], 0);
    assert_eq!(
        metrics["filters"]["debris"]["passed"],
        fitted_metrics["filters"]["debris"]["passed"]
    );
    assert!(metrics["kept"].as_u64() > fitted_metrics["kept"].as_u64());
    assert_eq!(
        fs::read_to_string(reapplied.join("gates.gatingml.xml")).unwrap(),
        fs::read_to_string(&edited).unwrap()
    );
}

#[test]
fn uses_viability_control_threshold() {
    let output = create_temp_dir("cytiq-engine-clean-control");
    let control = fixture("control.fcs");

    run_clean(
        "sample.fcs",
        &output,
        &["--viability-control", utf8(&control)],
    );

    let viability = &metrics(&output)["filters"]["viability"];
    assert_eq!(viability["method"], "control_quantile");
    // Unstained events are ~N(300, 250): p99.5 lands near 950.
    let threshold = viability["parameters"]["threshold_raw"].as_f64().unwrap();
    assert!((700.0..1_300.0).contains(&threshold), "{threshold}");
    let classes = truth_classes();
    let score = accuracy(Masks::read(&output).column("live"), &classes, DEAD);
    assert!(score > 0.98, "live accuracy {score}");
}

#[test]
fn falls_back_to_fsc_width_without_fsc_height() {
    let output = create_temp_dir("cytiq-engine-clean-width");

    run_clean("sample.fcs", &output, &["--fsc-h", "missing"]);

    let singlets = &metrics(&output)["filters"]["singlets"];
    assert_eq!(singlets["method"], "fsc_width");
    assert_eq!(singlets["channels"][0], "FSC-W");
}

#[test]
fn fails_when_fixed_debris_has_no_thresholds() {
    let output = create_temp_dir("cytiq-engine-clean-fixed");
    let input = fixture("sample.fcs");

    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "clean",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output),
            "--debris-mode",
            "fixed",
        ])
        .output()
        .expect("command should run");

    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--debris-fsc-min"));
}

#[test]
fn restricts_keep_and_fitting_to_the_input_mask() {
    let directory = create_temp_dir("cytiq-engine-clean-mask");
    let output = directory.join("clean");
    let mask_path = directory.join("mask.parquet");
    let input_mask = (0..10_000).map(|row| row % 2 == 0).collect::<Vec<_>>();
    cytiq_mask::EventMask::new(
        common::acquisition_of(&fixture("sample.fcs")),
        input_mask.clone(),
        None,
    )
    .write_parquet(&mask_path)
    .expect("mask should write");

    run_clean("sample.fcs", &output, &["--mask", utf8(&mask_path)]);

    let masks = Masks::read(&output);
    let keep = masks.column("keep");
    assert!(
        keep.iter()
            .zip(&input_mask)
            .all(|(keep, kept)| *kept || *keep == Some(false)),
        "events outside the input mask must not be kept"
    );
    assert!(keep.contains(&Some(true)));
    // Filters are still evaluated on every event.
    assert!(masks.column("singlets").iter().all(Option::is_some));
    let metrics = metrics(&output);
    assert_eq!(metrics["input_mask"]["kept"], 5_000);
    assert_eq!(
        metrics["input_mask"]["paths"][0],
        mask_path.display().to_string()
    );
}

/// 55% of the events have FSC-A exactly equal to FSC-H, as with heavily
/// discretised scatter, so the MAD of the area/height ratio is zero.
fn write_zero_mad_fixture(path: &Path) -> Vec<f64> {
    let mut ratios = Vec::new();
    for event in 0..1_000_u16 {
        let ratio = match event % 20 {
            0..=10 => 1.0,
            11..=17 => 0.9 + f64::from(event % 7) * 0.03,
            _ => 1.9,
        };
        ratios.push(ratio);
    }
    let height = (0..1_000_u16)
        .map(|event| 20_000.0 + f32::from(event % 50) * 400.0)
        .collect::<Vec<f32>>();
    #[allow(clippy::cast_possible_truncation)]
    let area = height
        .iter()
        .zip(&ratios)
        .map(|(height, ratio)| (f64::from(*height) * ratio) as f32)
        .collect::<Vec<f32>>();
    let ssc = (0..1_000_u16)
        .map(|event| 10_000.0 + f32::from(event % 13) * 100.0)
        .collect::<Vec<f32>>();
    let parameters = ["FSC-A", "FSC-H", "SSC-A"].map(cytiq_fcs::FcsWriteParameter::new);
    cytiq_fcs::write_fcs3_1_f32(path, &parameters, &[area, height, ssc])
        .expect("fixture should write");
    ratios
}

#[test]
fn singlets_survive_a_zero_mad() {
    let directory = create_temp_dir("cytiq-engine-clean-zero-mad");
    let input = directory.join("discrete.fcs");
    let output = directory.join("clean");
    let ratios = write_zero_mad_fixture(&input);

    run(&[
        "clean",
        "--input",
        utf8(&input),
        "--output",
        utf8(&output),
        "--no-viability",
    ]);

    let metrics = metrics(&output);
    assert_eq!(metrics["filters"]["singlets"]["status"], "applied");
    assert_eq!(
        metrics["filters"]["singlets"]["parameters"]["spread_source"],
        "iqr"
    );
    let masks = Masks::read(&output);
    for (singlet, ratio) in masks.column("singlets").iter().zip(&ratios) {
        if (*ratio - 1.0).abs() < f64::EPSILON {
            assert_eq!(*singlet, Some(true), "ratio-1 events are singlets");
        } else if *ratio > 1.5 {
            assert_eq!(*singlet, Some(false), "doublets are removed");
        }
    }
}

#[test]
fn require_blocks_acceptance_and_preserves_diagnostic_outputs() {
    let directory = create_temp_dir("cytiq-engine-clean-require");
    let output = directory.join("clean");
    let input = fixture("no_viability.fcs");

    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "clean",
            "--input",
            utf8(&input),
            "--output",
            utf8(&output),
            "--require",
            "singlets,viability",
        ])
        .output()
        .expect("command should run");

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("--require viability") && stderr.contains("not_assessed"),
        "stderr: {stderr}"
    );
    assert_eq!(result.status.code(), Some(3));
    for file in [
        "masks.parquet",
        "metrics.json",
        "gates.gatingml.xml",
        "report.html",
        "provenance.json",
    ] {
        assert!(output.join(file).exists(), "missing diagnostic {file}");
    }
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("provenance.json")).unwrap()).unwrap();
    assert_eq!(record["reason"], "checks_failed");
}

#[test]
fn rejects_a_transformed_viability_channel_early() {
    let directory = create_temp_dir("cytiq-engine-clean-transformed");
    let transformed = directory.join("sample.t.fcs");
    run(&[
        "transform",
        "--allow-uncompensated",
        "--input",
        utf8(&fixture("sample.fcs")),
        "--output",
        utf8(&transformed),
    ]);

    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "clean",
            "--input",
            utf8(&transformed),
            "--output",
            utf8(&directory.join("clean")),
        ])
        .output()
        .expect("command should run");

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("mask combine"), "stderr: {stderr}");
    run(&[
        "clean",
        "--input",
        utf8(&transformed),
        "--output",
        utf8(&directory.join("clean-no-viability")),
        "--no-viability",
    ]);
}

#[test]
fn fails_when_mask_belongs_to_another_file() {
    let directory = create_temp_dir("cytiq-engine-clean-foreign-mask");
    let output = directory.join("clean");
    let mask_path = directory.join("mask.parquet");
    // The viability control has its own acquisition.
    cytiq_mask::EventMask::new(
        common::acquisition_of(&fixture("control.fcs")),
        vec![true; 10_000],
        None,
    )
    .write_parquet(&mask_path)
    .expect("mask should write");

    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "clean",
            "--input",
            utf8(&fixture("sample.fcs")),
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
    assert!(!output.join("masks.parquet").exists());
}

#[test]
fn rejects_transformed_scatter() {
    let directory = create_temp_dir("cytiq-engine-clean-transformed-scatter");
    let input = directory.join("sample.fcs");
    fs::copy(fixture("sample.fcs"), &input).expect("fixture should copy");
    common::add_keyword(
        &input,
        cytiq_fcs::TRANSFORM_KEYWORD,
        &common::transform_keyword(&["FSC-A"]),
    );

    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "clean",
            "--input",
            utf8(&input),
            "--output",
            utf8(&directory.join("clean")),
        ])
        .output()
        .expect("command should run");

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("scatter channel `FSC-A`"), "{stderr}");
}

#[test]
fn report_escapes_paths_and_preserves_generated_plot_panels() {
    let directory = create_temp_dir("cytiq-clean-html");
    let input = directory.join(r#"sample<&"'>.fcs"#);
    fs::copy(fixture("sample.fcs"), &input).unwrap();
    let output = directory.join("result");
    run(&["clean", "--input", utf8(&input), "--output", utf8(&output)]);
    let report = fs::read_to_string(output.join("report.html")).unwrap();
    assert!(!report.contains(input.file_name().unwrap().to_str().unwrap()));
    assert!(report.contains("sample&#60;") || report.contains("sample&lt;"));
    assert!(report.contains("Filters") && report.contains("Removed events by filter combination"));
    assert!(report.contains("plotly") && report.contains("class=\"plot-panel\""));
}
