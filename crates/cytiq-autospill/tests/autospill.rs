#[path = "support.rs"]
mod support;

use std::fs;

use cytiq_autospill::{
    AutospillConfig, AutospillError, AutospillRunner, AutospreadConfig, AutospreadRunner,
    MatrixKind, export_matrix, read_controls_manifest, read_detectors_manifest,
    read_transform_manifest,
};

#[test]
fn runner_matches_synthetic_spillover_fixture() {
    let directory = support::copy_autospill_fixture_set("autospill-fixture");
    let manifest_path = directory.join("controls.csv");
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");

    let result = AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
        .expect("autospill should succeed");

    assert_eq!(result.channels, vec!["FL1-A", "FL2-A"]);
    assert_close(result.spillover_matrix[0][1], 0.2, 1e-6);
    assert_close(result.spillover_matrix[1][0], 0.1, 1e-6);
    assert_matrix_csv(
        &result.compensation_matrix,
        &support::read_fixture_text("expected_compensation.csv"),
        1e-9,
    );
    assert_matrix_csv(
        &result.spillover_matrix,
        &support::read_fixture_text("expected_spillover.csv"),
        1e-9,
    );
    assert!(
        result
            .gate_stats
            .iter()
            .all(|stats| stats.gated_events >= 60)
    );
    assert!(
        result
            .gate_stats
            .iter()
            .all(|stats| stats.bound_maxima >= 1)
    );
    assert!(
        result
            .gate_stats
            .iter()
            .all(|stats| stats.region_maxima >= 1)
    );
    assert!(result.convergence.converged);
}

#[test]
fn export_matrix_writes_parquet_magic_bytes() {
    let directory = support::copy_autospill_fixture_set("autospill-parquet");
    let manifest_path = directory.join("controls.csv");
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
    let result = AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
        .expect("autospill should succeed");

    let output_path = directory.join("matrix.parquet");
    export_matrix(
        &result,
        MatrixKind::Compensation,
        &output_path,
        cytiq_publish::Existing::Refuse,
    )
    .expect("parquet export should succeed");

    let bytes = fs::read(output_path).expect("parquet output should exist");
    assert!(bytes.len() > 8);
    assert_eq!(&bytes[..4], b"PAR1");
    assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
}

#[test]
fn runner_reports_unmatched_primary_channel_from_fcs_metadata() {
    let directory = support::create_temp_dir("autospill-missing-channel");
    let detector_labels = vec![None, None];
    support::write_control_file(
        &directory,
        "control_a.fcs",
        0,
        &[1.0, 0.2],
        &["FL1-A", "FL2-A"],
        &detector_labels,
    );
    let manifest_path = support::write_manifest_rows(&directory, &[("control_a.fcs", "FITC")]);
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");

    let error = AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
        .expect_err("missing channel should fail");
    assert!(matches!(
        error,
        AutospillError::PrimaryChannelNotFound { ref dye, .. } if dye == "FITC"
    ));
}

#[test]
fn runner_reports_ambiguous_primary_channel_from_fcs_metadata() {
    let directory = support::create_temp_dir("autospill-ambiguous-channel");
    let detector_labels = vec![None, None];
    support::write_control_file(
        &directory,
        "control_fitc.fcs",
        0,
        &[1.0, 0.2],
        &["FITC-A", "FITC-H"],
        &detector_labels,
    );
    let manifest_path = support::write_manifest_rows(&directory, &[("control_fitc.fcs", "FITC")]);
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");

    let error = AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
        .expect_err("ambiguous channel should fail");
    assert!(matches!(
        error,
        AutospillError::AmbiguousPrimaryChannel { ref dye, .. } if dye == "FITC"
    ));
}

#[test]
fn runner_infers_primary_channel_from_short_name() {
    let directory = support::create_temp_dir("autospill-short-name");
    let manifest_path = support::write_control_fixture_set(
        &directory,
        &["FITC", "PE"],
        &["FITC-A", "PE-A"],
        &["FITC-A", "PE-A"],
        &[vec![1.0, 0.2], vec![0.1, 1.0]],
        None,
    );
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");

    let result = AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
        .expect("autospill should infer short names");

    assert_eq!(result.primary_channels, vec!["FITC-A", "PE-A"]);
}

#[test]
fn runner_reports_duplicate_inferred_primary_channels() {
    let directory = support::create_temp_dir("autospill-duplicate-channel");
    support::write_control_file(
        &directory,
        "control_fitc.fcs",
        0,
        &[1.0, 0.2],
        &["FL1-A", "FL2-A"],
        &[Some("FITC".to_owned()), None],
    );
    support::write_control_file(
        &directory,
        "control_pe.fcs",
        0,
        &[1.0, 0.2],
        &["FL1-A", "FL2-A"],
        &[Some("PE".to_owned()), None],
    );
    let manifest_path = support::write_manifest_rows(
        &directory,
        &[("control_fitc.fcs", "FITC"), ("control_pe.fcs", "PE")],
    );
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");

    let error = AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
        .expect_err("duplicate inferred primary channel should fail");
    assert!(matches!(
        error,
        AutospillError::DuplicatePrimaryChannel { ref value } if value == "FL1-A"
    ));
}

#[test]
fn runner_caps_values_above_the_channel_range() {
    // autospill reads controls with `flowCore::read.FCS`, whose default
    // `truncate_max_range = TRUE` caps every value above `$PnR` at `$PnR`.
    let run = |label: &str, limit: Option<bool>| {
        let directory = support::create_temp_dir(label);
        let manifest_path =
            support::write_two_control_fixture(&directory, [[1.0, 0.2], [0.1, 1.0]]);
        if let Some(cap_data) = limit {
            for file in ["control_a.fcs", "control_b.fcs"] {
                for parameter in [3, 4] {
                    support::limit_parameter(&directory.join(file), parameter, 20_000, cap_data);
                }
            }
        }
        let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
        AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
            .expect("autospill should succeed")
            .spillover_matrix
    };
    let unlimited = run("autospill-range-unlimited", None);
    let by_range = run("autospill-range-pnr", Some(false));
    let by_data = run("autospill-range-capped", Some(true));

    for (range_row, data_row) in by_range.iter().zip(&by_data) {
        for (range_value, data_value) in range_row.iter().zip(data_row) {
            assert_close(*range_value, *data_value, 1e-12);
        }
    }
    assert!(
        (by_range[0][1] - unlimited[0][1]).abs() > 1e-6,
        "capping should change the fit: {} vs {}",
        by_range[0][1],
        unlimited[0][1]
    );
}

#[test]
fn runner_reports_singular_matrix() {
    let directory = support::create_temp_dir("autospill-singular");
    let manifest_path = support::write_two_control_fixture(&directory, [[1.0, 1.0], [1.0, 1.0]]);
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");

    let error = AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
        .expect_err("singular spillover matrix should fail");
    assert!(matches!(error, AutospillError::SingularMatrix));
}

#[test]
fn runner_supports_rectangular_detector_sets() {
    let directory = support::create_temp_dir("autospill-rectangular");
    let (manifest_path, detectors_path) = support::write_rectangular_autospill_fixture(&directory);
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
    let detectors = read_detectors_manifest(&detectors_path).expect("detectors should load");
    let config = AutospillConfig {
        detectors,
        ..AutospillConfig::default()
    };

    let result = AutospillRunner::run(&manifest, &directory, &config)
        .expect("rectangular autospill should succeed");

    assert_eq!(result.dyes, vec!["FITC", "PE"]);
    assert_eq!(result.detectors, vec!["FL1-A", "FL2-A", "FL3-A"]);
    assert_eq!(result.spillover_matrix.len(), 2);
    assert_eq!(result.spillover_matrix[0].len(), 3);
    assert_eq!(result.compensation_matrix.len(), 3);
    assert_eq!(result.compensation_matrix[0].len(), 2);
    assert_close(result.spillover_matrix[0][1], 0.2, 1e-6);
    assert_close(result.spillover_matrix[0][2], 0.05, 1e-5);
    assert_close(result.spillover_matrix[1][0], 0.1, 1e-6);
    assert_close(result.spillover_matrix[1][2], 0.03, 1e-5);
}

#[test]
fn runner_supports_autofluorescence_controls() {
    let directory = support::create_temp_dir("autospill-autofluorescence");
    let (manifest_path, detectors_path) = support::write_autofluorescence_fixture(&directory);
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
    let detectors = read_detectors_manifest(&detectors_path).expect("detectors should load");
    let config = AutospillConfig {
        detectors,
        ..AutospillConfig::default()
    };

    let result = AutospillRunner::run(&manifest, &directory, &config)
        .expect("autospill with autofluorescence should succeed");

    assert_eq!(result.dyes, vec!["FITC", "PE", "Autofluorescence"]);
    assert_eq!(result.detectors, vec!["FL1-A", "FL2-A", "AF-A"]);
    assert_eq!(result.spillover_matrix.len(), 3);
    assert_eq!(result.spillover_matrix[2].len(), 3);
    assert_close(result.spillover_matrix[2][2], 1.0, 1e-9);
    assert!(result.spillover_matrix[2][0] > 0.0);
    assert!(result.spillover_matrix[2][1] > 0.0);
}

#[test]
fn runner_requires_detectors_for_autofluorescence_controls() {
    let directory = support::create_temp_dir("autospill-autofluorescence-no-detectors");
    let (manifest_path, _) = support::write_autofluorescence_fixture(&directory);
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");

    let error = AutospillRunner::run(&manifest, &directory, &AutospillConfig::default())
        .expect_err("autofluorescence without detectors should fail");
    assert!(matches!(
        error,
        AutospillError::AutofluorescenceRequiresDetectors { .. }
    ));
}

#[test]
fn runner_reports_ambiguous_autofluorescence_detector() {
    let directory = support::create_temp_dir("autospill-autofluorescence-ambiguous");
    let (manifest_path, detectors_path) = support::write_autofluorescence_fixture(&directory);
    support::write_text(
        &detectors_path,
        "channel,label,wavelength\nFL1-A,FITC detector,530\nFL2-A,PE detector,585\nAF-A,Autofluorescence A,670\nAF-B,Autofluorescence B,710\n",
    );
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
    let detectors = read_detectors_manifest(&detectors_path).expect("detectors should load");
    let config = AutospillConfig {
        detectors,
        ..AutospillConfig::default()
    };

    let error = AutospillRunner::run(&manifest, &directory, &config)
        .expect_err("ambiguous autofluorescence detector should fail");
    assert!(matches!(
        error,
        AutospillError::AutofluorescenceDetectorResolution { ref candidates, .. }
            if candidates == &vec!["AF-A".to_owned(), "AF-B".to_owned()]
    ));
}

#[test]
fn runner_writes_debug_artifacts() {
    let directory = support::create_temp_dir("autospill-debug");
    let debug_dir = directory.join("debug");
    let (manifest_path, detectors_path) = support::write_rectangular_autospill_fixture(&directory);
    let transform_path = support::write_transform_manifest(&directory, &["FITC", "PE"]);
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
    let detectors = read_detectors_manifest(&detectors_path).expect("detectors should load");
    let transforms = read_transform_manifest(&transform_path).expect("transforms should load");
    let config = AutospillConfig {
        detectors,
        transforms,
        debug_dir: Some(debug_dir.clone()),
        ..AutospillConfig::default()
    };

    let _ = AutospillRunner::run(&manifest, &directory, &config)
        .expect("autospill with debug output should succeed");

    assert!(debug_dir.join("summary.json").is_file());
    let report_path = debug_dir.join("report.html");
    assert!(report_path.is_file(), "missing report.html");
    let report = fs::read_to_string(report_path).expect("report should exist");
    assert!(report.contains("cytiq AutoSpill Report"));
    assert!(report.contains("Summary"));
    assert!(report.contains("Convergence"));
    assert!(report.contains("Spillover Matrix"));
    assert!(report.contains("Compensation Matrix"));
    assert!(report.contains("Spillover Regression"));
}

#[test]
fn autospread_estimates_positive_spread_coefficients() {
    let directory = support::copy_autospread_fixture_set("autospread");
    let manifest_path = directory.join("controls.csv");
    let detectors_path = directory.join("detectors.csv");
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
    let detectors = read_detectors_manifest(&detectors_path).expect("detectors should load");
    let config = AutospreadConfig {
        detectors,
        ..AutospreadConfig::default()
    };

    let result =
        AutospreadRunner::run(&manifest, &directory, &config).expect("autospread should succeed");

    assert_eq!(result.spread_matrix.len(), 2);
    assert_eq!(result.spread_matrix[0].len(), 3);
    assert_matrix_csv(
        &result.spread_matrix,
        &support::read_autospread_fixture_text("expected_spread.csv"),
        1e-9,
    );
}

#[test]
fn autospread_writes_single_report() {
    let directory = support::copy_autospread_fixture_set("autospread-debug");
    let debug_dir = directory.join("debug");
    let manifest_path = directory.join("controls.csv");
    let detectors_path = directory.join("detectors.csv");
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
    let detectors = read_detectors_manifest(&detectors_path).expect("detectors should load");
    let config = AutospreadConfig {
        detectors,
        debug_dir: Some(debug_dir.clone()),
        ..AutospreadConfig::default()
    };

    let result = AutospreadRunner::run(&manifest, &directory, &config)
        .expect("autospread with debug report should succeed");

    assert!(debug_dir.join("summary.json").is_file());
    let summary: serde_json::Value =
        serde_json::from_slice(&fs::read(debug_dir.join("summary.json")).unwrap()).unwrap();
    let stats = summary["extra"]["spread_stats"].as_array().unwrap();
    assert_eq!(stats.len(), result.dyes.len());
    for (row, dye) in stats.iter().enumerate() {
        let fits = dye["fits"].as_array().unwrap();
        assert_eq!(fits.len(), result.detectors.len());
        for (column, fit) in fits.iter().enumerate() {
            assert_eq!(fit["detector"], result.detectors[column].as_str());
            // serde_json's default float parser may differ by one ulp.
            assert_close(
                fit["coefficient"].as_f64().unwrap(),
                result.spread_matrix[row][column],
                1e-12,
            );
            let bins = fit["primary_median"].as_array().unwrap().len();
            assert_eq!(fit["secondary_robust_sd"].as_array().unwrap().len(), bins);
            match fit["status"].as_str().unwrap() {
                "fitted" => {
                    assert_eq!(fit["residuals"].as_array().unwrap().len(), bins);
                    assert_close(
                        fit["coefficient"].as_f64().unwrap(),
                        fit["unclamped_coefficient"].as_f64().unwrap().max(0.0),
                        1e-12,
                    );
                }
                "primary" => assert_eq!(result.primary_channels[row], result.detectors[column]),
                other => panic!("unexpected status {other} for a well-populated fixture"),
            }
        }
    }
    let report_path = debug_dir.join("report.html");
    assert!(report_path.is_file(), "missing report.html");
    let report = fs::read_to_string(report_path).expect("report should exist");
    assert!(report.contains("cytiq AutoSpread Report"));
    assert!(report.contains("Spread Matrix"));
    assert!(report.contains("Spread Regression"));
    assert!(report.contains("Gate Statistics"));
}

#[test]
fn autospread_rows_are_dyes_and_columns_are_detectors() {
    // FITC spreads into FL2-A only; PE spreads nowhere. A transposed matrix
    // would put FITC's spread in PE's row.
    let directory = support::create_temp_dir("autospread-orientation");
    let manifest_path = support::write_control_fixture_set(
        &directory,
        &["FITC", "PE"],
        &["FL1-A", "FL2-A"],
        &["FL1-A", "FL2-A", "FL3-A"],
        &[vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]],
        Some(&[vec![0.0, 0.08, 0.0], vec![0.0, 0.0, 0.0]]),
    );
    let detectors_path = support::write_detector_manifest(&directory, &["FL1-A", "FL2-A", "FL3-A"]);
    let manifest = read_controls_manifest(&manifest_path).expect("manifest should load");
    let detectors = read_detectors_manifest(&detectors_path).expect("detectors should load");
    let config = AutospreadConfig {
        detectors,
        ..AutospreadConfig::default()
    };

    let result =
        AutospreadRunner::run(&manifest, &directory, &config).expect("autospread should succeed");

    assert_eq!(result.dyes, ["FITC", "PE"]);
    assert_eq!(result.detectors, ["FL1-A", "FL2-A", "FL3-A"]);
    assert!(
        result.spread_matrix[0][1] > 0.0,
        "{:?}",
        result.spread_matrix
    );
    assert_eq!(result.spread_matrix[0][2], 0.0);
    assert_eq!(result.spread_matrix[1], [0.0, 0.0, 0.0]);
    let output = directory.join("spread.csv");
    cytiq_autospill::export_spread_matrix(&result, &output, cytiq_publish::Existing::Refuse)
        .expect("spread matrix should export");
    let csv = fs::read_to_string(output).unwrap();
    let rows = csv
        .lines()
        .map(|line| line.split(',').next().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(&rows[1..], ["FITC", "PE"], "{csv}");
}

fn assert_close(actual: f64, expected: f64, tolerance: f64) {
    let delta = (actual - expected).abs();
    assert!(
        delta <= tolerance,
        "expected {expected} +/- {tolerance}, got {actual} (|delta|={delta})"
    );
}

fn assert_matrix_csv(actual: &[Vec<f64>], expected_csv: &str, tolerance: f64) {
    let rows = expected_csv
        .lines()
        .skip(1)
        .map(|line| {
            line.split(',')
                .skip(1)
                .map(|value| {
                    value
                        .parse::<f64>()
                        .expect("fixture matrix value should parse")
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual.len(), rows.len());
    for (actual_row, expected_row) in actual.iter().zip(rows) {
        assert_eq!(actual_row.len(), expected_row.len());
        for (actual_value, expected_value) in actual_row.iter().zip(expected_row) {
            assert_close(*actual_value, expected_value, tolerance);
        }
    }
}
