//! Pins every prediction and summary across the fixture bundles, batch sizes
//! and masks, so classify refactors can prove they change nothing.
//!
//! Within one run, outputs must be bit-identical across batch sizes. Against
//! the committed `data/fixtures/characterization/classify.json`, everything but
//! the probabilities must match exactly; probabilities must match within
//! `PROBABILITY_TOLERANCE`, because ONNX Runtime picks its kernels by CPU and
//! so differs between machines in the last bits. Run with `CYTIQ_BLESS=1` to
//! rewrite the golden after an intended change.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use arrow_array::{Array, BooleanArray, Float32Array, StringArray, UInt64Array};
use cytiq_classify::{ClassifyConfig, ClassifyRunner};
use cytiq_fcs::FcsReader;
use cytiq_mask::EventMask;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// (case name, bundle directory, input FCS), relative to the classify fixtures.
const CASES: &[(&str, &str, &str)] = &[
    ("v1-catboost", "catboost", "raw.fcs"),
    ("v1-lightgbm", "lightgbm", "raw.fcs"),
    ("v2-binary", "v2/binary", "raw.fcs"),
    ("v2-catboost", "v2/catboost", "v2/input.fcs"),
    ("v2-lightgbm", "v2/lightgbm", "v2/input.fcs"),
    ("cytof-lightgbm", "cytof/lightgbm", "cytof/input.fcs"),
    ("cytof-xgboost", "cytof/xgboost", "cytof/input.fcs"),
];
const BATCH_SIZES: &[usize] = &[1, 17, 65_536];
const PROBABILITY_TOLERANCE: f32 = 1e-6;

/// One classify output: a digest of everything except the float32 probability
/// columns, and those columns' values (`None` for excluded events).
#[derive(Debug, Serialize, Deserialize)]
struct Outputs {
    exact: String,
    probabilities: Vec<Vec<Option<f32>>>,
}

impl Outputs {
    fn bits(&self) -> Vec<Vec<Option<u32>>> {
        self.probabilities
            .iter()
            .map(|column| column.iter().map(|value| value.map(f32::to_bits)).collect())
            .collect()
    }

    /// Why `self` differs from `golden`, or `None` when it matches.
    fn mismatch(&self, golden: &Outputs) -> Option<String> {
        if self.exact != golden.exact {
            return Some("ids, labels, schema or summary changed".to_owned());
        }
        if self.probabilities.len() != golden.probabilities.len() {
            return Some("number of probability columns changed".to_owned());
        }
        for (index, (column, expected)) in self
            .probabilities
            .iter()
            .zip(&golden.probabilities)
            .enumerate()
        {
            if column.len() != expected.len() {
                return Some(format!("probability column {index} changed length"));
            }
            for (row, (value, expected)) in column.iter().zip(expected).enumerate() {
                let close = match (value, expected) {
                    (Some(value), Some(expected)) => {
                        (value - expected).abs() <= PROBABILITY_TOLERANCE
                    }
                    (None, None) => true,
                    _ => false,
                };
                if !close {
                    return Some(format!(
                        "probability column {index}, row {row}: {value:?}, expected {expected:?}"
                    ));
                }
            }
        }
        None
    }
}

#[test]
fn predictions_match_characterization() {
    let temp = tempfile::tempdir().unwrap();
    let mut actual = BTreeMap::new();
    for &(name, bundle, input) in CASES {
        for masked in [false, true] {
            let mut runs = Vec::new();
            for &batch_size in BATCH_SIZES {
                let output = temp.path().join(format!("{name}-{masked}-{batch_size}"));
                let config = ClassifyConfig {
                    input: fixtures().join(input),
                    model: fixtures().join(bundle),
                    output: output.clone(),
                    masks: if masked {
                        vec![alternating_mask(&fixtures().join(input), temp.path(), name)]
                    } else {
                        vec![]
                    },
                    threads: Some(1),
                    batch_size,
                    abstain_margin: 0.0,
                    overwrite: false,
                };
                ClassifyRunner::run(&config).unwrap();
                runs.push(read_outputs(&output));
            }
            assert!(
                runs.windows(2)
                    .all(|pair| pair[0].exact == pair[1].exact && pair[0].bits() == pair[1].bits()),
                "{name} (masked: {masked}) depends on the batch size"
            );
            let key = format!("{name}{}", if masked { "-masked" } else { "" });
            actual.insert(key, runs.swap_remove(0));
        }
    }

    let golden = fixtures().join("../characterization/classify.json");
    if std::env::var_os("CYTIQ_BLESS").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CYTIQ_BLESS rewrites a reviewed golden; CI never regenerates goldens"
        );
        let mut json = serde_json::to_string(&actual).unwrap();
        json.push('\n');
        fs::write(&golden, json).unwrap();
        eprintln!("rewrote {golden:?}; review `git diff` before committing it");
    }
    let expected: BTreeMap<String, Outputs> = serde_json::from_str(
        &fs::read_to_string(&golden)
            .unwrap_or_else(|error| panic!("read {golden:?} (bless with CYTIQ_BLESS=1): {error}")),
    )
    .unwrap();
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "characterized cases changed"
    );
    let mismatches = actual
        .iter()
        .filter_map(|(name, outputs)| {
            outputs
                .mismatch(&expected[name])
                .map(|why| format!("{name}: {why}"))
        })
        .collect::<Vec<_>>();
    assert!(
        mismatches.is_empty(),
        "classify output changed; if intended, rerun with CYTIQ_BLESS=1:\n{}",
        mismatches.join("\n")
    );
}

fn alternating_mask(input: &Path, directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(format!("{name}.mask.parquet"));
    if !path.exists() {
        let dataset = FcsReader::open(input).unwrap().read().unwrap();
        let acquisition = cytiq_fcs::acquisition_id(input, &dataset.text).unwrap();
        EventMask::new(
            acquisition,
            (0..dataset.event_count()).map(|i| i % 3 != 1).collect(),
            None,
        )
        .write_parquet(&path)
        .unwrap();
    }
    path
}

/// Every prediction column and the summary bytes. Float32 columns are the
/// probabilities and are kept as values; everything else, including where
/// they are null, goes into the exact digest.
fn read_outputs(output: &Path) -> Outputs {
    let mut hasher = Sha256::new();
    let reader = ParquetRecordBatchReaderBuilder::try_new(
        File::open(output.join("predictions.parquet")).unwrap(),
    )
    .unwrap();
    let schema = reader.schema().clone();
    let mut metadata = schema.metadata().iter().collect::<Vec<_>>();
    metadata.sort();
    hasher.update(format!("{metadata:?}{:?}", schema.fields()).as_bytes());
    let mut probabilities: BTreeMap<usize, Vec<Option<f32>>> = BTreeMap::new();
    for batch in reader.build().unwrap() {
        let batch = batch.unwrap();
        for (index, column) in batch.columns().iter().enumerate() {
            let any = column.as_any();
            for row in 0..column.len() {
                if let Some(values) = any.downcast_ref::<Float32Array>() {
                    let value = (!column.is_null(row)).then(|| values.value(row));
                    probabilities.entry(index).or_default().push(value);
                } else if column.is_null(row) {
                    hasher.update(b"null");
                } else if let Some(values) = any.downcast_ref::<UInt64Array>() {
                    hasher.update(values.value(row).to_le_bytes());
                } else if let Some(values) = any.downcast_ref::<BooleanArray>() {
                    hasher.update([u8::from(values.value(row))]);
                } else if let Some(values) = any.downcast_ref::<StringArray>() {
                    hasher.update(values.value(row).as_bytes());
                    hasher.update([0]);
                } else {
                    panic!("unexpected prediction column type {:?}", column.data_type());
                }
            }
        }
    }
    hasher.update(fs::read(output.join("summary.json")).unwrap());
    let exact = hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    Outputs {
        exact,
        probabilities: probabilities.into_values().collect(),
    }
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/classify")
}
