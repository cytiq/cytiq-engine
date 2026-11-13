//! Streaming adapters for existing membership and classifier Parquet files.

use arrow_array::{Array, BooleanArray, RecordBatch, StringArray, UInt64Array};
use cytiq_provenance::{RunRecord, hash_file};
mod definition;
use arrow_schema::Schema;
use cytiq_mask::parquet::{ParquetReadLimits, ScalarParquetReader};
use definition::validate_classifier_definition;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::{Path, PathBuf},
};

use crate::{
    ArtifactRef, Denominator, Measure, Method, ObservationStatus, PopulationObservation, Result,
    Target, TargetKind, knowledge::KnowledgeArtifact, observation::Lineage, require, serialization,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationFormat {
    GateMembership,
    ClassifyPredictions,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub label: String,
    pub target: Target,
    /// Gate label in the same source; None means all assessed scope events.
    #[serde(default)]
    pub denominator: Option<String>,
}

/// Semantic identity of a knowledge artifact, independent of JSON/YAML bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRef {
    pub path: PathBuf,
    pub sha256: String,
}

impl KnowledgeRef {
    /// # Errors
    /// Rejects invalid or changed knowledge artifacts.
    pub fn load(&self) -> Result<KnowledgeArtifact> {
        let artifact = KnowledgeArtifact::load(&self.path)?;
        require(
            artifact.sha256 == self.sha256,
            "knowledge reference checksum mismatch",
        )?;
        Ok(artifact)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDefinition {
    pub format: AnnotationFormat,
    /// Gating-ML file or model.onnx; always a byte digest.
    pub definition: ArtifactRef,
    #[serde(default)]
    pub bundle_manifest: Option<ArtifactRef>,
    #[serde(default)]
    pub abstain_margin: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_policy: Option<ArtifactRef>,
    #[serde(default)]
    pub vocabulary: Option<KnowledgeRef>,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

impl SourceDefinition {
    /// Resolve a vocabulary into targets; dataset label mappings are not gate mappings.
    /// # Errors
    /// Rejects incompatible projections and ambiguous or invalid bindings.
    /// # Panics
    /// Never: knowledge artifacts can only be constructed by validated readers.
    pub fn resolve(&mut self, projection: &KnowledgeArtifact) -> Result<()> {
        self.definition.verify()?;
        require(
            projection.value["artifact_type"] == "panel_projection",
            "source requires a panel projection",
        )?;
        match self.format {
            AnnotationFormat::GateMembership => {
                require(
                    self.vocabulary.is_none()
                        && self.bundle_manifest.is_none()
                        && self.decision_policy.is_none()
                        && self.abstain_margin == 0.0
                        && !self.bindings.is_empty(),
                    "gating requires explicit bindings and no vocabulary",
                )?;
                let ids: BTreeSet<_> = projection.value["tree"]["cells"]
                    .as_array()
                    .expect("validated tree")
                    .iter()
                    .map(|cell| cell["id"].as_str().expect("validated id"))
                    .collect();
                for binding in &self.bindings {
                    binding.target.validate()?;
                    require(
                        binding.target.kind == TargetKind::Population
                            && ids.contains(binding.target.id.as_str())
                            && ids.contains(binding.target.scope.as_str()),
                        "gate binding requires known population and scope",
                    )?;
                    require(
                        projection.value["populations"]
                            .as_array()
                            .expect("validated projection")
                            .iter()
                            .any(|entry| {
                                entry["population"] == binding.target.id
                                    && entry["status"] == "resolvable"
                            }),
                        "gate target is not resolvable in the panel projection",
                    )?;
                }
            }
            AnnotationFormat::ClassifyPredictions => {
                let vocabulary = self
                    .vocabulary
                    .as_ref()
                    .ok_or_else(|| {
                        crate::PopulationError::Invalid(
                            "classifier source requires a vocabulary".into(),
                        )
                    })?
                    .load()?;
                let v = &vocabulary.value;
                validate_classifier_definition(self, projection, &vocabulary)?;
                require(
                    v["artifact_type"] == "class_vocabulary"
                        && v["projection_sha256"] == projection.sha256
                        && v["panel_sha256"] == projection.value["panel_sha256"]
                        && v["tree_sha256"] == projection.value["tree_sha256"]
                        && v["registry_sha256"]
                            == serialization::digest(&projection.value["tree"]["registry"]),
                    "classifier/projection knowledge mismatch",
                )?;
                let bindings = v["classes"]
                    .as_array()
                    .expect("validated classes")
                    .iter()
                    .map(|class| {
                        Ok(Binding {
                            label: class["id"].as_str().expect("validated id").to_owned(),
                            target: Target {
                                kind: serde_json::from_value(class["kind"].clone())?,
                                id: if class["kind"] == "residual" {
                                    class["id"].as_str()
                                } else {
                                    class["target"].as_str()
                                }
                                .expect("validated target")
                                .to_owned(),
                                scope: v["scope"].as_str().expect("validated scope").to_owned(),
                                state: class["state"].as_bool(),
                            },
                            denominator: None,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                require(
                    self.bindings.is_empty() || self.bindings == bindings,
                    "classifier bindings must match the vocabulary",
                )?;
                self.bindings = bindings;
            }
        }
        validate_bindings(&self.bindings)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationContext {
    pub specimen_id: String,
    pub manifest_sha256: String,
    pub panel_id: String,
    pub acquisition_id: String,
    pub acquisition_sha256: String,
    pub raw_events: u64,
    pub source_id: String,
    pub projection: KnowledgeRef,
    pub source: SourceDefinition,
}

#[derive(Clone, Debug)]
pub struct ObserveConfig {
    pub context: ObservationContext,
    pub annotations: PathBuf,
    pub producer_provenance: PathBuf,
    pub mask: PathBuf,
    pub qc_report: PathBuf,
    pub qc_provenance: PathBuf,
}

impl ObserveConfig {
    #[must_use]
    pub fn input_files(&self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.annotations.clone(),
            self.producer_provenance.clone(),
            self.mask.clone(),
            self.qc_report.clone(),
            self.qc_provenance.clone(),
            self.context.projection.path.clone(),
            self.context.source.definition.path.clone(),
        ];
        paths.extend(
            self.context
                .source
                .vocabulary
                .iter()
                .map(|v| v.path.clone()),
        );
        paths.extend(
            self.context
                .source
                .bundle_manifest
                .iter()
                .map(|v| v.path.clone()),
        );
        paths
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventAnnotations {
    pub schema_version: u32,
    pub context: ObservationContext,
    pub data: ArtifactRef,
    pub producer_provenance: ArtifactRef,
    pub analysis_mask: ArtifactRef,
    pub technical_qc: ArtifactRef,
    pub technical_qc_provenance: ArtifactRef,
    pub technical_status: String,
    pub assessed_events: u64,
    pub unknown_events: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstained_events: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ood_events: Option<u64>,
}

/// Produce per-source observations without changing or materializing annotation files.
/// # Errors
/// Rejects broken provenance, wrong acquisitions, incomplete coverage and invalid memberships.
pub fn observe(config: &ObserveConfig) -> Result<(EventAnnotations, Vec<PopulationObservation>)> {
    let (descriptor, observations, _) = observe_with_release(config)?;
    Ok((descriptor, observations))
}

/// Observe and return a release assessment bound to the manifest and mask.
/// Specimen reconciliation consumes this snapshot without event-level I/O.
/// # Errors
/// Returns the same lineage, provenance and policy errors as [`observe`].
pub fn observe_with_release(
    config: &ObserveConfig,
) -> Result<(
    EventAnnotations,
    Vec<PopulationObservation>,
    serde_json::Value,
)> {
    let projection = config.context.projection.load()?;
    let mut context = config.context.clone();
    context.source.resolve(&projection)?;
    require(
        context.raw_events > 0 && context.raw_events <= 1_000_000_000,
        "invalid or excessive raw event count",
    )?;
    let data = ArtifactRef::of(&config.annotations)?;
    let mask = ArtifactRef::of(&config.mask)?;
    let producer_ref = ArtifactRef::of(&config.producer_provenance)?;
    let producer = producer_record(&config.producer_provenance)?;
    validate_producer(&context, &producer, &data, &mask)?;
    let qc_ref = ArtifactRef::of(&config.qc_report)?;
    let qc_provenance = ArtifactRef::of(&config.qc_provenance)?;
    let qc_record = producer_record(&config.qc_provenance)?;
    require(
        qc_record.tool == "cytiq qc" && qc_record.outputs.iter().any(|f| f.sha256 == qc_ref.sha256),
        "QC provenance mismatch",
    )?;
    check_single_mask(&qc_record, &mask)?;
    require(
        qc_record
            .inputs
            .iter()
            .any(|f| f.role.as_deref() == Some("manifest") && f.sha256 == context.manifest_sha256),
        "technical QC used a different manifest from the specimen plan",
    )?;
    let qc = serialization::read(&config.qc_report)?;
    let mut status = qc_status(&qc, &context)?;
    let eligible = read_mask(&config.mask, &context)?;
    let AnnotationCounts {
        counts,
        assessed,
        unknown,
        abstained,
        ood,
    } = count_annotations(&config.annotations, &context, &eligible)?;
    require(
        qc["metrics"]["kept"].as_u64() == Some(assessed),
        "QC and annotation assessment counts disagree",
    )?;
    let release_checks = release_checks(&context, &qc_record, &config.mask)?;
    status = match release_checks.iter().map(|check| check.status).max() {
        Some(cytiq_qc::CheckStatus::Fail) => "fail",
        Some(cytiq_qc::CheckStatus::Warn) if status == "pass" => "warn",
        _ => status,
    };
    let descriptor = EventAnnotations {
        schema_version: if context.source.decision_policy.is_some() {
            2
        } else {
            1
        },
        abstained_events: context.source.decision_policy.as_ref().map(|_| abstained),
        ood_events: context.source.decision_policy.as_ref().map(|_| ood),
        context,
        data,
        producer_provenance: producer_ref,
        analysis_mask: mask,
        technical_qc: qc_ref,
        technical_qc_provenance: qc_provenance,
        technical_status: status.to_owned(),
        assessed_events: assessed,
        unknown_events: unknown,
    };
    let observations = observations(&descriptor, &projection, &counts)?;
    let assessment = serde_json::json!({"schema_version":1,"algorithm":"panel-qc-release-v1",
        "manifest_sha256":descriptor.context.manifest_sha256,"mask_sha256":descriptor.analysis_mask.sha256,
        "acquisition":descriptor.context.acquisition_sha256,"sample":descriptor.context.acquisition_id,
        "panel":descriptor.context.panel_id,
        "status":release_checks.iter().map(|check|check.status).max().unwrap_or(cytiq_qc::CheckStatus::Fail),
        "checks":release_checks});
    Ok((descriptor, observations, assessment))
}

fn validate_producer(
    context: &ObservationContext,
    producer: &RunRecord,
    data: &ArtifactRef,
    mask: &ArtifactRef,
) -> Result<()> {
    let expected_tool = match context.source.format {
        AnnotationFormat::GateMembership => "cytiq gate",
        AnnotationFormat::ClassifyPredictions => "cytiq classify",
    };
    require(
        producer.tool == expected_tool && producer.outputs.iter().any(|f| f.sha256 == data.sha256),
        "producer does not describe annotations",
    )?;
    require(
        producer
            .inputs
            .iter()
            .any(|f| f.sha256 == context.source.definition.sha256),
        "producer definition checksum mismatch",
    )?;
    check_single_mask(producer, mask)?;
    if let Some(bundle) = &context.source.bundle_manifest {
        require(
            producer
                .inputs
                .iter()
                .any(|f| f.role.as_deref() == Some("model") && f.sha256 == bundle.sha256)
                && producer.config["abstain_margin"].as_f64()
                    == Some(context.source.abstain_margin),
            "classifier bundle or abstain configuration differs from the plan",
        )?;
    }
    if let Some(policy) = &context.source.decision_policy {
        policy.verify()?;
        require(
            producer
                .inputs
                .iter()
                .any(|f| f.role.as_deref() == Some("decision_policy") && f.sha256 == policy.sha256),
            "classifier decision policy differs from the plan",
        )?;
    } else {
        require(
            producer
                .inputs
                .iter()
                .all(|f| f.role.as_deref() != Some("decision_policy")),
            "classifier used an unplanned decision policy",
        )?;
    }
    Ok(())
}

fn producer_record(path: &Path) -> Result<RunRecord> {
    let record: RunRecord = serde_json::from_value(serialization::read(path)?)?;
    require(
        (cytiq_provenance::MIN_READ_SCHEMA_VERSION..=cytiq_provenance::SCHEMA_VERSION)
            .contains(&record.schema_version),
        "unsupported producer provenance version",
    )?;
    Ok(record)
}

fn check_single_mask(record: &RunRecord, mask: &ArtifactRef) -> Result<()> {
    let masks = record
        .inputs
        .iter()
        .filter(|f| f.role.as_deref() == Some("mask"))
        .collect::<Vec<_>>();
    require(
        masks.len() == 1 && masks[0].sha256 == mask.sha256,
        "P1 requires one identical final combined mask for annotations and technical QC",
    )
}

/// Masks and annotation tables are external inputs: read them through the
/// bounded scalar reader, which rejects malformed or over-budget pages before
/// the decoder allocates from their declared sizes.
pub(crate) fn open_table(path: &Path) -> Result<ScalarParquetReader<File>> {
    Ok(ScalarParquetReader::new(
        File::open(path)?,
        ParquetReadLimits::default(),
    )?)
}

fn metadata(schema: &Schema, context: &ObservationContext) -> Result<()> {
    require(
        schema.metadata().get("cytiq.acquisition_sha256") == Some(&context.acquisition_sha256)
            && schema
                .metadata()
                .get("cytiq.event_id_base")
                .map(String::as_str)
                == Some("0"),
        "annotation acquisition or event-base mismatch",
    )
}

struct Bitmap {
    words: Vec<u64>,
    len: usize,
}

impl Bitmap {
    fn get(&self, index: usize) -> bool {
        self.words[index / 64] & (1_u64 << (index % 64)) != 0
    }
    fn set(&mut self, index: usize, value: bool) {
        let bit = 1_u64 << (index % 64);
        if value {
            self.words[index / 64] |= bit;
        } else {
            self.words[index / 64] &= !bit;
        }
    }
    fn len(&self) -> usize {
        self.len
    }
}

fn booleans(size: usize) -> Result<Bitmap> {
    let mut values = Vec::new();
    let words = size.div_ceil(64);
    values.try_reserve_exact(words).map_err(|_| {
        crate::PopulationError::Invalid("lineage bitmap exceeds memory budget".into())
    })?;
    values.resize(words, 0_u64);
    Ok(Bitmap {
        words: values,
        len: size,
    })
}

fn read_mask(path: &Path, context: &ObservationContext) -> Result<Bitmap> {
    let reader = open_table(path)?;
    metadata(reader.schema(), context)?;
    require(
        reader
            .schema()
            .metadata()
            .get("cytiq.mask.schema_version")
            .map(String::as_str)
            == Some("2")
            && reader.metadata().file_metadata().num_rows()
                == i64::try_from(context.raw_events).unwrap_or(-1),
        "QC mask must cover every raw event with schema v2",
    )?;
    let size = usize::try_from(context.raw_events).map_err(|_| {
        crate::PopulationError::Invalid("raw event count overflows platform".into())
    })?;
    let mut seen = booleans(size)?;
    let mut eligible = booleans(size)?;
    for batch in reader.project(&["source_event_id", "keep"])? {
        let batch = batch?;
        let sources = uint(&batch, "source_event_id")?;
        let keep = boolean(&batch, "keep")?;
        for i in 0..batch.num_rows() {
            require(
                !sources.is_null(i) && !keep.is_null(i),
                "null mask lineage or eligibility",
            )?;
            let source = usize::try_from(sources.value(i))
                .map_err(|_| crate::PopulationError::Invalid("invalid event id".into()))?;
            require(
                source < size && !seen.get(source),
                "duplicate or out-of-range raw mask id",
            )?;
            seen.set(source, true);
            eligible.set(source, keep.value(i));
        }
    }
    require((0..size).all(|i| seen.get(i)), "incomplete raw mask")?;
    Ok(eligible)
}

#[derive(Default)]
struct AnnotationCounts {
    counts: BTreeMap<String, u64>,
    assessed: u64,
    unknown: u64,
    abstained: u64,
    ood: u64,
}

impl AnnotationCounts {
    fn new(context: &ObservationContext) -> Self {
        Self {
            counts: context
                .source
                .bindings
                .iter()
                .map(|binding| (binding.label.clone(), 0))
                .collect(),
            ..Self::default()
        }
    }
}

fn count_annotations(
    path: &Path,
    context: &ObservationContext,
    eligible: &Bitmap,
) -> Result<AnnotationCounts> {
    let reader = open_table(path)?;
    metadata(reader.schema(), context)?;
    let classifier = validate_source_metadata(reader.schema(), context)?;
    let policy = context
        .source
        .decision_policy
        .as_ref()
        .map(|reference| crate::decision::DecisionPolicy::load(&reference.path))
        .transpose()?;
    let mut columns = vec!["event_id".into(), "source_event_id".into()];
    columns.push(if classifier { "assessed" } else { "keep" }.into());
    if classifier {
        columns.push("label".into());
        if policy.is_some() {
            columns.extend([
                "decision_status".into(),
                "abstain_reason".into(),
                "is_ood".into(),
            ]);
        }
    } else {
        columns.extend(context.source.bindings.iter().map(|b| b.label.clone()));
    }
    let mut result = AnnotationCounts::new(context);
    let parents = denominator_indices(&context.source.bindings);
    let mut seen = booleans(eligible.len())?;
    let mut rows = 0_u64;
    let names = columns.iter().map(String::as_str).collect::<Vec<_>>();
    for batch in reader.project(&names)? {
        let batch = batch?;
        let ids = uint(&batch, "event_id")?;
        let sources = uint(&batch, "source_event_id")?;
        let keep = boolean(&batch, if classifier { "assessed" } else { "keep" })?;
        let labels = if classifier {
            Some(strings(&batch, "label")?)
        } else {
            None
        };
        let gates = if classifier {
            Vec::new()
        } else {
            context
                .source
                .bindings
                .iter()
                .map(|b| boolean(&batch, &b.label))
                .collect::<Result<Vec<_>>>()?
        };
        for i in 0..batch.num_rows() {
            require(
                !ids.is_null(i) && !sources.is_null(i) && !keep.is_null(i) && ids.value(i) == rows,
                "null or invalid annotation event id",
            )?;
            rows += 1;
            let source = usize::try_from(sources.value(i))
                .map_err(|_| crate::PopulationError::Invalid("invalid source event id".into()))?;
            require(
                source < seen.len() && !seen.get(source),
                "duplicate or out-of-range annotation source_event_id",
            )?;
            seen.set(source, true);
            require(
                keep.value(i) == eligible.get(source),
                "annotation eligibility disagrees with QC mask",
            )?;
            if keep.value(i) {
                result.assessed += 1;
            }
            if let Some(labels) = labels {
                count_classifier_row(
                    context,
                    &batch,
                    labels,
                    policy.as_ref(),
                    i,
                    keep.value(i),
                    &mut result,
                )?;
            } else {
                count_gate_row(
                    context,
                    &gates,
                    &parents,
                    &mut result.counts,
                    i,
                    keep.value(i),
                )?;
            }
        }
    }
    require(
        (0..eligible.len()).all(|i| !eligible.get(i) || seen.get(i)),
        "annotations omit eligible raw events",
    )?;
    Ok(result)
}

fn count_classifier_row(
    context: &ObservationContext,
    batch: &RecordBatch,
    labels: &StringArray,
    policy: Option<&crate::decision::DecisionPolicy>,
    i: usize,
    kept: bool,
    result: &mut AnnotationCounts,
) -> Result<()> {
    if let Some(policy) = policy {
        let statuses = strings(batch, "decision_status")?;
        let reasons = strings(batch, "abstain_reason")?;
        let flags = boolean(batch, "is_ood")?;
        require(!statuses.is_null(i), "missing classifier decision status")?;
        let status = statuses.value(i);
        let is_ood = !flags.is_null(i) && flags.value(i);
        require(
            flags.is_null(i) == (!kept || policy.ood.is_none()),
            "OOD assessment/null mismatch",
        )?;
        if !kept {
            require(
                status == "not_assessed" && labels.is_null(i) && reasons.is_null(i),
                "excluded event has a classification decision",
            )?;
            return Ok(());
        }
        if status == "abstained" {
            require(
                labels.is_null(i) && !reasons.is_null(i),
                "abstention requires a null label and a reason",
            )?;
            let reason = reasons.value(i);
            require(
                ["low_confidence", "ood", "low_confidence_and_ood"].contains(&reason)
                    && is_ood == (reason != "low_confidence"),
                "inconsistent abstention reason/OOD",
            )?;
            result.abstained += 1;
            result.ood += u64::from(is_ood);
            return Ok(());
        }
        require(
            status == "classified" && reasons.is_null(i) && !is_ood,
            "invalid accepted classification",
        )?;
    }
    require(
        labels.is_null(i) != kept,
        "classification assessment/label mismatch",
    )?;
    if kept {
        let label = labels.value(i);
        if label == "unknown" {
            require(
                policy.is_none(),
                "policy abstention must not become an unknown population",
            )?;
            require(
                context
                    .source
                    .bindings
                    .iter()
                    .all(|b| b.target.kind == TargetKind::Phenotype),
                "unknown is unsupported for multiclass predictions",
            )?;
            result.unknown += 1;
        } else {
            *result.counts.get_mut(label).ok_or_else(|| {
                crate::PopulationError::Invalid("unknown classifier label".into())
            })? += 1;
        }
    }
    Ok(())
}

fn uint<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref())
        .ok_or_else(|| crate::PopulationError::Invalid(format!("expected UInt64: {name}")))
}
fn boolean<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a BooleanArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref())
        .ok_or_else(|| crate::PopulationError::Invalid(format!("expected Boolean: {name}")))
}
fn strings<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref())
        .ok_or_else(|| crate::PopulationError::Invalid(format!("expected Utf8: {name}")))
}

fn observations(
    descriptor: &EventAnnotations,
    projection: &KnowledgeArtifact,
    counts: &BTreeMap<String, u64>,
) -> Result<Vec<PopulationObservation>> {
    let context = &descriptor.context;
    let mut result = Vec::new();
    for binding in &context.source.bindings {
        let count = counts[&binding.label];
        let denominator_count = binding
            .denominator
            .as_ref()
            .map_or(descriptor.assessed_events, |label| counts[label]);
        let denominator_target = binding.denominator.as_ref().map(|label| {
            context
                .source
                .bindings
                .iter()
                .find(|b| &b.label == label)
                .expect("resolved denominator")
                .target
                .clone()
        });
        let status = observation_status(descriptor);
        let mut observation = PopulationObservation {
            schema_version: 1,
            observation_id: String::new(),
            specimen_id: context.specimen_id.clone(),
            panel_id: context.panel_id.clone(),
            acquisition_id: Some(context.acquisition_id.clone()),
            source_id: context.source_id.clone(),
            target: binding.target.clone(),
            measure: Measure::ObservedCount,
            unit: "events".into(),
            status,
            value_u64: (status == ObservationStatus::Measured).then_some(count),
            value_f64: None,
            denominator: Some(Denominator {
                target: denominator_target,
                scope: binding.target.scope.clone(),
                observed_count: denominator_count,
            }),
            method: Method {
                name: match context.source.format {
                    AnnotationFormat::GateMembership => "gating",
                    AnnotationFormat::ClassifyPredictions => "classify",
                }
                .into(),
                version: "1".into(),
                definition_sha256: context.source.definition.sha256.clone(),
                experimental: false,
                source_observation_ids: Vec::new(),
            },
            lineage: Lineage {
                acquisition_sha256: Some(context.acquisition_sha256.clone()),
                tree_sha256: projection.value["tree_sha256"]
                    .as_str()
                    .expect("validated hash")
                    .into(),
                panel_profile_sha256: projection.value["panel_sha256"]
                    .as_str()
                    .expect("validated hash")
                    .into(),
                projection_sha256: projection.sha256.clone(),
                annotations_sha256: Some(descriptor.data.sha256.clone()),
                producer_provenance_sha256: Some(descriptor.producer_provenance.sha256.clone()),
                analysis_mask_sha256: Some(descriptor.analysis_mask.sha256.clone()),
            },
            n_events: descriptor.assessed_events,
            supporting_count: (status != ObservationStatus::Measured).then_some(count),
            reason: observation_reason(status, descriptor),
        };
        observation.seal()?;
        result.push(observation.clone());
        observation.measure = Measure::Fraction;
        observation.unit = "fraction".into();
        observation.value_u64 = None;
        if status == ObservationStatus::Measured && denominator_count > 0 {
            #[allow(clippy::cast_precision_loss)]
            {
                observation.value_f64 = Some(count as f64 / denominator_count as f64);
            }
        } else if status == ObservationStatus::Measured {
            observation.status = ObservationStatus::NotApplicable;
            observation.reason = Some("zero_denominator".into());
        }
        observation.seal()?;
        result.push(observation);
    }
    Ok(result)
}

fn observation_status(descriptor: &EventAnnotations) -> ObservationStatus {
    if descriptor.technical_status == "fail" {
        ObservationStatus::QcRejected
    } else if descriptor.assessed_events == 0
        || descriptor.unknown_events > 0
        || descriptor.abstained_events.unwrap_or(0) > 0
    {
        ObservationStatus::NotMeasured
    } else {
        ObservationStatus::Measured
    }
}

fn observation_reason(status: ObservationStatus, descriptor: &EventAnnotations) -> Option<String> {
    match status {
        ObservationStatus::QcRejected => Some("technical_qc_failed".into()),
        ObservationStatus::NotMeasured => Some(
            if descriptor.assessed_events == 0 {
                "no_assessed_events"
            } else {
                "classifier_abstained"
            }
            .into(),
        ),
        _ => None,
    }
}

fn validate_bindings(bindings: &[Binding]) -> Result<()> {
    let mut labels = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for binding in bindings {
        binding.target.validate()?;
        require(
            !binding.label.is_empty()
                && !["event_id", "source_event_id", "keep", "assessed", "unknown"]
                    .contains(&binding.label.as_str())
                && labels.insert(&binding.label)
                && targets.insert(&binding.target),
            "duplicate or reserved annotation binding",
        )?;
        if let Some(parent) = &binding.denominator {
            require(
                parent != &binding.label && bindings.iter().any(|b| &b.label == parent),
                "denominator must name a different bound gate",
            )?;
        }
    }
    Ok(())
}

fn validate_source_metadata(schema: &Schema, context: &ObservationContext) -> Result<bool> {
    let classifier = context.source.format == AnnotationFormat::ClassifyPredictions;
    if classifier {
        let output_version = schema
            .metadata()
            .get("cytiq.classify.schema_version")
            .and_then(|s| s.parse::<u32>().ok());
        let version = if output_version == Some(5) {
            let policy = context.source.decision_policy.as_ref().ok_or_else(|| {
                crate::PopulationError::Invalid("unplanned decision policy output".into())
            })?;
            require(
                schema
                    .metadata()
                    .get("cytiq.classify.decision_policy_sha256")
                    == Some(&policy.sha256),
                "prediction decision policy mismatch",
            )?;
            schema
                .metadata()
                .get("cytiq.classify.bundle_schema_version")
                .and_then(|s| s.parse::<u32>().ok())
        } else {
            require(
                context.source.decision_policy.is_none(),
                "decision policy output lacks schema 5",
            )?;
            output_version
        };
        require(
            matches!(version, Some(2..=4)),
            "specimen classifier import requires bundle v2-v4",
        )?;
        let vocabulary = context
            .source
            .vocabulary
            .as_ref()
            .expect("resolved classifier vocabulary");
        let expected = if version == Some(4) {
            vocabulary.sha256.clone()
        } else {
            hash_file(&vocabulary.path)?.1
        };
        require(
            schema.metadata().get("cytiq.classify.vocabulary_sha256") == Some(&expected),
            "prediction vocabulary mismatch",
        )?;
        let classes = context
            .source
            .bindings
            .iter()
            .map(|b| &b.label)
            .collect::<Vec<_>>();
        require(
            schema.metadata().get("cytiq.classify.classes")
                == Some(&serde_json::to_string(&classes)?),
            "prediction class order mismatch",
        )?;
    } else {
        require(
            schema
                .metadata()
                .get("cytiq.mask.schema_version")
                .map(String::as_str)
                == Some("2"),
            "unsupported membership schema",
        )?;
    }
    Ok(classifier)
}

fn count_gate_row(
    context: &ObservationContext,
    gates: &[&BooleanArray],
    parents: &[Option<usize>],
    counts: &mut BTreeMap<String, u64>,
    i: usize,
    kept: bool,
) -> Result<()> {
    for ((binding, gate), parent) in context.source.bindings.iter().zip(gates).zip(parents) {
        require(
            gate.is_null(i) != kept,
            "membership assessment/keep mismatch",
        )?;
        if kept && gate.value(i) {
            if let Some(parent) = parent {
                require(
                    !gates[*parent].is_null(i) && gates[*parent].value(i),
                    "gate membership exceeds its denominator",
                )?;
            }
            *counts.get_mut(&binding.label).expect("binding counter") += 1;
        }
    }
    Ok(())
}

/// Validate a hash-verified package assessment without reopening event files.
/// # Errors
/// Rejects mismatched evidence or an inconsistent assessment summary.
pub fn validate_release_assessment(
    value: &serde_json::Value,
    descriptor: &EventAnnotations,
) -> Result<cytiq_qc::CheckStatus> {
    require(
        value["schema_version"] == 1
            && value["algorithm"] == "panel-qc-release-v1"
            && value["manifest_sha256"] == descriptor.context.manifest_sha256
            && value["mask_sha256"] == descriptor.analysis_mask.sha256
            && value["acquisition"] == descriptor.context.acquisition_sha256
            && value["sample"] == descriptor.context.acquisition_id
            && value["panel"] == descriptor.context.panel_id,
        "release QC assessment does not belong to this source",
    )?;
    let checks = value["checks"]
        .as_array()
        .ok_or_else(|| crate::PopulationError::Invalid("missing release QC checks".into()))?;
    require(!checks.is_empty(), "empty release QC assessment")?;
    let statuses = checks
        .iter()
        .map(|check| serde_json::from_value::<cytiq_qc::CheckStatus>(check["status"].clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let status = statuses
        .into_iter()
        .max()
        .unwrap_or(cytiq_qc::CheckStatus::Fail);
    require(
        value["status"] == serde_json::to_value(status)?,
        "inconsistent release QC summary",
    )?;
    Ok(status)
}

fn release_checks(
    context: &ObservationContext,
    record: &RunRecord,
    mask_path: &Path,
) -> Result<Vec<cytiq_qc::QcCheck>> {
    let input = record
        .inputs
        .iter()
        .find(|input| {
            input.role.as_deref() == Some("manifest") && input.sha256 == context.manifest_sha256
        })
        .ok_or_else(|| crate::PopulationError::Invalid("missing pinned QC manifest".into()))?;
    require(
        cytiq_provenance::hash_file(Path::new(&input.path))?.1 == context.manifest_sha256,
        "QC manifest changed after assessment",
    )?;
    let invalid = |error: String| crate::PopulationError::Invalid(error);
    let manifest = cytiq_manifest::RunManifest::load(Path::new(&input.path))
        .map_err(|error| invalid(error.to_string()))?;
    let sample = manifest
        .samples
        .iter()
        .find(|sample| sample.id == context.acquisition_id)
        .ok_or_else(|| invalid("QC manifest sample is missing".into()))?;
    let panel = manifest
        .panel_of(sample)
        .ok_or_else(|| invalid("QC manifest panel is missing".into()))?;
    require(
        panel.label() == context.panel_id,
        "QC manifest panel changed",
    )?;
    read_mask(mask_path, context)?;
    let mask = cytiq_mask::EventMask::read_parquet(mask_path)
        .map_err(|error| invalid(error.to_string()))?;
    let metrics = cytiq_qc::QcMetrics::of(&mask, mask.len(), mask.kept());
    Ok(cytiq_qc::evaluate_release(
        &format!("sample {}", sample.id),
        panel.qc.as_ref(),
        &metrics,
    ))
}

fn qc_status<'a>(qc: &'a serde_json::Value, context: &ObservationContext) -> Result<&'a str> {
    require(
        qc["acquisition"] == context.acquisition_sha256
            && qc["sample"] == context.acquisition_id
            && qc["panel"] == context.panel_id
            && qc["metrics"]["events"].as_u64() == Some(context.raw_events),
        "QC acquisition, panel, sample or event-count mismatch",
    )?;
    let status = qc["status"]
        .as_str()
        .ok_or_else(|| crate::PopulationError::Invalid("missing QC status".into()))?;
    let checks = qc["checks"]
        .as_array()
        .ok_or_else(|| crate::PopulationError::Invalid("missing QC checks".into()))?;
    let failures = checks.iter().filter(|c| c["status"] == "fail").count();
    let warnings = checks.iter().filter(|c| c["status"] == "warn").count();
    let expected_status = if failures > 0 {
        "fail"
    } else if warnings > 0 {
        "warn"
    } else {
        "pass"
    };
    require(
        status == expected_status
            && qc["failures"].as_u64() == Some(failures as u64)
            && qc["warnings"].as_u64() == Some(warnings as u64)
            && checks
                .iter()
                .all(|c| ["pass", "warn", "fail"].contains(&c["status"].as_str().unwrap_or(""))),
        "inconsistent QC summary",
    )?;
    Ok(status)
}

fn denominator_indices(bindings: &[Binding]) -> Vec<Option<usize>> {
    let indices: BTreeMap<_, _> = bindings
        .iter()
        .enumerate()
        .map(|(i, b)| (b.label.as_str(), i))
        .collect();
    bindings
        .iter()
        .map(|b| b.denominator.as_ref().map(|label| indices[label.as_str()]))
        .collect()
}
