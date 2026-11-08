#![allow(clippy::cast_precision_loss)]
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use cytiq_fcs::{
    FcsDataset, FcsReader, FcsWriteOptions, FcsWriteParameter, NormalizationState, SignalState,
    TransformContract,
};
use cytiq_flowsom::{FlowSomConfig, FlowSomModel, Metaclustering};
use cytiq_publish::{Existing, Publication};
use serde::Serialize;
use serde_json::{Value, json};

use crate::bundle::{Goal, MarkerContract, write_mappings};
use crate::{
    CytoNormModel, Manifest, Mapping, Mode, ModelDocument, MonotoneSpline, Panel, Result, Role,
    SparsePolicy, Target, invalid,
};

#[derive(Clone, Debug, Serialize)]
pub struct TrainConfig {
    pub manifest: PathBuf,
    pub output: PathBuf,
    pub threads: Option<usize>,
    pub debug_dir: Option<PathBuf>,
    pub overwrite: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct ApplyConfig {
    pub input: PathBuf,
    pub model: PathBuf,
    pub batch: String,
    pub output: PathBuf,
    pub threads: Option<usize>,
    pub debug_dir: Option<PathBuf>,
    pub overwrite: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct RunResult {
    pub outputs: Vec<PathBuf>,
    pub provenance: PathBuf,
    pub model_sha256: String,
    pub events: usize,
}

struct LoadedSample {
    id: String,
    batch: String,
    role: Role,
    values: Vec<Vec<f64>>,
    clusters: Vec<usize>,
}

pub struct CytoNormRunner;
impl CytoNormRunner {
    /// All manifest, FCS and mask inputs, resolved before training.
    ///
    /// # Errors
    /// Returns manifest validation errors.
    pub fn input_files(manifest: &Path) -> Result<Vec<PathBuf>> {
        let m = Manifest::load(manifest)?;
        Ok(std::iter::once(manifest.to_path_buf())
            .chain(
                m.samples
                    .into_iter()
                    .flat_map(|s| std::iter::once(s.file).chain(s.masks)),
            )
            .collect())
    }

    /// Fit and atomically publish a panel-specific normalization bundle.
    ///
    /// # Errors
    /// Rejects incompatible cohorts, sparse mappings and output collisions.
    pub fn train(config: &TrainConfig) -> Result<RunResult> {
        with_pool(config.threads, || train_inner(config))
    }

    /// Apply fixed clustering and batch splines, preserving all event identities.
    ///
    /// # Errors
    /// Rejects unknown batches, incompatible signal state and invalid bundles.
    pub fn apply(config: &ApplyConfig) -> Result<RunResult> {
        with_pool(config.threads, || apply_inner(config))
    }
}

fn with_pool<T: Send>(
    threads: Option<usize>,
    work: impl FnOnce() -> Result<T> + Send,
) -> Result<T> {
    if let Some(threads) = threads {
        if threads == 0 {
            return Err(invalid("threads must be positive"));
        }
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_err(|e| invalid(e.to_string()))?
            .install(work)
    } else {
        work()
    }
}

struct TrainingCohort {
    samples: Vec<LoadedSample>,
    batches: BTreeMap<String, Option<String>>,
    transform: TransformContract,
    training_inputs: BTreeMap<String, Vec<String>>,
    sampling: Vec<Value>,
}

#[allow(clippy::too_many_lines)]
fn load_cohort(manifest: &Manifest, selected_channels: &[String]) -> Result<TrainingCohort> {
    let batch_names = manifest
        .samples
        .iter()
        .filter(|s| s.role == Role::Train)
        .map(|s| s.batch.clone())
        .collect::<BTreeSet<_>>();
    let mut batches = BTreeMap::new();
    let mut transform = None;
    let mut acquisition_roles: BTreeMap<String, Role> = BTreeMap::new();
    let mut samples = Vec::new();
    let mut training_inputs = BTreeMap::new();
    let mut sampling = Vec::new();
    for sample in &manifest.samples {
        let dataset = FcsReader::open(&sample.file)?.read()?;
        let contract = check_dataset(&dataset, &manifest.panel, selected_channels)?;
        if transform.as_ref().is_some_and(|t| t != &contract) {
            return Err(invalid(format!(
                "transform mismatch in sample {}",
                sample.id
            )));
        }
        transform = Some(contract);
        let acquisition = cytiq_fcs::acquisition_id(&sample.file, &dataset.text)?;
        if let Some(role) = acquisition_roles.insert(acquisition.clone(), sample.role) {
            return Err(invalid(if role == sample.role {
                "duplicate acquisition in cohort"
            } else {
                "train/validation acquisition overlap"
            }));
        }
        let instrument = instrument(&dataset);
        if let Some(previous) = batches.get(&sample.batch) {
            if previous != &instrument {
                return Err(invalid(format!(
                    "instrument mismatch within batch {}",
                    sample.batch
                )));
            }
        } else {
            batches.insert(sample.batch.clone(), instrument);
        }
        let keep = cytiq_mask::read_combined(&sample.masks)?
            .map(|m| m.keep_for(&dataset, &acquisition))
            .transpose()?
            .unwrap_or_else(|| vec![true; dataset.event_count()]);
        let rows = keep
            .iter()
            .enumerate()
            .filter_map(|(i, &keep)| keep.then_some(i))
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Err(invalid(format!("sample {} has no kept events", sample.id)));
        }
        let mut values = panel_values(&dataset, &manifest.panel, &rows)?;
        let available = values.len();
        if sample.role == Role::Train {
            let hashes = std::iter::once(&sample.file)
                .chain(&sample.masks)
                .map(|p| cytiq_provenance::hash_file(p).map(|(_, h)| h))
                .collect::<std::io::Result<Vec<_>>>()?;
            training_inputs.insert(sample.id.clone(), hashes);
            if manifest.mode == Mode::Aggregate {
                let files = manifest
                    .samples
                    .iter()
                    .filter(|s| s.role == Role::Train && s.batch == sample.batch)
                    .count();
                let quota = manifest.training_events / batch_names.len() / files;
                if quota == 0 {
                    return Err(invalid(
                        "training_events is smaller than the balanced file quotas",
                    ));
                }
                values = subsample(&values, quota, sample_seed(manifest.som.seed, &sample.id));
            }
        }
        sampling.push(json!({"sample":sample.id,"batch":sample.batch,"role":sample.role,"available_events":available,"selected_events":values.len()}));
        samples.push(LoadedSample {
            id: sample.id.clone(),
            batch: sample.batch.clone(),
            role: sample.role,
            values,
            clusters: Vec::new(),
        });
    }
    let transform = transform.ok_or_else(|| invalid("no transform contract"))?;
    Ok(TrainingCohort {
        samples,
        batches,
        transform,
        training_inputs,
        sampling,
    })
}

#[allow(clippy::too_many_lines)]
fn fit_mappings(
    manifest: &Manifest,
    samples: &[LoadedSample],
    normal_indices: &[usize],
) -> Result<(Vec<Goal>, Vec<Mapping>)> {
    let batch_names = samples
        .iter()
        .filter(|s| s.role == Role::Train)
        .map(|s| s.batch.clone())
        .collect::<BTreeSet<_>>();
    let k = manifest.som.metaclusters;
    let mut distributions: BTreeMap<(String, usize, String), (usize, Vec<f64>)> = BTreeMap::new();
    for batch in &batch_names {
        for cluster in 1..=k {
            for (marker, &index) in manifest.normalization_markers.iter().zip(normal_indices) {
                let mut values = samples
                    .iter()
                    .filter(|s| s.role == Role::Train && s.batch == *batch)
                    .flat_map(|s| {
                        s.values
                            .iter()
                            .zip(&s.clusters)
                            .filter_map(|(v, &c)| (c == cluster).then_some(v[index]))
                    })
                    .collect::<Vec<_>>();
                values.sort_by(f64::total_cmp);
                let q = quantiles(&values, &manifest.probabilities);
                distributions.insert((batch.clone(), cluster, marker.clone()), (values.len(), q));
            }
        }
    }
    let reliable = |count: usize, q: &[f64]| {
        count >= manifest.min_cluster_events && q.len() >= 2 && q[0] < q[q.len() - 1]
    };
    let mut goals = Vec::new();
    for cluster in 1..=k {
        for marker in &manifest.normalization_markers {
            let eligible = distributions
                .iter()
                .filter(|((b, c, m), (n, q))| {
                    *c == cluster
                        && m == marker
                        && reliable(*n, q)
                        && match &manifest.target {
                            Target::Mean => true,
                            Target::Batch { batch } => b == batch,
                        }
                })
                .map(|(_, (_, q))| q)
                .collect::<Vec<_>>();
            let goal = (!eligible.is_empty()).then(|| {
                (0..manifest.probabilities.len())
                    .map(|i| eligible.iter().map(|q| q[i]).sum::<f64>() / eligible.len() as f64)
                    .collect()
            });
            goals.push(Goal {
                metacluster: cluster,
                marker: marker.clone(),
                quantiles: goal,
            });
        }
    }
    let mut mappings = Vec::new();
    for ((batch, metacluster, marker), (events, q)) in distributions {
        let target = goals
            .iter()
            .find(|g| g.metacluster == metacluster && g.marker == marker)
            .and_then(|g| g.quantiles.as_deref());
        let mut reason = if events < manifest.min_cluster_events {
            Some(format!(
                "only {events} events; minimum {}",
                manifest.min_cluster_events
            ))
        } else if q.len() < 2 || q[0] >= q[q.len() - 1] {
            Some("fewer than two distinct source knots".into())
        } else if target.is_none() {
            Some("reference target has insufficient data".into())
        } else {
            None
        };
        let spline = if reason.is_none() {
            match MonotoneSpline::fit(&q, target.ok_or_else(|| invalid("missing target"))?) {
                Ok(spline) => Some(spline),
                Err(error) => {
                    reason = Some(format!("unsafe spline: {error}"));
                    None
                }
            }
        } else {
            None
        };
        if let Some(reason) = &reason
            && manifest.sparse_policy == SparsePolicy::Error
        {
            return Err(invalid(format!(
                "batch {batch}, cluster {metacluster}, marker {marker}: {reason}; choose sparse_policy: identity explicitly to leave this mapping unchanged"
            )));
        }
        mappings.push(Mapping {
            batch,
            metacluster,
            marker,
            events,
            spline,
            identity_reason: reason,
        });
    }
    Ok((goals, mappings))
}

fn train_inner(config: &TrainConfig) -> Result<RunResult> {
    let manifest = Manifest::load(&config.manifest)?;
    let inputs = CytoNormRunner::input_files(&config.manifest)?;
    check_destination(&config.output, &inputs, config.overwrite)?;
    if config.output.exists()
        && (!config.output.is_dir() || !config.output.join("model.json").is_file())
    {
        return Err(invalid(
            "overwrite requires an existing CytoNorm bundle directory",
        ));
    }
    if let Some(debug) = &config.debug_dir {
        check_separate_debug(&config.output, debug)?;
        check_destination(debug, &inputs, config.overwrite)?;
    }
    let cluster_indices = marker_indices(&manifest.panel, &manifest.clustering_markers)?;
    let normal_indices = marker_indices(&manifest.panel, &manifest.normalization_markers)?;
    let selected = cluster_indices
        .iter()
        .chain(&normal_indices)
        .copied()
        .collect::<BTreeSet<_>>();
    let selected_channels = selected
        .iter()
        .map(|&i| manifest.panel.markers[i].channel.clone())
        .collect::<Vec<_>>();
    let TrainingCohort {
        mut samples,
        batches,
        transform,
        training_inputs,
        sampling,
    } = load_cohort(&manifest, &selected_channels)?;
    let batch_names = batches.keys().cloned().collect::<BTreeSet<_>>();
    let som_values = balanced_training_values(&manifest, &samples, &batch_names, &cluster_indices)?;
    let som = train_som(&manifest, &cluster_indices, &som_values, &transform)?;
    for s in &mut samples {
        s.clusters = som
            .assign(&select_columns(&s.values, &cluster_indices))?
            .into_iter()
            .map(|(_, c)| c)
            .collect();
    }
    let (goals, mappings) = fit_mappings(&manifest, &samples, &normal_indices)?;
    let markers = MarkerContract {
        panel: manifest.panel.clone(),
        transform,
    };
    let document = ModelDocument {
        schema_version: 1,
        algorithm: "cytonorm-2.0".into(),
        mode: manifest.mode,
        target: manifest.target.clone(),
        batches,
        normalization_markers: manifest.normalization_markers.clone(),
        probabilities: manifest.probabilities.clone(),
        sparse_policy: manifest.sparse_policy,
        min_cluster_events: manifest.min_cluster_events,
        som: manifest.som.clone(),
        training_events: som_values.len(),
        training_inputs,
        files: BTreeMap::new(),
        goals,
    };
    let model = CytoNormModel {
        document,
        markers,
        som,
        mappings,
        model_sha256: String::new(),
        panel_sha256: String::new(),
    };
    model.validate()?;
    let mut qc = cohort_qc(&samples, &model)?;
    qc["sampling"] = json!(sampling);
    publish_training_model(config, model, &qc, &samples)
}

fn balanced_training_values(
    manifest: &Manifest,
    samples: &[LoadedSample],
    batch_names: &BTreeSet<String>,
    cluster_indices: &[usize],
) -> Result<Vec<Vec<f64>>> {
    let mut som_values = Vec::new();
    for batch in batch_names {
        let files = samples
            .iter()
            .filter(|s| s.role == Role::Train && s.batch == *batch)
            .count();
        let quota = manifest.training_events / batch_names.len() / files;
        if quota == 0 {
            return Err(invalid(
                "training_events is smaller than the balanced file quotas",
            ));
        }
        for sample in samples
            .iter()
            .filter(|s| s.role == Role::Train && s.batch == *batch)
        {
            let rows = subsample(
                &sample.values,
                quota,
                sample_seed(manifest.som.seed, &sample.id),
            );
            som_values.extend(select_columns(&rows, cluster_indices));
        }
    }
    Ok(som_values)
}

fn train_som(
    manifest: &Manifest,
    cluster_indices: &[usize],
    som_values: &[Vec<f64>],
    transform: &TransformContract,
) -> Result<FlowSomModel> {
    let mut som_config = FlowSomConfig::new(
        cluster_indices
            .iter()
            .map(|&i| manifest.panel.markers[i].channel.clone())
            .collect(),
    );
    som_config.xdim = manifest.som.xdim;
    som_config.ydim = manifest.som.ydim;
    som_config.metaclusters = manifest.som.metaclusters;
    som_config.scale = manifest.som.scale;
    som_config.seed = manifest.som.seed;
    let mut som = FlowSomModel::train(som_values, &som_config, Metaclustering::Consensus)?;
    som.transform = Some(transform.clone());
    som.channel_labels = Some(
        cluster_indices
            .iter()
            .map(|&i| {
                let marker = &manifest.panel.markers[i];
                if marker.labels.is_empty() {
                    vec![marker.marker.clone()]
                } else {
                    marker.labels.clone()
                }
            })
            .collect(),
    );
    Ok(som)
}

fn publish_training_model(
    config: &TrainConfig,
    mut model: CytoNormModel,
    qc: &Value,
    samples: &[LoadedSample],
) -> Result<RunResult> {
    let mut publication = Publication::new(Existing::overwrite(config.overwrite));
    let stage = publication.stage_dir(&config.output)?;
    write_json(&stage.join("som.json"), &model.som)?;
    write_json(&stage.join("markers.json"), &model.markers)?;
    write_mappings(&stage.join("splines.parquet"), &model.mappings)?;
    write_json(&stage.join("training-qc.json"), qc)?;
    for name in ["som.json", "markers.json", "splines.parquet"] {
        model.document.files.insert(
            name.into(),
            cytiq_provenance::hash_file(&stage.join(name))?.1,
        );
    }
    write_json(&stage.join("model.json"), &model.document)?;
    let model = CytoNormModel::load(&stage)?;
    if let Some(debug) = &config.debug_dir {
        write_diagnostics(debug, &model, qc, config.overwrite)?;
    }
    publication.commit()?;
    let mut outputs = CytoNormModel::input_files(&config.output);
    outputs.push(config.output.join("training-qc.json"));
    if let Some(debug) = &config.debug_dir {
        outputs.push(debug.join("report.html"));
    }
    Ok(RunResult {
        outputs,
        provenance: cytiq_provenance::record_path_for_dir(&config.output),
        model_sha256: model.model_sha256,
        events: samples
            .iter()
            .filter(|s| s.role == Role::Train)
            .map(|s| s.values.len())
            .sum(),
    })
}

fn apply_inner(config: &ApplyConfig) -> Result<RunResult> {
    if !config
        .output
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("fcs"))
    {
        return Err(invalid("apply output must be .fcs"));
    }
    let model = CytoNormModel::load(&config.model)?;
    let expected = model
        .document
        .batches
        .get(&config.batch)
        .ok_or_else(|| invalid(format!("unknown batch: {}", config.batch)))?;
    let mut inputs = CytoNormModel::input_files(&config.model);
    inputs.push(config.input.clone());
    let qc_path = qc_path(&config.output);
    let provenance = cytiq_provenance::record_path_for_file(&config.output);
    for p in [&config.output, &qc_path, &provenance] {
        check_destination(p, &inputs, config.overwrite)?;
    }
    if let Some(debug) = &config.debug_dir {
        check_separate_debug(&config.output, debug)?;
        check_destination(debug, &inputs, config.overwrite)?;
    }
    let dataset = FcsReader::open(&config.input)?.read()?;
    let cluster_indices = channels_indices(&model.markers.panel, &model.som.channels)?;
    let normal_indices =
        marker_indices(&model.markers.panel, &model.document.normalization_markers)?;
    let selected = cluster_indices
        .iter()
        .chain(&normal_indices)
        .copied()
        .collect::<BTreeSet<_>>();
    let channels = selected
        .iter()
        .map(|&i| model.markers.panel.markers[i].channel.clone())
        .collect::<Vec<_>>();
    if check_dataset(&dataset, &model.markers.panel, &channels)? != model.markers.transform
        || &instrument(&dataset) != expected
    {
        return Err(invalid(
            "input transformation or batch instrument does not match model",
        ));
    }
    let rows = (0..dataset.event_count()).collect::<Vec<_>>();
    let values = panel_values(&dataset, &model.markers.panel, &rows)?;
    let clusters = model
        .som
        .assign(&select_columns(&values, &cluster_indices))?
        .into_iter()
        .map(|(_, c)| c)
        .collect::<Vec<_>>();
    let sample = LoadedSample {
        id: config
            .input
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        batch: config.batch.clone(),
        role: Role::Validation,
        values,
        clusters,
    };
    let qc = cohort_qc(std::slice::from_ref(&sample), &model)?;
    let mut publication = Publication::new(Existing::overwrite(config.overwrite));
    publication.stage_path(&config.output, |path| {
        write_normalized_fcs(path, &config.input, &dataset, &sample, &model)
    })?;
    publication.stage_path(&qc_path, |path| write_json(path, &qc))?;
    if let Some(debug) = &config.debug_dir {
        write_diagnostics(debug, &model, &qc, config.overwrite)?;
    }
    publication.commit()?;
    let mut outputs = vec![config.output.clone(), qc_path];
    if let Some(debug) = &config.debug_dir {
        outputs.push(debug.join("report.html"));
    }
    Ok(RunResult {
        outputs,
        provenance,
        model_sha256: model.model_sha256,
        events: dataset.event_count(),
    })
}

fn write_normalized_fcs(
    path: &Path,
    source_path: &Path,
    dataset: &FcsDataset,
    sample: &LoadedSample,
    model: &CytoNormModel,
) -> Result<()> {
    let normal_indices =
        marker_indices(&model.markers.panel, &model.document.normalization_markers)?;
    let normalized = normalize(sample, model)?;
    let mut columns = dataset
        .data
        .columns
        .iter()
        .map(cytiq_fcs::ColumnData::to_f64_vec)
        .collect::<Vec<_>>();
    let panel_columns = resolve_panel(dataset, &model.markers.panel)?;
    for &index in &normal_indices {
        columns[panel_columns[index]] = normalized.iter().map(|r| r[index]).collect();
    }
    if columns.iter().flatten().any(|v| !v.is_finite()) {
        return Err(invalid("normalization produced nonfinite values"));
    }
    let normalized_channels = normal_indices
        .iter()
        .map(|&i| model.markers.panel.markers[i].channel.clone())
        .collect::<Vec<_>>();
    let parameters = dataset
        .parameters
        .iter()
        .map(|p| {
            let mut out = FcsWriteParameter::from_source(p);
            if normalized_channels.contains(&p.name) {
                out.range = None;
            }
            out
        })
        .collect::<Vec<_>>();
    let mut keywords = cytiq_fcs::carried_keywords(&dataset.text)
        .into_iter()
        .filter(|(key, _)| key != cytiq_fcs::MARGINS_KEYWORD)
        .collect::<Vec<_>>();
    let acquisition = cytiq_fcs::acquisition_id(source_path, &dataset.text)?;
    cytiq_fcs::set_acquisition(&mut keywords, &acquisition);
    let mut bounds = cytiq_fcs::dataset_margin_bounds(&dataset.text, &dataset.parameters)?;
    bounds.retain(|channel, _| !normalized_channels.contains(channel));
    keywords.push((
        cytiq_fcs::MARGINS_KEYWORD.into(),
        cytiq_fcs::encode_margin_bounds(&bounds),
    ));
    let normalization = NormalizationState {
        schema_version: 1,
        model_sha256: model.model_sha256.clone(),
        batch: sample.batch.clone(),
        panel_sha256: model.panel_sha256.clone(),
        channels: normalized_channels,
    };
    keywords.push((
        cytiq_fcs::NORMALIZATION_KEYWORD.into(),
        serde_json::to_string(&normalization)?,
    ));
    cytiq_fcs::write_fcs3_1_exact_with_options(
        path,
        &parameters,
        &columns,
        &FcsWriteOptions::new().with_text_keywords(keywords),
    )?;
    Ok(())
}

fn check_dataset(
    dataset: &FcsDataset,
    panel: &Panel,
    channels: &[String],
) -> Result<TransformContract> {
    cytiq_fcs::ensure_linear_scale(dataset)?;
    cytiq_fcs::source_event_ids(dataset)?;
    if dataset.event_count() == 0 {
        return Err(invalid("empty FCS input"));
    }
    let state = SignalState::of(&dataset.text)?;
    if state.normalization.is_some() {
        return Err(invalid("input is already normalized"));
    }
    resolve_panel(dataset, panel)?;
    for channel in channels {
        if !state.compensated(channel) && !state.unmixed(channel) {
            return Err(invalid(format!(
                "channel {channel} lacks verified compensation or unmixing"
            )));
        }
    }
    if cytiq_fcs::Spillover::from_dataset(dataset)?.is_some() {
        return Err(invalid("pending spillover matrix on transformed input"));
    }
    TransformContract::of(&dataset.text, channels).map_err(Into::into)
}

fn resolve_panel(dataset: &FcsDataset, panel: &Panel) -> Result<Vec<usize>> {
    for parameter in &dataset.parameters {
        let name = parameter.name.to_ascii_uppercase();
        let acquisition = matches!(
            name.as_str(),
            "TIME"
                | "ORIGINAL_ID"
                | "EVENT_LENGTH"
                | "EVENT-LENGTH"
                | "EVENTLENGTH"
                | "CELL_LENGTH"
                | "CELL-LENGTH"
                | "CELLLENGTH"
        ) || name.starts_with("FSC-")
            || name.starts_with("SSC-")
            || name.starts_with("FSC_")
            || name.starts_with("SSC_");
        if !acquisition && !panel.markers.iter().any(|m| m.channel == parameter.name) {
            return Err(invalid(format!(
                "channel {} is outside the declared panel",
                parameter.name
            )));
        }
    }
    panel
        .markers
        .iter()
        .map(|marker| {
            let matches = dataset
                .parameters
                .iter()
                .enumerate()
                .filter(|(_, p)| p.name == marker.channel)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(invalid(format!(
                    "missing or ambiguous panel channel {}",
                    marker.channel
                )));
            }
            let (index, parameter) = matches[0];
            let label = parameter.label.as_deref();
            let matches_label = if marker.labels.is_empty() {
                label == Some(marker.marker.as_str())
            } else {
                label.is_some_and(|l| marker.labels.iter().any(|a| a == l))
            };
            if !matches_label {
                return Err(invalid(format!(
                    "marker label does not match panel: {}",
                    marker.marker
                )));
            }
            Ok(index)
        })
        .collect()
}

fn panel_values(dataset: &FcsDataset, panel: &Panel, rows: &[usize]) -> Result<Vec<Vec<f64>>> {
    let columns = resolve_panel(dataset, panel)?
        .into_iter()
        .map(|i| dataset.data.columns[i].to_f64_vec())
        .collect::<Vec<_>>();
    let values = rows
        .iter()
        .map(|&r| columns.iter().map(|c| c[r]).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    if values.iter().flatten().any(|v| !v.is_finite()) {
        return Err(invalid("nonfinite panel data"));
    }
    Ok(values)
}

fn marker_indices(panel: &Panel, names: &[String]) -> Result<Vec<usize>> {
    names
        .iter()
        .map(|n| {
            panel
                .markers
                .iter()
                .position(|m| m.marker == *n)
                .ok_or_else(|| invalid(format!("unknown marker: {n}")))
        })
        .collect()
}
fn channels_indices(panel: &Panel, names: &[String]) -> Result<Vec<usize>> {
    names
        .iter()
        .map(|n| {
            panel
                .markers
                .iter()
                .position(|m| m.channel == *n)
                .ok_or_else(|| invalid(format!("unknown channel: {n}")))
        })
        .collect()
}
fn select_columns(values: &[Vec<f64>], indices: &[usize]) -> Vec<Vec<f64>> {
    values
        .iter()
        .map(|r| indices.iter().map(|&i| r[i]).collect())
        .collect()
}
fn instrument(dataset: &FcsDataset) -> Option<String> {
    dataset.text.iter().find_map(|(k, v)| {
        k.trim_start_matches('$')
            .eq_ignore_ascii_case("CYT")
            .then(|| v.to_owned())
    })
}
fn sample_seed(seed: u64, id: &str) -> u64 {
    id.bytes().fold(seed ^ 0x9e37_79b9_7f4a_7c15, |s, b| {
        s.wrapping_mul(0x100_0000_01b3) ^ u64::from(b)
    })
}
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn subsample(values: &[Vec<f64>], quota: usize, mut seed: u64) -> Vec<Vec<f64>> {
    let n = quota.min(values.len());
    let mut ids = (0..values.len()).collect::<Vec<_>>();
    for i in 0..n {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j =
            i + ((seed >> 11) as f64 / (1_u64 << 53) as f64 * (values.len() - i) as f64) as usize;
        ids.swap(i, j.min(values.len() - 1));
    }
    ids.truncate(n);
    ids.sort_unstable();
    ids.into_iter().map(|i| values[i].clone()).collect()
}
fn quantiles(sorted: &[f64], probabilities: &[f64]) -> Vec<f64> {
    if sorted.is_empty() {
        Vec::new()
    } else {
        probabilities
            .iter()
            .map(|&p| cytiq_density::quantile_type7(sorted, p))
            .collect()
    }
}

fn normalize(sample: &LoadedSample, model: &CytoNormModel) -> Result<Vec<Vec<f64>>> {
    let indices = marker_indices(&model.markers.panel, &model.document.normalization_markers)?;
    let maps = model
        .mappings
        .iter()
        .filter(|m| m.batch == sample.batch)
        .map(|m| ((m.metacluster, m.marker.as_str()), m))
        .collect::<BTreeMap<_, _>>();
    let mut values = sample.values.clone();
    for (row, &cluster) in values.iter_mut().zip(&sample.clusters) {
        for (marker, &i) in model.document.normalization_markers.iter().zip(&indices) {
            let mapping = maps
                .get(&(cluster, marker.as_str()))
                .ok_or_else(|| invalid("missing normalization mapping"))?;
            if let Some(spline) = &mapping.spline {
                row[i] = spline.evaluate(row[i]);
            }
            if !row[i].is_finite() {
                return Err(invalid("nonfinite normalized signal"));
            }
        }
    }
    Ok(values)
}

fn cohort_qc(samples: &[LoadedSample], model: &CytoNormModel) -> Result<Value> {
    let indices = marker_indices(&model.markers.panel, &model.document.normalization_markers)?;
    let k = model.document.som.metaclusters;
    let mut metrics = Vec::new();
    let mut occupancy = Vec::new();
    let mut train_frequencies = vec![Vec::new(); k];
    let mut validation_frequencies = vec![Vec::new(); k];
    for sample in samples {
        let after = normalize(sample, model)?;
        for cluster in 1..=k {
            let rows = sample
                .clusters
                .iter()
                .enumerate()
                .filter_map(|(i, &c)| (c == cluster).then_some(i))
                .collect::<Vec<_>>();
            let frequency = rows.len() as f64 / sample.values.len() as f64;
            if sample.role == Role::Train {
                train_frequencies[cluster - 1].push(frequency);
            } else {
                validation_frequencies[cluster - 1].push(frequency);
            }
            let means = (0..model.markers.panel.markers.len())
                .map(|i| {
                    (!rows.is_empty()).then(|| {
                        rows.iter().map(|&r| sample.values[r][i]).sum::<f64>() / rows.len() as f64
                    })
                })
                .collect::<Vec<_>>();
            occupancy.push(json!({"sample":sample.id,"batch":sample.batch,"role":sample.role,"metacluster":cluster,"events":rows.len(),"frequency":frequency,"marker_means":means}));
            for (marker, &i) in model.document.normalization_markers.iter().zip(&indices) {
                let mut before = rows
                    .iter()
                    .map(|&r| sample.values[r][i])
                    .collect::<Vec<_>>();
                before.sort_by(f64::total_cmp);
                let mut normalized = rows.iter().map(|&r| after[r][i]).collect::<Vec<_>>();
                normalized.sort_by(f64::total_cmp);
                let before = quantiles(&before, &model.document.probabilities);
                let after = quantiles(&normalized, &model.document.probabilities);
                let goal = model.goal(cluster, marker);
                let emd = |q: &[f64]| {
                    goal.filter(|g| !q.is_empty() && g.len() == q.len())
                        .map(|g| {
                            g.iter().zip(q).map(|(a, b)| (a - b).abs()).sum::<f64>()
                                / q.len() as f64
                        })
                };
                let mapping = model
                    .mappings
                    .iter()
                    .find(|m| {
                        m.batch == sample.batch && m.metacluster == cluster && m.marker == *marker
                    })
                    .ok_or_else(|| invalid("missing QC mapping"))?;
                let outside = mapping.spline.as_ref().map_or(0, |s| {
                    rows.iter()
                        .filter(|&&r| {
                            sample.values[r][i] < s.x[0] || sample.values[r][i] > s.x[s.x.len() - 1]
                        })
                        .count()
                });
                let max_shift = rows
                    .iter()
                    .map(|&r| (sample.values[r][i] - normalized_value(sample, r, i, mapping)).abs())
                    .fold(0.0, f64::max);
                metrics.push(json!({"sample":sample.id,"batch":sample.batch,"role":sample.role,"metacluster":cluster,"marker":marker,"events":rows.len(),
                    "emd_quantile_approx_before":emd(&before),"emd_quantile_approx_after":emd(&after),"quantiles_before":before,"quantiles_after":after,
                    "outside_knots_events":outside,"max_absolute_shift":max_shift,"identity_reason":mapping.identity_reason}));
            }
        }
    }
    let mut cv = Vec::new();
    for (role, frequencies) in [
        (Role::Train, train_frequencies),
        (Role::Validation, validation_frequencies),
    ] {
        for (c, values) in frequencies.into_iter().enumerate() {
            let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
            let coefficient = (values.len() > 1 && mean > 0.0).then(|| {
                (values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64)
                    .sqrt()
                    / mean
            });
            cv.push(json!({"role":role,"metacluster":c+1,"cv":coefficient}));
        }
    }
    let mut warnings = Vec::new();
    if model.document.mode == Mode::Aggregate {
        warnings.push("Aggregate mode assumes comparable biological composition across batches; QC cannot prove absence of biological overcorrection.");
    }
    if model.mappings.iter().any(|m| m.identity_reason.is_some()) {
        warnings.push("Some batch/cluster/marker mappings are identity fallbacks; these values were not corrected.");
    }
    for warning in &warnings {
        log::warn!("{warning}");
    }
    Ok(
        json!({"schema_version":1,"mode":model.document.mode,"probabilities":model.document.probabilities,"warnings":warnings,"occupancy":occupancy,"occupancy_cv":cv,"distributions":metrics}),
    )
}
fn normalized_value(sample: &LoadedSample, row: usize, index: usize, mapping: &Mapping) -> f64 {
    mapping
        .spline
        .as_ref()
        .map_or(sample.values[row][index], |s| {
            s.evaluate(sample.values[row][index])
        })
}

fn write_diagnostics(
    directory: &Path,
    model: &CytoNormModel,
    qc: &Value,
    overwrite: bool,
) -> Result<()> {
    use plotly::{Plot, Scatter, common::Mode as PlotMode};
    let path = directory.join("report.html");
    if path.exists() && !overwrite {
        return Err(invalid("debug report exists; pass --overwrite"));
    }
    let mut plot = Plot::new();
    for m in &model.mappings {
        if let Some(s) = &m.spline {
            plot.add_trace(
                Scatter::new(s.x.clone(), s.y.clone())
                    .mode(PlotMode::LinesMarkers)
                    .name(format!("{} / {} / {}", m.batch, m.metacluster, m.marker)),
            );
        }
    }
    let mut distributions = Plot::new();
    if let Some(rows) = qc["distributions"].as_array() {
        for row in rows {
            for key in ["quantiles_before", "quantiles_after"] {
                let x = row[key]
                    .as_array()
                    .map(|v| v.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
                    .unwrap_or_default();
                if !x.is_empty() {
                    distributions.add_trace(
                        Scatter::new(x, model.document.probabilities.clone())
                            .mode(PlotMode::Lines)
                            .name(format!(
                                "{} / {} / {} / {key}",
                                row["sample"], row["metacluster"], row["marker"]
                            )),
                    );
                }
            }
        }
    }
    fs::create_dir_all(directory)?;
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>CytoNorm diagnostics</title></head><body><h1>CytoNorm splines</h1>{}<h2>Before/after quantile distributions</h2>{}<p>EMD values are quantile approximations. Inspect population occupancy and biological composition alongside these plots.</p></body></html>",
        plot.to_inline_html(Some("splines")),
        distributions.to_inline_html(Some("distributions"))
    );
    // Plotly inline fragments omit the runtime script.
    let html = html.replace(
        "</head>",
        "<script src=\"https://cdn.plot.ly/plotly-3.0.1.min.js\"></script></head>",
    );
    fs::write(path, html)?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(File::create(path)?, value)?;
    Ok(())
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
fn qc_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_os_string();
    name.push(".cytonorm-qc.json");
    PathBuf::from(name)
}
fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        fs::canonicalize(path)
    } else if let Some(name) = path.file_name() {
        Ok(resolved(parent(path))?.join(name))
    } else {
        fs::canonicalize(path)
    }
}
fn check_destination(output: &Path, inputs: &[PathBuf], overwrite: bool) -> Result<()> {
    if fs::symlink_metadata(output).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("output must not be a symlink"));
    }
    if output.exists() && !overwrite {
        return Err(invalid(format!(
            "output exists: {}; pass --overwrite",
            output.display()
        )));
    }
    let output = resolved(output)?;
    for input in inputs {
        let input = fs::canonicalize(input)?;
        if input == output || input.starts_with(&output) {
            return Err(invalid("output aliases or contains an input"));
        }
    }
    Ok(())
}

fn check_separate_debug(output: &Path, debug: &Path) -> Result<()> {
    let output = resolved(output)?;
    let debug = resolved(debug)?;
    if output.starts_with(&debug) || debug.starts_with(&output) {
        return Err(invalid("debug-dir must be separate from the output path"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        serde_json::from_value(json!({"schema_version":1,"panel":{"id":"panel","markers":[{"marker":"A","channel":"A"},{"marker":"B","channel":"B","labels":["CD4"]}]},"mode":"controls","clustering_markers":["A"],"normalization_markers":["B"],"training_events":4,"samples":[],"som":{"xdim":2,"ydim":1,"metaclusters":2,"scale":true,"seed":23}})).unwrap()
    }
    fn sample(id: &str, batch: &str, role: Role, base: f64) -> LoadedSample {
        LoadedSample {
            id: id.into(),
            batch: batch.into(),
            role,
            values: (0..5)
                .map(|i| vec![f64::from(i), base + f64::from(i)])
                .collect(),
            clusters: vec![],
        }
    }

    #[test]
    fn balanced_training_samples_each_file_without_using_validation_rows() {
        let mut manifest = manifest();
        let samples = [
            sample("a1", "a", Role::Train, 100.0),
            sample("a2", "a", Role::Train, 200.0),
            sample("b1", "b", Role::Train, 300.0),
            sample("validation", "a", Role::Validation, 900.0),
        ];
        let batches = BTreeSet::from(["a".into(), "b".into()]);
        let values = balanced_training_values(&manifest, &samples, &batches, &[1]).unwrap();
        assert_eq!(values.len(), 4);
        for (index, base) in [100.0, 200.0, 300.0, 300.0].iter().enumerate() {
            assert_eq!(values[index].len(), 1);
            assert!((*base..*base + 5.0).contains(&values[index][0]));
        }
        assert_eq!(
            values,
            balanced_training_values(&manifest, &samples, &batches, &[1]).unwrap()
        );
        manifest.training_events = 3;
        assert!(
            balanced_training_values(&manifest, &samples, &batches, &[1])
                .unwrap_err()
                .to_string()
                .contains("balanced file quotas")
        );
    }

    #[test]
    fn trained_som_keeps_marker_order_labels_scale_and_transform_contract() {
        let manifest = manifest();
        let transform = TransformContract {
            method: "arcsinh".into(),
            parameters: json!({"cofactor":150.0}),
        };
        let values = [
            vec![10.0, 0.0],
            vec![11.0, 1.0],
            vec![20.0, 10.0],
            vec![21.0, 11.0],
        ];
        let model = train_som(&manifest, &[1, 0], &values, &transform).unwrap();
        assert_eq!(model.channels, ["B", "A"]);
        assert_eq!(
            model.channel_labels,
            Some(vec![vec!["CD4".into()], vec!["A".into()]])
        );
        assert_eq!(
            (
                model.xdim,
                model.ydim,
                model.seed,
                model.scale_input,
                model.training_events
            ),
            (2, 1, 23, true, 4)
        );
        assert_eq!(model.metaclustering, Metaclustering::Consensus);
        assert_eq!(model.transform, Some(transform));
    }
}
