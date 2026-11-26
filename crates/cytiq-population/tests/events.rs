//! `cytiq.events.v1`: export from FCS, verification and identity rules.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{Array, Float32Array, RecordBatch, StringArray, UInt64Array};
use cytiq_fcs::{FcsWriteOptions, FcsWriteParameter};
use cytiq_population::events::{
    self, BaseState, Definition, ExportOptions, LayerState, Origin, StepKind, ValueType,
};
use cytiq_population::issue::{IssueCode, Issues, Severity};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::Compression;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures")
        .join(name)
}

fn parameters(names: &[&str]) -> Vec<FcsWriteParameter> {
    names
        .iter()
        .map(|name| FcsWriteParameter::new(*name))
        .collect()
}

/// A small float32 FCS file with identifying TEXT keywords.
fn sample(
    directory: &Path,
    name: &str,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<f32>],
    keywords: &[(&str, &str)],
) -> PathBuf {
    let path = directory.join(name);
    let mut options = FcsWriteOptions::new()
        .with_text_keyword("$FIL", "Jane_Doe_MRN123.fcs")
        .with_text_keyword("$OP", "Dr. Example")
        .with_text_keyword("$DATE", "01-JAN-2024");
    for (key, value) in keywords {
        options = options.with_text_keyword(*key, *value);
    }
    cytiq_fcs::write_fcs3_1_f32_with_options(&path, parameters, columns, &options).unwrap();
    path
}

fn standard(directory: &Path, keywords: &[(&str, &str)]) -> PathBuf {
    let parameters = vec![
        FcsWriteParameter::new("FSC-A"),
        FcsWriteParameter::with_label("FL1-A", "CD3")
            .with_keyword("TAG", "FITC")
            .with_keyword("DET", "B530")
            .with_keyword("V", "450"),
        FcsWriteParameter::with_label("FL2-A", "CD4"),
    ];
    let columns = vec![
        vec![100.0, 200.0, 300.0, 400.0],
        vec![1.5, -2.25, 3.0, 0.0],
        vec![10.0, 20.0, 30.0, 40.0],
    ];
    sample(directory, "sample.fcs", &parameters, &columns, keywords)
}

fn export(input: &Path, options: &ExportOptions) -> (PathBuf, events::ExportSummary) {
    let output = input.with_extension("events.parquet");
    let summary = events::export(input, &output, options).unwrap();
    (output, summary)
}

fn verify(path: &Path) -> (Option<events::EventsSummary>, Issues) {
    let mut issues = Issues::new();
    let summary = events::verify_file(path, "/artifact", &mut issues);
    (summary, issues)
}

fn read_all(path: &Path) -> (Vec<RecordBatch>, Vec<KeyValue>, arrow_schema::SchemaRef) {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap()).unwrap();
    let metadata = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default();
    let schema = builder.schema().clone();
    let batches = builder
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (batches, metadata, schema)
}

/// Rewrite an events table, letting `change` alter its batches and metadata.
fn rewrite(
    path: &Path,
    output: &Path,
    compression: Compression,
    change: impl FnOnce(&mut Vec<RecordBatch>, &mut Vec<KeyValue>),
) {
    let (mut batches, mut metadata, schema) = read_all(path);
    metadata.retain(|kv| kv.key != "ARROW:schema");
    change(&mut batches, &mut metadata);
    let schema = batches.first().map_or(schema, RecordBatch::schema);
    let properties = WriterProperties::builder()
        .set_compression(compression)
        .set_key_value_metadata(Some(metadata))
        .build();
    let mut writer =
        ArrowWriter::try_new(File::create(output).unwrap(), schema, Some(properties)).unwrap();
    for batch in &batches {
        writer.write(batch).unwrap();
    }
    writer.close().unwrap();
}

/// The acquisition a cytiq stage records in the files it derives.
const ACQUISITION: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn layer(state: LayerState) -> ExportOptions {
    ExportOptions {
        layer: state,
        ..ExportOptions::default()
    }
}

fn errors(issues: &Issues) -> std::collections::BTreeSet<IssueCode> {
    issues.codes(Severity::Error)
}

#[test]
fn raw_events_round_trip_exactly_with_keys_in_every_row() {
    let directory = tempfile::tempdir().unwrap();
    let input = standard(directory.path(), &[]);
    let (output, summary) = export(&input, &ExportOptions::default());
    let acquisition = cytiq_provenance::hash_file(&input).unwrap().1;
    assert_eq!(summary.descriptor.acquisition_id, acquisition);
    assert_eq!(summary.descriptor.state, LayerState::Raw);
    assert_eq!(summary.descriptor.steps.len(), 0);
    assert_eq!(summary.rows, 4);
    assert_eq!(summary.raw_events, Some(4));

    let (verified, issues) = verify(&output);
    assert!(issues.items().is_empty(), "{:?}", issues.items());
    let verified = verified.unwrap();
    assert_eq!(verified.layer_id, summary.layer_id);
    assert_eq!(verified.values_sha256, summary.values_sha256);
    assert_eq!(verified.max_source_event_id, Some(3));

    let (batches, _, schema) = read_all(&output);
    let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(
        names,
        [
            "acquisition_id",
            "layer_id",
            "source_event_id",
            "FSC-A",
            "FL1-A",
            "FL2-A"
        ]
    );
    let batch = &batches[0];
    let acquisitions = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert!((0..4).all(|i| acquisitions.value(i) == acquisition));
    let ids = batch
        .column(2)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .unwrap();
    assert_eq!(ids.values().as_ref(), [0, 1, 2, 3]);
    let fl1 = batch
        .column(4)
        .as_any()
        .downcast_ref::<Float32Array>()
        .unwrap();
    assert_eq!(fl1.values().as_ref(), [1.5, -2.25, 3.0, 0.0]);

    // Detector, marker and reporter stay distinct field metadata.
    let field = schema.field(4);
    let metadata = field.metadata();
    assert_eq!(metadata["cytiq.channel.index"], "2");
    assert_eq!(metadata["cytiq.channel.label"], "CD3");
    assert_eq!(metadata["cytiq.channel.reporter"], "FITC");
    assert_eq!(metadata["cytiq.channel.detector"], "B530");
    assert_eq!(metadata["cytiq.channel.voltage"], "450");
    assert_eq!(metadata["cytiq.channel.kind"], "unspecified");
    assert!(
        !schema
            .field(3)
            .metadata()
            .contains_key("cytiq.channel.reporter")
    );
}

#[test]
fn identifying_keywords_never_reach_the_table() {
    let directory = tempfile::tempdir().unwrap();
    let input = standard(directory.path(), &[]);
    let (output, _) = export(&input, &ExportOptions::default());
    let bytes = std::fs::read(&output).unwrap();
    for needle in [
        "Jane_Doe",
        "MRN123",
        "Dr. Example",
        "01-JAN-2024",
        "$FIL",
        "$OP",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "`{needle}` leaked into the events table"
        );
    }
}

#[test]
fn integer_float_and_double_channels_keep_their_types() {
    let directory = tempfile::tempdir().unwrap();
    for (name, expected) in [
        (
            "fcs/fcs3_0_integer.fcs",
            vec![ValueType::Uint64, ValueType::Uint64],
        ),
        (
            "fcs/fcs3_2_mixed.fcs",
            vec![ValueType::Uint64, ValueType::Float32, ValueType::Float64],
        ),
    ] {
        let output = directory.path().join(name.replace('/', "-") + ".parquet");
        let summary = events::export(&fixture(name), &output, &ExportOptions::default()).unwrap();
        let types: Vec<ValueType> = summary
            .descriptor
            .channels
            .iter()
            .map(|c| c.value_type)
            .collect();
        assert_eq!(types, expected, "{name}");
        let (verified, issues) = verify(&output);
        assert!(verified.is_some(), "{name}: {:?}", issues.items());
    }
    // FCS 3.2 detector, type, analyte and feature keywords become field metadata.
    let output = directory.path().join("fcs-fcs3_2_mixed.fcs.parquet");
    let (_, _, schema) = read_all(&output);
    assert_eq!(
        schema.field(4).metadata()["cytiq.channel.detector"],
        "FITC PMT"
    );
    assert_eq!(
        schema.field(4).metadata()["cytiq.channel.kind"],
        "Raw Fluorescence"
    );
    assert_eq!(schema.field(5).metadata()["cytiq.channel.analyte"], "CD3");
    assert_eq!(
        schema.field(5).metadata()["cytiq.channel.feature"],
        "Height"
    );
}

#[test]
fn original_ids_become_source_event_ids_of_a_subset() {
    let directory = tempfile::tempdir().unwrap();
    let input = sample(
        directory.path(),
        "cleaned.fcs",
        &parameters(&["FSC-A", "Original_ID"]),
        &[vec![1.0, 2.0, 3.0], vec![2.0, 5.0, 9.0]],
        &[],
    );
    let error = events::export(
        &input,
        &directory.path().join("refused.parquet"),
        &ExportOptions::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("keep the filter as a mask"),
        "{error}"
    );
    let subset = ExportOptions {
        allow_subset: true,
        ..ExportOptions::default()
    };
    let (output, summary) = export(&input, &subset);
    assert_eq!(summary.raw_events, None);
    assert_eq!(
        summary.descriptor.channels.len(),
        1,
        "Original_ID is not a channel"
    );
    let (batches, _, _) = read_all(&output);
    let ids = batches[0]
        .column(2)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .unwrap();
    assert_eq!(ids.values().as_ref(), [1, 4, 8]);
    let (verified, issues) = verify(&output);
    assert_eq!(
        verified.unwrap().max_source_event_id,
        Some(8),
        "{:?}",
        issues.items()
    );

    let unordered = sample(
        directory.path(),
        "unordered.fcs",
        &parameters(&["FSC-A", "Original_ID"]),
        &[vec![1.0, 2.0], vec![5.0, 2.0]],
        &[],
    );
    let error =
        events::export(&unordered, &directory.path().join("u.parquet"), &subset).unwrap_err();
    assert!(error.to_string().contains("increase strictly"), "{error}");
}

#[test]
fn reserved_and_colliding_channel_names_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    for (names, expected) in [
        (vec!["FSC-A", "keep"], "reserved"),
        (vec!["FSC-A", "Source_Event_ID"], "reserved"),
        (vec!["FL1-A", "fl1-a"], "repeats ignoring case"),
    ] {
        let columns = vec![vec![1.0_f32]; names.len()];
        let input = sample(
            directory.path(),
            "names.fcs",
            &parameters(&names),
            &columns,
            &[],
        );
        let error = events::export(
            &input,
            &directory.path().join("n.parquet"),
            &ExportOptions::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains(expected), "{names:?}: {error}");
    }
}

#[test]
fn more_channels_than_a_table_holds_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..=events::MAX_CHANNELS)
        .map(|i| format!("C{i}"))
        .collect();
    let parameters: Vec<FcsWriteParameter> = names.iter().map(FcsWriteParameter::new).collect();
    let columns = vec![vec![1.0_f32]; names.len()];
    let input = sample(directory.path(), "wide.fcs", &parameters, &columns, &[]);
    let error = events::export(
        &input,
        &directory.path().join("w.parquet"),
        &ExportOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("channels exceed"), "{error}");
}

const SPILL: &str = "2,FL1-A,FL2-A,1,0.1,0.05,1";

#[test]
fn compensation_with_the_files_own_spillover_is_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let input = standard(
        directory.path(),
        &[
            ("CYTIQ_ACQUISITION", ACQUISITION),
            (
                "CYTIQ_COMPENSATION",
                r#"{"source":"fcs","channels":["FL1-A","FL2-A"]}"#,
            ),
            ("CYTIQ_SOURCE_SPILL", SPILL),
        ],
    );
    let (output, summary) = export(&input, &layer(LayerState::Compensated));
    assert_eq!(summary.descriptor.acquisition_id, ACQUISITION);
    assert_eq!(
        summary.source_sha256,
        cytiq_provenance::hash_file(&input).unwrap().1,
        "the source is the derived file, the acquisition its raw file"
    );
    let step = &summary.descriptor.steps[0];
    assert_eq!(summary.descriptor.state, LayerState::Compensated);
    assert_eq!(
        (step.kind, step.origin),
        (StepKind::Compensation, Origin::Recorded)
    );
    assert!(
        step.definition_sha256
            .as_deref()
            .is_some_and(cytiq_fcs::is_sha256)
    );
    let (_, _, schema) = read_all(&output);
    assert_eq!(
        schema.field(4).metadata()["cytiq.channel.steps"],
        "compensation"
    );
    assert!(
        !schema
            .field(3)
            .metadata()
            .contains_key("cytiq.channel.steps")
    );
}

#[test]
fn a_matrix_file_needs_its_definition_and_its_path_is_never_stored() {
    let directory = tempfile::tempdir().unwrap();
    let input = standard(
        directory.path(),
        &[
            ("CYTIQ_ACQUISITION", ACQUISITION),
            (
                "CYTIQ_COMPENSATION",
                r#"{"source":"/home/alice/study/matrix.csv","channels":["FL1-A"]}"#,
            ),
        ],
    );
    let output = directory.path().join("m.parquet");
    let error = events::export(&input, &output, &layer(LayerState::Compensated)).unwrap_err();
    assert!(
        error.to_string().contains("supply the compensation matrix"),
        "{error}"
    );

    let options = ExportOptions {
        compensation_matrix: Some(Definition {
            sha256: "e".repeat(64),
            verified: false,
        }),
        ..layer(LayerState::Compensated)
    };
    let summary = events::export(&input, &output, &options).unwrap();
    assert_eq!(summary.descriptor.steps[0].origin, Origin::Declared);
    let bytes = std::fs::read(&output).unwrap();
    assert!(
        !bytes.windows(5).any(|w| w == b"alice"),
        "the matrix path leaked"
    );
}

#[test]
fn processing_cannot_be_applied_twice_or_out_of_order() {
    let directory = tempfile::tempdir().unwrap();
    let compensated = standard(
        directory.path(),
        &[
            ("CYTIQ_ACQUISITION", ACQUISITION),
            (
                "CYTIQ_COMPENSATION",
                r#"{"source":"fcs","channels":["FL1-A"]}"#,
            ),
            ("CYTIQ_SOURCE_SPILL", SPILL),
        ],
    );
    let output = directory.path().join("x.parquet");
    let declared = ExportOptions {
        base_state: BaseState::Compensated,
        ..layer(LayerState::Compensated)
    };
    let error = events::export(&compensated, &output, &declared).unwrap_err();
    assert!(
        error.to_string().contains("declared already processed"),
        "{error}"
    );

    let normalized = standard(
        directory.path(),
        &[
            ("CYTIQ_ACQUISITION", ACQUISITION),
            (
                "CYTIQ_NORMALIZATION",
                &format!(
                    r#"{{"schema_version":1,"model_sha256":"{}","batch":"B1","panel_sha256":"{}","channels":["FL1-A"]}}"#,
                    "a".repeat(64),
                    "b".repeat(64)
                ),
            ),
        ],
    );
    let error = events::export(&normalized, &output, &layer(LayerState::Normalized)).unwrap_err();
    assert!(error.to_string().contains("transformed first"), "{error}");

    let unrelated = ExportOptions {
        unmixing_reference: Some(Definition {
            sha256: "c".repeat(64),
            verified: true,
        }),
        ..ExportOptions::default()
    };
    let raw = standard(directory.path(), &[]);
    let error = events::export(&raw, &output, &unrelated).unwrap_err();
    assert!(
        error.to_string().contains("not unmixed by cytiq"),
        "{error}"
    );
}

#[test]
fn externally_processed_files_carry_an_external_step() {
    let directory = tempfile::tempdir().unwrap();
    let input = standard(directory.path(), &[]);
    let options = ExportOptions {
        base_state: BaseState::Compensated,
        ..layer(LayerState::Compensated)
    };
    let (_, summary) = export(&input, &options);
    assert_eq!(summary.descriptor.state, LayerState::Compensated);
    assert_eq!(summary.descriptor.steps[0].origin, Origin::External);
    assert_eq!(summary.descriptor.steps[0].channels.len(), 0);
}

const ARCSINH: &str = r#"{"method":"arcsinh","parameters":{"cofactor":150},"files":[{"channels":["FL1-A","FL2-A"]}]}"#;

#[test]
fn layers_of_different_acquisitions_never_share_an_identity() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let a = standard(
        first.path(),
        &[
            ("CYTIQ_ACQUISITION", ACQUISITION),
            ("CYTIQ_TRANSFORM", ARCSINH),
        ],
    );
    // Same events, another acquisition.
    let other = "2".repeat(64);
    let b = standard(
        second.path(),
        &[
            ("CYTIQ_ACQUISITION", other.as_str()),
            ("CYTIQ_TRANSFORM", ARCSINH),
        ],
    );
    let (output_a, summary_a) = export(&a, &layer(LayerState::Transformed));
    let (output_b, summary_b) = export(&b, &layer(LayerState::Transformed));
    assert_ne!(
        summary_a.descriptor.acquisition_id,
        summary_b.descriptor.acquisition_id
    );
    assert_ne!(summary_a.layer_id, summary_b.layer_id);
    assert_ne!(summary_a.values_sha256, summary_b.values_sha256);
    assert_eq!(
        summary_a.recipe_id, summary_b.recipe_id,
        "same processing recipe"
    );
    // Both count rows from 0: the same source_event_id in two acquisitions.
    let ids = |path: &Path| {
        let (batches, _, _) = read_all(path);
        batches[0]
            .column(2)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap()
            .values()
            .to_vec()
    };
    assert_eq!(ids(&output_a), ids(&output_b));
    let transform = &summary_a.descriptor.steps[0];
    assert_eq!(transform.kind, StepKind::Transform);
    assert_eq!(
        transform.parameters,
        Some(serde_json::json!({"cofactor": 150.0}))
    );
}

#[test]
fn the_values_digest_does_not_depend_on_parquet_encoding() {
    let directory = tempfile::tempdir().unwrap();
    let input = standard(directory.path(), &[]);
    let (output, summary) = export(&input, &ExportOptions::default());
    let plain = directory.path().join("plain.parquet");
    rewrite(&output, &plain, Compression::UNCOMPRESSED, |_, _| {});
    assert_ne!(
        std::fs::read(&output).unwrap(),
        std::fs::read(&plain).unwrap()
    );
    let (verified, issues) = verify(&plain);
    assert_eq!(
        verified.unwrap().values_sha256,
        summary.values_sha256,
        "{:?}",
        issues.items()
    );
}

fn replace_column(batches: &mut [RecordBatch], index: usize, column: Arc<dyn Array>) {
    let batch = &batches[0];
    let mut columns = batch.columns().to_vec();
    columns[index] = column;
    batches[0] = RecordBatch::try_new(batch.schema(), columns).unwrap();
}

#[test]
fn tampered_tables_are_reported_by_code() {
    let directory = tempfile::tempdir().unwrap();
    let input = standard(directory.path(), &[]);
    let (output, _) = export(&input, &ExportOptions::default());
    let tampered = directory.path().join("t.parquet");
    let check = |code: IssueCode, change: &dyn Fn(&mut Vec<RecordBatch>, &mut Vec<KeyValue>)| {
        rewrite(&output, &tampered, Compression::UNCOMPRESSED, |b, m| {
            change(b, m);
        });
        let (summary, issues) = verify(&tampered);
        assert!(summary.is_none());
        assert!(
            errors(&issues).contains(&code),
            "{code:?}: {:?}",
            issues.items()
        );
    };
    check(IssueCode::CorruptArtifactHash, &|batches, _| {
        replace_column(
            batches,
            4,
            Arc::new(Float32Array::from(vec![1.5, -2.25, 3.0, 0.5])),
        );
    });
    check(IssueCode::InvalidReference, &|batches, _| {
        replace_column(
            batches,
            0,
            Arc::new(StringArray::from(vec!["f".repeat(64); 4])),
        );
    });
    check(IssueCode::DuplicateEventIdentity, &|batches, _| {
        replace_column(batches, 2, Arc::new(UInt64Array::from(vec![0, 1, 1, 3])));
    });
    check(IssueCode::UnsupportedSchema, &|_, metadata| {
        for kv in metadata
            .iter_mut()
            .filter(|kv| kv.key == events::SCHEMA_VERSION_KEY)
        {
            kv.value = Some("2".into());
        }
    });
    check(IssueCode::CorruptArtifactHash, &|_, metadata| {
        for kv in metadata
            .iter_mut()
            .filter(|kv| kv.key == events::LAYER_ID_KEY)
        {
            kv.value = Some("0".repeat(64));
        }
    });
    check(IssueCode::InconsistentChannelOrder, &|batches, _| {
        let batch = &batches[0];
        let schema = batch.schema();
        let order = [0, 1, 2, 5, 4, 3];
        let fields: Vec<_> = order.iter().map(|i| schema.field(*i).clone()).collect();
        let columns = order.iter().map(|i| batch.column(*i).clone()).collect();
        let schema = Arc::new(arrow_schema::Schema::new_with_metadata(
            fields,
            schema.metadata().clone(),
        ));
        batches[0] = RecordBatch::try_new(schema, columns).unwrap();
    });
}

/// A dictionary that contradicts the table is corrupt; one in another order is
/// an order problem.
#[test]
fn an_edited_channel_dictionary_is_corrupt() {
    let directory = tempfile::tempdir().unwrap();
    let input = standard(directory.path(), &[]);
    let (output, _) = export(&input, &ExportOptions::default());
    let tampered = directory.path().join("t.parquet");
    let codes = |change: &dyn Fn(&mut Vec<serde_json::Value>)| {
        rewrite(
            &output,
            &tampered,
            Compression::UNCOMPRESSED,
            |_, metadata| {
                for kv in metadata
                    .iter_mut()
                    .filter(|kv| kv.key == events::CHANNELS_KEY)
                {
                    let mut entries: Vec<serde_json::Value> =
                        serde_json::from_str(kv.value.as_deref().unwrap()).unwrap();
                    change(&mut entries);
                    kv.value = Some(serde_json::to_string(&entries).unwrap());
                }
            },
        );
        errors(&verify(&tampered).1)
    };
    let corrupt = [IssueCode::CorruptArtifactHash].into();
    assert_eq!(codes(&|_| {}), [].into(), "the rewrite itself is fine");
    assert_eq!(codes(&|entries| entries[0]["bits"] = "16".into()), corrupt);
    assert_eq!(
        codes(&|entries| entries[0]["label"] = "another".into()),
        corrupt
    );
    assert_eq!(
        codes(&|entries| entries[0]["value_type"] = "float64".into()),
        corrupt
    );
    // Without its entries nothing says what the columns are.
    assert!(codes(&Vec::clear).contains(&IssueCode::CorruptArtifactHash));
    assert!(codes(&|entries| entries.swap(0, 1)).contains(&IssueCode::InconsistentChannelOrder));
}

#[test]
fn files_that_are_not_events_tables_are_unsupported() {
    let directory = tempfile::tempdir().unwrap();
    let mask = directory.path().join("mask.parquet");
    cytiq_mask::EventMask::new("a".repeat(64), vec![true, false], None)
        .write_parquet(&mask)
        .unwrap();
    let (summary, issues) = verify(&mask);
    assert!(summary.is_none());
    assert_eq!(errors(&issues), [IssueCode::UnsupportedSchema].into());
    let garbage = directory.path().join("garbage.parquet");
    std::fs::write(&garbage, b"not parquet").unwrap();
    assert_eq!(
        errors(&verify(&garbage).1),
        [IssueCode::CorruptArtifactHash].into()
    );
    assert_eq!(
        errors(&verify(&directory.path().join("absent.parquet")).1),
        [IssueCode::MissingObject].into()
    );
}
