//! `cytiq.mask.v2` tables: technical QC masks (`qc`) and gate membership
//! (`gate`). Columns are `event_id`, `source_event_id`, one nullable Boolean
//! per filter or gate, then `keep`.

use std::path::Path;

use arrow_array::{Array, BooleanArray};

use super::Rows;
use crate::labels::{LabelKind, LabelState};
use crate::native::{
    MASK_VERSION_KEY, boolean, check_acquisition, check_gate_row, open_table, uint,
};
use crate::{PopulationError, Result, require};

pub(crate) const CONTRACT: &str = "cytiq.mask.v2";

/// The filter or gate columns of the mask v2 table at `path`, which must
/// belong to `acquisition`, in table order.
pub(crate) fn filters(path: &Path, acquisition: &str) -> Result<Vec<String>> {
    let reader = open_table(path)?;
    let schema = reader.schema();
    check_acquisition(schema, acquisition)?;
    require(
        schema.metadata().get(MASK_VERSION_KEY).map(String::as_str) == Some("2"),
        "the native file is not a cytiq.mask.v2 table",
    )?;
    let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    require(
        names.len() >= 3
            && names[0] == cytiq_mask::EVENT_ID_COLUMN
            && names[1] == cytiq_mask::SOURCE_EVENT_ID_COLUMN
            && names[names.len() - 1] == cytiq_mask::KEEP_COLUMN,
        "mask columns must be `event_id`, `source_event_id`, the filters, then `keep`",
    )?;
    Ok(names[2..names.len() - 1]
        .iter()
        .map(|name| (*name).to_owned())
        .collect())
}

/// Stream the mask at `path` into `rows`. A `qc` row is accepted when kept
/// and QC-rejected otherwise; a `gate` row is assessed when inside the
/// producer's input mask, its gates null otherwise and members of their
/// parent (`parents`) when set.
pub(crate) fn stream(
    path: &Path,
    kind: LabelKind,
    parents: &[Option<usize>],
    rows: &mut Rows<'_>,
) -> Result<()> {
    let reader = open_table(path)?;
    let mut row = 0_u64;
    for batch in reader {
        let batch = batch?;
        let ids = uint(&batch, cytiq_mask::EVENT_ID_COLUMN)?;
        let sources = uint(&batch, cytiq_mask::SOURCE_EVENT_ID_COLUMN)?;
        let keep = boolean(&batch, cytiq_mask::KEEP_COLUMN)?;
        let columns = (2..batch.num_columns() - 1)
            .map(|index| {
                batch
                    .column(index)
                    .as_any()
                    .downcast_ref::<BooleanArray>()
                    .ok_or_else(|| {
                        PopulationError::Invalid("mask filters must be Boolean columns".into())
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        for i in 0..batch.num_rows() {
            require(
                !ids.is_null(i) && ids.value(i) == row && !sources.is_null(i) && !keep.is_null(i),
                "null or invalid mask event id",
            )?;
            row += 1;
            let id = sources.value(i);
            rows.at(id)?;
            let kept = keep.value(i);
            let state = match kind {
                LabelKind::Gate => {
                    check_gate_row(&columns, parents, i, kept)?;
                    if kept {
                        rows.assessed(id)?;
                        LabelState::Assessed
                    } else {
                        rows.excluded(id)?
                    }
                }
                _ if kept => LabelState::Accepted,
                _ => LabelState::QcRejected,
            };
            rows.push(state, |builders| {
                for (builder, column) in builders.iter_mut().zip(&columns) {
                    builder.boolean(column.is_valid(i).then(|| column.value(i)));
                }
            })?;
        }
    }
    Ok(())
}
