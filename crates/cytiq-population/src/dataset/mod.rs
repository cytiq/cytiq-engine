//! Dataset manifest v1: one versioned document describing an analytical
//! dataset: its acquisitions and their panels, channels and markers, the
//! derived artifacts (signal layers, masks, annotations, observations,
//! definitions), their identities, checksums and sizes, scientific metadata
//! after `MIFlowCyt`, and the privacy contract under which it is exported.
//!
//! Artifacts are located by object keys relative to the manifest's root and
//! identified three ways (see [`crate::identity`]). `dataset_id` is the
//! digest of the manifest without locations, contents and executions, so a
//! dataset keeps its identity when it moves or is re-encoded.

mod document;
mod from_run;
pub(crate) mod objects;
mod profile;
mod runner;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cytiq_manifest::{
    ControlRole, Cytometry, Experiment, Instrument, Reagent, SignalState, Specimen,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::{LayerState, Origin, StepKind};
use crate::identity::{ContentRef, ExecutionRef};
use crate::issue::{Issue, IssueCode, Issues};
use crate::privacy::PrivacyPolicy;
use crate::{Result, serialization};

pub use document::validate_document;
pub use from_run::{acquisition_entry, panel_from_run};
pub use objects::verify_objects;
pub use profile::Profile;
pub use runner::{ValidateConfig, ValidateRunner};

pub const SCHEMA_VERSION: u32 = 1;
pub const ARTIFACT_TYPE: &str = "cytiq_dataset_manifest";
pub const REPORT_ARTIFACT_TYPE: &str = "cytiq_dataset_validation";

/// Contracts an artifact may declare. Readers reject any other.
pub mod contracts {
    pub const EVENTS: &str = crate::events::CONTRACT;
    /// A signal layer split into `cytiq.events.v1` parts.
    pub const EVENTS_PARTS: &str = crate::events::PARTS_CONTRACT;
    pub const MASK: &str = "cytiq.mask.v2";
    pub const CLASSIFY_PREDICTIONS: &[&str] = &[
        "cytiq.classify.predictions.v2",
        "cytiq.classify.predictions.v3",
        "cytiq.classify.predictions.v4",
        "cytiq.classify.predictions.v5",
    ];
    pub const ANNOTATIONS: &[&str] = &[
        "cytiq.population.annotations.v1",
        "cytiq.population.annotations.v2",
    ];
    pub const OBSERVATIONS: &str = "cytiq.population.observations.v1";
    /// Per-event labels of one native output (`cytiq export labels`).
    pub const LABELS: &str = crate::labels::CONTRACT;
    pub const GATING_ML: &str = "gating-ml.v2";
    pub const CLASSIFY_BUNDLE: &str = "cytiq.classify.bundle";
    pub const CYTIQ_TREE: &str = "cytiq-tree.v2";
    pub const PROVENANCE: &str = "cytiq.provenance.v3";

    /// Whether `contract` is one this version of cytiq reads.
    #[must_use]
    pub fn is_known(contract: &str) -> bool {
        [
            EVENTS,
            EVENTS_PARTS,
            MASK,
            LABELS,
            OBSERVATIONS,
            GATING_ML,
            CLASSIFY_BUNDLE,
            CYTIQ_TREE,
            PROVENANCE,
        ]
        .contains(&contract)
            || CLASSIFY_PREDICTIONS.contains(&contract)
            || ANNOTATIONS.contains(&contract)
    }
}

/// What a channel measures, after FCS 3.2 `$PnTYPE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    ForwardScatter,
    SideScatter,
    Fluorescence,
    UnmixedComponent,
    Mass,
    Time,
    Other,
}

/// One channel of a panel, in `$Pn` order. Channel, detector, marker and
/// reporter are separate identities and never derived from one another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelEntry {
    /// `$Pn` index.
    pub index: u16,
    /// `$PnN`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ChannelKind>,
    /// Instrument detector (`$PnDET`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector: Option<String>,
    /// Analyte (`$PnANALYTE`, the run manifest's marker).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    /// Fluorochrome or metal tag (`$PnTAG`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reagent: Option<Reagent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelEntry {
    pub id: String,
    pub cytometry: Cytometry,
    /// What the panel's files held before any cytiq stage.
    #[serde(default)]
    pub signal_state: SignalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    /// The channels of the panel's acquired files, in `$Pn` order.
    pub channels: Vec<ChannelEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleEntry {
    /// Study pseudonym.
    pub id: String,
    pub panel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specimen: Option<String>,
    /// The acquisition's SHA-256 identity.
    pub acquisition: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionRole {
    Sample,
    Control,
}

/// The source FCS file as an object of the dataset (needs a disclosure).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceObject {
    pub location: String,
    pub content: ContentRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionEntry {
    /// SHA-256 of the raw FCS file (`CYTIQ_ACQUISITION`).
    pub id: String,
    pub panel: String,
    pub raw_events: u64,
    pub role: AcquisitionRole,
    /// Exported FCS keywords: the privacy allowlist plus disclosures.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub keywords: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceObject>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlEntry {
    pub acquisition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    pub role: ControlRole,
    /// The reporter a single-stain control carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
}

/// An upstream knowledge artifact (cytiq-tree) by canonical digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEntry {
    /// `curated_tree`, `panel_projection`, `class_vocabulary`, ...
    pub kind: String,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    /// Events of one acquisition in one signal state (`cytiq.events.v1`).
    SignalLayer,
    /// Per-event QC filters (`cytiq.mask.v2`).
    TechnicalMask,
    /// Biological event annotations: gate membership or classifier
    /// predictions, or their descriptor.
    Annotation,
    /// Population observations.
    Observations,
    /// A classifier bundle or model.
    Model,
    /// A Gating-ML definition.
    Gating,
    /// An upstream knowledge artifact.
    Knowledge,
    /// A v3 provenance record (needs a disclosure).
    Provenance,
}

/// A processing step as the manifest summarizes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSummary {
    pub kind: StepKind,
    pub method: String,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerSummary {
    pub state: LayerState,
    pub recipe_id: String,
    pub steps: Vec<StepSummary>,
}

impl LayerSummary {
    /// The summary of a full layer descriptor.
    #[must_use]
    pub fn of(descriptor: &crate::events::LayerDescriptor) -> Self {
        Self {
            state: descriptor.state,
            recipe_id: descriptor.recipe_id(),
            steps: descriptor
                .steps
                .iter()
                .map(|step| StepSummary {
                    kind: step.kind,
                    method: step.method.clone(),
                    origin: step.origin,
                    definition_sha256: step.definition_sha256.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEntry {
    pub role: ArtifactRole,
    pub contract: String,
    /// Path-free identity; parents refer to it.
    pub semantic_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerSummary>,
    pub content: ContentRef,
    /// Object key relative to the dataset root.
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionRef>,
}

impl ArtifactEntry {
    /// The entry of an events table holding `descriptor`'s layer with
    /// values digest `values_sha256`; its semantic id is the layer id.
    #[must_use]
    pub fn signal_layer(
        descriptor: &crate::events::LayerDescriptor,
        values_sha256: &str,
        panel: Option<String>,
        parents: Vec<String>,
        location: String,
        content: ContentRef,
    ) -> Self {
        Self {
            role: ArtifactRole::SignalLayer,
            contract: contracts::EVENTS.into(),
            semantic_id: descriptor.layer_id(),
            acquisition: Some(descriptor.acquisition_id.clone()),
            panel,
            parents,
            layer: Some(LayerSummary::of(descriptor)),
            content: ContentRef {
                values_sha256: Some(values_sha256.to_owned()),
                ..content
            },
            location,
            execution: None,
        }
    }

    /// [`Self::signal_layer`] for a layer split into parts: `location` and
    /// `content` are those of its `parts.json`, `values_sha256` the layer's.
    #[must_use]
    pub fn signal_layer_parts(
        descriptor: &crate::events::LayerDescriptor,
        values_sha256: &str,
        panel: Option<String>,
        parents: Vec<String>,
        location: String,
        content: ContentRef,
    ) -> Self {
        Self {
            contract: contracts::EVENTS_PARTS.into(),
            ..Self::signal_layer(descriptor, values_sha256, panel, parents, location, content)
        }
    }

    /// The entry of a `cytiq.events.labels.v1` table: an annotation of its
    /// acquisition whose semantic id is its `annotation_id`.
    #[must_use]
    pub fn labels(
        summary: &crate::labels::LabelsSummary,
        panel: Option<String>,
        parents: Vec<String>,
        location: String,
        content: ContentRef,
    ) -> Self {
        Self {
            role: ArtifactRole::Annotation,
            contract: contracts::LABELS.into(),
            semantic_id: summary.annotation_id.clone(),
            acquisition: Some(summary.descriptor.acquisition_id.clone()),
            panel,
            parents,
            layer: None,
            content: ContentRef {
                values_sha256: Some(summary.values_sha256.clone()),
                ..content
            },
            location,
            execution: None,
        }
    }

    /// The entry of another object, identified by its role, contract,
    /// acquisition, parents and content digest.
    #[must_use]
    pub fn object(
        role: ArtifactRole,
        contract: &str,
        acquisition: Option<String>,
        parents: Vec<String>,
        location: String,
        content: ContentRef,
    ) -> Self {
        let role_name = serde_json::to_value(role)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        Self {
            semantic_id: crate::identity::semantic_id(
                contract,
                &role_name,
                acquisition.as_deref(),
                &parents,
                &content.sha256,
            ),
            role,
            contract: contract.into(),
            acquisition,
            panel: None,
            parents,
            layer: None,
            content,
            location,
            execution: None,
        }
    }
}

/// Size and digest of the file at `path`.
///
/// # Errors
/// Returns read errors.
pub fn content_of(path: &Path) -> Result<ContentRef> {
    let (bytes, sha256) = cytiq_provenance::hash_file(path)?;
    Ok(ContentRef {
        sha256,
        bytes,
        values_sha256: None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetManifest {
    pub schema_version: u32,
    pub artifact_type: String,
    /// Digest of the location-free manifest; checked when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<String>,
    pub contracts: Vec<String>,
    pub privacy: PrivacyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub study: Option<Experiment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruments: Vec<Instrument>,
    pub panels: Vec<PanelEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specimens: Vec<Specimen>,
    pub samples: Vec<SampleEntry>,
    pub acquisitions: Vec<AcquisitionEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge: Vec<KnowledgeEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactEntry>,
    /// Subjects and experimental factors, for cohort statistics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<Design>,
}

/// The study design. Subjects are the biological units cohort statistics
/// count: many events, samples or specimens of one subject are still one
/// subject. Factors (group, timepoint, batch...) are declared once, at the
/// level they vary: a subject's group, a specimen's timepoint, a sample's
/// batch. Ids and values are study pseudonyms, never names or dates.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Design {
    pub factors: Vec<Factor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<SubjectEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specimens: Vec<SpecimenDesign>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<SampleDesign>,
}

/// One experimental factor and the level it is given at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Factor {
    pub name: String,
    pub level: FactorLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorLevel {
    Subject,
    Specimen,
    Sample,
}

impl FactorLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Specimen => "specimen",
            Self::Sample => "sample",
        }
    }
}

/// A subject (donor, patient, animal) and its subject-level factors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub factors: BTreeMap<String, String>,
}

/// The subject a specimen comes from, and its specimen-level factors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecimenDesign {
    pub specimen: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub factors: BTreeMap<String, String>,
}

/// A sample's sample-level factors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleDesign {
    pub sample: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub factors: BTreeMap<String, String>,
}

/// Fields that depend on where and how objects are stored, not on what the
/// dataset is.
const LOCATION_FIELDS: &[&str] = &["content", "location", "execution", "source"];

impl DatasetManifest {
    /// The dataset's identity: the canonical digest of the manifest without
    /// `dataset_id` and without every artifact's content, location and
    /// execution and every acquisition's source object.
    ///
    /// # Panics
    /// Never: the manifest holds only strings, integers and booleans.
    #[must_use]
    pub fn dataset_id(&self) -> String {
        let mut value = serde_json::to_value(self).expect("manifest serializes");
        if let Value::Object(map) = &mut value {
            map.remove("dataset_id");
            for section in ["artifacts", "acquisitions"] {
                if let Some(Value::Array(entries)) = map.get_mut(section) {
                    for entry in entries.iter_mut().filter_map(Value::as_object_mut) {
                        for field in LOCATION_FIELDS {
                            entry.remove(*field);
                        }
                    }
                }
            }
        }
        serialization::digest(&value)
    }

    /// The same manifest with `dataset_id` filled in.
    #[must_use]
    pub fn sealed(mut self) -> Self {
        self.dataset_id = None;
        self.dataset_id = Some(self.dataset_id());
        self
    }

    /// The canonical JSON bytes of the manifest (sorted keys, two-space
    /// indent, ASCII), the form fixtures and digests use.
    ///
    /// # Panics
    /// Never: the manifest serializes to JSON.
    #[must_use]
    pub fn canonical_json(&self) -> String {
        serialization::canonical(&serde_json::to_value(self).expect("manifest serializes"))
    }
}

/// The published JSON Schemas of this version, by file name.
pub const PUBLISHED_SCHEMAS: &[(&str, &str)] = &[
    (
        "dataset-manifest.schema.json",
        include_str!("../../contracts/dataset/v1/dataset-manifest.schema.json"),
    ),
    (
        "events-channels.schema.json",
        include_str!("../../contracts/dataset/v1/events-channels.schema.json"),
    ),
    (
        "events-layer.schema.json",
        include_str!("../../contracts/dataset/v1/events-layer.schema.json"),
    ),
    (
        "events-parts.schema.json",
        include_str!("../../contracts/dataset/v1/events-parts.schema.json"),
    ),
    (
        "labels-descriptor.schema.json",
        include_str!("../../contracts/dataset/v1/labels-descriptor.schema.json"),
    ),
    (
        "validation-report.schema.json",
        include_str!("../../contracts/dataset/v1/validation-report.schema.json"),
    ),
];

/// Check `document` against one of [`PUBLISHED_SCHEMAS`], with the JSON
/// Schema subset the pinned contracts use (`$ref`, `anyOf`, `type`, `const`,
/// `enum`, `properties`, `required`, `additionalProperties`, `items`,
/// `minItems`, `minLength`, `minimum` and the SHA-256 `pattern`).
///
/// # Errors
/// Returns why the document does not conform, or that `schema` is unknown.
pub fn conforms(document: &Value, schema: &str) -> Result<()> {
    let (_, text) = PUBLISHED_SCHEMAS
        .iter()
        .find(|(name, _)| *name == schema)
        .ok_or_else(|| crate::PopulationError::Invalid(format!("unknown schema `{schema}`")))?;
    let schema: Value = serde_json::from_str(text)?;
    let mut copy = document.clone();
    crate::knowledge::schema::normalize(&mut copy, &schema, &schema)
}

/// Migrate a parsed document to [`SCHEMA_VERSION`]. Version 1 is current;
/// a later version adds a row here and migration tests.
///
/// # Errors
/// Returns the issue for a document of an unknown version or type.
pub fn migrate(document: Value) -> std::result::Result<Value, Issue> {
    let unsupported = |message: String| Issue {
        code: IssueCode::UnsupportedSchema,
        severity: crate::issue::Severity::Error,
        pointer: String::new(),
        message,
    };
    if document.get("artifact_type").and_then(Value::as_str) != Some(ARTIFACT_TYPE) {
        return Err(unsupported(format!("not a `{ARTIFACT_TYPE}` document")));
    }
    match document.get("schema_version").and_then(Value::as_u64) {
        Some(1) => Ok(document),
        Some(version) => Err(unsupported(format!(
            "dataset manifest schema_version {version} is not supported"
        ))),
        None => Err(unsupported(
            "`schema_version` is missing or not an integer".into(),
        )),
    }
}

/// Parse a manifest: bounded, duplicate keys rejected, version checked and
/// migrated, then strictly typed. Problems are reported to `issues`.
pub fn parse(
    bytes: &[u8],
    format: serialization::DocumentFormat,
    issues: &mut Issues,
) -> Option<DatasetManifest> {
    let value = match serialization::parse(bytes, format) {
        Ok(value) => value,
        Err(error) => {
            issues.error(IssueCode::MalformedDocument, "", error.to_string());
            return None;
        }
    };
    let value = match migrate(value) {
        Ok(value) => value,
        Err(issue) => {
            issues.push(issue);
            return None;
        }
    };
    match serde_json::from_value::<DatasetManifest>(value) {
        Ok(manifest) => Some(manifest),
        Err(error) => {
            issues.error(IssueCode::MalformedDocument, "", error.to_string());
            None
        }
    }
}

/// How [`validate`] checks a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidateOptions {
    pub profile: Profile,
    /// Directory object keys resolve against; the manifest's directory by
    /// default.
    pub root: Option<PathBuf>,
    /// Read every object: sizes, digests, events tables, masks, annotations,
    /// observations and provenance records.
    pub verify_objects: bool,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        Self {
            profile: Profile::Structural,
            root: None,
            verify_objects: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Valid,
    Invalid,
}

/// The outcome of [`validate`], written by `cytiq dataset validate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationReport {
    pub schema_version: u32,
    pub artifact_type: String,
    #[serde(default)]
    pub dataset_id: Option<String>,
    /// SHA-256 of the manifest bytes validated.
    pub manifest_sha256: String,
    pub profile: Profile,
    pub verified_objects: bool,
    pub status: ReportStatus,
    pub errors: usize,
    pub warnings: usize,
    pub truncated: bool,
    pub issues: Vec<Issue>,
}

/// Validate the manifest at `path`: its document rules, the profile and,
/// when asked, every object it lists.
///
/// # Errors
/// Returns an error only when the manifest cannot be read; everything wrong
/// with its content is an issue of the report.
pub fn validate(path: &Path, options: &ValidateOptions) -> Result<ValidationReport> {
    if !path.exists() {
        return Err(crate::PopulationError::Missing(path.to_owned()));
    }
    let bytes = cytiq_provenance::input::document_bytes(path)?;
    let manifest_sha256 = cytiq_provenance::hash_file(path)?.1;
    let mut issues = Issues::new();
    let manifest = parse(&bytes, serialization::DocumentFormat::of(path), &mut issues);
    let dataset_id = manifest.as_ref().map(DatasetManifest::dataset_id);
    if let Some(manifest) = &manifest {
        validate_document(manifest, options.profile, &mut issues);
        if options.verify_objects {
            let root = options
                .root
                .clone()
                .unwrap_or_else(|| path.parent().map_or_else(PathBuf::new, Path::to_path_buf));
            verify_objects(manifest, &root, &mut issues);
        }
    }
    Ok(ValidationReport {
        schema_version: 1,
        artifact_type: REPORT_ARTIFACT_TYPE.into(),
        dataset_id,
        manifest_sha256,
        profile: options.profile,
        verified_objects: options.verify_objects,
        status: if issues.has_errors() {
            ReportStatus::Invalid
        } else {
            ReportStatus::Valid
        },
        errors: issues.errors(),
        warnings: issues.warnings(),
        truncated: issues.truncated(),
        issues: issues.into_items(),
    })
}
