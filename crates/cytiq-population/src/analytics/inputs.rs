//! What the product reads from a dataset: the manifest, checked without
//! I/O, and the objects it summarizes, each verified as `dataset validate
//! --verify-objects` does. Events tables are not read: views refer to them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::annotations::EventAnnotations;
use crate::dataset::objects::verified_labels;
use crate::dataset::{self, AcquisitionEntry, ArtifactRole, DatasetManifest, Profile, contracts};
use crate::issue::{Issues, pointer};
use crate::labels::LabelsSummary;
use crate::object::ObjectRoot;
use crate::serialization::{self, DocumentFormat};
use crate::{PopulationError, PopulationObservation, Result, table};

/// A verified label table of the dataset.
pub(crate) struct LabelTable {
    /// Its object key, relative to the dataset root.
    pub location: String,
    pub panel: Option<String>,
    pub summary: LabelsSummary,
}

/// A verified observations table and its rows.
pub(crate) struct ObservationTable {
    pub semantic_id: String,
    pub rows: Vec<PopulationObservation>,
}

/// A population annotation descriptor (`event-annotations.json`).
pub(crate) struct Descriptor {
    pub semantic_id: String,
    pub value: EventAnnotations,
}

pub(crate) struct Inputs {
    pub manifest: DatasetManifest,
    pub labels: Vec<LabelTable>,
    pub observations: Vec<ObservationTable>,
    pub descriptors: Vec<Descriptor>,
}

/// The manifest's own bytes and every object the product reads, by
/// provenance role, for the export's record.
pub(crate) fn files(manifest_path: &Path, root: &Path) -> Result<Vec<(&'static str, PathBuf)>> {
    let manifest = read_manifest(manifest_path)?;
    let objects = ObjectRoot::open(root)?;
    let mut files = vec![("manifest", manifest_path.to_path_buf())];
    for artifact in &manifest.artifacts {
        let role = match (artifact.role, artifact.contract.as_str()) {
            (ArtifactRole::Annotation, contract) if contract == contracts::LABELS => "labels",
            (ArtifactRole::Observations, _) => "observations",
            (ArtifactRole::Annotation, contract) if contracts::ANNOTATIONS.contains(&contract) => {
                "annotations"
            }
            _ => continue,
        };
        // An object the export will refuse is not read for the record either.
        if let Ok(key) = crate::identity::ObjectKey::parse(&artifact.location)
            && objects.object(&key).is_ok()
        {
            files.push((role, key.resolve(root)));
        }
    }
    Ok(files)
}

fn read_manifest(path: &Path) -> Result<DatasetManifest> {
    if !path.is_file() {
        return Err(PopulationError::Missing(path.to_path_buf()));
    }
    let bytes = std::fs::read(path)?;
    let mut issues = Issues::new();
    dataset::parse(&bytes, DocumentFormat::Json, &mut issues).ok_or_else(|| refusal(&issues))
}

fn refusal(issues: &Issues) -> PopulationError {
    let first = issues
        .items()
        .iter()
        .find(|issue| issue.severity == crate::issue::Severity::Error);
    PopulationError::Invalid(match first {
        Some(issue) => format!(
            "the dataset does not validate ({} at `{}`: {}); see `cytiq dataset validate`",
            serde_json::to_value(issue.code)
                .ok()
                .and_then(|code| code.as_str().map(str::to_owned))
                .unwrap_or_default(),
            issue.pointer,
            issue.message
        ),
        None => "the dataset does not validate; see `cytiq dataset validate`".into(),
    })
}

/// Read and verify everything the product summarizes.
pub(crate) fn read(manifest_path: &Path, root: &Path) -> Result<Inputs> {
    let manifest = read_manifest(manifest_path)?;
    let mut issues = Issues::new();
    dataset::validate_document(&manifest, Profile::Structural, &mut issues);
    if issues.has_errors() {
        return Err(refusal(&issues));
    }
    let acquisitions: HashMap<&str, &AcquisitionEntry> = manifest
        .acquisitions
        .iter()
        .map(|a| (a.id.as_str(), a))
        .collect();
    let root = ObjectRoot::open(root)?;
    let mut labels = Vec::new();
    let mut observations = Vec::new();
    let mut descriptors = Vec::new();
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let at = pointer(["artifacts", &index.to_string()]);
        let reads = match artifact.role {
            ArtifactRole::Annotation => {
                artifact.contract == contracts::LABELS
                    || contracts::ANNOTATIONS.contains(&artifact.contract.as_str())
            }
            ArtifactRole::Observations => true,
            _ => false,
        };
        if !reads {
            continue;
        }
        let Some(file) = root.verified(&artifact.location, &artifact.content, &at, &mut issues)
        else {
            continue;
        };
        if artifact.contract == contracts::LABELS {
            let acquisition = artifact
                .acquisition
                .as_deref()
                .and_then(|id| acquisitions.get(id).copied());
            if let Some(acquisition) = acquisition
                && let Some(summary) =
                    verified_labels(artifact, acquisition, file, &at, &mut issues)
            {
                labels.push(LabelTable {
                    location: artifact.location.clone(),
                    panel: artifact.panel.clone(),
                    summary,
                });
            }
        } else if artifact.role == ArtifactRole::Observations {
            match table::read_from(file) {
                Ok(rows) => observations.push(ObservationTable {
                    semantic_id: artifact.semantic_id.clone(),
                    rows,
                }),
                Err(error) => issues.error(
                    crate::issue::IssueCode::CorruptArtifactHash,
                    at,
                    format!("unreadable observations: {error}"),
                ),
            }
        } else {
            match serialization::read_from(file, Path::new(&artifact.location))
                .and_then(|value| Ok(serde_json::from_value::<EventAnnotations>(value)?))
            {
                Ok(value) => descriptors.push(Descriptor {
                    semantic_id: artifact.semantic_id.clone(),
                    value,
                }),
                Err(error) => issues.error(
                    crate::issue::IssueCode::CorruptArtifactHash,
                    at,
                    format!("unreadable annotation descriptor: {error}"),
                ),
            }
        }
    }
    if issues.has_errors() {
        return Err(refusal(&issues));
    }
    Ok(Inputs {
        manifest,
        labels,
        observations,
        descriptors,
    })
}
