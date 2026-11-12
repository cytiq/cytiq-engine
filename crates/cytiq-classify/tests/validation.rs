#![allow(clippy::cast_possible_truncation)]
use std::fs::{self, File};
use std::io::Cursor;
use std::path::PathBuf;

use cytiq_classify::{Bundle, ClassifyConfig, ClassifyError, ClassifyRunner, feature_columns};
use cytiq_fcs::{FcsDataset, FcsReader, FcsWriteOptions, FcsWriteParameter};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/classify")
}

/// `data` rewritten with `key` set to `value`.
fn with_keyword(data: &FcsDataset, key: &str, value: &str) -> FcsDataset {
    let mut pairs = cytiq_fcs::carried_keywords(&data.text);
    pairs.retain(|(name, _)| name != key);
    pairs.push((key.to_owned(), value.to_owned()));
    let parameters = data
        .parameters
        .iter()
        .map(FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    let columns = data
        .data
        .columns
        .iter()
        .map(|c| {
            c.to_f64_vec()
                .into_iter()
                .map(|v| v as f32)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    cytiq_fcs::write_fcs3_1_f32_to_writer_with_options(
        &mut bytes,
        &parameters,
        &columns,
        &FcsWriteOptions::new().with_text_keywords(pairs),
    )
    .unwrap();
    FcsReader::new(Cursor::new(bytes)).read().unwrap()
}

#[test]
fn raw_features_require_the_matrix_to_cover_every_fluorescence_channel() {
    let root = fixtures();
    let profile = Bundle::load(&root.join("catboost"), 0.0).unwrap().profile;
    let data = FcsReader::open(root.join("raw.fcs"))
        .unwrap()
        .read()
        .unwrap();
    let matrix_key = data
        .text
        .iter()
        .find(|(key, _)| cytiq_fcs::is_spillover_keyword(key))
        .unwrap()
        .0
        .to_owned();
    let fluorescence = profile
        .features
        .iter()
        .filter(|feature| feature.fluorescence)
        .map(|feature| feature.channel.as_str())
        .collect::<Vec<_>>();
    assert!(fluorescence.len() > 2);
    let partial = format!("2,{},{},1,0,0,1", fluorescence[0], fluorescence[1]);
    let incomplete = with_keyword(&data, &matrix_key, &partial);
    assert!(
        matches!(feature_columns(&incomplete, &profile), Err(ClassifyError::Invalid(reason)) if reason == "incomplete spillover matrix")
    );
}

#[test]
fn existing_outputs_are_rejected_before_input_loading_and_overwrite_is_explicit() {
    let root = fixtures();
    for name in ["predictions.parquet", "summary.json", "provenance.json"] {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("out");
        fs::create_dir(&output).unwrap();
        fs::write(output.join(name), b"protected").unwrap();
        let mut config = ClassifyConfig {
            input: root.join("missing.fcs"),
            model: root.join("catboost"),
            output: output.clone(),
            masks: vec![],
            threads: Some(1),
            batch_size: 2,
            abstain_margin: 0.0,
            overwrite: false,
        };
        assert!(matches!(
            ClassifyRunner::run(&config),
            Err(ClassifyError::Publish(cytiq_publish::PublishError::Exists { path }))
                if path == output.join(name)
        ));
        assert_eq!(fs::read(output.join(name)).unwrap(), b"protected");
        assert_eq!(fs::read_dir(&output).unwrap().count(), 1);
        config.overwrite = true;
        config.input = root.join("raw.fcs");
        let result = ClassifyRunner::run(&config).unwrap();
        assert!(result.assessed > 0);
        let summary: serde_json::Value =
            serde_json::from_reader(File::open(output.join("summary.json")).unwrap()).unwrap();
        assert_eq!(summary["assessed"], result.assessed);
    }
}
