use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{Array, ArrayRef, Float64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use cytiq_fcs::TransformContract;
use cytiq_flowsom::FlowSomModel;
use cytiq_mask::parquet::{ParquetReadLimits, ScalarParquetReader};
use parquet::arrow::ArrowWriter;
use serde::{Deserialize, Serialize};

use crate::{Mode, MonotoneSpline, Panel, Result, SomSettings, SparsePolicy, Target, invalid};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    pub metacluster: usize,
    pub marker: String,
    pub quantiles: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDocument {
    pub schema_version: u32,
    pub algorithm: String,
    pub mode: Mode,
    pub target: Target,
    pub batches: BTreeMap<String, Option<String>>,
    pub normalization_markers: Vec<String>,
    pub probabilities: Vec<f64>,
    pub sparse_policy: SparsePolicy,
    pub min_cluster_events: usize,
    pub som: SomSettings,
    pub training_events: usize,
    pub training_inputs: BTreeMap<String, Vec<String>>,
    pub files: BTreeMap<String, String>,
    pub goals: Vec<Goal>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkerContract {
    pub panel: Panel,
    pub transform: TransformContract,
}

#[derive(Clone, Debug)]
pub struct Mapping {
    pub batch: String,
    pub metacluster: usize,
    pub marker: String,
    pub events: usize,
    pub spline: Option<MonotoneSpline>,
    pub identity_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CytoNormModel {
    pub document: ModelDocument,
    pub markers: MarkerContract,
    pub som: FlowSomModel,
    pub mappings: Vec<Mapping>,
    pub model_sha256: String,
    pub panel_sha256: String,
}

impl CytoNormModel {
    /// Verify payload digests and all cross-artifact dimensions before use.
    ///
    /// # Errors
    /// Rejects missing, corrupted, incompatible or incomplete bundles.
    pub fn load(directory: &Path) -> Result<Self> {
        let document: ModelDocument = serde_json::from_slice(
            &cytiq_provenance::input::document_bytes(&directory.join("model.json"))?,
        )?;
        if document.schema_version != 1
            || document.algorithm != "cytonorm-2.0"
            || document
                .files
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != BTreeSet::from(["markers.json", "som.json", "splines.parquet"])
        {
            return Err(invalid("unsupported CytoNorm model schema or payload set"));
        }
        for (file, hash) in &document.files {
            if !cytiq_fcs::is_sha256(hash)
                || cytiq_provenance::hash_file(&directory.join(file))?.1 != *hash
            {
                return Err(invalid(format!("model checksum mismatch: {file}")));
            }
        }
        let markers: MarkerContract = serde_json::from_slice(
            &cytiq_provenance::input::document_bytes(&directory.join("markers.json"))?,
        )?;
        let som = FlowSomModel::load(&directory.join("som.json"))?;
        let mappings = read_mappings(&directory.join("splines.parquet"))?;
        let model = Self {
            document,
            markers,
            som,
            mappings,
            model_sha256: cytiq_provenance::hash_file(&directory.join("model.json"))?.1,
            panel_sha256: cytiq_provenance::hash_file(&directory.join("markers.json"))?.1,
        };
        model.validate()?;
        Ok(model)
    }

    /// Validate a programmatically assembled model.
    ///
    /// # Errors
    /// Rejects inconsistent panels, goals and batch/cluster/marker coverage.
    pub fn validate(&self) -> Result<()> {
        self.som.validate()?;
        self.validate_panel()?;
        self.validate_som_contract()?;
        self.validate_training_contract()?;
        self.validate_channel_labels()?;
        let k = self
            .som
            .node_metaclusters
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        self.validate_goals(k)?;
        self.validate_mappings(k)
    }

    fn validate_panel(&self) -> Result<()> {
        let d = &self.document;
        let panel = &self.markers.panel;
        let names = panel
            .markers
            .iter()
            .map(|m| m.marker.as_str())
            .collect::<BTreeSet<_>>();
        let channels = panel
            .markers
            .iter()
            .map(|m| m.channel.as_str())
            .collect::<BTreeSet<_>>();
        if d.schema_version != 1
            || d.algorithm != "cytonorm-2.0"
            || panel.id.trim().is_empty()
            || panel.markers.is_empty()
            || names.len() != panel.markers.len()
            || channels.len() != panel.markers.len()
            || panel
                .markers
                .iter()
                .any(|m| m.marker.is_empty() || m.channel.is_empty())
            || d.normalization_markers.is_empty()
            || d.normalization_markers
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != d.normalization_markers.len()
            || d.normalization_markers
                .iter()
                .any(|m| !names.contains(m.as_str()))
            || self
                .som
                .channels
                .iter()
                .any(|c| !channels.contains(c.as_str()))
            || d.normalization_markers.iter().any(|name| {
                panel.markers.iter().any(|m| {
                    m.marker == *name && {
                        let channel = m.channel.to_ascii_uppercase();
                        channel == "TIME"
                            || channel == "ORIGINAL_ID"
                            || channel.starts_with("FSC-")
                            || channel.starts_with("SSC-")
                    }
                })
            })
        {
            return Err(invalid(
                "inconsistent model panel, SOM, batch or quantile contract",
            ));
        }
        Ok(())
    }

    fn validate_som_contract(&self) -> Result<()> {
        let d = &self.document;
        let k = self
            .som
            .node_metaclusters
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        if self.som.transform.as_ref() != Some(&self.markers.transform)
            || self.som.normalization_model.is_some()
            || d.som.xdim != self.som.xdim
            || d.som.ydim != self.som.ydim
            || d.som.metaclusters != k
            || d.som.seed != self.som.seed
            || d.som.scale != self.som.scale_input
            || d.training_events != self.som.training_events
            || self.som.metaclustering != cytiq_flowsom::Metaclustering::Consensus
        {
            return Err(invalid(
                "inconsistent model panel, SOM, batch or quantile contract",
            ));
        }
        Ok(())
    }

    fn validate_training_contract(&self) -> Result<()> {
        let d = &self.document;
        if d.batches.len() < 2
            || d.batches.keys().any(|s| s.trim().is_empty())
            || d.min_cluster_events < 2
            || d.training_events == 0
            || d.probabilities.len() < 2
            || d.probabilities
                .iter()
                .any(|p| !p.is_finite() || *p < 0.0 || *p > 1.0)
            || d.probabilities.windows(2).any(|p| p[0] >= p[1])
            || matches!(&d.target,Target::Batch{batch} if !d.batches.contains_key(batch))
        {
            return Err(invalid(
                "inconsistent model panel, SOM, batch or quantile contract",
            ));
        }
        Ok(())
    }

    fn validate_channel_labels(&self) -> Result<()> {
        let panel = &self.markers.panel;
        if let Some(labels) = &self.som.channel_labels {
            for (channel, labels) in self.som.channels.iter().zip(labels) {
                let marker = panel
                    .markers
                    .iter()
                    .find(|m| m.channel == *channel)
                    .ok_or_else(|| invalid("missing SOM panel marker"))?;
                let expected = if marker.labels.is_empty() {
                    vec![marker.marker.clone()]
                } else {
                    marker.labels.clone()
                };
                if *labels != expected {
                    return Err(invalid("SOM reagent labels differ from the panel contract"));
                }
            }
        }
        Ok(())
    }

    fn validate_goals(&self, k: usize) -> Result<()> {
        let d = &self.document;
        let mut goals = BTreeSet::new();
        for g in &d.goals {
            if g.metacluster == 0
                || g.metacluster > k
                || !d.normalization_markers.contains(&g.marker)
                || !goals.insert((g.metacluster, g.marker.as_str()))
                || g.quantiles.as_ref().is_some_and(|q| {
                    q.len() != d.probabilities.len()
                        || q.iter().any(|v| !v.is_finite())
                        || q.windows(2).any(|v| v[0] > v[1])
                })
            {
                return Err(invalid("invalid or duplicate goal quantiles"));
            }
        }
        if goals.len() != k * d.normalization_markers.len() {
            return Err(invalid("missing goal distribution"));
        }
        Ok(())
    }

    fn validate_mappings(&self, k: usize) -> Result<()> {
        let d = &self.document;
        let mut seen = BTreeSet::new();
        for m in &self.mappings {
            if !d.batches.contains_key(&m.batch)
                || m.metacluster == 0
                || m.metacluster > k
                || !d.normalization_markers.contains(&m.marker)
                || !seen.insert((m.batch.as_str(), m.metacluster, m.marker.as_str()))
            {
                return Err(invalid("invalid or duplicate normalization mapping"));
            }
            match (&m.spline, &m.identity_reason) {
                (Some(s), None) => {
                    if m.events < d.min_cluster_events
                        || self.goal(m.metacluster, &m.marker).is_none()
                    {
                        return Err(invalid("spline without sufficient events or target"));
                    }
                    s.validate()?;
                }
                (None, Some(reason))
                    if d.sparse_policy == SparsePolicy::Identity && !reason.is_empty() => {}
                _ => return Err(invalid("invalid identity mapping policy")),
            }
        }
        if seen.len() != d.batches.len() * k * d.normalization_markers.len() {
            return Err(invalid("missing batch/cluster/marker mapping"));
        }
        Ok(())
    }

    pub(crate) fn goal(&self, cluster: usize, marker: &str) -> Option<&[f64]> {
        self.document
            .goals
            .iter()
            .find(|g| g.metacluster == cluster && g.marker == marker)
            .and_then(|g| g.quantiles.as_deref())
    }

    /// Payload files read by apply, for pre-run provenance snapshots.
    #[must_use]
    pub fn input_files(directory: &Path) -> Vec<PathBuf> {
        ["model.json", "som.json", "markers.json", "splines.parquet"]
            .map(|p| directory.join(p))
            .to_vec()
    }
}

fn mapping_schema() -> Schema {
    Schema::new(vec![
        Field::new("batch", DataType::Utf8, false),
        Field::new("metacluster", DataType::UInt64, false),
        Field::new("marker", DataType::Utf8, false),
        Field::new("knot", DataType::UInt64, false),
        Field::new("x", DataType::Float64, false),
        Field::new("y", DataType::Float64, false),
        Field::new("slope", DataType::Float64, false),
        Field::new("events", DataType::UInt64, false),
        Field::new("identity_reason", DataType::Utf8, true),
    ])
}

pub(crate) fn write_mappings(path: &Path, mappings: &[Mapping]) -> Result<()> {
    let mut batch = Vec::new();
    let mut clusters = Vec::new();
    let mut marker = Vec::new();
    let mut knot = Vec::new();
    let mut x = Vec::new();
    let mut y = Vec::new();
    let mut slopes = Vec::new();
    let mut events = Vec::new();
    let mut reasons = Vec::new();
    for m in mappings {
        let n = m.spline.as_ref().map_or(1, |s| s.x.len());
        for i in 0..n {
            batch.push(m.batch.as_str());
            clusters.push(m.metacluster as u64);
            marker.push(m.marker.as_str());
            knot.push(i as u64);
            events.push(m.events as u64);
            reasons.push(m.identity_reason.as_deref());
            x.push(m.spline.as_ref().map_or(0.0, |s| s.x[i]));
            y.push(m.spline.as_ref().map_or(0.0, |s| s.y[i]));
            slopes.push(m.spline.as_ref().map_or(0.0, |s| s.slopes[i]));
        }
    }
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(batch)),
        Arc::new(UInt64Array::from(clusters)),
        Arc::new(StringArray::from(marker)),
        Arc::new(UInt64Array::from(knot)),
        Arc::new(Float64Array::from(x)),
        Arc::new(Float64Array::from(y)),
        Arc::new(Float64Array::from(slopes)),
        Arc::new(UInt64Array::from(events)),
        Arc::new(StringArray::from(reasons)),
    ];
    let schema = Arc::new(mapping_schema());
    let record = RecordBatch::try_new(schema.clone(), arrays)?;
    let mut writer = ArrowWriter::try_new(File::create(path)?, schema, None)?;
    writer.write(&record)?;
    writer.close()?;
    Ok(())
}

fn read_mappings(path: &Path) -> Result<Vec<Mapping>> {
    type Key = (String, usize, String);
    type Row = (usize, f64, f64, f64, usize, Option<String>);
    let reader = ScalarParquetReader::new(File::open(path)?, ParquetReadLimits::default())?;
    if reader.schema().as_ref() != &mapping_schema() {
        return Err(invalid("unsupported spline Parquet schema"));
    }
    let mut grouped: BTreeMap<Key, Vec<Row>> = BTreeMap::new();
    for record in reader {
        let record = record?;
        macro_rules! col {
            ($i:expr,$ty:ty) => {
                record
                    .column($i)
                    .as_any()
                    .downcast_ref::<$ty>()
                    .ok_or_else(|| invalid("invalid spline column type"))?
            };
        }
        let batch = col!(0, StringArray);
        let cluster = col!(1, UInt64Array);
        let marker = col!(2, StringArray);
        let knot = col!(3, UInt64Array);
        let x = col!(4, Float64Array);
        let y = col!(5, Float64Array);
        let slope = col!(6, Float64Array);
        let events = col!(7, UInt64Array);
        let reason = col!(8, StringArray);
        if record.columns()[..8].iter().any(|a| a.null_count() != 0) {
            return Err(invalid("null spline coefficient"));
        }
        for i in 0..record.num_rows() {
            let cluster =
                usize::try_from(cluster.value(i)).map_err(|_| invalid("cluster index overflow"))?;
            let knot =
                usize::try_from(knot.value(i)).map_err(|_| invalid("knot index overflow"))?;
            let events =
                usize::try_from(events.value(i)).map_err(|_| invalid("event count overflow"))?;
            grouped
                .entry((batch.value(i).into(), cluster, marker.value(i).into()))
                .or_default()
                .push((
                    knot,
                    x.value(i),
                    y.value(i),
                    slope.value(i),
                    events,
                    (!reason.is_null(i)).then(|| reason.value(i).to_owned()),
                ));
        }
    }
    let mut mappings = Vec::new();
    for ((batch, metacluster, marker), mut rows) in grouped {
        rows.sort_by_key(|r| r.0);
        let events = rows[0].4;
        let identity_reason = rows[0].5.clone();
        if rows
            .iter()
            .enumerate()
            .any(|(i, r)| r.0 != i || r.4 != events || r.5 != identity_reason)
        {
            return Err(invalid("inconsistent or duplicate spline knots"));
        }
        let spline = if identity_reason.is_some() {
            if rows.len() != 1 || rows[0].1 != 0.0 || rows[0].2 != 0.0 || rows[0].3 != 0.0 {
                return Err(invalid("invalid identity spline row"));
            }
            None
        } else {
            Some(MonotoneSpline {
                x: rows.iter().map(|r| r.1).collect(),
                y: rows.iter().map(|r| r.2).collect(),
                slopes: rows.iter().map(|r| r.3).collect(),
            })
        };
        mappings.push(Mapping {
            batch,
            metacluster,
            marker,
            events,
            spline,
            identity_reason,
        });
    }
    Ok(mappings)
}
