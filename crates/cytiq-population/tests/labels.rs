//! `cytiq.events.labels.v1` tables: the writer, the verifier and the rules of
//! each kind, on tables written directly and on tampered copies.

use std::collections::BTreeMap;
use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{
    Array, ArrayRef, BooleanArray, DictionaryArray, Float32Array, RecordBatch, StringArray,
    UInt64Array, types::Int32Type,
};
use arrow_schema::{DataType, Field, Schema};
use cytiq_mask::parquet::ParquetReadLimits;
use cytiq_population::events::Codec;
use cytiq_population::issue::{IssueCode, Issues, Severity};
use cytiq_population::labels::{
    self, Definition, Eligibility, LabelBatch, LabelColumn, LabelDescriptor, LabelKind,
    LabelSettings, LabelState, LabelWriter, Producer, WrittenLabels,
};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;

const ACQUISITION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn digest(c: char) -> String {
    c.to_string().repeat(64)
}

fn producer(tool: &str, native_contract: &str) -> Producer {
    Producer {
        tool: tool.into(),
        native_contract: native_contract.into(),
        native_sha256: digest('b'),
        input_sha256: digest('c'),
        input_layer_id: None,
    }
}

fn column(name: &str, parent: Option<&str>) -> LabelColumn {
    LabelColumn {
        name: name.into(),
        parent: parent.map(Into::into),
        class: None,
    }
}

fn gate_descriptor(raw_events: u64) -> LabelDescriptor {
    LabelDescriptor {
        contract: labels::DESCRIPTOR_CONTRACT.into(),
        kind: LabelKind::Gate,
        acquisition_id: ACQUISITION.into(),
        raw_events,
        producer: producer("cytiq gate", "cytiq.mask.v2"),
        definition: Some(Definition {
            inputs: BTreeMap::from([("gating_ml".into(), vec![digest('d')])]),
            parameters: BTreeMap::new(),
        }),
        eligibility: Eligibility {
            masks: vec![digest('e')],
            verified: true,
        },
        labels: vec![
            column("cells", None),
            column("CD3+", Some("cells")),
            column("CD19+", Some("cells")),
        ],
    }
}

fn classifier_descriptor(raw_events: u64) -> LabelDescriptor {
    LabelDescriptor {
        kind: LabelKind::Classifier,
        producer: producer("cytiq classify", "cytiq.classify.predictions.v5"),
        definition: Some(Definition {
            inputs: BTreeMap::from([("model".into(), vec![digest('d')])]),
            parameters: BTreeMap::new(),
        }),
        labels: ["blast", "normal"]
            .iter()
            .map(|class| LabelColumn {
                name: format!("p_{class}"),
                parent: None,
                class: Some((*class).into()),
            })
            .collect(),
        ..gate_descriptor(raw_events)
    }
}

/// Gate states and cells of `rows` events: every fifth is QC-rejected; the
/// others are in `cells`, every other one in `CD3+`, every third in `CD19+`
/// (overlapping).
fn gate_rows(first: u64, rows: u64) -> LabelBatch {
    let ids = first..first + rows;
    let state = |id: u64| {
        if id % 5 == 4 {
            LabelState::QcRejected
        } else {
            LabelState::Assessed
        }
    };
    let cell = |id: u64, member: bool| (state(id) == LabelState::Assessed).then_some(member);
    let gate = |member: fn(u64) -> bool| -> ArrayRef {
        Arc::new(BooleanArray::from(
            ids.clone()
                .map(|id| cell(id, member(id)))
                .collect::<Vec<_>>(),
        ))
    };
    LabelBatch {
        first,
        states: ids.clone().map(state).collect(),
        columns: vec![
            gate(|_| true),
            gate(|id| id % 2 == 0),
            gate(|id| id % 3 == 0),
        ],
    }
}

fn write(
    path: &Path,
    descriptor: LabelDescriptor,
    batches: Vec<LabelBatch>,
    settings: &LabelSettings,
) -> cytiq_population::Result<WrittenLabels> {
    let mut writer = LabelWriter::create(path, descriptor, settings, ParquetReadLimits::default())?;
    for batch in batches {
        writer.write(batch)?;
    }
    writer.finish()
}

fn gate_table(directory: &Path, raw_events: u64) -> (PathBuf, WrittenLabels) {
    let path = directory.join("gate.parquet");
    let written = write(
        &path,
        gate_descriptor(raw_events),
        vec![gate_rows(0, raw_events)],
        &LabelSettings::default(),
    )
    .unwrap();
    (path, written)
}

fn verify(path: &Path) -> (Option<labels::LabelsSummary>, Issues) {
    let mut issues = Issues::new();
    let summary = labels::verify_file(path, "/x", &mut issues);
    (summary, issues)
}

fn codes(issues: &Issues) -> Vec<IssueCode> {
    issues.codes(Severity::Error).into_iter().collect()
}

/// Rewrite the table at `path` into `output` with the standard arrow-rs
/// reader and writer, after `edit` changes its footer metadata or batches.
fn rewrite(
    path: &Path,
    output: &Path,
    edit: impl FnOnce(&mut BTreeMap<String, String>, &mut Vec<RecordBatch>),
) {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap()).unwrap();
    let mut metadata: BTreeMap<String, String> = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .unwrap()
        .iter()
        .filter(|kv| kv.key != "ARROW:schema")
        .map(|kv| (kv.key.clone(), kv.value.clone().unwrap_or_default()))
        .collect();
    let mut batches = builder
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    // Footer keys come from `metadata` alone, not from the Arrow schema the
    // writer embeds.
    for batch in &mut batches {
        let bare = Arc::new(Schema::new(batch.schema().fields().clone()));
        *batch = RecordBatch::try_new(bare, batch.columns().to_vec()).unwrap();
    }
    edit(&mut metadata, &mut batches);
    let schema = batches[0].schema();
    let properties = WriterProperties::builder()
        .set_key_value_metadata(Some(
            metadata
                .into_iter()
                .map(|(key, value)| KeyValue::new(key, value))
                .collect(),
        ))
        .build();
    let mut writer =
        ArrowWriter::try_new(File::create(output).unwrap(), schema, Some(properties)).unwrap();
    for batch in &batches {
        writer.write(batch).unwrap();
    }
    writer.close().unwrap();
}

fn replace_column(batch: &RecordBatch, name: &str, array: ArrayRef) -> RecordBatch {
    let index = batch.schema().index_of(name).unwrap();
    let mut columns = batch.columns().to_vec();
    columns[index] = array;
    RecordBatch::try_new(batch.schema(), columns).unwrap()
}

#[test]
fn every_raw_event_gets_exactly_one_row_in_source_order() {
    let directory = tempfile::tempdir().unwrap();
    let (path, written) = gate_table(directory.path(), 10);
    assert_eq!(written.rows, 10);
    let (summary, issues) = verify(&path);
    let summary = summary.unwrap_or_else(|| panic!("{:?}", issues.items()));
    assert_eq!(summary.rows, 10);
    let ids = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
        .unwrap()
        .build()
        .unwrap()
        .flat_map(|batch| {
            batch
                .unwrap()
                .column_by_name("source_event_id")
                .unwrap()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .values()
                .to_vec()
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, (0..10).collect::<Vec<_>>());

    let refused = |batches: Vec<LabelBatch>| {
        write(
            &directory.path().join("refused.parquet"),
            gate_descriptor(10),
            batches,
            &LabelSettings::default(),
        )
        .unwrap_err()
        .to_string()
    };
    assert!(refused(vec![gate_rows(1, 9)]).contains("raw order"));
    assert!(refused(vec![gate_rows(0, 4), gate_rows(5, 5)]).contains("raw order"));
    assert!(refused(vec![gate_rows(0, 9)]).contains("9 label rows"));
    assert!(refused(vec![gate_rows(0, 11)]).contains("more label rows"));
}

#[test]
fn values_digest_does_not_depend_on_row_groups_or_codec() {
    let directory = tempfile::tempdir().unwrap();
    let rows = 1_000;
    let mut digests = Vec::new();
    for (index, (row_group, codec, batch)) in [
        (131_072, Codec::Zstd(1), rows),
        (7, Codec::Snappy, 13),
        (100, Codec::Zstd(9), 1),
    ]
    .into_iter()
    .enumerate()
    {
        let batches = (0..rows)
            .step_by(usize::try_from(batch).unwrap())
            .map(|first| gate_rows(first, batch.min(rows - first)))
            .collect();
        let path = directory.path().join(format!("{index}.parquet"));
        let settings = LabelSettings {
            row_group_rows: NonZeroUsize::new(row_group).unwrap(),
            codec,
        };
        let written = write(&path, gate_descriptor(rows), batches, &settings).unwrap();
        digests.push((written.annotation_id, written.values_sha256));
    }
    assert!(digests.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn annotation_id_is_path_free_and_changes_with_eligibility() {
    let directory = tempfile::tempdir().unwrap();
    let (_, one) = gate_table(directory.path(), 10);
    let other = tempfile::tempdir().unwrap();
    let (_, moved) = gate_table(other.path(), 10);
    assert_eq!(one.annotation_id, moved.annotation_id);
    assert_eq!(one.values_sha256, moved.values_sha256);

    let mut unverified = gate_descriptor(10);
    unverified.eligibility.verified = false;
    let states: Vec<_> = gate_rows(0, 10)
        .states
        .into_iter()
        .map(|state| match state {
            LabelState::QcRejected => LabelState::NotAssessed,
            state => state,
        })
        .collect();
    let batch = LabelBatch {
        states,
        ..gate_rows(0, 10)
    };
    let written = write(
        &directory.path().join("unverified.parquet"),
        unverified,
        vec![batch],
        &LabelSettings::default(),
    )
    .unwrap();
    assert_ne!(written.annotation_id, one.annotation_id);
    assert_eq!(written.definition_id, one.definition_id);
}

#[test]
fn counts_are_exact_per_column_value_and_state() {
    let directory = tempfile::tempdir().unwrap();
    let (path, _) = gate_table(directory.path(), 100);
    let summary = verify(&path).0.unwrap();
    assert_eq!(
        summary.states,
        BTreeMap::from([(LabelState::Assessed, 80), (LabelState::QcRejected, 20)])
    );
    let count = |column: &str, value: Option<&str>, state: LabelState| {
        summary
            .counts
            .iter()
            .find(|c| c.column == column && c.value.as_deref() == value && c.state == state)
            .map_or(0, |c| c.count)
    };
    let assessed = (0..100_u64).filter(|id| id % 5 != 4);
    assert_eq!(count("cells", Some("true"), LabelState::Assessed), 80);
    assert_eq!(count("cells", None, LabelState::QcRejected), 20);
    assert_eq!(
        count("CD3+", Some("true"), LabelState::Assessed),
        assessed.clone().filter(|id| id % 2 == 0).count() as u64
    );
    assert_eq!(
        count("CD19+", Some("true"), LabelState::Assessed),
        assessed.filter(|id| id % 3 == 0).count() as u64
    );
}

#[test]
fn arrow_dictionary_columns_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let (path, _) = gate_table(directory.path(), 10);
    let tampered = directory.path().join("dictionary.parquet");
    rewrite(&path, &tampered, |_, batches| {
        for batch in batches.iter_mut() {
            let states = batch
                .column_by_name("state")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .iter()
                .collect::<DictionaryArray<Int32Type>>();
            let mut fields = batch.schema().fields().to_vec();
            fields[3] = Arc::new(Field::new(
                "state",
                DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
                false,
            ));
            let mut columns = batch.columns().to_vec();
            columns[3] = Arc::new(states);
            *batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).unwrap();
        }
    });
    let (summary, issues) = verify(&tampered);
    assert!(summary.is_none());
    assert_eq!(codes(&issues), [IssueCode::CorruptArtifactHash]);
}

#[test]
fn tables_the_verifier_would_refuse_are_refused_before_writing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large.parquet");
    let limits = ParquetReadLimits {
        decoded_bytes: 1 << 20,
        ..ParquetReadLimits::default()
    };
    let error = LabelWriter::create(
        &path,
        gate_descriptor(100_000),
        &LabelSettings::default(),
        limits,
    )
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("decodes to about"), "{error}");
    assert!(!path.exists());

    let mut invalid = gate_descriptor(10);
    invalid.labels[2].name = "cd3+".into();
    assert!(
        LabelWriter::create(
            &path,
            invalid,
            &LabelSettings::default(),
            ParquetReadLimits::default()
        )
        .is_err()
    );
    assert!(!path.exists());
}

type Edit = Box<dyn FnOnce(&mut BTreeMap<String, String>, &mut Vec<RecordBatch>)>;

/// Each tampered copy of a valid gate table fails with its code.
fn assert_tampered(cases: Vec<(&str, IssueCode, Edit)>) {
    let directory = tempfile::tempdir().unwrap();
    let (path, _) = gate_table(directory.path(), 10);
    for (name, code, edit) in cases {
        let tampered = directory.path().join(format!("{name}.parquet"));
        rewrite(&path, &tampered, edit);
        let (summary, issues) = verify(&tampered);
        assert!(summary.is_none(), "{name} verified");
        assert!(
            codes(&issues).contains(&code),
            "{name}: {:?}",
            issues.items()
        );
    }
}

#[test]
fn tampered_footers_are_reported_by_code() {
    let cases: Vec<(&str, IssueCode, Edit)> = vec![
        (
            "version",
            IssueCode::UnsupportedSchema,
            Box::new(|m, _| {
                m.insert(labels::SCHEMA_VERSION_KEY.into(), "2".into());
            }),
        ),
        (
            "kind",
            IssueCode::CorruptArtifactHash,
            Box::new(|m, _| {
                m.insert(labels::KIND_KEY.into(), "qc".into());
            }),
        ),
        (
            "descriptor",
            IssueCode::CorruptArtifactHash,
            Box::new(|m, _| {
                let text = m[labels::DESCRIPTOR_KEY].replace("\"CD19+\"", "\"CD20+\"");
                m.insert(labels::DESCRIPTOR_KEY.into(), text);
            }),
        ),
        (
            "definition",
            IssueCode::CorruptArtifactHash,
            Box::new(|m, _| {
                m.remove(labels::DEFINITION_ID_KEY);
            }),
        ),
        (
            "acquisition",
            IssueCode::InvalidReference,
            Box::new(|m, _| {
                m.insert(labels::ACQUISITION_KEY.into(), digest('f'));
            }),
        ),
        (
            "digest",
            IssueCode::CorruptArtifactHash,
            Box::new(|m, _| {
                m.insert(labels::VALUES_SHA256_KEY.into(), digest('0'));
            }),
        ),
    ];
    assert_tampered(cases);
}

#[test]
fn tampered_rows_are_reported_by_code() {
    let cases: Vec<(&str, IssueCode, Edit)> = vec![
        (
            "row acquisition",
            IssueCode::InvalidReference,
            Box::new(|_, b| {
                let rows = b[0].num_rows();
                b[0] = replace_column(
                    &b[0],
                    "acquisition_id",
                    Arc::new(StringArray::from(vec![digest('f'); rows])),
                );
            }),
        ),
        (
            "reordered",
            IssueCode::DuplicateEventIdentity,
            Box::new(|_, b| {
                let mut ids: Vec<u64> = (0..b[0].num_rows() as u64).collect();
                ids.swap(1, 2);
                b[0] = replace_column(&b[0], "source_event_id", Arc::new(UInt64Array::from(ids)));
            }),
        ),
        (
            "undecided gate",
            IssueCode::CorruptArtifactHash,
            Box::new(|_, b| {
                let mut cells: Vec<Option<bool>> = vec![Some(true); b[0].num_rows()];
                cells[4] = None;
                cells[9] = None;
                cells[0] = None;
                b[0] = replace_column(&b[0], "cells", Arc::new(BooleanArray::from(cells)));
            }),
        ),
        (
            "child outside parent",
            IssueCode::CorruptArtifactHash,
            Box::new(|_, b| {
                let cells: Vec<Option<bool>> = (0..b[0].num_rows())
                    .map(|i| (i % 5 != 4).then_some(i != 0))
                    .collect();
                b[0] = replace_column(&b[0], "cells", Arc::new(BooleanArray::from(cells)));
            }),
        ),
        (
            "foreign state",
            IssueCode::CorruptArtifactHash,
            Box::new(|_, b| {
                let mut states = b[0]
                    .column_by_name("state")
                    .unwrap()
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap()
                    .iter()
                    .map(|s| s.unwrap().to_owned())
                    .collect::<Vec<_>>();
                states[0] = "classified".into();
                b[0] = replace_column(&b[0], "state", Arc::new(StringArray::from(states)));
            }),
        ),
    ];
    assert_tampered(cases);
}

/// A classifier batch of the decision policy's every case.
fn classifier_rows() -> LabelBatch {
    let states = vec![
        LabelState::Classified,
        LabelState::Abstained,
        LabelState::Abstained,
        LabelState::QcRejected,
    ];
    let label = StringArray::from(vec![Some("blast"), None, None, None]);
    let reason = StringArray::from(vec![None, Some("low_confidence"), Some("ood"), None]);
    let confidence = Float32Array::from(vec![Some(0.9), Some(0.55), Some(0.8), None]);
    let uncertainty = Float32Array::from(vec![Some(0.1), Some(0.45), Some(0.2), None]);
    let score = Float32Array::from(vec![Some(0.1), Some(0.2), Some(3.5), None]);
    let ood = BooleanArray::from(vec![Some(false), Some(false), Some(true), None]);
    let blast = Float32Array::from(vec![Some(0.9), Some(0.45), Some(0.8), None]);
    let normal = Float32Array::from(vec![Some(0.1), Some(0.55), Some(0.2), None]);
    LabelBatch {
        first: 0,
        states,
        columns: vec![
            Arc::new(label),
            Arc::new(reason),
            Arc::new(confidence),
            Arc::new(uncertainty),
            Arc::new(score),
            Arc::new(ood),
            Arc::new(blast),
            Arc::new(normal),
        ],
    }
}

#[test]
fn classifier_tables_keep_abstentions_and_refuse_contradictions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("classifier.parquet");
    let written = write(
        &path,
        classifier_descriptor(4),
        vec![classifier_rows()],
        &LabelSettings::default(),
    )
    .unwrap();
    let states = &written.summary.states;
    assert_eq!(states[&LabelState::Abstained], 2);
    assert_eq!(states[&LabelState::Classified], 1);

    let contradictions: Vec<(&str, usize, ArrayRef)> = vec![
        // An abstention with a class.
        (
            "label",
            0,
            Arc::new(StringArray::from(vec![
                Some("blast"),
                Some("blast"),
                None,
                None,
            ])),
        ),
        // An OOD abstention without the OOD flag.
        (
            "is_ood",
            5,
            Arc::new(BooleanArray::from(vec![
                Some(false),
                Some(false),
                Some(false),
                None,
            ])),
        ),
        // A prediction for a QC-rejected event.
        (
            "p_blast",
            6,
            Arc::new(Float32Array::from(vec![
                Some(0.9),
                Some(0.45),
                Some(0.8),
                Some(0.5),
            ])),
        ),
        // OOD assessed for some events only.
        (
            "is_ood",
            5,
            Arc::new(BooleanArray::from(vec![
                None,
                Some(false),
                Some(true),
                None,
            ])),
        ),
        // A class the model does not have.
        (
            "label",
            0,
            Arc::new(StringArray::from(vec![Some("other"), None, None, None])),
        ),
    ];
    for (name, index, array) in contradictions {
        let mut batch = classifier_rows();
        batch.columns[index] = array;
        let refused = write(
            &directory.path().join("refused.parquet"),
            classifier_descriptor(4),
            vec![batch],
            &LabelSettings::default(),
        );
        assert!(refused.is_err(), "{name}");
    }
}
