//! The published dataset contracts: JSON Schemas pinned by a lock, identity
//! vectors other implementations reproduce, and conformance of what cytiq
//! writes. `CYTIQ_BLESS=1` rewrites the lock and the vectors when a change
//! is intended; a released version's files never change.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cytiq_population::dataset::{self, Profile, ValidateOptions};
use cytiq_population::events::{self, ColumnValues, LayerDescriptor, ValuesHasher};
use cytiq_population::identity;
use cytiq_population::issue::Issues;
use cytiq_population::labels::{self, LabelBatch, LabelDescriptor, LabelState, LabelWriter};
use serde_json::{Value, json};

fn contracts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("contracts/dataset")
}

fn blessing() -> bool {
    std::env::var_os("CYTIQ_BLESS").is_some()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    use std::fmt::Write as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").unwrap();
            text
        })
}

#[test]
fn published_v1_files_match_their_lock() {
    let directory = contracts().join("v1");
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        files.insert(name, sha256(&std::fs::read(&path).unwrap()));
    }
    let lock_path = contracts().join("v1.lock.json");
    let actual = json!({"schema_version": 1, "files": files});
    if blessing() {
        std::fs::write(
            &lock_path,
            serde_json::to_string_pretty(&actual).unwrap() + "\n",
        )
        .unwrap();
        return;
    }
    let lock: Value = serde_json::from_slice(&std::fs::read(&lock_path).unwrap()).unwrap();
    assert_eq!(
        lock, actual,
        "contracts/dataset/v1 changed; a released version is immutable"
    );
    for (name, text) in dataset::PUBLISHED_SCHEMAS {
        assert_eq!(
            lock["files"][*name].as_str(),
            Some(sha256(text.as_bytes()).as_str()),
            "{name} compiled into the crate differs from the lock"
        );
    }
}

fn descriptor() -> LayerDescriptor {
    serde_json::from_value(json!({
        "contract": "cytiq.events.layer.v1",
        "acquisition_id": "a".repeat(64),
        "state": "transformed",
        "steps": [
            {"kind": "compensation", "method": "spillover", "origin": "recorded",
             "definition_sha256": "c".repeat(64), "channels": ["FL1-A"]},
            {"kind": "transform", "method": "arcsinh", "origin": "recorded",
             "parameters": {"cofactor": 150.0}, "channels": ["FL1-A"]}
        ],
        "channels": [
            {"index": 1, "name": "FSC-A", "value_type": "float32"},
            {"index": 2, "name": "FL1-A", "value_type": "float32"},
            {"index": 3, "name": "Time", "value_type": "uint64"}
        ]
    }))
    .unwrap()
}

fn label_descriptor() -> LabelDescriptor {
    serde_json::from_value(json!({
        "contract": labels::DESCRIPTOR_CONTRACT,
        "kind": "gate",
        "acquisition_id": "a".repeat(64),
        "raw_events": 3,
        "producer": {
            "tool": "cytiq gate", "native_contract": "cytiq.mask.v2",
            "native_sha256": "b".repeat(64), "input_sha256": "c".repeat(64),
            "input_layer_id": null
        },
        "definition": {"inputs": {"gating_ml": ["d".repeat(64)]}, "parameters": {}},
        "eligibility": {"masks": ["e".repeat(64)], "verified": true},
        "labels": [{"name": "cells"}, {"name": "CD3+", "parent": "cells"}]
    }))
    .unwrap()
}

/// The values digest of a three-event gate table, and the rows it covers.
fn label_values(descriptor: &LabelDescriptor) -> (Value, String) {
    let cells = [
        (LabelState::Assessed, [Some(true), Some(true)]),
        (LabelState::Assessed, [Some(true), Some(false)]),
        (LabelState::QcRejected, [None, None]),
    ];
    let directory = tempfile::tempdir().unwrap();
    let mut writer = LabelWriter::create(
        &directory.path().join("labels.parquet"),
        descriptor.clone(),
        &labels::LabelSettings::default(),
        cytiq_mask::parquet::ParquetReadLimits::default(),
    )
    .unwrap();
    let column = |gate: usize| -> arrow_array::ArrayRef {
        std::sync::Arc::new(arrow_array::BooleanArray::from(
            cells
                .iter()
                .map(|(_, gates)| gates[gate])
                .collect::<Vec<_>>(),
        ))
    };
    writer
        .write(LabelBatch {
            first: 0,
            states: cells.iter().map(|(state, _)| *state).collect(),
            columns: vec![column(0), column(1)],
        })
        .unwrap();
    let written = writer.finish().unwrap();
    let rows: Vec<Value> = cells
        .iter()
        .enumerate()
        .map(|(id, (state, gates))| {
            json!({
                "source_event_id": id,
                "state": state.as_str(),
                "cells": gates.iter().map(|cell| json!({"type": "boolean", "value": cell})).collect::<Vec<_>>()
            })
        })
        .collect();
    (
        json!({
            "domain": labels::VALUES_DOMAIN,
            "annotation_id": descriptor.annotation_id(),
            "rows": rows
        }),
        written.values_sha256,
    )
}

/// Each vector names its inputs and the digest cytiq computes from them.
fn vectors() -> Value {
    let layer = descriptor();
    let labels_descriptor = label_descriptor();
    let (label_rows, label_digest) = label_values(&labels_descriptor);
    let semantic = json!({
        "contract": "cytiq.mask.v2", "role": "technical_mask", "acquisition": "a".repeat(64),
        "parents": ["e".repeat(64), "d".repeat(64)], "basis": "b".repeat(64)
    });
    let parents: Vec<String> = semantic["parents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect();
    let semantic_id = identity::semantic_id(
        "cytiq.mask.v2",
        "technical_mask",
        Some(&"a".repeat(64)),
        &parents,
        &"b".repeat(64),
    );
    let rows = [(0_u64, 1.5_f32, -2.0_f32, 7_u64), (3, 0.0, 1e-7, u64::MAX)];
    let mut hasher = ValuesHasher::new(&layer.layer_id());
    let ids: Vec<u64> = rows.iter().map(|r| r.0).collect();
    let a: Vec<f32> = rows.iter().map(|r| r.1).collect();
    let b: Vec<f32> = rows.iter().map(|r| r.2).collect();
    let c: Vec<u64> = rows.iter().map(|r| r.3).collect();
    hasher.update(
        &ids,
        &[
            ColumnValues::Float32(&a),
            ColumnValues::Float32(&b),
            ColumnValues::Uint64(&c),
        ],
    );
    let values_rows: Vec<Value> = rows
        .iter()
        .map(|(id, x, y, z)| {
            json!({
                "source_event_id": id,
                "values": [
                    {"type": "float32", "bits": format!("{:08x}", x.to_bits())},
                    {"type": "float32", "bits": format!("{:08x}", y.to_bits())},
                    {"type": "uint64", "bits": format!("{z:016x}")}
                ]
            })
        })
        .collect();
    json!({
        "schema_version": 1,
        "note": "Digests are SHA-256 over canonical JSON (sorted keys, indent 2, ASCII, Python float repr, trailing newline) unless stated; values_sha256 hashes the domain tag, layer id and little-endian rows; labels_values_sha256 hashes the domain tag, annotation id and each row as a little-endian u64 id, a u32-length-prefixed state, then per cell a null tag (0) or 1 and the value.",
        "vectors": [
            {"name": "semantic_id", "input": semantic, "sha256": semantic_id},
            {"name": "layer_id", "input": layer, "sha256": layer.layer_id()},
            {"name": "recipe_id", "input": layer, "sha256": layer.recipe_id()},
            {"name": "values_sha256", "input": {
                "domain": events::VALUES_DOMAIN, "layer_id": layer.layer_id(), "rows": values_rows
            }, "sha256": hasher.finish()},
            {"name": "annotation_id", "input": labels_descriptor, "sha256": labels_descriptor.annotation_id()},
            {"name": "definition_id", "input": {
                "contract": labels::DEFINITION_CONTRACT,
                "kind": labels_descriptor.kind,
                "tool": labels_descriptor.producer.tool,
                "definition": labels_descriptor.definition,
                "labels": labels_descriptor.labels
            }, "sha256": labels_descriptor.definition_id()},
            {"name": "labels_values_sha256", "input": label_rows, "sha256": label_digest}
        ]
    })
}

#[test]
fn identity_vectors_reproduce() {
    let path = contracts().join("v1/identity-vectors.json");
    let actual = vectors();
    if blessing() {
        std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
        return;
    }
    let pinned: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(pinned, actual, "an identity computation changed");
    // The layer id is the canonical digest of the descriptor as written.
    let layer = &pinned["vectors"][1];
    assert_eq!(
        cytiq_population::serialization::digest(&layer["input"]),
        layer["sha256"].as_str().unwrap()
    );
}

#[test]
fn documents_cytiq_writes_conform_to_the_published_schemas() {
    let built = common::build();
    let manifest = serde_json::to_value(&built.manifest).unwrap();
    dataset::conforms(&manifest, "dataset-manifest.schema.json").unwrap();

    let mut issues = Issues::new();
    let layer = &built.manifest.artifacts[2];
    let summary =
        events::verify_file(&built.root.path().join(&layer.location), "", &mut issues).unwrap();
    let descriptor = serde_json::to_value(&summary.descriptor).unwrap();
    dataset::conforms(&descriptor, "events-layer.schema.json").unwrap();

    let path = built.root.path().join("dataset.json");
    std::fs::write(&path, built.manifest.canonical_json()).unwrap();
    for verify_objects in [false, true] {
        let options = ValidateOptions {
            profile: Profile::MiflowcytMinimal,
            root: None,
            verify_objects,
        };
        let report = dataset::validate(&path, &options).unwrap();
        dataset::conforms(
            &serde_json::to_value(&report).unwrap(),
            "validation-report.schema.json",
        )
        .unwrap();
    }
    let invalid = serde_json::to_value(
        dataset::validate(
            &built.root.path().join("absent.json"),
            &ValidateOptions::default(),
        )
        .is_err(),
    )
    .unwrap();
    assert_eq!(invalid, json!(true));
}

#[test]
fn the_schemas_reject_what_the_parser_rejects() {
    let built = common::build();
    let manifest = serde_json::to_value(&built.manifest).unwrap();
    let mut unknown = manifest.clone();
    unknown["panels"][0]["fluorochrome"] = json!("FITC");
    let mut future = manifest.clone();
    future["schema_version"] = json!(2);
    let mut bad_digest = manifest;
    bad_digest["artifacts"][0]["semantic_id"] = json!("not-a-digest");
    for document in [&unknown, &future] {
        assert!(dataset::conforms(document, "dataset-manifest.schema.json").is_err());
        let mut issues = Issues::new();
        let text = serde_json::to_vec(document).unwrap();
        assert!(
            dataset::parse(
                &text,
                cytiq_population::serialization::DocumentFormat::Json,
                &mut issues
            )
            .is_none()
        );
    }
    assert!(dataset::conforms(&bad_digest, "dataset-manifest.schema.json").is_err());
    assert!(dataset::conforms(&json!({}), "unknown.schema.json").is_err());
}
