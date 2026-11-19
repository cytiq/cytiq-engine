use std::sync::Arc;

use bytes::Bytes;

use arrow_array::{ArrayRef, BooleanArray, RecordBatch, UInt64Array};
use arrow_schema::{Field, Schema};
use cytiq_mask::parquet::ParquetReadLimits;
use cytiq_mask::{ACQUISITION_KEY, EventMask};
use cytiq_publish::Existing;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use serde_json::json;

use super::*;

fn sha() -> String {
    "a".repeat(64)
}

fn generated(method: &str) -> ClassLabels {
    ClassLabels {
        evidence: Evidence::Generative,
        selection: Selection::AllEvents,
        method: method.into(),
        channels: Vec::new(),
        annotators: None,
        agreement: None,
    }
}

fn descriptor(classes: &[ArtifactClass]) -> TruthDescriptor {
    let mut described = TruthClasses::default();
    for class in classes {
        let labels = Some(generated("parity/gen/pipeline.py"));
        match class {
            ArtifactClass::Debris => described.debris = labels,
            ArtifactClass::Doublet => described.doublet = labels,
            ArtifactClass::Dead => described.dead = labels,
            ArtifactClass::TimeAnomaly => described.time_anomaly = labels,
            ArtifactClass::Saturated => described.saturated = labels,
        }
    }
    TruthDescriptor {
        annotation: Annotation {
            producer: "test".into(),
            protocol: "unit test".into(),
            reference: None,
        },
        classes: described,
    }
}

fn descriptor_json(classes: &[ArtifactClass]) -> String {
    serde_json::to_string(&descriptor(classes)).unwrap()
}

type Columns<'a> = Vec<(&'a str, ArrayRef, bool)>;

/// A Parquet file with arbitrary columns and footer, for the reader to judge.
fn parquet(columns: Columns<'_>, footer: Vec<(&str, Option<String>)>) -> Bytes {
    let schema = Arc::new(Schema::new(
        columns
            .iter()
            .map(|(name, array, nullable)| Field::new(*name, array.data_type().clone(), *nullable))
            .collect::<Vec<_>>(),
    ));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        columns.into_iter().map(|(_, array, _)| array).collect(),
    )
    .unwrap();
    let properties = WriterProperties::builder()
        .set_key_value_metadata(Some(
            footer
                .into_iter()
                .map(|(key, value)| KeyValue {
                    key: key.into(),
                    value,
                })
                .collect(),
        ))
        .build();
    let mut buffer = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buffer, schema, Some(properties)).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
    Bytes::from(buffer)
}

fn ids(values: &[u64]) -> ArrayRef {
    Arc::new(UInt64Array::from(values.to_vec()))
}

fn flags(values: &[Option<bool>]) -> ArrayRef {
    Arc::new(BooleanArray::from(values.to_vec()))
}

fn footer(descriptor: String) -> Vec<(&'static str, Option<String>)> {
    vec![
        (TRUTH_SCHEMA_VERSION_KEY, Some("1".into())),
        (ACQUISITION_KEY, Some(sha())),
        (TRUTH_DESCRIPTOR_KEY, Some(descriptor)),
    ]
}

fn read(bytes: Bytes) -> Result<ArtifactTruth, TruthError> {
    ArtifactTruth::read_from(bytes, ParquetReadLimits::default())
}

fn rejected(bytes: Bytes, expected: &str) {
    match read(bytes) {
        Err(TruthError::Invalid(message)) => assert!(
            message.contains(expected),
            "`{message}` does not mention `{expected}`"
        ),
        other => panic!("expected an invalid-truth error mentioning `{expected}`, got {other:?}"),
    }
}

/// A valid two-event debris table with a replaced footer entry.
fn with_footer(footer: Vec<(&'static str, Option<String>)>) -> Bytes {
    parquet(
        vec![
            ("source_event_id", ids(&[0, 1]), false),
            ("debris", flags(&[Some(true), None]), true),
        ],
        footer,
    )
}

#[test]
fn round_trips_values_and_descriptor_uncompressed() {
    let truth = ArtifactTruth::new(
        sha(),
        descriptor(&[ArtifactClass::Dead, ArtifactClass::Debris]),
        vec![
            (ArtifactClass::Dead, vec![Some(false), None, Some(true)]),
            (ArtifactClass::Debris, vec![Some(true), Some(false), None]),
        ],
    )
    .unwrap();
    assert_eq!(
        truth.classes[0].0,
        ArtifactClass::Debris,
        "sorted into class order"
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("truth.parquet");
    truth.write_parquet(&path, Existing::Refuse).unwrap();
    assert_eq!(ArtifactTruth::read_parquet(&path).unwrap(), truth);
    let file = std::fs::File::open(&path).unwrap();
    let metadata = parquet::file::reader::FileReader::metadata(
        &parquet::file::serialized_reader::SerializedFileReader::new(file).unwrap(),
    )
    .clone();
    for column in metadata.row_group(0).columns() {
        assert_eq!(column.compression(), Compression::UNCOMPRESSED);
    }
    assert!(matches!(
        truth.write_parquet(&path, Existing::Refuse),
        Err(TruthError::Publish(_))
    ));
}

#[test]
fn a_mask_is_not_truth_and_truth_is_not_a_mask() {
    let directory = tempfile::tempdir().unwrap();
    let mask_path = directory.path().join("mask.parquet");
    EventMask::new(sha(), vec![true, false], None)
        .with_filter("debris", vec![Some(true), Some(false)])
        .write_parquet(&mask_path)
        .unwrap();
    assert!(matches!(
        ArtifactTruth::read_parquet(&mask_path),
        Err(TruthError::Invalid(_))
    ));
    let truth_path = directory.path().join("truth.parquet");
    ArtifactTruth::new(
        sha(),
        descriptor(&[ArtifactClass::Debris]),
        vec![(ArtifactClass::Debris, vec![Some(true), None])],
    )
    .unwrap()
    .write_parquet(&truth_path, Existing::Refuse)
    .unwrap();
    assert!(EventMask::read_parquet(&truth_path).is_err());
    assert!(matches!(
        ArtifactTruth::read_parquet(&directory.path().join("missing.parquet")),
        Err(TruthError::Io(_))
    ));
}

#[test]
fn event_states_take_any_artifact_over_unlabelled_classes() {
    use EventTruth::{Artifact, Clean, Undetermined};
    let truth = ArtifactTruth::new(
        sha(),
        descriptor(&[ArtifactClass::Doublet, ArtifactClass::Dead]),
        vec![
            (
                ArtifactClass::Doublet,
                vec![Some(true), Some(true), Some(false), Some(false), None, None],
            ),
            (
                ArtifactClass::Dead,
                vec![Some(true), None, Some(false), None, Some(false), None],
            ),
        ],
    )
    .unwrap();
    assert_eq!(
        truth.event_states(),
        vec![
            Artifact,
            Artifact,
            Clean,
            Undetermined,
            Undetermined,
            Undetermined
        ]
    );
    assert_eq!(truth.events(), 6);
    assert_eq!(truth.class(ArtifactClass::Dead).unwrap()[0], Some(true));
    assert_eq!(truth.class(ArtifactClass::Debris), None);
}

#[test]
fn truth_class_follows_declared_evidence() {
    let labels = |evidence, annotators, agreement| ClassLabels {
        evidence,
        selection: Selection::RandomSubset,
        method: "m".into(),
        channels: vec!["FSC-A".into()],
        annotators,
        agreement,
    };
    let agreement = Some(Agreement {
        statistic: AgreementStatistic::CohenKappa,
        value: 0.9,
        events: 100,
    });
    let cases = [
        (
            labels(Evidence::Generative, None, None),
            TruthClass::Synthetic,
        ),
        (
            labels(Evidence::Definitional, None, None),
            TruthClass::Definitional,
        ),
        (
            labels(Evidence::OrthogonalAssay, None, None),
            TruthClass::Independent,
        ),
        (
            labels(Evidence::ExpertConsensus, Some(2), agreement.clone()),
            TruthClass::ExpertReference,
        ),
        (
            labels(Evidence::ExpertConsensus, Some(3), None),
            TruthClass::Exploratory,
        ),
        (
            labels(Evidence::SingleExpert, Some(1), agreement),
            TruthClass::Exploratory,
        ),
    ];
    for (labels, expected) in cases {
        labels.validate().unwrap();
        assert_eq!(labels.truth_class(), expected, "{labels:?}");
    }
}

fn consensus() -> ClassLabels {
    ClassLabels {
        evidence: Evidence::ExpertConsensus,
        selection: Selection::Targeted,
        method: "manual gating".into(),
        channels: vec!["FSC-A".into(), "SSC-A".into()],
        annotators: Some(2),
        agreement: None,
    }
}

fn agreement(statistic: AgreementStatistic, value: f64, events: u64) -> Agreement {
    Agreement {
        statistic,
        value,
        events,
    }
}

/// `consensus()` after `change` is refused with a message containing
/// `expected`.
fn rejects(expected: &str, change: impl FnOnce(&mut ClassLabels)) {
    let mut labels = consensus();
    change(&mut labels);
    let error = labels.validate().expect_err(expected);
    assert!(error.contains(expected), "`{error}` lacks `{expected}`");
}

#[test]
fn class_rules_reject_inconsistent_provenance() {
    consensus().validate().unwrap();
    rejects("`method`", |l| l.method = " ".into());
    rejects("channels", |l| {
        l.channels = vec!["x".into(); MAX_CHANNELS + 1];
    });
    rejects("channel name", |l| l.channels = vec![String::new()]);
    rejects("at least 2", |l| l.annotators = Some(1));
    rejects("at least 2", |l| l.annotators = None);
    rejects("`annotators: 1`", |l| l.evidence = Evidence::SingleExpert);
    rejects("`annotators: 1`", |l| {
        l.evidence = Evidence::SingleExpert;
        l.annotators = None;
    });
    rejects("expert evidence only", |l| {
        l.evidence = Evidence::Generative;
    });
    let mut nested = descriptor(&[ArtifactClass::Debris]);
    let mut labels = consensus();
    labels.annotators = Some(1);
    nested.classes.debris = Some(labels);
    assert!(nested.validate().unwrap_err().contains("class `debris`"));
}

#[test]
fn agreement_is_bounded_and_for_experts_only() {
    rejects("expert evidence only", |l| {
        l.evidence = Evidence::OrthogonalAssay;
        l.annotators = None;
        l.agreement = Some(agreement(AgreementStatistic::CohenKappa, 0.5, 10));
    });
    rejects("outside [-1, 1]", |l| {
        l.agreement = Some(agreement(AgreementStatistic::FleissKappa, 1.5, 10));
    });
    rejects("outside [-1, 1]", |l| {
        l.agreement = Some(agreement(
            AgreementStatistic::KrippendorffAlpha,
            f64::NAN,
            10,
        ));
    });
    rejects("outside [0, 1]", |l| {
        l.agreement = Some(agreement(AgreementStatistic::PercentAgreement, -0.1, 10));
    });
    rejects("no event", |l| {
        l.agreement = Some(agreement(AgreementStatistic::CohenKappa, 0.5, 0));
    });
    for edge in [
        Some(agreement(AgreementStatistic::CohenKappa, -1.0, 1)),
        Some(agreement(AgreementStatistic::CohenKappa, 1.0, 1)),
        Some(agreement(AgreementStatistic::PercentAgreement, 0.0, 1)),
        Some(agreement(AgreementStatistic::PercentAgreement, 1.0, 1)),
    ] {
        let mut labels = consensus();
        labels.agreement = edge;
        labels.validate().unwrap();
        let mut single = labels.clone();
        single.evidence = Evidence::SingleExpert;
        single.annotators = Some(1);
        single.validate().unwrap();
    }
}

#[test]
fn annotation_needs_a_producer_and_a_protocol() {
    let mut empty = descriptor(&[ArtifactClass::Debris]);
    empty.annotation.producer = String::new();
    assert!(empty.validate().unwrap_err().contains("`producer`"));
    let mut empty = descriptor(&[ArtifactClass::Debris]);
    empty.annotation.protocol = " ".into();
    assert!(empty.validate().unwrap_err().contains("`protocol`"));
    let mut empty = descriptor(&[ArtifactClass::Debris]);
    empty.annotation.reference = Some(String::new());
    assert!(empty.validate().unwrap_err().contains("reference"));
}

#[test]
fn constructor_rejects_inconsistent_columns() {
    let debris = descriptor(&[ArtifactClass::Debris]);
    let column = |values: Vec<Option<bool>>| (ArtifactClass::Debris, values);
    let cases: Vec<(&str, Result<ArtifactTruth, TruthError>)> = vec![
        (
            "SHA-256",
            ArtifactTruth::new("not-a-sha", debris.clone(), vec![column(vec![None])]),
        ),
        (
            "no artifact class",
            ArtifactTruth::new(sha(), debris.clone(), vec![]),
        ),
        (
            "repeated",
            ArtifactTruth::new(
                sha(),
                debris.clone(),
                vec![column(vec![None]), column(vec![None])],
            ),
        ),
        (
            "does not have",
            ArtifactTruth::new(
                sha(),
                descriptor(&[ArtifactClass::Debris, ArtifactClass::Dead]),
                vec![column(vec![None, None]), (ArtifactClass::Dead, vec![None])],
            ),
        ),
        (
            "exactly the labelled",
            ArtifactTruth::new(
                sha(),
                descriptor(&[ArtifactClass::Doublet]),
                vec![column(vec![None])],
            ),
        ),
    ];
    for (expected, result) in cases {
        match result {
            Err(TruthError::Invalid(message)) => {
                assert!(message.contains(expected), "`{message}` lacks `{expected}`");
            }
            other => panic!("{expected}: {other:?}"),
        }
    }
}

#[test]
fn reader_rejects_footers_outside_the_contract() {
    let good = descriptor_json(&[ArtifactClass::Debris]);
    read(with_footer(footer(good.clone()))).unwrap();
    let mut no_version = footer(good.clone());
    no_version.remove(0);
    rejected(with_footer(no_version), "not a `cytiq.qc.truth.v1` table");
    let mut version = footer(good.clone());
    version[0].1 = Some("2".into());
    rejected(with_footer(version), "unsupported");
    let mut repeated = footer(good.clone());
    repeated.push((TRUTH_SCHEMA_VERSION_KEY, Some("1".into())));
    rejected(with_footer(repeated), "more than once");
    let mut valueless = footer(good.clone());
    valueless[1].1 = None;
    rejected(with_footer(valueless), "has no value");
    let mut mask_key = footer(good.clone());
    mask_key.push(("cytiq.mask.schema_version", Some("2".into())));
    rejected(with_footer(mask_key), "unexpected footer key");
    let mut unknown = footer(good.clone());
    unknown.push(("cytiq.qc_truth.extra", Some("x".into())));
    rejected(with_footer(unknown), "unexpected footer key");
    let mut foreign = footer(good.clone());
    foreign.push(("ARROW:extra", Some("ignored".into())));
    read(with_footer(foreign)).unwrap();
    let mut no_acquisition = footer(good.clone());
    no_acquisition.remove(1);
    rejected(with_footer(no_acquisition), "no `cytiq.acquisition_sha256`");
    let mut bad_sha = footer(good.clone());
    bad_sha[1].1 = Some("A".repeat(64));
    rejected(with_footer(bad_sha), "SHA-256");
    let mut no_descriptor = footer(good);
    no_descriptor.remove(2);
    rejected(with_footer(no_descriptor), "no `cytiq.qc_truth.descriptor`");
}

#[test]
fn reader_rejects_descriptors_outside_the_contract() {
    let labels =
        json!({"evidence": "generative", "selection": "all_events", "method": "m", "channels": []});
    let annotation = json!({"producer": "p", "protocol": "q"});
    let duplicate = format!(
        r#"{{"annotation": {annotation}, "classes": {{"debris": {labels}, "debris": {labels}}}}}"#
    );
    rejected(with_footer(footer(duplicate)), "duplicate field");
    let unknown_class =
        json!({"annotation": annotation, "classes": {"debris": labels, "beads": labels}});
    rejected(
        with_footer(footer(unknown_class.to_string())),
        "unknown field",
    );
    let unknown_field =
        json!({"annotation": annotation, "classes": {"debris": labels}, "date": "2026"});
    rejected(
        with_footer(footer(unknown_field.to_string())),
        "unknown field",
    );
    let mut extra = labels.clone();
    extra["confidence"] = json!(0.9);
    let unknown_label = json!({"annotation": annotation, "classes": {"debris": extra}});
    rejected(
        with_footer(footer(unknown_label.to_string())),
        "unknown field",
    );
    let large = json!({
        "annotation": {"producer": "p", "protocol": "x".repeat(MAX_DESCRIPTOR_BYTES)},
        "classes": {"debris": labels}
    });
    rejected(with_footer(footer(large.to_string())), "more than 65536");
    let mismatch = json!({"annotation": annotation, "classes": {"doublet": labels}});
    rejected(
        with_footer(footer(mismatch.to_string())),
        "exactly the labelled",
    );
    let broken = json!({"annotation": annotation, "classes": {"debris": {
        "evidence": "single_expert", "selection": "targeted", "method": "m", "channels": []}}});
    rejected(with_footer(footer(broken.to_string())), "`annotators: 1`");
}

#[test]
fn reader_rejects_columns_outside_the_contract() {
    let good = || footer(descriptor_json(&[ArtifactClass::Debris]));
    let debris = || ("debris", flags(&[Some(true), None]), true);
    let cases: Vec<(&str, Columns<'_>)> = vec![
        (
            "0..rows",
            vec![("source_event_id", ids(&[0, 2]), false), debris()],
        ),
        (
            "0..rows",
            vec![("source_event_id", ids(&[0, 0]), false), debris()],
        ),
        (
            "0..rows",
            vec![("source_event_id", ids(&[1, 0]), false), debris()],
        ),
        (
            "non-null UInt64",
            vec![
                (
                    "source_event_id",
                    Arc::new(UInt64Array::from(vec![Some(0), None])),
                    true,
                ),
                debris(),
            ],
        ),
        ("no `source_event_id`", vec![debris()]),
        (
            "no artifact class",
            vec![("source_event_id", ids(&[0, 1]), false)],
        ),
        (
            "must be Boolean",
            vec![
                ("source_event_id", ids(&[0, 1]), false),
                ("debris", ids(&[0, 1]), false),
            ],
        ),
        (
            "must be UInt64",
            vec![("source_event_id", flags(&[None, None]), true), debris()],
        ),
        (
            "unexpected column `keep`",
            vec![
                ("source_event_id", ids(&[0, 1]), false),
                debris(),
                ("keep", flags(&[Some(true), Some(true)]), false),
            ],
        ),
        (
            "unexpected column `__index_level_0__`",
            vec![
                ("source_event_id", ids(&[0, 1]), false),
                debris(),
                ("__index_level_0__", ids(&[0, 1]), false),
            ],
        ),
        (
            "more than once",
            vec![("source_event_id", ids(&[0, 1]), false), debris(), debris()],
        ),
        (
            "more than once",
            vec![
                ("source_event_id", ids(&[0, 1]), false),
                ("source_event_id", ids(&[0, 1]), false),
                debris(),
            ],
        ),
    ];
    for (expected, columns) in cases {
        rejected(parquet(columns, good()), expected);
    }
}

#[test]
fn reader_accepts_an_empty_acquisition_and_non_nullable_classes() {
    let empty = parquet(
        vec![
            ("source_event_id", ids(&[]), false),
            ("debris", flags(&[]), true),
        ],
        footer(descriptor_json(&[ArtifactClass::Debris])),
    );
    let truth = read(empty).unwrap();
    assert_eq!(truth.events(), 0);
    assert_eq!(truth.event_states(), Vec::<EventTruth>::new());
    let strict = parquet(
        vec![
            ("source_event_id", ids(&[0]), false),
            ("debris", flags(&[Some(false)]), false),
        ],
        footer(descriptor_json(&[ArtifactClass::Debris])),
    );
    assert_eq!(
        read(strict).unwrap().event_states(),
        vec![EventTruth::Clean]
    );
}

#[test]
fn reader_enforces_parquet_limits() {
    let limits = ParquetReadLimits {
        rows: 1,
        ..ParquetReadLimits::default()
    };
    let bytes = with_footer(footer(descriptor_json(&[ArtifactClass::Debris])));
    assert!(matches!(
        ArtifactTruth::read_from(bytes, limits),
        Err(TruthError::Invalid(_))
    ));
}

#[test]
fn class_columns_round_trip_their_names() {
    for class in ArtifactClass::ALL {
        assert_eq!(ArtifactClass::from_column(class.column()), Some(class));
        assert_eq!(
            serde_json::to_value(class).unwrap(),
            json!(class.column()),
            "the serialized name is the column name"
        );
    }
    assert_eq!(ArtifactClass::from_column("margin"), None);
}
