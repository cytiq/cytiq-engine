use arrow_array::{ArrayRef, BooleanArray, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use cytiq_population::{
    ArtifactRef, Measure, ObservationStatus, ObserveConfig,
    annotations::{ObservationContext, SourceDefinition},
    knowledge::KnowledgeArtifact,
    observe,
};
use cytiq_provenance::{RunRecorder, hash_file};
use parquet::arrow::ArrowWriter;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures/specimen")
        .join(name)
}

fn parquet(path: &Path, sources: Vec<u64>, keep: &[bool], gates: bool) {
    if !gates {
        cytiq_mask::EventMask::new("a".repeat(64), keep.to_vec(), Some(sources))
            .write_parquet(path)
            .unwrap();
        return;
    }
    let size = sources.len();
    let mut fields = vec![
        Field::new("event_id", DataType::UInt64, false),
        Field::new("source_event_id", DataType::UInt64, false),
        Field::new("keep", DataType::Boolean, false),
    ];
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from((0..size as u64).collect::<Vec<_>>())),
        Arc::new(UInt64Array::from(sources)),
        Arc::new(BooleanArray::from(keep.to_vec())),
    ];
    if gates {
        for (name, values) in [
            ("All", vec![true, true, true, false]),
            ("T", vec![true, true, false, false]),
            ("Activated", vec![false, true, true, false]),
            ("Zero", vec![false, false, false, false]),
        ] {
            fields.push(Field::new(name, DataType::Boolean, true));
            arrays.push(Arc::new(BooleanArray::from(
                values
                    .into_iter()
                    .zip(keep)
                    .map(|(v, k)| k.then_some(v))
                    .collect::<Vec<_>>(),
            )));
        }
    }
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        std::collections::HashMap::from([
            ("cytiq.mask.schema_version".into(), "2".into()),
            ("cytiq.acquisition_sha256".into(), "a".repeat(64)),
            ("cytiq.event_id_base".into(), "0".into()),
        ]),
    ));
    let mut writer =
        ArrowWriter::try_new(File::create(path).unwrap(), schema.clone(), None).unwrap();
    writer
        .write(&RecordBatch::try_new(schema, arrays).unwrap())
        .unwrap();
    writer.close().unwrap();
}

fn setup(directory: &Path, fail: bool) -> ObserveConfig {
    setup_with_policy(directory, fail, &json!({"min_events":1}))
}

fn setup_with_policy(directory: &Path, fail: bool, policy: &serde_json::Value) -> ObserveConfig {
    let manifest = directory.join("run.yaml");
    std::fs::write(&manifest, serde_json::to_vec(&json!({
        "cytometry":"conventional", "panel":{"name":"P1","markers":[{"marker":"CD3"}], "qc":policy},
        "samples":[{"id":"A01","file":fixture("../pipeline/sample.fcs")}]
    })).unwrap()).unwrap();
    let projection = fixture("P1.projection.json");
    let knowledge = KnowledgeArtifact::load(&projection).unwrap();
    let source: SourceDefinition = serde_json::from_value(json!({
        "format":"gate_membership","definition":ArtifactRef::of(&fixture("gates.xml")).unwrap(),"bindings":[
            {"label":"All","target":{"kind":"population","id":"cells","scope":"cells"}},
            {"label":"T","target":{"kind":"population","id":"t_cells","scope":"cells"},"denominator":"All"},
            {"label":"Activated","target":{"kind":"population","id":"activated","scope":"cells"},"denominator":"All"},
            {"label":"Zero","target":{"kind":"population","id":"zero","scope":"cells"},"denominator":"All"}
        ]})).unwrap();
    let config = ObserveConfig {
        context: ObservationContext {
            specimen_id: "S01".into(),
            manifest_sha256: hash_file(&manifest).unwrap().1,
            panel_id: "P1".into(),
            acquisition_id: "A01".into(),
            acquisition_sha256: "a".repeat(64),
            raw_events: 4,
            source_id: "gates".into(),
            projection: cytiq_population::annotations::KnowledgeRef {
                path: projection,
                sha256: knowledge.sha256().into(),
            },
            source,
        },
        annotations: directory.join("membership.parquet"),
        producer_provenance: directory.join("producer.json"),
        mask: directory.join("mask.parquet"),
        qc_report: directory.join("qc.json"),
        qc_provenance: directory.join("qc.provenance.json"),
    };
    parquet(
        &config.mask,
        vec![0, 1, 2, 3],
        &[true, true, true, false],
        false,
    );
    parquet(
        &config.annotations,
        vec![0, 1, 2, 3],
        &[true, true, true, false],
        true,
    );
    let mut producer = RunRecorder::start("cytiq gate", "0.4.0", vec![]);
    producer
        .snapshot_inputs(
            "gating_ml",
            std::slice::from_ref(&config.context.source.definition.path),
        )
        .unwrap();
    producer
        .snapshot_inputs("mask", std::slice::from_ref(&config.mask))
        .unwrap();
    producer
        .finish(
            std::slice::from_ref(&config.annotations),
            &config.producer_provenance,
            cytiq_provenance::outcome::OutcomeReason::Success,
        )
        .unwrap();
    let status = if fail { "fail" } else { "pass" };
    std::fs::write(&config.qc_report,serde_json::to_vec(&json!({
        "acquisition":"a".repeat(64),"sample":"A01","panel":"P1","status":status,"failures":u64::from(fail),"warnings":0,
        "metrics":{"events":4,"kept":3},"checks":[{"status":status}]
    })).unwrap()).unwrap();
    let mut qc = RunRecorder::start("cytiq qc", "0.4.0", vec![]);
    qc.snapshot_inputs("manifest", std::slice::from_ref(&manifest))
        .unwrap();
    qc.snapshot_inputs("mask", std::slice::from_ref(&config.mask))
        .unwrap();
    qc.finish(
        std::slice::from_ref(&config.qc_report),
        &config.qc_provenance,
        cytiq_provenance::outcome::OutcomeReason::Success,
    )
    .unwrap();
    config
}

#[test]
fn release_reassessment_cannot_turn_an_unassessed_optional_filter_into_a_pass() {
    let directory = tempfile::tempdir().unwrap();
    // The pinned QC report represents an older producer that called absence a pass.
    let config = setup_with_policy(
        directory.path(),
        false,
        &json!({"filters":{"live":{"max_removed_fraction":0.1}}}),
    );
    let (annotations, rows, assessment) =
        cytiq_population::annotations::observe_with_release(&config).unwrap();
    assert_eq!(annotations.technical_status, "warn");
    assert_eq!(assessment["status"], "warn");
    assert!(
        rows.iter()
            .all(|row| row.status == ObservationStatus::Measured)
    );
}

#[test]
fn overlapping_memberships_remain_independent_and_zero_is_measured() {
    let directory = tempfile::tempdir().unwrap();
    let config = setup(directory.path(), false);
    let (annotations, rows) = observe(&config).unwrap();
    assert_eq!(annotations.assessed_events, 3);
    let counts: BTreeMap<_, _> = rows
        .iter()
        .filter(|r| r.measure == Measure::ObservedCount)
        .map(|r| (r.target.id.as_str(), r.value_u64.unwrap()))
        .collect();
    assert_eq!(
        counts,
        BTreeMap::from([("cells", 3), ("t_cells", 2), ("activated", 2), ("zero", 0)])
    );
    assert!(counts["t_cells"] + counts["activated"] > counts["cells"]);
    assert!(rows.iter().all(|r| r.status == ObservationStatus::Measured));
    assert_eq!(
        rows.iter()
            .find(|r| r.target.id == "t_cells" && r.measure == Measure::Fraction)
            .unwrap()
            .value_f64,
        Some(2.0 / 3.0)
    );
}

#[test]
fn technical_rejection_preserves_counts_only_as_supporting_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let config = setup(directory.path(), true);
    let (_, rows) = observe(&config).unwrap();
    assert!(
        rows.iter()
            .all(|r| r.status == ObservationStatus::QcRejected
                && r.value_u64.is_none()
                && r.value_f64.is_none())
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.target.id == "t_cells")
            .unwrap()
            .supporting_count,
        Some(2)
    );
}

#[test]
fn duplicates_and_wrong_masks_are_rejected_even_with_updated_file_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let config = setup(directory.path(), false);
    parquet(
        &config.annotations,
        vec![0, 1, 1, 3],
        &[true, true, true, false],
        true,
    );
    let mut record: cytiq_provenance::RunRecord =
        serde_json::from_slice(&std::fs::read(&config.producer_provenance).unwrap()).unwrap();
    record.outputs[0].sha256 = hash_file(&config.annotations).unwrap().1;
    std::fs::write(
        &config.producer_provenance,
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert!(
        observe(&config)
            .unwrap_err()
            .to_string()
            .contains("source_event_id")
    );
    let config = setup(directory.path(), false);
    parquet(
        &config.mask,
        vec![0, 1, 2, 3],
        &[true, false, true, false],
        false,
    );
    assert!(observe(&config).unwrap_err().to_string().contains("mask"));
    let mut config = setup(directory.path(), false);
    config.context.manifest_sha256 = "c".repeat(64);
    assert!(
        observe(&config)
            .unwrap_err()
            .to_string()
            .contains("different manifest")
    );
}
