//! Rules that read the objects a manifest lists: presence, size and digest,
//! then what each contract allows to be checked inside.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;

use arrow_array::{Array, UInt64Array};

use super::{
    AcquisitionEntry, ArtifactEntry, ArtifactRole, DatasetManifest, LayerSummary, contracts,
};
use crate::bitmap::Bitmap;
use crate::events::{self, EventsSummary, LayerDescriptor, LayerState, StepKind};
use crate::identity::ObjectKey;
use crate::issue::{IssueCode, Issues, pointer};
use crate::object::{ObjectRoot, OpenError};

/// Report every listed object that is missing, differs from its recorded
/// size or digest, or whose content contradicts the manifest. Objects
/// resolve against `root` and are never read through a symbolic link below
/// it; each is opened once, and what is hashed is what is checked. Keys that
/// are not relative were reported by the document rules and are skipped.
pub fn verify_objects(manifest: &DatasetManifest, root: &Path, issues: &mut Issues) {
    let root = match ObjectRoot::open(root) {
        Ok(root) => root,
        Err(error) => {
            issues.error(
                IssueCode::MissingObject,
                "",
                format!("the dataset root cannot be opened: {error}"),
            );
            return;
        }
    };
    let acquisitions: HashMap<&str, &AcquisitionEntry> = manifest
        .acquisitions
        .iter()
        .map(|a| (a.id.as_str(), a))
        .collect();
    let mut layers: HashMap<&str, LayerDescriptor> = HashMap::new();
    let mut sources = Sources::default();
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let at = pointer(["artifacts", &index.to_string()]);
        let Some(file) = root.verified(&artifact.location, &artifact.content, &at, issues) else {
            continue;
        };
        let acquisition = artifact
            .acquisition
            .as_deref()
            .and_then(|id| acquisitions.get(id).copied());
        match artifact.role {
            ArtifactRole::SignalLayer => {
                let summary = if artifact.contract == contracts::EVENTS_PARTS {
                    events::verify_parts_in(&root, &artifact.location, file, &at, issues)
                } else {
                    events::verify(file, &at, issues)
                };
                if let Some(summary) = summary {
                    sources.of_layer(&summary, &at);
                    check_layer(manifest, artifact, acquisition, &summary, &at, issues);
                    layers.insert(artifact.semantic_id.as_str(), summary.descriptor);
                }
            }
            ArtifactRole::TechnicalMask | ArtifactRole::Annotation
                if artifact.contract == contracts::MASK
                    || contracts::CLASSIFY_PREDICTIONS.contains(&artifact.contract.as_str()) =>
            {
                if let Some(acquisition) = acquisition {
                    check_event_table(artifact, acquisition, file, &at, issues);
                }
            }
            ArtifactRole::Annotation if artifact.contract == contracts::LABELS => {
                if let Some(acquisition) = acquisition {
                    check_labels(artifact, acquisition, file, &at, issues);
                }
            }
            ArtifactRole::Observations => {
                check_observations(&acquisitions, artifact, file, &at, issues);
            }
            ArtifactRole::Provenance if artifact.contract == contracts::PROVENANCE => {
                sources.of_record(file, &artifact.location);
            }
            _ => {}
        }
        check_provenance(&root, artifact, &at, issues);
    }
    sources.check(issues);
    check_layer_parents(manifest, &layers, issues);
    for (index, acquisition) in manifest.acquisitions.iter().enumerate() {
        if let Some(source) = &acquisition.source {
            let at = pointer(["acquisitions", &index.to_string(), "source"]);
            if let Some(file) = root.verified(&source.location, &source.content, &at, issues) {
                // A file that records no acquisition is one: the digest just
                // verified is its identity. The instrument's file of a
                // repaired acquisition is that too, though only the repaired
                // file reads.
                let same = (source.content.sha256 == acquisition.id
                    && sources.repaired(&acquisition.id))
                    || cytiq_fcs::FcsReader::new(file).read().is_ok_and(|dataset| {
                        cytiq_fcs::recorded_acquisition(&dataset.text).is_ok_and(|recorded| {
                            recorded.unwrap_or(&source.content.sha256) == acquisition.id
                        })
                    });
                if !same {
                    issues.error(
                        IssueCode::InvalidReference,
                        at,
                        "the source FCS file is not this acquisition",
                    );
                }
            }
        }
    }
}

/// An events table agrees with its manifest entry, acquisition and panel.
fn check_layer(
    manifest: &DatasetManifest,
    artifact: &ArtifactEntry,
    acquisition: Option<&AcquisitionEntry>,
    summary: &EventsSummary,
    at: &str,
    issues: &mut Issues,
) {
    if summary.layer_id != artifact.semantic_id {
        issues.error(
            IssueCode::CorruptArtifactHash,
            format!("{at}/semantic_id"),
            "the table's layer id differs from the artifact's semantic id",
        );
    }
    if artifact.acquisition.as_deref() != Some(summary.descriptor.acquisition_id.as_str()) {
        issues.error(
            IssueCode::InvalidReference,
            format!("{at}/acquisition"),
            "the table holds events of another acquisition",
        );
    }
    if artifact
        .content
        .values_sha256
        .as_ref()
        .is_some_and(|digest| *digest != summary.values_sha256)
    {
        issues.error(
            IssueCode::CorruptArtifactHash,
            format!("{at}/content/values_sha256"),
            "the table's values digest differs from the recorded one",
        );
    }
    if artifact.layer.as_ref() != Some(&LayerSummary::of(&summary.descriptor)) {
        issues.error(
            IssueCode::CorruptArtifactHash,
            format!("{at}/layer"),
            "the layer summary differs from the table's descriptor",
        );
    }
    if !summary.all_columns {
        issues.error(
            IssueCode::PanelMismatch,
            at,
            "the table stores a projection of the layer's channels; a signal layer stores all",
        );
    }
    let Some(acquisition) = acquisition else {
        return;
    };
    if let Some(max) = summary.max_source_event_id
        && max >= acquisition.raw_events
    {
        issues.error(
            IssueCode::DuplicateEventIdentity,
            at,
            format!(
                "`source_event_id` {max} is beyond the acquisition's {} raw events",
                acquisition.raw_events
            ),
        );
    }
    if summary
        .raw_events
        .is_some_and(|raw| raw != acquisition.raw_events)
    {
        issues.error(
            IssueCode::InvalidReference,
            at,
            "the table's raw event count differs from the acquisition's",
        );
    }
    let unmixed = summary
        .descriptor
        .steps
        .iter()
        .any(|step| step.kind == StepKind::Unmixing && !step.channels.is_empty());
    if let Some(panel) = manifest.panels.iter().find(|p| p.id == acquisition.panel)
        && !unmixed
    {
        check_channels(panel, &summary.descriptor, at, issues);
    }
}

/// The tool a repair record names.
const REPAIR_TOOL: &str = "cytiq repair";

/// A raw layer of every event holds the acquisition's own rows, so the file it
/// was exported from is the acquisition, or a file `cytiq repair` made from it
/// as a listed record says. A derived layer, and a subset, come from a file a
/// stage wrote.
#[derive(Default)]
struct Sources {
    /// Raw layers of every event from another file: where, acquisition, source.
    foreign: Vec<(String, String, String)>,
    /// `(acquisition, file)` pairs a listed repair record made.
    repairs: HashSet<(String, String)>,
}

impl Sources {
    fn of_layer(&mut self, summary: &EventsSummary, at: &str) {
        let descriptor = &summary.descriptor;
        let raw = descriptor.state == LayerState::Raw && descriptor.steps.is_empty();
        if raw && summary.raw_events.is_some() && summary.source_sha256 != descriptor.acquisition_id
        {
            self.foreign.push((
                at.to_owned(),
                descriptor.acquisition_id.clone(),
                summary.source_sha256.clone(),
            ));
        }
    }

    /// A record of a successful `cytiq repair`: each output was made from its
    /// input. A record that cannot be read vouches for nothing.
    fn of_record(&mut self, file: File, location: &str) {
        let Ok(record) = crate::native::producer_record_from(file, Path::new(location)) else {
            return;
        };
        if record.tool != REPAIR_TOOL
            || record.reason != Some(cytiq_provenance::outcome::OutcomeReason::Success)
        {
            return;
        }
        let inputs = record
            .inputs
            .iter()
            .filter(|input| input.role.as_deref() == Some("input"));
        for input in inputs {
            for output in &record.outputs {
                self.repairs
                    .insert((input.sha256.clone(), output.sha256.clone()));
            }
        }
    }

    fn repaired(&self, acquisition: &str) -> bool {
        self.repairs.iter().any(|(input, _)| input == acquisition)
    }

    fn check(&self, issues: &mut Issues) {
        for (at, acquisition, source) in &self.foreign {
            if !self
                .repairs
                .contains(&(acquisition.clone(), source.clone()))
            {
                issues.error(
                    IssueCode::InvalidReference,
                    at.as_str(),
                    "a raw layer of every event was exported from a file that is not its \
                     acquisition, and no listed `cytiq repair` record made that file from it",
                );
            }
        }
    }
}

/// The table's channels are the panel's, in the same `$Pn` order.
fn check_channels(
    panel: &super::PanelEntry,
    descriptor: &LayerDescriptor,
    at: &str,
    issues: &mut Issues,
) {
    let table: Vec<(u16, &str)> = descriptor
        .channels
        .iter()
        .map(|c| (c.index, c.name.as_str()))
        .collect();
    let declared: Vec<(u16, &str)> = panel
        .channels
        .iter()
        .map(|c| (c.index, c.name.as_str()))
        .collect();
    if table == declared {
        return;
    }
    let mut sorted_table: Vec<&str> = table.iter().map(|(_, n)| *n).collect();
    let mut sorted_panel: Vec<&str> = declared.iter().map(|(_, n)| *n).collect();
    sorted_table.sort_unstable();
    sorted_panel.sort_unstable();
    if sorted_table == sorted_panel {
        issues.error(
            IssueCode::InconsistentChannelOrder,
            at,
            format!(
                "the table's channels are in another order than panel `{}`'s",
                panel.id
            ),
        );
    } else {
        issues.error(
            IssueCode::PanelMismatch,
            at,
            format!("the table's channels are not panel `{}`'s", panel.id),
        );
    }
}

/// A mask or prediction table belongs to its acquisition and names each raw
/// event at most once; a mask names every raw event.
fn check_event_table(
    artifact: &ArtifactEntry,
    acquisition: &AcquisitionEntry,
    file: File,
    at: &str,
    issues: &mut Issues,
) {
    let reader = match crate::native::table(file) {
        Ok(reader) => reader,
        Err(error) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!("unreadable table: {error}"),
            );
            return;
        }
    };
    if !table_header(artifact, acquisition, reader.schema(), at, issues) {
        return;
    }
    let reader = match reader.project(&[events::SOURCE_EVENT_ID_COLUMN]) {
        Ok(reader) => reader,
        Err(error) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!("no `source_event_id`: {error}"),
            );
            return;
        }
    };
    if let Some(seen) = scan_event_ids(reader, acquisition, at, issues)
        && artifact.contract == contracts::MASK
        && !seen.is_full()
    {
        issues.error(
            IssueCode::InvalidReference,
            at,
            "a mask covers every raw event of its acquisition",
        );
    }
}

/// A label table verifies, labels every raw event of its acquisition, and
/// is listed under its `annotation_id` and values digest.
fn check_labels(
    artifact: &ArtifactEntry,
    acquisition: &AcquisitionEntry,
    file: File,
    at: &str,
    issues: &mut Issues,
) {
    verified_labels(artifact, acquisition, file, at, issues);
}

/// [`check_labels`], returning the table's summary when it has no error.
pub(crate) fn verified_labels(
    artifact: &ArtifactEntry,
    acquisition: &AcquisitionEntry,
    file: File,
    at: &str,
    issues: &mut Issues,
) -> Option<crate::labels::LabelsSummary> {
    let errors = issues.errors();
    let summary = crate::labels::verify(file, at, issues)?;
    let descriptor = &summary.descriptor;
    if descriptor.acquisition_id != acquisition.id
        || descriptor.raw_events != acquisition.raw_events
    {
        issues.error(
            IssueCode::InvalidReference,
            at,
            "the label table is of another acquisition or event count than the artifact's",
        );
    }
    if artifact.semantic_id != summary.annotation_id {
        issues.error(
            IssueCode::CorruptArtifactHash,
            format!("{at}/semantic_id"),
            "a label table's semantic id is its `annotation_id`",
        );
    }
    if artifact.content.values_sha256.as_ref() != Some(&summary.values_sha256) {
        issues.error(
            IssueCode::CorruptArtifactHash,
            format!("{at}/content/values_sha256"),
            "the label table's values digest differs from the manifest's",
        );
    }
    (issues.errors() == errors).then_some(summary)
}

/// The table's contract version, acquisition and event base.
fn table_header(
    artifact: &ArtifactEntry,
    acquisition: &AcquisitionEntry,
    schema: &arrow_schema::Schema,
    at: &str,
    issues: &mut Issues,
) -> bool {
    let metadata = schema.metadata();
    let (version_key, expected) = if artifact.contract == contracts::MASK {
        (cytiq_mask::SCHEMA_VERSION_KEY, "2")
    } else {
        let version = artifact.contract.rsplit('v').next().unwrap_or_default();
        ("cytiq.classify.schema_version", version)
    };
    if metadata.get(version_key).map(String::as_str) != Some(expected) {
        issues.error(
            IssueCode::UnsupportedSchema,
            at,
            format!("the table is not `{}`", artifact.contract),
        );
        return false;
    }
    if metadata.get(cytiq_mask::ACQUISITION_KEY) != Some(&acquisition.id)
        || metadata
            .get(cytiq_mask::EVENT_ID_BASE_KEY)
            .map(String::as_str)
            != Some("0")
    {
        issues.error(
            IssueCode::InvalidReference,
            at,
            "the table belongs to another acquisition or event base",
        );
        return false;
    }
    true
}

/// The raw events a table names, each once and within the acquisition.
fn scan_event_ids(
    reader: cytiq_mask::parquet::ScalarParquetReader<std::fs::File>,
    acquisition: &AcquisitionEntry,
    at: &str,
    issues: &mut Issues,
) -> Option<Bitmap> {
    let mut seen = usize::try_from(acquisition.raw_events)
        .map_err(|_| crate::PopulationError::Invalid("raw event count overflows".into()))
        .and_then(Bitmap::new)
        .map_err(|error| issues.error(IssueCode::InvalidReference, at, error.to_string()))
        .ok()?;
    for batch in reader {
        let ids = match &batch {
            Ok(batch) => batch.column(0).as_any().downcast_ref::<UInt64Array>(),
            Err(_) => None,
        };
        let Some(ids) = ids.filter(|ids| ids.null_count() == 0) else {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                "`source_event_id` is not decodable",
            );
            return None;
        };
        for id in ids.values() {
            let problem = match seen.insert(*id) {
                Some(true) => continue,
                Some(false) => format!("raw event {id} appears more than once"),
                None => format!(
                    "raw event {id} is beyond the acquisition's {} events",
                    acquisition.raw_events
                ),
            };
            issues.error(IssueCode::DuplicateEventIdentity, at, problem);
            return None;
        }
    }
    Some(seen)
}

/// Every observation's acquisition is one the dataset lists, and the
/// artifact's when it names one.
fn check_observations(
    acquisitions: &HashMap<&str, &AcquisitionEntry>,
    artifact: &ArtifactEntry,
    file: File,
    at: &str,
    issues: &mut Issues,
) {
    let rows = match crate::table::read_from(file) {
        Ok(rows) => rows,
        Err(error) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!("unreadable observations: {error}"),
            );
            return;
        }
    };
    for row in &rows {
        let Some(sha) = &row.lineage.acquisition_sha256 else {
            continue;
        };
        let foreign = !acquisitions.contains_key(sha.as_str())
            || artifact.acquisition.as_ref().is_some_and(|own| own != sha);
        if foreign {
            issues.error(
                IssueCode::InvalidReference,
                at,
                format!(
                    "observation `{}` is of an acquisition outside the dataset",
                    row.observation_id
                ),
            );
            return;
        }
    }
}

/// The provenance record an artifact names lists it as an output and holds
/// the recorded fingerprint.
fn check_provenance(root: &ObjectRoot, artifact: &ArtifactEntry, at: &str, issues: &mut Issues) {
    let Some(execution) = &artifact.execution else {
        return;
    };
    let Some(location) = &execution.provenance else {
        return;
    };
    let Ok(key) = ObjectKey::parse(location) else {
        return;
    };
    let at = format!("{at}/execution");
    let file = match root.object(&key) {
        Ok(file) => file,
        // A dataset may reference a record it does not carry.
        Err(OpenError::Missing) => return,
        Err(error) => {
            error.report(location, at, issues);
            return;
        }
    };
    let record = match crate::native::producer_record_from(file, Path::new(location)) {
        Ok(record) => record,
        Err(error) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!("unreadable provenance: {error}"),
            );
            return;
        }
    };
    if !record
        .outputs
        .iter()
        .any(|output| output.sha256 == artifact.content.sha256)
    {
        issues.error(
            IssueCode::InvalidReference,
            at.clone(),
            "the provenance record does not list this object as an output",
        );
    }
    if execution.fingerprint.is_some() && execution.fingerprint != record.execution_fingerprint {
        issues.error(
            IssueCode::InvalidReference,
            at,
            "the execution fingerprint differs from the provenance record's",
        );
    }
}

/// Parent layers are step prefixes of their children, on the same acquisition.
fn check_layer_parents(
    manifest: &DatasetManifest,
    layers: &HashMap<&str, LayerDescriptor>,
    issues: &mut Issues,
) {
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let Some(child) = layers.get(artifact.semantic_id.as_str()) else {
            continue;
        };
        for (p, parent) in artifact.parents.iter().enumerate() {
            if let Some(parent) = layers.get(parent.as_str())
                && !parent.is_parent_of(child)
            {
                issues.error(
                    IssueCode::IncompatibleSignalState,
                    pointer(["artifacts", &index.to_string(), "parents", &p.to_string()]),
                    "the parent table's steps are not a prefix of this table's",
                );
            }
        }
    }
}
