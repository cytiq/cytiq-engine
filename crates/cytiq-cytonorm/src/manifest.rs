use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{DEFAULT_MIN_CLUSTER_EVENTS, DEFAULT_TRAINING_EVENTS, Result, invalid};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Controls,
    Aggregate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    #[default]
    Train,
    Validation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SparsePolicy {
    #[default]
    Error,
    Identity,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    #[default]
    Mean,
    Batch {
        batch: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub marker: String,
    pub channel: String,
    /// When omitted, the marker itself must match `$PnS`.
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub id: String,
    pub markers: Vec<Marker>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub id: String,
    pub batch: String,
    pub file: PathBuf,
    #[serde(default)]
    pub masks: Vec<PathBuf>,
    #[serde(default)]
    pub role: Role,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SomSettings {
    pub xdim: usize,
    pub ydim: usize,
    pub metaclusters: usize,
    pub scale: bool,
    pub seed: u64,
}
impl Default for SomSettings {
    fn default() -> Self {
        Self {
            xdim: 15,
            ydim: 15,
            metaclusters: 10,
            scale: false,
            seed: 1,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub panel: Panel,
    pub mode: Mode,
    pub clustering_markers: Vec<String>,
    pub normalization_markers: Vec<String>,
    pub samples: Vec<Sample>,
    #[serde(default)]
    pub target: Target,
    #[serde(default)]
    pub som: SomSettings,
    #[serde(default = "default_events")]
    pub training_events: usize,
    #[serde(default = "default_min")]
    pub min_cluster_events: usize,
    #[serde(default = "default_probabilities")]
    pub probabilities: Vec<f64>,
    #[serde(default)]
    pub sparse_policy: SparsePolicy,
}
fn default_events() -> usize {
    DEFAULT_TRAINING_EVENTS
}
fn default_min() -> usize {
    DEFAULT_MIN_CLUSTER_EVENTS
}
pub(crate) fn default_probabilities() -> Vec<f64> {
    (1..100).map(|i| f64::from(i) / 100.0).collect()
}

impl Manifest {
    /// Load a strict YAML/JSON document and resolve paths relative to it.
    ///
    /// # Errors
    /// Returns parsing, path or experimental-design validation errors.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = cytiq_provenance::input::document_bytes(path)?;
        let text = std::str::from_utf8(&bytes).map_err(|e| invalid(e.to_string()))?;
        let mut m: Self = match path.extension().and_then(|s| s.to_str()) {
            Some("json") => serde_json::from_str(text)?,
            Some("yaml" | "yml") => {
                serde_saphyr::from_str(text).map_err(|e| invalid(e.to_string()))?
            }
            _ => return Err(invalid("manifest must be .yaml, .yml or .json")),
        };
        m.validate()?;
        let parent = path.parent().unwrap_or(Path::new("."));
        for sample in &mut m.samples {
            if sample.file.is_relative() {
                sample.file = parent.join(&sample.file);
            }
            for mask in &mut sample.masks {
                if mask.is_relative() {
                    *mask = parent.join(&mask);
                }
            }
        }
        m.samples
            .sort_by(|a, b| (&a.batch, &a.id).cmp(&(&b.batch, &b.id)));
        Ok(m)
    }

    /// Validate schema, panel, quantile grid and batch membership.
    ///
    /// # Errors
    /// Rejects ambiguous markers, empty cohorts and invalid numeric settings.
    pub fn validate(&self) -> Result<()> {
        let unique = |v: &[String]| {
            !v.is_empty()
                && v.iter().all(|s| !s.trim().is_empty())
                && v.iter().collect::<HashSet<_>>().len() == v.len()
        };
        let names = self
            .panel
            .markers
            .iter()
            .map(|m| m.marker.clone())
            .collect::<Vec<_>>();
        let channels = self
            .panel
            .markers
            .iter()
            .map(|m| m.channel.clone())
            .collect::<Vec<_>>();
        if self.schema_version != 1
            || self.panel.id.trim().is_empty()
            || !unique(&names)
            || !unique(&channels)
            || !unique(&self.clustering_markers)
            || !unique(&self.normalization_markers)
            || self
                .clustering_markers
                .iter()
                .chain(&self.normalization_markers)
                .any(|m| !names.contains(m))
        {
            return Err(invalid("invalid schema, panel or marker lists"));
        }
        if self.normalization_markers.iter().any(|name| {
            let channel =
                &self.panel.markers[names.iter().position(|n| n == name).unwrap_or(0)].channel;
            let c = channel.to_ascii_uppercase();
            c == "TIME" || c == "ORIGINAL_ID" || c.starts_with("FSC-") || c.starts_with("SSC-")
        }) {
            return Err(invalid(
                "acquisition and lineage channels cannot be normalized",
            ));
        }
        let nodes = self
            .som
            .xdim
            .checked_mul(self.som.ydim)
            .ok_or_else(|| invalid("SOM grid overflow"))?;
        if self.som.xdim == 0
            || self.som.ydim == 0
            || self.som.metaclusters == 0
            || self.som.metaclusters > nodes
            || self.training_events == 0
            || self.min_cluster_events < 2
            || self.probabilities.len() < 2
            || self
                .probabilities
                .iter()
                .any(|p| !p.is_finite() || *p < 0.0 || *p > 1.0)
            || self.probabilities.windows(2).any(|p| p[0] >= p[1])
        {
            return Err(invalid("invalid SOM, sampling or quantile settings"));
        }
        let mut ids = HashSet::new();
        for s in &self.samples {
            if s.id.trim().is_empty()
                || s.batch.trim().is_empty()
                || s.file.as_os_str().is_empty()
                || !ids.insert(&s.id)
                || s.masks.iter().any(|p| p.as_os_str().is_empty())
            {
                return Err(invalid(
                    "sample ids must be unique; sample, batch and paths must be nonempty",
                ));
            }
        }
        let batches = self
            .samples
            .iter()
            .filter(|s| s.role == Role::Train)
            .map(|s| s.batch.as_str())
            .collect::<BTreeSet<_>>();
        if batches.len() < 2
            || self
                .samples
                .iter()
                .any(|s| !batches.contains(s.batch.as_str()))
            || matches!(&self.target,Target::Batch{batch} if !batches.contains(batch.as_str()))
        {
            return Err(invalid(
                "need at least two training batches; validation and reference batches must be trained",
            ));
        }
        Ok(())
    }
}
