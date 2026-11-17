//! The label tables of a dataset: a catalog, their label columns and their
//! exact counts, taken from the verified summaries (no row is read twice).

use super::inputs::LabelTable;
use super::tables::Columns;

/// One row per label table.
pub(crate) fn catalog(labels: &[LabelTable]) -> Columns {
    let field = |f: fn(&LabelTable) -> Option<String>| labels.iter().map(f).collect::<Vec<_>>();
    Columns::default()
        .text(
            "annotation_id",
            labels
                .iter()
                .map(|l| l.summary.annotation_id.clone())
                .collect(),
        )
        .text_or_null("definition_id", field(|l| l.summary.definition_id.clone()))
        .text(
            "kind",
            labels
                .iter()
                .map(|l| l.summary.descriptor.kind.as_str().to_owned())
                .collect(),
        )
        .text(
            "acquisition_id",
            labels
                .iter()
                .map(|l| l.summary.descriptor.acquisition_id.clone())
                .collect(),
        )
        .text_or_null("panel_id", field(|l| l.panel.clone()))
        .text(
            "location",
            labels.iter().map(|l| l.location.clone()).collect(),
        )
        .uint(
            "raw_events",
            labels
                .iter()
                .map(|l| l.summary.descriptor.raw_events)
                .collect(),
        )
        .text(
            "tool",
            labels
                .iter()
                .map(|l| l.summary.descriptor.producer.tool.clone())
                .collect(),
        )
        .text(
            "native_contract",
            labels
                .iter()
                .map(|l| l.summary.descriptor.producer.native_contract.clone())
                .collect(),
        )
        .text(
            "native_sha256",
            labels
                .iter()
                .map(|l| l.summary.descriptor.producer.native_sha256.clone())
                .collect(),
        )
        .text_or_null(
            "input_layer_id",
            field(|l| l.summary.descriptor.producer.input_layer_id.clone()),
        )
        .boolean(
            "eligibility_verified",
            labels
                .iter()
                .map(|l| l.summary.descriptor.eligibility.verified)
                .collect(),
        )
        .uint_or_null(
            "metaclusters",
            labels
                .iter()
                .map(|l| parameter(l, "metaclusters").and_then(serde_json::Value::as_u64))
                .collect(),
        )
        .text(
            "values_sha256",
            labels
                .iter()
                .map(|l| l.summary.values_sha256.clone())
                .collect(),
        )
}

fn parameter<'a>(table: &'a LabelTable, name: &str) -> Option<&'a serde_json::Value> {
    table
        .summary
        .descriptor
        .definition
        .as_ref()
        .and_then(|definition| definition.parameters.get(name))
}

/// The label columns of each table: filters, gates with their parents, and
/// probability columns with their classes.
pub(crate) fn columns(labels: &[LabelTable]) -> Columns {
    let rows: Vec<_> = labels
        .iter()
        .flat_map(|table| {
            table
                .summary
                .descriptor
                .labels
                .iter()
                .enumerate()
                .map(move |(position, column)| (table, position, column))
        })
        .collect();
    Columns::default()
        .text(
            "annotation_id",
            rows.iter()
                .map(|(t, ..)| t.summary.annotation_id.clone())
                .collect(),
        )
        .uint("position", rows.iter().map(|(_, p, _)| *p as u64).collect())
        .text(
            "label_column",
            rows.iter().map(|(.., c)| c.name.clone()).collect(),
        )
        .text_or_null(
            "parent",
            rows.iter().map(|(.., c)| c.parent.clone()).collect(),
        )
        .text_or_null(
            "class",
            rows.iter().map(|(.., c)| c.class.clone()).collect(),
        )
}

/// Common key columns of a count table.
fn keyed<T>(rows: &[(&LabelTable, T)]) -> Columns {
    Columns::default()
        .text(
            "annotation_id",
            rows.iter()
                .map(|(t, _)| t.summary.annotation_id.clone())
                .collect(),
        )
        .text(
            "acquisition_id",
            rows.iter()
                .map(|(t, _)| t.summary.descriptor.acquisition_id.clone())
                .collect(),
        )
        .text_or_null(
            "definition_id",
            rows.iter()
                .map(|(t, _)| t.summary.definition_id.clone())
                .collect(),
        )
        .text(
            "kind",
            rows.iter()
                .map(|(t, _)| t.summary.descriptor.kind.as_str().to_owned())
                .collect(),
        )
}

/// Events per state of each table.
pub(crate) fn states(labels: &[LabelTable]) -> Columns {
    let rows: Vec<_> = labels
        .iter()
        .flat_map(|t| t.summary.states.iter().map(move |state| (t, state)))
        .collect();
    keyed(&rows)
        .text(
            "state",
            rows.iter()
                .map(|(_, (s, _))| s.as_str().to_owned())
                .collect(),
        )
        .uint("count", rows.iter().map(|(_, (_, n))| **n).collect())
}

/// Events per (column, value, state) of each table.
pub(crate) fn counts(labels: &[LabelTable]) -> Columns {
    let rows: Vec<_> = labels
        .iter()
        .flat_map(|t| t.summary.counts.iter().map(move |count| (t, count)))
        .collect();
    keyed(&rows)
        .text(
            "label_column",
            rows.iter().map(|(_, c)| c.column.clone()).collect(),
        )
        .text_or_null(
            "label_value",
            rows.iter().map(|(_, c)| c.value.clone()).collect(),
        )
        .text(
            "state",
            rows.iter()
                .map(|(_, c)| c.state.as_str().to_owned())
                .collect(),
        )
        .uint("count", rows.iter().map(|(_, c)| c.count).collect())
}

/// The classes of each classifier table, in vocabulary order.
pub(crate) fn classes(labels: &[LabelTable]) -> Columns {
    let rows: Vec<_> = labels
        .iter()
        .flat_map(|t| {
            t.summary
                .descriptor
                .classes()
                .into_iter()
                .enumerate()
                .map(move |(position, class)| (t, position, class))
        })
        .collect();
    Columns::default()
        .text(
            "annotation_id",
            rows.iter()
                .map(|(t, ..)| t.summary.annotation_id.clone())
                .collect(),
        )
        .uint("position", rows.iter().map(|(_, p, _)| *p as u64).collect())
        .text("class", rows.iter().map(|(.., c)| c.clone()).collect())
}
