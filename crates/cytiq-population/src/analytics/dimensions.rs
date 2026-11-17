//! Dimension tables: who and what was measured, from the manifest alone.

use std::collections::HashMap;

use serde::Serialize;

use super::tables::Columns;
use crate::dataset::{ArtifactRole, DatasetManifest, FactorLevel};

/// The `snake_case` name serde gives `value`.
pub(crate) fn name(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The subject of each specimen the design places.
pub(crate) fn subjects_of(manifest: &DatasetManifest) -> HashMap<&str, &str> {
    manifest
        .design
        .iter()
        .flat_map(|design| &design.specimens)
        .map(|entry| (entry.specimen.as_str(), entry.subject.as_str()))
        .collect()
}

pub(crate) fn subjects(manifest: &DatasetManifest) -> Columns {
    let ids = manifest
        .design
        .iter()
        .flat_map(|design| &design.subjects)
        .map(|subject| subject.id.clone())
        .collect();
    Columns::default().text("subject_id", ids)
}

pub(crate) fn specimens(manifest: &DatasetManifest) -> Columns {
    let subjects = subjects_of(manifest);
    let specimens = &manifest.specimens;
    Columns::default()
        .text(
            "specimen_id",
            specimens.iter().map(|s| s.id.clone()).collect(),
        )
        .text_or_null(
            "subject_id",
            specimens
                .iter()
                .map(|s| subjects.get(s.id.as_str()).map(|id| (*id).to_owned()))
                .collect(),
        )
        .text_or_null(
            "organism",
            specimens.iter().map(|s| s.organism.clone()).collect(),
        )
        .text_or_null(
            "material",
            specimens.iter().map(|s| s.material.clone()).collect(),
        )
}

pub(crate) fn samples(manifest: &DatasetManifest) -> Columns {
    let subjects = subjects_of(manifest);
    let samples = &manifest.samples;
    Columns::default()
        .text("sample_id", samples.iter().map(|s| s.id.clone()).collect())
        .text(
            "panel_id",
            samples.iter().map(|s| s.panel.clone()).collect(),
        )
        .text(
            "acquisition_id",
            samples.iter().map(|s| s.acquisition.clone()).collect(),
        )
        .text_or_null(
            "specimen_id",
            samples.iter().map(|s| s.specimen.clone()).collect(),
        )
        .text_or_null(
            "subject_id",
            samples
                .iter()
                .map(|s| {
                    s.specimen
                        .as_deref()
                        .and_then(|specimen| subjects.get(specimen))
                        .map(|id| (*id).to_owned())
                })
                .collect(),
        )
}

/// Every factor value, long: (level, entity, factor, value).
pub(crate) fn factors(manifest: &DatasetManifest) -> Columns {
    let mut rows: Vec<(FactorLevel, String, String, String)> = Vec::new();
    if let Some(design) = &manifest.design {
        let mut add =
            |level, entity: &str, factors: &std::collections::BTreeMap<String, String>| {
                for (factor, value) in factors {
                    rows.push((level, entity.to_owned(), factor.clone(), value.clone()));
                }
            };
        for subject in &design.subjects {
            add(FactorLevel::Subject, &subject.id, &subject.factors);
        }
        for specimen in &design.specimens {
            add(FactorLevel::Specimen, &specimen.specimen, &specimen.factors);
        }
        for sample in &design.samples {
            add(FactorLevel::Sample, &sample.sample, &sample.factors);
        }
    }
    Columns::default()
        .text(
            "level",
            rows.iter().map(|r| r.0.as_str().to_owned()).collect(),
        )
        .text("entity_id", rows.iter().map(|r| r.1.clone()).collect())
        .text("factor", rows.iter().map(|r| r.2.clone()).collect())
        .text("value", rows.iter().map(|r| r.3.clone()).collect())
}

pub(crate) fn acquisitions(manifest: &DatasetManifest) -> Columns {
    let acquisitions = &manifest.acquisitions;
    Columns::default()
        .text(
            "acquisition_id",
            acquisitions.iter().map(|a| a.id.clone()).collect(),
        )
        .text(
            "panel_id",
            acquisitions.iter().map(|a| a.panel.clone()).collect(),
        )
        .uint(
            "raw_events",
            acquisitions.iter().map(|a| a.raw_events).collect(),
        )
        .text("role", acquisitions.iter().map(|a| name(&a.role)).collect())
}

pub(crate) fn panels(manifest: &DatasetManifest) -> Columns {
    let panels = &manifest.panels;
    Columns::default()
        .text("panel_id", panels.iter().map(|p| p.id.clone()).collect())
        .text(
            "cytometry",
            panels.iter().map(|p| name(&p.cytometry)).collect(),
        )
        .text(
            "signal_state",
            panels.iter().map(|p| name(&p.signal_state)).collect(),
        )
}

pub(crate) fn channels(manifest: &DatasetManifest) -> Columns {
    let rows: Vec<_> = manifest
        .panels
        .iter()
        .flat_map(|panel| {
            panel
                .channels
                .iter()
                .enumerate()
                .map(move |(position, channel)| (panel, position, channel))
        })
        .collect();
    Columns::default()
        .text(
            "panel_id",
            rows.iter().map(|(panel, ..)| panel.id.clone()).collect(),
        )
        .uint(
            "position",
            rows.iter()
                .map(|(_, position, _)| *position as u64)
                .collect(),
        )
        .uint(
            "index",
            rows.iter().map(|(.., c)| u64::from(c.index)).collect(),
        )
        .text("name", rows.iter().map(|(.., c)| c.name.clone()).collect())
        .text_or_null(
            "kind",
            rows.iter()
                .map(|(.., c)| c.kind.as_ref().map(name))
                .collect(),
        )
        .text_or_null(
            "detector",
            rows.iter().map(|(.., c)| c.detector.clone()).collect(),
        )
        .text_or_null(
            "marker",
            rows.iter().map(|(.., c)| c.marker.clone()).collect(),
        )
        .text_or_null(
            "reporter",
            rows.iter().map(|(.., c)| c.reporter.clone()).collect(),
        )
}

/// Signal layers, with the object key of their table (or parts manifest)
/// relative to the dataset root.
pub(crate) fn layers(manifest: &DatasetManifest) -> Columns {
    let rows: Vec<_> = manifest
        .artifacts
        .iter()
        .filter(|a| a.role == ArtifactRole::SignalLayer)
        .collect();
    Columns::default()
        .text(
            "layer_id",
            rows.iter().map(|a| a.semantic_id.clone()).collect(),
        )
        .text_or_null(
            "acquisition_id",
            rows.iter().map(|a| a.acquisition.clone()).collect(),
        )
        .text_or_null("panel_id", rows.iter().map(|a| a.panel.clone()).collect())
        .text_or_null(
            "state",
            rows.iter()
                .map(|a| {
                    a.layer
                        .as_ref()
                        .map(|layer| layer.state.as_str().to_owned())
                })
                .collect(),
        )
        .text_or_null(
            "recipe_id",
            rows.iter()
                .map(|a| a.layer.as_ref().map(|layer| layer.recipe_id.clone()))
                .collect(),
        )
        .text(
            "contract",
            rows.iter().map(|a| a.contract.clone()).collect(),
        )
        .text(
            "location",
            rows.iter().map(|a| a.location.clone()).collect(),
        )
}
