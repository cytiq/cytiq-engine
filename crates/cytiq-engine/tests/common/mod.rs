//! Helpers shared by the CLI integration tests.

// Each test crate uses a different subset of these helpers.
#![allow(dead_code)]

pub mod dataset;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Assert that `record` is a provenance record of `tool` that lists every
/// path in `inputs` and `outputs` with its current SHA-256.
pub fn assert_provenance(record: &Path, tool: &str, inputs: &[&Path], outputs: &[&Path]) {
    let text = fs::read_to_string(record)
        .unwrap_or_else(|error| panic!("provenance {} should exist: {error}", record.display()));
    let record: serde_json::Value = serde_json::from_str(&text).expect("provenance should be JSON");
    assert_eq!(record["tool"], tool);
    assert!(
        record["arguments"]
            .as_array()
            .is_some_and(|arguments| !arguments.is_empty())
    );
    for (key, paths) in [("inputs", inputs), ("outputs", outputs)] {
        let listed = record[key].as_array().expect("file list");
        for path in paths {
            let sha256 = sha256_hex(path);
            let entry = listed
                .iter()
                .find(|entry| entry["path"] == path.display().to_string())
                .unwrap_or_else(|| panic!("{key} should list {}: {listed:?}", path.display()));
            assert_eq!(entry["sha256"], sha256, "{}", path.display());
        }
    }
}

/// Lowercase hex SHA-256 of a file.
pub fn sha256_hex(path: &Path) -> String {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// The raw acquisition an FCS file belongs to, as stages record it in masks.
pub fn acquisition_of(path: &Path) -> String {
    let dataset = cytiq_fcs::FcsReader::open(path)
        .and_then(cytiq_fcs::FcsReader::read)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    cytiq_fcs::acquisition_id(path, &dataset.text).expect("acquisition should resolve")
}

/// Rewrite `path` with `channel` stored log-amplified (`$PnE=4,1`, four
/// decades), as analog instruments with log amplifiers recorded it. The
/// writer only emits `$PnE=0,0`, so the TEXT is patched in place: same length,
/// same offsets.
pub fn set_log_amplified(path: &Path, channel: &str) {
    let dataset = cytiq_fcs::FcsReader::open(path)
        .and_then(cytiq_fcs::FcsReader::read)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    let number = dataset.parameters[dataset
        .find_parameter(channel)
        .unwrap_or_else(|| panic!("{} has no channel `{channel}`", path.display()))]
    .index;
    // Rewritten by the cytiq writer, whatever wrote the source.
    add_keyword(path, "$COM", "log-amplified test copy");
    let mut bytes = fs::read(path).expect("rewritten FCS should read");
    let linear = format!("$P{number}E|0,0|").into_bytes();
    let start = bytes
        .windows(linear.len())
        .position(|window| window == linear)
        .expect("the writer declares `$PnE=0,0`");
    bytes[start..start + linear.len()].copy_from_slice(format!("$P{number}E|4,1|").as_bytes());
    fs::write(path, bytes).expect("patched FCS should write");
}

/// Rewrite `path` with an extra TEXT keyword, for example a `CYTIQ_*` signal
/// state as if a cytiq stage had produced the file.
pub fn add_keyword(path: &Path, key: &str, value: &str) {
    let dataset = cytiq_fcs::FcsReader::open(path)
        .and_then(cytiq_fcs::FcsReader::read)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    let parameters = dataset
        .parameters
        .iter()
        .map(cytiq_fcs::FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    let columns = dataset
        .data
        .columns
        .iter()
        .map(cytiq_fcs::ColumnData::to_f64_vec)
        .collect::<Vec<_>>();
    cytiq_fcs::write_fcs3_1_exact_with_options(
        path,
        &parameters,
        &columns,
        &cytiq_fcs::FcsWriteOptions::new()
            .with_text_keywords(cytiq_fcs::carried_keywords(&dataset.text))
            .with_text_keyword(key, value),
    )
    .expect("rewritten FCS should write");
}

/// A `CYTIQ_TRANSFORM` value covering `channels`.
pub fn transform_keyword(channels: &[&str]) -> String {
    serde_json::json!({ "method": "arcsinh", "files": [{ "channels": channels }] }).to_string()
}

/// Copy `source` to `target` with an `Original_ID` column beyond f32
/// precision (from 2^24 + 1), as `PeacoQC` writes for large acquisitions.
/// Returns the identifiers.
#[allow(clippy::cast_precision_loss)]
pub fn write_with_large_original_ids(source: &Path, target: &Path) -> Vec<f64> {
    let dataset = cytiq_fcs::FcsReader::open(source)
        .and_then(cytiq_fcs::FcsReader::read)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", source.display()));
    let ids = (0..dataset.event_count())
        .map(|row| 16_777_217.0 + (2 * row) as f64)
        .collect::<Vec<_>>();
    let mut parameters = dataset
        .parameters
        .iter()
        .map(cytiq_fcs::FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    parameters.push(cytiq_fcs::FcsWriteParameter::new(
        cytiq_fcs::ORIGINAL_ID_PARAMETER,
    ));
    let mut columns = dataset
        .data
        .columns
        .iter()
        .map(cytiq_fcs::ColumnData::to_f64_vec)
        .collect::<Vec<_>>();
    columns.push(ids.clone());
    cytiq_fcs::write_fcs3_1_f64_with_options(
        target,
        &parameters,
        &columns,
        &cytiq_fcs::FcsWriteOptions::new()
            .with_text_keywords(cytiq_fcs::carried_keywords(&dataset.text)),
    )
    .expect("copy with Original_ID should write");
    ids
}

/// Adjusted Rand index between two labelings of the same events.
#[allow(clippy::cast_precision_loss)]
pub fn adjusted_rand_index(left: &[u64], right: &[u64]) -> f64 {
    assert_eq!(left.len(), right.len());
    let pairs = |count: usize| (count * count.saturating_sub(1) / 2) as f64;
    let mut table = HashMap::<(u64, u64), usize>::new();
    let mut left_counts = HashMap::<u64, usize>::new();
    let mut right_counts = HashMap::<u64, usize>::new();
    for (a, b) in left.iter().zip(right) {
        *table.entry((*a, *b)).or_default() += 1;
        *left_counts.entry(*a).or_default() += 1;
        *right_counts.entry(*b).or_default() += 1;
    }
    let index = table.values().map(|count| pairs(*count)).sum::<f64>();
    let left_sum = left_counts.values().map(|count| pairs(*count)).sum::<f64>();
    let right_sum = right_counts
        .values()
        .map(|count| pairs(*count))
        .sum::<f64>();
    let expected = left_sum * right_sum / pairs(left.len());
    let maximum = f64::midpoint(left_sum, right_sum);
    if (maximum - expected).abs() < f64::EPSILON {
        1.0
    } else {
        (index - expected) / (maximum - expected)
    }
}

/// Every `*.provenance.json` under `directory`: a file read by one stage has
/// the SHA-256 another stage recorded when it wrote that file.
/// Every provenance record under `directory` is a schema 2 record with build
/// info, a config and input roles, and each stage's inputs are byte-identical
/// to the outputs an earlier stage recorded.
pub fn assert_provenance_chain(directory: &Path) {
    let records = walk(directory)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("provenance.json"))
        })
        .map(|path| {
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path).expect("record"))
                .expect("record should be JSON")
        })
        .collect::<Vec<_>>();
    assert!(
        !records.is_empty(),
        "no provenance records under {}",
        directory.display()
    );
    let canonical = |path: &str| {
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            directory.join(path)
        };
        fs::canonicalize(&path).unwrap_or(path)
    };
    let written = records
        .iter()
        .flat_map(|record| record["outputs"].as_array().cloned().unwrap_or_default())
        .map(|output| {
            (
                canonical(output["path"].as_str().expect("path")),
                output["sha256"].clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    for record in &records {
        assert_eq!(record["schema_version"], 3, "{record:#}");
        let fingerprint = record["execution_fingerprint"]
            .as_str()
            .expect("execution fingerprint");
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(
            record["build"]["git_commit"]
                .as_str()
                .is_some_and(|commit| !commit.is_empty()),
            "{record:#}"
        );
        assert!(record["config"].is_object(), "{record:#}");
        assert!(
            record["inputs"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|input| input["role"].is_string()),
            "every input has a role: {record:#}"
        );
    }
    let mut linked = 0;
    for record in &records {
        for input in record["inputs"].as_array().into_iter().flatten() {
            let path = canonical(input["path"].as_str().expect("path"));
            if let Some(sha256) = written.get(&path) {
                assert_eq!(
                    &input["sha256"],
                    sha256,
                    "{} changed between stages",
                    path.display()
                );
                linked += 1;
            }
        }
    }
    assert!(linked > 0, "no stage read another stage's output");
}

fn walk(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
