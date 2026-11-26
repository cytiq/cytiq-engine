mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

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

/// A conventional manifest over the clean fixtures; `samples` and `markers`
/// are YAML list items.
fn manifest(samples: &str, markers: &str) -> String {
    format!(
        "cytometry: conventional\n\
         panel:\n  name: test\n  markers:\n{markers}\
         samples:\n{samples}\
         controls:\n  - {{ file: {control}, dye: Autofluorescence }}\n",
        control = utf8(&fixture("control.fcs")),
    )
}

const MARKERS: &str =
    "    - { marker: CD3 }\n    - { marker: CD19, channel: FL2-A }\n    - { marker: Live/Dead }\n";

fn sample(id: &str, path: &Path) -> String {
    format!("  - {{ id: {id}, file: {} }}\n", utf8(path))
}

fn preflight(directory: &Path, name: &str, text: &str) -> (std::process::Output, Value, PathBuf) {
    let manifest_path = directory.join(name);
    fs::write(&manifest_path, text).expect("manifest should write");
    let report_path = directory.join(format!("{name}.report.json"));
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "preflight",
            "--manifest",
            utf8(&manifest_path),
            "--output",
            utf8(&report_path),
        ])
        .output()
        .expect("command should run");
    let report = serde_json::from_str(
        &fs::read_to_string(&report_path).expect("the report is written even on failure"),
    )
    .expect("report should be JSON");
    (output, report, manifest_path)
}

fn failed_checks(report: &Value) -> Vec<String> {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter(|check| check["status"] == "fail")
        .map(|check| format!("{} {}", check["scope"], check["check"]))
        .collect()
}

#[test]
fn passes_a_consistent_run() {
    let directory = create_temp_dir("cytiq-engine-preflight-pass");
    let (output, report, manifest_path) = preflight(
        &directory,
        "run.yaml",
        &manifest(&sample("S01", &fixture("sample.fcs")), MARKERS),
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(report["failures"], 0, "{:?}", failed_checks(&report));
    // The fixtures have no `$CYT`/`$DATE`.
    assert_eq!(report["status"], "warn");
    let checks = report["checks"].as_array().expect("checks");
    assert!(checks.iter().any(|check| {
        check["check"] == "panel_marker" && check["message"] == "marker `CD3` → `FL1-A`"
    }));
    assert!(
        checks
            .iter()
            .any(|check| check["check"] == "spillover" && check["status"] == "pass")
    );
    common::assert_provenance(
        &directory.join("run.yaml.report.json.provenance.json"),
        "cytiq preflight",
        &[
            &manifest_path,
            &fixture("sample.fcs"),
            &fixture("control.fcs"),
        ],
        &[&directory.join("run.yaml.report.json")],
    );
}

#[test]
fn yaml_and_json_manifests_give_the_same_checks() {
    let directory = create_temp_dir("cytiq-engine-preflight-json");
    let yaml = manifest(&sample("S01", &fixture("sample.fcs")), MARKERS);
    let (_, from_yaml, _) = preflight(&directory, "run.yaml", &yaml);
    let json = format!(
        r#"{{"cytometry":"conventional",
            "panel":{{"name":"test","markers":[{{"marker":"CD3"}},{{"marker":"CD19","channel":"FL2-A"}},{{"marker":"Live/Dead"}}]}},
            "samples":[{{"id":"S01","file":"{sample}"}}],
            "controls":[{{"file":"{control}","dye":"Autofluorescence"}}]}}"#,
        sample = utf8(&fixture("sample.fcs")),
        control = utf8(&fixture("control.fcs")),
    );
    let (_, from_json, _) = preflight(&directory, "run.json", &json);
    assert_eq!(from_yaml["checks"], from_json["checks"]);
}

#[test]
fn fails_on_a_missing_required_marker() {
    let directory = create_temp_dir("cytiq-engine-preflight-marker");
    let markers =
        format!("{MARKERS}    - {{ marker: CD8 }}\n    - {{ marker: CD45, required: false }}\n");
    let (output, report, _) = preflight(
        &directory,
        "run.yaml",
        &manifest(&sample("S01", &fixture("sample.fcs")), &markers),
    );

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(failed_checks(&report), [r#""sample S01" "panel_marker""#]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("marker `CD8`"), "stderr: {stderr}");
}

#[test]
fn fails_on_mismatched_detectors_and_processed_files() {
    let directory = create_temp_dir("cytiq-engine-preflight-mismatch");
    let transformed = directory.join("sample.t.fcs");
    let transform = Command::new(env!("CARGO_BIN_EXE_cytiq"))
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
    assert!(transform.status.success());
    let samples = [
        sample("S01", &fixture("sample.fcs")),
        sample("S02", &fixture("no_viability.fcs")),
        sample("S03", &transformed),
    ]
    .concat();

    let (output, report, _) = preflight(&directory, "run.yaml", &manifest(&samples, MARKERS));

    assert!(!output.status.success());
    let failed = failed_checks(&report);
    for expected in [
        r#""sample S02" "panel_marker""#,
        r#""sample S02" "detector_configuration""#,
        r#""sample S03" "raw_signal""#,
    ] {
        assert!(failed.iter().any(|check| check == expected), "{failed:?}");
    }
}

#[test]
fn rejects_an_invalid_manifest() {
    let directory = create_temp_dir("cytiq-engine-preflight-invalid");
    let manifest_path = directory.join("run.yaml");
    fs::write(&manifest_path, "cytometry: conventional\nsampels: []\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "preflight",
            "--manifest",
            utf8(&manifest_path),
            "--output",
            utf8(&directory.join("report.json")),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown field `sampels`"),
        "stderr: {stderr}"
    );
}

/// A single-panel conventional manifest whose panel carries `compensation`.
fn manifest_with_compensation(samples: &str, compensation: &str) -> String {
    manifest(samples, MARKERS).replace(
        "  name: test\n",
        &format!("  name: test\n  compensation: {compensation}\n"),
    )
}

/// Copy the fixture with the specified embedded spillover keyword.
fn with_spillover(source: &Path, target: &Path, spillover: Option<&str>) {
    let dataset = cytiq_fcs::FcsReader::open(source)
        .and_then(cytiq_fcs::FcsReader::read)
        .expect("fixture should be readable");
    let keywords = cytiq_fcs::carried_keywords(&dataset.text)
        .into_iter()
        .filter(|(key, _)| !cytiq_fcs::is_spillover_keyword(key))
        .chain(spillover.map(|value| ("$SPILLOVER".to_owned(), value.to_owned())))
        .collect::<Vec<_>>();
    cytiq_fcs::write_fcs3_1_exact_with_options(
        target,
        &dataset
            .parameters
            .iter()
            .map(cytiq_fcs::FcsWriteParameter::from_source)
            .collect::<Vec<_>>(),
        &dataset
            .data
            .columns
            .iter()
            .map(cytiq_fcs::ColumnData::to_f64_vec)
            .collect::<Vec<_>>(),
        &cytiq_fcs::FcsWriteOptions::new().with_text_keywords(keywords),
    )
    .expect("copy should write");
}

fn checks_named<'a>(report: &'a Value, name: &str) -> Vec<&'a Value> {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter(|check| check["check"] == name)
        .collect()
}

#[test]
fn passes_with_an_external_compensation_matrix() {
    let directory = create_temp_dir("cytiq-engine-preflight-matrix");
    let controls = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/autospill");
    let matrix = directory.join("compensation.csv");
    let autospill = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "autospill",
            "--controls",
            utf8(&controls.join("controls.csv")),
            "--input-dir",
            utf8(&controls),
            "--output",
            utf8(&matrix),
        ])
        .output()
        .expect("command should run");
    assert!(autospill.status.success());
    let sample_path = directory.join("sample.fcs");
    with_spillover(&fixture("sample.fcs"), &sample_path, None);
    let samples = sample("S01", &sample_path);

    // Without a matrix source the missing `$SPILLOVER` fails the run.
    let (output, report, _) = preflight(&directory, "fcs.yaml", &manifest(&samples, MARKERS));
    assert!(!output.status.success());
    assert!(failed_checks(&report).contains(&r#""sample S01" "spillover""#.to_owned()));

    let (output, report, manifest_path) = preflight(
        &directory,
        "matrix.yaml",
        &manifest_with_compensation(
            &samples,
            "{ source: matrix, matrix: compensation.csv, kind: compensation }",
        ),
    );
    assert!(output.status.success(), "{:?}", failed_checks(&report));
    assert_eq!(
        checks_named(&report, "compensation_matrix")[0]["status"],
        "pass"
    );
    assert_eq!(
        checks_named(&report, "compensation_channels")[0]["status"],
        "pass"
    );
    assert_eq!(checks_named(&report, "spillover").len(), 0);
    assert_eq!(report["panels"][0]["compensation"]["source"], "matrix");
    common::assert_provenance(
        &directory.join("matrix.yaml.report.json.provenance.json"),
        "cytiq preflight",
        &[&manifest_path, &sample_path, &matrix],
        &[&directory.join("matrix.yaml.report.json")],
    );
}

#[test]
fn fails_when_the_matrix_misses_a_sample_channel() {
    let directory = create_temp_dir("cytiq-engine-preflight-matrix-channels");
    fs::write(
        directory.join("matrix.csv"),
        "channel,FL1-A,FL9-A\nFL1-A,1,0\nFL9-A,0,1\n",
    )
    .expect("matrix should write");

    let (output, report, _) = preflight(
        &directory,
        "run.yaml",
        &manifest_with_compensation(
            &sample("S01", &fixture("sample.fcs")),
            "{ source: matrix, matrix: matrix.csv, kind: compensation }",
        ),
    );

    assert!(!output.status.success());
    assert_eq!(
        failed_checks(&report),
        [r#""sample S01" "compensation_channels""#]
    );
    let message = checks_named(&report, "compensation_channels")[0]["message"]
        .as_str()
        .expect("message")
        .to_owned();
    assert!(message.contains("FL9-A"), "{message}");
}

#[test]
fn warns_when_row_vector_condition_exceeds_threshold() {
    let directory = create_temp_dir("cytiq-engine-preflight-matrix-condition");
    // The column-norm condition is 36; the row-norm condition is 121.
    fs::write(
        directory.join("spillover.csv"),
        "channel,FL1-A,FL2-A,FL3-A\nFL1-A,1,5,5\nFL2-A,0,1,0\nFL3-A,0,0,1\n",
    )
    .expect("matrix should write");

    let embedded = directory.join("embedded.fcs");
    with_spillover(
        &fixture("sample.fcs"),
        &embedded,
        Some("3,FL1-A,FL2-A,FL3-A,1,5,5,0,1,0,0,0,1"),
    );
    let (_, embedded_report, _) = preflight(
        &directory,
        "embedded.yaml",
        &manifest(&sample("S01", &embedded), MARKERS),
    );
    assert_eq!(
        checks_named(&embedded_report, "spillover")[0]["status"],
        "warn"
    );

    let (output, report, _) = preflight(
        &directory,
        "run.yaml",
        &manifest_with_compensation(
            &sample("S01", &fixture("sample.fcs")),
            "{ source: matrix, matrix: spillover.csv, kind: spillover }",
        ),
    );

    assert!(output.status.success(), "{:?}", failed_checks(&report));
    let check = checks_named(&report, "compensation_matrix")[0];
    assert_eq!(check["status"], "warn");
    assert!(
        check["message"]
            .as_str()
            .is_some_and(|message| message.contains("amplify noise")),
        "{check}"
    );
}

#[test]
fn controls_source_needs_controls_for_every_marker() {
    let directory = create_temp_dir("cytiq-engine-preflight-controls");
    let text = manifest_with_compensation(
        &sample("S01", &fixture("sample.fcs")),
        "{ source: controls }",
    )
    .replace(
        "controls:\n",
        &format!(
            "controls:\n  - {{ file: {}, dye: FITC, primary_channel: FL1-A }}\n",
            utf8(&fixture("control.fcs"))
        ),
    );

    let (output, report, _) = preflight(&directory, "run.yaml", &text);

    assert!(!output.status.success());
    assert_eq!(
        failed_checks(&report),
        [r#""panel test" "control_coverage""#]
    );
    let message = checks_named(&report, "control_coverage")[0]["message"]
        .as_str()
        .expect("message")
        .to_owned();
    assert!(
        message.contains("CD19 (FL2-A)")
            && message.contains("Live/Dead (FL3-A)")
            && !message.contains("CD3"),
        "{message}"
    );
}

#[test]
fn checks_each_sample_against_its_own_panel() {
    let directory = create_temp_dir("cytiq-engine-preflight-panels");
    // `no_viability.fcs` has no Live/Dead channel: valid for a panel without it.
    let text = format!(
        "cytometry: conventional\n\
         panels:\n\
         \x20 - name: full\n    markers:\n{MARKERS}\
         \x20 - name: lineage\n    markers:\n      - {{ marker: CD3 }}\n      - {{ marker: CD19, channel: FL2-A }}\n\
         samples:\n\
         \x20 - {{ id: S01, file: {full}, panel: full, specimen: P1 }}\n\
         \x20 - {{ id: S02, file: {lineage}, panel: lineage, specimen: P1 }}\n\
         controls:\n  - {{ file: {control}, dye: Autofluorescence, panel: full }}\n",
        MARKERS = MARKERS.replace("    - ", "      - "),
        full = utf8(&fixture("sample.fcs")),
        lineage = utf8(&fixture("no_viability.fcs")),
        control = utf8(&fixture("control.fcs")),
    );

    let (output, report, _) = preflight(&directory, "run.yaml", &text);

    assert!(output.status.success(), "{:?}", failed_checks(&report));
    let panels = report["panels"].as_array().expect("panels");
    assert_eq!(panels.len(), 2);
    assert_eq!(panels[1]["name"], "lineage");
    assert_eq!(panels[1]["samples"][0], "S02");
    assert!(panels[1]["controls"].as_array().is_some_and(Vec::is_empty));
}

#[test]
fn reports_log_amplified_samples_and_controls() {
    let directory = create_temp_dir("cytiq-engine-preflight-log");
    let sample_path = directory.join("sample.fcs");
    let control_path = directory.join("control.fcs");
    fs::copy(fixture("sample.fcs"), &sample_path).expect("fixture should copy");
    fs::copy(fixture("control.fcs"), &control_path).expect("fixture should copy");
    common::set_log_amplified(&sample_path, "FL1-A");
    common::set_log_amplified(&control_path, "FL1-A");
    let text = manifest(&sample("S01", &sample_path), MARKERS)
        .replace(utf8(&fixture("control.fcs")), utf8(&control_path));

    let (output, report, _) = preflight(&directory, "run.yaml", &text);

    // `cytiq compensate` linearises conventional samples; controls are read
    // as stored by `cytiq autospill`.
    assert!(!output.status.success());
    assert_eq!(
        failed_checks(&report),
        [r#""control Autofluorescence" "linear_scale""#]
    );
    let sample_check = checks_named(&report, "linear_scale")
        .into_iter()
        .find(|check| check["scope"] == "sample S01")
        .expect("sample linear_scale check");
    assert_eq!(sample_check["status"], "warn");
    assert!(
        sample_check["message"]
            .as_str()
            .is_some_and(|message| message.contains("`FL1-A` store log-amplified values")),
        "{sample_check}"
    );

    // Samples compensated elsewhere skip `cytiq compensate`, so nothing writes
    // them linear before the stages that refuse them.
    let stripped = directory.join("sample.nospill.fcs");
    with_spillover(&fixture("sample.fcs"), &stripped, None);
    common::set_log_amplified(&stripped, "FL1-A");
    let (output, report, _) = preflight(
        &directory,
        "compensated.yaml",
        &manifest(&sample("S01", &stripped), MARKERS).replace(
            "  name: test\n",
            "  name: test\n  signal_state: compensated\n",
        ),
    );
    assert!(!output.status.success());
    assert_eq!(failed_checks(&report), [r#""sample S01" "linear_scale""#]);
}

#[test]
fn declared_compensated_samples_must_not_keep_their_spillover() {
    let directory = create_temp_dir("cytiq-engine-preflight-compensated");
    let stripped = directory.join("sample.nospill.fcs");
    with_spillover(&fixture("sample.fcs"), &stripped, None);
    let declared = |samples: &str| {
        manifest(samples, MARKERS).replace(
            "  name: test\n",
            "  name: test\n  signal_state: compensated\n",
        )
    };

    // Stages would apply the file's `$SPILLOVER` to values already compensated.
    let (output, report, _) = preflight(
        &directory,
        "kept.yaml",
        &declared(&sample("S01", &fixture("sample.fcs"))),
    );
    assert!(!output.status.success());
    assert_eq!(failed_checks(&report), [r#""sample S01" "signal_state""#]);

    let (output, report, _) = preflight(
        &directory,
        "stripped.yaml",
        &declared(&sample("S01", &stripped)),
    );
    assert!(output.status.success(), "{:?}", failed_checks(&report));
    assert!(
        checks_named(&report, "spillover").is_empty(),
        "compensated samples need no matrix"
    );
    assert_eq!(report["panels"][0]["signal_state"], "compensated");
    assert_eq!(report["panels"][0]["compensation"], Value::Null);
}

#[test]
fn accepts_samples_compensated_by_cytiq_only_where_declared() {
    let directory = create_temp_dir("cytiq-engine-preflight-cytiq-compensated");
    let compensated = directory.join("sample.comp.fcs");
    let compensate = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "compensate",
            "--input",
            utf8(&fixture("sample.fcs")),
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
    let samples = sample("S01", &compensated);

    // `cytiq compensate` also moved the `$SPILLOVER` a raw panel needs aside.
    let (output, report, _) = preflight(&directory, "raw.yaml", &manifest(&samples, MARKERS));
    assert!(!output.status.success());
    assert_eq!(
        failed_checks(&report),
        [
            r#""sample S01" "raw_signal""#,
            r#""sample S01" "spillover""#
        ]
    );

    let (output, report, _) = preflight(
        &directory,
        "compensated.yaml",
        &manifest(&samples, MARKERS).replace(
            "  name: test\n",
            "  name: test\n  signal_state: compensated\n",
        ),
    );
    assert!(output.status.success(), "{:?}", failed_checks(&report));
    let raw_signal = checks_named(&report, "raw_signal")
        .into_iter()
        .find(|check| check["scope"] == "sample S01")
        .expect("sample raw_signal check");
    assert_eq!(raw_signal["status"], "pass");
    assert_eq!(
        raw_signal["message"],
        "compensated by `cytiq compensate`, as `signal_state: compensated` declares"
    );
}
