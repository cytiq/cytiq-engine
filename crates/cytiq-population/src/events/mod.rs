//! `cytiq.events.v1`: one Parquet table per acquisition and signal layer.
//!
//! Each row is one event, identified by `acquisition_id` (the SHA-256
//! acquisition identity, `CYTIQ_ACQUISITION`) and `source_event_id` (its
//! 0-based row in the raw file). `layer_id` identifies the signal layer: the
//! acquisition, the processing steps that produced the stored values and the
//! physical channel order. Events of different acquisitions are never equal,
//! whatever their row: every key column is stored in every row, so tables can
//! be concatenated without losing identity.
//!
//! Columns, in order: `acquisition_id` (Utf8), `layer_id` (Utf8),
//! `source_event_id` (`UInt64`, strictly increasing), then one column per FCS
//! channel, named `$PnN`, in `$Pn` order, typed as the FCS stores it
//! (`UInt64`, `Float32` or `Float64`), bit-exact. Nothing is nullable.
//! Technical masks and biological annotations keep their own formats
//! (`cytiq-mask` v2, gate membership, classifier predictions) and join on
//! (`acquisition_id`, `source_event_id`).
//!
//! A table may store a projection of the layer's channels
//! (`column_set=subset`), and a layer may be split into part tables listed
//! by a [`PartsManifest`] (`cytiq.events.parts.v1`).

mod channels;
mod descriptor;
mod parts;
mod read;
mod runner;
mod settings;
mod write;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::serialization;

pub use channels::ChannelEntry;
pub use descriptor::{BaseState, Definition, ExportOptions, LayerBasis};
pub(crate) use parts::verify_parts_in;
pub use parts::{
    PARTS_CONTRACT, PARTS_FILE, PARTS_MAX, PartEntry, PartsManifest, RowSet, part_name,
    verify_parts, verify_parts_with_limits,
};
pub use read::{EventsSummary, verify, verify_file, verify_with_limits};
pub use runner::{ExportConfig, ExportRunner};
pub use settings::{
    BATCH_BYTES_MAX, Codec, DEFAULT_BATCH_BYTES, DEFAULT_ROW_GROUP_ROWS, DEFAULT_ZSTD_LEVEL,
    ROW_GROUP_BYTES_MAX, WriteSettings,
};
pub(crate) use settings::{TableShape, check_table};
pub use write::{ExportSummary, export, export_with};

/// Contract identifier of an events table.
pub const CONTRACT: &str = "cytiq.events.v1";
/// Contract identifier of the layer descriptor.
pub const LAYER_CONTRACT: &str = "cytiq.events.layer.v1";
/// Contract identifier of the comparability recipe.
pub const RECIPE_CONTRACT: &str = "cytiq.events.recipe.v1";
pub const SCHEMA_VERSION: u32 = 1;

pub const SCHEMA_VERSION_KEY: &str = "cytiq.events.schema_version";
/// Canonical JSON of the [`LayerDescriptor`]; authoritative over field
/// metadata, which engines that rewrite files without Arrow drop.
pub const LAYER_KEY: &str = "cytiq.events.layer";
pub const LAYER_ID_KEY: &str = "cytiq.events.layer_id";
/// `all` when the rows are every raw event in order, `subset` otherwise.
pub const ROW_SET_KEY: &str = "cytiq.events.row_set";
/// Raw event count, present when `row_set` is `all`.
pub const RAW_EVENTS_KEY: &str = "cytiq.events.raw_events";
/// Encoding-independent digest of keys and values (see [`ValuesHasher`]).
pub const VALUES_SHA256_KEY: &str = "cytiq.events.values_sha256";
/// JSON channel dictionary of the stored channels (see [`ChannelEntry`]):
/// what Arrow field metadata holds, readable by engines that drop it.
pub const CHANNELS_KEY: &str = "cytiq.events.channels";
/// `all` when every channel of the layer is stored, `subset` otherwise.
pub const COLUMN_SET_KEY: &str = "cytiq.events.column_set";
/// SHA-256 of the FCS file the table was exported from.
pub const SOURCE_SHA256_KEY: &str = "cytiq.events.source_sha256";
pub use cytiq_mask::{ACQUISITION_KEY, EVENT_ID_BASE_KEY};

pub const ACQUISITION_COLUMN: &str = "acquisition_id";
pub const LAYER_COLUMN: &str = "layer_id";
pub use cytiq_mask::SOURCE_EVENT_ID_COLUMN;

/// Names no channel may take, compared ignoring case; names starting with
/// `cytiq.` or `__` are reserved as well.
pub const RESERVED_NAMES: &[&str] = &[
    ACQUISITION_COLUMN,
    LAYER_COLUMN,
    SOURCE_EVENT_ID_COLUMN,
    "event_id",
    "keep",
    "original_id",
];
/// Channels per table: the bounded Parquet reader accepts 1024 columns, three
/// of which are keys.
pub const MAX_CHANNELS: usize = 1021;
/// Prefix of the per-channel Arrow field metadata keys.
pub const FIELD_KEY_PREFIX: &str = "cytiq.channel.";
/// Domain tag that starts the values digest.
pub const VALUES_DOMAIN: &str = "cytiq.events.values.v1";
/// Identifies the writer in the Parquet footer.
pub const CREATED_BY: &str = "cytiq events.v1";

/// What a layer's stored values are: the last processing step applied.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LayerState {
    #[default]
    Raw,
    Compensated,
    Unmixed,
    Transformed,
    Normalized,
}

impl LayerState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Compensated => "compensated",
            Self::Unmixed => "unmixed",
            Self::Transformed => "transformed",
            Self::Normalized => "normalized",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Compensation,
    Unmixing,
    Transform,
    Normalization,
}

impl StepKind {
    /// Position in the pipeline: compensation or unmixing, then transform,
    /// then normalization.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Compensation | Self::Unmixing => 0,
            Self::Transform => 1,
            Self::Normalization => 2,
        }
    }

    #[must_use]
    pub const fn state(self) -> LayerState {
        match self {
            Self::Compensation => LayerState::Compensated,
            Self::Unmixing => LayerState::Unmixed,
            Self::Transform => LayerState::Transformed,
            Self::Normalization => LayerState::Normalized,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compensation => "compensation",
            Self::Unmixing => "unmixing",
            Self::Transform => "transform",
            Self::Normalization => "normalization",
        }
    }
}

/// Where a step's definition and parameters come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Read from the file's own keywords, written by the cytiq stage that
    /// applied the step.
    Recorded,
    /// Supplied by the exporter and matched against the provenance record of
    /// the run that applied the step.
    Verified,
    /// Supplied by the exporter and not verifiable from the file.
    Declared,
    /// Applied by other software; method and definition are unknown.
    External,
}

/// One processing step, in pipeline order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub kind: StepKind,
    pub method: String,
    pub origin: Origin,
    /// Digest of the spillover matrix, unmixing reference or model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_sha256: Option<String>,
    /// Path-free settings, for example a transform's cofactor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    /// Channels the step changed; empty for an external step, whose extent
    /// is unknown.
    pub channels: Vec<String>,
}

/// Physical type of a channel column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Uint64,
    Float32,
    Float64,
}

impl ValueType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uint64 => "uint64",
            Self::Float32 => "float32",
            Self::Float64 => "float64",
        }
    }

    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::Float32 => 4,
            Self::Uint64 | Self::Float64 => 8,
        }
    }

    #[must_use]
    pub fn arrow(self) -> arrow_schema::DataType {
        match self {
            Self::Uint64 => arrow_schema::DataType::UInt64,
            Self::Float32 => arrow_schema::DataType::Float32,
            Self::Float64 => arrow_schema::DataType::Float64,
        }
    }

    #[must_use]
    pub fn of_arrow(data_type: &arrow_schema::DataType) -> Option<Self> {
        match data_type {
            arrow_schema::DataType::UInt64 => Some(Self::Uint64),
            arrow_schema::DataType::Float32 => Some(Self::Float32),
            arrow_schema::DataType::Float64 => Some(Self::Float64),
            _ => None,
        }
    }
}

/// One channel column: its `$Pn` index, `$PnN` name and stored type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSpec {
    pub index: u16,
    pub name: String,
    pub value_type: ValueType,
}

/// What a layer's values are and where they come from. `layer_id` is its
/// canonical digest, so it changes with the acquisition, any step, any
/// definition or parameter, and the channel order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerDescriptor {
    pub contract: String,
    pub acquisition_id: String,
    pub state: LayerState,
    pub steps: Vec<Step>,
    pub channels: Vec<ChannelSpec>,
}

impl LayerDescriptor {
    /// The canonical JSON digest identifying this layer.
    ///
    /// # Panics
    /// Never: the descriptor holds only strings, integers and finite numbers.
    #[must_use]
    pub fn layer_id(&self) -> String {
        serialization::digest(&serde_json::to_value(self).expect("descriptor serializes"))
    }

    /// Comparability key of a lakehouse table: the layer without its
    /// acquisition, step origins, per-file compensation or unmixing
    /// definitions and normalization batch. Layers of one panel that may be
    /// stacked in one table share it.
    #[must_use]
    pub fn recipe_id(&self) -> String {
        let steps: Vec<serde_json::Value> = self
            .steps
            .iter()
            .map(|step| {
                let parameters = match step.kind {
                    StepKind::Transform => step.parameters.clone(),
                    StepKind::Normalization => Some(json!({
                        "model_sha256": step.definition_sha256,
                        "panel_sha256": step
                            .parameters
                            .as_ref()
                            .and_then(|p| p.get("panel_sha256"))
                            .cloned(),
                    })),
                    StepKind::Compensation | StepKind::Unmixing => None,
                };
                json!({
                    "kind": step.kind,
                    "method": step.method,
                    "external": step.origin == Origin::External,
                    "parameters": parameters,
                    "channels": step.channels,
                })
            })
            .collect();
        serialization::digest(&json!({
            "contract": RECIPE_CONTRACT,
            "state": self.state,
            "steps": steps,
            "channels": self.channels,
        }))
    }

    /// The step kinds applied to `channel`, in pipeline order; external
    /// steps of unknown extent are not listed.
    #[must_use]
    pub fn steps_of(&self, channel: &str) -> Vec<StepKind> {
        self.steps
            .iter()
            .filter(|step| step.channels.iter().any(|name| name == channel))
            .map(|step| step.kind)
            .collect()
    }

    /// Whether `self`'s steps are a strict prefix of `child`'s, on the same
    /// acquisition: `child` can derive from `self`.
    #[must_use]
    pub fn is_parent_of(&self, child: &Self) -> bool {
        self.acquisition_id == child.acquisition_id
            && self.steps.len() < child.steps.len()
            && child.steps.starts_with(&self.steps)
    }
}

/// SHA-256 over [`VALUES_DOMAIN`], the layer id and every row's
/// `source_event_id` and channel values as little-endian bit patterns, in
/// row order. It does not depend on the Parquet encoding or batching.
pub struct ValuesHasher {
    hasher: Sha256,
    /// Row-major bytes of up to [`HASH_CHUNK_BYTES`] of rows.
    chunk: Vec<u8>,
}

/// Bytes of rows laid out at a time before they are hashed.
const HASH_CHUNK_BYTES: usize = 64 << 10;

/// Write `values` at `offset` of each `stride`-byte row of `chunk`.
fn scatter<const N: usize, T: Copy>(
    chunk: &mut [u8],
    offset: usize,
    stride: usize,
    values: &[T],
    bytes: impl Fn(T) -> [u8; N],
) {
    for (row, value) in chunk.chunks_exact_mut(stride).zip(values) {
        row[offset..offset + N].copy_from_slice(&bytes(*value));
    }
}

impl ValuesHasher {
    #[must_use]
    pub fn new(layer_id: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(VALUES_DOMAIN.as_bytes());
        hasher.update(b"\n");
        hasher.update(layer_id.as_bytes());
        hasher.update(b"\n");
        Self {
            hasher,
            chunk: Vec::with_capacity(HASH_CHUNK_BYTES),
        }
    }

    /// Hash one batch: `ids[i]` and `columns[c][i]` form row `i`. Rows are
    /// laid out a chunk at a time, column by column.
    pub fn update(&mut self, ids: &[u64], columns: &[ColumnValues<'_>]) {
        let stride = 8 + columns.iter().map(ColumnValues::width).sum::<usize>();
        let rows_per_chunk = (HASH_CHUNK_BYTES / stride).max(1);
        let mut start = 0;
        while start < ids.len() {
            let end = (start + rows_per_chunk).min(ids.len());
            self.chunk.clear();
            self.chunk.resize((end - start) * stride, 0);
            scatter(
                &mut self.chunk,
                0,
                stride,
                &ids[start..end],
                u64::to_le_bytes,
            );
            let mut offset = 8;
            for column in columns {
                match column {
                    ColumnValues::Uint64(values) => scatter(
                        &mut self.chunk,
                        offset,
                        stride,
                        &values[start..end],
                        u64::to_le_bytes,
                    ),
                    ColumnValues::Float32(values) => {
                        scatter(&mut self.chunk, offset, stride, &values[start..end], |v| {
                            v.to_bits().to_le_bytes()
                        });
                    }
                    ColumnValues::Float64(values) => {
                        scatter(&mut self.chunk, offset, stride, &values[start..end], |v| {
                            v.to_bits().to_le_bytes()
                        });
                    }
                }
                offset += column.width();
            }
            self.hasher.update(&self.chunk);
            start = end;
        }
    }

    #[must_use]
    pub fn finish(self) -> String {
        hex(&self.hasher.finalize())
    }
}

/// A borrowed channel column of one batch.
#[derive(Clone, Copy, Debug)]
pub enum ColumnValues<'a> {
    Uint64(&'a [u64]),
    Float32(&'a [f32]),
    Float64(&'a [f64]),
}

impl ColumnValues<'_> {
    /// Bytes of one value.
    #[must_use]
    pub const fn width(&self) -> usize {
        match self {
            Self::Float32(_) => 4,
            Self::Uint64(_) | Self::Float64(_) => 8,
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            write!(text, "{byte:02x}").expect("write to String");
            text
        })
}

/// Why `name` cannot be a channel column, or `None`.
#[must_use]
pub fn channel_name_problem(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if name.trim().is_empty() {
        Some("channel name is empty")
    } else if name.chars().any(char::is_control) {
        Some("channel name contains control characters")
    } else if RESERVED_NAMES.contains(&lower.as_str())
        || lower.starts_with("cytiq.")
        || lower.starts_with("__")
    {
        Some("channel name is reserved")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> LayerDescriptor {
        LayerDescriptor {
            contract: LAYER_CONTRACT.into(),
            acquisition_id: "a".repeat(64),
            state: LayerState::Transformed,
            steps: vec![
                Step {
                    kind: StepKind::Compensation,
                    method: "spillover".into(),
                    origin: Origin::Recorded,
                    definition_sha256: Some("c".repeat(64)),
                    parameters: None,
                    channels: vec!["FL1".into()],
                },
                Step {
                    kind: StepKind::Transform,
                    method: "arcsinh".into(),
                    origin: Origin::Recorded,
                    definition_sha256: None,
                    parameters: Some(json!({"cofactor": 150.0})),
                    channels: vec!["FL1".into()],
                },
            ],
            channels: vec![
                ChannelSpec {
                    index: 1,
                    name: "FSC".into(),
                    value_type: ValueType::Float32,
                },
                ChannelSpec {
                    index: 2,
                    name: "FL1".into(),
                    value_type: ValueType::Float32,
                },
            ],
        }
    }

    #[test]
    fn layer_ids_change_with_acquisition_and_recipes_do_not() {
        let base = descriptor();
        let mut other = base.clone();
        other.acquisition_id = "b".repeat(64);
        other.steps[0].definition_sha256 = Some("d".repeat(64));
        assert_ne!(base.layer_id(), other.layer_id());
        assert_eq!(base.recipe_id(), other.recipe_id());
        let mut cofactor = base.clone();
        cofactor.steps[1].parameters = Some(json!({"cofactor": 5.0}));
        assert_ne!(base.recipe_id(), cofactor.recipe_id());
        let mut order = base.clone();
        order.channels.swap(0, 1);
        assert_ne!(base.layer_id(), order.layer_id());
        assert_ne!(base.recipe_id(), order.recipe_id());
    }

    #[test]
    fn parents_are_step_prefixes_of_the_same_acquisition() {
        let child = descriptor();
        let mut parent = child.clone();
        parent.steps.truncate(1);
        parent.state = LayerState::Compensated;
        assert!(parent.is_parent_of(&child));
        assert!(!child.is_parent_of(&parent));
        assert!(!child.is_parent_of(&child));
        let mut foreign = parent.clone();
        foreign.acquisition_id = "b".repeat(64);
        assert!(!foreign.is_parent_of(&child));
        assert_eq!(
            child.steps_of("FL1"),
            [StepKind::Compensation, StepKind::Transform]
        );
        assert_eq!(child.steps_of("FSC"), Vec::<StepKind>::new());
    }

    /// The definition, one row at a time.
    fn reference_digest(layer_id: &str, ids: &[u64], columns: &[ColumnValues<'_>]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!("{VALUES_DOMAIN}\n{layer_id}\n").as_bytes());
        for (row, id) in ids.iter().enumerate() {
            hasher.update(id.to_le_bytes());
            for column in columns {
                match column {
                    ColumnValues::Uint64(v) => hasher.update(v[row].to_le_bytes()),
                    ColumnValues::Float32(v) => hasher.update(v[row].to_bits().to_le_bytes()),
                    ColumnValues::Float64(v) => hasher.update(v[row].to_bits().to_le_bytes()),
                }
            }
        }
        hex(&hasher.finalize())
    }

    #[test]
    fn chunked_hashing_matches_the_row_by_row_definition() {
        // Enough rows to span several chunks, with every type.
        let rows = 3 * HASH_CHUNK_BYTES / 28 + 5;
        let ids: Vec<u64> = (0..rows as u64).map(|i| i * 3 + 1).collect();
        #[allow(clippy::cast_precision_loss)]
        let a: Vec<f32> = (0..rows).map(|i| i as f32 * 0.5 - 7.0).collect();
        let b: Vec<u64> = (0..rows as u64).map(|i| u64::MAX - i).collect();
        #[allow(clippy::cast_precision_loss)]
        let c: Vec<f64> = (0..rows).map(|i| -(i as f64) / 3.0).collect();
        let columns = [
            ColumnValues::Float32(&a),
            ColumnValues::Uint64(&b),
            ColumnValues::Float64(&c),
        ];
        let mut hasher = ValuesHasher::new("layer");
        hasher.update(&ids[..17], &columns.map(|column| slice(column, 0, 17)));
        hasher.update(&ids[17..], &columns.map(|column| slice(column, 17, rows)));
        assert_eq!(hasher.finish(), reference_digest("layer", &ids, &columns));
    }

    fn slice(column: ColumnValues<'_>, start: usize, end: usize) -> ColumnValues<'_> {
        match column {
            ColumnValues::Uint64(v) => ColumnValues::Uint64(&v[start..end]),
            ColumnValues::Float32(v) => ColumnValues::Float32(&v[start..end]),
            ColumnValues::Float64(v) => ColumnValues::Float64(&v[start..end]),
        }
    }

    #[test]
    fn values_digest_is_row_major_and_batch_independent() {
        let ids = [0_u64, 1, 2];
        let a = [1.0_f32, 2.0, 3.0];
        let b = [7_u64, 8, 9];
        let mut whole = ValuesHasher::new("x");
        whole.update(&ids, &[ColumnValues::Float32(&a), ColumnValues::Uint64(&b)]);
        let mut split = ValuesHasher::new("x");
        split.update(
            &ids[..1],
            &[
                ColumnValues::Float32(&a[..1]),
                ColumnValues::Uint64(&b[..1]),
            ],
        );
        split.update(
            &ids[1..],
            &[
                ColumnValues::Float32(&a[1..]),
                ColumnValues::Uint64(&b[1..]),
            ],
        );
        assert_eq!(whole.finish(), split.finish());
        let mut other_layer = ValuesHasher::new("y");
        other_layer.update(&ids, &[ColumnValues::Float32(&a), ColumnValues::Uint64(&b)]);
        let mut same = ValuesHasher::new("x");
        same.update(&ids, &[ColumnValues::Float32(&a), ColumnValues::Uint64(&b)]);
        assert_ne!(other_layer.finish(), same.finish());
    }

    #[test]
    fn reserved_and_malformed_channel_names_are_refused() {
        for name in ["FSC-A", "CD3 FITC", "FL1.INT_CD14.FITC"] {
            assert_eq!(channel_name_problem(name), None, "{name}");
        }
        for name in [
            "",
            " ",
            "a\tb",
            "Source_Event_ID",
            "KEEP",
            "Original_ID",
            "cytiq.x",
            "__index",
        ] {
            assert!(channel_name_problem(name).is_some(), "{name:?}");
        }
    }
}
