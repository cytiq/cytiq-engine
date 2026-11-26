mod common;
#[path = "../../cytiq-autospectral/tests/support.rs"]
mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use cytiq_fcs::FcsReader;

#[test]
#[allow(clippy::too_many_lines)]
fn cli_runs_autospectral() {
    let directory = support::copy_autospectral_fixture_set("cytiq-engine-autospectral");
    let output_dir = directory.join("out");
    let debug_dir = directory.join("debug");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "autospectral",
            "--controls",
            utf8(&directory.join("controls.csv")),
            "--samples",
            utf8(&directory.join("samples.csv")),
            "--input-dir",
            utf8(&directory),
            "--output",
            utf8(&output_dir),
            "--debug-dir",
            utf8(&debug_dir),
            "--threads",
            "1",
            // 80 events per control: too few for the default KDE gate.
            "--quantile-gate",
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output_dir
            .join("references/fluorophore_spectra.csv")
            .is_file()
    );
    assert!(
        output_dir
            .join("references/autospectral-references.json")
            .is_file()
    );
    assert!(output_dir.join("diagnostics/run.json").is_file());
    assert!(debug_dir.join("summary.json").is_file());
    assert!(debug_dir.join("reference_events.json").is_file());
    let unmixed = output_dir.join("unmixed/sample.unmixed.fcs");
    let dataset = FcsReader::open(&unmixed)
        .and_then(FcsReader::read)
        .expect("unmixed FCS should be readable");
    let names = dataset
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["FSC-A", "SSC-A", "FITC", "PE", "Autofluorescence"]
    );

    let state = cytiq_fcs::unmixing_state(&dataset.text)
        .expect("unmixing state should parse")
        .expect("unmixing should be recorded");
    assert_eq!(state.components, ["FITC", "PE", "Autofluorescence"]);
    let margins = cytiq_fcs::recorded_margin_bounds(&dataset.text)
        .expect("margins should parse")
        .expect("margins of passed-through channels should be recorded");
    assert!(margins.contains_key("FSC-A") && margins.contains_key("SSC-A"));
    assert!(!margins.contains_key("FITC"));
    let mask = cytiq_mask::EventMask::read_parquet(
        &output_dir.join("unmixed/sample.unmixed.masks.parquet"),
    )
    .expect("raw margin mask should be written");
    assert_eq!(mask.len(), dataset.event_count());
    assert_eq!(mask.filters[0].0, "margin_pass");
    let run: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(output_dir.join("diagnostics/run.json")).expect("run.json"),
    )
    .expect("run.json should be JSON");
    assert_eq!(run["solver_method"], "householder_qr");
    assert_eq!(run["control_gate"], "fsc_ssc_quantile_box");
    common::assert_provenance(
        &output_dir.join("provenance.json"),
        "cytiq autospectral",
        &[
            &directory.join("controls.csv"),
            &directory.join("samples.csv"),
            &directory.join("sample.fcs"),
        ],
        &[&unmixed, &output_dir.join("diagnostics/run.json")],
    );

    // An unmixed file is never a raw layer of its acquisition.
    let raw = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "export",
            "events",
            "--input",
            utf8(&unmixed),
            "--output",
            utf8(&directory.join("raw.parquet")),
            "--layer",
            "raw",
            "--unmixing-reference",
            utf8(&output_dir.join("references/autospectral-references.json")),
        ])
        .output()
        .expect("command should run");
    assert_eq!(raw.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&raw.stderr);
    assert!(stderr.contains("not a raw one"), "stderr: {stderr}");

    // Unmixed abundances have no detector margins.
    let peacoqc = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "peacoqc",
            "--input",
            utf8(&unmixed),
            "--output",
            utf8(&directory.join("peacoqc.fcs")),
            "--channels",
            "FITC",
            "--remove-margins",
        ])
        .output()
        .expect("command should run");
    assert!(!peacoqc.status.success());
    let stderr = String::from_utf8_lossy(&peacoqc.stderr);
    assert!(
        stderr.contains("margin bounds of channel `FITC`"),
        "stderr: {stderr}"
    );
}

#[test]
fn cli_fails_when_autospectral_manifest_has_no_autofluorescence() {
    let directory = support::create_temp_dir("cytiq-engine-autospectral-no-af");
    fs::write(
        directory.join("controls.csv"),
        "filename,dye\nfitc.fcs,FITC\n",
    )
    .expect("manifest should write");
    fs::write(directory.join("samples.csv"), "filename\nsample.fcs\n")
        .expect("manifest should write");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "autospectral",
            "--controls",
            utf8(&directory.join("controls.csv")),
            "--samples",
            utf8(&directory.join("samples.csv")),
            "--input-dir",
            utf8(&directory),
            "--output",
            utf8(&directory.join("out")),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Autofluorescence"));
}

#[test]
fn cli_rejects_zero_threads_for_autospectral() {
    let directory = support::create_temp_dir("cytiq-engine-autospectral-zero-threads");
    fs::write(
        directory.join("controls.csv"),
        "filename,dye\nfitc.fcs,FITC\n",
    )
    .expect("manifest should write");
    fs::write(directory.join("samples.csv"), "filename\nsample.fcs\n")
        .expect("manifest should write");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "autospectral",
            "--controls",
            utf8(&directory.join("controls.csv")),
            "--samples",
            utf8(&directory.join("samples.csv")),
            "--input-dir",
            utf8(&directory),
            "--output",
            utf8(&directory.join("out")),
            "--threads",
            "0",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("thread count must be greater"));
}

#[test]
fn preserves_large_original_ids_exactly() {
    let directory = support::copy_autospectral_fixture_set("cytiq-engine-autospectral-original-id");
    let sample = directory.join("sample.fcs");
    let source = directory.join("source.fcs");
    fs::rename(&sample, &source).expect("fixture should move");
    let ids = common::write_with_large_original_ids(&source, &sample);
    let output_dir = directory.join("out");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "autospectral",
            "--controls",
            utf8(&directory.join("controls.csv")),
            "--samples",
            utf8(&directory.join("samples.csv")),
            "--input-dir",
            utf8(&directory),
            "--output",
            utf8(&output_dir),
            "--threads",
            "1",
            "--quantile-gate",
        ])
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let unmixed = output_dir.join("unmixed/sample.unmixed.fcs");
    let dataset = FcsReader::open(&unmixed)
        .and_then(FcsReader::read)
        .expect("unmixed FCS should be readable");
    let index = dataset.find_parameter("Original_ID").expect("lineage");
    let Some(cytiq_fcs::ColumnData::F64(written)) = dataset.data.column(index) else {
        panic!("identifiers beyond 2^24 need a double column");
    };
    assert_eq!(written, &ids);
    let state = cytiq_fcs::unmixing_state(&dataset.text)
        .expect("unmixing state should parse")
        .expect("unmixing should be recorded");
    assert!(!state.detectors.iter().any(|name| name == "Original_ID"));
    assert_eq!(
        common::acquisition_of(&unmixed),
        common::sha256_hex(&sample)
    );
    let mask = cytiq_mask::EventMask::read_parquet(
        &output_dir.join("unmixed/sample.unmixed.masks.parquet"),
    )
    .expect("mask should be written");
    assert_eq!(mask.source_event_ids[0], 16_777_216);
}

#[test]
fn cli_rejects_processed_autospectral_controls() {
    let directory = support::copy_autospectral_fixture_set("cytiq-engine-autospectral-processed");
    common::add_keyword(
        &directory.join("fitc.fcs"),
        cytiq_fcs::TRANSFORM_KEYWORD,
        &common::transform_keyword(&[]),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "autospectral",
            "--controls",
            utf8(&directory.join("controls.csv")),
            "--samples",
            utf8(&directory.join("samples.csv")),
            "--input-dir",
            utf8(&directory),
            "--output",
            utf8(&directory.join("out")),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fitc.fcs") && stderr.contains("unmixes raw acquisition files"),
        "stderr: {stderr}"
    );
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be valid utf-8")
}

#[test]
fn cleaning_controls_by_default_reports_a_degenerate_scatter_gate() {
    // The fixture controls hold 80 events, too few for the KDE scatter gate
    // that cleans controls by default; `--clean-controls` asks for it too.
    let directory = support::copy_autospectral_fixture_set("cytiq-engine-autospectral-clean");
    for flags in [&[][..], &["--clean-controls"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
            .args([
                "autospectral",
                "--controls",
                utf8(&directory.join("controls.csv")),
                "--samples",
                utf8(&directory.join("samples.csv")),
                "--input-dir",
                utf8(&directory),
                "--output",
                utf8(&directory.join("out")),
            ])
            .args(flags)
            .output()
            .expect("command should run");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("FSC/SSC gate of control") && stderr.contains("failed"),
            "stderr: {stderr}"
        );
    }

    let conflicting = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "autospectral",
            "--controls",
            utf8(&directory.join("controls.csv")),
            "--samples",
            utf8(&directory.join("samples.csv")),
            "--input-dir",
            utf8(&directory),
            "--output",
            utf8(&directory.join("out")),
            "--clean-controls",
            "--quantile-gate",
        ])
        .output()
        .expect("command should run");
    assert!(!conflicting.status.success());
}

#[test]
fn cli_rejects_log_amplified_autospectral_controls() {
    let directory = support::copy_autospectral_fixture_set("cytiq-engine-autospectral-log");
    common::set_log_amplified(&directory.join("fitc.fcs"), "FITC");

    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "autospectral",
            "--controls",
            utf8(&directory.join("controls.csv")),
            "--samples",
            utf8(&directory.join("samples.csv")),
            "--input-dir",
            utf8(&directory),
            "--output",
            utf8(&directory.join("out")),
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fitc.fcs") && stderr.contains("`FITC` store log-amplified values"),
        "stderr: {stderr}"
    );
}
