//! Supervised FCS event classification using a portable model bundle.
mod bundle;
mod decision;
#[cfg(test)]
mod decision_tests;
mod features;
mod serialization;

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use arrow_array::{ArrayRef, BooleanArray, Float32Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use cytiq_fcs::{FcsDataset, FcsReadLimits, FcsReader, FcsStream};
use cytiq_publish::{Existing, Publication};
use ort::{session::Session, value::Tensor};
use parquet::arrow::ArrowWriter;
use serde::Serialize;

pub use bundle::{Bundle, NormalizationContract, input_files};
pub use cytiq_population::decision::DecisionPolicy;
pub use decision::DecisionSummary;
pub use features::{Feature, Profile, feature_columns, feature_columns_with_normalization};

pub const DEFAULT_BATCH_SIZE: usize = 65_536;
pub const UPSTREAM: &[(&str, &str)] = &[("ONNX Runtime", "1.31.0")];

#[derive(Debug, thiserror::Error)]
pub enum ClassifyError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Fcs(#[from] cytiq_fcs::Error),
    #[error(transparent)]
    Mask(#[from] cytiq_mask::MaskError),
    #[error(transparent)]
    Arrow(#[from] arrow_schema::ArrowError),
    #[error(transparent)]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error(transparent)]
    Ort(#[from] ort::Error),
    #[error(transparent)]
    Publish(#[from] cytiq_publish::PublishError),
}

#[derive(Clone, Debug, Serialize)]
pub struct ClassifyConfig {
    pub input: PathBuf,
    pub model: PathBuf,
    pub output: PathBuf,
    pub masks: Vec<PathBuf>,
    pub threads: Option<usize>,
    pub batch_size: usize,
    pub abstain_margin: f32,
    pub overwrite: bool,
}

#[derive(Debug, Serialize)]
pub struct ClassifyRunResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<DecisionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalization: Option<NormalizationContract>,
    pub schema_version: u32,
    pub acquisition_sha256: String,
    pub model_sha256: String,
    pub run_id: String,
    pub events: usize,
    pub assessed: usize,
    pub blast: usize,
    pub non_blast: usize,
    pub unknown: usize,
    pub blast_fraction: Option<f64>,
    pub non_blast_fraction: Option<f64>,
    pub unknown_fraction: Option<f64>,
    pub threshold: f32,
    pub abstain_margin: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_counts: Option<BTreeMap<String, usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_fractions: Option<BTreeMap<String, f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vocabulary_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

pub struct ClassifyRunner;

impl ClassifyRunner {
    /// Classify one FCS, preserving input event order and lineage.
    ///
    /// The input is read `batch_size` events at a time: the panel, signal
    /// state and normalization are checked from TEXT before the model loads,
    /// and each batch's lineage, mask, features and predictions are computed
    /// on that batch alone, so memory holds one batch rather than the file.
    ///
    /// # Errors
    /// Returns a typed error for incompatible inputs, invalid bundles or output failures.
    #[allow(
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation
    )]
    pub fn run(config: &ClassifyConfig) -> Result<ClassifyRunResult, ClassifyError> {
        Self::run_with_policy(config, None)
    }

    /// Apply an independently validated selective-classification policy.
    /// Existing bundle v1-v4 execution is unchanged when no policy is supplied.
    /// # Errors
    /// Rejects missing evidence, mismatched policy, invalid scorer output and ordinary classification errors.
    #[allow(
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation
    )]
    pub fn run_with_policy(
        config: &ClassifyConfig,
        policy_path: Option<&Path>,
    ) -> Result<ClassifyRunResult, ClassifyError> {
        let invalid = |s: &str| ClassifyError::Invalid(s.to_owned());
        let batch_size = validate_run_config(config)?;
        let bundle = Bundle::load(&config.model, config.abstain_margin)?;
        let policy = policy_path
            .map(DecisionPolicy::load)
            .transpose()
            .map_err(|e| invalid(&e.to_string()))?;
        let policy_hash = policy_path
            .map(cytiq_provenance::hash_file)
            .transpose()
            .map_err(|e| invalid(&e.to_string()))?
            .map(|(_, hash)| hash);
        if let Some(policy) = &policy {
            if config.abstain_margin != 0.0 {
                return Err(invalid(
                    "decision policy conflicts with legacy abstain margin",
                ));
            }
            policy
                .check_bundle(&bundle::manifest(&config.model)?.0)
                .map_err(|e| invalid(&e.to_string()))?;
        }
        let model = config.model.join("model.onnx");
        if cytiq_provenance::hash_file(&model)?.1 != bundle.model_sha256 {
            return Err(invalid("model checksum mismatch"));
        }
        let refuse =
            cytiq_publish::Publication::new(cytiq_publish::Existing::overwrite(config.overwrite));
        for name in ["predictions.parquet", "summary.json", "provenance.json"] {
            refuse.check(config.output.join(name))?;
        }
        let FcsStream { metadata, batches } =
            FcsReader::open(&config.input)?.stream(batch_size, FcsReadLimits::default())?;
        let acquisition = cytiq_fcs::acquisition_id(&config.input, &metadata.text)?;
        let mask = cytiq_mask::read_combined(&config.masks)?;
        let mask_keep = mask
            .as_ref()
            .map(|mask| mask.keep_by_source(&acquisition))
            .transpose()?;
        let normalization = bundle
            .normalization
            .as_ref()
            .unwrap_or(&NormalizationContract::None);
        let events = metadata.event_count;
        // One dataset holds the file's metadata and, in turn, each batch of its events.
        let mut window = FcsDataset {
            version: metadata.version,
            header: metadata.header,
            text: metadata.text,
            parameters: metadata.parameters,
            data: batches.empty_batch(),
            analysis: None,
        };
        // An empty batch runs every TEXT check of the recipe before the model loads.
        feature_columns_with_normalization(&window, &bundle.profile, normalization)?;
        let environment = runtime_environment()?;
        let mut builder = Session::builder(environment)?;
        if let Some(threads) = config.threads {
            builder = builder
                .with_intra_threads(threads)
                .map_err(|e| invalid(&e.to_string()))?;
        }
        let mut session = builder.commit_from_file(model)?;
        let mut ood_session = if let Some(ood) = policy.as_ref().and_then(|p| p.ood.as_ref()) {
            // Keep the scalar safety decision on the portable CPU provider.
            // Accelerator compilation/caching must not change its execution contract.
            let mut builder = Session::builder(environment)?
                .with_auto_device(ort::session::builder::AutoDevicePolicy::PreferCPU)
                .map_err(|e| invalid(&e.to_string()))?;
            if let Some(threads) = config.threads {
                builder = builder
                    .with_intra_threads(threads)
                    .map_err(|e| invalid(&e.to_string()))?;
            }
            let session = builder.commit_from_file(&ood.model.path)?;
            validate_model_input(
                &session
                    .inputs()
                    .iter()
                    .map(ort::value::Outlet::name)
                    .collect::<Vec<_>>(),
                "features",
            )?;
            if session.outputs().len() != 1 || session.outputs()[0].name() != "ood_score" {
                return Err(invalid("invalid OOD ONNX output interface"));
            }
            Some(session)
        } else {
            None
        };
        let input_names = session
            .inputs()
            .iter()
            .map(ort::value::Outlet::name)
            .collect::<Vec<_>>();
        validate_model_input(&input_names, &bundle.input)?;
        let schema = Arc::new(prediction_schema(
            &bundle,
            &acquisition,
            policy_hash.as_deref(),
        )?);
        let mut result = ClassifyRunResult {
            decision: policy_hash.map(|hash| DecisionSummary {
                policy_sha256: hash,
                bundle_schema_version: bundle.schema_version,
                abstained: 0,
                ood_events: 0,
                ood_assessed: ood_session.is_some(),
            }),
            normalization: bundle.normalization.clone(),
            schema_version: if policy.is_some() {
                5
            } else {
                bundle.schema_version
            },
            acquisition_sha256: acquisition,
            model_sha256: bundle.model_sha256.clone(),
            run_id: bundle.run_id.clone(),
            events,
            assessed: 0,
            blast: 0,
            non_blast: 0,
            unknown: 0,
            blast_fraction: None,
            non_blast_fraction: None,
            unknown_fraction: None,
            threshold: bundle.threshold,
            abstain_margin: config.abstain_margin,
            class_counts: (bundle.schema_version >= 2)
                .then(|| bundle.classes.iter().map(|c| (c.clone(), 0)).collect()),
            class_fractions: None,
            vocabulary_sha256: bundle.vocabulary_sha256.clone(),
            task: bundle.task.clone(),
        };
        let mut publication = Publication::new(Existing::overwrite(config.overwrite));
        publication.stage_path(config.output.join("predictions.parquet"), |path| {
            let mut writer = ArrowWriter::try_new(File::create(path)?, schema.clone(), None)?;
            let mut start = 0_usize;
            for batch in batches {
                window.data = batch?;
                let end = start + window.event_count();
                let source = batch_source_ids(&window, start..end, mask_keep.is_some())?;
                let keep = match &mask_keep {
                    Some(mask) => mask.keep(&source)?,
                    None => vec![true; end - start],
                };
                // Masked events are featurized too: a nonfinite feature fails the run either way.
                let columns =
                    feature_columns_with_normalization(&window, &bundle.profile, normalization)?;
                let selected = (0..end - start).filter(|i| keep[*i]).collect::<Vec<_>>();
                let mut probabilities = infer_probabilities(
                    &mut session,
                    &columns,
                    &selected,
                    bundle.classes.len(),
                    end - start,
                )?;
                if policy.is_some() && bundle.schema_version == 1 {
                    // V1 publishes p_negative = 1 - p_positive. Evaluate the
                    // policy on that same public probability vector.
                    for row in probabilities.iter_mut().flatten() {
                        row[0] = 1.0 - row[1];
                    }
                }
                let ood_scores = if policy.is_some() {
                    decision::infer_ood(ood_session.as_mut(), &columns, &selected, end - start)?
                } else {
                    Vec::new()
                };
                let decisions = policy.as_ref().map(|policy| {
                    probabilities
                        .iter()
                        .zip(&ood_scores)
                        .map(|(row, score)| decision::assess(policy, row.as_deref(), *score))
                        .collect::<Vec<_>>()
                });
                let mut labels = Vec::with_capacity(probabilities.len());
                for (index, row) in probabilities.iter().enumerate() {
                    let abstained = decisions
                        .as_ref()
                        .is_some_and(|rows| rows[index].status == "abstained");
                    if abstained {
                        result.assessed += 1;
                        let summary = result
                            .decision
                            .as_mut()
                            .ok_or_else(|| invalid("missing policy decision summary"))?;
                        summary.abstained += 1;
                        summary.ood_events += usize::from(
                            ood_scores[index]
                                .zip(policy.as_ref().and_then(|p| p.ood.as_ref()))
                                .is_some_and(|(score, ood)| score >= ood.threshold),
                        );
                        labels.push(None);
                    } else {
                        labels.push(
                            row.as_ref().map(|row| {
                                decide(&bundle, row, config.abstain_margin, &mut result)
                            }),
                        );
                    }
                }
                let mut arrays: Vec<ArrayRef> = vec![
                    Arc::new(UInt64Array::from_iter_values(start as u64..end as u64)),
                    Arc::new(UInt64Array::from(source)),
                    Arc::new(BooleanArray::from(keep)),
                ];
                for column in 0..bundle.classes.len() {
                    arrays.push(Arc::new(Float32Array::from(
                        probabilities
                            .iter()
                            .map(|row| {
                                row.as_ref().map(|values| {
                                    if bundle.schema_version == 1 && column == 0 {
                                        1.0 - values[1]
                                    } else {
                                        values[column]
                                    }
                                })
                            })
                            .collect::<Vec<_>>(),
                    )));
                }
                arrays.push(Arc::new(StringArray::from(labels)));
                if let Some(decisions) = &decisions {
                    arrays.extend(decision::columns(decisions));
                }
                writer.write(&RecordBatch::try_new(schema.clone(), arrays)?)?;
                start = end;
            }
            writer.close()?;
            Ok::<(), ClassifyError>(())
        })?;
        if let Some(decision) = &mut result.decision {
            decision.ood_assessed = ood_session.is_some() && result.assessed > 0;
        }
        let summary = classification_summary(&bundle, &mut result)?;
        publication.stage_file(config.output.join("summary.json"), |writer| {
            serde_json::to_writer_pretty(writer, &summary).map_err(ClassifyError::from)
        })?;
        publication.commit()?;
        Ok(result)
    }
}

#[allow(clippy::cast_precision_loss)]
fn classification_summary(
    bundle: &Bundle,
    result: &mut ClassifyRunResult,
) -> Result<serde_json::Value, ClassifyError> {
    if result.assessed > 0 {
        result.blast_fraction = Some(result.blast as f64 / result.assessed as f64);
        result.non_blast_fraction = Some(result.non_blast as f64 / result.assessed as f64);
        result.unknown_fraction = Some(result.unknown as f64 / result.assessed as f64);
    }
    if let Some(counts) = &result.class_counts
        && result.assessed > 0
    {
        result.class_fractions = Some(
            counts
                .iter()
                .map(|(id, count)| (id.clone(), *count as f64 / result.assessed as f64))
                .collect(),
        );
    }
    let mut summary = serde_json::to_value(&*result)?;
    if bundle.multiclass()
        && let Some(object) = summary.as_object_mut()
    {
        for key in [
            "blast",
            "non_blast",
            "blast_fraction",
            "non_blast_fraction",
            "threshold",
            "abstain_margin",
        ] {
            object.remove(key);
        }
    }
    Ok(summary)
}

fn validate_run_config(config: &ClassifyConfig) -> Result<NonZeroUsize, ClassifyError> {
    let invalid =
        || ClassifyError::Invalid("invalid batch size, threads or abstain margin".to_owned());
    if config.threads == Some(0)
        || !config.abstain_margin.is_finite()
        || config.abstain_margin < 0.0
    {
        return Err(invalid());
    }
    NonZeroUsize::new(config.batch_size).ok_or_else(invalid)
}

fn validate_model_input(names: &[&str], expected: &str) -> Result<(), ClassifyError> {
    if names.len() != 1 || names[0] != expected {
        return Err(ClassifyError::Invalid(
            "invalid ONNX feature input".to_owned(),
        ));
    }
    Ok(())
}

fn prediction_schema(
    bundle: &Bundle,
    acquisition: &str,
    policy_hash: Option<&str>,
) -> Result<Schema, ClassifyError> {
    let mut fields = vec![
        Field::new("event_id", DataType::UInt64, false),
        Field::new("source_event_id", DataType::UInt64, false),
        Field::new("assessed", DataType::Boolean, false),
    ];
    fields.extend(
        bundle
            .classes
            .iter()
            .map(|id| Field::new(format!("p_{id}"), DataType::Float32, true)),
    );
    fields.push(Field::new("label", DataType::Utf8, true));
    if policy_hash.is_some() {
        fields.extend([
            Field::new("confidence", DataType::Float32, true),
            Field::new("uncertainty", DataType::Float32, true),
            Field::new("ood_score", DataType::Float32, true),
            Field::new("is_ood", DataType::Boolean, true),
            Field::new("decision_status", DataType::Utf8, false),
            Field::new("abstain_reason", DataType::Utf8, true),
        ]);
    }
    let mut metadata = HashMap::from([
        (
            "cytiq.classify.schema_version".into(),
            if policy_hash.is_some() {
                "5".to_owned()
            } else {
                bundle.schema_version.to_string()
            },
        ),
        ("cytiq.acquisition_sha256".into(), acquisition.to_owned()),
        ("cytiq.event_id_base".into(), "0".into()),
    ]);
    if let Some(hash) = policy_hash {
        metadata.insert("cytiq.classify.decision_policy_sha256".into(), hash.into());
        metadata.insert(
            "cytiq.classify.bundle_schema_version".into(),
            bundle.schema_version.to_string(),
        );
    }
    if let Some(hash) = &bundle.vocabulary_sha256 {
        metadata.insert("cytiq.classify.vocabulary_sha256".into(), hash.clone());
        metadata.insert(
            "cytiq.classify.classes".into(),
            serde_json::to_string(&bundle.classes)?,
        );
    }
    if let Some(contract) = &bundle.normalization {
        metadata.insert(
            "cytiq.classify.normalization".into(),
            serde_json::to_string(contract)?,
        );
    }
    Ok(Schema::new_with_metadata(fields, metadata))
}

fn infer_probabilities(
    session: &mut Session,
    columns: &[Vec<f32>],
    selected: &[usize],
    classes: usize,
    batch_len: usize,
) -> Result<Vec<Option<Vec<f32>>>, ClassifyError> {
    let invalid = |s: &str| ClassifyError::Invalid(s.to_owned());
    let mut probabilities = vec![None; batch_len];
    if !selected.is_empty() {
        let values = selected
            .iter()
            .flat_map(|i| columns.iter().map(move |c| c[*i]))
            .collect::<Vec<_>>();
        let tensor = Tensor::from_array(([selected.len(), columns.len()], values))?;
        let outputs = session.run(ort::inputs!["features" => tensor])?;
        let probabilities_output = outputs
            .get("probabilities")
            .ok_or_else(|| invalid("ONNX probability output missing"))?;
        let (shape, data) = probabilities_output.try_extract_tensor::<f32>()?;
        validate_probabilities(shape.as_ref(), data, selected.len(), classes)?;
        for (i, row) in selected.iter().zip(data.chunks_exact(classes)) {
            probabilities[*i] = Some(row.to_vec());
        }
    }
    Ok(probabilities)
}

fn validate_probabilities(
    shape: &[i64],
    data: &[f32],
    rows: usize,
    k: usize,
) -> Result<(), ClassifyError> {
    let invalid = |s: &str| ClassifyError::Invalid(s.to_owned());
    let rows = i64::try_from(rows).map_err(|_| invalid("batch too large"))?;
    let columns = i64::try_from(k).map_err(|_| invalid("too many classes"))?;
    if shape != [rows, columns]
        || data
            .iter()
            .any(|p| !p.is_finite() || !(0.0..=1.0).contains(p))
    {
        return Err(invalid("invalid ONNX probabilities"));
    }
    for row in data.chunks_exact(k) {
        if (row.iter().sum::<f32>() - 1.0).abs() > 1e-5 {
            return Err(invalid("ONNX probabilities do not sum to one"));
        }
    }
    Ok(())
}

/// Raw-file rows of a batch holding rows `rows` of the input: `Original_ID`
/// when the input has it, the rows themselves otherwise. With a mask, a bad
/// `Original_ID` is reported as the mask's lineage error, as for a whole file.
fn batch_source_ids(
    window: &FcsDataset,
    rows: std::ops::Range<usize>,
    masked: bool,
) -> Result<Vec<u64>, ClassifyError> {
    match cytiq_fcs::source_event_ids(window) {
        Ok(ids) => Ok(ids.unwrap_or_else(|| (rows.start as u64..rows.end as u64).collect())),
        Err(error) if masked => Err(cytiq_mask::MaskError::Lineage(error).into()),
        Err(error) => Err(error.into()),
    }
}

fn decide<'a>(
    bundle: &'a Bundle,
    row: &[f32],
    margin: f32,
    result: &mut ClassifyRunResult,
) -> &'a str {
    result.assessed += 1;
    let index = if bundle.multiclass() {
        // Strict comparison deliberately retains the first vocabulary index on ties.
        row.iter().enumerate().fold(
            0,
            |best, (i, value)| if *value > row[best] { i } else { best },
        )
    } else {
        if margin > 0.0 && (row[1] - bundle.threshold).abs() < margin {
            result.unknown += 1;
            return "unknown";
        }
        let positive = row[1] >= bundle.threshold;
        if positive {
            result.blast += 1;
        } else {
            result.non_blast += 1;
        }
        usize::from(positive)
    };
    let id = &bundle.classes[index];
    if let Some(counts) = &mut result.class_counts {
        *counts.entry(id.clone()).or_default() += 1;
    }
    id
}

fn runtime_environment() -> Result<&'static ort::environment::Environment, ClassifyError> {
    static ENVIRONMENT: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| ort::init().build().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| ClassifyError::Invalid(format!("ONNX initialization: {message}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_result() -> ClassifyRunResult {
        ClassifyRunResult {
            decision: None,
            normalization: None,
            schema_version: 1,
            acquisition_sha256: "acquisition".into(),
            model_sha256: "model".into(),
            run_id: "run".into(),
            events: 8,
            assessed: 4,
            blast: 1,
            non_blast: 2,
            unknown: 1,
            blast_fraction: None,
            non_blast_fraction: None,
            unknown_fraction: None,
            threshold: 0.5,
            abstain_margin: 0.0,
            class_counts: None,
            class_fractions: None,
            vocabulary_sha256: None,
            task: None,
        }
    }

    #[test]
    fn summary_uses_assessed_events_as_the_fraction_denominator() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/classify");
        let bundle = Bundle::load(&root.join("catboost"), 0.0).unwrap();
        let mut result = summary_result();
        let summary = classification_summary(&bundle, &mut result).unwrap();
        assert_eq!(result.blast_fraction, Some(0.25));
        assert_eq!(result.non_blast_fraction, Some(0.5));
        assert_eq!(result.unknown_fraction, Some(0.25));
        assert_eq!(summary["assessed"], 4);
        assert_eq!(summary["events"], 8);
        assert_eq!(summary["blast_fraction"], 0.25);
        let bundle = Bundle::load(&root.join("v2/catboost"), 0.0).unwrap();
        assert!(bundle.multiclass());
        let mut result = summary_result();
        result.class_counts = Some(BTreeMap::from([
            ("a".into(), 1),
            ("b".into(), 2),
            ("c".into(), 1),
        ]));
        let summary = classification_summary(&bundle, &mut result).unwrap();
        assert_eq!(
            result.class_fractions,
            Some(BTreeMap::from([
                ("a".into(), 0.25),
                ("b".into(), 0.5),
                ("c".into(), 0.25)
            ]))
        );
        for key in [
            "blast",
            "non_blast",
            "blast_fraction",
            "non_blast_fraction",
            "threshold",
            "abstain_margin",
        ] {
            assert!(summary.get(key).is_none(), "{key}");
        }
    }

    #[test]
    fn summary_keeps_fractions_absent_when_no_events_were_assessed() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/classify");
        let bundle = Bundle::load(&root.join("catboost"), 0.0).unwrap();
        let mut result = summary_result();
        result.assessed = 0;
        result.blast = 0;
        result.non_blast = 0;
        result.unknown = 0;
        result.class_counts = Some(BTreeMap::from([("a".into(), 0)]));
        classification_summary(&bundle, &mut result).unwrap();
        assert_eq!(result.blast_fraction, None);
        assert_eq!(result.non_blast_fraction, None);
        assert_eq!(result.unknown_fraction, None);
        assert_eq!(result.class_fractions, None);
    }

    #[test]
    fn model_input_requires_one_input_with_the_declared_name() {
        assert!(validate_model_input(&["features"], "features").is_ok());
        for names in [vec![], vec!["other"], vec!["features", "other"]] {
            assert!(
                matches!(validate_model_input(&names, "features"), Err(ClassifyError::Invalid(reason)) if reason == "invalid ONNX feature input")
            );
        }
    }

    #[test]
    fn probability_validation_checks_shape_range_finiteness_and_each_row() {
        assert!(validate_probabilities(&[2, 2], &[0.0, 1.0, 0.25, 0.75], 2, 2).is_ok());
        for (shape, data) in [
            (vec![1, 2], vec![0.0, 1.0]),
            (vec![2, 1], vec![0.0, 1.0]),
            (vec![2, 2], vec![f32::NAN, 1.0, 0.5, 0.5]),
            (vec![2, 2], vec![f32::INFINITY, 1.0, 0.5, 0.5]),
            (vec![2, 2], vec![-0.1, 1.1, 0.5, 0.5]),
            (vec![2, 2], vec![0.0, 1.1, 0.5, 0.5]),
            (vec![2, 2], vec![0.0, 1.0, 0.2, 0.3]),
            (vec![2, 2], vec![0.0, 1.0, 0.8, 0.9]),
        ] {
            assert!(
                matches!(
                    validate_probabilities(&shape, &data, 2, 2),
                    Err(ClassifyError::Invalid(_))
                ),
                "{shape:?} {data:?}"
            );
        }
        assert!(validate_probabilities(&[1, 2], &[0.5, 0.500_005], 1, 2).is_ok());
        assert!(validate_probabilities(&[1, 2], &[0.5, 0.500_02], 1, 2).is_err());
        assert!(
            validate_probabilities(&[], &[], usize::MAX, 2)
                .unwrap_err()
                .to_string()
                .contains("batch too large")
        );
        assert!(
            validate_probabilities(&[], &[], 1, usize::MAX)
                .unwrap_err()
                .to_string()
                .contains("too many classes")
        );
    }

    #[test]
    fn probability_sum_checks_the_closest_representable_tolerance_boundaries() {
        let above = 1.0_f32 + 1e-5;
        let below = 1.0_f32 - 1e-5;
        for sum in [above.next_down(), below.next_up()] {
            assert!(validate_probabilities(&[1, 2], &[0.5, sum - 0.5], 1, 2).is_ok());
        }
        for sum in [above, below] {
            assert!(validate_probabilities(&[1, 2], &[0.5, sum - 0.5], 1, 2).is_err());
        }
    }

    #[test]
    fn run_config_accepts_zero_margin_and_rejects_each_invalid_setting() {
        let valid = ClassifyConfig {
            input: "input.fcs".into(),
            model: "model".into(),
            output: "out".into(),
            masks: vec![],
            threads: None,
            batch_size: 7,
            abstain_margin: 0.0,
            overwrite: false,
        };
        assert_eq!(validate_run_config(&valid).unwrap().get(), 7);
        let mut changed = valid.clone();
        changed.batch_size = 0;
        assert!(validate_run_config(&changed).is_err());
        changed = valid.clone();
        changed.threads = Some(0);
        assert!(validate_run_config(&changed).is_err());
        changed.threads = Some(1);
        assert_eq!(validate_run_config(&changed).unwrap().get(), 7);
        for margin in [-0.1, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            changed = valid.clone();
            changed.abstain_margin = margin;
            assert!(validate_run_config(&changed).is_err());
        }
        changed.abstain_margin = 0.1;
        assert!(validate_run_config(&changed).is_ok());
    }
}
