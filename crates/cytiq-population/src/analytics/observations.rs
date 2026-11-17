//! Population observations flattened for SQL, and the path-free sources
//! and bindings of the annotation descriptors the dataset lists.

use std::collections::HashMap;

use super::dimensions::name;
use super::inputs::{Descriptor, Inputs};
use super::tables::Columns;
use crate::{PopulationError, PopulationObservation, Result};

/// Observations per product, at most.
pub const MAX_ROWS: usize = 10 * crate::table::MAX_OBSERVATIONS;

/// One flattened observation: the observations table it came from, the
/// dataset sample of its acquisition, and the row.
struct Row<'a> {
    table: &'a str,
    sample: Option<&'a str>,
    observation: &'a PopulationObservation,
    qc: Option<&'a str>,
}

/// Every observation of every observations table. The sample is the
/// dataset's sample of the observation's acquisition; the plan's own
/// acquisition label, which may not be a pseudonym, is not exported.
///
/// # Errors
/// Refuses observations of an acquisition or specimen outside the dataset,
/// and more than [`MAX_ROWS`] observations.
pub(crate) fn observations(inputs: &Inputs) -> Result<Columns> {
    let manifest = &inputs.manifest;
    let samples: HashMap<&str, &str> = manifest
        .samples
        .iter()
        .map(|s| (s.acquisition.as_str(), s.id.as_str()))
        .collect();
    let specimens: Vec<&str> = manifest.specimens.iter().map(|s| s.id.as_str()).collect();
    let qc: HashMap<&str, &str> = inputs
        .descriptors
        .iter()
        .map(|d| {
            (
                d.value.data.sha256.as_str(),
                d.value.technical_status.as_str(),
            )
        })
        .collect();
    let mut rows = Vec::new();
    for table in &inputs.observations {
        for observation in &table.rows {
            let sample = match &observation.lineage.acquisition_sha256 {
                None => None,
                Some(acquisition) => Some(*samples.get(acquisition.as_str()).ok_or_else(|| {
                    PopulationError::Invalid(format!(
                        "observation `{}` is of an acquisition without a dataset sample",
                        observation.observation_id
                    ))
                })?),
            };
            if !specimens.is_empty() && !specimens.contains(&observation.specimen_id.as_str()) {
                return Err(PopulationError::Invalid(format!(
                    "observation `{}` is of specimen `{}`, which the dataset does not list",
                    observation.observation_id, observation.specimen_id
                )));
            }
            rows.push(Row {
                table: &table.semantic_id,
                sample,
                qc: observation
                    .lineage
                    .annotations_sha256
                    .as_deref()
                    .and_then(|data| qc.get(data).copied()),
                observation,
            });
            if rows.len() > MAX_ROWS {
                return Err(PopulationError::Invalid(format!(
                    "the dataset holds more than {MAX_ROWS} observations"
                )));
            }
        }
    }
    Ok(columns(&rows))
}

fn observation_of<'a>(row: &Row<'a>) -> &'a PopulationObservation {
    row.observation
}

fn columns(rows: &[Row<'_>]) -> Columns {
    let text = |f: &dyn Fn(&Row<'_>) -> String| rows.iter().map(f).collect::<Vec<_>>();
    let optional = |f: &dyn Fn(&Row<'_>) -> Option<String>| rows.iter().map(f).collect::<Vec<_>>();
    let o = observation_of;
    Columns::default()
        .text("observations_id", text(&|r| r.table.to_owned()))
        .text("observation_id", text(&|r| o(r).observation_id.clone()))
        .text("specimen_id", text(&|r| o(r).specimen_id.clone()))
        .text_or_null("sample_id", optional(&|r| r.sample.map(str::to_owned)))
        .text_or_null(
            "acquisition_id",
            optional(&|r| o(r).lineage.acquisition_sha256.clone()),
        )
        .text("panel_id", text(&|r| o(r).panel_id.clone()))
        .text("source_id", text(&|r| o(r).source_id.clone()))
        .text("target_kind", text(&|r| name(&o(r).target.kind)))
        .text("target_id", text(&|r| o(r).target.id.clone()))
        .text("target_scope", text(&|r| o(r).target.scope.clone()))
        .boolean_or_null(
            "target_state",
            rows.iter().map(|r| o(r).target.state).collect(),
        )
        .text("measure", text(&|r| name(&o(r).measure)))
        .text("unit", text(&|r| o(r).unit.clone()))
        .text("status", text(&|r| name(&o(r).status)))
        .text_or_null("reason", optional(&|r| o(r).reason.clone()))
        .uint_or_null("value_u64", rows.iter().map(|r| o(r).value_u64).collect())
        .float_or_null("value_f64", rows.iter().map(|r| o(r).value_f64).collect())
        .uint("n_events", rows.iter().map(|r| o(r).n_events).collect())
        .uint_or_null(
            "supporting_count",
            rows.iter().map(|r| o(r).supporting_count).collect(),
        )
        .text_or_null(
            "denominator_target_kind",
            optional(&|r| {
                let target = o(r).denominator.as_ref()?.target.as_ref()?;
                Some(name(&target.kind))
            }),
        )
        .text_or_null(
            "denominator_target_id",
            optional(&|r| Some(o(r).denominator.as_ref()?.target.as_ref()?.id.clone())),
        )
        .text_or_null(
            "denominator_scope",
            optional(&|r| Some(o(r).denominator.as_ref()?.scope.clone())),
        )
        .uint_or_null(
            "denominator_count",
            rows.iter()
                .map(|r| o(r).denominator.as_ref().map(|d| d.observed_count))
                .collect(),
        )
        .text("method_name", text(&|r| o(r).method.name.clone()))
        .text("method_version", text(&|r| o(r).method.version.clone()))
        .text(
            "method_definition_sha256",
            text(&|r| o(r).method.definition_sha256.clone()),
        )
        .boolean(
            "method_experimental",
            rows.iter().map(|r| o(r).method.experimental).collect(),
        )
        .uint(
            "source_observations",
            rows.iter()
                .map(|r| o(r).method.source_observation_ids.len() as u64)
                .collect(),
        )
        .text(
            "lineage_tree_sha256",
            text(&|r| o(r).lineage.tree_sha256.clone()),
        )
        .text(
            "lineage_panel_profile_sha256",
            text(&|r| o(r).lineage.panel_profile_sha256.clone()),
        )
        .text(
            "lineage_projection_sha256",
            text(&|r| o(r).lineage.projection_sha256.clone()),
        )
        .text_or_null(
            "lineage_annotations_sha256",
            optional(&|r| o(r).lineage.annotations_sha256.clone()),
        )
        .text_or_null(
            "lineage_producer_provenance_sha256",
            optional(&|r| o(r).lineage.producer_provenance_sha256.clone()),
        )
        .text_or_null(
            "lineage_analysis_mask_sha256",
            optional(&|r| o(r).lineage.analysis_mask_sha256.clone()),
        )
        .text_or_null(
            "technical_qc_status",
            optional(&|r| r.qc.map(str::to_owned)),
        )
}

/// One row per listed annotation descriptor: its native data, source,
/// definition digests and counts, without any path.
pub(crate) fn sources(descriptors: &[Descriptor], annotation_of: &HashMap<&str, &str>) -> Columns {
    let d = |f: &dyn Fn(&Descriptor) -> String| descriptors.iter().map(f).collect::<Vec<_>>();
    let o =
        |f: &dyn Fn(&Descriptor) -> Option<String>| descriptors.iter().map(f).collect::<Vec<_>>();
    Columns::default()
        .text("annotations_sha256", d(&|x| x.value.data.sha256.clone()))
        .text("descriptor_id", d(&|x| x.semantic_id.clone()))
        .text_or_null(
            "annotation_id",
            o(&|x| {
                annotation_of
                    .get(x.value.data.sha256.as_str())
                    .map(|id| (*id).to_owned())
            }),
        )
        .text("specimen_id", d(&|x| x.value.context.specimen_id.clone()))
        .text("panel_id", d(&|x| x.value.context.panel_id.clone()))
        .text(
            "acquisition_id",
            d(&|x| x.value.context.acquisition_sha256.clone()),
        )
        .text("source_id", d(&|x| x.value.context.source_id.clone()))
        .text("format", d(&|x| name(&x.value.context.source.format)))
        .text(
            "definition_sha256",
            d(&|x| x.value.context.source.definition.sha256.clone()),
        )
        .text_or_null(
            "bundle_manifest_sha256",
            o(&|x| {
                x.value
                    .context
                    .source
                    .bundle_manifest
                    .as_ref()
                    .map(|r| r.sha256.clone())
            }),
        )
        .text_or_null(
            "decision_policy_sha256",
            o(&|x| {
                x.value
                    .context
                    .source
                    .decision_policy
                    .as_ref()
                    .map(|r| r.sha256.clone())
            }),
        )
        .text(
            "analysis_mask_sha256",
            d(&|x| x.value.analysis_mask.sha256.clone()),
        )
        .text("technical_status", d(&|x| x.value.technical_status.clone()))
        .uint(
            "assessed_events",
            descriptors
                .iter()
                .map(|x| x.value.assessed_events)
                .collect(),
        )
        .uint(
            "unknown_events",
            descriptors.iter().map(|x| x.value.unknown_events).collect(),
        )
        .uint_or_null(
            "abstained_events",
            descriptors
                .iter()
                .map(|x| x.value.abstained_events)
                .collect(),
        )
        .uint_or_null(
            "ood_events",
            descriptors.iter().map(|x| x.value.ood_events).collect(),
        )
}

/// Which label of each source names which population target, and its
/// denominator label.
pub(crate) fn bindings(descriptors: &[Descriptor]) -> Columns {
    let rows: Vec<_> = descriptors
        .iter()
        .flat_map(|d| {
            d.value
                .context
                .source
                .bindings
                .iter()
                .map(move |binding| (d, binding))
        })
        .collect();
    Columns::default()
        .text(
            "annotations_sha256",
            rows.iter()
                .map(|(d, _)| d.value.data.sha256.clone())
                .collect(),
        )
        .text("label", rows.iter().map(|(_, b)| b.label.clone()).collect())
        .text(
            "target_kind",
            rows.iter().map(|(_, b)| name(&b.target.kind)).collect(),
        )
        .text(
            "target_id",
            rows.iter().map(|(_, b)| b.target.id.clone()).collect(),
        )
        .text(
            "target_scope",
            rows.iter().map(|(_, b)| b.target.scope.clone()).collect(),
        )
        .boolean_or_null(
            "target_state",
            rows.iter().map(|(_, b)| b.target.state).collect(),
        )
        .text_or_null(
            "denominator_label",
            rows.iter().map(|(_, b)| b.denominator.clone()).collect(),
        )
}
