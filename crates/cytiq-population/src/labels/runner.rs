//! `cytiq export labels`: one native per-event output to one label table,
//! published atomically.
//!
//! The native file is bound to its acquisition and to the run that wrote
//! it before any row is read: the producer's provenance record must list it
//! as an output, its acquisition must be the events table's, and the
//! definitions and QC masks given must be the very files the producer read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cytiq_mask::parquet::ParquetReadLimits;
use cytiq_provenance::{RunRecord, hash_file, record_path_for_file};
use cytiq_publish::{Existing, Publication, PublishError};

use super::sources::{Rows, classify, flowsom, mask};
use super::{
    DESCRIPTOR_CONTRACT, Definition, Eligibility, LabelColumn, LabelDescriptor, LabelKind,
    LabelSettings, LabelWriter, Producer, WrittenLabels,
};
use crate::bitmap::Bitmap;
use crate::events::{self, EventsSummary, PARTS_FILE};
use crate::issue::Issues;
use crate::native::{producer_record, read_raw_mask};
use crate::{PopulationError, Result, require};

/// A Gating-ML strategy the caller parsed: its gates in evaluation order,
/// each with its parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatingDefinition {
    pub path: PathBuf,
    pub gates: Vec<(String, Option<String>)>,
}

/// A `FlowSOM` model the caller loaded: its node and metacluster counts and
/// the metacluster of each node (1-based).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterModel {
    pub path: PathBuf,
    pub nodes: u64,
    pub metaclusters: u64,
    pub node_metaclusters: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportLabelsConfig {
    pub kind: LabelKind,
    /// The native file: a mask, a membership, predictions or assignments.
    pub input: PathBuf,
    /// The provenance record of the run that wrote `input`.
    pub producer_provenance: PathBuf,
    /// An events table, or a parts manifest, holding every raw event of the
    /// acquisition: the event identity the labels bind to.
    pub events: PathBuf,
    /// The QC masks the producer read, to tell QC-rejected events apart.
    pub qc_masks: Vec<PathBuf>,
    /// Required for `gate`.
    pub gating: Option<GatingDefinition>,
    /// Required for `cluster` when the producer applied a saved model.
    pub cluster_model: Option<ClusterModel>,
    /// Keep the classifier's per-class probabilities.
    pub probabilities: bool,
    pub output: PathBuf,
    pub overwrite: bool,
    pub settings: LabelSettings,
}

impl ExportLabelsConfig {
    /// Where the provenance record of the export goes.
    #[must_use]
    pub fn record_path(&self) -> PathBuf {
        record_path_for_file(&self.output)
    }
}

/// The tools whose output each kind converts.
fn producers(kind: LabelKind) -> &'static [&'static str] {
    match kind {
        LabelKind::Qc => &[
            "cytiq clean",
            "cytiq peacoqc",
            "cytiq compensate",
            "cytiq autospectral",
            "cytiq mask combine",
        ],
        LabelKind::Gate => &["cytiq gate"],
        LabelKind::Cluster => &["cytiq flowsom", "cytiq flowsom apply"],
        LabelKind::Classifier => &["cytiq classify"],
    }
}

pub struct LabelsRunner;

impl LabelsRunner {
    /// Files the export reads, by provenance role.
    #[must_use]
    pub fn input_files(config: &ExportLabelsConfig) -> Vec<(&'static str, PathBuf)> {
        let mut files = vec![
            ("input", config.input.clone()),
            ("producer_provenance", config.producer_provenance.clone()),
            ("events", anchor_file(&config.events)),
        ];
        files.extend(config.qc_masks.iter().map(|mask| ("qc_mask", mask.clone())));
        if let Some(gating) = &config.gating {
            files.push(("gating_ml", gating.path.clone()));
        }
        if let Some(model) = &config.cluster_model {
            files.push(("model", model.path.clone()));
        }
        files
    }

    /// Convert the native file, refusing an existing output without
    /// `overwrite` before any work, and publish the label table atomically
    /// once it verifies.
    ///
    /// # Errors
    /// Returns [`PublishError::Exists`] for an existing output,
    /// [`PopulationError::Missing`] for an absent input, and refuses a native
    /// file its producer record does not list, of another acquisition, out
    /// of raw order or contradicting the QC masks, definitions other than
    /// the producer's, and a native file that changed during the export.
    pub fn run(config: &ExportLabelsConfig) -> Result<WrittenLabels> {
        if config.output.exists() && !config.overwrite {
            return Err(PublishError::Exists {
                path: config.output.clone(),
            }
            .into());
        }
        for (_, path) in Self::input_files(config) {
            if !path.is_file() {
                return Err(PopulationError::Missing(path));
            }
        }
        let native_sha256 = hash_file(&config.input)?.1;
        let record = producer(config, &native_sha256)?;
        let anchor = anchor(&config.events)?;
        let raw_events = anchor.raw_events.ok_or_else(|| {
            PopulationError::Invalid(
                "the events table must hold every raw event of the acquisition (row_set=all)"
                    .into(),
            )
        })?;
        let acquisition = anchor.descriptor.acquisition_id.clone();
        // `cytiq mask combine` reads masks only; every other producer reads
        // one FCS file.
        let input_sha256 = match (config.kind, digests(&record, "input").as_slice()) {
            (_, [digest]) => Some(digest.clone()),
            (LabelKind::Qc, []) => None,
            _ => {
                return Err(PopulationError::Invalid(
                    "the producer record must name exactly one `input` input".into(),
                ));
            }
        };
        let eligibility = Eligibility {
            masks: digests(&record, "mask"),
            verified: !config.qc_masks.is_empty(),
        };
        require(
            config.kind != LabelKind::Qc || config.qc_masks.is_empty(),
            "a QC mask's own `keep` decides QC labels; give no `--qc-mask`",
        )?;
        let eligible = eligible(config, &eligibility, &acquisition, raw_events)?;
        if config.kind == LabelKind::Cluster {
            // The assignments name no acquisition: the producer's input must
            // be the events table's file, the raw file itself, or read with
            // QC masks of this acquisition.
            require(
                input_sha256
                    .as_ref()
                    .is_some_and(|input| *input == anchor.source_sha256 || *input == acquisition)
                    || eligible.is_some(),
                "FlowSOM assignments name no acquisition: give the events table exported from \
                 the FCS file FlowSOM read, or the QC masks it read",
            )?;
        }
        let source = Source::read(config, &record, &acquisition)?;
        // The input's layer, path-free, when the events table is its export.
        let input_layer =
            (input_sha256.as_ref() == Some(&anchor.source_sha256)).then(|| anchor.layer_id.clone());
        let descriptor = LabelDescriptor {
            contract: DESCRIPTOR_CONTRACT.into(),
            kind: config.kind,
            acquisition_id: acquisition,
            raw_events,
            producer: Producer {
                tool: record.tool.clone(),
                native_contract: source.contract.clone(),
                native_sha256: native_sha256.clone(),
                input_layer_id: input_layer.clone(),
                input_sha256: if input_layer.is_some() {
                    None
                } else {
                    input_sha256
                },
            },
            definition: source.definition.clone(),
            eligibility,
            labels: source.labels.clone(),
        };
        let mut publication = Publication::new(Existing::overwrite(config.overwrite));
        let mut written = None;
        publication.stage_path(&config.output, |staged| {
            written = Some(write(
                staged,
                descriptor,
                config,
                &source,
                eligible.as_ref(),
            )?);
            Ok::<(), PopulationError>(())
        })?;
        // A modification time can stay the same through a rewrite, so the
        // bytes themselves are compared.
        require(
            hash_file(&config.input)?.1 == native_sha256,
            "the native file changed while it was exported",
        )?;
        publication.commit()?;
        written.ok_or_else(|| PopulationError::Invalid("the export wrote nothing".into()))
    }
}

/// The record of the run that wrote the native file: its tool writes this
/// kind, and it lists the file among its outputs.
fn producer(config: &ExportLabelsConfig, native_sha256: &str) -> Result<RunRecord> {
    let record = producer_record(&config.producer_provenance)?;
    require(
        producers(config.kind).contains(&record.tool.as_str()),
        format!(
            "`{}` does not write {} labels",
            record.tool,
            config.kind.as_str()
        ),
    )?;
    require(
        record.outputs.iter().any(|f| f.sha256 == native_sha256),
        format!(
            "the producer record does not list `{}` among the outputs of `{}`",
            config.input.display(),
            record.tool
        ),
    )?;
    Ok(record)
}

/// The events table or parts manifest `path` names.
fn anchor_file(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join(PARTS_FILE)
    } else {
        path.to_path_buf()
    }
}

/// Verify the events table or parts manifest every row binds to.
fn anchor(path: &Path) -> Result<EventsSummary> {
    use std::io::Read;
    let file = anchor_file(path);
    let mut head = Vec::new();
    std::fs::File::open(&file)?.take(4).read_to_end(&mut head)?;
    let mut issues = Issues::new();
    // A table starts with `PAR1`; anything else must be a parts manifest.
    let summary = if head.starts_with(b"PAR1") {
        events::verify_file(&file, "", &mut issues)
    } else {
        events::verify_parts(&file, "", &mut issues)
    };
    summary.ok_or_else(|| {
        let reason = issues
            .items()
            .first()
            .map_or("no reason given", |issue| issue.message.as_str());
        PopulationError::Invalid(format!("the events table does not verify: {reason}"))
    })
}

/// The sorted, distinct SHA-256 of every file the producer read as `role`.
fn digests(record: &RunRecord, role: &str) -> Vec<String> {
    let mut digests: Vec<String> = record
        .inputs
        .iter()
        .filter(|f| f.role.as_deref() == Some(role))
        .map(|f| f.sha256.clone())
        .collect();
    digests.sort();
    digests.dedup();
    digests
}

/// The SHA-256 of the one file the producer read as `role`.
fn single(record: &RunRecord, role: &str) -> Result<String> {
    match digests(record, role).as_slice() {
        [digest] => Ok(digest.clone()),
        _ => Err(PopulationError::Invalid(format!(
            "the producer record must name exactly one `{role}` input"
        ))),
    }
}

/// Combined `keep` of the QC masks, which must be exactly the producer's.
fn eligible(
    config: &ExportLabelsConfig,
    eligibility: &Eligibility,
    acquisition: &str,
    raw_events: u64,
) -> Result<Option<Bitmap>> {
    if config.qc_masks.is_empty() {
        return Ok(None);
    }
    let mut given = config
        .qc_masks
        .iter()
        .map(|path| Ok(hash_file(path)?.1))
        .collect::<Result<Vec<_>>>()?;
    given.sort();
    given.dedup();
    require(
        given == eligibility.masks,
        "the QC masks must be exactly the masks the producer read",
    )?;
    let mut combined: Option<Bitmap> = None;
    for path in &config.qc_masks {
        let keep = read_raw_mask(path, acquisition, raw_events)?;
        combined = Some(match combined {
            None => keep,
            Some(mut all) => {
                for index in 0..all.len() {
                    if all.get(index) && !keep.get(index) {
                        all.set(index, false);
                    }
                }
                all
            }
        });
    }
    Ok(combined)
}

/// What the native file is and how its labels are named, read from its
/// footer and the definitions before any row.
struct Source {
    contract: String,
    definition: Option<Definition>,
    labels: Vec<LabelColumn>,
    parents: Vec<Option<usize>>,
    predictions: Option<classify::Predictions>,
}

impl Source {
    fn read(config: &ExportLabelsConfig, record: &RunRecord, acquisition: &str) -> Result<Self> {
        match config.kind {
            LabelKind::Qc => {
                let filters = mask::filters(&config.input, acquisition)?;
                Ok(Self {
                    contract: mask::CONTRACT.into(),
                    definition: None,
                    labels: filters
                        .into_iter()
                        .map(|name| LabelColumn {
                            name,
                            parent: None,
                            class: None,
                        })
                        .collect(),
                    parents: Vec::new(),
                    predictions: None,
                })
            }
            LabelKind::Gate => Self::gates(config, record, acquisition),
            LabelKind::Classifier => Self::classifier(config, record, acquisition),
            LabelKind::Cluster => Self::clusters(config, record),
        }
    }

    fn clusters(config: &ExportLabelsConfig, record: &RunRecord) -> Result<Self> {
        let applied = record.tool == "cytiq flowsom apply";
        let definition = match (&config.cluster_model, applied) {
            (Some(model), true) => {
                let digest = hash_file(&model.path)?.1;
                require(
                    single(record, "model")? == digest,
                    "the FlowSOM model is not the one the producer applied",
                )?;
                require(
                    model.nodes > 0
                        && model.node_metaclusters.len() as u64 == model.nodes
                        && model
                            .node_metaclusters
                            .iter()
                            .all(|m| (1..=model.metaclusters).contains(m)),
                    "the FlowSOM model's metaclusters disagree with its nodes",
                )?;
                Some(Definition {
                    inputs: BTreeMap::from([("model".to_owned(), vec![digest])]),
                    parameters: BTreeMap::from([
                        ("nodes".to_owned(), serde_json::json!(model.nodes)),
                        (
                            "metaclusters".to_owned(),
                            serde_json::json!(model.metaclusters),
                        ),
                        (
                            "node_metaclusters".to_owned(),
                            serde_json::json!(model.node_metaclusters),
                        ),
                    ]),
                })
            }
            (None, true) => {
                return Err(PopulationError::Invalid(
                    "labels of `cytiq flowsom apply` need the FlowSOM model it applied".into(),
                ));
            }
            // A model trained on this acquisition alone exists nowhere else:
            // its clusters are not comparable with any other's.
            (None, false) => None,
            (Some(_), false) => {
                return Err(PopulationError::Invalid(
                    "`cytiq flowsom` trained its own model; give no FlowSOM model".into(),
                ));
            }
        };
        Ok(Self {
            contract: flowsom::CONTRACT.into(),
            definition,
            labels: Vec::new(),
            parents: Vec::new(),
            predictions: None,
        })
    }

    fn gates(config: &ExportLabelsConfig, record: &RunRecord, acquisition: &str) -> Result<Self> {
        let gating = config.gating.as_ref().ok_or_else(|| {
            PopulationError::Invalid(
                "gate labels need the Gating-ML file the gates came from".into(),
            )
        })?;
        let digest = hash_file(&gating.path)?.1;
        require(
            single(record, "gating_ml")? == digest,
            "the Gating-ML file is not the one the producer evaluated",
        )?;
        let columns = mask::filters(&config.input, acquisition)?;
        let gates: Vec<&str> = gating.gates.iter().map(|(id, _)| id.as_str()).collect();
        require(
            columns == gates,
            "the membership's gate columns are not the Gating-ML gates in evaluation order",
        )?;
        let labels: Vec<LabelColumn> = gating
            .gates
            .iter()
            .map(|(name, parent)| LabelColumn {
                name: name.clone(),
                parent: parent.clone(),
                class: None,
            })
            .collect();
        let parents = labels
            .iter()
            .map(|label| {
                label
                    .parent
                    .as_ref()
                    .and_then(|parent| labels.iter().position(|l| &l.name == parent))
            })
            .collect();
        Ok(Self {
            contract: mask::CONTRACT.into(),
            definition: Some(Definition {
                inputs: BTreeMap::from([("gating_ml".to_owned(), vec![digest])]),
                parameters: BTreeMap::new(),
            }),
            labels,
            parents,
            predictions: None,
        })
    }

    fn classifier(
        config: &ExportLabelsConfig,
        record: &RunRecord,
        acquisition: &str,
    ) -> Result<Self> {
        let predictions = classify::header(&config.input, acquisition)?;
        let model = digests(record, "model");
        require(!model.is_empty(), "the producer record names no model")?;
        let policies = digests(record, "decision_policy");
        match &predictions.policy {
            Some(policy) => require(
                policies.contains(policy),
                "the predictions' decision policy is not one the producer read",
            )?,
            None => require(
                policies.is_empty(),
                "the producer read a decision policy its predictions do not pin",
            )?,
        }
        let mut inputs = BTreeMap::from([("model".to_owned(), model)]);
        if !policies.is_empty() {
            inputs.insert("decision_policy".into(), policies);
        }
        let mut parameters = predictions.parameters.clone();
        parameters.insert(
            "abstain_margin".into(),
            record.config["abstain_margin"].clone(),
        );
        let labels = if config.probabilities {
            predictions
                .classes
                .iter()
                .map(|class| LabelColumn {
                    name: format!("p_{class}"),
                    parent: None,
                    class: Some(class.clone()),
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            contract: predictions.contract(),
            definition: Some(Definition { inputs, parameters }),
            labels,
            parents: Vec::new(),
            predictions: Some(predictions),
        })
    }
}

fn write(
    path: &Path,
    descriptor: LabelDescriptor,
    config: &ExportLabelsConfig,
    source: &Source,
    eligible: Option<&Bitmap>,
) -> Result<WrittenLabels> {
    let columns = descriptor.columns();
    let raw_events = descriptor.raw_events;
    let mut writer = LabelWriter::create(
        path,
        descriptor,
        &config.settings,
        ParquetReadLimits::default(),
    )?;
    let mut rows = Rows::new(&mut writer, &columns, raw_events, eligible);
    match (&source.predictions, config.kind) {
        (Some(predictions), _) => {
            classify::stream(&config.input, predictions, config.probabilities, &mut rows)?;
        }
        (None, LabelKind::Cluster) => flowsom::stream(&config.input, &mut rows)?,
        (None, kind) => mask::stream(&config.input, kind, &source.parents, &mut rows)?,
    }
    rows.finish()?;
    writer.finish()
}
