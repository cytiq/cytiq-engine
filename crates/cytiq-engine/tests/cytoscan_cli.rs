mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use cytiq_fcs::{FcsWriteOptions, FcsWriteParameter};

#[test]
fn cli_runs_cytoscan_outlier_with_debug_artifacts() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-outlier");
    let input_dir = directory.join("test");
    let output_dir = directory.join("out");
    let debug_dir = directory.join("debug");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 4.0, 8.0, 280.0]);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&output_dir),
            "--channels",
            "CD3,CD4,CD8",
            "--method",
            "quantiles",
            "--flag-method",
            "outlier",
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
    assert!(output_dir.join("cytoscan_summary.csv").is_file());
    assert!(output_dir.join("cytoscan_features.csv").is_file());
    assert!(output_dir.join("cytoscan_config.json").is_file());
    assert!(debug_dir.join("pca_coordinates.csv").is_file());
    common::assert_provenance(
        &output_dir.join("provenance.json"),
        "cytiq cytoscan",
        &[
            &input_dir.join("sample_00.fcs"),
            &input_dir.join("sample_03.fcs"),
        ],
        &[&output_dir.join("cytoscan_summary.csv")],
    );
    assert!(debug_dir.join("pca_loadings.csv").is_file());
    assert!(debug_dir.join("feature_heatmap.csv").is_file());
    assert!(debug_dir.join("boxplot_samples.csv").is_file());
    let report = fs::read_to_string(debug_dir.join("report.html")).expect("report should exist");
    assert!(report.contains("plotly"));
    assert!(report.contains("Top PCA Loadings"));
    assert!(report.contains("Feature Heatmap"));
    assert!(report.contains("Boxplots"));

    let rows = read_csv_rows(&output_dir.join("cytoscan_summary.csv"));
    assert_eq!(rows.len(), 4);
    // columns: filename, path, flag_method, feature_method, is_outlier, ...;
    // only the file shifted by 280 is an outlier
    let flagged = rows
        .iter()
        .filter(|row| &row[4] == "true")
        .map(|row| row[0].to_owned())
        .collect::<Vec<_>>();
    assert_eq!(flagged, vec!["sample_03.fcs".to_owned()]);
    let pca_header = read_csv_header(&debug_dir.join("pca_coordinates.csv"));
    assert!(pca_header.iter().any(|column| column == "status"));
    let heatmap_rows = read_csv_rows(&debug_dir.join("feature_heatmap.csv"));
    assert_eq!(heatmap_rows.len(), 4);
    let boxplot_rows = read_csv_rows(&debug_dir.join("boxplot_samples.csv"));
    // 4 files x 80 events x 3 channels (files are smaller than the 1000-event sample)
    assert_eq!(boxplot_rows.len(), 4 * 80 * 3);
    let feature_header = read_csv_header(&output_dir.join("cytoscan_features.csv"));
    assert!(
        feature_header
            .iter()
            .any(|header| header == "FL1-A [CD3]_0.1")
    );
}

#[test]
fn cli_seed_defaults_to_42_and_changes_outlier_scores() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-seed");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 4.0, 8.0, 280.0]);
    let run = |name: &str, seed: Option<&str>| {
        let output_dir = directory.join(name);
        let mut command = Command::new(env!("CARGO_BIN_EXE_cytiq"));
        command.args(["cytoscan", "--input", utf8(&input_dir)]);
        command.args(["--output", utf8(&output_dir), "--channels", "CD3,CD4,CD8"]);
        command.args(["--flag-method", "outlier", "--threads", "1"]);
        if let Some(seed) = seed {
            command.args(["--seed", seed]);
        }
        let output = command.output().expect("command should run");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let record: serde_json::Value = serde_json::from_slice(
            &fs::read(output_dir.join("provenance.json")).expect("provenance"),
        )
        .expect("provenance JSON");
        (
            fs::read(output_dir.join("cytoscan_summary.csv")).expect("summary"),
            record,
        )
    };

    let (default, default_record) = run("default", None);
    let (explicit, explicit_record) = run("explicit", Some("42"));
    let (other, other_record) = run("other", Some("7"));

    assert_eq!(default, explicit, "the default seed is 42");
    // The seed is recorded once, under the fingerprinted constants; the
    // recorded config of a run without `--seed` is unchanged.
    assert_eq!(default_record["constants"]["random_seed"], 42);
    assert_eq!(default_record["constants"], explicit_record["constants"]);
    let without_output = |record: &serde_json::Value| {
        let mut config = record["config"].clone();
        config.as_object_mut().expect("config").remove("output");
        config
    };
    assert_eq!(
        without_output(&default_record),
        without_output(&explicit_record)
    );
    assert!(default_record["config"].get("seed").is_none());
    assert_ne!(default, other, "the forest depends on the seed");
    assert_eq!(other_record["constants"]["random_seed"], 7);
}

#[test]
fn cli_runs_cytoscan_novelty_with_reference() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-novelty");
    let input_dir = directory.join("test");
    let reference_dir = directory.join("reference");
    let output_dir = directory.join("out");
    let debug_dir = directory.join("debug");
    write_cytoscan_fixture_set(&reference_dir, &[0.0, 3.0, 6.0, 9.0]);
    write_cytoscan_fixture_set(&input_dir, &[2.0, 260.0]);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--reference",
            utf8(&reference_dir),
            "--output",
            utf8(&output_dir),
            "--channel",
            "FL1-A [CD3]",
            "--channel",
            "FL2-A [CD4]",
            "--channel",
            "FL3-A [CD8]",
            "--method",
            "binning",
            "--flag-method",
            "novelty",
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
    let header = read_csv_header(&output_dir.join("cytoscan_summary.csv"));
    assert!(
        header
            .iter()
            .any(|column| column == "novelty_log_likelihood")
    );
    let config = fs::read_to_string(output_dir.join("cytoscan_config.json"))
        .expect("config json should exist");
    assert!(config.contains("novelty_threshold"));
    // 4 reference files, 30 binning features: mclust's spherical and
    // diagonal structures only, with a warning.
    let config: serde_json::Value = serde_json::from_str(&config).expect("config should be JSON");
    let diagnostics = &config["diagnostics"];
    assert_eq!(
        diagnostics["novelty_calibration_status"],
        "insufficient_reference"
    );
    // Novelty only: no outlier flag to describe; the novelty flag is
    // relative to the reference, not calibrated and not assessed for stability.
    assert!(diagnostics["outlier_flag"].is_null(), "{diagnostics}");
    let novelty = &diagnostics["novelty_flag"];
    assert_eq!(novelty["scope"], "against_reference");
    assert_eq!(novelty["threshold_rule"], "in_sample_quantile");
    assert_eq!(novelty["calibration_status"], "insufficient_reference");
    assert_eq!(novelty["population_false_positive_rate"], "not_assessed");
    assert_eq!(novelty["seed_dependence"], "seed_invariant");
    assert_eq!(novelty["stability_status"], "not_assessed");
    assert!(diagnostics.get("stability").is_none());
    assert!(diagnostics["reference_effective_rank"].is_u64());
    assert!(diagnostics["novelty_component_effective_counts"].is_array());
    assert!(
        diagnostics["preprocessing"]
            .as_array()
            .unwrap()
            .iter()
            .all(|channel| channel["compensated"] == true && channel["arcsinh_cofactor"] == 150.0)
    );
    assert!(
        ["XII", "XXI", "EII", "VII", "EEI", "VEI", "EVI", "VVI"]
            .contains(&diagnostics["novelty_model"].as_str().unwrap_or_default()),
        "{diagnostics}"
    );
    assert!(diagnostics["novelty_bic"].is_f64(), "{diagnostics}");
    assert!(
        diagnostics["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.iter().any(|warning| warning
                .as_str()
                .is_some_and(|warning| warning.contains("4 reference file(s) for 30 features")))),
        "{diagnostics}"
    );
    assert!(debug_dir.join("reference_features.csv").is_file());
    assert!(debug_dir.join("feature_heatmap.csv").is_file());
    assert!(debug_dir.join("boxplot_samples.csv").is_file());
    let pca_rows = read_csv_rows(&debug_dir.join("pca_coordinates.csv"));
    assert!(pca_rows.iter().any(|row| row.get(0) == Some("reference")));
    let report = fs::read_to_string(debug_dir.join("report.html")).expect("report should exist");
    assert!(report.contains("reference"));
    assert!(report.contains("Feature Heatmap"));
}

#[test]
fn cli_requires_reference_for_cytoscan_novelty() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-reference-required");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0]);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&directory.join("out")),
            "--channels",
            "CD3",
            "--flag-method",
            "novelty",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--reference"));
}

#[test]
fn cli_cytoscan_channels_and_repeated_channel_are_equivalent() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-channel-equivalence");
    let input_dir = directory.join("test");
    let first_output = directory.join("first");
    let second_output = directory.join("second");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0, 10.0]);

    let first = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&first_output),
            "--channels",
            "CD3,CD4,CD8",
            "--method",
            "summary",
            "--threads",
            "1",
        ])
        .output()
        .expect("command should run");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&second_output),
            "--channel",
            "CD3",
            "--channel",
            "CD4",
            "--channel",
            "CD8",
            "--method",
            "summary",
            "--threads",
            "1",
        ])
        .output()
        .expect("command should run");
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let first_features =
        fs::read(first_output.join("cytoscan_features.csv")).expect("features should exist");
    let second_features =
        fs::read(second_output.join("cytoscan_features.csv")).expect("features should exist");
    assert_eq!(first_features, second_features);
}

#[test]
fn cli_runs_cytoscan_all_feature_methods() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-methods");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 6.0, 12.0, 260.0]);

    for method in ["summary", "quantiles", "emd", "binning", "fingerprint"] {
        let output_dir = directory.join(method);
        let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
            .args([
                "cytoscan",
                "--input",
                utf8(&input_dir),
                "--output",
                utf8(&output_dir),
                "--channels",
                "CD3,CD4,CD8",
                "--method",
                method,
                "--threads",
                "1",
            ])
            .output()
            .expect("command should run");

        assert!(
            output.status.success(),
            "{method} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows = read_csv_rows(&output_dir.join("cytoscan_features.csv"));
        assert_eq!(rows.len(), 4);
    }
}

#[test]
fn cli_fails_when_cytoscan_channel_is_missing() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-missing-channel");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0]);

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&directory.join("out")),
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
fn cli_rejects_transformed_cytoscan_input() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-transformed");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0]);
    let transformed = input_dir.join("sample_01.fcs");
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "transform",
            "--allow-uncompensated",
            "--input",
            utf8(&transformed),
            "--output",
            utf8(&transformed),
            "--overwrite",
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
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&directory.join("out")),
            "--channels",
            "CD3,CD4",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pass the raw FCS"), "stderr: {stderr}");
}

#[test]
fn cli_rejects_compensated_cytoscan_input() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-compensated");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0]);
    // A `cytiq compensate` output: no spillover left, linear values.
    common::add_keyword(
        &input_dir.join("sample_01.fcs"),
        cytiq_fcs::COMPENSATION_KEYWORD,
        r#"{"source":"fcs","channels":["FL1-A","FL2-A"]}"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&directory.join("out")),
            "--channels",
            "CD3,CD4",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("was compensated by `cytiq compensate`")
            && stderr.contains("pass the raw FCS"),
        "stderr: {stderr}"
    );
}

/// Rewrite `path` without its spillover keyword.
fn strip_spillover(path: &Path) {
    let dataset = cytiq_fcs::FcsReader::open(path)
        .and_then(cytiq_fcs::FcsReader::read)
        .expect("fixture should be readable");
    let parameters = dataset
        .parameters
        .iter()
        .map(FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    let columns = dataset
        .data
        .columns
        .iter()
        .map(|column| match column {
            cytiq_fcs::ColumnData::F32(values) => values.clone(),
            other => panic!("expected f32 fixture column, got {other:?}"),
        })
        .collect::<Vec<_>>();
    let keywords = cytiq_fcs::carried_keywords(&dataset.text)
        .into_iter()
        .filter(|(key, _)| !cytiq_fcs::is_spillover_keyword(key));
    cytiq_fcs::write_fcs3_1_f32_with_options(
        path,
        &parameters,
        &columns,
        &FcsWriteOptions::new().with_text_keywords(keywords),
    )
    .expect("fixture should be rewritten");
}

#[test]
fn cli_strict_rejects_files_without_spillover() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-strict");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0, 10.0]);
    strip_spillover(&input_dir.join("sample_01.fcs"));
    let run = |strict: bool, output: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cytiq"));
        command.args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&directory.join(output)),
            "--channels",
            "CD3,CD4",
        ]);
        if strict {
            command.arg("--strict");
        }
        command.output().expect("command should run")
    };

    let lenient = run(false, "lenient");
    assert!(
        !lenient.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&lenient.stderr)
    );
    assert!(String::from_utf8_lossy(&lenient.stderr).contains("incompatible"));
    assert!(!directory.join("lenient/cytoscan_summary.csv").exists());

    let strict = run(true, "strict");
    assert!(!strict.status.success());
    let stderr = String::from_utf8_lossy(&strict.stderr);
    assert!(
        stderr.contains("sample_01.fcs") && stderr.contains("--strict"),
        "stderr: {stderr}"
    );
}

fn write_cytoscan_fixture_set(directory: &Path, offsets: &[f32]) {
    fs::create_dir_all(directory).expect("fixture directory should be created");
    for (index, offset) in offsets.iter().enumerate() {
        write_cytoscan_fixture_fcs(&directory.join(format!("sample_{index:02}.fcs")), *offset);
    }
}

fn write_cytoscan_fixture_fcs(path: &Path, marker_offset: f32) {
    write_cytoscan_fixture_events(path, marker_offset, 80);
}

/// `events` events; beyond 80, a deterministic jitter makes every
/// subsample's quantiles differ.
fn write_cytoscan_fixture_events(path: &Path, marker_offset: f32, events: u16) {
    let mut columns = vec![Vec::new(); 6];
    for event_index in 0_u16..events {
        let event = f32::from(event_index);
        let jitter = if events > 80 {
            f32::from((event_index.wrapping_mul(7919) ^ (event_index >> 3)) % 97) * 0.9
        } else {
            0.0
        };
        columns[0].push(event);
        columns[1].push(20.0 + event * 0.2);
        columns[2].push(30.0 + event * 0.25);
        columns[3].push(100.0 + marker_offset + event * 1.2 + jitter);
        columns[4].push(160.0 + marker_offset * 0.5 + event * 0.8 + jitter * 0.5);
        columns[5].push(90.0 + marker_offset * 0.25 + event * 0.6 + jitter * 0.25);
    }
    let parameters = vec![
        FcsWriteParameter::new("Time"),
        FcsWriteParameter::new("FSC-A"),
        FcsWriteParameter::new("SSC-A"),
        FcsWriteParameter::with_label("FL1-A", "CD3"),
        FcsWriteParameter::with_label("FL2-A", "CD4"),
        FcsWriteParameter::with_label("FL3-A", "CD8"),
    ];
    let options = FcsWriteOptions::new().with_text_keyword(
        "SPILL",
        "3,FL1-A,FL2-A,FL3-A,1,0.02,0.01,0.01,1,0.02,0.0,0.02,1",
    );
    cytiq_fcs::write_fcs3_1_f32_with_options(path, &parameters, &columns, &options)
        .expect("fixture FCS should write");
}

/// Test files whose events exceed the per-file sample (1,000), so the event
/// subsample depends on the seed, and a reference cohort to fit novelty on.
fn write_subsampled_cohort(directory: &Path) -> (PathBuf, PathBuf) {
    let test = directory.join("test");
    let reference = directory.join("reference");
    fs::create_dir_all(&test).expect("test directory");
    fs::create_dir_all(&reference).expect("reference directory");
    for (index, offset) in [0.0, 3.0, 9.0, 160.0].iter().enumerate() {
        write_cytoscan_fixture_events(&test.join(format!("test_{index:02}.fcs")), *offset, 1_500);
    }
    for index in 0_u16..10 {
        let offset = f32::from(index) * 1.7;
        write_cytoscan_fixture_events(
            &reference.join(format!("reference_{index:02}.fcs")),
            offset,
            1_500 + index,
        );
    }
    (test, reference)
}

fn run_cytoscan(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(["cytoscan"])
        .args(args)
        .args(["--threads", "1"])
        .output()
        .expect("command should run")
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).expect("JSON file")).expect("valid JSON")
}

/// The `seed_index` rows of `stable`'s per-seed file equal the summary of the
/// paired run, and the last pair's threshold equals that run's diagnostics.
fn assert_seed_rows_repeat_summaries(stable: &Path, expected: &[(usize, &Path)]) {
    let header = read_csv_header(&stable.join("cytoscan_stability_seeds.csv"));
    let column = |name: &str| header.iter().position(|h| h == name).expect(name);
    let seed_rows = read_csv_rows(&stable.join("cytoscan_stability_seeds.csv"));
    assert_eq!(seed_rows.len(), 3 * 4);
    let summary_header = read_csv_header(&stable.join("cytoscan_summary.csv"));
    let summary_column = |name: &str| summary_header.iter().position(|h| h == name).expect(name);
    for &(index, expected_dir) in expected {
        let expected = read_csv_rows(&expected_dir.join("cytoscan_summary.csv"));
        let rows = seed_rows
            .iter()
            .filter(|row| row.get(column("seed_index")) == Some(&index.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), expected.len());
        for (row, summary) in rows.iter().zip(&expected) {
            assert_eq!(row.get(column("status")), Some("ok"));
            for name in [
                "filename",
                "is_outlier",
                "outlier_score",
                "is_novelty",
                "novelty_log_likelihood",
            ] {
                assert_eq!(
                    row.get(column(name)),
                    summary.get(summary_column(name)),
                    "seed {index} {name}"
                );
            }
        }
    }
    let &(index, expected_dir) = expected.last().expect("a seed to compare");
    let seed_record = read_json(&expected_dir.join("cytoscan_config.json"));
    let threshold = seed_rows
        .iter()
        .find(|row| row.get(column("seed_index")) == Some(&index.to_string()))
        .and_then(|row| row.get(column("novelty_threshold")))
        .expect("threshold");
    assert_eq!(
        threshold,
        format!(
            "{:.12}",
            seed_record["diagnostics"]["novelty_threshold"]
                .as_f64()
                .expect("threshold")
        )
    );
}

/// Provenance and `cytoscan_config.json` carry the stability option and its
/// results only for the run that asked for them.
fn assert_stability_is_recorded_only_when_requested(plain: &Path, stable: &Path) {
    let plain_record = read_json(&plain.join("provenance.json"));
    let stable_record = read_json(&stable.join("provenance.json"));
    assert!(plain_record["config"].get("stability_seeds").is_none());
    assert_eq!(stable_record["config"]["stability_seeds"], 3);
    assert!(
        stable_record["outputs"]
            .to_string()
            .contains("cytoscan_stability_seeds.csv")
    );
    let plain_config = read_json(&plain.join("cytoscan_config.json"));
    assert!(plain_config["parameters"].get("stability_seeds").is_none());
    assert!(plain_config["diagnostics"].get("stability").is_none());
    assert_eq!(
        plain_config["diagnostics"]["novelty_flag"]["stability_status"],
        "not_assessed"
    );
}

#[test]
fn cli_stability_seeds_repeat_separate_seed_runs_without_changing_the_summary() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-stability");
    let (test, reference) = write_subsampled_cohort(&directory);
    let common = |output: &Path| {
        vec![
            "--input".to_owned(),
            utf8(&test).to_owned(),
            "--reference".to_owned(),
            utf8(&reference).to_owned(),
            "--output".to_owned(),
            utf8(output).to_owned(),
            "--channels".to_owned(),
            "CD3,CD4,CD8".to_owned(),
            "--method".to_owned(),
            "summary".to_owned(),
            "--flag-method".to_owned(),
            "both".to_owned(),
        ]
    };
    let run = |output: &Path, extra: &[&str]| {
        let mut args = common(output);
        args.extend(extra.iter().map(|value| (*value).to_owned()));
        let output = run_cytoscan(&args.iter().map(String::as_str).collect::<Vec<_>>());
        assert_success(&output);
    };
    let plain = directory.join("plain");
    let stable = directory.join("stable");
    let seed_two = directory.join("seed-two");
    run(&plain, &[]);
    run(&stable, &["--stability-seeds", "3"]);
    let derived = cytiq_cytoscan::stability_seed(42, 2);
    assert_eq!(derived, 42 + 2 * (1_u64 << 32));
    run(&seed_two, &["--seed", &derived.to_string()]);

    // The primary outputs do not depend on the stability run.
    for name in ["cytoscan_summary.csv", "cytoscan_features.csv"] {
        assert_eq!(
            fs::read(plain.join(name)).expect("plain"),
            fs::read(stable.join(name)).expect("stable"),
            "{name}"
        );
    }
    assert!(!plain.join("cytoscan_stability.csv").exists());
    assert!(!plain.join("cytoscan_stability_seeds.csv").exists());

    assert_seed_rows_repeat_summaries(&stable, &[(0, &stable), (2, &seed_two)]);

    let files = read_csv_rows(&stable.join("cytoscan_stability.csv"));
    assert_eq!(files.len(), 4);
    let diagnostics = &read_json(&stable.join("cytoscan_config.json"))["diagnostics"];
    let stability = &diagnostics["stability"];
    assert_eq!(
        stability["seeds"],
        serde_json::json!([42, cytiq_cytoscan::stability_seed(42, 1), derived])
    );
    assert_eq!(stability["failed_seeds"], serde_json::json!([]));
    assert_eq!(
        diagnostics["novelty_flag"]["seed_dependence"],
        "seed_dependent"
    );
    assert_eq!(diagnostics["outlier_flag"]["scope"], "within_test_cohort");
    assert_eq!(diagnostics["novelty_flag"]["scope"], "against_reference");
    for flag in ["outlier", "novelty"] {
        let status = stability[flag]["status"].as_str().expect("status");
        assert!(["stable", "unstable"].contains(&status), "{status}");
        assert_eq!(
            diagnostics[format!("{flag}_flag")]["stability_status"],
            status
        );
        assert_eq!(stability[flag]["seeds_assessed"], 3);
    }

    assert_stability_is_recorded_only_when_requested(&plain, &stable);
}

#[test]
fn cli_stability_on_short_files_reports_seed_invariant_novelty() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-stability-invariant");
    let input_dir = directory.join("test");
    let reference_dir = directory.join("reference");
    let output_dir = directory.join("out");
    write_cytoscan_fixture_set(&reference_dir, &[0.0, 3.0, 6.0, 9.0, 12.0]);
    write_cytoscan_fixture_set(&input_dir, &[2.0, 260.0, 5.0]);
    let output = run_cytoscan(&[
        "--input",
        utf8(&input_dir),
        "--reference",
        utf8(&reference_dir),
        "--output",
        utf8(&output_dir),
        "--channels",
        "CD3,CD4,CD8",
        "--method",
        "summary",
        "--flag-method",
        "both",
        "--stability-seeds",
        "4",
    ]);
    assert_success(&output);
    // 80 events per file: nothing is subsampled, so every seed computes the
    // same novelty values; only the isolation trees change.
    let diagnostics = &read_json(&output_dir.join("cytoscan_config.json"))["diagnostics"];
    assert_eq!(
        diagnostics["novelty_flag"]["seed_dependence"],
        "seed_invariant"
    );
    assert_eq!(
        diagnostics["novelty_flag"]["stability_status"],
        "seed_invariant"
    );
    assert_eq!(
        diagnostics["stability"]["novelty"]["status"],
        "seed_invariant"
    );
    assert_eq!(
        diagnostics["stability"]["novelty"]["pairwise_disagreement"],
        0.0
    );
    assert_eq!(
        diagnostics["outlier_flag"]["seed_dependence"],
        "seed_dependent"
    );
    let seeds = read_csv_rows(&output_dir.join("cytoscan_stability_seeds.csv"));
    let header = read_csv_header(&output_dir.join("cytoscan_stability_seeds.csv"));
    let likelihood = header
        .iter()
        .position(|h| h == "novelty_log_likelihood")
        .expect("column");
    for file in 0..3 {
        let values = seeds
            .iter()
            .skip(file)
            .step_by(3)
            .map(|row| row.get(likelihood).expect("value").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 4);
        assert!(
            values.windows(2).all(|pair| pair[0] == pair[1]),
            "{values:?}"
        );
    }
}

#[test]
fn cli_rejects_out_of_range_stability_seed_counts() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-stability-range");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0]);
    for count in ["0", "1", "65", "many"] {
        let output_dir = directory.join(format!("out-{count}"));
        let output = run_cytoscan(&[
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&output_dir),
            "--channels",
            "CD3",
            "--stability-seeds",
            count,
        ]);
        assert_eq!(output.status.code(), Some(2), "{count}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("from 2 to 64"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output_dir.exists());
    }
}

#[test]
fn cli_refuses_to_leave_stale_stability_outputs() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-stability-stale");
    let input_dir = directory.join("test");
    let output_dir = directory.join("out");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0, 260.0]);
    let args = |extra: &[&'static str]| {
        let mut args = vec![
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&output_dir),
            "--channels",
            "CD3,CD4",
        ];
        args.extend_from_slice(extra);
        args
    };
    assert_success(&run_cytoscan(&args(&["--stability-seeds", "2"])));
    let summary = fs::read(output_dir.join("cytoscan_summary.csv")).expect("summary");
    // A plain rerun would publish next to stability files it does not
    // describe: refused before any work, exit 4, nothing changed.
    let refused = run_cytoscan(&args(&["--overwrite"]));
    assert_eq!(refused.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("earlier --stability-seeds run"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(
        fs::read(output_dir.join("cytoscan_summary.csv")).expect("summary"),
        summary
    );
    // Rerunning the stability run replaces both files.
    assert_success(&run_cytoscan(&args(&[
        "--overwrite",
        "--stability-seeds",
        "3",
    ])));
    let rows = read_csv_rows(&output_dir.join("cytoscan_stability_seeds.csv"));
    assert_eq!(rows.len(), 3 * 3);
}

fn read_csv_header(path: &Path) -> Vec<String> {
    let mut reader = csv::Reader::from_path(path).expect("CSV should open");
    reader
        .headers()
        .expect("headers should parse")
        .iter()
        .map(ToOwned::to_owned)
        .collect()
}

fn read_csv_rows(path: &Path) -> Vec<csv::StringRecord> {
    let mut reader = csv::Reader::from_path(path).expect("CSV should open");
    reader
        .records()
        .collect::<Result<Vec<_>, _>>()
        .expect("CSV rows should parse")
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
fn cli_rejects_log_amplified_cytoscan_input() {
    let directory = create_temp_dir("cytiq-engine-cytoscan-log");
    let input_dir = directory.join("test");
    write_cytoscan_fixture_set(&input_dir, &[0.0, 5.0]);
    common::set_log_amplified(&input_dir.join("sample_01.fcs"), "FL1-A");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "cytoscan",
            "--input",
            utf8(&input_dir),
            "--output",
            utf8(&directory.join("out")),
            "--channels",
            "CD3,CD4",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sample_01.fcs") && stderr.contains("`FL1-A` store log-amplified values"),
        "stderr: {stderr}"
    );
}
