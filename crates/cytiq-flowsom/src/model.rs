//! Portable SOM inference and independently implemented consensus clustering.

use cytiq_fcs::{SignalState, TransformContract};
use cytiq_publish::{Existing, Publication};
use serde::{Deserialize, Serialize};

use super::{
    Cluster, DeterministicRng, EQUALITY_EPSILON, FlowSomAssignment, FlowSomConfig, FlowSomError,
    FlowSomScaleSummary, ResolvedChannel, SomGrid, assign_events, channel_matrix,
    closest_cluster_pair, hierarchical_metaclusters, load_dataset, resolve_channels, scale_matrix,
    selected_rows, train_som, training_rows, validate_config, with_thread_pool,
    write_assignments_csv,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metaclustering {
    #[default]
    Average,
    Consensus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSomModel {
    pub schema_version: u32,
    pub channels: Vec<String>,
    /// Allowed reagent labels per channel; empty means an unlabeled channel.
    /// Matrix-only callers may leave this unspecified.
    #[serde(default)]
    pub channel_labels: Option<Vec<Vec<String>>>,
    pub xdim: usize,
    pub ydim: usize,
    pub seed: u64,
    pub scale_input: bool,
    pub scale: Vec<FlowSomScaleSummary>,
    pub codebook: Vec<Vec<f64>>,
    /// One-based metacluster for each node, in codebook order.
    pub node_metaclusters: Vec<usize>,
    pub metaclustering: Metaclustering,
    pub training_events: usize,
    pub transform: Option<TransformContract>,
    pub normalization_model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PopulationStatistics {
    pub metacluster: usize,
    pub events: usize,
    pub frequency: f64,
    pub marker_means: Vec<f64>,
}

fn invalid(message: &str) -> FlowSomError {
    FlowSomError::InvalidConfig(message.to_owned())
}

impl FlowSomModel {
    /// Train and save after checking that outputs cannot replace source files.
    ///
    /// # Errors
    /// Rejects input aliases, existing outputs and invalid training data.
    pub fn train_to_file(
        inputs: &[PathBuf],
        output: &Path,
        config: &FlowSomConfig,
        method: Metaclustering,
        overwrite: bool,
    ) -> Result<Self, FlowSomError> {
        let sources = inputs
            .iter()
            .chain(&config.masks)
            .cloned()
            .collect::<Vec<_>>();
        check_file_outputs(output, &sources, overwrite)?;
        let model = Self::train_files(inputs, config, method)?;
        model.save(output, overwrite)?;
        Ok(model)
    }
    /// Apply a model file after protecting it and masks from output collisions.
    ///
    /// # Errors
    /// Returns model, input, mask, output or preprocessing validation errors.
    pub fn apply_model_file(
        model_path: &Path,
        input: &Path,
        output: &Path,
        masks: &[PathBuf],
        threads: Option<usize>,
        overwrite: bool,
    ) -> Result<usize, FlowSomError> {
        let sources = std::iter::once(input.to_path_buf())
            .chain(std::iter::once(model_path.to_path_buf()))
            .chain(masks.iter().cloned())
            .collect::<Vec<_>>();
        check_file_outputs(output, &sources, overwrite)?;
        Self::load(model_path)?.apply_file(input, output, masks, threads, overwrite)
    }
    /// Train reusable SOM geometry on a row-major matrix in channel order.
    ///
    /// # Errors
    /// Rejects empty, nonfinite or ragged data and invalid configuration.
    pub fn train(
        values: &[Vec<f64>],
        config: &FlowSomConfig,
        method: Metaclustering,
    ) -> Result<Self, FlowSomError> {
        with_thread_pool(config.threads, || {
            validate_config(config)?;
            validate_matrix(values, config.channels.len())?;
            let unique: HashSet<_> = config.channels.iter().collect();
            if unique.len() != config.channels.len()
                || config.channels.iter().any(|c| c.trim().is_empty())
            {
                return Err(invalid("empty or duplicate model channels"));
            }
            let channels = config
                .channels
                .iter()
                .map(|name| ResolvedChannel {
                    requested_name: name.clone(),
                    parameter_name: name.clone(),
                    display_name: name.clone(),
                    parameter_index: 0,
                })
                .collect::<Vec<_>>();
            let scaled = scale_matrix(values.to_vec(), &channels, config.scale);
            let rows = training_rows(values.len(), config);
            let training_sample = rows.map(|r| {
                r.into_iter()
                    .map(|i| scaled.values[i].clone())
                    .collect::<Vec<_>>()
            });
            let training = training_sample.as_deref().unwrap_or(&scaled.values);
            let codebook = train_som(training, SomGrid::of(config), config.seed);
            let node_metaclusters = match method {
                Metaclustering::Average => {
                    hierarchical_metaclusters(&codebook, config.metaclusters)
                }
                Metaclustering::Consensus => {
                    consensus_metaclusters(&codebook, config.metaclusters, config.seed)?
                }
            };
            let model = Self {
                schema_version: 1,
                channels: config.channels.clone(),
                channel_labels: None,
                xdim: config.xdim,
                ydim: config.ydim,
                seed: config.seed,
                scale_input: config.scale,
                scale: scaled.scale,
                codebook,
                node_metaclusters,
                metaclustering: method,
                training_events: training.len(),
                transform: None,
                normalization_model: None,
            };
            model.validate()?;
            Ok(model)
        })
    }

    /// Train on several files; masks in config are supported for one file.
    ///
    /// # Errors
    /// Rejects incompatible channel or signal contracts across acquisitions.
    pub fn train_files(
        inputs: &[PathBuf],
        config: &FlowSomConfig,
        method: Metaclustering,
    ) -> Result<Self, FlowSomError> {
        if inputs.is_empty() || (inputs.len() > 1 && !config.masks.is_empty()) {
            return Err(invalid(
                "provide inputs; per-acquisition masks require a single input",
            ));
        }
        let mut matrix = Vec::new();
        let mut resolved = None;
        let mut contract = None;
        let mut label_contract = None;
        for input in inputs {
            let dataset = load_dataset(input)?;
            let rows = selected_rows(input, &dataset, &config.masks)?;
            let channels = resolve_channels(input, &dataset, &config.channels, &rows)?;
            let names = channels
                .iter()
                .map(|c| c.parameter_name.clone())
                .collect::<Vec<_>>();
            let labels = names
                .iter()
                .map(|name| {
                    dataset
                        .parameters
                        .iter()
                        .find(|p| p.name == *name)
                        .and_then(|p| p.label.clone())
                        .into_iter()
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            if label_contract
                .as_ref()
                .is_some_and(|previous| previous != &labels)
            {
                return Err(invalid("training files have incompatible marker labels"));
            }
            label_contract = Some(labels);
            let state = SignalState::of(&dataset.text).map_err(|source| FlowSomError::FcsRead {
                path: input.clone(),
                source,
            })?;
            let current = (
                if state.transform.is_some() {
                    Some(
                        TransformContract::of(&dataset.text, &names).map_err(|source| {
                            FlowSomError::FcsRead {
                                path: input.clone(),
                                source,
                            }
                        })?,
                    )
                } else {
                    None
                },
                state.normalization.map(|n| n.model_sha256),
            );
            if resolved.as_ref().is_some_and(|n| n != &names)
                || contract.as_ref().is_some_and(|c| c != &current)
            {
                return Err(invalid(
                    "training files have incompatible channels or signal state",
                ));
            }
            resolved = Some(names);
            contract = Some(current);
            matrix.extend(channel_matrix(&channels, &dataset, &rows)?);
        }
        let mut config = config.clone();
        config.channels = resolved.ok_or_else(|| invalid("no training files"))?;
        let mut model = Self::train(&matrix, &config, method)?;
        model.channel_labels = label_contract;
        if let Some((transform, normalization)) = contract {
            model.transform = transform;
            model.normalization_model = normalization;
        }
        Ok(model)
    }

    /// Validate dimensions and finite parameters before any inference.
    ///
    /// # Errors
    /// Rejects unsupported or internally inconsistent portable models.
    pub fn validate(&self) -> Result<(), FlowSomError> {
        let nodes = self
            .xdim
            .checked_mul(self.ydim)
            .ok_or_else(|| invalid("SOM grid overflow"))?;
        let count = self.channels.len();
        if self.schema_version != 1
            || nodes == 0
            || count == 0
            || nodes != self.codebook.len()
            || self.node_metaclusters.len() != nodes
            || self.scale.len() != count
            || self.training_events == 0
            || self.channels.iter().any(|c| c.trim().is_empty())
            || self.channels.iter().collect::<HashSet<_>>().len() != count
            || self.channel_labels.as_ref().is_some_and(|labels| {
                labels.len() != count || labels.iter().flatten().any(|s| s.trim().is_empty())
            })
            || self.scale.iter().zip(&self.channels).any(|(s, c)| {
                s.channel != *c
                    || !s.mean.is_finite()
                    || !s.standard_deviation.is_finite()
                    || s.standard_deviation < 0.0
            })
            || self.node_metaclusters.iter().any(|c| *c == 0 || *c > nodes)
        {
            return Err(invalid("invalid SOM model dimensions, scale or labels"));
        }
        let max = self.node_metaclusters.iter().copied().max().unwrap_or(0);
        if self.node_metaclusters.iter().collect::<HashSet<_>>().len() != max {
            return Err(invalid("metacluster labels must be contiguous"));
        }
        validate_matrix(&self.codebook, count)
    }

    /// Assign (one-based node, one-based metacluster) without fitting new scale.
    ///
    /// # Errors
    /// Rejects an invalid model or incompatible matrix.
    pub fn assign(&self, values: &[Vec<f64>]) -> Result<Vec<(usize, usize)>, FlowSomError> {
        self.validate()?;
        validate_matrix(values, self.channels.len())?;
        let mut values = values.to_vec();
        if self.scale_input {
            for row in &mut values {
                for (value, scale) in row.iter_mut().zip(&self.scale) {
                    *value = if scale.standard_deviation <= EQUALITY_EPSILON {
                        0.0
                    } else {
                        (*value - scale.mean) / scale.standard_deviation
                    };
                }
            }
        }
        Ok(assign_events(&values, &self.codebook)
            .into_iter()
            .map(|n| (n + 1, self.node_metaclusters[n]))
            .collect())
    }

    /// Summarize population abundance and profiles in the original signal space.
    ///
    /// # Errors
    /// Returns assignment validation errors.
    pub fn populations(
        &self,
        values: &[Vec<f64>],
    ) -> Result<Vec<PopulationStatistics>, FlowSomError> {
        let assigned = self.assign(values)?;
        let k = self.node_metaclusters.iter().copied().max().unwrap_or(0);
        let mut stats = (1..=k)
            .map(|metacluster| PopulationStatistics {
                metacluster,
                events: 0,
                frequency: 0.0,
                marker_means: vec![0.0; self.channels.len()],
            })
            .collect::<Vec<_>>();
        for (row, (_, cluster)) in values.iter().zip(assigned) {
            let s = &mut stats[cluster - 1];
            s.events += 1;
            for (total, value) in s.marker_means.iter_mut().zip(row) {
                *total += value;
            }
        }
        for s in &mut stats {
            s.frequency = s.events as f64 / values.len() as f64;
            if s.events > 0 {
                for mean in &mut s.marker_means {
                    *mean /= s.events as f64;
                }
            }
        }
        Ok(stats)
    }

    /// Load and validate a portable model.
    ///
    /// # Errors
    /// Returns I/O, JSON or model validation errors.
    pub fn load(path: &Path) -> Result<Self, FlowSomError> {
        let model: Self = serde_json::from_slice(&cytiq_provenance::input::document_bytes(path)?)?;
        model.validate()?;
        Ok(model)
    }

    /// Save atomically, protecting existing outputs unless overwrite is set.
    ///
    /// # Errors
    /// Returns I/O or model validation errors.
    pub fn save(&self, path: &Path, overwrite: bool) -> Result<(), FlowSomError> {
        self.validate()?;
        if path.exists() && !overwrite {
            return Err(invalid("model exists; pass --overwrite"));
        }
        cytiq_publish::write_file(path, Existing::overwrite(overwrite), |writer| {
            serde_json::to_writer_pretty(writer, self).map_err(FlowSomError::from)
        })
    }

    /// Assign an FCS and write the existing assignment CSV schema.
    ///
    /// # Errors
    /// Rejects mismatched preprocessing or output collisions.
    pub fn apply_file(
        &self,
        input: &Path,
        output: &Path,
        masks: &[PathBuf],
        threads: Option<usize>,
        overwrite: bool,
    ) -> Result<usize, FlowSomError> {
        let sources = std::iter::once(input.to_path_buf())
            .chain(masks.iter().cloned())
            .collect::<Vec<_>>();
        check_file_outputs(output, &sources, overwrite)?;
        if input == output || (output.exists() && !overwrite) {
            return Err(invalid("assignment output exists or aliases input"));
        }
        with_thread_pool(threads, || {
            let dataset = load_dataset(input)?;
            let state = SignalState::of(&dataset.text).map_err(|source| FlowSomError::FcsRead {
                path: input.to_path_buf(),
                source,
            })?;
            let contract = if state.transform.is_some() {
                Some(
                    TransformContract::of(&dataset.text, &self.channels).map_err(|source| {
                        FlowSomError::FcsRead {
                            path: input.to_path_buf(),
                            source,
                        }
                    })?,
                )
            } else {
                None
            };
            if contract != self.transform
                || state.normalization.map(|n| n.model_sha256) != self.normalization_model
            {
                return Err(invalid("model signal contract mismatch"));
            }
            let rows = selected_rows(input, &dataset, masks)?;
            let channels = resolve_channels(input, &dataset, &self.channels, &rows)?;
            if let Some(expected) = &self.channel_labels {
                for (channel, labels) in self.channels.iter().zip(expected) {
                    let parameters = dataset
                        .parameters
                        .iter()
                        .filter(|p| p.name == *channel)
                        .collect::<Vec<_>>();
                    if parameters.len() != 1
                        || !match parameters[0].label.as_ref() {
                            Some(label) => labels.contains(label),
                            None => labels.is_empty(),
                        }
                    {
                        return Err(invalid("model marker label mismatch"));
                    }
                }
            }
            let assigned = self.assign(&channel_matrix(&channels, &dataset, &rows)?)?;
            let source =
                cytiq_fcs::source_event_ids(&dataset).map_err(|source| FlowSomError::FcsRead {
                    path: input.to_path_buf(),
                    source,
                })?;
            let assignments = assigned
                .iter()
                .zip(&rows)
                .map(|(&(node, cluster), &row)| FlowSomAssignment {
                    event_index: row,
                    source_event_id: source.as_ref().map_or(row as u64, |ids| ids[row]),
                    flowsom_node: node,
                    flowsom_metacluster: cluster,
                })
                .collect::<Vec<_>>();
            let mut publication = Publication::new(Existing::overwrite(overwrite));
            publication.stage_path(output, |path| write_assignments_csv(path, &assignments))?;
            publication.commit()?;
            Ok(assignments.len())
        })
    }
}

fn check_file_outputs(
    output: &Path,
    inputs: &[PathBuf],
    overwrite: bool,
) -> Result<(), FlowSomError> {
    let mut provenance = output.as_os_str().to_os_string();
    provenance.push(".provenance.json");
    for path in [output, Path::new(&provenance)] {
        if path.exists() && !overwrite {
            return Err(invalid("output exists; pass --overwrite"));
        }
        if let Ok(destination) = std::fs::canonicalize(path) {
            for input in inputs {
                if std::fs::canonicalize(input)? == destination {
                    return Err(invalid("output aliases a source file"));
                }
            }
        }
    }
    Ok(())
}

fn validate_matrix(values: &[Vec<f64>], columns: usize) -> Result<(), FlowSomError> {
    if columns == 0
        || values.is_empty()
        || values
            .iter()
            .any(|r| r.len() != columns || r.iter().any(|v| !v.is_finite()))
    {
        return Err(invalid("matrix must be nonempty, rectangular and finite"));
    }
    Ok(())
}

fn consensus_metaclusters(
    codebook: &[Vec<f64>],
    k: usize,
    seed: u64,
) -> Result<Vec<usize>, FlowSomError> {
    let n = codebook.len();
    if k == 1 {
        return Ok(vec![1; n]);
    }
    if k == n {
        return Ok((1..=n).collect());
    }
    let size = (n * 9 / 10).max(2);
    if k > size {
        return Err(invalid(
            "consensus metaclusters exceed the 90% node subsample",
        ));
    }
    let mut rng = DeterministicRng::new(seed ^ 0xa076_1d64_78bd_642f);
    let schedules = (0..100)
        .map(|_| {
            let mut ids = (0..n).collect::<Vec<_>>();
            for i in 0..size {
                let j = i + rng.next_index(n - i);
                ids.swap(i, j);
            }
            ids.truncate(size);
            ids.sort_unstable();
            ids
        })
        .collect::<Vec<_>>();
    Ok(consensus_from_schedules(codebook, k, &schedules))
}

fn consensus_from_schedules(
    codebook: &[Vec<f64>],
    k: usize,
    schedules: &[Vec<usize>],
) -> Vec<usize> {
    let n = codebook.len();
    let mut together = vec![vec![0_u32; n]; n];
    let mut seen = together.clone();
    for ids in schedules {
        let sample = ids.iter().map(|&i| codebook[i].clone()).collect::<Vec<_>>();
        let classes = hierarchical_metaclusters(&sample, k);
        for (a, &i) in ids.iter().enumerate() {
            for (b, &j) in ids.iter().enumerate() {
                seen[i][j] += 1;
                if classes[a] == classes[b] {
                    together[i][j] += 1;
                }
            }
        }
    }
    let distances = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| {
                    if i == j {
                        0.0
                    } else if seen[i][j] == 0 {
                        1.0
                    } else {
                        1.0 - f64::from(together[i][j]) / f64::from(seen[i][j])
                    }
                })
                .collect()
        })
        .collect::<Vec<Vec<f64>>>();
    cluster_distances(&distances, k)
}

fn cluster_distances(distances: &[Vec<f64>], k: usize) -> Vec<usize> {
    let mut clusters = (0..distances.len())
        .map(|i| Cluster { members: vec![i] })
        .collect::<Vec<_>>();
    while clusters.len() > k {
        let (a, b) = closest_cluster_pair(&clusters, distances);
        let other = clusters.remove(b);
        clusters[a].members.extend(other.members);
        clusters[a].members.sort_unstable();
    }
    clusters.sort_by_key(|c| c.members[0]);
    let mut labels = vec![0; distances.len()];
    for (c, cluster) in clusters.iter().enumerate() {
        for &i in &cluster.members {
            labels[i] = c + 1;
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consensus_matches_r_on_identical_codebook_and_resampling_schedules() {
        #[derive(Deserialize)]
        struct Consensus {
            codebook: Vec<Vec<f64>>,
            k: usize,
            schedules: Vec<Vec<usize>>,
            labels: Vec<usize>,
        }
        #[derive(Deserialize)]
        struct Fixture {
            consensus: Consensus,
        }
        let f: Fixture =
            serde_json::from_str(include_str!("../../../data/fixtures/cytonorm/numeric.json"))
                .unwrap();
        let f = f.consensus;
        assert_eq!(
            consensus_from_schedules(&f.codebook, f.k, &f.schedules),
            f.labels
        );
    }
    #[test]
    fn saved_model_assigns_without_rescaling_new_samples() {
        let mut config = FlowSomConfig::new(vec!["A".into()]);
        config.xdim = 2;
        config.ydim = 1;
        config.metaclusters = 2;
        let values = vec![vec![0.0], vec![0.1], vec![10.0], vec![10.1]];
        let model = FlowSomModel::train(&values, &config, Metaclustering::Average).unwrap();
        let path = tempfile::NamedTempFile::new().unwrap();
        model.save(path.path(), true).unwrap();
        let loaded = FlowSomModel::load(path.path()).unwrap();
        assert_eq!(model, loaded);
        assert_eq!(
            loaded.assign(&values).unwrap(),
            model.assign(&values).unwrap()
        );
        assert_eq!(
            loaded.assign(&[vec![10.0]]).unwrap()[0],
            model.assign(&values).unwrap()[2]
        );
    }
    #[test]
    fn consensus_recovers_separated_populations_deterministically() {
        let values = (0..20)
            .map(|i| {
                vec![if i < 10 {
                    f64::from(i) * 0.01
                } else {
                    100.0 + f64::from(i) * 0.01
                }]
            })
            .collect::<Vec<_>>();
        let labels = consensus_metaclusters(&values, 2, 1).unwrap();
        assert_eq!(labels, consensus_metaclusters(&values, 2, 1).unwrap());
        assert_eq!(
            labels,
            (0..20)
                .map(|i| if i < 10 { 1 } else { 2 })
                .collect::<Vec<_>>()
        );
    }
}
