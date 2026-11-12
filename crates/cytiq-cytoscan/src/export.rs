#![allow(
    clippy::cast_precision_loss,
    clippy::module_name_repetitions,
    clippy::too_many_lines
)]

use std::fs;
use std::path::Path;

use cytiq_publish::{Existing, Publication};
use nalgebra::{DMatrix, DVector, SymmetricEigen};
use plotly::box_plot::BoxPoints;
use plotly::common::{ColorScale, ColorScalePalette, Line, Marker, Mode, Title};
use plotly::layout::Axis;
use plotly::{Bar, BoxPlot, HeatMap, Layout, Plot, Scatter};
use serde::Serialize;

use crate::error::CytoScanError;
use crate::runner::{
    CytoScanFeatureMatrix, CytoScanResult, CytoScanSummaryRow, DEFAULT_EVENTS_PER_FILE, LoadedFile,
    sampled_matrix,
};

const SUMMARY_FILE_NAME: &str = "cytoscan_summary.csv";
const FEATURES_FILE_NAME: &str = "cytoscan_features.csv";
const CONFIG_FILE_NAME: &str = "cytoscan_config.json";
const REFERENCE_FEATURES_FILE_NAME: &str = "reference_features.csv";
const AGGREGATE_SAMPLING_FILE_NAME: &str = "aggregate_sampling.csv";
const MODEL_DIAGNOSTICS_FILE_NAME: &str = "model_diagnostics.json";
const PCA_COORDINATES_FILE_NAME: &str = "pca_coordinates.csv";
const PCA_LOADINGS_FILE_NAME: &str = "pca_loadings.csv";
const FEATURE_HEATMAP_FILE_NAME: &str = "feature_heatmap.csv";
const BOXPLOT_SAMPLES_FILE_NAME: &str = "boxplot_samples.csv";
const REPORT_FILE_NAME: &str = "report.html";
const TOP_LOADING_COUNT: usize = 10;
const SCALE_EPSILON: f64 = 1.0e-12;

#[derive(Serialize)]
struct ConfigDocument<'a> {
    parameters: &'a crate::runner::CytoScanParameters,
    diagnostics: &'a crate::runner::CytoScanDiagnostics,
}

#[derive(Clone, Debug)]
struct PcaResult {
    coordinates: Vec<PcaCoordinate>,
    loadings: Vec<PcaLoading>,
    pc1_variance_percent: f64,
    pc2_variance_percent: f64,
}

#[derive(Clone, Debug)]
struct PcaCoordinate {
    source: String,
    status: String,
    filename: String,
    pc1: f64,
    pc2: f64,
}

#[derive(Clone, Debug)]
struct PcaLoading {
    feature: String,
    pc1: f64,
    pc2: f64,
}

#[derive(Clone, Debug)]
struct FeaturePlotRow {
    source: String,
    status: String,
    filename: String,
    values: Vec<f64>,
}

#[derive(Clone, Debug)]
struct HeatmapResult {
    feature_names: Vec<String>,
    rows: Vec<HeatmapRow>,
}

#[derive(Clone, Debug)]
struct HeatmapRow {
    source: String,
    status: String,
    filename: String,
    values: Vec<f64>,
}

#[derive(Clone, Debug)]
struct BoxplotSample {
    source: String,
    status: String,
    filename: String,
    channel: String,
    value: f64,
}

#[derive(Clone, Debug)]
struct BoxplotTraceData {
    source: String,
    status: String,
    filename: String,
    values: Vec<f64>,
    median: f64,
}

#[derive(Clone, Debug)]
struct WardCluster {
    members: Vec<usize>,
    centroid: Vec<f64>,
}

pub(crate) fn write_outputs(
    config: &crate::runner::CytoScanConfig,
    result: &CytoScanResult,
    test_files: &[LoadedFile],
    reference_files: &[LoadedFile],
) -> Result<(), CytoScanError> {
    // The three outputs describe one run: publish them together.
    let mut publication = Publication::new(Existing::overwrite(config.overwrite));
    publication.stage_path(config.output_dir.join(SUMMARY_FILE_NAME), |path| {
        write_summary_csv(path, result)
    })?;
    publication.stage_path(config.output_dir.join(FEATURES_FILE_NAME), |path| {
        write_feature_matrix_csv(path, &result.test_features)
    })?;
    publication.stage_path(config.output_dir.join(CONFIG_FILE_NAME), |path| {
        write_config_json(path, result)
    })?;
    publication.commit()?;
    if let Some(debug_dir) = config.debug_dir.as_deref() {
        write_debug_artifacts(debug_dir, result, test_files, reference_files)?;
    }
    Ok(())
}

fn write_summary_csv(path: &Path, result: &CytoScanResult) -> Result<(), CytoScanError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| CytoScanError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    writer
        .write_record([
            "filename",
            "path",
            "flag_method",
            "feature_method",
            "is_outlier",
            "outlier_score",
            "is_novelty",
            "novelty_log_likelihood",
            "channels",
            "warnings",
        ])
        .map_err(|source| CytoScanError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for row in &result.summary_rows {
        writer
            .write_record([
                row.filename.clone(),
                row.path.display().to_string(),
                row.flag_method.as_str().to_owned(),
                row.feature_method.as_str().to_owned(),
                option_bool(row.is_outlier),
                option_f64(row.outlier_score),
                option_bool(row.is_novelty),
                option_f64(row.novelty_log_likelihood),
                row.channels.clone(),
                row.warnings.clone(),
            ])
            .map_err(|source| CytoScanError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| CytoScanError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_feature_matrix_csv(
    path: &Path,
    matrix: &CytoScanFeatureMatrix,
) -> Result<(), CytoScanError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| CytoScanError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    let mut header = vec!["filename".to_owned(), "path".to_owned()];
    header.extend(matrix.feature_names.clone());
    writer
        .write_record(header)
        .map_err(|source| CytoScanError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for row in &matrix.rows {
        let mut record = vec![row.filename.clone(), row.path.display().to_string()];
        record.extend(row.values.iter().map(|value| format!("{value:.12}")));
        writer
            .write_record(record)
            .map_err(|source| CytoScanError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| CytoScanError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_config_json(path: &Path, result: &CytoScanResult) -> Result<(), CytoScanError> {
    let document = ConfigDocument {
        parameters: &result.parameters,
        diagnostics: &result.diagnostics,
    };
    let bytes =
        serde_json::to_vec_pretty(&document).map_err(|source| CytoScanError::JsonSerialize {
            path: path.to_path_buf(),
            source,
        })?;
    fs::write(path, bytes).map_err(|source| CytoScanError::JsonWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn write_debug_artifacts(
    debug_dir: &Path,
    result: &CytoScanResult,
    test_files: &[LoadedFile],
    reference_files: &[LoadedFile],
) -> Result<(), CytoScanError> {
    let mut publication = Publication::new(Existing::Replace);
    if let Some(reference_features) = result.reference_features.as_ref() {
        publication.stage_path(debug_dir.join(REFERENCE_FEATURES_FILE_NAME), |path| {
            write_feature_matrix_csv(path, reference_features)
        })?;
    }
    publication.stage_path(debug_dir.join(AGGREGATE_SAMPLING_FILE_NAME), |path| {
        write_aggregate_sampling_csv(path, result)
    })?;
    publication.stage_path(debug_dir.join(MODEL_DIAGNOSTICS_FILE_NAME), |path| {
        write_model_diagnostics_json(path, result)
    })?;
    let pca = calculate_pca(result);
    let heatmap = calculate_feature_heatmap(result);
    let boxplot_samples = collect_boxplot_samples(result, test_files, reference_files);
    publication.stage_path(debug_dir.join(PCA_COORDINATES_FILE_NAME), |path| {
        write_pca_coordinates_csv(path, &pca)
    })?;
    publication.stage_path(debug_dir.join(PCA_LOADINGS_FILE_NAME), |path| {
        write_pca_loadings_csv(path, &pca)
    })?;
    publication.stage_path(debug_dir.join(FEATURE_HEATMAP_FILE_NAME), |path| {
        write_feature_heatmap_csv(path, &heatmap)
    })?;
    publication.stage_path(debug_dir.join(BOXPLOT_SAMPLES_FILE_NAME), |path| {
        write_boxplot_samples_csv(path, &boxplot_samples)
    })?;
    publication.stage_path(debug_dir.join(REPORT_FILE_NAME), |path| {
        write_report_html(path, result, &pca, &heatmap, &boxplot_samples)
    })?;
    Ok(publication.commit()?)
}

fn write_aggregate_sampling_csv(path: &Path, result: &CytoScanResult) -> Result<(), CytoScanError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| CytoScanError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    writer
        .write_record(["aggregate_source", "aggregate_events", "file"])
        .map_err(|source| CytoScanError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for file in &result.diagnostics.aggregate_files {
        writer
            .write_record([
                result.diagnostics.aggregate_source.clone(),
                result.diagnostics.aggregate_events.to_string(),
                file.display().to_string(),
            ])
            .map_err(|source| CytoScanError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| CytoScanError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_model_diagnostics_json(path: &Path, result: &CytoScanResult) -> Result<(), CytoScanError> {
    let bytes = serde_json::to_vec_pretty(&result.diagnostics).map_err(|source| {
        CytoScanError::JsonSerialize {
            path: path.to_path_buf(),
            source,
        }
    })?;
    fs::write(path, bytes).map_err(|source| CytoScanError::JsonWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn write_pca_coordinates_csv(path: &Path, pca: &PcaResult) -> Result<(), CytoScanError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| CytoScanError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    writer
        .write_record(["source", "status", "filename", "pc1", "pc2"])
        .map_err(|source| CytoScanError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for row in &pca.coordinates {
        writer
            .write_record([
                row.source.clone(),
                row.status.clone(),
                row.filename.clone(),
                format!("{:.12}", row.pc1),
                format!("{:.12}", row.pc2),
            ])
            .map_err(|source| CytoScanError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| CytoScanError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_pca_loadings_csv(path: &Path, pca: &PcaResult) -> Result<(), CytoScanError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| CytoScanError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    writer
        .write_record(["feature", "pc1", "pc2"])
        .map_err(|source| CytoScanError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for row in &pca.loadings {
        writer
            .write_record([
                row.feature.clone(),
                format!("{:.12}", row.pc1),
                format!("{:.12}", row.pc2),
            ])
            .map_err(|source| CytoScanError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| CytoScanError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_feature_heatmap_csv(path: &Path, heatmap: &HeatmapResult) -> Result<(), CytoScanError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| CytoScanError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    let mut header = vec![
        "source".to_owned(),
        "status".to_owned(),
        "filename".to_owned(),
    ];
    header.extend(heatmap.feature_names.clone());
    writer
        .write_record(header)
        .map_err(|source| CytoScanError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for row in &heatmap.rows {
        let mut record = vec![row.source.clone(), row.status.clone(), row.filename.clone()];
        record.extend(row.values.iter().map(|value| format!("{value:.12}")));
        writer
            .write_record(record)
            .map_err(|source| CytoScanError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| CytoScanError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_boxplot_samples_csv(path: &Path, samples: &[BoxplotSample]) -> Result<(), CytoScanError> {
    let mut writer = csv::Writer::from_path(path).map_err(|source| CytoScanError::CsvWrite {
        path: path.to_path_buf(),
        source,
    })?;
    writer
        .write_record(["source", "status", "filename", "channel", "value"])
        .map_err(|source| CytoScanError::CsvWrite {
            path: path.to_path_buf(),
            source,
        })?;
    for sample in samples {
        writer
            .write_record([
                sample.source.clone(),
                sample.status.clone(),
                sample.filename.clone(),
                sample.channel.clone(),
                format!("{:.12}", sample.value),
            ])
            .map_err(|source| CytoScanError::CsvWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| CytoScanError::CsvFlush {
        path: path.to_path_buf(),
        source,
    })
}

fn write_report_html(
    path: &Path,
    result: &CytoScanResult,
    pca: &PcaResult,
    heatmap: &HeatmapResult,
    boxplot_samples: &[BoxplotSample],
) -> Result<(), CytoScanError> {
    let plots = report_plots(pca, heatmap, boxplot_samples);
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>CytoScan Report</title>
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
<h1>CytoScan Report</h1>
<table>
<tr><th>Metric</th><th>Value</th></tr>
<tr><td>Test files</td><td>{test_files}</td></tr>
<tr><td>Feature method</td><td>{feature_method}</td></tr>
<tr><td>Flag method</td><td>{flag_method}</td></tr>
<tr><td>Features</td><td>{feature_count}</td></tr>
<tr><td>Aggregate source</td><td>{aggregate_source}</td></tr>
</table>
<h2>Plots</h2>
{plots}
</body>
</html>
"#,
        plotly_js = Plot::online_cdn_js(),
        test_files = result.summary_rows.len(),
        feature_method = result.parameters.feature_method.as_str(),
        flag_method = result.parameters.flag_method.as_str(),
        feature_count = result.diagnostics.feature_count,
        aggregate_source = escape_html(&result.diagnostics.aggregate_source),
        plots = plots
    );
    fs::write(path, html).map_err(|source| CytoScanError::HtmlWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn report_plots(
    pca: &PcaResult,
    heatmap: &HeatmapResult,
    boxplot_samples: &[BoxplotSample],
) -> String {
    let mut panels = vec![
        plot_panel("PCA", &pca_plot(pca).to_inline_html(Some("cytoscan-pca"))),
        plot_panel(
            "Top PCA Loadings",
            &pca_loading_plot(pca).to_inline_html(Some("cytoscan-pca-loadings")),
        ),
        plot_panel(
            "Feature Heatmap",
            &feature_heatmap_plot(heatmap).to_inline_html(Some("cytoscan-feature-heatmap")),
        ),
    ];
    for (index, channel) in ordered_channels(boxplot_samples).iter().enumerate() {
        panels.push(plot_panel(
            &format!("Boxplots: {channel}"),
            &boxplot_plot(channel, boxplot_samples)
                .to_inline_html(Some(&format!("cytoscan-boxplot-{index}"))),
        ));
    }
    panels.join("\n")
}

fn plot_panel(title: &str, plot_html: &str) -> String {
    format!(
        "<section class=\"plot-panel\"><h2>{}</h2>{}</section>",
        escape_html(title),
        plot_html
    )
}

fn calculate_pca(result: &CytoScanResult) -> PcaResult {
    let plot_rows = feature_plot_rows(result);
    if plot_rows.is_empty() {
        return empty_pca(result);
    }
    let feature_count = result.test_features.feature_names.len();
    let fit_matrix = result.reference_features.as_ref().map_or_else(
        || result.test_features.values(),
        CytoScanFeatureMatrix::values,
    );
    if fit_matrix.len() < 2 || feature_count == 0 {
        return zero_pca(result, &plot_rows);
    }

    let (means, standard_deviations) = fit_feature_scale(&fit_matrix);
    let scaled_fit = scaled_matrix(&fit_matrix, &means, &standard_deviations);
    let covariance = (&scaled_fit.transpose() * &scaled_fit) / (fit_matrix.len() - 1) as f64;
    let eigen = SymmetricEigen::new(covariance);
    let mut order = (0..feature_count).collect::<Vec<_>>();
    order.sort_by(|left, right| eigen.eigenvalues[*right].total_cmp(&eigen.eigenvalues[*left]));
    let pc1_index = order[0];
    let pc2_index = order.get(1).copied();
    let pc1_vector = eigen.eigenvectors.column(pc1_index).clone_owned();
    let pc2_vector = pc2_index.map_or_else(
        || DVector::zeros(feature_count),
        |index| eigen.eigenvectors.column(index).clone_owned(),
    );

    let plot_matrix = plot_rows
        .iter()
        .map(|row| row.values.clone())
        .collect::<Vec<_>>();
    let scaled_plot = scaled_matrix(&plot_matrix, &means, &standard_deviations);
    let coordinates_matrix =
        scaled_plot * DMatrix::from_columns(&[pc1_vector.clone(), pc2_vector.clone()]);
    let coordinates = plot_rows
        .iter()
        .enumerate()
        .map(|(index, row)| PcaCoordinate {
            source: row.source.clone(),
            status: row.status.clone(),
            filename: row.filename.clone(),
            pc1: coordinates_matrix[(index, 0)],
            pc2: coordinates_matrix[(index, 1)],
        })
        .collect();
    let loadings = result
        .test_features
        .feature_names
        .iter()
        .enumerate()
        .map(|(index, feature)| PcaLoading {
            feature: feature.clone(),
            pc1: pc1_vector[index],
            pc2: pc2_vector[index],
        })
        .collect();
    let total_variance = eigen
        .eigenvalues
        .iter()
        .map(|value| value.max(0.0))
        .sum::<f64>();
    PcaResult {
        coordinates,
        loadings,
        pc1_variance_percent: variance_percent(eigen.eigenvalues[pc1_index], total_variance),
        pc2_variance_percent: pc2_index.map_or(0.0, |index| {
            variance_percent(eigen.eigenvalues[index], total_variance)
        }),
    }
}

fn fit_feature_scale(matrix: &[Vec<f64>]) -> (Vec<f64>, Vec<f64>) {
    let rows = matrix.len();
    let columns = matrix[0].len();
    let means = (0..columns)
        .map(|column| matrix.iter().map(|row| row[column]).sum::<f64>() / rows as f64)
        .collect::<Vec<_>>();
    let denominator = rows.saturating_sub(1).max(1) as f64;
    let standard_deviations = (0..columns)
        .map(|column| {
            let variance = matrix
                .iter()
                .map(|row| (row[column] - means[column]).powi(2))
                .sum::<f64>()
                / denominator;
            let standard_deviation = variance.sqrt();
            if standard_deviation <= SCALE_EPSILON {
                1.0
            } else {
                standard_deviation
            }
        })
        .collect::<Vec<_>>();
    (means, standard_deviations)
}

fn scaled_matrix(matrix: &[Vec<f64>], means: &[f64], standard_deviations: &[f64]) -> DMatrix<f64> {
    DMatrix::from_fn(matrix.len(), means.len(), |row, column| {
        (matrix[row][column] - means[column]) / standard_deviations[column]
    })
}

fn variance_percent(value: f64, total: f64) -> f64 {
    if total <= SCALE_EPSILON {
        0.0
    } else {
        value.max(0.0) / total * 100.0
    }
}

fn empty_pca(result: &CytoScanResult) -> PcaResult {
    zero_pca(result, &[])
}

fn zero_pca(result: &CytoScanResult, rows: &[FeaturePlotRow]) -> PcaResult {
    let coordinates = rows
        .iter()
        .map(|row| PcaCoordinate {
            source: row.source.clone(),
            status: row.status.clone(),
            filename: row.filename.clone(),
            pc1: 0.0,
            pc2: 0.0,
        })
        .collect();
    let loadings = result
        .test_features
        .feature_names
        .iter()
        .map(|feature| PcaLoading {
            feature: feature.clone(),
            pc1: 0.0,
            pc2: 0.0,
        })
        .collect();
    PcaResult {
        coordinates,
        loadings,
        pc1_variance_percent: 0.0,
        pc2_variance_percent: 0.0,
    }
}

fn calculate_feature_heatmap(result: &CytoScanResult) -> HeatmapResult {
    let plot_rows = feature_plot_rows(result);
    let feature_names = result.test_features.feature_names.clone();
    if plot_rows.is_empty() || feature_names.is_empty() {
        return HeatmapResult {
            feature_names,
            rows: plot_rows
                .into_iter()
                .map(|row| HeatmapRow {
                    source: row.source,
                    status: row.status,
                    filename: row.filename,
                    values: Vec::new(),
                })
                .collect(),
        };
    }

    let raw_values = plot_rows
        .iter()
        .map(|row| row.values.clone())
        .collect::<Vec<_>>();
    let scaled_values = zscore_columns(&raw_values);
    let order = ward_row_order(&scaled_values);
    let rows = order
        .into_iter()
        .map(|index| {
            let row = &plot_rows[index];
            HeatmapRow {
                source: row.source.clone(),
                status: row.status.clone(),
                filename: row.filename.clone(),
                values: scaled_values[index].clone(),
            }
        })
        .collect();
    HeatmapResult {
        feature_names,
        rows,
    }
}

fn feature_plot_rows(result: &CytoScanResult) -> Vec<FeaturePlotRow> {
    let mut rows = Vec::new();
    if let Some(reference) = result.reference_features.as_ref() {
        rows.extend(reference.rows.iter().map(|row| FeaturePlotRow {
            source: "reference".to_owned(),
            status: "reference".to_owned(),
            filename: row.filename.clone(),
            values: row.values.clone(),
        }));
    }
    rows.extend(
        result
            .test_features
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| FeaturePlotRow {
                source: "test".to_owned(),
                status: result
                    .summary_rows
                    .get(index)
                    .map_or_else(|| "test".to_owned(), test_status),
                filename: row.filename.clone(),
                values: row.values.clone(),
            }),
    );
    rows
}

fn collect_boxplot_samples(
    result: &CytoScanResult,
    test_files: &[LoadedFile],
    reference_files: &[LoadedFile],
) -> Vec<BoxplotSample> {
    let mut samples = Vec::new();
    for file in reference_files {
        append_boxplot_samples(&mut samples, "reference", "reference", file);
    }
    for (index, file) in test_files.iter().enumerate() {
        let status = result
            .summary_rows
            .get(index)
            .map_or_else(|| "test".to_owned(), test_status);
        append_boxplot_samples(&mut samples, "test", &status, file);
    }
    samples
}

fn append_boxplot_samples(
    samples: &mut Vec<BoxplotSample>,
    source: &str,
    status: &str,
    file: &LoadedFile,
) {
    for row in sampled_matrix(file, DEFAULT_EVENTS_PER_FILE) {
        for (channel_index, channel) in file.channel_names.iter().enumerate() {
            if let Some(value) = row.get(channel_index) {
                samples.push(BoxplotSample {
                    source: source.to_owned(),
                    status: status.to_owned(),
                    filename: file.filename.clone(),
                    channel: channel.clone(),
                    value: *value,
                });
            }
        }
    }
}

fn zscore_columns(matrix: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let rows = matrix.len();
    let columns = matrix[0].len();
    let means = (0..columns)
        .map(|column| matrix.iter().map(|row| row[column]).sum::<f64>() / rows as f64)
        .collect::<Vec<_>>();
    let denominator = rows.saturating_sub(1).max(1) as f64;
    let standard_deviations = (0..columns)
        .map(|column| {
            let variance = matrix
                .iter()
                .map(|row| (row[column] - means[column]).powi(2))
                .sum::<f64>()
                / denominator;
            let standard_deviation = variance.sqrt();
            if standard_deviation <= SCALE_EPSILON {
                1.0
            } else {
                standard_deviation
            }
        })
        .collect::<Vec<_>>();
    matrix
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(column, value)| (value - means[column]) / standard_deviations[column])
                .collect()
        })
        .collect()
}

fn ward_row_order(matrix: &[Vec<f64>]) -> Vec<usize> {
    if matrix.len() <= 1 {
        return (0..matrix.len()).collect();
    }
    let mut clusters = matrix
        .iter()
        .enumerate()
        .map(|(index, row)| WardCluster {
            members: vec![index],
            centroid: row.clone(),
        })
        .collect::<Vec<_>>();
    while clusters.len() > 1 {
        let mut best_left = 0;
        let mut best_right = 1;
        let mut best_distance = f64::INFINITY;
        for left in 0..clusters.len() {
            for right in (left + 1)..clusters.len() {
                let distance = ward_distance(&clusters[left], &clusters[right]);
                let ordering = distance.total_cmp(&best_distance);
                if matches!(ordering, std::cmp::Ordering::Less)
                    || (matches!(ordering, std::cmp::Ordering::Equal)
                        && (left, right) < (best_left, best_right))
                {
                    best_left = left;
                    best_right = right;
                    best_distance = distance;
                }
            }
        }
        let right = clusters.remove(best_right);
        let left = clusters.remove(best_left);
        clusters.insert(best_left, merge_clusters(left, right));
    }
    clusters.remove(0).members
}

fn ward_distance(left: &WardCluster, right: &WardCluster) -> f64 {
    let left_count = left.members.len() as f64;
    let right_count = right.members.len() as f64;
    let centroid_distance = left
        .centroid
        .iter()
        .zip(&right.centroid)
        .map(|(left_value, right_value)| (left_value - right_value).powi(2))
        .sum::<f64>();
    (left_count * right_count) / (left_count + right_count) * centroid_distance
}

fn merge_clusters(left: WardCluster, right: WardCluster) -> WardCluster {
    let left_count = left.members.len();
    let right_count = right.members.len();
    let total_count = left_count + right_count;
    let centroid = left
        .centroid
        .iter()
        .zip(&right.centroid)
        .map(|(left_value, right_value)| {
            (left_value * left_count as f64 + right_value * right_count as f64) / total_count as f64
        })
        .collect();
    let mut members = left.members;
    members.extend(right.members);
    WardCluster { members, centroid }
}

fn pca_plot(pca: &PcaResult) -> Plot {
    let mut plot = Plot::new();
    for status in status_order() {
        let rows = pca
            .coordinates
            .iter()
            .filter(|row| row.status == status)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        let x = rows.iter().map(|row| row.pc1).collect::<Vec<_>>();
        let y = rows.iter().map(|row| row.pc2).collect::<Vec<_>>();
        let text = rows
            .iter()
            .map(|row| format!("{}: {}", row.source, row.filename))
            .collect::<Vec<_>>();
        plot.add_trace(
            Scatter::new(x, y)
                .mode(Mode::Markers)
                .text_array(text)
                .marker(Marker::new().color(status_color(status)))
                .name(status),
        );
    }

    let axis_scale = pca
        .coordinates
        .iter()
        .flat_map(|row| [row.pc1.abs(), row.pc2.abs()])
        .fold(0.0, f64::max)
        .max(1.0)
        * 0.8;
    for loading in top_loadings(pca, TOP_LOADING_COUNT) {
        let x = loading.pc1 * axis_scale;
        let y = loading.pc2 * axis_scale;
        plot.add_trace(
            Scatter::new(vec![0.0, x], vec![0.0, y])
                .mode(Mode::LinesText)
                .text_array(vec![String::new(), loading.feature.clone()])
                .line(Line::new().color("#6b7280").width(1.0))
                .show_legend(false)
                .name(loading.feature.clone()),
        );
    }

    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Feature PCA"))
            .x_axis(Axis::new().title(Title::with_text(format!(
                "PC1 ({:.1}%)",
                pca.pc1_variance_percent
            ))))
            .y_axis(Axis::new().title(Title::with_text(format!(
                "PC2 ({:.1}%)",
                pca.pc2_variance_percent
            ))))
            .height(560),
    );
    plot
}

fn pca_loading_plot(pca: &PcaResult) -> Plot {
    let mut plot = Plot::new();
    let loadings = top_loadings(pca, TOP_LOADING_COUNT);
    let features = loadings
        .iter()
        .map(|loading| loading.feature.clone())
        .collect::<Vec<_>>();
    let pc1 = loadings
        .iter()
        .map(|loading| loading.pc1)
        .collect::<Vec<_>>();
    let pc2 = loadings
        .iter()
        .map(|loading| loading.pc2)
        .collect::<Vec<_>>();
    plot.add_trace(
        Bar::new(features.clone(), pc1)
            .name("PC1")
            .marker(Marker::new().color("#4E79A7")),
    );
    plot.add_trace(
        Bar::new(features, pc2)
            .name("PC2")
            .marker(Marker::new().color("#F28E2B")),
    );
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Top absolute PCA loadings"))
            .x_axis(Axis::new().title(Title::with_text("Feature")))
            .y_axis(Axis::new().title(Title::with_text("Loading")))
            .height(520),
    );
    plot
}

fn feature_heatmap_plot(heatmap: &HeatmapResult) -> Plot {
    let mut plot = Plot::new();
    let y = heatmap
        .rows
        .iter()
        .map(|row| format!("{}: {} ({})", row.source, row.filename, row.status))
        .collect::<Vec<_>>();
    let z = heatmap
        .rows
        .iter()
        .map(|row| row.values.clone())
        .collect::<Vec<_>>();
    plot.add_trace(
        HeatMap::new(heatmap.feature_names.clone(), y, z)
            .color_scale(ColorScale::Palette(ColorScalePalette::RdBu))
            .zmin(-3.0)
            .zmid(0.0)
            .zmax(3.0),
    );
    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Feature z-score heatmap"))
            .x_axis(Axis::new().title(Title::with_text("Feature")))
            .y_axis(Axis::new().title(Title::with_text("File")))
            .height(620),
    );
    plot
}

fn boxplot_plot(channel: &str, samples: &[BoxplotSample]) -> Plot {
    let mut plot = Plot::new();
    let mut groups = Vec::<BoxplotTraceData>::new();
    for sample in samples.iter().filter(|sample| sample.channel == channel) {
        if let Some(group) = groups.iter_mut().find(|group| {
            group.source == sample.source
                && group.status == sample.status
                && group.filename == sample.filename
        }) {
            group.values.push(sample.value);
        } else {
            groups.push(BoxplotTraceData {
                source: sample.source.clone(),
                status: sample.status.clone(),
                filename: sample.filename.clone(),
                values: vec![sample.value],
                median: 0.0,
            });
        }
    }
    for group in &mut groups {
        group.median = median(&group.values);
    }
    groups.sort_by(|left, right| {
        left.median
            .total_cmp(&right.median)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.filename.cmp(&right.filename))
    });
    for group in groups {
        let label = format!("{}: {}", group.source, group.filename);
        let x = vec![label; group.values.len()];
        plot.add_trace(
            BoxPlot::new_xy(x, group.values)
                .name(format!("{} ({})", group.filename, group.status))
                .box_points(BoxPoints::False)
                .marker(Marker::new().color(status_color(&group.status)))
                .line(Line::new().color(status_color(&group.status))),
        );
    }
    plot.set_layout(
        Layout::new()
            .title(Title::with_text(format!("Per-file {channel} distribution")))
            .x_axis(Axis::new().title(Title::with_text("File")))
            .y_axis(Axis::new().title(Title::with_text("Transformed intensity")))
            .height(520),
    );
    plot
}

fn top_loadings(pca: &PcaResult, limit: usize) -> Vec<&PcaLoading> {
    let mut loadings = pca.loadings.iter().collect::<Vec<_>>();
    loadings.sort_by(|left, right| {
        loading_strength(right)
            .total_cmp(&loading_strength(left))
            .then_with(|| left.feature.cmp(&right.feature))
    });
    loadings.truncate(limit.min(loadings.len()));
    loadings
}

fn loading_strength(loading: &PcaLoading) -> f64 {
    loading.pc1.mul_add(loading.pc1, loading.pc2 * loading.pc2)
}

fn ordered_channels(samples: &[BoxplotSample]) -> Vec<String> {
    let mut channels = Vec::new();
    for sample in samples {
        if !channels.iter().any(|channel| channel == &sample.channel) {
            channels.push(sample.channel.clone());
        }
    }
    channels
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let midpoint = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        f64::midpoint(sorted[midpoint - 1], sorted[midpoint])
    } else {
        sorted[midpoint]
    }
}

fn test_status(row: &CytoScanSummaryRow) -> String {
    match (row.is_outlier, row.is_novelty) {
        (Some(true), Some(true)) => "outlier+novelty".to_owned(),
        (Some(true), _) => "outlier".to_owned(),
        (_, Some(true)) => "novelty".to_owned(),
        (_, Some(false)) => "non-novelty".to_owned(),
        _ => "inlier".to_owned(),
    }
}

fn status_order() -> [&'static str; 6] {
    [
        "reference",
        "inlier",
        "non-novelty",
        "outlier",
        "novelty",
        "outlier+novelty",
    ]
}

fn status_color(status: &str) -> &'static str {
    match status {
        "reference" => "#4E79A7",
        "inlier" => "#59A14F",
        "non-novelty" => "#76B7B2",
        "outlier" => "#E15759",
        "novelty" => "#F28E2B",
        "outlier+novelty" => "#B07AA1",
        _ => "#6B7280",
    }
}

fn option_bool(value: Option<bool>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn option_f64(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.12}"))
        .unwrap_or_default()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
