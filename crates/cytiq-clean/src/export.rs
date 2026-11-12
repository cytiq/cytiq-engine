use std::io::Write as _;
use std::path::Path;

use cytiq_fcs::FcsDataset;
use cytiq_gating::{GatingResults, GatingStrategy};
use cytiq_mask::EventMask;
use cytiq_publish::{Existing, Publication};

use crate::error::CleanError;
use crate::filter::{LIVE_GATE, NON_DEBRIS_GATE, SINGLETS_GATE};
use crate::report::report_html;
use crate::runner::{CleanConfig, CleanMasks, CleanMetrics};

pub const MASKS_FILE: &str = "masks.parquet";
pub const METRICS_FILE: &str = "metrics.json";
pub const GATINGML_FILE: &str = "gates.gatingml.xml";
pub const REPORT_FILE: &str = "report.html";

/// Masks, metrics, Gating-ML and report are published together, the report
/// last, so a directory never holds outputs of two runs.
pub(crate) fn write_outputs(
    config: &CleanConfig,
    metrics: &CleanMetrics,
    masks: &CleanMasks,
    gatingml: &str,
    strategy: &GatingStrategy,
    dataset: &FcsDataset,
    results: &GatingResults,
) -> Result<(), CleanError> {
    let directory = config.output.as_path();
    let mut publication = Publication::new(Existing::overwrite(config.overwrite));
    stage_masks_parquet(&mut publication, &directory.join(MASKS_FILE), masks)?;
    stage_metrics_json(&mut publication, &directory.join(METRICS_FILE), metrics)?;
    stage_text(&mut publication, &directory.join(GATINGML_FILE), gatingml)?;
    let report = report_html(
        &directory.join(REPORT_FILE),
        metrics,
        strategy,
        dataset,
        results,
    )?;
    stage_text(&mut publication, &directory.join(REPORT_FILE), &report)?;
    Ok(publication.commit()?)
}

/// One row per input event. `source_event_id` is the raw-file row (equal to
/// `event_id` unless the input has `Original_ID`). Filter columns are null when
/// the filter was not assessed, so "not checked" is never confused with
/// "passed".
fn stage_masks_parquet(
    publication: &mut Publication,
    path: &Path,
    masks: &CleanMasks,
) -> Result<(), CleanError> {
    let event_count = masks.keep.len();
    let optional = |mask: &Option<Vec<bool>>| match mask {
        Some(mask) => mask.iter().copied().map(Some).collect(),
        None => vec![None; event_count],
    };
    EventMask::new(
        masks.acquisition.clone(),
        masks.keep.clone(),
        masks.source_event_ids.clone(),
    )
    .with_filter(NON_DEBRIS_GATE, optional(&masks.non_debris))
    .with_filter(SINGLETS_GATE, optional(&masks.singlets))
    .with_filter(LIVE_GATE, optional(&masks.live))
    .stage_parquet(publication, path)
    .map_err(|error| CleanError::ParquetWrite {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn stage_metrics_json(
    publication: &mut Publication,
    path: &Path,
    metrics: &CleanMetrics,
) -> Result<(), CleanError> {
    let mut bytes = serde_json::to_vec_pretty(metrics).map_err(|source| CleanError::JsonWrite {
        path: path.to_path_buf(),
        source,
    })?;
    bytes.push(b'\n');
    stage_text_bytes(publication, path, &bytes)
}

fn stage_text(publication: &mut Publication, path: &Path, text: &str) -> Result<(), CleanError> {
    stage_text_bytes(publication, path, text.as_bytes())
}

fn stage_text_bytes(
    publication: &mut Publication,
    path: &Path,
    bytes: &[u8],
) -> Result<(), CleanError> {
    publication.stage_file(path, |writer| {
        writer
            .write_all(bytes)
            .map_err(|source| CleanError::OutputWrite {
                path: path.to_path_buf(),
                source,
            })
    })
}
