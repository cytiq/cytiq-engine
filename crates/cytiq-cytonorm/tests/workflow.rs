#![allow(clippy::cast_precision_loss)]
mod support;

use cytiq_cytonorm::{ApplyConfig, CytoNormModel, CytoNormRunner, TrainConfig};
use cytiq_fcs::{FcsReader, SignalState};
use serde_json::Value;

fn configs(root: &std::path::Path, mode: &str) -> (TrainConfig, ApplyConfig) {
    let (manifest, _) = support::design(root, mode);
    (
        TrainConfig {
            manifest,
            output: root.join("model"),
            debug_dir: None,
            overwrite: false,
        },
        ApplyConfig {
            input: root.join("vb.fcs"),
            model: root.join("model"),
            batch: "b".into(),
            output: root.join("normalized.fcs"),
            debug_dir: None,
            overwrite: false,
        },
    )
}

#[test]
fn controls_remove_technical_shift_and_preserve_independent_biology_and_lineage() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (train, apply) = configs(root, "controls");
    CytoNormRunner::train(&train).unwrap();
    CytoNormRunner::apply(&apply).unwrap();
    let output = FcsReader::open(&apply.output).unwrap().read().unwrap();
    let source = FcsReader::open(&apply.input).unwrap().read().unwrap();
    assert_eq!(output.event_count(), source.event_count());
    for i in [2, 3] {
        assert_eq!(
            output.data.columns[i].to_f64_vec(),
            source.data.columns[i].to_f64_vec()
        );
    }
    assert_eq!(
        cytiq_fcs::source_event_ids(&output).unwrap(),
        cytiq_fcs::source_event_ids(&source).unwrap()
    );
    let state = SignalState::of(&output.text).unwrap();
    assert_eq!(state.normalization.unwrap().batch, "b");
    let a = output.data.columns[0].to_f64_vec();
    let b = output.data.columns[1].to_f64_vec();
    // Mean target lies half way between batch shifts. Biological CD4 shift stays +3.
    for i in 0..output.event_count() {
        assert!((a[i] - (source.data.columns[0].to_f64_vec()[i] - 0.5)).abs() < 1e-9);
        assert!((b[i] - (5.5 + (i % 1000) as f64 / 1000.0 + 0.001)).abs() < 1e-8);
    }
    let qc: Value = serde_json::from_reader(
        std::fs::File::open(train.output.join("training-qc.json")).unwrap(),
    )
    .unwrap();
    let mut before = 0.0;
    let mut after = 0.0;
    for row in qc["distributions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|q| q["role"] == "train")
    {
        before += row["emd_quantile_approx_before"].as_f64().unwrap();
        after += row["emd_quantile_approx_after"].as_f64().unwrap();
    }
    assert!(after < before * 0.2, "before={before}, after={after}");
    assert!(
        !cytiq_fcs::dataset_margin_bounds(&output.text, &output.parameters)
            .unwrap()
            .contains_key("A")
    );
}

#[test]
fn aggregate_training_is_deterministic_across_threads_and_does_not_fit_validation() {
    let temp = tempfile::tempdir().unwrap();
    let (train, _) = configs(temp.path(), "aggregate");
    support::write_sample(&temp.path().join("a2.fcs"), 0.0, 0.0, 0.002, false);
    support::write_sample(&temp.path().join("b2.fcs"), 1.0, 0.0, 0.002, false);
    let mut manifest: Value =
        serde_json::from_reader(std::fs::File::open(&train.manifest).unwrap()).unwrap();
    manifest["training_events"] = 1000.into();
    manifest["samples"].as_array_mut().unwrap().extend([
        serde_json::json!({"id":"a2","batch":"a","file":"a2.fcs"}),
        serde_json::json!({"id":"b2","batch":"b","file":"b2.fcs"}),
    ]);
    support::save(&train.manifest, &manifest);
    // One and two worker threads must train the same model.
    let pool = |threads| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    };
    pool(1).install(|| CytoNormRunner::train(&train)).unwrap();
    let first = CytoNormModel::load(&train.output).unwrap();
    support::write_sample(&temp.path().join("vb.fcs"), 1.0, 100.0, 0.005, false);
    let mut second = train.clone();
    second.output = temp.path().join("second");
    pool(2).install(|| CytoNormRunner::train(&second)).unwrap();
    let second = CytoNormModel::load(&second.output).unwrap();
    assert_eq!(first.model_sha256, second.model_sha256);
    assert_eq!(first.som, second.som);
    assert_eq!(first.document.training_inputs.len(), 4);
    assert_eq!(first.document.training_events, 1000);
    let qc: Value = serde_json::from_reader(
        std::fs::File::open(train.output.join("training-qc.json")).unwrap(),
    )
    .unwrap();
    assert!(
        qc["sampling"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["role"] == "train")
            .all(|s| s["selected_events"] == 250)
    );
}

#[test]
fn rejects_unknown_batches_reprocessing_transform_mismatch_and_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let (train, apply) = configs(temp.path(), "controls");
    CytoNormRunner::train(&train).unwrap();
    let mut invalid = apply.clone();
    invalid.batch = "new".into();
    assert!(CytoNormRunner::apply(&invalid).is_err());
    assert!(!invalid.output.exists());
    CytoNormRunner::apply(&apply).unwrap();
    let mut twice = apply.clone();
    twice.input = apply.output.clone();
    twice.output = temp.path().join("twice.fcs");
    assert!(CytoNormRunner::apply(&twice).is_err());
    assert!(!twice.output.exists());
    let mut transform = cytiq_transform_config(&apply.output, &temp.path().join("retransform.fcs"));
    transform.allow_retransform = true;
    assert!(cytiq_transform::TransformRunner::run(&transform).is_err());
    std::fs::write(train.output.join("som.json"), "{}").unwrap();
    assert!(CytoNormModel::load(&train.output).is_err());
}

fn cytiq_transform_config(
    input: &std::path::Path,
    output: &std::path::Path,
) -> cytiq_transform::TransformConfig {
    cytiq_transform::TransformConfig::new(vec![input.to_path_buf()], output.to_path_buf())
}

#[test]
fn sparse_mappings_require_explicit_identity_and_reference_target_is_supported() {
    let temp = tempfile::tempdir().unwrap();
    let (train, _) = configs(temp.path(), "controls");
    support::write_sample(&temp.path().join("b.fcs"), 0.2, 0.0, 0.0, true);
    assert!(CytoNormRunner::train(&train).is_err());
    assert!(!train.output.exists());
    let mut manifest: Value =
        serde_json::from_reader(std::fs::File::open(&train.manifest).unwrap()).unwrap();
    manifest["sparse_policy"] = "identity".into();
    manifest["target"] = serde_json::json!({"kind":"batch","batch":"a"});
    support::save(&train.manifest, &manifest);
    CytoNormRunner::train(&train).unwrap();
    let model = CytoNormModel::load(&train.output).unwrap();
    assert!(model.mappings.iter().any(|m| m.identity_reason.is_some()));
    for m in model.mappings.iter().filter(|m| m.batch == "a") {
        if let Some(s) = &m.spline {
            for &x in &s.x {
                assert!((s.evaluate(x) - x).abs() < 1e-12);
            }
        }
    }
}

#[test]
fn rejects_train_validation_acquisition_overlap_and_unknown_manifest_fields() {
    let temp = tempfile::tempdir().unwrap();
    let (train, _) = configs(temp.path(), "controls");
    std::fs::copy(temp.path().join("a.fcs"), temp.path().join("va.fcs")).unwrap();
    let error = CytoNormRunner::train(&train).unwrap_err().to_string();
    assert!(error.contains("overlap"), "{error}");
    let mut manifest: Value =
        serde_json::from_reader(std::fs::File::open(&train.manifest).unwrap()).unwrap();
    manifest["typo"] = true.into();
    support::save(&train.manifest, &manifest);
    assert!(cytiq_cytonorm::Manifest::load(&train.manifest).is_err());
}

#[test]
fn masks_limit_training_and_transform_contracts_are_checked_before_output() {
    let temp = tempfile::tempdir().unwrap();
    let (train, apply) = configs(temp.path(), "controls");
    let a = FcsReader::open(temp.path().join("a.fcs"))
        .unwrap()
        .read()
        .unwrap();
    let acquisition = cytiq_fcs::acquisition_id(&temp.path().join("a.fcs"), &a.text).unwrap();
    let keep = (0..a.event_count()).map(|i| i % 3 != 0).collect();
    let mask = temp.path().join("mask.parquet");
    cytiq_mask::EventMask::new(acquisition, keep, None)
        .write_parquet(&mask)
        .unwrap();
    let mut manifest: Value =
        serde_json::from_reader(std::fs::File::open(&train.manifest).unwrap()).unwrap();
    manifest["samples"][0]["masks"] = serde_json::json!(["mask.parquet"]);
    support::save(&train.manifest, &manifest);
    let result = CytoNormRunner::train(&train).unwrap();
    assert_eq!(result.events, 3333);
    let dataset = FcsReader::open(&apply.input).unwrap().read().unwrap();
    let mut keywords = cytiq_fcs::carried_keywords(&dataset.text);
    let (_, transform) = keywords
        .iter_mut()
        .find(|(k, _)| k == "CYTIQ_TRANSFORM")
        .unwrap();
    let mut doc: Value = serde_json::from_str(transform).unwrap();
    doc["parameters"]["cofactor"] = 100.0.into();
    *transform = doc.to_string();
    let columns = dataset
        .data
        .columns
        .iter()
        .map(cytiq_fcs::ColumnData::to_f64_vec)
        .collect::<Vec<_>>();
    let parameters = dataset
        .parameters
        .iter()
        .map(cytiq_fcs::FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    cytiq_fcs::write_fcs3_1_exact_with_options(
        &apply.input,
        &parameters,
        &columns,
        &cytiq_fcs::FcsWriteOptions::new().with_text_keywords(keywords),
    )
    .unwrap();
    assert!(CytoNormRunner::apply(&apply).is_err());
    assert!(!apply.output.exists());
}

#[test]
fn retains_a_point_two_percent_population_with_enough_reference_events() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (train, mut apply) = configs(root, "controls");
    let template = FcsReader::open(root.join("a.fcs")).unwrap().read().unwrap();
    let parameters = template
        .parameters
        .iter()
        .map(cytiq_fcs::FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    for (name, shift) in [("a.fcs", 0.0), ("b.fcs", 1.0)] {
        let columns=vec![
            (0..60_000).map(|i|if i<29_940 {0.0}else if i<59_880 {10.0}else{100.0} + f64::from(i%1000)/1000.0+shift).collect(),
            (0..60_000).map(|i|2.0+f64::from(i%1000)/1000.0+shift).collect(),
            (0..60_000).map(f64::from).collect(),(1..=60_000).map(f64::from).collect(),
        ];
        cytiq_fcs::write_fcs3_1_exact_with_options(
            root.join(name),
            &parameters,
            &columns,
            &cytiq_fcs::FcsWriteOptions::new()
                .with_text_keywords(cytiq_fcs::carried_keywords(&template.text)),
        )
        .unwrap();
    }
    let mut manifest: Value =
        serde_json::from_reader(std::fs::File::open(&train.manifest).unwrap()).unwrap();
    manifest["samples"]
        .as_array_mut()
        .unwrap()
        .retain(|s| s["role"] != "validation");
    manifest["training_events"] = 120_000.into();
    manifest["som"] = serde_json::json!({"xdim":3,"ydim":3,"metaclusters":3});
    support::save(&train.manifest, &manifest);
    CytoNormRunner::train(&train).unwrap();
    let qc: Value = serde_json::from_reader(
        std::fs::File::open(train.output.join("training-qc.json")).unwrap(),
    )
    .unwrap();
    for id in ["a", "b"] {
        assert!(
            qc["occupancy"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["sample"] == id && r["events"] == 120)
        );
    }
    apply.input = root.join("b.fcs");
    CytoNormRunner::apply(&apply).unwrap();
    let data = FcsReader::open(&apply.output).unwrap().read().unwrap();
    assert_eq!(data.event_count(), 60_000);
    let values = data.data.columns[0].to_f64_vec();
    for (i, &value) in values.iter().enumerate().skip(59_880) {
        assert!((value - (100.5 + (i % 1000) as f64 / 1000.0)).abs() < 1e-8);
    }
}

#[test]
fn accepts_reordered_channels_and_rejects_a_superset_panel() {
    let temp = tempfile::tempdir().unwrap();
    let (train, apply) = configs(temp.path(), "controls");
    CytoNormRunner::train(&train).unwrap();
    let data = FcsReader::open(&apply.input).unwrap().read().unwrap();
    let order = [3, 1, 2, 0];
    let mut parameters = order
        .iter()
        .map(|&i| cytiq_fcs::FcsWriteParameter::from_source(&data.parameters[i]))
        .collect::<Vec<_>>();
    let mut columns = order
        .iter()
        .map(|&i| data.data.columns[i].to_f64_vec())
        .collect::<Vec<_>>();
    let options = cytiq_fcs::FcsWriteOptions::new()
        .with_text_keywords(cytiq_fcs::carried_keywords(&data.text));
    cytiq_fcs::write_fcs3_1_exact_with_options(&apply.input, &parameters, &columns, &options)
        .unwrap();
    CytoNormRunner::apply(&apply).unwrap();
    let result = FcsReader::open(&apply.output).unwrap().read().unwrap();
    assert_eq!(result.parameters[0].name, "Original_ID");
    assert_eq!(result.data.columns[0].to_f64_vec(), columns[0]);
    parameters.push(cytiq_fcs::FcsWriteParameter::with_label("C", "CD19"));
    columns.push(vec![1.0; data.event_count()]);
    cytiq_fcs::write_fcs3_1_exact_with_options(&apply.input, &parameters, &columns, &options)
        .unwrap();
    let mut wrong = apply.clone();
    wrong.output = temp.path().join("wrong.fcs");
    assert!(
        CytoNormRunner::apply(&wrong)
            .unwrap_err()
            .to_string()
            .contains("outside the declared panel")
    );
    assert!(!wrong.output.exists());
}

#[test]
fn overwrite_refuses_files_and_directories_without_a_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let (mut train, _) = configs(temp.path(), "controls");
    train.overwrite = true;
    std::fs::write(&train.output, b"protected").unwrap();
    assert!(
        CytoNormRunner::train(&train)
            .unwrap_err()
            .to_string()
            .contains("overwrite requires an existing CytoNorm bundle directory")
    );
    assert_eq!(std::fs::read(&train.output).unwrap(), b"protected");
    std::fs::remove_file(&train.output).unwrap();
    std::fs::create_dir(&train.output).unwrap();
    let sentinel = train.output.join("sentinel");
    std::fs::write(&sentinel, b"protected").unwrap();
    assert!(
        CytoNormRunner::train(&train)
            .unwrap_err()
            .to_string()
            .contains("overwrite requires an existing CytoNorm bundle directory")
    );
    assert_eq!(std::fs::read(sentinel).unwrap(), b"protected");
    assert_eq!(std::fs::read_dir(&train.output).unwrap().count(), 1);
}

#[test]
fn training_reports_only_fitted_events_and_publishes_debug_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let (mut train, _) = configs(temp.path(), "controls");
    let mut manifest: Value =
        serde_json::from_reader(std::fs::File::open(&train.manifest).unwrap()).unwrap();
    manifest["samples"]
        .as_array_mut()
        .unwrap()
        .retain(|sample| sample["role"] != "validation");
    support::save(&train.manifest, &manifest);
    let debug = temp.path().join("debug");
    train.debug_dir = Some(debug.clone());
    let result = CytoNormRunner::train(&train).unwrap();
    assert_eq!(result.events, 4000);
    assert_eq!(result.outputs.len(), 6);
    assert!(result.outputs.contains(&debug.join("report.html")));
    assert!(result.outputs.iter().all(|path| path.is_file()));
    let model = CytoNormModel::load(&train.output).unwrap();
    assert_eq!(result.model_sha256, model.model_sha256);
    train.overwrite = true;
    let repeated = CytoNormRunner::train(&train).unwrap();
    assert_eq!(repeated.events, 4000);
    assert_eq!(repeated.model_sha256, result.model_sha256);
}
