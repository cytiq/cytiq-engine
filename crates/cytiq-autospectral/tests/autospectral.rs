mod support;

use std::error::Error;

use cytiq_autospectral::{
    AutoSpectralConfig, AutoSpectralRunner, read_controls_manifest, read_samples_manifest,
};
use cytiq_fcs::FcsReader;

#[test]
fn runner_unmixes_synthetic_spectral_fixture() -> Result<(), Box<dyn Error>> {
    let directory = support::copy_autospectral_fixture_set("autospectral-runner");
    let controls = read_controls_manifest(directory.join("controls.csv"))?;
    let samples = read_samples_manifest(directory.join("samples.csv"))?;
    let output_dir = directory.join("out");
    // 80 events per control: too few for the KDE gate.
    let config = AutoSpectralConfig {
        output_dir: output_dir.clone(),
        clean_controls: false,
        ..AutoSpectralConfig::new(output_dir)
    };

    let summary = AutoSpectralRunner::run(&controls, &samples, &directory, &config)?;

    assert_eq!(summary.components, vec!["FITC", "PE", "Autofluorescence"]);
    assert_eq!(summary.samples.len(), 1);
    assert!(summary.condition_number.is_finite());
    let sensitivity = summary
        .reference_diagnostics
        .af_peak_sensitivity
        .as_ref()
        .expect("several detectors give a runner-up AF detector");
    assert_ne!(
        sensitivity.runner_up_detector,
        summary.reference_diagnostics.af_peak_detector
    );
    assert!(sensitivity.relative_margin.is_some_and(f64::is_finite));
    let cosine = sensitivity
        .runner_up_spectrum_cosine
        .expect("the fixture AF control has spread on every detector");
    assert!(cosine.is_finite() && cosine <= 1.0 + 1e-12);
    assert_eq!(
        sensitivity.stable,
        Some(cosine >= sensitivity.cosine_threshold)
    );
    let run: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("out/diagnostics/run.json"))?)?;
    assert!(
        run["reference_diagnostics"]["af_peak_sensitivity"]["runner_up_spectrum_cosine"].is_f64()
    );
    assert!(
        directory
            .join("out/references/fluorophore_spectra.csv")
            .is_file()
    );
    assert!(
        directory
            .join("out/references/autospectral-references.json")
            .is_file()
    );
    assert!(directory.join("out/diagnostics/run.json").is_file());
    let unmixed = directory.join("out/unmixed/sample.unmixed.fcs");
    let dataset = FcsReader::open(&unmixed)?.read()?;
    let names = dataset
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["FSC-A", "SSC-A", "FITC", "PE", "Autofluorescence"]
    );

    Ok(())
}

#[test]
fn runner_unmixes_with_explicit_thread_limit() -> Result<(), Box<dyn Error>> {
    let directory = support::copy_autospectral_fixture_set("autospectral-runner-threads");
    let controls = read_controls_manifest(directory.join("controls.csv"))?;
    let samples = read_samples_manifest(directory.join("samples.csv"))?;
    let output_dir = directory.join("out");
    let config = AutoSpectralConfig {
        output_dir: output_dir.clone(),
        clean_controls: false,
        ..AutoSpectralConfig::new(output_dir)
    };

    // The runner uses the caller's pool; the CLI builds it from `--threads`.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let summary =
        pool.install(|| AutoSpectralRunner::run(&controls, &samples, &directory, &config))?;

    assert_eq!(summary.components, vec!["FITC", "PE", "Autofluorescence"]);
    assert_eq!(summary.samples.len(), 1);
    assert!(summary.condition_number.is_finite());

    Ok(())
}

fn strictly_ascending(rows: &[serde_json::Value]) -> Vec<u64> {
    let rows = rows
        .iter()
        .map(|row| row.as_u64().expect("rows are unsigned integers"))
        .collect::<Vec<_>>();
    assert!(rows.windows(2).all(|pair| pair[0] < pair[1]), "{rows:?}");
    rows
}

#[test]
fn debug_dir_exports_reference_events_without_changing_outputs() -> Result<(), Box<dyn Error>> {
    let directory = support::copy_autospectral_fixture_set("autospectral-reference-events");
    let controls = read_controls_manifest(directory.join("controls.csv"))?;
    let samples = read_samples_manifest(directory.join("samples.csv"))?;
    let output_dir = directory.join("out");
    let plain = AutoSpectralConfig {
        output_dir: output_dir.clone(),
        clean_controls: false,
        ..AutoSpectralConfig::new(output_dir.clone())
    };
    AutoSpectralRunner::run(&controls, &samples, &directory, &plain)?;
    // The same output path, so `run.json` records the same sample paths.
    let baseline = directory.join("baseline");
    std::fs::rename(&output_dir, &baseline)?;
    let debug_dir = directory.join("debug");
    let debug = AutoSpectralConfig {
        debug_dir: Some(debug_dir.clone()),
        ..plain
    };
    AutoSpectralRunner::run(&controls, &samples, &directory, &debug)?;

    for file in [
        "references/fluorophore_spectra.csv",
        "references/autospectral-references.json",
        "unmixed/sample.unmixed.fcs",
        "unmixed/sample.unmixed.masks.parquet",
    ] {
        assert_eq!(
            std::fs::read(baseline.join(file))?,
            std::fs::read(output_dir.join(file))?,
            "{file} changed with --debug-dir"
        );
    }
    let read_run = |root: &std::path::Path| -> Result<serde_json::Value, Box<dyn Error>> {
        let mut run: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("diagnostics/run.json"))?)?;
        run.as_object_mut()
            .expect("run.json is an object")
            .remove("run_id");
        Ok(run)
    };
    let run = read_run(&output_dir)?;
    assert_eq!(read_run(&baseline)?, run);

    let events: serde_json::Value = serde_json::from_slice(&std::fs::read(
        debug_dir.join(cytiq_autospectral::REFERENCE_EVENTS_FILE),
    )?)?;
    assert_reference_events(&events, &run);

    Ok(())
}

/// The export's rows agree with `run.json` and with the estimator's selection
/// rules: positives among a control's retained rows, negatives among the
/// AF control's.
fn assert_reference_events(events: &serde_json::Value, run: &serde_json::Value) {
    assert_eq!(events["schema_version"], 1);
    assert_eq!(events["detectors"], run["spectral_channels"]);
    assert_eq!(events["control_gate"], run["control_gate"]);
    let detectors = events["detectors"].as_array().expect("detector list");
    let qc = run["controls"].as_array().expect("control list");
    let af_qc = qc.last().expect("the AF control comes last");

    let af = &events["autofluorescence"];
    assert_eq!(af["dye"], af_qc["dye"]);
    let af_total = af["total_events"].as_u64().expect("event count");
    assert_eq!(af["total_events"], af_qc["total_events"]);
    let af_rows = strictly_ascending(af["retained"].as_array().expect("AF rows"));
    assert_eq!(
        af_rows.len() as u64,
        af_qc["retained_events"].as_u64().unwrap()
    );
    assert!(af_rows.iter().all(|&row| row < af_total));
    let detector =
        |key: &serde_json::Value| &detectors[usize::try_from(key.as_u64().unwrap()).unwrap()];
    let diagnostics = &run["reference_diagnostics"];
    assert_eq!(
        detector(&af["peak_detector"]),
        &diagnostics["af_peak_detector"]
    );
    assert_eq!(
        detector(&af["runner_up_detector"]),
        &diagnostics["af_peak_sensitivity"]["runner_up_detector"]
    );

    let stains = events["single_stains"].as_array().expect("single stains");
    assert_eq!(stains.len(), qc.len() - 1);
    for (stain, control) in stains.iter().zip(qc) {
        assert_eq!(stain["dye"], control["dye"]);
        assert_eq!(stain["filename"], control["filename"]);
        assert_eq!(
            detector(&stain["primary_detector"]),
            &control["primary_channel"]
        );
        let total = stain["total_events"].as_u64().unwrap();
        let retained = strictly_ascending(stain["retained"].as_array().unwrap());
        assert_eq!(
            retained.len() as u64,
            control["retained_events"].as_u64().unwrap()
        );
        assert!(retained.iter().all(|&row| row < total));
        let positive = strictly_ascending(stain["positive"].as_array().unwrap());
        assert_eq!(
            positive.len() as u64,
            control["positive_events"].as_u64().unwrap()
        );
        assert!(
            positive
                .iter()
                .all(|row| retained.binary_search(row).is_ok())
        );
        let negatives = stain["matched_negative"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row.as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            negatives.len() as u64,
            control["negative_events"].as_u64().unwrap()
        );
        let unique = negatives.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), negatives.len());
        assert!(
            negatives
                .iter()
                .all(|row| af_rows.binary_search(row).is_ok())
        );
    }
}
