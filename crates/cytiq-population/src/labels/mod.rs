//! `cytiq.events.labels.v1`: what QC, gating, `FlowSOM` and classification
//! decided about each event, converted from their native outputs, never
//! recomputed.
//!
//! A label table holds one row per raw event of one acquisition, in raw
//! order (`source_event_id` is exactly `0..raw_events`), for one label
//! source. Every row stores its keys, `acquisition_id`, `annotation_id` and
//! `source_event_id`, so tables of several acquisitions and sources can be
//! stacked and joined to `cytiq.events.v1` tables without losing identity.
//!
//! `state` says what happened to the event as a whole; the label columns
//! after it hold the per-label decisions, null where nothing was decided.
//! The kinds and their columns:
//!
//! | kind | `state` | label columns |
//! | --- | --- | --- |
//! | `qc` | `accepted`, `qc_rejected`, `not_assessed` | one nullable Boolean per mask filter |
//! | `gate` | `assessed`, `qc_rejected`, `not_assessed` | one nullable Boolean per gate, null unless `assessed` |
//! | `cluster` | `classified`, `qc_rejected`, `not_assessed` | `metacluster`, `node` |
//! | `classifier` | `classified`, `abstained`, `qc_rejected`, `not_assessed` | `label`, `abstain_reason`, `confidence`, `uncertainty`, `ood_score`, `is_ood`, then `p_<class>` |
//!
//! The [`LabelDescriptor`] in the footer says where the labels come from;
//! its digest is the table's `annotation_id`, and the digest of its
//! definition part, when there is one, its `definition_id`.

mod descriptor;
mod digest;
mod read;
mod runner;
mod sources;
mod write;

use serde::{Deserialize, Serialize};

pub use descriptor::{Definition, Eligibility, LabelColumn, LabelDescriptor, Producer};
pub use read::{
    ABSTAIN_REASONS, CellCount, LabelsSummary, verify, verify_file, verify_with_limits,
};
pub use runner::{ClusterModel, ExportLabelsConfig, GatingDefinition, LabelsRunner};
pub use sources::flowsom::{Assignment, Assignments, read_flowsom_assignments};
pub use write::{LabelBatch, LabelSettings, LabelWriter, WrittenLabels};

/// Contract identifier of a label table.
pub const CONTRACT: &str = "cytiq.events.labels.v1";
/// Contract identifier of the [`LabelDescriptor`].
pub const DESCRIPTOR_CONTRACT: &str = "cytiq.events.labels.descriptor.v1";
/// Contract identifier of what `definition_id` digests.
pub const DEFINITION_CONTRACT: &str = "cytiq.events.labels.definition.v1";
pub const SCHEMA_VERSION: u32 = 1;

pub const SCHEMA_VERSION_KEY: &str = "cytiq.labels.schema_version";
/// The [`LabelKind`].
pub const KIND_KEY: &str = "cytiq.labels.kind";
/// Canonical JSON of the [`LabelDescriptor`]; authoritative.
pub const DESCRIPTOR_KEY: &str = "cytiq.labels.descriptor";
pub const ANNOTATION_ID_KEY: &str = "cytiq.labels.annotation_id";
/// Present only when the labels have a definition artifact.
pub const DEFINITION_ID_KEY: &str = "cytiq.labels.definition_id";
pub const RAW_EVENTS_KEY: &str = "cytiq.labels.raw_events";
/// Encoding-independent digest of every row (see `digest.rs`).
pub const VALUES_SHA256_KEY: &str = "cytiq.labels.values_sha256";
pub use cytiq_mask::{ACQUISITION_KEY, EVENT_ID_BASE_KEY, SOURCE_EVENT_ID_COLUMN};

pub const ACQUISITION_COLUMN: &str = "acquisition_id";
pub const ANNOTATION_COLUMN: &str = "annotation_id";
pub const STATE_COLUMN: &str = "state";
/// Key columns, in order, before the kind's columns.
pub const KEY_COLUMNS: [&str; 4] = [
    ACQUISITION_COLUMN,
    ANNOTATION_COLUMN,
    SOURCE_EVENT_ID_COLUMN,
    STATE_COLUMN,
];
/// Columns per table: what the bounded Parquet reader accepts.
pub const MAX_COLUMNS: usize = 1024;
/// Domain tag that starts the values digest.
pub const VALUES_DOMAIN: &str = "cytiq.events.labels.values.v1";
/// Identifies the writer in the Parquet footer.
pub const CREATED_BY: &str = "cytiq labels.v1";
/// Distinct (column, value, state) counts a summary keeps.
pub const MAX_COUNTS: usize = 1 << 16;

/// Which native output the labels come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelKind {
    /// A technical QC mask (`cytiq.mask.v2`).
    Qc,
    /// Gate membership (`cytiq gate --membership`).
    Gate,
    /// `FlowSOM` node and metacluster assignments.
    Cluster,
    /// Classifier predictions, with decisions and abstentions.
    Classifier,
}

impl LabelKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Qc => "qc",
            Self::Gate => "gate",
            Self::Cluster => "cluster",
            Self::Classifier => "classifier",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        [Self::Qc, Self::Gate, Self::Cluster, Self::Classifier]
            .into_iter()
            .find(|kind| kind.as_str() == text)
    }

    /// The states a row of this kind may have.
    #[must_use]
    pub const fn states(self) -> &'static [LabelState] {
        use LabelState::{Abstained, Accepted, Assessed, Classified, NotAssessed, QcRejected};
        match self {
            Self::Qc => &[Accepted, QcRejected, NotAssessed],
            Self::Gate => &[Assessed, QcRejected, NotAssessed],
            Self::Cluster => &[Classified, QcRejected, NotAssessed],
            Self::Classifier => &[Classified, Abstained, QcRejected, NotAssessed],
        }
    }

    /// Columns every table of this kind has after the keys.
    #[must_use]
    pub const fn fixed_columns(self) -> &'static [(&'static str, LabelType)] {
        match self {
            Self::Qc | Self::Gate => &[],
            Self::Cluster => &[
                ("metacluster", LabelType::UInt64),
                ("node", LabelType::UInt64),
            ],
            Self::Classifier => &[
                ("label", LabelType::Utf8),
                ("abstain_reason", LabelType::Utf8),
                ("confidence", LabelType::Float32),
                ("uncertainty", LabelType::Float32),
                ("ood_score", LabelType::Float32),
                ("is_ood", LabelType::Boolean),
            ],
        }
    }

    /// The type of the descriptor's label columns, `None` when the kind has
    /// none.
    #[must_use]
    pub const fn label_type(self) -> Option<LabelType> {
        match self {
            Self::Qc | Self::Gate => Some(LabelType::Boolean),
            Self::Cluster => None,
            Self::Classifier => Some(LabelType::Float32),
        }
    }
}

/// What happened to an event as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelState {
    /// Passed technical QC (`qc`).
    Accepted,
    /// Excluded by the technical QC the producer was given.
    QcRejected,
    /// The producer decided nothing for this event, for no recorded QC
    /// reason.
    NotAssessed,
    /// Every gate was evaluated (`gate`).
    Assessed,
    /// Assigned a class or cluster.
    Classified,
    /// Evaluated, but the classifier declined to assign a class.
    Abstained,
}

impl LabelState {
    pub const ALL: [Self; 6] = [
        Self::Accepted,
        Self::QcRejected,
        Self::NotAssessed,
        Self::Assessed,
        Self::Classified,
        Self::Abstained,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::QcRejected => "qc_rejected",
            Self::NotAssessed => "not_assessed",
            Self::Assessed => "assessed",
            Self::Classified => "classified",
            Self::Abstained => "abstained",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == text)
    }
}

/// Physical type of a label column. Every label column is nullable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LabelType {
    Boolean,
    UInt64,
    Float32,
    Utf8,
}

impl LabelType {
    #[must_use]
    pub const fn arrow(self) -> arrow_schema::DataType {
        match self {
            Self::Boolean => arrow_schema::DataType::Boolean,
            Self::UInt64 => arrow_schema::DataType::UInt64,
            Self::Float32 => arrow_schema::DataType::Float32,
            Self::Utf8 => arrow_schema::DataType::Utf8,
        }
    }
}

/// Why `name` cannot be a label column of a `kind` table, or `None`. Names
/// are compared ignoring case, as SQL engines do.
#[must_use]
pub fn label_name_problem(kind: LabelKind, name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if name.trim().is_empty() {
        Some("label name is empty")
    } else if name.chars().any(char::is_control) {
        Some("label name contains a control character")
    } else if lower.starts_with("cytiq.") || lower.starts_with("__") {
        Some("label names starting with `cytiq.` or `__` are reserved")
    } else if KEY_COLUMNS
        .iter()
        .chain(&["event_id", "keep", "assessed"])
        .chain(kind.fixed_columns().iter().map(|(name, _)| name))
        .any(|reserved| *reserved == lower)
    {
        Some("label name is reserved")
    } else {
        None
    }
}
