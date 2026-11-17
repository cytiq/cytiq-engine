//! Rules that read the objects a manifest lists: presence, size and digest,
//! then what each contract allows to be checked inside.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use arrow_array::{Array, UInt64Array};

use super::{
    AcquisitionEntry, ArtifactEntry, ArtifactRole, DatasetManifest, LayerSummary, contracts,
};
use crate::bitmap::Bitmap;
use crate::events::{self, EventsSummary, LayerDescriptor, StepKind};
use crate::identity::ObjectKey;
use crate::issue::{IssueCode, Issues, pointer};

/// Report every listed object that is missing, differs from its recorded
/// size or digest, or whose content contradicts the manifest. Objects
/// resolve against `root`; keys that are not relative were reported by the
/// document rules and are skipped.
pub fn verify_objects(manifest: &DatasetManifest, root: &Path, issues: &mut Issues) {
    let acquisitions: HashMap<&str, &AcquisitionEntry> = manifest
        .acquisitions
        .iter()
        .map(|a| (a.id.as_str(), a))
        .collect();
    let mut layers: HashMap<&str, LayerDescriptor> = HashMap::new();
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let at = pointer(["artifacts", &index.to_string()]);
        let Some(path) = present(root, &artifact.location, &artifact.content, &at, issues) else {
            continue;
        };
        let acquisition = artifact
            .acquisition
            .as_deref()
            .and_then(|id| acquisitions.get(id).copied());
        match artifact.role {
            ArtifactRole::SignalLayer => {
                let summary = if artifact.contract == contracts::EVENTS_PARTS {
                    events::verify_parts(&path, &at, issues)
                } else {
                    events::verify_file(&path, &at, issues)
                };
                if let Some(summary) = summary {
                    check_layer(manifest, artifact, acquisition, &summary, &at, issues);
                    layers.insert(artifact.semantic_id.as_str(), summary.descriptor);
                }
            }
            ArtifactRole::TechnicalMask | ArtifactRole::Annotation
                if artifact.contract == contracts::MASK
                    || contracts::CLASSIFY_PREDICTIONS.contains(&artifact.contract.as_str()) =>
            {
                if let Some(acquisition) = acquisition {
                    check_event_table(artifact, acquisition, &path, &at, issues);
                }
            }
            ArtifactRole::Observations => {
                check_observations(&acquisitions, artifact, &path, &at, issues);
            }
            _ => {}
        }
        check_provenance(root, artifact, &at, issues);
    }
    check_layer_parents(manifest, &layers, issues);
    for (index, acquisition) in manifest.acquisitions.iter().enumerate() {
        if let Some(source) = &acquisition.source {
            let at = pointer(["acquisitions", &index.to_string(), "source"]);
            if let Some(path) = present(root, &source.location, &source.content, &at, issues) {
                let raw = cytiq_fcs::FcsReader::open(&path)
                    .and_then(cytiq_fcs::FcsReader::read)
                    .and_then(|dataset| cytiq_fcs::acquisition_id(&path, &dataset.text));
                if raw.as_deref().ok() != Some(acquisition.id.as_str()) {
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

/// The object's path when it exists with the recorded size and digest.
fn present(
    root: &Path,
    location: &str,
    content: &crate::identity::ContentRef,
    at: &str,
    issues: &mut Issues,
) -> Option<PathBuf> {
    let path = ObjectKey::parse(location).ok()?.resolve(root);
    if !path.is_file() {
        issues.error(
            IssueCode::MissingObject,
            format!("{at}/location"),
            format!("`{location}` is missing"),
        );
        return None;
    }
    match cytiq_provenance::hash_file(&path) {
        Ok((bytes, sha256)) if bytes == content.bytes && sha256 == content.sha256 => Some(path),
        Ok((bytes, _)) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                format!("{at}/content"),
                format!(
                    "`{location}` has {bytes} bytes and a different digest than recorded ({} bytes)",
                    content.bytes
                ),
            );
            None
        }
        Err(error) => {
            issues.error(
                IssueCode::MissingObject,
                format!("{at}/location"),
                format!("`{location}` cannot be read: {error}"),
            );
            None
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
    path: &Path,
    at: &str,
    issues: &mut Issues,
) {
    let reader = match crate::native::open_table(path) {
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
    path: &Path,
    at: &str,
    issues: &mut Issues,
) {
    let rows = match crate::table::read(path) {
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
fn check_provenance(root: &Path, artifact: &ArtifactEntry, at: &str, issues: &mut Issues) {
    let Some(execution) = &artifact.execution else {
        return;
    };
    let Some(location) = &execution.provenance else {
        return;
    };
    let Ok(key) = ObjectKey::parse(location) else {
        return;
    };
    let path = key.resolve(root);
    if !path.is_file() {
        return;
    }
    let at = format!("{at}/execution");
    let record = match crate::native::producer_record(&path) {
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
