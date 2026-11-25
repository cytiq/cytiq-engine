use std::fs;
use std::path::Path;

use cytiq_publish::{Existing, Publication};
use plotly::common::Title;
use plotly::{Bar, Layout, Plot};

use crate::{TransformChannelMetrics, TransformError, TransformRunResult};

const REPORT_FILE_NAME: &str = "report.html";
const PLOT_HEIGHT: usize = 520;

/// Stage the debug artifacts with the outputs; every run replaces them.
pub(crate) fn stage_debug_artifacts(
    publication: &mut Publication,
    result: &TransformRunResult,
    debug_dir: &Path,
) -> Result<(), TransformError> {
    type Write = fn(&Path, &TransformRunResult) -> Result<(), TransformError>;
    let artifacts: [(&str, Write); 3] = [
        ("summary.json", write_summary_json),
        ("metrics.csv", write_metrics_csv),
        (REPORT_FILE_NAME, write_report_html),
    ];
    for (name, write) in artifacts {
        publication.stage_path_with(debug_dir.join(name), Existing::Replace, |path| {
            write(path, result)
        })?;
    }
    Ok(())
}

fn write_summary_json(path: &Path, result: &TransformRunResult) -> Result<(), TransformError> {
    let bytes =
        serde_json::to_vec_pretty(result).map_err(|source| TransformError::DebugJsonSerialize {
            path: path.to_path_buf(),
            source,
        })?;
    fs::write(path, bytes).map_err(|source| TransformError::DebugJsonWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn write_metrics_csv(path: &Path, result: &TransformRunResult) -> Result<(), TransformError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| TransformError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    writer
        .write_record([
            "input",
            "output",
            "method",
            "channel",
            "events",
            "raw_min",
            "raw_max",
            "raw_mean",
            "raw_standard_deviation",
            "transformed_min",
            "transformed_max",
            "transformed_mean",
            "transformed_standard_deviation",
            "raw_negative_count",
            "raw_zero_count",
        ])
        .map_err(|source| TransformError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;

    for file in &result.files {
        for metrics in &file.metrics {
            writer
                .write_record([
                    file.input_path.display().to_string(),
                    file.output_path.display().to_string(),
                    file.method.as_str().to_owned(),
                    metrics.channel.clone(),
                    metrics.events.to_string(),
                    format!("{:.12}", metrics.raw.min),
                    format!("{:.12}", metrics.raw.max),
                    format!("{:.12}", metrics.raw.mean),
                    format!("{:.12}", metrics.raw.standard_deviation),
                    format!("{:.12}", metrics.transformed.min),
                    format!("{:.12}", metrics.transformed.max),
                    format!("{:.12}", metrics.transformed.mean),
                    format!("{:.12}", metrics.transformed.standard_deviation),
                    metrics.raw_negative_count.to_string(),
                    metrics.raw_zero_count.to_string(),
                ])
                .map_err(|source| TransformError::CsvWrite {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
    }
    writer.flush().map_err(|source| TransformError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_report_html(path: &Path, result: &TransformRunResult) -> Result<(), TransformError> {
    let plots = report_plots(result);
    let file_count = result.files.len();
    let channel_count = result
        .files
        .iter()
        .map(|file| file.metrics.len())
        .sum::<usize>();
    let events = result.files.iter().map(|file| file.events).sum::<usize>();
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Transform Report</title>
{plotly_js}
<style>
body {{ font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 2rem; color: #1f2933; }}
table {{ border-collapse: collapse; margin-top: 1rem; }}
td, th {{ border: 1px solid #d9e2ec; padding: 0.45rem 0.65rem; text-align: left; }}
th {{ background: #f0f4f8; }}
code {{ background: #f0f4f8; padding: 0.1rem 0.25rem; }}
.plot-panel {{ margin-top: 1.5rem; border: 1px solid #d9e2ec; padding: 1rem; }}
.plot-panel h2 {{ margin-top: 0; }}
.js-plotly-plot, .plotly, .plotly-graph-div {{ width: 100% !important; }}
</style>
</head>
<body>
<h1>Transform Report</h1>
<table>
<tr><th>Metric</th><th>Value</th></tr>
<tr><td>Files</td><td>{file_count}</td></tr>
<tr><td>Transformed channels</td><td>{channel_count}</td></tr>
<tr><td>Total input events</td><td>{events}</td></tr>
</table>
<h2>Plots</h2>
{plots}
</body>
</html>
"#,
        plotly_js = Plot::online_cdn_js(),
        file_count = file_count,
        channel_count = channel_count,
        events = events,
        plots = plots
    );
    fs::write(path, html).map_err(|source| TransformError::HtmlWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn report_plots(result: &TransformRunResult) -> String {
    let metrics = flattened_metrics(result);
    if metrics.is_empty() {
        return "<p>No transformed channel metrics were recorded.</p>".to_owned();
    }
    [
        plot_panel(
            "Channel means before and after transformation",
            &mean_plot(&metrics).to_inline_html(Some("transform-channel-means")),
        ),
        plot_panel(
            "Channel standard deviations before and after transformation",
            &standard_deviation_plot(&metrics).to_inline_html(Some("transform-channel-sd")),
        ),
        plot_panel(
            "Raw zero and negative counts",
            &raw_count_plot(&metrics).to_inline_html(Some("transform-raw-counts")),
        ),
    ]
    .join("\n")
}

fn plot_panel(title: &str, plot_html: &str) -> String {
    format!(
        "<section class=\"plot-panel\"><h2>{}</h2>{}</section>",
        escape_html(title),
        plot_html
    )
}

fn mean_plot(metrics: &[(String, &TransformChannelMetrics)]) -> Plot {
    let x = metric_labels(metrics);
    let raw = metrics
        .iter()
        .map(|(_, metrics)| metrics.raw.mean)
        .collect::<Vec<_>>();
    let transformed = metrics
        .iter()
        .map(|(_, metrics)| metrics.transformed.mean)
        .collect::<Vec<_>>();
    let mut plot = Plot::new();
    plot.add_trace(Bar::new(x.clone(), raw).name("raw mean"));
    plot.add_trace(Bar::new(x, transformed).name("transformed mean"));
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Mean signal by channel"))
            .height(PLOT_HEIGHT),
    );
    plot
}

fn standard_deviation_plot(metrics: &[(String, &TransformChannelMetrics)]) -> Plot {
    let x = metric_labels(metrics);
    let raw = metrics
        .iter()
        .map(|(_, metrics)| metrics.raw.standard_deviation)
        .collect::<Vec<_>>();
    let transformed = metrics
        .iter()
        .map(|(_, metrics)| metrics.transformed.standard_deviation)
        .collect::<Vec<_>>();
    let mut plot = Plot::new();
    plot.add_trace(Bar::new(x.clone(), raw).name("raw standard deviation"));
    plot.add_trace(Bar::new(x, transformed).name("transformed standard deviation"));
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Standard deviation by channel"))
            .height(PLOT_HEIGHT),
    );
    plot
}

fn raw_count_plot(metrics: &[(String, &TransformChannelMetrics)]) -> Plot {
    let x = metric_labels(metrics);
    let negatives = metrics
        .iter()
        .map(|(_, metrics)| metrics.raw_negative_count)
        .collect::<Vec<_>>();
    let zeros = metrics
        .iter()
        .map(|(_, metrics)| metrics.raw_zero_count)
        .collect::<Vec<_>>();
    let mut plot = Plot::new();
    plot.add_trace(Bar::new(x.clone(), negatives).name("negative raw values"));
    plot.add_trace(Bar::new(x, zeros).name("zero raw values"));
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Raw zero and negative event counts"))
            .height(PLOT_HEIGHT),
    );
    plot
}

fn flattened_metrics(result: &TransformRunResult) -> Vec<(String, &TransformChannelMetrics)> {
    result
        .files
        .iter()
        .flat_map(|file| {
            let file_name = file
                .input_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("input");
            file.metrics
                .iter()
                .map(move |metrics| (format!("{file_name}:{}", metrics.channel), metrics))
        })
        .collect()
}

fn metric_labels(metrics: &[(String, &TransformChannelMetrics)]) -> Vec<String> {
    metrics.iter().map(|(label, _)| label.clone()).collect()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
