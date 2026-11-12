#![allow(
    clippy::cast_precision_loss,
    clippy::module_name_repetitions,
    clippy::too_many_lines
)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use plotly::common::{Mode, Title};
use plotly::{Bar, HeatMap, Layout, Plot, Scatter};

use crate::error::FlowSomError;
use crate::runner::{FlowSomAssignment, FlowSomMetacluster, FlowSomNode, FlowSomSummary};

const REPORT_FILE_NAME: &str = "report.html";
const GRID_PLOT_HEIGHT: usize = 620;
const BAR_PLOT_HEIGHT: usize = 520;
const ASSIGNMENT_PLOT_HEIGHT: usize = 520;
const ASSIGNMENT_SAMPLE_LIMIT: usize = 5_000;

/// The assignment CSV; `path` only names the target in errors. Callers
/// stage it through `cytiq-publish`, so no run leaves a truncated file.
pub(crate) fn write_assignments<W: std::io::Write>(
    writer: W,
    path: &Path,
    assignments: &[FlowSomAssignment],
) -> Result<(), FlowSomError> {
    let mut writer = csv::Writer::from_writer(writer);
    for assignment in assignments {
        writer
            .serialize(assignment)
            .map_err(|source| FlowSomError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| FlowSomError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn write_debug_artifacts(
    summary: &FlowSomSummary,
    assignments: &[FlowSomAssignment],
    nodes: &[FlowSomNode],
    metaclusters: &[FlowSomMetacluster],
    debug_dir: &Path,
) -> Result<(), FlowSomError> {
    fs::create_dir_all(debug_dir).map_err(|source| FlowSomError::DebugDirectoryCreate {
        path: debug_dir.to_path_buf(),
        source,
    })?;
    write_summary_json(&debug_dir.join("summary.json"), summary)?;
    write_nodes_csv(&debug_dir.join("nodes.csv"), summary, nodes)?;
    write_metaclusters_csv(&debug_dir.join("metaclusters.csv"), metaclusters)?;
    write_report_html(
        &debug_dir.join(REPORT_FILE_NAME),
        summary,
        assignments,
        nodes,
        metaclusters,
    )
}

fn write_summary_json(path: &Path, summary: &FlowSomSummary) -> Result<(), FlowSomError> {
    let bytes =
        serde_json::to_vec_pretty(summary).map_err(|source| FlowSomError::JsonSerialize {
            path: path.to_path_buf(),
            source,
        })?;
    fs::write(path, bytes).map_err(|source| FlowSomError::JsonWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn write_nodes_csv(
    path: &Path,
    summary: &FlowSomSummary,
    nodes: &[FlowSomNode],
) -> Result<(), FlowSomError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| FlowSomError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    let mut header = vec![
        "node_id".to_owned(),
        "x".to_owned(),
        "y".to_owned(),
        "metacluster".to_owned(),
        "event_count".to_owned(),
    ];
    header.extend(
        summary
            .resolved_channels
            .iter()
            .map(|channel| format!("codebook_{channel}")),
    );
    writer
        .write_record(header)
        .map_err(|source| FlowSomError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for node in nodes {
        let mut record = vec![
            node.node_id.to_string(),
            node.x.to_string(),
            node.y.to_string(),
            node.metacluster.to_string(),
            node.event_count.to_string(),
        ];
        record.extend(node.codebook.iter().map(|value| format!("{value:.12}")));
        writer
            .write_record(record)
            .map_err(|source| FlowSomError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| FlowSomError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_metaclusters_csv(
    path: &Path,
    metaclusters: &[FlowSomMetacluster],
) -> Result<(), FlowSomError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| FlowSomError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    for metacluster in metaclusters {
        writer
            .serialize(metacluster)
            .map_err(|source| FlowSomError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| FlowSomError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_report_html(
    path: &Path,
    summary: &FlowSomSummary,
    assignments: &[FlowSomAssignment],
    nodes: &[FlowSomNode],
    metaclusters: &[FlowSomMetacluster],
) -> Result<(), FlowSomError> {
    let plots = report_plots(assignments, nodes, metaclusters);
    let mut html = String::new();
    let _ = write!(
        html,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>FlowSOM Report</title>
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
<h1>FlowSOM Report</h1>
<p><strong>Status:</strong> {status}</p>
<p><strong>Input:</strong> <code>{input}</code></p>
<p><strong>Output:</strong> <code>{output}</code></p>
<p><strong>Channels:</strong> {channels}</p>
<table>
<tr><th>Metric</th><th>Value</th></tr>
<tr><td>Events</td><td>{events}</td></tr>
<tr><td>Channels</td><td>{channel_count}</td></tr>
<tr><td>SOM nodes</td><td>{nodes}</td></tr>
<tr><td>Metaclusters</td><td>{metaclusters}</td></tr>
<tr><td>Empty nodes</td><td>{empty_nodes}</td></tr>
</table>
<h2>Plots</h2>
{plots}
</body>
</html>
"#,
        plotly_js = Plot::online_cdn_js(),
        status = escape_html(&summary.status),
        input = escape_html(&summary.input),
        output = escape_html(&summary.output),
        channels = escape_html(&summary.resolved_channels.join(", ")),
        events = summary.metrics.events,
        channel_count = summary.metrics.channels,
        nodes = summary.metrics.som_nodes,
        metaclusters = summary.metrics.metaclusters,
        empty_nodes = summary.metrics.empty_nodes,
        plots = plots,
    );
    fs::write(path, html).map_err(|source| FlowSomError::HtmlWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn report_plots(
    assignments: &[FlowSomAssignment],
    nodes: &[FlowSomNode],
    metaclusters: &[FlowSomMetacluster],
) -> String {
    [
        plot_panel(
            "Node event counts",
            &node_event_count_heatmap(nodes).to_inline_html(Some("node-event-counts")),
        ),
        plot_panel(
            "Node metaclusters",
            &node_metacluster_heatmap(nodes).to_inline_html(Some("node-metaclusters")),
        ),
        plot_panel(
            "Metacluster event counts",
            &metacluster_event_bar(metaclusters).to_inline_html(Some("metacluster-events")),
        ),
        plot_panel(
            "Node assignment trace",
            &node_assignment_trace(assignments).to_inline_html(Some("node-assignment-trace")),
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

fn node_event_count_heatmap(nodes: &[FlowSomNode]) -> Plot {
    let mut plot = Plot::new();
    plot.add_trace(HeatMap::new(
        grid_x_labels(nodes),
        grid_y_labels(nodes),
        node_grid(nodes, |node| node.event_count as f64),
    ));
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Events per SOM node"))
            .height(GRID_PLOT_HEIGHT),
    );
    plot
}

fn node_metacluster_heatmap(nodes: &[FlowSomNode]) -> Plot {
    let mut plot = Plot::new();
    plot.add_trace(HeatMap::new(
        grid_x_labels(nodes),
        grid_y_labels(nodes),
        node_grid(nodes, |node| node.metacluster as f64),
    ));
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("SOM node metaclusters"))
            .height(GRID_PLOT_HEIGHT),
    );
    plot
}

fn metacluster_event_bar(metaclusters: &[FlowSomMetacluster]) -> Plot {
    let x = metaclusters
        .iter()
        .map(|metacluster| metacluster.metacluster_id.to_string())
        .collect::<Vec<_>>();
    let y = metaclusters
        .iter()
        .map(|metacluster| metacluster.event_count)
        .collect::<Vec<_>>();
    let mut plot = Plot::new();
    plot.add_trace(Bar::new(x, y).name("events"));
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Events per metacluster"))
            .height(BAR_PLOT_HEIGHT),
    );
    plot
}

fn node_assignment_trace(assignments: &[FlowSomAssignment]) -> Plot {
    let sampled = assignments
        .iter()
        .take(ASSIGNMENT_SAMPLE_LIMIT)
        .collect::<Vec<_>>();
    let x = sampled
        .iter()
        .map(|assignment| assignment.event_index)
        .collect::<Vec<_>>();
    let y = sampled
        .iter()
        .map(|assignment| assignment.flowsom_node)
        .collect::<Vec<_>>();
    let mut plot = Plot::new();
    plot.add_trace(Scatter::new(x, y).mode(Mode::Markers).name("assigned node"));
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Sampled event-to-node assignments"))
            .height(ASSIGNMENT_PLOT_HEIGHT),
    );
    plot
}

/// SOM grid size, from the 1-based node coordinates.
fn grid_size(nodes: &[FlowSomNode]) -> (usize, usize) {
    (
        nodes.iter().map(|node| node.x).max().unwrap_or(0),
        nodes.iter().map(|node| node.y).max().unwrap_or(0),
    )
}

fn node_grid(nodes: &[FlowSomNode], value: impl Fn(&FlowSomNode) -> f64) -> Vec<Vec<f64>> {
    let (xdim, ydim) = grid_size(nodes);
    let mut grid = vec![vec![0.0; xdim]; ydim];
    for node in nodes {
        grid[node.y - 1][node.x - 1] = value(node);
    }
    grid
}

fn grid_x_labels(nodes: &[FlowSomNode]) -> Vec<String> {
    (1..=grid_size(nodes).0)
        .map(|value| value.to_string())
        .collect()
}

fn grid_y_labels(nodes: &[FlowSomNode]) -> Vec<String> {
    (1..=grid_size(nodes).1)
        .map(|value| value.to_string())
        .collect()
}

fn escape_html(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
