//! `views.sql` (`cytiq.analytics.views.v1`): `DuckDB` views over the product's
//! tables and the dataset's events and label tables. Every path is relative:
//! product tables to the product directory, events and labels to the dataset
//! root. `SET file_search_path = '<product>,<dataset root>'` before running
//! it, so the views resolve wherever the files are.
//!
//! Cohort views aggregate per subject first: an n is a count of subjects,
//! never of events or replicate samples. A subject with more than one usable
//! value in a cell is counted as ambiguous, not averaged.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::inputs::LabelTable;
use crate::dataset::{ArtifactRole, DatasetManifest, FactorLevel, contracts};
use crate::events::PARTS_FILE;
use crate::labels::LabelKind;

/// Contract identifier of `views.sql`.
pub const CONTRACT: &str = "cytiq.analytics.views.v1";

/// The product tables every `views.sql` reads.
pub const TABLES: [&str; 17] = [
    "subjects",
    "specimens",
    "samples",
    "factors",
    "acquisitions",
    "panels",
    "channels",
    "layers",
    "labels",
    "label_columns",
    "label_classes",
    "label_states",
    "label_counts",
    "observations",
    "sources",
    "bindings",
    "parity",
];

/// A double-quoted SQL identifier.
pub(crate) fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A single-quoted SQL string.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// What `views.sql` defines, and whether its cohort views are present.
pub(crate) struct Views {
    pub text: String,
    pub names: Vec<String>,
}

struct Builder {
    text: String,
    names: Vec<String>,
}

impl Builder {
    fn view(&mut self, name: &str, body: &str) {
        let _ = writeln!(
            self.text,
            "CREATE OR REPLACE VIEW {} AS\n{body};\n",
            identifier(name)
        );
        self.names.push(name.to_owned());
    }

    fn comment(&mut self, text: &str) {
        for line in text.lines() {
            let _ = writeln!(self.text, "-- {line}");
        }
    }
}

/// The declared factors at the subject and specimen levels: what cohort
/// cells are made of. Sample factors (batches) are technical.
fn cell_factors(manifest: &DatasetManifest) -> Vec<String> {
    manifest
        .design
        .iter()
        .flat_map(|design| &design.factors)
        .filter(|f| f.level != FactorLevel::Sample)
        .map(|f| f.name.clone())
        .collect()
}

/// Generate `views.sql`; `cohort` says whether every sample has a subject.
pub(crate) fn generate(manifest: &DatasetManifest, labels: &[LabelTable], cohort: bool) -> Views {
    let mut b = Builder {
        text: String::new(),
        names: Vec::new(),
    };
    b.comment(&format!(
        "{CONTRACT}: DuckDB views over a cytiq analytics product.\n\
         Before running: SET file_search_path = '<product directory>,<dataset root>';"
    ));
    b.text.push('\n');
    for table in TABLES {
        b.view(
            table,
            &format!(
                "SELECT * FROM read_parquet({})",
                literal(&format!("tables/{table}.parquet"))
            ),
        );
    }
    design_views(&mut b, manifest);
    events_views(&mut b, manifest);
    label_views(&mut b, labels);
    sample_views(&mut b);
    if cohort {
        cohort_views(&mut b, manifest);
    }
    diffcyt_views(&mut b, manifest, labels);
    Views {
        text: b.text,
        names: b.names,
    }
}

/// One row per sample (and per specimen) with every declared factor of its
/// subject, specimen and sample as a column.
fn design_views(b: &mut Builder, manifest: &DatasetManifest) {
    let factors: Vec<(String, FactorLevel)> = manifest
        .design
        .iter()
        .flat_map(|design| &design.factors)
        .map(|f| (f.name.clone(), f.level))
        .collect();
    let column = |name: &str, level: FactorLevel, entity: &str| {
        format!(
            ",\n  (SELECT f.value FROM factors f WHERE f.level = {} AND f.entity_id = {entity} \
             AND f.factor = {}) AS {}",
            literal(level.as_str()),
            literal(name),
            identifier(name)
        )
    };
    let entity = |level: FactorLevel| match level {
        FactorLevel::Subject => "s.subject_id",
        FactorLevel::Specimen => "s.specimen_id",
        FactorLevel::Sample => "s.sample_id",
    };
    let mut sample = String::from(
        "SELECT s.sample_id, s.panel_id, s.acquisition_id, s.specimen_id, s.subject_id",
    );
    for (name, level) in &factors {
        sample.push_str(&column(name, *level, entity(*level)));
    }
    sample.push_str("\nFROM samples s");
    b.view("sample_design", &sample);
    let mut specimen = String::from("SELECT s.specimen_id, s.subject_id");
    for (name, level) in factors.iter().filter(|(_, l)| *l != FactorLevel::Sample) {
        specimen.push_str(&column(name, *level, entity(*level)));
    }
    specimen.push_str("\nFROM specimens s");
    b.view("specimen_design", &specimen);
}

/// `events_<panel>_<state>`: the signal layers of one panel and state,
/// stacked; tables of every acquisition carry their keys.
fn events_views(b: &mut Builder, manifest: &DatasetManifest) {
    let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for artifact in &manifest.artifacts {
        if artifact.role != ArtifactRole::SignalLayer {
            continue;
        }
        let (Some(panel), Some(layer)) = (&artifact.panel, &artifact.layer) else {
            continue;
        };
        let location = if artifact.contract == contracts::EVENTS_PARTS {
            artifact.location.strip_suffix(PARTS_FILE).map_or_else(
                || artifact.location.clone(),
                |dir| format!("{dir}part-*.parquet"),
            )
        } else {
            artifact.location.clone()
        };
        groups
            .entry((panel.clone(), layer.state.as_str().to_owned()))
            .or_default()
            .push(location);
    }
    for ((panel, state), locations) in groups {
        b.view(
            &format!("events_{panel}_{state}"),
            &format!(
                "SELECT * FROM read_parquet([{}], union_by_name = true)",
                list(&locations)
            ),
        );
    }
}

fn list(locations: &[String]) -> String {
    locations
        .iter()
        .map(|l| literal(l))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `labels_<kind>`: the label tables of one kind, stacked; and
/// `gate_membership`, every gate decision as a row (nulls kept).
fn label_views(b: &mut Builder, labels: &[LabelTable]) {
    let mut kinds: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for table in labels {
        kinds
            .entry(table.summary.descriptor.kind.as_str())
            .or_default()
            .push(table.location.clone());
    }
    for (kind, locations) in &kinds {
        b.view(
            &format!("labels_{kind}"),
            &format!(
                "SELECT * FROM read_parquet([{}], union_by_name = true)",
                list(locations)
            ),
        );
    }
    let gates: Vec<String> = labels
        .iter()
        .filter(|t| t.summary.descriptor.kind == LabelKind::Gate)
        .map(|t| {
            let columns = t
                .summary
                .descriptor
                .labels
                .iter()
                .map(|l| identifier(&l.name))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "  SELECT * FROM (SELECT acquisition_id, annotation_id, source_event_id, state, \
                 {columns} FROM read_parquet({})) UNPIVOT INCLUDE NULLS (member FOR gate IN \
                 ({columns}))",
                literal(&t.location)
            )
        })
        .collect();
    if !gates.is_empty() {
        b.view(
            "gate_membership",
            &format!(
                "SELECT m.*, l.definition_id FROM (\n{}\n) m JOIN labels l USING (annotation_id)",
                gates.join("\n  UNION ALL\n")
            ),
        );
    }
}

/// Exact counts per sample, and per-sample fractions of gates, classes and
/// metaclusters under a definition shared across acquisitions, zero-filled.
fn sample_views(b: &mut Builder) {
    b.view(
        "sample_label_states",
        "SELECT s.sample_id, s.subject_id, c.* FROM label_states c JOIN samples s USING (acquisition_id)",
    );
    b.view(
        "sample_label_counts",
        "SELECT s.sample_id, s.subject_id, c.* FROM label_counts c JOIN samples s USING (acquisition_id)",
    );
    b.view(
        "sample_label_fractions",
        "WITH totals AS (\n\
         \x20 SELECT annotation_id,\n\
         \x20   sum(count) FILTER (WHERE state = 'assessed') AS assessed,\n\
         \x20   sum(count) FILTER (WHERE state IN ('classified', 'abstained')) AS predicted,\n\
         \x20   sum(count) FILTER (WHERE state = 'classified') AS classified\n\
         \x20 FROM label_states GROUP BY annotation_id\n\
         ), cells AS (\n\
         \x20 SELECT l.annotation_id, c.label_column, 'true' AS label_value\n\
         \x20 FROM labels l JOIN label_columns c USING (annotation_id) WHERE l.kind = 'gate'\n\
         \x20 UNION ALL\n\
         \x20 SELECT annotation_id, 'label', class FROM label_classes\n\
         \x20 UNION ALL\n\
         \x20 SELECT annotation_id, 'metacluster', CAST(unnest(range(1, CAST(metaclusters AS BIGINT) + 1)) AS VARCHAR)\n\
         \x20 FROM labels WHERE kind = 'cluster' AND metaclusters IS NOT NULL\n\
         )\n\
         SELECT s.sample_id, s.subject_id, s.panel_id, l.acquisition_id, l.annotation_id, l.definition_id, l.kind,\n\
         \x20 cells.label_column, cells.label_value,\n\
         \x20 coalesce(n.count, 0) AS count,\n\
         \x20 CASE l.kind WHEN 'gate' THEN t.assessed WHEN 'classifier' THEN t.predicted ELSE t.classified END AS denominator,\n\
         \x20 coalesce(n.count, 0) / CAST(nullif(denominator, 0) AS DOUBLE) AS fraction\n\
         FROM cells\n\
         JOIN labels l USING (annotation_id)\n\
         JOIN totals t USING (annotation_id)\n\
         JOIN samples s ON s.acquisition_id = l.acquisition_id\n\
         LEFT JOIN label_counts n ON n.annotation_id = cells.annotation_id\n\
         \x20 AND n.label_column = cells.label_column AND n.label_value = cells.label_value\n\
         \x20 AND n.state = CASE l.kind WHEN 'gate' THEN 'assessed' ELSE 'classified' END\n\
         WHERE l.definition_id IS NOT NULL",
    );
    b.view(
        "population_measures",
        "SELECT specimen_id, sample_id, acquisition_id, panel_id, source_id, target_kind, target_id,\n\
         \x20 target_scope, target_state, denominator_target_id, denominator_scope,\n\
         \x20 any_value(denominator_count) AS denominator_count,\n\
         \x20 any_value(n_events) AS n_events,\n\
         \x20 any_value(value_u64) FILTER (WHERE measure = 'observed_count') AS observed_count,\n\
         \x20 any_value(value_f64) FILTER (WHERE measure = 'estimated_count') AS estimated_count,\n\
         \x20 any_value(value_f64) FILTER (WHERE measure = 'fraction') AS fraction,\n\
         \x20 any_value(status) FILTER (WHERE measure = 'observed_count') AS count_status,\n\
         \x20 any_value(status) FILTER (WHERE measure = 'fraction') AS fraction_status,\n\
         \x20 any_value(reason) FILTER (WHERE measure = 'fraction') AS fraction_reason,\n\
         \x20 any_value(method_name) AS method_name, any_value(method_definition_sha256) AS method_definition_sha256,\n\
         \x20 any_value(technical_qc_status) AS technical_qc_status\n\
         FROM (SELECT DISTINCT ON (observation_id) * FROM observations)\n\
         GROUP BY ALL",
    );
}

/// Per-subject statistics by subject and specimen factors.
fn cohort_views(b: &mut Builder, manifest: &DatasetManifest) {
    let factors = cell_factors(manifest)
        .iter()
        .map(|f| format!("{}, ", identifier(f)))
        .collect::<Vec<_>>()
        .concat();
    let statistics = "\x20 count(*) FILTER (WHERE usable = 1) AS n_subjects,\n\
         \x20 count(*) FILTER (WHERE usable > 1) AS n_subjects_ambiguous,\n\
         \x20 count(*) FILTER (WHERE usable = 0 AND qc_rejected > 0) AS n_subjects_qc_rejected,\n\
         \x20 count(*) FILTER (WHERE usable = 0 AND qc_rejected = 0) AS n_subjects_not_measured,\n\
         \x20 avg(value) FILTER (WHERE usable = 1) AS mean,\n\
         \x20 stddev_samp(value) FILTER (WHERE usable = 1) AS sd,\n\
         \x20 min(value) FILTER (WHERE usable = 1) AS min,\n\
         \x20 quantile_cont(value, 0.25) FILTER (WHERE usable = 1) AS q25,\n\
         \x20 median(value) FILTER (WHERE usable = 1) AS median,\n\
         \x20 quantile_cont(value, 0.75) FILTER (WHERE usable = 1) AS q75,\n\
         \x20 max(value) FILTER (WHERE usable = 1) AS max";
    b.view(
        "cohort_population",
        &format!(
            "WITH per_subject AS (\n\
             \x20 SELECT d.subject_id, {factors}o.panel_id, o.source_id, o.target_kind, o.target_id,\n\
             \x20   o.target_scope, o.measure, o.unit,\n\
             \x20   count(DISTINCT o.observation_id) FILTER (WHERE o.status IN ('measured', 'estimated')) AS usable,\n\
             \x20   count(DISTINCT o.observation_id) FILTER (WHERE o.status = 'qc_rejected') AS qc_rejected,\n\
             \x20   max(coalesce(o.value_f64, CAST(o.value_u64 AS DOUBLE)))\n\
             \x20     FILTER (WHERE o.status IN ('measured', 'estimated')) AS value\n\
             \x20 FROM observations o JOIN specimen_design d USING (specimen_id)\n\
             \x20 GROUP BY ALL\n\
             )\n\
             SELECT {factors}panel_id, source_id, target_kind, target_id, target_scope, measure, unit,\n\
             {statistics}\n\
             FROM per_subject GROUP BY ALL"
        ),
    );
    b.view(
        "cohort_labels",
        &format!(
            "WITH per_subject AS (\n\
             \x20 SELECT d.subject_id, {factors}f.panel_id, f.definition_id, f.kind, f.label_column,\n\
             \x20   f.label_value,\n\
             \x20   count(DISTINCT f.annotation_id) FILTER (WHERE f.fraction IS NOT NULL) AS usable,\n\
             \x20   0 AS qc_rejected,\n\
             \x20   max(f.fraction) AS value\n\
             \x20 FROM sample_label_fractions f JOIN sample_design d USING (sample_id, panel_id)\n\
             \x20 GROUP BY ALL\n\
             )\n\
             SELECT {factors}panel_id, definition_id, kind, label_column, label_value,\n\
             {statistics}\n\
             FROM per_subject GROUP BY ALL"
        ),
    );
}

/// Inputs for diffcyt and CATALYST: experiment info, zero-filled cluster
/// counts per sample, marker info and per-panel cluster medians.
fn diffcyt_views(b: &mut Builder, manifest: &DatasetManifest, labels: &[LabelTable]) {
    let clusters: Vec<&LabelTable> = labels
        .iter()
        .filter(|t| {
            t.summary.descriptor.kind == LabelKind::Cluster && t.summary.definition_id.is_some()
        })
        .collect();
    if clusters.is_empty() {
        return;
    }
    let factors = manifest
        .design
        .iter()
        .flat_map(|design| &design.factors)
        .map(|f| format!(", {}", identifier(&f.name)))
        .collect::<Vec<_>>()
        .concat();
    b.view(
        "diffcyt_experiment_info",
        &format!("SELECT sample_id, subject_id AS patient_id{factors} FROM sample_design"),
    );
    b.view(
        "diffcyt_counts",
        "SELECT l.definition_id, CAST(l.m AS VARCHAR) AS cluster_id, s.sample_id,\n\
         \x20 coalesce(c.count, 0) AS n_cells\n\
         FROM (SELECT annotation_id, acquisition_id, definition_id, unnest(range(1, CAST(metaclusters AS BIGINT) + 1)) AS m\n\
         \x20     FROM labels WHERE kind = 'cluster' AND definition_id IS NOT NULL AND metaclusters IS NOT NULL) l\n\
         JOIN samples s USING (acquisition_id)\n\
         LEFT JOIN label_counts c ON c.annotation_id = l.annotation_id AND c.label_column = 'metacluster'\n\
         \x20 AND c.label_value = CAST(l.m AS VARCHAR) AND c.state = 'classified'",
    );
    b.view(
        "diffcyt_marker_info",
        "SELECT panel_id, name AS channel_name, marker AS marker_name,\n\
         \x20 CAST(NULL AS VARCHAR) AS marker_class\n\
         FROM channels WHERE marker IS NOT NULL",
    );
    // Medians over the transformed (or normalized) layer of each panel
    // that has clusters of a shared definition.
    let panels: BTreeSet<&str> = clusters.iter().filter_map(|t| t.panel.as_deref()).collect();
    for panel in panels {
        let Some(entry) = manifest.panels.iter().find(|p| p.id == panel) else {
            continue;
        };
        let state = ["normalized", "transformed"].into_iter().find(|state| {
            b.names
                .iter()
                .any(|name| *name == format!("events_{panel}_{state}"))
        });
        let Some(state) = state else {
            continue;
        };
        let medians = entry
            .channels
            .iter()
            .filter(|c| c.marker.is_some())
            .map(|c| {
                format!(
                    ",\n  median(CAST(e.{} AS DOUBLE)) AS {}",
                    identifier(&c.name),
                    identifier(&c.name)
                )
            })
            .collect::<Vec<_>>()
            .concat();
        b.view(
            &format!("diffcyt_medians_{panel}"),
            &format!(
                "SELECT c.definition_id, CAST(l.metacluster AS VARCHAR) AS cluster_id, s.sample_id{medians}\n\
                 FROM {} e\n\
                 JOIN labels_cluster l USING (acquisition_id, source_event_id)\n\
                 JOIN labels c ON c.annotation_id = l.annotation_id AND c.definition_id IS NOT NULL\n\
                 JOIN samples s ON s.acquisition_id = e.acquisition_id\n\
                 WHERE l.state = 'classified'\n\
                 GROUP BY ALL",
                identifier(&format!("events_{panel}_{state}"))
            ),
        );
    }
}
