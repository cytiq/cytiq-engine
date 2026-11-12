#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cytiq_fcs::{ColumnData, FcsDataset, FcsReader, Parameter, SignalState};
use log::{debug, info};
use nalgebra::DMatrix;
use ndarray::Array2;
use rayon::prelude::*;
use serde::Serialize;

use crate::error::AutospillError;
use crate::manifest::{ControlsManifestRow, DetectorSpec, TransformSpec};
use crate::plotting::{write_autospill_debug_artifacts, write_autospread_debug_artifacts};
use crate::regression::{
    RegressionResult, ordinary_linear_regression, ordinary_linear_regression_without_intercept,
    robust_linear_regression, robust_standard_deviation, sample_standard_deviation_iter, skewness,
    trim_joint_pairs,
};
use crate::transform::{BiExponentialTransform, resolve_transforms, transformed_slope_to_linear};
use cytiq_density::{GateComputation, gate_events};

const RLM_ITER_MAX: usize = 100;
const RLM_TRIM_FACTOR: f64 = 0.01;

const RS_ITER_MAX: usize = 100;
const RS_LAMBDA_COARSE: f64 = 1.0;
const RS_LAMBDA_FINE: f64 = 0.1;
const RS_DELTA_HISTORY: usize = 10;
const RS_DELTA_THRESHOLD_LINEAR: f64 = 1e-2;
const RS_DELTA_THRESHOLD_BIEXP: f64 = 1e-4;
const RS_DELTA_THRESHOLD_CHANGE: f64 = 1e-6;

const PSEUDO_INVERSE_EPSILON: f64 = 1e-10;
const AUTOSPREAD_MIN_BINS: usize = 8;
const AUTOSPREAD_MAX_BINS: usize = 256;
const AUTOSPREAD_TARGET_BIN_SIZE: usize = 8;
const AUTOFLUORESCENCE_DYE: &str = "autofluorescence";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MatrixKind {
    #[default]
    Compensation,
    Spillover,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutospillConfig {
    pub fsc: String,
    pub ssc: String,
    pub matrix_kind: MatrixKind,
    pub detectors: Vec<DetectorSpec>,
    pub transforms: Vec<TransformSpec>,
    pub debug_dir: Option<PathBuf>,
}

impl Default for AutospillConfig {
    fn default() -> Self {
        Self {
            fsc: "FSC-A".to_owned(),
            ssc: "SSC-A".to_owned(),
            matrix_kind: MatrixKind::Compensation,
            detectors: Vec::new(),
            transforms: Vec::new(),
            debug_dir: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GateStats {
    pub dye: String,
    pub primary_channel: String,
    pub total_events: usize,
    pub gated_events: usize,
    pub strict_points: usize,
    pub bound_maxima: usize,
    pub region_maxima: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConvergenceStep {
    pub iteration: usize,
    pub scale: String,
    pub lambda: f64,
    pub delta: f64,
    pub delta_max: f64,
    pub delta_change: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConvergenceSummary {
    pub converged: bool,
    pub iterations: usize,
    pub final_delta_max: f64,
    pub steps: Vec<ConvergenceStep>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutospillResult {
    pub dyes: Vec<String>,
    pub detectors: Vec<String>,
    pub channels: Vec<String>,
    pub primary_channels: Vec<String>,
    pub spillover_matrix: Vec<Vec<f64>>,
    pub compensation_matrix: Vec<Vec<f64>>,
    pub residual_slope_error: Vec<Vec<f64>>,
    pub residual_skewness: Vec<Vec<f64>>,
    pub gate_stats: Vec<GateStats>,
    pub convergence: ConvergenceSummary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutospreadConfig {
    pub fsc: String,
    pub ssc: String,
    pub detectors: Vec<DetectorSpec>,
    pub debug_dir: Option<PathBuf>,
}

impl Default for AutospreadConfig {
    fn default() -> Self {
        Self {
            fsc: "FSC-A".to_owned(),
            ssc: "SSC-A".to_owned(),
            detectors: Vec::new(),
            debug_dir: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SpreadStats {
    pub dye: String,
    pub primary_channel: String,
    pub quantile_bins: usize,
    pub non_zero_coefficients: usize,
    /// One fit per detector, in detector order (row `dye` of the matrix).
    pub fits: Vec<SpreadFit>,
}

/// Why a spread coefficient has its value. Every status but `Fitted` leaves 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreadFitStatus {
    /// The dye's primary detector: a structural zero.
    Primary,
    /// Fewer than two bins with at least three events.
    TooFewBins,
    /// `sigma ~ sqrt_like(primary)` had no solution.
    FirstFitFailed,
    /// The through-origin fit of the noise-corrected spread had no solution.
    SecondFitFailed,
    Fitted,
}

/// One dye x detector `AutoSpread` fit, as the debug summary reports it: the
/// binned points both regressions used and their results, in raw detector
/// units, so the coefficient can be recomputed and its units checked
/// outside cytiq. The arithmetic is the matrix estimate's own.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SpreadFit {
    pub detector: String,
    pub status: SpreadFitStatus,
    /// Median primary signal of each bin with at least three events.
    pub primary_median: Vec<f64>,
    /// Robust SD (84th percentile − median) of this detector in those bins.
    pub secondary_robust_sd: Vec<f64>,
    /// Bins left out for having fewer than three events.
    pub skipped_bins: usize,
    /// First fit `secondary_robust_sd ~ intercept + slope · sqrt_like(primary_median)`.
    pub first_intercept: Option<f64>,
    pub first_slope: Option<f64>,
    /// `max(first_intercept, 0)`: the spread without primary signal.
    pub sigma0: Option<f64>,
    /// Through-origin slope of `sqrt_like(max(sd² − sigma0², 0))` on
    /// `sqrt_like(primary_median)`, before it is clamped at zero.
    pub unclamped_coefficient: Option<f64>,
    /// The spread matrix cell.
    pub coefficient: f64,
    /// Second-fit residuals per bin, against the unclamped slope.
    pub residuals: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutospreadResult {
    pub dyes: Vec<String>,
    pub detectors: Vec<String>,
    pub channels: Vec<String>,
    pub primary_channels: Vec<String>,
    pub spread_matrix: Vec<Vec<f64>>,
    pub gate_stats: Vec<GateStats>,
    pub spread_stats: Vec<SpreadStats>,
}

impl AutospillResult {
    #[must_use]
    pub fn matrix(&self, kind: MatrixKind) -> &[Vec<f64>] {
        match kind {
            MatrixKind::Compensation => &self.compensation_matrix,
            MatrixKind::Spillover => &self.spillover_matrix,
        }
    }

    #[must_use]
    pub fn row_labels(&self, kind: MatrixKind) -> &[String] {
        match kind {
            MatrixKind::Compensation => &self.detectors,
            MatrixKind::Spillover => &self.dyes,
        }
    }

    #[must_use]
    pub fn column_labels(&self, kind: MatrixKind) -> &[String] {
        match kind {
            MatrixKind::Compensation => &self.dyes,
            MatrixKind::Spillover => &self.detectors,
        }
    }
}

pub struct AutospillRunner;
pub struct AutospreadRunner;

/// `read.flow.control`: `flow.set.resolution <- max($P1R)` over all controls.
fn scatter_resolution(controls: &[ControlData]) -> Option<f64> {
    controls
        .iter()
        .filter_map(|control| control.resolution)
        .reduce(f64::max)
}

#[derive(Clone, Debug)]
pub(crate) struct ControlData {
    pub(crate) row: ControlsManifestRow,
    pub(crate) path: PathBuf,
    pub(crate) primary_channel: String,
    pub(crate) detector_events: Array2<f64>,
    pub(crate) scatter: Vec<[f64; 2]>,
    /// `$P1R` of the control file; autospill clamps scatter to `[0, max($P1R)]`.
    pub(crate) resolution: Option<f64>,
    pub(crate) primary_detector_index: usize,
    pub(crate) dye_index: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedControls {
    pub(crate) dyes: Vec<String>,
    pub(crate) primary_channels: Vec<String>,
    pub(crate) detectors: Vec<DetectorSpec>,
    pub(crate) dye_transforms: Vec<BiExponentialTransform>,
    pub(crate) controls: Vec<ControlData>,
}

#[derive(Clone, Debug)]
struct PreparedControl {
    row: ControlsManifestRow,
    path: PathBuf,
    dataset: FcsDataset,
    primary_channel: String,
    dye_index: usize,
    is_autofluorescence: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefinementScale {
    Linear,
    BiExponential,
}

#[derive(Clone, Debug)]
struct RegressionMatrices {
    slopes: DMatrix<f64>,
    skewness: DMatrix<f64>,
}

impl AutospillRunner {
    /// The control files `run` reads for `manifest`: each row's file under
    /// `input_dir`, in manifest order.
    #[must_use]
    pub fn input_files(
        manifest: &[crate::manifest::ControlsManifestRow],
        input_dir: &Path,
    ) -> Vec<PathBuf> {
        manifest
            .iter()
            .map(|row| input_dir.join(&row.filename))
            .collect()
    }

    /// Run the `AutoSpill` pipeline for the provided controls.
    ///
    /// # Errors
    ///
    /// Returns an error if controls cannot be loaded, gated, refined, or
    /// exported into debug artifacts.
    pub fn run(
        manifest_rows: &[ControlsManifestRow],
        input_dir: impl AsRef<Path>,
        config: &AutospillConfig,
    ) -> Result<AutospillResult, AutospillError> {
        let input_dir = input_dir.as_ref();
        Self::run_inner(manifest_rows, input_dir, config)
    }

    fn run_inner(
        manifest_rows: &[ControlsManifestRow],
        input_dir: &Path,
        config: &AutospillConfig,
    ) -> Result<AutospillResult, AutospillError> {
        info!(
            "autospill pipeline: loading {} controls from {}",
            manifest_rows.len(),
            input_dir.display()
        );
        let loaded = load_controls(manifest_rows, input_dir, config)?;
        info!(
            "autospill pipeline: loaded {} controls across {} detectors",
            loaded.controls.len(),
            loaded.detectors.len()
        );
        info!("autospill pipeline: computing FSC/SSC gates");
        let scatter_max = scatter_resolution(&loaded.controls);
        let gates = parallel_try_map(&loaded.controls, "FSC/SSC gating", |_, control| {
            gate_events(&control.scatter, scatter_max).map_err(|reason| {
                AutospillError::DegenerateGate {
                    path: control.path.clone(),
                    reason,
                }
            })
        })?;

        let gate_stats = loaded
            .controls
            .iter()
            .zip(&gates)
            .map(|(control, gate)| GateStats {
                dye: control.row.dye.clone(),
                primary_channel: control.primary_channel.clone(),
                total_events: control.detector_events.nrows(),
                gated_events: gate.gated_indices.len(),
                strict_points: gate.strict_points,
                bound_maxima: gate.bound_maxima,
                region_maxima: gate.region_maxima,
            })
            .collect::<Vec<_>>();

        info!("autospill pipeline: estimating initial spillover matrix");
        let initial_spillover =
            estimate_spillover(&loaded.controls, &gates, loaded.detectors.len())?;
        info!("autospill pipeline: refining spillover/compensation matrices");
        let (spillover_matrix, compensation_matrix, convergence, compensation_error) =
            refine_spillover(&loaded, &gates, &initial_spillover)?;

        let result = AutospillResult {
            dyes: loaded.dyes.clone(),
            detectors: loaded
                .detectors
                .iter()
                .map(|detector| detector.channel.clone())
                .collect(),
            channels: loaded.primary_channels.clone(),
            primary_channels: loaded.primary_channels.clone(),
            spillover_matrix: matrix_to_rows(&spillover_matrix),
            compensation_matrix: matrix_to_rows(&compensation_matrix),
            residual_slope_error: matrix_to_rows(
                &(compensation_error.slopes
                    - DMatrix::identity(loaded.dyes.len(), loaded.dyes.len())),
            ),
            residual_skewness: matrix_to_rows(&compensation_error.skewness),
            gate_stats,
            convergence,
        };

        if let Some(debug_dir) = config.debug_dir.as_deref() {
            info!(
                "autospill pipeline: writing debug artifacts to {}",
                debug_dir.display()
            );
            write_autospill_debug_artifacts(debug_dir, &loaded, &gates, &result)?;
        }

        info!("autospill pipeline: completed successfully");
        Ok(result)
    }
}

impl AutospreadRunner {
    /// The control files `run` reads, as for [`AutospillRunner::input_files`].
    #[must_use]
    pub fn input_files(
        manifest: &[crate::manifest::ControlsManifestRow],
        input_dir: &Path,
    ) -> Vec<PathBuf> {
        AutospillRunner::input_files(manifest, input_dir)
    }

    /// Run the `AutoSpread` pipeline for the provided controls.
    ///
    /// # Errors
    ///
    /// Returns an error if controls cannot be loaded, gated, analyzed, or
    /// exported into debug artifacts.
    pub fn run(
        manifest_rows: &[ControlsManifestRow],
        input_dir: impl AsRef<Path>,
        config: &AutospreadConfig,
    ) -> Result<AutospreadResult, AutospillError> {
        let input_dir = input_dir.as_ref();
        Self::run_inner(manifest_rows, input_dir, config)
    }

    fn run_inner(
        manifest_rows: &[ControlsManifestRow],
        input_dir: &Path,
        config: &AutospreadConfig,
    ) -> Result<AutospreadResult, AutospillError> {
        info!(
            "autospread pipeline: loading {} controls from {}",
            manifest_rows.len(),
            input_dir.display()
        );
        let loaded = load_controls(
            manifest_rows,
            input_dir,
            &AutospillConfig {
                fsc: config.fsc.clone(),
                ssc: config.ssc.clone(),
                matrix_kind: MatrixKind::Spillover,
                detectors: config.detectors.clone(),
                transforms: Vec::new(),
                debug_dir: config.debug_dir.clone(),
            },
        )?;
        info!(
            "autospread pipeline: loaded {} controls across {} detectors",
            loaded.controls.len(),
            loaded.detectors.len()
        );
        info!("autospread pipeline: computing FSC/SSC gates");
        let scatter_max = scatter_resolution(&loaded.controls);
        let gates = parallel_try_map(&loaded.controls, "FSC/SSC gating", |_, control| {
            gate_events(&control.scatter, scatter_max).map_err(|reason| {
                AutospillError::DegenerateGate {
                    path: control.path.clone(),
                    reason,
                }
            })
        })?;

        let gate_stats = loaded
            .controls
            .iter()
            .zip(&gates)
            .map(|(control, gate)| GateStats {
                dye: control.row.dye.clone(),
                primary_channel: control.primary_channel.clone(),
                total_events: control.detector_events.nrows(),
                gated_events: gate.gated_indices.len(),
                strict_points: gate.strict_points,
                bound_maxima: gate.bound_maxima,
                region_maxima: gate.region_maxima,
            })
            .collect::<Vec<_>>();

        info!("autospread pipeline: estimating spread matrix");
        let detector_names = loaded
            .detectors
            .iter()
            .map(|detector| detector.channel.clone())
            .collect::<Vec<_>>();
        let (spread_matrix, spread_stats) =
            estimate_spread_matrix(&loaded.controls, &gates, &detector_names)?;

        let result = AutospreadResult {
            dyes: loaded.dyes.clone(),
            detectors: detector_names,
            channels: loaded.primary_channels.clone(),
            primary_channels: loaded.primary_channels.clone(),
            spread_matrix: matrix_to_rows(&spread_matrix),
            gate_stats,
            spread_stats,
        };

        if let Some(debug_dir) = config.debug_dir.as_deref() {
            info!(
                "autospread pipeline: writing debug artifacts to {}",
                debug_dir.display()
            );
            write_autospread_debug_artifacts(debug_dir, &loaded, &gates, &result)?;
        }

        info!("autospread pipeline: completed successfully");
        Ok(result)
    }
}

#[allow(clippy::too_many_lines)]
fn load_controls(
    manifest_rows: &[ControlsManifestRow],
    input_dir: &Path,
    config: &AutospillConfig,
) -> Result<LoadedControls, AutospillError> {
    let dyes = manifest_rows
        .iter()
        .map(|row| row.dye.clone())
        .collect::<Vec<_>>();
    let prepared_controls = parallel_try_map::<_, _, AutospillError, _>(
        manifest_rows,
        "control loading",
        |dye_index, row| {
            let path = input_dir.join(&row.filename);
            if !path.is_file() {
                return Err(AutospillError::MissingControlFile { path });
            }

            debug!(
                "loading control `{}` for dye `{}` from {}",
                row.filename,
                row.dye,
                path.display()
            );
            let dataset = FcsReader::open(&path)
                .and_then(FcsReader::read)
                .and_then(|dataset| cytiq_fcs::ensure_linear_scale(&dataset).map(|()| dataset))
                .map_err(|source| AutospillError::FcsRead {
                    path: path.clone(),
                    source,
                })?;
            let event_count = dataset.data.columns.first().map_or(0, ColumnData::len);
            if event_count == 0 {
                return Err(AutospillError::EmptyControl { path });
            }
            // Spillover and spread are estimated from raw linear detector
            // values; autospill applies its own transforms.
            let state =
                SignalState::of(&dataset.text).map_err(|source| AutospillError::FcsRead {
                    path: path.clone(),
                    source,
                })?;
            if !state.is_raw() {
                return Err(AutospillError::ProcessedControl {
                    path,
                    state: state.to_string(),
                });
            }
            let primary_channel =
                infer_primary_channel(&row.dye, &path, &dataset.parameters, &config.detectors)?;
            debug!(
                "resolved control `{}` primary channel to `{}` with {} parameters and {} events",
                row.dye,
                primary_channel,
                dataset.parameters.len(),
                event_count
            );

            Ok(PreparedControl {
                row: row.clone(),
                path,
                dataset,
                primary_channel,
                dye_index,
                is_autofluorescence: is_autofluorescence_dye(&row.dye),
            })
        },
    )?;

    let primary_channels = prepared_controls
        .iter()
        .map(|control| control.primary_channel.clone())
        .collect::<Vec<_>>();
    validate_unique_primary_channels(&primary_channels)?;
    debug!(
        "validated {} unique inferred primary channels",
        primary_channels.len()
    );

    let detectors = if config.detectors.is_empty() {
        info!("detector order not provided; using inferred primary-channel order");
        primary_channels
            .iter()
            .map(|channel| DetectorSpec {
                channel: channel.clone(),
                label: None,
                wavelength: None,
            })
            .collect::<Vec<_>>()
    } else {
        info!(
            "using {} explicit detectors from configuration",
            config.detectors.len()
        );
        config.detectors.clone()
    };
    let detector_names = detectors
        .iter()
        .map(|detector| detector.channel.clone())
        .collect::<Vec<_>>();
    debug!("resolved detector order: {}", detector_names.join(", "));
    let synthetic_autofluorescence_channels = prepared_controls
        .iter()
        .filter(|control| control.is_autofluorescence)
        .map(|control| control.primary_channel.clone())
        .collect::<HashSet<_>>();
    let dye_transforms = resolve_transforms(&dyes, &config.transforms);
    debug!(
        "resolved {} dye transforms for {} dyes",
        dye_transforms.len(),
        dyes.len()
    );

    let controls = prepared_controls
        .into_iter()
        .map(|control| {
            let PreparedControl {
                row,
                path,
                dataset,
                primary_channel,
                dye_index,
                is_autofluorescence,
            } = control;

            let event_count = dataset.data.columns.first().map_or(0, ColumnData::len);
            let resolution = dataset.parameters.first().and_then(parameter_range);
            let primary_detector_index = resolve_primary_detector_index(
                &dataset.parameters,
                &primary_channel,
                &detector_names,
                is_autofluorescence,
            )?;
            let fsc_index =
                find_parameter_index(&dataset.parameters, &config.fsc).ok_or_else(|| {
                    AutospillError::MissingChannel {
                        path: path.clone(),
                        channel: config.fsc.clone(),
                    }
                })?;
            let ssc_index =
                find_parameter_index(&dataset.parameters, &config.ssc).ok_or_else(|| {
                    AutospillError::MissingChannel {
                        path: path.clone(),
                        channel: config.ssc.clone(),
                    }
                })?;

            let resolved_detector_columns = detector_names
                .iter()
                .map(|channel| {
                    find_parameter_index(&dataset.parameters, channel)
                        .map(|index| control_column(&dataset, index))
                })
                .collect::<Vec<_>>();
            let autofluorescence_proxy = if is_autofluorescence {
                autofluorescence_proxy(
                    &detector_names,
                    &resolved_detector_columns,
                    &synthetic_autofluorescence_channels,
                    event_count,
                )
            } else {
                vec![0.0; event_count]
            };
            let detector_columns = detector_names
                .iter()
                .zip(resolved_detector_columns)
                .map(|(channel, column)| {
                    if let Some(column) = column {
                        Ok(column)
                    } else if synthetic_autofluorescence_channels.contains(channel) {
                        if is_autofluorescence && channel == &primary_channel {
                            Ok(autofluorescence_proxy.clone())
                        } else {
                            Ok(vec![0.0; event_count])
                        }
                    } else {
                        Err(AutospillError::MissingChannel {
                            path: path.clone(),
                            channel: channel.clone(),
                        })
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let fsc_values = control_column(&dataset, fsc_index);
            let ssc_values = control_column(&dataset, ssc_index);

            let mut detector_events = Array2::zeros((event_count, detector_names.len()));
            for (column_index, column) in detector_columns.iter().enumerate() {
                for (row_index, value) in column.iter().enumerate() {
                    detector_events[(row_index, column_index)] = *value;
                }
            }
            let scatter = fsc_values
                .iter()
                .zip(&ssc_values)
                .map(|(fsc, ssc)| [*fsc, *ssc])
                .collect::<Vec<_>>();

            Ok(ControlData {
                row,
                path,
                primary_channel,
                detector_events,
                scatter,
                resolution,
                primary_detector_index,
                dye_index,
            })
        })
        .collect::<Result<Vec<_>, AutospillError>>()?;

    debug!(
        "prepared {} control matrices with detector width {}",
        controls.len(),
        detector_names.len()
    );
    Ok(LoadedControls {
        dyes,
        primary_channels,
        detectors,
        dye_transforms,
        controls,
    })
}

fn validate_unique_primary_channels(primary_channels: &[String]) -> Result<(), AutospillError> {
    let mut seen_channels = HashSet::new();
    for channel in primary_channels {
        if !seen_channels.insert(channel.clone()) {
            return Err(AutospillError::DuplicatePrimaryChannel {
                value: channel.clone(),
            });
        }
    }

    Ok(())
}

fn infer_primary_channel(
    dye: &str,
    path: &Path,
    parameters: &[Parameter],
    detectors: &[DetectorSpec],
) -> Result<String, AutospillError> {
    if is_autofluorescence_dye(dye) {
        debug!(
            "inferring synthetic autofluorescence primary channel for {}",
            path.display()
        );
        infer_autofluorescence_primary_channel(path, parameters, detectors)
    } else {
        infer_primary_channel_from_dye(dye, path, parameters)
    }
}

fn infer_primary_channel_from_dye(
    dye: &str,
    path: &Path,
    parameters: &[Parameter],
) -> Result<String, AutospillError> {
    let normalized_dye = normalize_identifier(dye);
    debug!(
        "matching dye `{}` against {} FCS parameters in {}",
        dye,
        parameters.len(),
        path.display()
    );
    let name_matches = parameters
        .iter()
        .filter(|parameter| normalize_identifier(&parameter.name) == normalized_dye)
        .collect::<Vec<_>>();
    if let Some(primary_channel) = resolve_unique_primary_channel_match(path, dye, &name_matches)? {
        return Ok(primary_channel);
    }

    let label_matches = parameters
        .iter()
        .filter(|parameter| {
            parameter
                .label
                .as_deref()
                .is_some_and(|label| normalize_identifier(label) == normalized_dye)
        })
        .collect::<Vec<_>>();
    if let Some(primary_channel) = resolve_unique_primary_channel_match(path, dye, &label_matches)?
    {
        return Ok(primary_channel);
    }

    Err(AutospillError::PrimaryChannelNotFound {
        path: path.to_path_buf(),
        dye: dye.to_owned(),
        available_channels: parameters
            .iter()
            .map(display_parameter_identifier)
            .collect(),
    })
}

fn resolve_unique_primary_channel_match(
    path: &Path,
    dye: &str,
    matches: &[&Parameter],
) -> Result<Option<String>, AutospillError> {
    match matches {
        [] => Ok(None),
        [parameter] => Ok(Some(parameter.name.clone())),
        _ => Err(AutospillError::AmbiguousPrimaryChannel {
            path: path.to_path_buf(),
            dye: dye.to_owned(),
            matches: matches
                .iter()
                .map(|parameter| display_parameter_identifier(parameter))
                .collect(),
        }),
    }
}

fn infer_autofluorescence_primary_channel(
    path: &Path,
    parameters: &[Parameter],
    detectors: &[DetectorSpec],
) -> Result<String, AutospillError> {
    if detectors.is_empty() {
        return Err(AutospillError::AutofluorescenceRequiresDetectors {
            path: path.to_path_buf(),
        });
    }

    let candidates = detectors
        .iter()
        .filter(|detector| find_parameter_index(parameters, &detector.channel).is_none())
        .map(|detector| detector.channel.clone())
        .collect::<Vec<_>>();
    debug!(
        "autofluorescence detector candidates for {}: {}",
        path.display(),
        candidates.join(", ")
    );
    match candidates.as_slice() {
        [channel] => Ok(channel.clone()),
        _ => Err(AutospillError::AutofluorescenceDetectorResolution {
            path: path.to_path_buf(),
            candidates,
        }),
    }
}

fn resolve_primary_detector_index(
    parameters: &[Parameter],
    primary_channel: &str,
    detector_names: &[String],
    is_autofluorescence: bool,
) -> Result<usize, AutospillError> {
    let primary_detector_index = if is_autofluorescence {
        detector_names
            .iter()
            .position(|channel| channel == primary_channel)
    } else {
        detector_names.iter().position(|channel| {
            find_parameter_index(parameters, channel)
                .is_some_and(|index| parameters[index].name == primary_channel)
        })
    };
    primary_detector_index.ok_or_else(|| AutospillError::MissingDetector {
        channel: primary_channel.to_owned(),
    })
}

fn is_autofluorescence_dye(dye: &str) -> bool {
    normalize_identifier(dye) == AUTOFLUORESCENCE_DYE
}

fn normalize_identifier(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase();
    for suffix in ["-a", "-h", "-w"] {
        if let Some(stripped) = normalized.strip_suffix(suffix) {
            return stripped.to_owned();
        }
    }

    normalized
}

fn display_parameter_identifier(parameter: &Parameter) -> String {
    match parameter.label.as_deref() {
        Some(label) if label != parameter.name => format!("{} [{}]", parameter.name, label),
        _ => parameter.name.clone(),
    }
}

fn autofluorescence_proxy(
    detector_names: &[String],
    resolved_detector_columns: &[Option<Vec<f64>>],
    synthetic_autofluorescence_channels: &HashSet<String>,
    event_count: usize,
) -> Vec<f64> {
    let real_columns = detector_names
        .iter()
        .zip(resolved_detector_columns)
        .filter(|(channel, _)| !synthetic_autofluorescence_channels.contains(*channel))
        .filter_map(|(_, column)| column.as_ref())
        .collect::<Vec<_>>();
    if real_columns.is_empty() {
        return vec![0.0; event_count];
    }

    let divisor = real_columns.len() as f64;
    (0..event_count)
        .map(|row_index| {
            real_columns
                .iter()
                .map(|column| column[row_index].max(0.0))
                .sum::<f64>()
                / divisor
        })
        .collect()
}

fn estimate_spillover(
    controls: &[ControlData],
    gates: &[GateComputation],
    detector_count: usize,
) -> Result<DMatrix<f64>, AutospillError> {
    debug!(
        "estimating initial spillover from {} controls into {} detectors",
        controls.len(),
        detector_count
    );
    let dye_count = controls.len();
    let detector_primary_channels = (0..detector_count)
        .map(|detector_index| {
            controls
                .iter()
                .find(|candidate| candidate.primary_detector_index == detector_index)
                .map_or_else(
                    || format!("detector:{detector_index}"),
                    |candidate| candidate.primary_channel.clone(),
                )
        })
        .collect::<Vec<_>>();
    let coefficient_rows = parallel_try_map::<_, _, AutospillError, _>(
        controls,
        "initial spillover estimation",
        |index, control| {
            let gate = &gates[index];
            let primary_values = gated_column(
                &control.detector_events,
                &gate.gated_indices,
                control.primary_detector_index,
            );
            let mut coefficients = vec![0.0; detector_count];
            coefficients[control.primary_detector_index] = 1.0;

            for detector_index in 0..detector_count {
                if detector_index == control.primary_detector_index {
                    continue;
                }

                let secondary_values = gated_column(
                    &control.detector_events,
                    &gate.gated_indices,
                    detector_index,
                );
                let regression = fit_pair_regression(
                    &primary_values,
                    &secondary_values,
                    &control.path,
                    &control.primary_channel,
                    &detector_primary_channels[detector_index],
                )?;
                coefficients[detector_index] = regression.slope;
            }

            Ok(coefficients)
        },
    )?;
    let coefficients = coefficient_rows.into_iter().flatten().collect::<Vec<_>>();

    Ok(DMatrix::from_row_slice(
        dye_count,
        detector_count,
        &coefficients,
    ))
}

#[allow(clippy::too_many_lines)]
fn refine_spillover(
    loaded: &LoadedControls,
    gates: &[GateComputation],
    initial_spillover: &DMatrix<f64>,
) -> Result<
    (
        DMatrix<f64>,
        DMatrix<f64>,
        ConvergenceSummary,
        RegressionMatrices,
    ),
    AutospillError,
> {
    debug!(
        "starting spillover refinement for {} controls across {} detectors",
        loaded.controls.len(),
        loaded.detectors.len()
    );
    let dye_count = loaded.controls.len();
    let detector_count = loaded.detectors.len();
    let identity = DMatrix::identity(dye_count, dye_count);
    let primary_detector_indices = loaded
        .controls
        .iter()
        .map(|control| control.primary_detector_index)
        .collect::<Vec<_>>();

    let mut spillover_current =
        basis_spillover(dye_count, detector_count, &primary_detector_indices);
    let mut spillover_update = initial_spillover - &spillover_current;
    let mut scale = RefinementScale::Linear;
    let mut lambda = RS_LAMBDA_COARSE;
    let mut delta = -1.0;
    let mut delta_threshold = RS_DELTA_THRESHOLD_LINEAR;
    let mut delta_history = [-1.0; RS_DELTA_HISTORY];
    let mut iteration = 0_usize;
    let mut final_iteration = false;
    let mut steps = Vec::new();
    loop {
        spillover_current += &spillover_update;
        normalize_rows_on_primary(&mut spillover_current, &primary_detector_indices)?;
        let compensation = compensation_matrix(&spillover_current)?;
        let compensation_error = compensation_error(loaded, gates, &compensation, scale)?;
        let slope_error = compensation_error.slopes.clone() - identity.clone();

        let previous_delta = delta;
        delta = sample_standard_deviation_iter(slope_error.iter().copied());
        let delta_max = slope_error
            .iter()
            .map(|value| value.abs())
            .fold(0.0, f64::max);

        let history_slot = iteration % RS_DELTA_HISTORY;
        delta_history[history_slot] = if previous_delta >= 0.0 {
            delta - previous_delta
        } else {
            -1.0
        };
        let mut delta_change = delta_history.iter().sum::<f64>() / (delta_history.len() as f64);

        steps.push(ConvergenceStep {
            iteration,
            scale: match scale {
                RefinementScale::Linear => "linear".to_owned(),
                RefinementScale::BiExponential => "bi-exp".to_owned(),
            },
            lambda,
            delta,
            delta_max,
            delta_change,
        });
        debug!(
            "refinement iteration {iteration}: scale={scale:?}, lambda={lambda:.6}, delta={delta:.6}, delta_max={delta_max:.6}, delta_change={delta_change:.6}"
        );

        if scale == RefinementScale::Linear && delta_max < delta_threshold {
            debug!("switching refinement scale from linear to bi-exponential");
            scale = RefinementScale::BiExponential;
            delta_threshold = RS_DELTA_THRESHOLD_BIEXP;
            lambda = RS_LAMBDA_COARSE;
            delta = -1.0;
            delta_history.fill(-1.0);
            // refine.spillover resets rs.delta.change <- -1 on every switch
            delta_change = -1.0;
        }
        if delta_change > -RS_DELTA_THRESHOLD_CHANGE
            && (lambda - RS_LAMBDA_COARSE).abs() <= f64::EPSILON
        {
            debug!("switching refinement lambda from coarse to fine");
            lambda = RS_LAMBDA_FINE;
            delta = -1.0;
            delta_history.fill(-1.0);
            delta_change = -1.0;
        }

        let converged = matches!(scale, RefinementScale::BiExponential)
            && (delta_max < delta_threshold || delta_change > -RS_DELTA_THRESHOLD_CHANGE);
        let exit = (converged && final_iteration)
            || (delta_change > -RS_DELTA_THRESHOLD_CHANGE
                && matches!(scale, RefinementScale::Linear))
            || (!converged && iteration == RS_ITER_MAX)
            || iteration > RS_ITER_MAX;
        final_iteration = converged;
        iteration += 1;

        spillover_update = lambda * (slope_error * &spillover_current);

        if exit {
            if !converged {
                return Err(AutospillError::NonConvergent {
                    iterations: iteration,
                    delta_max,
                });
            }

            info!(
                "spillover refinement converged after {iteration} iterations with final delta_max {delta_max:.6}"
            );
            return Ok((
                spillover_current,
                compensation,
                ConvergenceSummary {
                    converged,
                    iterations: iteration,
                    final_delta_max: delta_max,
                    steps,
                },
                compensation_error,
            ));
        }
    }
}

fn compensation_error(
    loaded: &LoadedControls,
    gates: &[GateComputation],
    compensation: &DMatrix<f64>,
    scale: RefinementScale,
) -> Result<RegressionMatrices, AutospillError> {
    let dye_count = loaded.controls.len();
    let compensation_array = dmat_to_array2(compensation);
    let row_results = parallel_try_map::<_, _, AutospillError, _>(
        &loaded.controls,
        "compensation error estimation",
        |index, control| {
            let gate = &gates[index];
            let unmixed_linear = control.detector_events.dot(&compensation_array);
            let primary_linear =
                gated_column(&unmixed_linear, &gate.gated_indices, control.dye_index);
            let primary_values = match scale {
                RefinementScale::Linear => primary_linear.clone(),
                RefinementScale::BiExponential => primary_linear
                    .iter()
                    .map(|value| loaded.dye_transforms[control.dye_index].apply(*value))
                    .collect(),
            };

            let mut slopes = vec![0.0; dye_count];
            let mut skew_values = vec![0.0; dye_count];
            slopes[control.dye_index] = 1.0;

            for secondary_index in 0..dye_count {
                if secondary_index == control.dye_index {
                    continue;
                }

                let secondary_linear =
                    gated_column(&unmixed_linear, &gate.gated_indices, secondary_index);
                let secondary_values = match scale {
                    RefinementScale::Linear => secondary_linear.clone(),
                    RefinementScale::BiExponential => secondary_linear
                        .iter()
                        .map(|value| loaded.dye_transforms[secondary_index].apply(*value))
                        .collect(),
                };
                let regression = fit_pair_regression(
                    &primary_values,
                    &secondary_values,
                    &control.path,
                    &control.row.dye,
                    &loaded.dyes[secondary_index],
                )?;

                match scale {
                    RefinementScale::Linear => {
                        slopes[secondary_index] = regression.slope;
                        let (_, trimmed_secondary) =
                            trim_joint_pairs(&primary_values, &secondary_values, RLM_TRIM_FACTOR);
                        skew_values[secondary_index] = skewness(&trimmed_secondary);
                    }
                    RefinementScale::BiExponential => {
                        let (trimmed_primary, trimmed_secondary) =
                            trim_joint_pairs(&primary_values, &secondary_values, RLM_TRIM_FACTOR);
                        let primary_low = trimmed_primary
                            .iter()
                            .copied()
                            .fold(f64::INFINITY, f64::min);
                        let primary_high = trimmed_primary
                            .iter()
                            .copied()
                            .fold(f64::NEG_INFINITY, f64::max);
                        slopes[secondary_index] = transformed_slope_to_linear(
                            &loaded.dye_transforms[secondary_index],
                            &loaded.dye_transforms[control.dye_index],
                            regression.intercept,
                            regression.slope,
                            primary_low,
                            primary_high,
                        );
                        skew_values[secondary_index] = skewness(
                            &trimmed_secondary
                                .iter()
                                .map(|value| loaded.dye_transforms[secondary_index].inverse(*value))
                                .collect::<Vec<_>>(),
                        );
                    }
                }
            }

            Ok((slopes, skew_values))
        },
    )?;
    let mut slopes = Vec::with_capacity(dye_count * dye_count);
    let mut skew_values = Vec::with_capacity(dye_count * dye_count);
    for (control_slopes, control_skew_values) in row_results {
        slopes.extend(control_slopes);
        skew_values.extend(control_skew_values);
    }

    Ok(RegressionMatrices {
        slopes: DMatrix::from_row_slice(dye_count, dye_count, &slopes),
        skewness: DMatrix::from_row_slice(dye_count, dye_count, &skew_values),
    })
}

impl SpreadFit {
    fn unfitted(detector: String, status: SpreadFitStatus) -> Self {
        Self {
            detector,
            status,
            primary_median: Vec::new(),
            secondary_robust_sd: Vec::new(),
            skipped_bins: 0,
            first_intercept: None,
            first_slope: None,
            sigma0: None,
            unclamped_coefficient: None,
            coefficient: 0.0,
            residuals: Vec::new(),
        }
    }
}

/// The spread of `secondary_values` with the primary signal, over the
/// primary quantile `bins`: robust SD per bin, the noise floor `sigma0` from
/// a first fit on `sqrt_like(primary)`, then the through-origin slope of the
/// noise-corrected spread, clamped at zero.
fn fit_spread(
    detector: String,
    primary_values: &[f64],
    secondary_values: &[f64],
    bins: &[Vec<usize>],
) -> SpreadFit {
    let mut fit = SpreadFit::unfitted(detector, SpreadFitStatus::TooFewBins);
    for bin in bins {
        let primary_bin = bin
            .iter()
            .map(|&bin_index| primary_values[bin_index])
            .collect::<Vec<_>>();
        let secondary_bin = bin
            .iter()
            .map(|&bin_index| secondary_values[bin_index])
            .collect::<Vec<_>>();
        if primary_bin.len() < 3 || secondary_bin.len() < 3 {
            fit.skipped_bins += 1;
            continue;
        }
        fit.primary_median
            .push(crate::regression::median(&primary_bin));
        fit.secondary_robust_sd
            .push(robust_standard_deviation(&secondary_bin));
    }
    if fit.primary_median.len() < 2 {
        return fit;
    }

    let x_first = fit
        .primary_median
        .iter()
        .map(|value| sqrt_like_transform(*value))
        .collect::<Vec<_>>();
    let Some(first_regression) = ordinary_linear_regression(&x_first, &fit.secondary_robust_sd)
    else {
        fit.status = SpreadFitStatus::FirstFitFailed;
        return fit;
    };
    let sigma0 = first_regression.intercept.max(0.0);
    fit.first_intercept = Some(first_regression.intercept);
    fit.first_slope = Some(first_regression.slope);
    fit.sigma0 = Some(sigma0);

    let adjusted_sigma = fit
        .secondary_robust_sd
        .iter()
        .map(|value| sqrt_like_transform((value * value - sigma0 * sigma0).max(0.0)))
        .collect::<Vec<_>>();
    let Some(second_regression) =
        ordinary_linear_regression_without_intercept(&x_first, &adjusted_sigma)
    else {
        fit.status = SpreadFitStatus::SecondFitFailed;
        return fit;
    };
    fit.status = SpreadFitStatus::Fitted;
    fit.unclamped_coefficient = Some(second_regression.slope);
    fit.coefficient = second_regression.slope.max(0.0);
    fit.residuals = adjusted_sigma
        .iter()
        .zip(&x_first)
        .map(|(y, x)| y - second_regression.slope * x)
        .collect();
    fit
}

fn estimate_spread_matrix(
    controls: &[ControlData],
    gates: &[GateComputation],
    detectors: &[String],
) -> Result<(DMatrix<f64>, Vec<SpreadStats>), AutospillError> {
    let detector_count = detectors.len();
    debug!(
        "estimating spread matrix from {} controls into {} detectors",
        controls.len(),
        detector_count
    );
    let dye_count = controls.len();
    let row_results = parallel_try_map(controls, "spread matrix estimation", |index, control| {
        let gate = &gates[index];
        let primary_values = gated_column(
            &control.detector_events,
            &gate.gated_indices,
            control.primary_detector_index,
        );
        let bin_count = quantile_bin_count(primary_values.len());
        let bins = quantile_bins(&primary_values, bin_count);
        if bins.len() < 2 {
            return Err(AutospillError::AutospreadFailed);
        }

        let fits = (0..detector_count)
            .map(|detector_index| {
                let detector = detectors[detector_index].clone();
                if detector_index == control.primary_detector_index {
                    return SpreadFit::unfitted(detector, SpreadFitStatus::Primary);
                }
                let secondary_values = gated_column(
                    &control.detector_events,
                    &gate.gated_indices,
                    detector_index,
                );
                fit_spread(detector, &primary_values, &secondary_values, &bins)
            })
            .collect::<Vec<_>>();
        let coefficients = fits.iter().map(|fit| fit.coefficient).collect::<Vec<_>>();
        let non_zero_coefficients = coefficients.iter().filter(|value| **value > 0.0).count();

        let stats = SpreadStats {
            dye: control.row.dye.clone(),
            primary_channel: control.primary_channel.clone(),
            quantile_bins: bins.len(),
            non_zero_coefficients,
            fits,
        };
        debug!(
            "spread stats for `{}`: {} bins, {} non-zero coefficients",
            control.row.dye,
            bins.len(),
            non_zero_coefficients
        );

        Ok((coefficients, stats))
    })?;
    let mut values = Vec::with_capacity(dye_count * detector_count);
    let mut stats = Vec::with_capacity(dye_count);
    for (coefficients, spread_stats) in row_results {
        values.extend(coefficients);
        stats.push(spread_stats);
    }

    Ok((
        DMatrix::from_row_slice(dye_count, detector_count, &values),
        stats,
    ))
}

fn basis_spillover(
    dye_count: usize,
    detector_count: usize,
    primary_detector_indices: &[usize],
) -> DMatrix<f64> {
    let mut values = vec![0.0; dye_count * detector_count];
    for (row, &primary_detector_index) in primary_detector_indices.iter().enumerate() {
        values[row * detector_count + primary_detector_index] = 1.0;
    }
    DMatrix::from_row_slice(dye_count, detector_count, &values)
}

fn normalize_rows_on_primary(
    matrix: &mut DMatrix<f64>,
    primary_detector_indices: &[usize],
) -> Result<(), AutospillError> {
    for (row, &primary_detector_index) in primary_detector_indices.iter().enumerate() {
        let diagonal = matrix[(row, primary_detector_index)];
        if diagonal.abs() <= f64::EPSILON {
            return Err(AutospillError::SingularMatrix);
        }
        for column in 0..matrix.ncols() {
            matrix[(row, column)] /= diagonal;
        }
    }
    Ok(())
}

fn compensation_matrix(spillover: &DMatrix<f64>) -> Result<DMatrix<f64>, AutospillError> {
    if spillover.nrows() == spillover.ncols() {
        spillover
            .clone()
            .try_inverse()
            .ok_or(AutospillError::SingularMatrix)
    } else {
        spillover
            .clone()
            .pseudo_inverse(PSEUDO_INVERSE_EPSILON)
            .map_err(|_| AutospillError::IllConditionedPseudoInverse)
    }
}

fn fit_pair_regression(
    primary_values: &[f64],
    secondary_values: &[f64],
    path: &Path,
    primary_channel: &str,
    secondary_channel: &str,
) -> Result<RegressionResult, AutospillError> {
    let (trimmed_primary, trimmed_secondary) =
        trim_joint_pairs(primary_values, secondary_values, RLM_TRIM_FACTOR);
    robust_linear_regression(&trimmed_primary, &trimmed_secondary, RLM_ITER_MAX).ok_or_else(|| {
        AutospillError::RegressionFailed {
            path: path.to_path_buf(),
            primary_channel: primary_channel.to_owned(),
            secondary_channel: secondary_channel.to_owned(),
        }
    })
}

fn gated_column(matrix: &Array2<f64>, indices: &[usize], column: usize) -> Vec<f64> {
    indices
        .iter()
        .map(|&index| matrix[(index, column)])
        .collect()
}

/// `$PnR` of a parameter.
fn parameter_range(parameter: &Parameter) -> Option<f64> {
    let key = format!("$P{}R", parameter.index);
    parameter
        .keywords
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&key))
        .and_then(|(_, value)| value.trim().parse::<f64>().ok())
}

/// A control column as autospill reads it: `flowCore::read.FCS` keeps its
/// default `truncate_max_range = TRUE`, which caps values above `$PnR` at
/// `$PnR` (float data can exceed it).
fn control_column(dataset: &FcsDataset, index: usize) -> Vec<f64> {
    let mut values = dataset
        .data
        .column(index)
        .expect("validated parameter index")
        .to_f64_vec();
    if let Some(range) = parameter_range(&dataset.parameters[index]) {
        for value in &mut values {
            if *value > range {
                *value = range;
            }
        }
    }
    values
}

fn find_parameter_index(parameters: &[Parameter], channel: &str) -> Option<usize> {
    parameters
        .iter()
        .position(|parameter| parameter.name == channel)
        .or_else(|| {
            parameters.iter().position(|parameter| {
                parameter
                    .label
                    .as_deref()
                    .is_some_and(|label| label == channel)
            })
        })
}

fn parallel_try_map<T, U, E, F>(items: &[T], stage: &str, worker: F) -> Result<Vec<U>, E>
where
    T: Sync,
    U: Send,
    E: Send,
    F: Fn(usize, &T) -> Result<U, E> + Sync + Send,
{
    if items.len() < 2 {
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| worker(index, item))
            .collect();
    }

    debug!("parallelizing {stage} with rayon for {} tasks", items.len());
    items
        .par_iter()
        .enumerate()
        .map(|(index, item)| worker(index, item))
        .collect::<Vec<_>>()
        .into_iter()
        .collect()
}

fn dmat_to_array2(matrix: &DMatrix<f64>) -> Array2<f64> {
    Array2::from_shape_fn((matrix.nrows(), matrix.ncols()), |(row, column)| {
        matrix[(row, column)]
    })
}

fn matrix_to_rows(matrix: &DMatrix<f64>) -> Vec<Vec<f64>> {
    (0..matrix.nrows())
        .map(|row| {
            (0..matrix.ncols())
                .map(|column| matrix[(row, column)])
                .collect()
        })
        .collect()
}

pub(crate) fn quantile_bin_count(point_count: usize) -> usize {
    (point_count / AUTOSPREAD_TARGET_BIN_SIZE).clamp(AUTOSPREAD_MIN_BINS, AUTOSPREAD_MAX_BINS)
}

pub(crate) fn quantile_bins(values: &[f64], requested_bins: usize) -> Vec<Vec<usize>> {
    if values.is_empty() {
        return Vec::new();
    }

    let mut order = values
        .iter()
        .enumerate()
        .map(|(index, value)| (index, *value))
        .collect::<Vec<_>>();
    order.sort_by(|left, right| left.1.total_cmp(&right.1));

    let bin_count = requested_bins.min(order.len()).max(1);
    let mut bins = Vec::with_capacity(bin_count);
    for bin_index in 0..bin_count {
        let start = bin_index * order.len() / bin_count;
        let end = (bin_index + 1) * order.len() / bin_count;
        if start < end {
            bins.push(order[start..end].iter().map(|(index, _)| *index).collect());
        }
    }
    bins
}

pub(crate) fn sqrt_like_transform(value: f64) -> f64 {
    value.signum() * ((value.abs() + 1.0).sqrt() - 1.0)
}

#[cfg(test)]
mod tests {
    use super::parallel_try_map;

    #[test]
    fn parallel_try_map_preserves_input_order() {
        let values = [7, 3, 9, 1];
        let mapped = parallel_try_map(&values, "test", |index, value| {
            Ok::<_, &'static str>(format!("{index}:{value}"))
        })
        .expect("parallel map should succeed");

        assert_eq!(mapped, vec!["0:7", "1:3", "2:9", "3:1"]);
    }

    #[test]
    fn parallel_try_map_collects_errors_in_input_order() {
        let values = [0, 1, 2];
        let error = parallel_try_map(&values, "test", |index, value| {
            if *value == 1 {
                Err("expected error")
            } else if index == 2 {
                Err("later error")
            } else {
                Ok(*value)
            }
        })
        .expect_err("parallel map should surface the first ordered error");

        assert_eq!(error, "expected error");
    }
}
