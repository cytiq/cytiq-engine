use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use cytiq_publish::Publication;
use log::{debug, info};
use plotly::common::{Mode, Title};
use plotly::{HeatMap, Layout, Plot, Scatter};
use serde::Serialize;

use crate::error::AutospillError;
use crate::regression::{
    ordinary_linear_regression, ordinary_linear_regression_without_intercept,
    robust_standard_deviation,
};
use crate::runner::{
    AutospillResult, AutospreadResult, ControlData, LoadedControls, quantile_bin_count,
    quantile_bins, sqrt_like_transform,
};
use cytiq_density::GateComputation;

const REPORT_FILE_NAME: &str = "report.html";
const GATE_PLOT_HEIGHT: usize = 760;
const CONVERGENCE_PLOT_HEIGHT: usize = 640;
const HEATMAP_PLOT_HEIGHT: usize = 860;
const REGRESSION_PLOT_HEIGHT: usize = 760;
const REPORT_CSS: &str = r#"
body {
  margin: 0;
  background: #eef3f8;
  color: #193247;
  font-family: "Helvetica Neue", Arial, sans-serif;
}

a {
  color: inherit;
}

.report-shell {
  max-width: 1240px;
  margin: 0 auto;
  padding: 24px;
}

.report-hero {
  background: linear-gradient(135deg, #134f7c 0%, #1c7eb0 100%);
  color: #f8fbff;
  border-radius: 24px;
  padding: 28px 32px;
  box-shadow: 0 18px 44px rgba(19, 79, 124, 0.18);
}

.report-eyebrow {
  text-transform: uppercase;
  letter-spacing: 0.16em;
  font-size: 12px;
  font-weight: 700;
  opacity: 0.82;
}

.report-hero h1 {
  margin: 10px 0 8px;
  font-size: 34px;
  line-height: 1.1;
}

.report-subtitle {
  margin: 0;
  max-width: 860px;
  font-size: 15px;
  line-height: 1.6;
  color: rgba(248, 251, 255, 0.9);
}

.report-nav {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  margin-top: 22px;
}

.report-nav a {
  display: inline-flex;
  align-items: center;
  padding: 9px 14px;
  border-radius: 999px;
  border: 1px solid rgba(255, 255, 255, 0.2);
  background: rgba(255, 255, 255, 0.08);
  text-decoration: none;
  font-size: 13px;
  font-weight: 600;
}

.report-section {
  margin-top: 24px;
  background: #ffffff;
  border: 1px solid #d6e2ef;
  border-radius: 22px;
  padding: 26px 28px;
  box-shadow: 0 14px 32px rgba(28, 57, 84, 0.08);
}

.report-section h2 {
  margin: 0 0 8px;
  font-size: 25px;
  color: #12344f;
}

.section-description {
  margin: 0 0 20px;
  color: #597288;
  line-height: 1.6;
}

.metric-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
  gap: 14px;
  margin-bottom: 22px;
}

.metric-card {
  border: 1px solid #d8e5f2;
  border-radius: 18px;
  padding: 16px 18px;
  background: linear-gradient(180deg, #f8fbfe 0%, #f1f6fb 100%);
}

.metric-label {
  display: block;
  font-size: 12px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: #6f879a;
}

.metric-value {
  display: block;
  margin-top: 8px;
  font-size: 28px;
  line-height: 1.1;
  color: #10324c;
}

.table-block + .table-block {
  margin-top: 20px;
}

.table-block h3 {
  margin: 0 0 10px;
  font-size: 18px;
  color: #173750;
}

.report-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 14px;
}

.report-table thead th {
  text-align: left;
  padding: 10px 12px;
  background: #edf4fb;
  color: #416177;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  font-size: 11px;
}

.report-table tbody td {
  padding: 10px 12px;
  border-top: 1px solid #e5eef6;
  vertical-align: top;
}

.report-table tbody tr:nth-child(even) td {
  background: #fbfdff;
}

.panel-stack {
  display: grid;
  gap: 22px;
}

.plot-panel {
  border: 1px solid #dbe7f3;
  border-radius: 18px;
  padding: 18px;
  background: #fcfdff;
}

.plot-panel h3 {
  margin: 0 0 8px;
  font-size: 18px;
  color: #173750;
}

.plot-panel p {
  margin: 0 0 16px;
  color: #627b91;
  line-height: 1.6;
}

.js-plotly-plot,
.plotly,
.plotly-graph-div {
  width: 100% !important;
}

.plot-panel .js-plotly-plot,
.plot-panel .plotly,
.plot-panel .plotly-graph-div {
  min-height: 720px !important;
}

@media (max-width: 900px) {
  .plot-panel .js-plotly-plot,
  .plot-panel .plotly,
  .plot-panel .plotly-graph-div {
    min-height: 540px !important;
  }
}
"#;

#[derive(Clone, Debug, Serialize)]
struct ArtifactSummary {
    kind: String,
    file: String,
}

#[derive(Clone, Debug, Serialize)]
struct DebugSummary<T> {
    mode: &'static str,
    artifacts: Vec<ArtifactSummary>,
    gate_stats: Vec<crate::runner::GateStats>,
    extra: T,
}

#[derive(Clone, Debug, Serialize)]
struct AutospillExtra {
    convergence: crate::runner::ConvergenceSummary,
    primary_channels: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct AutospreadExtra {
    spread_stats: Vec<crate::runner::SpreadStats>,
    primary_channels: Vec<String>,
}

struct ReportSection {
    id: String,
    title: String,
    description: Option<String>,
    body_html: Option<String>,
    panels: Vec<ReportPanel>,
}

struct ReportPanel {
    title: String,
    note: Option<String>,
    plot: Plot,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn stage_autospill_debug_artifacts(
    publication: &mut Publication,
    debug_dir: &Path,
    loaded: &LoadedControls,
    gates: &[GateComputation],
    result: &AutospillResult,
) -> Result<(), AutospillError> {
    info!(
        "staging autospill debug artifacts for {}",
        debug_dir.display()
    );

    let mut sections = Vec::new();
    sections.push(ReportSection {
        id: "summary".to_owned(),
        title: "Summary".to_owned(),
        description: Some(
            "Run overview, gating statistics, and convergence status for the current AutoSpill job."
                .to_owned(),
        ),
        body_html: Some(render_autospill_summary(result)),
        panels: Vec::new(),
    });
    sections.push(ReportSection {
        id: "convergence".to_owned(),
        title: "Convergence".to_owned(),
        description: Some(
            "Iteration-by-iteration refinement diagnostics in linear and transformed space."
                .to_owned(),
        ),
        body_html: Some(render_convergence_body(result)),
        panels: vec![ReportPanel {
            title: "Refinement trace".to_owned(),
            note: Some(
                "Tracks delta, delta_max, and delta_change across the AutoSpill update loop."
                    .to_owned(),
            ),
            plot: convergence_plot(result),
        }],
    });
    sections.push(ReportSection {
        id: "spillover-matrix".to_owned(),
        title: "Spillover Matrix".to_owned(),
        description: Some(
            "Estimated spillover coefficients with dyes on rows and detectors on columns."
                .to_owned(),
        ),
        body_html: None,
        panels: vec![ReportPanel {
            title: "Spillover heatmap".to_owned(),
            note: None,
            plot: heatmap_plot(
                "Spillover matrix",
                &result.dyes,
                &result.detectors,
                &result.spillover_matrix,
            ),
        }],
    });
    sections.push(ReportSection {
        id: "compensation-matrix".to_owned(),
        title: "Compensation Matrix".to_owned(),
        description: Some(
            "Inverse or pseudo-inverse used for square compensation or rectangular unmixing."
                .to_owned(),
        ),
        body_html: None,
        panels: vec![ReportPanel {
            title: "Compensation heatmap".to_owned(),
            note: None,
            plot: heatmap_plot(
                "Compensation matrix",
                &result.detectors,
                &result.dyes,
                &result.compensation_matrix,
            ),
        }],
    });
    sections.push(ReportSection {
        id: "gates".to_owned(),
        title: "Gates".to_owned(),
        description: Some(
            "FSC/SSC auto-gating per control. All events are shown together with the accepted gated population."
                .to_owned(),
        ),
        body_html: None,
        panels: loaded
            .controls
            .iter()
            .zip(gates)
            .map(|(control, gate)| ReportPanel {
                title: format!("{} ({})", control.row.dye, control.primary_channel),
                note: Some("Gray points are all events; blue points are events kept after gating.".to_owned()),
                plot: gate_plot(control, gate),
            })
            .collect(),
    });
    sections.push(ReportSection {
        id: "regression".to_owned(),
        title: "Spillover Regression".to_owned(),
        description: Some(
            "Per-control detector regressions used to estimate spillover and inspect outlier structure."
                .to_owned(),
        ),
        body_html: None,
        panels: loaded
            .controls
            .iter()
            .zip(gates)
            .flat_map(|(control, gate)| autospill_scatter_panels(control, gate, result))
            .collect(),
    });

    publication.stage_path(debug_dir.join(REPORT_FILE_NAME), |path| {
        write_report(
            path,
            "cytiq AutoSpill Report",
            "Single-page debug report with run metrics first, followed by charts in reading order.",
            "AutoSpill",
            &sections,
        )
    })?;
    debug!(
        "wrote autospill report with {} sections and {} controls",
        sections.len(),
        loaded.controls.len()
    );

    let summary = DebugSummary {
        mode: "autospill",
        artifacts: vec![ArtifactSummary {
            kind: "report".to_owned(),
            file: REPORT_FILE_NAME.to_owned(),
        }],
        gate_stats: result.gate_stats.clone(),
        extra: AutospillExtra {
            convergence: result.convergence.clone(),
            primary_channels: result.primary_channels.clone(),
        },
    };
    publication.stage_path(debug_dir.join("summary.json"), |path| {
        write_json(path, &summary)
    })?;
    info!("autospill debug artifacts ready in {}", debug_dir.display());
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn stage_autospread_debug_artifacts(
    publication: &mut Publication,
    debug_dir: &Path,
    loaded: &LoadedControls,
    gates: &[GateComputation],
    result: &AutospreadResult,
) -> Result<(), AutospillError> {
    info!(
        "staging autospread debug artifacts for {}",
        debug_dir.display()
    );

    let mut sections = Vec::new();
    sections.push(ReportSection {
        id: "summary".to_owned(),
        title: "Summary".to_owned(),
        description: Some(
            "Run overview, gating statistics, and spread estimation summaries for the current AutoSpread job."
                .to_owned(),
        ),
        body_html: Some(render_autospread_summary(result)),
        panels: Vec::new(),
    });
    sections.push(ReportSection {
        id: "spread-matrix".to_owned(),
        title: "Spread Matrix".to_owned(),
        description: Some(
            "Estimated spillover spreading matrix with dyes on rows and detectors on columns."
                .to_owned(),
        ),
        body_html: None,
        panels: vec![ReportPanel {
            title: "Spread heatmap".to_owned(),
            note: None,
            plot: heatmap_plot(
                "Spread matrix",
                &result.dyes,
                &result.detectors,
                &result.spread_matrix,
            ),
        }],
    });
    sections.push(ReportSection {
        id: "gates".to_owned(),
        title: "Gates".to_owned(),
        description: Some(
            "FSC/SSC auto-gating per control before AutoSpread binning and regression.".to_owned(),
        ),
        body_html: None,
        panels: loaded
            .controls
            .iter()
            .zip(gates)
            .map(|(control, gate)| ReportPanel {
                title: format!("{} ({})", control.row.dye, control.primary_channel),
                note: Some(
                    "Gray points are all events; blue points are events kept after gating."
                        .to_owned(),
                ),
                plot: gate_plot(control, gate),
            })
            .collect(),
    });
    sections.push(ReportSection {
        id: "spread-regression".to_owned(),
        title: "Spread Regression".to_owned(),
        description: Some(
            "Quantile-bin sigma regressions per control/detector pair used to derive spread coefficients."
                .to_owned(),
        ),
        body_html: None,
        panels: loaded
            .controls
            .iter()
            .zip(gates)
            .flat_map(|(control, gate)| autospread_panels(control, gate, result))
            .collect(),
    });

    publication.stage_path(debug_dir.join(REPORT_FILE_NAME), |path| {
        write_report(
            path,
            "cytiq AutoSpread Report",
            "Single-page debug report with run metrics first, followed by charts in reading order.",
            "AutoSpread",
            &sections,
        )
    })?;
    debug!(
        "wrote autospread report with {} sections and {} controls",
        sections.len(),
        loaded.controls.len()
    );

    let summary = DebugSummary {
        mode: "autospread",
        artifacts: vec![ArtifactSummary {
            kind: "report".to_owned(),
            file: REPORT_FILE_NAME.to_owned(),
        }],
        gate_stats: result.gate_stats.clone(),
        extra: AutospreadExtra {
            spread_stats: result.spread_stats.clone(),
            primary_channels: result.primary_channels.clone(),
        },
    };
    publication.stage_path(debug_dir.join("summary.json"), |path| {
        write_json(path, &summary)
    })?;
    info!(
        "autospread debug artifacts ready in {}",
        debug_dir.display()
    );
    Ok(())
}

fn gate_plot(control: &ControlData, gate: &GateComputation) -> Plot {
    let all_x = control
        .scatter
        .iter()
        .map(|point| point[0])
        .collect::<Vec<_>>();
    let all_y = control
        .scatter
        .iter()
        .map(|point| point[1])
        .collect::<Vec<_>>();
    let gated_x = gate
        .gated_indices
        .iter()
        .map(|&index| control.scatter[index][0])
        .collect::<Vec<_>>();
    let gated_y = gate
        .gated_indices
        .iter()
        .map(|&index| control.scatter[index][1])
        .collect::<Vec<_>>();

    let mut plot = Plot::new();
    plot.add_trace(
        Scatter::new(all_x, all_y)
            .mode(Mode::Markers)
            .name("all events"),
    );
    plot.add_trace(
        Scatter::new(gated_x, gated_y)
            .mode(Mode::Markers)
            .name("gated events"),
    );
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("FSC/SSC gate"))
            .height(GATE_PLOT_HEIGHT),
    );
    plot
}

fn convergence_plot(result: &AutospillResult) -> Plot {
    let iterations = result
        .convergence
        .steps
        .iter()
        .map(|step| step.iteration)
        .collect::<Vec<_>>();
    let delta = result
        .convergence
        .steps
        .iter()
        .map(|step| step.delta)
        .collect::<Vec<_>>();
    let delta_max = result
        .convergence
        .steps
        .iter()
        .map(|step| step.delta_max)
        .collect::<Vec<_>>();
    let delta_change = result
        .convergence
        .steps
        .iter()
        .map(|step| step.delta_change)
        .collect::<Vec<_>>();

    let mut plot = Plot::new();
    plot.add_trace(
        Scatter::new(iterations.clone(), delta)
            .mode(Mode::LinesMarkers)
            .name("delta"),
    );
    plot.add_trace(
        Scatter::new(iterations.clone(), delta_max)
            .mode(Mode::LinesMarkers)
            .name("delta_max"),
    );
    plot.add_trace(
        Scatter::new(iterations, delta_change)
            .mode(Mode::LinesMarkers)
            .name("delta_change"),
    );
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Convergence"))
            .height(CONVERGENCE_PLOT_HEIGHT),
    );
    plot
}

fn heatmap_plot(
    title: &str,
    row_labels: &[String],
    col_labels: &[String],
    matrix: &[Vec<f64>],
) -> Plot {
    let mut plot = Plot::new();
    plot.add_trace(HeatMap::new(
        col_labels.to_vec(),
        row_labels.to_vec(),
        matrix.to_vec(),
    ));
    plot.set_layout(
        Layout::new()
            .title(Title::with_text(title))
            .height(HEATMAP_PLOT_HEIGHT),
    );
    plot
}

fn autospill_scatter_panels(
    control: &ControlData,
    gate: &GateComputation,
    result: &AutospillResult,
) -> Vec<ReportPanel> {
    let primary = gate
        .gated_indices
        .iter()
        .map(|&index| control.detector_events[(index, control.primary_detector_index)])
        .collect::<Vec<_>>();
    let x_min = primary.iter().copied().fold(f64::INFINITY, f64::min);
    let x_max = primary.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    (0..result.detectors.len())
        .filter(|detector_index| *detector_index != control.primary_detector_index)
        .map(|detector_index| {
            let secondary = gate
                .gated_indices
                .iter()
                .map(|&index| control.detector_events[(index, detector_index)])
                .collect::<Vec<_>>();
            let coefficient = result.spillover_matrix[control.dye_index][detector_index];
            let fit_x = vec![x_min, x_max];
            let fit_y = fit_x
                .iter()
                .map(|value| coefficient * value)
                .collect::<Vec<_>>();

            let mut plot = Plot::new();
            plot.add_trace(
                Scatter::new(primary.clone(), secondary)
                    .mode(Mode::Markers)
                    .name("gated events"),
            );
            plot.add_trace(
                Scatter::new(fit_x, fit_y)
                    .mode(Mode::Lines)
                    .name("spillover fit"),
            );
            plot.set_layout(
                Layout::new()
                    .title(Title::with_text(format!(
                        "{} -> {}",
                        control.row.dye, result.detectors[detector_index]
                    )))
                    .height(REGRESSION_PLOT_HEIGHT),
            );
            ReportPanel {
                title: format!(
                    "{} -> {}",
                    control.row.dye, result.detectors[detector_index]
                ),
                note: Some(format!("Estimated spillover coefficient: {coefficient:.6}")),
                plot,
            }
        })
        .collect()
}

fn autospread_panels(
    control: &ControlData,
    gate: &GateComputation,
    result: &AutospreadResult,
) -> Vec<ReportPanel> {
    let primary_values = gate
        .gated_indices
        .iter()
        .map(|&index| control.detector_events[(index, control.primary_detector_index)])
        .collect::<Vec<_>>();
    let bins = quantile_bins(&primary_values, quantile_bin_count(primary_values.len()));

    (0..result.detectors.len())
        .filter(|detector_index| *detector_index != control.primary_detector_index)
        .filter_map(|detector_index| {
            let secondary_values = gate
                .gated_indices
                .iter()
                .map(|&index| control.detector_events[(index, detector_index)])
                .collect::<Vec<_>>();

            let mut fluorescence = Vec::new();
            let mut sigma = Vec::new();
            for bin in &bins {
                let primary_bin = bin
                    .iter()
                    .map(|&index| primary_values[index])
                    .collect::<Vec<_>>();
                let secondary_bin = bin
                    .iter()
                    .map(|&index| secondary_values[index])
                    .collect::<Vec<_>>();
                if primary_bin.len() < 3 || secondary_bin.len() < 3 {
                    continue;
                }
                fluorescence.push(crate::regression::median(&primary_bin));
                sigma.push(robust_standard_deviation(&secondary_bin));
            }

            if fluorescence.len() < 2 {
                return None;
            }

            let x = fluorescence
                .iter()
                .map(|value| sqrt_like_transform(*value))
                .collect::<Vec<_>>();
            let first_regression = ordinary_linear_regression(&x, &sigma)?;
            let sigma0 = first_regression.intercept.max(0.0);
            let adjusted_sigma = sigma
                .iter()
                .map(|value| sqrt_like_transform((value * value - sigma0 * sigma0).max(0.0)))
                .collect::<Vec<_>>();
            let spread = ordinary_linear_regression_without_intercept(&x, &adjusted_sigma)
                .map_or(0.0, |summary| summary.slope);

            let x_min = x.iter().copied().fold(f64::INFINITY, f64::min);
            let x_max = x.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let fit_x = vec![x_min, x_max];
            let fit_y = fit_x
                .iter()
                .map(|value| first_regression.intercept + first_regression.slope * value)
                .collect::<Vec<_>>();

            let mut plot = Plot::new();
            plot.add_trace(Scatter::new(x, sigma).mode(Mode::Markers).name("bin sigma"));
            plot.add_trace(
                Scatter::new(fit_x, fit_y)
                    .mode(Mode::Lines)
                    .name("sigma0 fit"),
            );
            plot.set_layout(
                Layout::new()
                    .title(Title::with_text(format!(
                        "{} -> {}",
                        control.row.dye, result.detectors[detector_index]
                    )))
                    .height(REGRESSION_PLOT_HEIGHT),
            );
            Some(ReportPanel {
                title: format!(
                    "{} -> {}",
                    control.row.dye, result.detectors[detector_index]
                ),
                note: Some(format!(
                    "Estimated spread coefficient: {:.6}",
                    spread.max(0.0)
                )),
                plot,
            })
        })
        .collect()
}

fn write_report(
    path: &Path,
    title: &str,
    subtitle: &str,
    eyebrow: &str,
    sections: &[ReportSection],
) -> Result<(), AutospillError> {
    debug!("rendering HTML report to {}", path.display());
    let mut html = String::new();
    let _ = write!(
        html,
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title><style>{}</style>{}</head><body><div class=\"report-shell\"><header class=\"report-hero\"><div class=\"report-eyebrow\">{}</div><h1>{}</h1><p class=\"report-subtitle\">{}</p><nav class=\"report-nav\">",
        html_escape(title),
        REPORT_CSS,
        Plot::online_cdn_js(),
        html_escape(eyebrow),
        html_escape(title),
        html_escape(subtitle),
    );

    for section in sections {
        let _ = write!(
            html,
            "<a href=\"#{}\">{}</a>",
            html_escape(&section.id),
            html_escape(&section.title)
        );
    }
    html.push_str("</nav></header><main>");

    let mut plot_index = 0_usize;
    for section in sections {
        let _ = write!(
            html,
            "<section class=\"report-section\" id=\"{}\"><h2>{}</h2>",
            html_escape(&section.id),
            html_escape(&section.title)
        );
        if let Some(description) = &section.description {
            let _ = write!(
                html,
                "<p class=\"section-description\">{}</p>",
                html_escape(description)
            );
        }
        if let Some(body_html) = &section.body_html {
            html.push_str(body_html);
        }
        if !section.panels.is_empty() {
            html.push_str("<div class=\"panel-stack\">");
            for panel in &section.panels {
                let _ = write!(
                    html,
                    "<article class=\"plot-panel\"><h3>{}</h3>",
                    html_escape(&panel.title)
                );
                if let Some(note) = &panel.note {
                    let _ = write!(html, "<p>{}</p>", html_escape(note));
                }
                let plot_id = format!("plot-{plot_index}");
                html.push_str(&panel.plot.to_inline_html(Some(&plot_id)));
                html.push_str("</article>");
                plot_index += 1;
            }
            html.push_str("</div>");
        }
        html.push_str("</section>");
    }

    html.push_str("</main></div></body></html>");
    fs::write(path, html).map_err(|source| AutospillError::DebugWrite {
        path: path.to_path_buf(),
        source,
    })?;
    debug!("wrote HTML report to {}", path.display());
    Ok(())
}

fn render_autospill_summary(result: &AutospillResult) -> String {
    let gated_events = result
        .gate_stats
        .iter()
        .map(|stats| stats.gated_events)
        .sum::<usize>();
    let cards = render_metric_cards(&[
        ("Dyes", result.dyes.len().to_string()),
        ("Detectors", result.detectors.len().to_string()),
        ("Controls", result.gate_stats.len().to_string()),
        ("Gated Events", gated_events.to_string()),
        (
            "Converged",
            if result.convergence.converged {
                "yes".to_owned()
            } else {
                "no".to_owned()
            },
        ),
        ("Iterations", result.convergence.iterations.to_string()),
    ]);

    format!("{}{}", cards, render_gate_stats_table(&result.gate_stats))
}

fn render_autospread_summary(result: &AutospreadResult) -> String {
    let gated_events = result
        .gate_stats
        .iter()
        .map(|stats| stats.gated_events)
        .sum::<usize>();
    let non_zero_coefficients = result
        .spread_matrix
        .iter()
        .flatten()
        .filter(|value| value.abs() > f64::EPSILON)
        .count();
    let cards = render_metric_cards(&[
        ("Dyes", result.dyes.len().to_string()),
        ("Detectors", result.detectors.len().to_string()),
        ("Controls", result.gate_stats.len().to_string()),
        ("Gated Events", gated_events.to_string()),
        ("Non-zero Coefficients", non_zero_coefficients.to_string()),
        (
            "Primary Channels",
            result.primary_channels.len().to_string(),
        ),
    ]);

    format!(
        "{}{}{}",
        cards,
        render_gate_stats_table(&result.gate_stats),
        render_spread_stats_table(&result.spread_stats)
    )
}

fn render_convergence_body(result: &AutospillResult) -> String {
    let cards = render_metric_cards(&[
        (
            "Final delta_max",
            format!("{:.6}", result.convergence.final_delta_max),
        ),
        ("Iterations", result.convergence.iterations.to_string()),
        (
            "Converged",
            if result.convergence.converged {
                "yes".to_owned()
            } else {
                "no".to_owned()
            },
        ),
    ]);

    format!(
        "{}{}",
        cards,
        render_convergence_steps_table(&result.convergence)
    )
}

fn render_metric_cards(metrics: &[(&str, String)]) -> String {
    let mut html = String::from("<div class=\"metric-grid\">");
    for (label, value) in metrics {
        let _ = write!(
            html,
            "<div class=\"metric-card\"><span class=\"metric-label\">{}</span><span class=\"metric-value\">{}</span></div>",
            html_escape(label),
            html_escape(value),
        );
    }
    html.push_str("</div>");
    html
}

fn render_gate_stats_table(stats: &[crate::runner::GateStats]) -> String {
    let mut html = String::from(
        "<div class=\"table-block\"><h3>Gate Statistics</h3><table class=\"report-table\"><thead><tr><th>Dye</th><th>Primary Channel</th><th>Total Events</th><th>Gated Events</th><th>Strict Points</th><th>Bound Maxima</th><th>Region Maxima</th></tr></thead><tbody>",
    );
    for row in stats {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_escape(&row.dye),
            html_escape(&row.primary_channel),
            row.total_events,
            row.gated_events,
            row.strict_points,
            row.bound_maxima,
            row.region_maxima,
        );
    }
    html.push_str("</tbody></table></div>");
    html
}

fn render_spread_stats_table(stats: &[crate::runner::SpreadStats]) -> String {
    let mut html = String::from(
        "<div class=\"table-block\"><h3>Spread Statistics</h3><table class=\"report-table\"><thead><tr><th>Dye</th><th>Primary Channel</th><th>Quantile Bins</th><th>Non-zero Coefficients</th></tr></thead><tbody>",
    );
    for row in stats {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_escape(&row.dye),
            html_escape(&row.primary_channel),
            row.quantile_bins,
            row.non_zero_coefficients,
        );
    }
    html.push_str("</tbody></table></div>");
    html
}

fn render_convergence_steps_table(convergence: &crate::runner::ConvergenceSummary) -> String {
    let mut html = String::from(
        "<div class=\"table-block\"><h3>Convergence Steps</h3><table class=\"report-table\"><thead><tr><th>Iteration</th><th>Scale</th><th>Lambda</th><th>Delta</th><th>Delta Max</th><th>Delta Change</th></tr></thead><tbody>",
    );
    for step in &convergence.steps {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{:.6}</td><td>{:.6}</td><td>{:.6}</td><td>{:.6}</td></tr>",
            step.iteration,
            html_escape(&step.scale),
            step.lambda,
            step.delta,
            step.delta_max,
            step.delta_change,
        );
    }
    html.push_str("</tbody></table></div>");
    html
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), AutospillError> {
    debug!("writing JSON summary to {}", path.display());
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| AutospillError::DebugWrite {
        path: path.to_path_buf(),
        source: std::io::Error::other(source),
    })?;
    fs::write(path, bytes).map_err(|source| AutospillError::DebugWrite {
        path: path.to_path_buf(),
        source,
    })?;
    debug!("wrote JSON summary to {}", path.display());
    Ok(())
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
