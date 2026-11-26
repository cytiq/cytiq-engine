mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use cytiq_fcs::{ColumnData, FcsDataset, FcsReader};
use cytiq_mask::EventMask;

const SPILLOVER: &str = "3,FL1-A,FL2-A,FL3-A,1,0.12,0.03,0.05,1,0.18,0.01,0.07,1";

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

fn cytiq(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run")
}

fn run(args: &[&str]) -> std::process::Output {
    let output = cytiq(args);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn read(path: &Path) -> FcsDataset {
    FcsReader::open(path)
        .and_then(FcsReader::read)
        .expect("FCS should be readable")
}

fn f32_column(dataset: &FcsDataset, name: &str) -> Vec<f32> {
    let index = dataset.find_parameter(name).expect("channel should exist");
    match dataset.data.column(index) {
        Some(ColumnData::F32(values)) => values.clone(),
        other => panic!("expected an f32 column for {name}, got {other:?}"),
    }
}

fn compensate(input: &Path, output: &Path, extra: &[&str]) {
    let mut args = vec![
        "compensate",
        "--input",
        utf8(input),
        "--output",
        utf8(output),
    ];
    args.extend_from_slice(extra);
    run(&args);
}

#[test]
fn compensates_with_the_file_spillover() {
    let directory = create_temp_dir("cytiq-engine-compensate-fcs");
    let input = fixture("gating", "sample.fcs");
    let output = directory.join("sample.comp.fcs");

    compensate(&input, &output, &[]);

    let raw = read(&input);
    let compensated = read(&output);
    let channels = ["FL1-A", "FL2-A", "FL3-A"];
    let linear = channels
        .iter()
        .map(|name| cytiq_fcs::linear_values(&raw, raw.find_parameter(name).unwrap()))
        .collect::<Vec<_>>();
    let spillover = cytiq_fcs::Spillover::from_dataset(&raw).unwrap().unwrap();
    let expected = cytiq_fcs::compensate(&linear, &spillover.compensation_matrix().unwrap());
    for (name, expected) in channels.iter().zip(expected) {
        let actual = f32_column(&compensated, name);
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (f64::from(*actual) - expected).abs() <= expected.abs() * 1.0e-6 + 1.0e-6,
                "{name}: {actual} vs {expected}"
            );
        }
    }
    assert_eq!(f32_column(&compensated, "FSC-A"), f32_column(&raw, "FSC-A"));

    assert_eq!(compensated.text.get("$SPILLOVER"), None);
    assert_eq!(compensated.text.get("CYTIQ_SOURCE_SPILL"), Some(SPILLOVER));
    let state = cytiq_fcs::compensation_state(&compensated.text)
        .unwrap()
        .expect("compensation should be recorded");
    assert_eq!(state.source, "fcs");
    assert_eq!(state.channels, channels);

    let mask = EventMask::read_parquet(&directory.join("sample.comp.masks.parquet"))
        .expect("raw margin mask should be written");
    assert_eq!(mask.len(), raw.event_count());
    assert_eq!(mask.filters[0].0, "margin_pass");
    common::assert_provenance(
        &directory.join("sample.comp.fcs.provenance.json"),
        "cytiq compensate",
        &[&input],
        &[&output, &directory.join("sample.comp.masks.parquet")],
    );
}

#[test]
fn matrix_files_match_the_file_spillover() {
    let directory = create_temp_dir("cytiq-engine-compensate-matrix");
    let input = fixture("gating", "sample.fcs");
    let raw = read(&input);
    let spillover = cytiq_fcs::Spillover::from_dataset(&raw).unwrap().unwrap();
    let channels = spillover.channels();
    let write_matrix = |name: &str, values: &[f64]| {
        let mut text = format!("channel,{}\n", channels.join(","));
        for (row, channel) in channels.iter().enumerate() {
            let cells = values[row * channels.len()..(row + 1) * channels.len()]
                .iter()
                .map(|value| format!("{value:.17}"))
                .collect::<Vec<_>>();
            text.push_str(channel);
            text.push(',');
            text.push_str(&cells.join(","));
            text.push('\n');
        }
        let path = directory.join(name);
        fs::write(&path, text).expect("matrix should write");
        path
    };
    let spillover_csv = write_matrix("spillover.csv", spillover.matrix());
    let compensation_csv = write_matrix(
        "compensation.csv",
        &spillover.compensation_matrix().unwrap(),
    );

    let from_keyword = directory.join("keyword.fcs");
    let from_spillover = directory.join("spillover.fcs");
    let from_compensation = directory.join("compensation.fcs");
    compensate(&input, &from_keyword, &[]);
    compensate(
        &input,
        &from_spillover,
        &[
            "--matrix",
            utf8(&spillover_csv),
            "--matrix-kind",
            "spillover",
        ],
    );
    compensate(
        &input,
        &from_compensation,
        &[
            "--matrix",
            utf8(&compensation_csv),
            "--matrix-kind",
            "compensation",
        ],
    );

    let expected = read(&from_keyword);
    for path in [&from_spillover, &from_compensation] {
        let actual = read(path);
        for name in ["FL1-A", "FL2-A", "FL3-A"] {
            assert_eq!(
                f32_column(&actual, name),
                f32_column(&expected, name),
                "{name}"
            );
        }
        let state = cytiq_fcs::compensation_state(&actual.text)
            .unwrap()
            .unwrap();
        assert!(
            Path::new(&state.source)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("csv")),
            "{}",
            state.source
        );
    }
}

#[test]
fn refuses_to_compensate_twice() {
    let directory = create_temp_dir("cytiq-engine-compensate-twice");
    let once = directory.join("once.fcs");
    compensate(&fixture("gating", "sample.fcs"), &once, &[]);

    let output = cytiq(&[
        "compensate",
        "--input",
        utf8(&once),
        "--output",
        utf8(&directory.join("twice.fcs")),
    ]);

    assert!(!output.status.success());
    // The spillover keyword moved away, so the error names the missing matrix
    // unless one is passed; with a matrix the compensation state stops it.
    let with_matrix = cytiq(&[
        "compensate",
        "--input",
        utf8(&once),
        "--output",
        utf8(&directory.join("twice.fcs")),
        "--matrix",
        utf8(&fixture("autospill", "expected_spillover.csv")),
        "--matrix-kind",
        "spillover",
    ]);
    assert!(!with_matrix.status.success());
    let stderr = String::from_utf8_lossy(&with_matrix.stderr);
    assert!(stderr.contains("already compensated"), "stderr: {stderr}");
}

#[test]
fn requires_matrix_kind_with_matrix() {
    let directory = create_temp_dir("cytiq-engine-compensate-kind");
    let output = cytiq(&[
        "compensate",
        "--input",
        utf8(&fixture("gating", "sample.fcs")),
        "--output",
        utf8(&directory.join("out.fcs")),
        "--matrix",
        utf8(&fixture("autospill", "expected_spillover.csv")),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--matrix-kind"));
}

#[test]
fn fails_when_compensate_input_is_missing() {
    let directory = create_temp_dir("cytiq-engine-compensate-missing");
    let output = cytiq(&[
        "compensate",
        "--input",
        utf8(&directory.join("missing.fcs")),
        "--output",
        utf8(&directory.join("out.fcs")),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing.fcs"));
}

/// Gates on `FCS`-compensated dimensions of the raw file count the same events
/// as `uncompensated` dimensions of the `cytiq compensate` output.
#[test]
fn gating_compensated_output_matches_gating_raw_with_fcs_compensation() {
    let directory = create_temp_dir("cytiq-engine-compensate-gate");
    let raw = fixture("gating", "sample.fcs");
    let compensated = directory.join("sample.comp.fcs");
    compensate(&raw, &compensated, &[]);

    let gating_ml = |compensation: &str| {
        let path = directory.join(format!("{compensation}.xml"));
        let gates = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<gating:Gating-ML
    xmlns:gating="http://www.isac-net.org/std/Gating-ML/v2.0/gating"
    xmlns:transforms="http://www.isac-net.org/std/Gating-ML/v2.0/transformations"
    xmlns:data-type="http://www.isac-net.org/std/Gating-ML/v2.0/datatypes">
  <transforms:transformation transforms:id="logicle">
    <transforms:logicle transforms:T="262144" transforms:W="0.5" transforms:M="4.5" transforms:A="0"/>
  </transforms:transformation>
  <transforms:transformation transforms:id="asinh">
    <transforms:fasinh transforms:T="262144" transforms:M="4.5" transforms:A="0.5"/>
  </transforms:transformation>
  <gating:RectangleGate gating:id="CD3pos">
    <gating:dimension gating:min="0.55" gating:compensation-ref="{compensation}" gating:transformation-ref="logicle">
      <data-type:fcs-dimension data-type:name="FL1-A"/>
    </gating:dimension>
  </gating:RectangleGate>
  <gating:RectangleGate gating:id="CD4pos_CD8neg" gating:parent_id="CD3pos">
    <gating:dimension gating:min="0.5" gating:compensation-ref="{compensation}" gating:transformation-ref="asinh">
      <data-type:fcs-dimension data-type:name="FL2-A"/>
    </gating:dimension>
    <gating:dimension gating:max="0.5" gating:compensation-ref="{compensation}" gating:transformation-ref="asinh">
      <data-type:fcs-dimension data-type:name="FL3-A"/>
    </gating:dimension>
  </gating:RectangleGate>
</gating:Gating-ML>
"#
        );
        fs::write(&path, gates).expect("GatingML should write");
        path
    };
    let counts = |input: &Path, gates: &Path, name: &str| {
        let output = directory.join(name);
        run(&[
            "gate",
            "--input",
            utf8(input),
            "--gating-ml",
            utf8(gates),
            "--output",
            utf8(&output),
            "--format",
            "json",
        ]);
        let report: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output).expect("counts should exist"))
                .expect("counts should be JSON");
        report
            .as_array()
            .expect("counts should be a list")
            .iter()
            .map(|gate| (gate["gate_id"].clone(), gate["count"].clone()))
            .collect::<Vec<_>>()
    };

    let fcs = gating_ml("FCS");
    let uncompensated = gating_ml("uncompensated");
    let expected = counts(&raw, &fcs, "raw.json");
    assert_eq!(counts(&compensated, &uncompensated, "comp.json"), expected);
    assert!(expected.iter().all(|(_, count)| count.as_u64() > Some(0)));

    // Compensating the compensated channels again is refused.
    let refused = cytiq(&[
        "gate",
        "--input",
        utf8(&compensated),
        "--gating-ml",
        utf8(&fcs),
        "--output",
        utf8(&directory.join("refused.json")),
    ]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("compensated by `cytiq compensate`"),
        "stderr: {stderr}"
    );
}

/// Scatter is never compensated, but its detectors saturate like any other:
/// events at `$PnR - 1` on FSC or SSC fail `margin_pass` as they would on a
/// matrix channel (a `FlowCyt` bone-marrow case had 29% of its events there).
#[test]
fn flags_saturated_scatter_as_a_margin() {
    let directory = create_temp_dir("cytiq-engine-compensate-scatter-margin");
    let source = fixture("gating", "sample.fcs");
    let input = directory.join("saturated.fcs");
    let dataset = read(&source);
    let mut columns = dataset
        .data
        .columns
        .iter()
        .map(ColumnData::to_f64_vec)
        .collect::<Vec<_>>();
    let saturated = [("FSC-A", 0..5), ("SSC-A", 5..8)];
    for (name, rows) in saturated {
        let index = dataset.find_parameter(name).expect("scatter channel");
        let limit = cytiq_fcs::MarginBounds::from_raw_parameter(&dataset.parameters[index])
            .detector_limit();
        assert!(limit.is_finite(), "{name} has a $PnR");
        for row in rows {
            columns[index][row] = limit;
        }
    }
    let parameters = dataset
        .parameters
        .iter()
        .map(cytiq_fcs::FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    cytiq_fcs::write_fcs3_1_f64_with_options(
        &input,
        &parameters,
        &columns,
        &cytiq_fcs::FcsWriteOptions::new()
            .with_text_keywords(cytiq_fcs::carried_keywords(&dataset.text)),
    )
    .expect("saturated copy should write");

    compensate(&input, &directory.join("saturated.comp.fcs"), &[]);
    compensate(&source, &directory.join("plain.comp.fcs"), &[]);

    let mask = |name: &str| {
        EventMask::read_parquet(&directory.join(name)).expect("margin mask should be written")
    };
    let (saturated, plain) = (
        mask("saturated.comp.masks.parquet"),
        mask("plain.comp.masks.parquet"),
    );
    for (row, (kept, plain_kept)) in saturated.keep.iter().zip(&plain.keep).enumerate() {
        if row < 8 {
            assert!(!kept, "event {row} saturates a scatter detector");
        } else {
            assert_eq!(kept, plain_kept, "event {row} is as in the original file");
        }
    }
}

#[test]
fn preserves_large_original_ids_exactly() {
    let directory = create_temp_dir("cytiq-engine-compensate-original-id");
    let source = fixture("gating", "sample.fcs");
    let input = directory.join("sample.peacoqc.fcs");
    let ids = common::write_with_large_original_ids(&source, &input);
    let output = directory.join("sample.comp.fcs");
    let plain = directory.join("plain.comp.fcs");

    compensate(&input, &output, &[]);
    compensate(&source, &plain, &[]);

    let compensated = read(&output);
    let index = compensated.find_parameter("Original_ID").expect("lineage");
    let Some(ColumnData::F64(written)) = compensated.data.column(index) else {
        panic!("identifiers beyond 2^24 need a double column");
    };
    assert_eq!(written, &ids);
    // Compensated values do not depend on how the file had to be encoded.
    let plain = read(&plain);
    for name in ["FL1-A", "FL2-A", "FL3-A"] {
        let column = |dataset: &FcsDataset| {
            let index = dataset.find_parameter(name).expect("channel");
            dataset.data.column(index).expect("column").to_f64_vec()
        };
        assert_eq!(column(&compensated), column(&plain), "{name}");
    }
    let mask = EventMask::read_parquet(&directory.join("sample.comp.masks.parquet"))
        .expect("mask should be written");
    assert_eq!(mask.source_event_ids[0], 16_777_216);
    assert_eq!(mask.acquisition, common::sha256_hex(&input));
}

#[test]
fn non_finite_output_publishes_neither_fcs_mask_nor_provenance() {
    let directory = create_temp_dir("cytiq-engine-compensate-nonfinite");
    let input = directory.join("input.fcs");
    fs::copy(fixture("gating", "sample.fcs"), &input).unwrap();
    let dataset = read(&input);
    assert!(matches!(dataset.data.columns[0], ColumnData::F32(_)));
    let start = usize::try_from(dataset.header.data.unwrap().start).unwrap();
    let index = dataset.find_parameter("FL1-A").unwrap();
    let mut bytes = fs::read(&input).unwrap();
    bytes[start + index * 4..start + (index + 1) * 4].copy_from_slice(&f32::NAN.to_le_bytes());
    fs::write(&input, bytes).unwrap();
    let output = directory.join("output.fcs");
    let result = cytiq(&[
        "compensate",
        "--input",
        utf8(&input),
        "--output",
        utf8(&output),
    ]);
    assert!(!result.status.success());
    assert!(!output.exists());
    assert!(!directory.join("output.masks.parquet").exists());
    assert!(!directory.join("output.fcs.provenance.json").exists());
}

#[test]
fn refuses_declared_unmixed_channels_before_applying_a_matrix() {
    let directory = create_temp_dir("cytiq-engine-compensate-declared-unmixed");
    let input = directory.join("sample.fcs");
    cytiq_fcs::write_fcs3_1_exact_with_options(
        &input,
        &[cytiq_fcs::FcsWriteParameter::new("A").with_keyword("TYPE", "Unmixed_Fluorescence")],
        &[vec![2.0, 4.0]],
        &cytiq_fcs::FcsWriteOptions::new().with_text_keyword("$SPILL", "1,A,1"),
    )
    .unwrap();
    let path = directory.join("result.fcs");
    let output = cytiq(&[
        "compensate",
        "--input",
        utf8(&input),
        "--output",
        utf8(&path),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("spectrally unmixed"));
    assert!(!path.exists());
}

#[test]
fn refuses_to_compensate_unmixed_input() {
    let directory = create_temp_dir("cytiq-engine-compensate-unmixed");
    let input = directory.join("sample.unmixed.fcs");
    fs::copy(fixture("gating", "sample.fcs"), &input).expect("fixture should copy");
    common::add_keyword(
        &input,
        cytiq_fcs::UNMIXING_KEYWORD,
        r#"{"components":["FL1-A","FL2-A","FL3-A"],"detectors":["V1","V2","V3"]}"#,
    );

    let result = cytiq(&[
        "compensate",
        "--input",
        utf8(&input),
        "--output",
        utf8(&directory.join("sample.comp.fcs")),
    ]);

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("unmixing replaces compensation"),
        "{stderr}"
    );
}

/// `compensate` is the stage that linearises `$PnE` log amplification, so
/// the stages that refuse log-amplified input run on its output.
#[test]
fn linearises_log_amplified_input_for_later_stages() {
    let directory = create_temp_dir("cytiq-engine-compensate-log");
    let input = directory.join("sample.fcs");
    let output = directory.join("sample.comp.fcs");
    fs::copy(fixture("gating", "sample.fcs"), &input).expect("fixture should copy");
    common::set_log_amplified(&input, "FL1-A");

    compensate(&input, &output, &[]);

    let raw = read(&input);
    let compensated = read(&output);
    assert_eq!(cytiq_fcs::log_amplified_parameters(&raw), ["FL1-A"]);
    assert_eq!(
        cytiq_fcs::log_amplified_parameters(&compensated),
        Vec::<&str>::new()
    );
    let linear = ["FL1-A", "FL2-A", "FL3-A"]
        .iter()
        .map(|name| cytiq_fcs::linear_values(&raw, raw.find_parameter(name).unwrap()))
        .collect::<Vec<_>>();
    let spillover = cytiq_fcs::Spillover::from_dataset(&raw).unwrap().unwrap();
    let expected = cytiq_fcs::compensate(&linear, &spillover.compensation_matrix().unwrap());
    for (actual, expected) in f32_column(&compensated, "FL1-A").iter().zip(&expected[0]) {
        assert!(
            (f64::from(*actual) - expected).abs() <= expected.abs() * 1.0e-6 + 1.0e-6,
            "FL1-A: {actual} vs {expected}"
        );
    }

    run(&[
        "transform",
        "--input",
        utf8(&output),
        "--output",
        utf8(&directory.join("sample.t.fcs")),
    ]);
}
