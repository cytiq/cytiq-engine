use askama::Template;
use std::fs;
use std::path::Path;

use cytiq_mask::EventMask;
use cytiq_publish::Publication;
use plotly::common::{Mode, Title};
use plotly::{Bar, Layout, Plot, Scatter};

use crate::error::PeacoQcError;
use crate::runner::{EventMaskRecord, PeacoQcSummary};

/// Write `PeacoQC` debug artifacts to `debug_dir`.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or any artifact cannot
/// be written.
pub(crate) fn stage_debug_artifacts(
    publication: &mut Publication,
    summary: &PeacoQcSummary,
    mask: &[EventMaskRecord],
    debug_dir: &Path,
) -> Result<(), PeacoQcError> {
    publication.stage_path(debug_dir.join("summary.json"), |path| {
        write_summary_json(path, summary)
    })?;
    publication.stage_path(debug_dir.join("mask.csv"), |path| {
        write_mask_csv(path, mask)
    })?;
    publication.stage_path(debug_dir.join("report.html"), |path| {
        write_report_html(path, summary, mask)
    })
}

/// One row per input event, in the schema of `cytiq clean`'s masks: `event_id`
/// is the 0-based input row, `source_event_id` the 0-based raw-file row (from
/// `Original_ID` when the input has one). A null filter column means the
/// filter did not assess the event, never that it passed.
pub(crate) fn stage_masks_parquet(
    publication: &mut Publication,
    path: &Path,
    acquisition: &str,
    mask: &[EventMaskRecord],
    source_event_ids: Option<&[u64]>,
    margins_assessed: bool,
) -> Result<(), PeacoQcError> {
    let margin_pass = mask
        .iter()
        .map(|record| margins_assessed.then_some(!record.removed_by_margin))
        .collect();
    let peacoqc_pass = mask
        .iter()
        .map(|record| {
            (!record.removed_by_margin).then_some(
                !(record.removed_by_mad
                    || record.removed_by_it
                    || record.removed_by_consecutive_bins),
            )
        })
        .collect();
    EventMask::new(
        acquisition,
        mask.iter().map(|record| record.is_good).collect(),
        source_event_ids.map(<[u64]>::to_vec),
    )
    .with_filter("margin_pass", margin_pass)
    .with_filter(
        "margin_lower_pass",
        mask.iter()
            .map(|r| margins_assessed.then_some(!r.removed_by_lower_margin))
            .collect(),
    )
    .with_filter(
        "margin_upper_pass",
        mask.iter()
            .map(|r| margins_assessed.then_some(!r.removed_by_upper_margin))
            .collect(),
    )
    .with_filter("peacoqc_pass", peacoqc_pass)
    .stage_parquet(publication, path)
    .map_err(|error| PeacoQcError::ParquetWrite {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn write_summary_json(path: &Path, summary: &PeacoQcSummary) -> Result<(), PeacoQcError> {
    let bytes =
        serde_json::to_vec_pretty(summary).map_err(|source| PeacoQcError::JsonSerialize {
            path: path.to_path_buf(),
            source,
        })?;
    fs::write(path, bytes).map_err(|source| PeacoQcError::JsonWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn write_mask_csv(path: &Path, mask: &[EventMaskRecord]) -> Result<(), PeacoQcError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| PeacoQcError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    for record in mask {
        writer
            .serialize(record)
            .map_err(|source| PeacoQcError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| PeacoQcError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_report_html(
    path: &Path,
    summary: &PeacoQcSummary,
    mask: &[EventMaskRecord],
) -> Result<(), PeacoQcError> {
    let html = PeacoQcReport {
        summary,
        channels: summary.channels.join(", "),
        plots: report_plots(summary, mask),
        plotly_js: PlotHtml(Plot::online_cdn_js()),
    }
    .render()
    .map_err(|error| PeacoQcError::HtmlWrite {
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    })?;
    fs::write(path, html).map_err(|source| PeacoQcError::HtmlWrite {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Template)]
#[template(path = "report.html")]
struct PeacoQcReport<'a> {
    summary: &'a PeacoQcSummary,
    channels: String,
    plots: Vec<PlotPanel>,
    plotly_js: PlotHtml,
}
struct PlotPanel {
    title: &'static str,
    plot: PlotHtml,
}
// Only generated Plotly fragments use this private wrapper and template |safe.
struct PlotHtml(String);
impl std::fmt::Display for PlotHtml {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
fn report_plots(summary: &PeacoQcSummary, mask: &[EventMaskRecord]) -> Vec<PlotPanel> {
    let mut panels = vec![
        PlotPanel {
            title: "Removal summary",
            plot: PlotHtml(removal_summary_plot(summary).to_inline_html(Some("removal-summary"))),
        },
        PlotPanel {
            title: "Removed bins",
            plot: PlotHtml(removed_bins_plot(summary).to_inline_html(Some("removed-bins"))),
        },
    ];
    if !mask.is_empty() {
        panels.push(PlotPanel {
            title: "Event mask",
            plot: PlotHtml(event_mask_plot(mask).to_inline_html(Some("event-mask"))),
        });
    }
    panels
}

fn removal_summary_plot(summary: &PeacoQcSummary) -> Plot {
    let metrics = &summary.metrics;
    let x = vec![
        "margins".to_owned(),
        "MAD".to_owned(),
        "IT".to_owned(),
        "consecutive".to_owned(),
    ];
    let y = vec![
        metrics.margin_removed_count,
        metrics.mad_removed_event_count,
        metrics.it_removed_event_count,
        metrics.consecutive_removed_event_count,
    ];
    let mut plot = Plot::new();
    plot.add_trace(Bar::new(x, y).name("removed events"));
    plot.set_layout(Layout::new().title(Title::from("Removed events by reason")));
    plot
}

fn removed_bins_plot(summary: &PeacoQcSummary) -> Plot {
    let metrics = &summary.metrics;
    let bin_ids = (1..=metrics.n_bins).collect::<Vec<_>>();
    let mad = bin_indicator(&bin_ids, &metrics.mad_outlier_bins);
    let it = bin_indicator(&bin_ids, &metrics.it_outlier_bins);
    let consecutive = bin_indicator(&bin_ids, &metrics.consecutive_removed_bins);

    let mut plot = Plot::new();
    plot.add_trace(
        Scatter::new(bin_ids.clone(), mad)
            .mode(Mode::LinesMarkers)
            .name("MAD"),
    );
    plot.add_trace(
        Scatter::new(bin_ids.clone(), it)
            .mode(Mode::LinesMarkers)
            .name("IT"),
    );
    plot.add_trace(
        Scatter::new(bin_ids, consecutive)
            .mode(Mode::LinesMarkers)
            .name("consecutive"),
    );
    plot.set_layout(Layout::new().title(Title::from("Removed bins over acquisition order")));
    plot
}

fn bin_indicator(bin_ids: &[usize], removed_bins: &[usize]) -> Vec<usize> {
    bin_ids
        .iter()
        .map(|bin_id| usize::from(removed_bins.contains(bin_id)))
        .collect()
}

fn event_mask_plot(mask: &[EventMaskRecord]) -> Plot {
    let sampled = sampled_mask(mask);
    let event_ids = sampled
        .iter()
        .map(|record| record.event_id)
        .collect::<Vec<_>>();
    let good = sampled
        .iter()
        .map(|record| usize::from(record.is_good))
        .collect::<Vec<_>>();
    let removed = sampled
        .iter()
        .map(|record| usize::from(!record.is_good))
        .collect::<Vec<_>>();

    let mut plot = Plot::new();
    plot.add_trace(
        Scatter::new(event_ids.clone(), good)
            .mode(Mode::Markers)
            .name("good"),
    );
    plot.add_trace(
        Scatter::new(event_ids, removed)
            .mode(Mode::Markers)
            .name("removed"),
    );
    plot.set_layout(Layout::new().title(Title::from("Sampled event mask")));
    plot
}

fn sampled_mask(mask: &[EventMaskRecord]) -> Vec<EventMaskRecord> {
    const MAX_POINTS: usize = 5_000;
    if mask.len() <= MAX_POINTS {
        return mask.to_vec();
    }
    let step = mask.len().div_ceil(MAX_POINTS);
    mask.iter().step_by(step).cloned().collect()
}
