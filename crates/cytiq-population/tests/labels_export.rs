//! `LabelsRunner`: native masks, memberships, predictions and `FlowSOM`
//! assignments of a synthetic acquisition, converted to label tables, and
//! every binding the export refuses.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{
    Array, ArrayRef, BooleanArray, Float32Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema};
use cytiq_mask::EventMask;
use cytiq_population::events::{self, ExportOptions, LayerState};
use cytiq_population::labels::{
    ClusterModel, ExportLabelsConfig, GatingDefinition, LabelKind, LabelSettings, LabelState,
    LabelsRunner, LabelsSummary, WrittenLabels,
};
use cytiq_provenance::{BuildInfo, FileRecord, Platform, RunRecord, hash_file};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::json;

/// Events of the synthetic acquisition.
const COUNT: usize = 12;
const EVENTS: u64 = COUNT as u64;

fn index(id: u64) -> usize {
    usize::try_from(id).unwrap()
}

fn float(value: u64) -> f32 {
    f32::from(u16::try_from(value).unwrap())
}

struct Acquisition {
    directory: tempfile::TempDir,
    raw: PathBuf,
    events: PathBuf,
    id: String,
}

impl Acquisition {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("raw.fcs");
        let parameters = ["FSC-A", "FL1-A"].map(cytiq_fcs::FcsWriteParameter::new);
        let columns: Vec<Vec<f32>> = (0..2)
            .map(|c| (0..EVENTS).map(|r| float(r) + 100.0 * float(c)).collect())
            .collect();
        cytiq_fcs::write_fcs3_1_f32_with_options(
            &raw,
            &parameters,
            &columns,
            &cytiq_fcs::FcsWriteOptions::new(),
        )
        .unwrap();
        let events = directory.path().join("raw.parquet");
        let options = ExportOptions {
            layer: LayerState::Raw,
            ..ExportOptions::default()
        };
        events::export(&raw, &events, &options).unwrap();
        let id = hash_file(&raw).unwrap().1;
        Self {
            directory,
            raw,
            events,
            id,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    /// A mask v2 of every raw event: `keep` and named filter columns.
    fn mask(&self, name: &str, keep: Vec<bool>, filters: &[(&str, Vec<Option<bool>>)]) -> PathBuf {
        self.mask_of(name, &self.id, keep, (0..EVENTS).collect(), filters)
    }

    fn mask_of(
        &self,
        name: &str,
        acquisition: &str,
        keep: Vec<bool>,
        ids: Vec<u64>,
        filters: &[(&str, Vec<Option<bool>>)],
    ) -> PathBuf {
        let path = self.path(name);
        let mut mask = EventMask::new(acquisition, keep, Some(ids));
        for (filter, values) in filters {
            mask = mask.with_filter(*filter, values.clone());
        }
        mask.write_parquet(&path).unwrap();
        path
    }

    /// The provenance record of `tool`, which read `inputs` by role and
    /// wrote `outputs`.
    fn record(
        &self,
        name: &str,
        tool: &str,
        inputs: &[(&str, &Path)],
        outputs: &[&Path],
        config: serde_json::Value,
    ) -> PathBuf {
        let file = |path: &Path, role: Option<&str>| {
            let (bytes, sha256) = hash_file(path).unwrap();
            FileRecord {
                path: path.display().to_string(),
                role: role.map(Into::into),
                bytes,
                sha256,
            }
        };
        let record = RunRecord {
            schema_version: cytiq_provenance::SCHEMA_VERSION,
            tool: tool.into(),
            version: "0.0.0".into(),
            build: BuildInfo::unknown(),
            platform: Platform::current(),
            arguments: Vec::new(),
            config,
            constants: json!({}),
            algorithms: BTreeMap::new(),
            started_at_unix_ms: 0,
            finished_at_unix_ms: 0,
            inputs: inputs
                .iter()
                .map(|(role, path)| file(path, Some(role)))
                .collect(),
            outputs: outputs.iter().map(|path| file(path, None)).collect(),
            execution_fingerprint: None,
            reason: None,
            parallelism: None,
        };
        let path = self.path(name);
        std::fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        path
    }

    fn config(&self, kind: LabelKind, input: &Path, record: &Path) -> ExportLabelsConfig {
        ExportLabelsConfig {
            kind,
            input: input.to_path_buf(),
            producer_provenance: record.to_path_buf(),
            events: self.events.clone(),
            qc_masks: Vec::new(),
            gating: None,
            cluster_model: None,
            probabilities: true,
            output: self.path(&format!("{}.labels.parquet", kind.as_str())),
            overwrite: false,
            settings: LabelSettings::default(),
        }
    }
}

/// Every fourth event is QC-rejected.
fn qc_keep() -> Vec<bool> {
    (0..EVENTS).map(|id| id % 4 != 3).collect()
}

fn run(config: &ExportLabelsConfig) -> cytiq_population::Result<WrittenLabels> {
    LabelsRunner::run(config)
}

fn refusal(config: &ExportLabelsConfig) -> String {
    let error = run(config).unwrap_err().to_string();
    assert!(!config.output.exists(), "a refusal published `{error}`");
    error
}

/// `(state, column values)` of every row, read back with the standard
/// arrow-rs reader.
fn rows(path: &Path, columns: &[&str]) -> Vec<(String, Vec<String>)> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let states = batch
            .column_by_name("state")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..batch.num_rows() {
            let cells = columns
                .iter()
                .map(|name| cell(batch.column_by_name(name).unwrap(), i))
                .collect();
            rows.push((states.value(i).to_owned(), cells));
        }
    }
    rows
}

fn cell(column: &ArrayRef, i: usize) -> String {
    if column.is_null(i) {
        return "null".to_owned();
    }
    let any = column.as_any();
    if let Some(values) = any.downcast_ref::<BooleanArray>() {
        values.value(i).to_string()
    } else if let Some(values) = any.downcast_ref::<UInt64Array>() {
        values.value(i).to_string()
    } else if let Some(values) = any.downcast_ref::<Float32Array>() {
        values.value(i).to_string()
    } else {
        any.downcast_ref::<StringArray>()
            .unwrap()
            .value(i)
            .to_owned()
    }
}

fn count(summary: &LabelsSummary, column: &str, value: Option<&str>, state: LabelState) -> u64 {
    summary
        .counts
        .iter()
        .find(|c| c.column == column && c.value.as_deref() == value && c.state == state)
        .map_or(0, |c| c.count)
}

/// A gating membership: `keep` from the QC mask; `cells` holds every kept
/// event, `CD3+` the even ones and `CD19+` multiples of three (overlapping
/// `CD3+`), both inside `cells`.
fn membership(acquisition: &Acquisition, keep: &[bool]) -> PathBuf {
    let gate = |member: fn(u64) -> bool| -> Vec<Option<bool>> {
        (0..EVENTS)
            .map(|id| keep[index(id)].then(|| member(id)))
            .collect()
    };
    acquisition.mask(
        "membership.parquet",
        keep.to_vec(),
        &[
            ("cells", gate(|_| true)),
            ("CD3+", gate(|id| id % 2 == 0)),
            ("CD19+", gate(|id| id % 3 == 0)),
        ],
    )
}

fn gating(acquisition: &Acquisition) -> GatingDefinition {
    let path = acquisition.path("gates.xml");
    std::fs::write(&path, "<Gating-ML/>").unwrap();
    GatingDefinition {
        path,
        gates: vec![
            ("cells".into(), None),
            ("CD3+".into(), Some("cells".into())),
            ("CD19+".into(), Some("cells".into())),
        ],
    }
}

/// A gate export with the QC mask the producer read, verified.
fn gate_config(acquisition: &Acquisition) -> ExportLabelsConfig {
    let qc = acquisition.mask("qc.parquet", qc_keep(), &[]);
    let native = membership(acquisition, &qc_keep());
    let gating = gating(acquisition);
    let record = acquisition.record(
        "gate.provenance.json",
        "cytiq gate",
        &[
            ("input", &acquisition.raw),
            ("gating_ml", &gating.path),
            ("mask", &qc),
        ],
        &[&native],
        json!({}),
    );
    ExportLabelsConfig {
        qc_masks: vec![qc],
        gating: Some(gating),
        ..acquisition.config(LabelKind::Gate, &native, &record)
    }
}

#[test]
fn gate_states_are_lossless_for_qc_rejected_not_assessed_excluded_and_accepted() {
    let acquisition = Acquisition::new();
    let config = gate_config(&acquisition);
    let written = run(&config).unwrap();
    let summary = &written.summary;
    assert_eq!(
        summary
            .descriptor
            .producer
            .input_layer_id
            .as_deref()
            .map(str::len),
        Some(64)
    );
    assert!(summary.descriptor.eligibility.verified);
    assert!(written.definition_id.is_some());
    assert_eq!(summary.states[&LabelState::Assessed], 9);
    assert_eq!(summary.states[&LabelState::QcRejected], 3);
    // Overlapping gates count independently: kept events 0, 2, 4, 6, 8 and
    // 10 are CD3+, and 0 and 6 are CD19+ as well.
    assert_eq!(
        count(summary, "CD3+", Some("true"), LabelState::Assessed),
        6
    );
    assert_eq!(
        count(summary, "CD19+", Some("true"), LabelState::Assessed),
        3
    );
    assert_eq!(
        count(summary, "CD19+", Some("false"), LabelState::Assessed),
        6
    );
    assert_eq!(count(summary, "cells", None, LabelState::QcRejected), 3);
    let table = rows(&config.output, &["CD3+", "CD19+"]);
    assert_eq!(
        table[0],
        ("assessed".into(), vec!["true".into(), "true".into()])
    );
    assert_eq!(
        table[1],
        ("assessed".into(), vec!["false".into(), "false".into()])
    );
    assert_eq!(
        table[3],
        ("qc_rejected".into(), vec!["null".into(), "null".into()])
    );

    // Without the QC masks, the same exclusions are not assessed.
    let unverified = ExportLabelsConfig {
        qc_masks: Vec::new(),
        output: acquisition.path("unverified.parquet"),
        ..config.clone()
    };
    let written = run(&unverified).unwrap();
    assert_eq!(written.summary.states[&LabelState::NotAssessed], 3);
    assert!(!written.summary.descriptor.eligibility.verified);
    assert_eq!(written.summary.descriptor.eligibility.masks.len(), 1);
}

#[test]
fn a_child_outside_its_parent_is_refused() {
    let acquisition = Acquisition::new();
    let config = gate_config(&acquisition);
    let keep = qc_keep();
    let native = acquisition.mask(
        "membership.parquet",
        keep.clone(),
        &[
            (
                "cells",
                (0..EVENTS)
                    .map(|id| keep[index(id)].then_some(id != 0))
                    .collect(),
            ),
            (
                "CD3+",
                (0..EVENTS)
                    .map(|id| keep[index(id)].then_some(id % 2 == 0))
                    .collect(),
            ),
            (
                "CD19+",
                (0..EVENTS)
                    .map(|id| keep[index(id)].then_some(false))
                    .collect(),
            ),
        ],
    );
    let gating = config.gating.clone().unwrap();
    let record = acquisition.record(
        "child.provenance.json",
        "cytiq gate",
        &[
            ("input", &acquisition.raw),
            ("gating_ml", &gating.path),
            ("mask", &config.qc_masks[0]),
        ],
        &[&native],
        json!({}),
    );
    let config = ExportLabelsConfig {
        input: native,
        producer_provenance: record,
        ..config
    };
    assert!(refusal(&config).contains("exceeds its denominator"));
}

#[test]
fn qc_rejected_requires_exactly_the_producers_masks() {
    let acquisition = Acquisition::new();
    let config = gate_config(&acquisition);
    let other = acquisition.mask("other.parquet", vec![true; COUNT], &[]);
    let wrong = ExportLabelsConfig {
        qc_masks: vec![other.clone()],
        ..config.clone()
    };
    assert!(refusal(&wrong).contains("exactly the masks the producer read"));
    let extra = ExportLabelsConfig {
        qc_masks: vec![config.qc_masks[0].clone(), other],
        ..config
    };
    assert!(refusal(&extra).contains("exactly the masks the producer read"));
}

#[test]
fn eligibility_that_disagrees_with_the_qc_mask_is_refused() {
    let acquisition = Acquisition::new();
    let config = gate_config(&acquisition);
    // The membership keeps every event although the QC mask rejects some.
    let native = membership(&acquisition, &[true; COUNT]);
    let gating = config.gating.clone().unwrap();
    let record = acquisition.record(
        "all.provenance.json",
        "cytiq gate",
        &[
            ("input", &acquisition.raw),
            ("gating_ml", &gating.path),
            ("mask", &config.qc_masks[0]),
        ],
        &[&native],
        json!({}),
    );
    let config = ExportLabelsConfig {
        input: native,
        producer_provenance: record,
        ..config
    };
    assert!(refusal(&config).contains("which the QC masks exclude"));
}

#[test]
fn native_that_omits_an_eligible_event_is_refused() {
    let acquisition = Acquisition::new();
    let qc = acquisition.mask("qc.parquet", qc_keep(), &[]);
    // FlowSOM leaves out masked events, and here also kept event 4.
    let assignments = acquisition.path("assignments.csv");
    let mut text = String::from("event_index,source_event_id,flowsom_node,flowsom_metacluster\n");
    for (index, id) in (0..EVENTS).filter(|id| id % 4 != 3 && *id != 4).enumerate() {
        writeln!(text, "{index},{id},1,1").unwrap();
    }
    std::fs::write(&assignments, text).unwrap();
    let record = acquisition.record(
        "flowsom.provenance.json",
        "cytiq flowsom",
        &[("input", &acquisition.raw), ("mask", &qc)],
        &[&assignments],
        json!({}),
    );
    let config = ExportLabelsConfig {
        qc_masks: vec![qc],
        ..acquisition.config(LabelKind::Cluster, &assignments, &record)
    };
    assert!(refusal(&config).contains("omits event 4"));
}

#[test]
fn duplicate_out_of_range_and_non_increasing_native_rows_are_refused() {
    let acquisition = Acquisition::new();
    for (name, ids) in [
        ("duplicate", vec![0, 1, 1]),
        ("descending", vec![0, 2, 1]),
        ("beyond", vec![0, 1, EVENTS]),
    ] {
        let assignments = acquisition.path(&format!("{name}.csv"));
        let mut text =
            String::from("event_index,source_event_id,flowsom_node,flowsom_metacluster\n");
        for (index, id) in ids.iter().enumerate() {
            writeln!(text, "{index},{id},1,1").unwrap();
        }
        std::fs::write(&assignments, text).unwrap();
        let record = acquisition.record(
            &format!("{name}.provenance.json"),
            "cytiq flowsom",
            &[("input", &acquisition.raw)],
            &[&assignments],
            json!({}),
        );
        let config = acquisition.config(LabelKind::Cluster, &assignments, &record);
        assert!(
            refusal(&config).contains("strictly increasing order"),
            "{name}"
        );
    }
}

#[test]
fn a_native_the_producer_record_does_not_list_is_refused() {
    let acquisition = Acquisition::new();
    let config = gate_config(&acquisition);
    let record = acquisition.record(
        "elsewhere.provenance.json",
        "cytiq gate",
        &[("input", &acquisition.raw)],
        &[&acquisition.events],
        json!({}),
    );
    let unlisted = ExportLabelsConfig {
        producer_provenance: record,
        ..config.clone()
    };
    assert!(refusal(&unlisted).contains("does not list"));
    let wrong_tool = ExportLabelsConfig {
        kind: LabelKind::Qc,
        gating: None,
        qc_masks: Vec::new(),
        ..config
    };
    assert!(refusal(&wrong_tool).contains("does not write qc labels"));
}

#[test]
fn a_native_of_another_acquisition_is_refused() {
    let acquisition = Acquisition::new();
    let native = acquisition.mask_of(
        "foreign.parquet",
        &"f".repeat(64),
        vec![true; COUNT],
        (0..EVENTS).collect(),
        &[("live", vec![Some(true); COUNT])],
    );
    let record = acquisition.record(
        "clean.provenance.json",
        "cytiq clean",
        &[("input", &acquisition.raw)],
        &[&native],
        json!({}),
    );
    let config = acquisition.config(LabelKind::Qc, &native, &record);
    assert!(refusal(&config).contains("acquisition or event-base mismatch"));
}

#[test]
fn a_subset_anchor_is_refused() {
    let acquisition = Acquisition::new();
    let config = gate_config(&acquisition);
    // An events export of a QC-filtered file: its rows are a subset.
    let filtered = acquisition.path("filtered.fcs");
    // `Original_ID` numbers raw events from 1.
    let ids: Vec<f32> = (0..EVENTS)
        .filter(|id| id % 2 == 0)
        .map(|id| float(id + 1))
        .collect();
    let parameters = ["FSC-A", "Original_ID"].map(cytiq_fcs::FcsWriteParameter::new);
    let options = cytiq_fcs::FcsWriteOptions::new()
        .with_text_keyword(cytiq_fcs::ACQUISITION_KEYWORD, acquisition.id.clone());
    cytiq_fcs::write_fcs3_1_f32_with_options(&filtered, &parameters, &[ids.clone(), ids], &options)
        .unwrap();
    let subset = acquisition.path("subset.parquet");
    let options = ExportOptions {
        layer: LayerState::Raw,
        allow_subset: true,
        ..ExportOptions::default()
    };
    events::export(&filtered, &subset, &options).unwrap();
    let config = ExportLabelsConfig {
        events: subset,
        ..config
    };
    assert!(refusal(&config).contains("row_set=all"));
}

#[test]
fn existing_outputs_and_missing_inputs_are_refused_before_any_work() {
    let acquisition = Acquisition::new();
    let config = gate_config(&acquisition);
    run(&config).unwrap();
    let error = run(&config).unwrap_err();
    assert!(matches!(
        error,
        cytiq_population::PopulationError::Publish(cytiq_publish::PublishError::Exists { .. })
    ));
    run(&ExportLabelsConfig {
        overwrite: true,
        ..config.clone()
    })
    .unwrap();
    let missing = ExportLabelsConfig {
        input: acquisition.path("absent.parquet"),
        output: acquisition.path("absent.labels.parquet"),
        ..config
    };
    assert!(matches!(
        run(&missing).unwrap_err(),
        cytiq_population::PopulationError::Missing(_)
    ));
}

#[test]
fn qc_masks_keep_their_filter_tri_states() {
    let acquisition = Acquisition::new();
    let live: Vec<Option<bool>> = (0..EVENTS)
        .map(|id| match id % 4 {
            3 => Some(false),
            2 => None,
            _ => Some(true),
        })
        .collect();
    let native = acquisition.mask("clean.parquet", qc_keep(), &[("live", live)]);
    let record = acquisition.record(
        "clean.provenance.json",
        "cytiq clean",
        &[("input", &acquisition.raw)],
        &[&native],
        json!({}),
    );
    let written = run(&acquisition.config(LabelKind::Qc, &native, &record)).unwrap();
    let summary = &written.summary;
    assert_eq!(written.definition_id, None);
    assert_eq!(summary.states[&LabelState::Accepted], 9);
    assert_eq!(summary.states[&LabelState::QcRejected], 3);
    assert_eq!(
        count(summary, "live", Some("false"), LabelState::QcRejected),
        3
    );
    assert_eq!(count(summary, "live", None, LabelState::Accepted), 3);
    assert_eq!(
        count(summary, "live", Some("true"), LabelState::Accepted),
        6
    );
}

/// A schema 5 row: `decision_status`, `abstain_reason`, `is_ood` and
/// `confidence`.
type Decision = (
    &'static str,
    Option<&'static str>,
    Option<bool>,
    Option<f32>,
);

/// Predictions of `classes` in the classifier's layout: `assessed`, a
/// probability per class, `label`, and schema 5's decision columns.
struct Predictions {
    classes: Vec<&'static str>,
    assessed: Vec<bool>,
    labels: Vec<Option<&'static str>>,
    probabilities: Vec<Vec<Option<f32>>>,
    /// `decision_status`, `abstain_reason`, `is_ood`, `confidence`.
    policy: Option<Vec<Decision>>,
}

impl Predictions {
    fn write(&self, acquisition: &Acquisition, name: &str) -> PathBuf {
        let rows = self.assessed.len();
        let mut metadata = std::collections::HashMap::from([
            (
                "cytiq.classify.schema_version".to_owned(),
                if self.policy.is_some() { "5" } else { "2" }.to_owned(),
            ),
            (
                "cytiq.acquisition_sha256".to_owned(),
                acquisition.id.clone(),
            ),
            ("cytiq.event_id_base".to_owned(), "0".to_owned()),
            (
                "cytiq.classify.vocabulary_sha256".to_owned(),
                "9".repeat(64),
            ),
            (
                "cytiq.classify.classes".to_owned(),
                serde_json::to_string(&self.classes).unwrap(),
            ),
        ]);
        let mut fields = vec![
            Field::new("event_id", DataType::UInt64, false),
            Field::new("source_event_id", DataType::UInt64, false),
            Field::new("assessed", DataType::Boolean, false),
        ];
        let mut columns: Vec<ArrayRef> = vec![
            Arc::new(UInt64Array::from_iter_values(0..rows as u64)),
            Arc::new(UInt64Array::from_iter_values(0..rows as u64)),
            Arc::new(BooleanArray::from(self.assessed.clone())),
        ];
        for (class, values) in self.classes.iter().zip(&self.probabilities) {
            fields.push(Field::new(format!("p_{class}"), DataType::Float32, true));
            columns.push(Arc::new(Float32Array::from(values.clone())));
        }
        fields.push(Field::new("label", DataType::Utf8, true));
        columns.push(Arc::new(StringArray::from(self.labels.clone())));
        if let Some(policy) = &self.policy {
            metadata.insert("cytiq.classify.bundle_schema_version".into(), "4".into());
            metadata.insert(
                "cytiq.classify.decision_policy_sha256".into(),
                policy_digest(acquisition),
            );
            let confidence: Vec<Option<f32>> = policy.iter().map(|row| row.3).collect();
            let uncertainty: Vec<Option<f32>> =
                confidence.iter().map(|c| c.map(|c| 1.0 - c)).collect();
            let ood: Vec<Option<bool>> = policy.iter().map(|row| row.2).collect();
            let score: Vec<Option<f32>> = ood
                .iter()
                .map(|o| o.map(|o| if o { 9.0 } else { 0.5 }))
                .collect();
            for (name, array) in [
                (
                    "confidence",
                    Arc::new(Float32Array::from(confidence)) as ArrayRef,
                ),
                ("uncertainty", Arc::new(Float32Array::from(uncertainty))),
                ("ood_score", Arc::new(Float32Array::from(score))),
            ] {
                fields.push(Field::new(name, DataType::Float32, true));
                columns.push(array);
            }
            fields.push(Field::new("is_ood", DataType::Boolean, true));
            columns.push(Arc::new(BooleanArray::from(ood)));
            fields.push(Field::new("decision_status", DataType::Utf8, false));
            columns.push(Arc::new(StringArray::from(
                policy.iter().map(|row| row.0).collect::<Vec<_>>(),
            )));
            fields.push(Field::new("abstain_reason", DataType::Utf8, true));
            columns.push(Arc::new(StringArray::from(
                policy.iter().map(|row| row.1).collect::<Vec<_>>(),
            )));
        }
        let schema = Arc::new(Schema::new_with_metadata(fields, metadata));
        let path = acquisition.path(name);
        let mut writer =
            ArrowWriter::try_new(File::create(&path).unwrap(), Arc::clone(&schema), None).unwrap();
        writer
            .write(&RecordBatch::try_new(schema, columns).unwrap())
            .unwrap();
        writer.close().unwrap();
        path
    }
}

fn policy_path(acquisition: &Acquisition) -> PathBuf {
    let path = acquisition.path("policy.json");
    if !path.exists() {
        std::fs::write(&path, "{}").unwrap();
    }
    path
}

fn policy_digest(acquisition: &Acquisition) -> String {
    hash_file(&policy_path(acquisition)).unwrap().1
}

fn classifier_config(
    acquisition: &Acquisition,
    predictions: &Predictions,
    margin: f64,
) -> ExportLabelsConfig {
    let native = predictions.write(acquisition, "predictions.parquet");
    let model = acquisition.path("model.onnx");
    std::fs::write(&model, "onnx").unwrap();
    let mut inputs: Vec<(&str, &Path)> = vec![("input", &acquisition.raw), ("model", &model)];
    let policy = policy_path(acquisition);
    if predictions.policy.is_some() {
        inputs.push(("decision_policy", &policy));
    }
    let record = acquisition.record(
        "classify.provenance.json",
        "cytiq classify",
        &inputs,
        &[&native],
        json!({"abstain_margin": margin}),
    );
    acquisition.config(LabelKind::Classifier, &native, &record)
}

fn binary_margin_predictions() -> Predictions {
    let p: Vec<Option<f32>> = (0..EVENTS)
        .map(|id| (id % 4 != 3).then(|| float(id) / 12.0))
        .collect();
    Predictions {
        classes: vec!["normal", "blast"],
        assessed: (0..EVENTS).map(|id| id % 4 != 3).collect(),
        labels: (0..EVENTS)
            .map(|id| match id % 4 {
                3 => None,
                0 => Some("unknown"),
                1 => Some("blast"),
                _ => Some("normal"),
            })
            .collect(),
        probabilities: vec![p.iter().map(|p| p.map(|p| 1.0 - p)).collect(), p],
        policy: None,
    }
}

#[test]
fn legacy_unknown_becomes_an_abstention_with_the_margin_reason() {
    let acquisition = Acquisition::new();
    let config = classifier_config(&acquisition, &binary_margin_predictions(), 0.1);
    let written = run(&config).unwrap();
    let summary = &written.summary;
    assert_eq!(summary.states[&LabelState::Abstained], 3);
    assert_eq!(summary.states[&LabelState::Classified], 6);
    assert_eq!(summary.states[&LabelState::NotAssessed], 3);
    assert_eq!(
        count(
            summary,
            "abstain_reason",
            Some("margin"),
            LabelState::Abstained
        ),
        3
    );
    assert_eq!(
        count(summary, "label", Some("blast"), LabelState::Classified),
        3
    );
    let definition = summary.descriptor.definition.as_ref().unwrap();
    assert_eq!(definition.parameters["abstain_margin"], json!(0.1));
}

#[test]
fn unknown_in_a_multiclass_prediction_is_refused() {
    let acquisition = Acquisition::new();
    let mut predictions = binary_margin_predictions();
    predictions.classes = vec!["normal", "blast", "other"];
    predictions.probabilities.push(vec![Some(0.0); COUNT]);
    let config = classifier_config(&acquisition, &predictions, 0.0);
    assert!(refusal(&config).contains("binary classifier's margin band"));
}

#[test]
fn probabilities_are_bit_exact_and_can_be_omitted() {
    let acquisition = Acquisition::new();
    let mut predictions = binary_margin_predictions();
    // Assessed events 0, 1, 2 and 4 (event 3 is excluded).
    let special = [
        (0, f32::from_bits(1)),
        (1, -0.0),
        (2, 0.1),
        (4, 1.0 - f32::EPSILON),
    ];
    for (id, value) in special {
        predictions.probabilities[1][id] = Some(value);
    }
    let config = classifier_config(&acquisition, &predictions, 0.1);
    run(&config).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&config.output).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let batch = reader.into_iter().next().unwrap().unwrap();
    let blast = batch
        .column_by_name("p_blast")
        .unwrap()
        .as_any()
        .downcast_ref::<Float32Array>()
        .unwrap();
    for (id, value) in special {
        assert_eq!(blast.value(id).to_bits(), value.to_bits());
    }

    let without = ExportLabelsConfig {
        probabilities: false,
        output: acquisition.path("without.parquet"),
        ..config
    };
    let written = run(&without).unwrap();
    assert_eq!(written.summary.descriptor.labels, []);
    assert_eq!(written.summary.descriptor.classes(), ["normal", "blast"]);
    let schema = ParquetRecordBatchReaderBuilder::try_new(File::open(&without.output).unwrap())
        .unwrap()
        .schema()
        .clone();
    assert!(schema.field_with_name("p_blast").is_err());
}

#[test]
fn policy_abstentions_keep_reason_and_ood_flag() {
    let acquisition = Acquisition::new();
    let decisions = (0..EVENTS)
        .map(|id| match id % 4 {
            3 => ("not_assessed", None, None, None),
            0 => ("abstained", Some("low_confidence"), Some(false), Some(0.5)),
            1 => ("abstained", Some("ood"), Some(true), Some(0.9)),
            _ => ("classified", None, Some(false), Some(0.9)),
        })
        .collect::<Vec<_>>();
    let predictions = Predictions {
        classes: vec!["normal", "blast"],
        assessed: (0..EVENTS).map(|id| id % 4 != 3).collect(),
        labels: (0..EVENTS)
            .map(|id| (id % 4 == 2).then_some("normal"))
            .collect(),
        probabilities: vec![
            (0..EVENTS).map(|id| (id % 4 != 3).then_some(0.9)).collect(),
            (0..EVENTS).map(|id| (id % 4 != 3).then_some(0.1)).collect(),
        ],
        policy: Some(decisions),
    };
    let config = ExportLabelsConfig {
        qc_masks: Vec::new(),
        ..classifier_config(&acquisition, &predictions, 0.0)
    };
    let written = run(&config).unwrap();
    let summary = &written.summary;
    assert_eq!(
        count(
            summary,
            "abstain_reason",
            Some("ood"),
            LabelState::Abstained
        ),
        3
    );
    assert_eq!(
        count(
            summary,
            "abstain_reason",
            Some("low_confidence"),
            LabelState::Abstained
        ),
        3
    );
    assert_eq!(
        count(summary, "is_ood", Some("true"), LabelState::Abstained),
        3
    );
    assert_eq!(count(summary, "is_ood", None, LabelState::NotAssessed), 3);
    let definition = summary.descriptor.definition.as_ref().unwrap();
    assert_eq!(
        definition.inputs["decision_policy"],
        [policy_digest(&acquisition)]
    );
    let table = rows(
        &config.output,
        &["label", "abstain_reason", "is_ood", "confidence"],
    );
    assert_eq!(
        table[1],
        (
            "abstained".into(),
            vec!["null".into(), "ood".into(), "true".into(), "0.9".into()]
        )
    );
}

fn assignments(acquisition: &Acquisition, node_of: impl Fn(u64) -> (u64, u64)) -> PathBuf {
    let path = acquisition.path("assignments.csv");
    let mut text = String::from("event_index,source_event_id,flowsom_node,flowsom_metacluster\n");
    for (index, id) in (0..EVENTS).filter(|id| id % 4 != 3).enumerate() {
        let (node, metacluster) = node_of(id);
        writeln!(text, "{index},{id},{node},{metacluster}").unwrap();
    }
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn bare_flowsom_labels_have_no_definition_id() {
    let acquisition = Acquisition::new();
    let qc = acquisition.mask("qc.parquet", qc_keep(), &[]);
    let native = assignments(&acquisition, |id| (id % 4 + 1, id % 2 + 1));
    let record = acquisition.record(
        "flowsom.provenance.json",
        "cytiq flowsom",
        &[("input", &acquisition.raw), ("mask", &qc)],
        &[&native],
        json!({}),
    );
    let config = ExportLabelsConfig {
        qc_masks: vec![qc],
        ..acquisition.config(LabelKind::Cluster, &native, &record)
    };
    let written = run(&config).unwrap();
    assert_eq!(written.definition_id, None);
    assert_eq!(written.summary.states[&LabelState::Classified], 9);
    assert_eq!(written.summary.states[&LabelState::QcRejected], 3);
    let table = rows(&config.output, &["metacluster", "node"]);
    assert_eq!(
        table[2],
        ("classified".into(), vec!["1".into(), "3".into()])
    );
    assert_eq!(
        table[3],
        ("qc_rejected".into(), vec!["null".into(), "null".into()])
    );
}

#[test]
fn nodes_must_belong_to_their_model_metacluster() {
    let acquisition = Acquisition::new();
    let model = ClusterModel {
        path: acquisition.path("model.json"),
        nodes: 4,
        metaclusters: 2,
        node_metaclusters: vec![1, 1, 2, 2],
    };
    std::fs::write(&model.path, "{}").unwrap();
    let config = |native: &Path, name: &str| {
        let record = acquisition.record(
            &format!("{name}.provenance.json"),
            "cytiq flowsom apply",
            &[("input", &acquisition.raw), ("model", &model.path)],
            &[native],
            json!({}),
        );
        ExportLabelsConfig {
            cluster_model: Some(model.clone()),
            output: acquisition.path(&format!("{name}.labels.parquet")),
            ..acquisition.config(LabelKind::Cluster, native, &record)
        }
    };
    let consistent = assignments(&acquisition, |id| {
        let node = id % 4 + 1;
        (node, if node <= 2 { 1 } else { 2 })
    });
    let written = run(&config(&consistent, "consistent")).unwrap();
    assert!(written.definition_id.is_some());
    assert_eq!(written.summary.states[&LabelState::NotAssessed], 3);

    let inconsistent = assignments(&acquisition, |id| (id % 4 + 1, 1));
    assert!(refusal(&config(&inconsistent, "inconsistent")).contains("another metacluster"));
}

#[test]
fn assignments_without_an_acquisition_binding_are_refused() {
    let acquisition = Acquisition::new();
    // FlowSOM read a transformed file, not the table's: nothing ties the
    // assignments to this acquisition.
    let other = acquisition.path("transformed.fcs");
    std::fs::write(&other, "not the raw file").unwrap();
    let native = assignments(&acquisition, |_| (1, 1));
    let record = acquisition.record(
        "flowsom.provenance.json",
        "cytiq flowsom",
        &[("input", &other)],
        &[&native],
        json!({}),
    );
    let config = acquisition.config(LabelKind::Cluster, &native, &record);
    assert!(refusal(&config).contains("name no acquisition"));
}
