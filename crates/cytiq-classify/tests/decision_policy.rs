#[path = "support/decision.rs"]
mod decision_support;
use decision_support::policy;
// Software-contract fixtures only; these reports do not validate a biological operating point.
use arrow_array::{Array, BooleanArray, Float32Array, StringArray};
use cytiq_classify::{ClassifyConfig, ClassifyRunner, DecisionPolicy};
use cytiq_population::{ArtifactRef, serialization};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures")
        .join(name)
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn config(model: &str, input: &str, output: PathBuf) -> ClassifyConfig {
    ClassifyConfig {
        input: fixture(input),
        model: fixture(model),
        output,
        masks: vec![],
        threads: Some(1),
        batch_size: 17,
        abstain_margin: 0.0,
        overwrite: false,
    }
}

fn read_predictions(path: PathBuf) -> arrow_array::RecordBatch {
    ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap())
        .unwrap()
        .with_batch_size(4096)
        .build()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
}

#[test]
fn explicit_abstention_preserves_probabilities_and_existing_bundle_versions() {
    let temp = tempfile::tempdir().unwrap();
    for (index, (model, input)) in [
        ("classify/catboost", "classify/raw.fcs"),
        ("classify/v2/binary", "classify/raw.fcs"),
        ("classify/v2/catboost", "classify/v2/input.fcs"),
        ("classify/v4/catboost", "classify/v2/input.fcs"),
        ("classify/v2/catboost", "classify/v2/input.fcs"),
    ]
    .into_iter()
    .enumerate()
    {
        let root = temp.path().join(index.to_string());
        fs::create_dir(&root).unwrap();
        let mut cfg = config(model, input, root.join("legacy"));
        if index >= 3 {
            let copied = root.join("model");
            fs::create_dir(&copied).unwrap();
            for entry in fs::read_dir(&cfg.model).unwrap() {
                let entry = entry.unwrap();
                fs::copy(entry.path(), copied.join(entry.file_name())).unwrap();
            }
            if index == 3 {
                fs::copy(
                    fixture("classify/v2/catboost/model.onnx"),
                    copied.join("model.onnx"),
                )
                .unwrap();
            } else {
                let mut manifest = serialization::read(&copied.join("manifest.json")).unwrap();
                manifest["schema_version"] = json!(3);
                manifest["normalization"] = json!({"kind":"none"});
                write(&copied.join("manifest.json"), &manifest);
            }
            cfg.model = copied;
        }
        let baseline = ClassifyRunner::run(&cfg).unwrap();
        let path = policy(&root, &cfg.model, Some(1.0), None);
        cfg.output = root.join("selective");
        let result = ClassifyRunner::run_with_policy(&cfg, Some(&path)).unwrap();
        assert_eq!(result.assessed, baseline.assessed);
        assert_eq!(result.schema_version, 5);
        assert!(result.decision.as_ref().unwrap().abstained > 0);
        let old = read_predictions(root.join("legacy/predictions.parquet"));
        let new = read_predictions(root.join("selective/predictions.parquet"));
        for (field, column) in old.schema().fields().iter().zip(old.columns()) {
            if field.name().starts_with("p_") {
                assert_eq!(
                    column.to_data(),
                    new.column_by_name(field.name()).unwrap().to_data()
                );
            }
        }
        let statuses = new
            .column_by_name("decision_status")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let confidence = new
            .column_by_name("confidence")
            .unwrap()
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        let labels = new.column_by_name("label").unwrap();
        let ood = new.column_by_name("is_ood").unwrap();
        for row in 0..new.num_rows() {
            let expected = old
                .schema()
                .fields()
                .iter()
                .zip(old.columns())
                .filter(|(field, _)| field.name().starts_with("p_"))
                .map(|(_, column)| {
                    column
                        .as_any()
                        .downcast_ref::<Float32Array>()
                        .unwrap()
                        .value(row)
                })
                .fold(0.0_f32, f32::max);
            assert_eq!(confidence.value(row), expected);
            assert_eq!(labels.is_null(row), statuses.value(row) == "abstained");
            assert!(ood.is_null(row));
        }
        cfg.output = root.join("streamed");
        cfg.batch_size = 1;
        cfg.threads = Some(2);
        let streamed = ClassifyRunner::run_with_policy(&cfg, Some(&path)).unwrap();
        assert_eq!(
            streamed.decision.unwrap().abstained,
            result.decision.unwrap().abstained
        );
    }
}

#[test]
fn confident_predictions_can_be_out_of_distribution_and_bad_scores_publish_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(
        "classify/v2/catboost",
        "classify/v2/input.fcs",
        temp.path().join("out"),
    );
    let scorer = fixture("classify_safety/mean-ood.onnx");
    let path = policy(temp.path(), &cfg.model, None, Some(&scorer));
    let result = ClassifyRunner::run_with_policy(&cfg, Some(&path)).unwrap();
    assert!(result.decision.as_ref().unwrap().ood_events > 0);
    let mut confident_ood = false;
    for batch in ParquetRecordBatchReaderBuilder::try_new(
        File::open(cfg.output.join("predictions.parquet")).unwrap(),
    )
    .unwrap()
    .build()
    .unwrap()
    {
        let batch = batch.unwrap();
        let confidence = batch
            .column_by_name("confidence")
            .unwrap()
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        let ood = batch
            .column_by_name("is_ood")
            .unwrap()
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        for row in 0..batch.num_rows() {
            confident_ood |= confidence.value(row) > 0.9 && ood.value(row);
        }
    }
    assert!(confident_ood);
    let invalid = fixture("classify_safety/nonfinite-ood.onnx");
    let path = policy(temp.path(), &cfg.model, None, Some(&invalid));
    cfg.output = temp.path().join("invalid");
    let error = ClassifyRunner::run_with_policy(&cfg, Some(&path)).unwrap_err();
    assert!(
        error.to_string().contains("invalid ONNX OOD scores"),
        "{error}"
    );
    assert!(!cfg.output.join("predictions.parquet").exists());
    assert!(!cfg.output.join("summary.json").exists());
}

#[test]
fn excluded_events_remain_null_and_do_not_claim_ood_assessment() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(
        "classify/v2/catboost",
        "classify/v2/input.fcs",
        temp.path().join("out"),
    );
    let data = cytiq_fcs::FcsReader::open(&cfg.input)
        .unwrap()
        .read()
        .unwrap();
    let acquisition = cytiq_fcs::acquisition_id(&cfg.input, &data.text).unwrap();
    let mask = temp.path().join("mask.parquet");
    cytiq_mask::EventMask::new(acquisition, vec![false; data.event_count()], None)
        .write_parquet(&mask)
        .unwrap();
    cfg.masks.push(mask);
    cfg.batch_size = 1;
    let path = policy(
        temp.path(),
        &cfg.model,
        None,
        Some(&fixture("classify_safety/mean-ood.onnx")),
    );
    let result = ClassifyRunner::run_with_policy(&cfg, Some(&path)).unwrap();
    assert_eq!(result.assessed, 0);
    assert!(!result.decision.unwrap().ood_assessed);
    let mut rows = 0;
    for batch in ParquetRecordBatchReaderBuilder::try_new(
        File::open(cfg.output.join("predictions.parquet")).unwrap(),
    )
    .unwrap()
    .build()
    .unwrap()
    {
        let batch = batch.unwrap();
        rows += batch.num_rows();
        for name in ["label", "confidence", "uncertainty", "ood_score", "is_ood"] {
            assert_eq!(
                batch.column_by_name(name).unwrap().null_count(),
                batch.num_rows()
            );
        }
        let status = batch
            .column_by_name("decision_status")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(status.iter().all(|s| s == Some("not_assessed")));
    }
    assert_eq!(rows, data.event_count());
}

#[test]
fn overlapping_groups_failed_limits_changed_evidence_and_wrong_bundles_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(
        "classify/v2/catboost",
        "classify/v2/input.fcs",
        temp.path().join("out"),
    );
    for alteration in ["overlap", "risk", "identity", "checksum"] {
        let path = policy(temp.path(), &cfg.model, Some(0.9), None);
        let mut value = serialization::read(&path).unwrap();
        let validation = temp.path().join("validation.json");
        let mut report = serialization::read(&validation).unwrap();
        match alteration {
            "overlap" => report["groups"]["test"] = report["groups"]["train"].clone(),
            "risk" => report["metrics"]["risk"] = json!(0.8),
            "identity" => report["policy_sha256"] = json!("0".repeat(64)),
            _ => {}
        }
        write(&validation, &report);
        if alteration == "checksum" {
            fs::write(&validation, b"{}").unwrap();
        } else {
            value["validation"] =
                serde_json::to_value(ArtifactRef::of(&validation).unwrap()).unwrap();
            write(&path, &value);
        }
        assert!(DecisionPolicy::load(&path).is_err(), "{alteration}");
        assert!(ClassifyRunner::run_with_policy(&cfg, Some(&path)).is_err());
        assert!(!cfg.output.join("predictions.parquet").exists());
    }
    let path = policy(temp.path(), &cfg.model, Some(0.9), None);
    let policy = DecisionPolicy::load(&path).unwrap();
    assert!(
        policy
            .check_bundle(&fixture("classify/v2/lightgbm/manifest.json"))
            .is_err()
    );
}
