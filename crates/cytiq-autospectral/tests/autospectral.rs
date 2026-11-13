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
