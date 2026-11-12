#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::module_name_repetitions,
    clippy::similar_names,
    clippy::too_many_lines
)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use cytiq_fcs::{
    Error as FcsError, FcsDataset, FcsReader, Parameter, Spillover, compensate,
    find_parameter_index,
};
use log::{debug, info};
use nalgebra::DMatrix;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, Uniform};
use rayon::prelude::*;
use serde::Serialize;

use crate::error::CytoScanError;
use crate::export::write_outputs;
use crate::mixture::MclustModel;
use crate::novelty;
use crate::sampling::sample_indices;

pub const DEFAULT_RANDOM_SEED: u64 = 42;
pub const DEFAULT_EVENTS_PER_FILE: usize = 1_000;
pub const DEFAULT_AGGREGATE_EVENTS: usize = 10_000;
pub const DEFAULT_QUANTILE_STEP: f64 = 0.1;
pub const DEFAULT_OUTLIER_THRESHOLD: f64 = 0.5;
pub const DEFAULT_NOVELTY_QUANTILE: f64 = 0.05;
pub const DEFAULT_ISOLATION_TREES: usize = 1_000;
pub const DEFAULT_FINGERPRINT_RECURSIONS: usize = 4;
pub const DEFAULT_ARCSINH_COFACTOR: f64 = 150.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureMethod {
    Summary,
    #[default]
    Quantiles,
    Emd,
    Binning,
    Fingerprint,
}

impl FeatureMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Quantiles => "quantiles",
            Self::Emd => "emd",
            Self::Binning => "binning",
            Self::Fingerprint => "fingerprint",
        }
    }

    const fn uses_aggregate(self) -> bool {
        matches!(self, Self::Emd | Self::Binning | Self::Fingerprint)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagMethod {
    #[default]
    Outlier,
    Novelty,
    Both,
}

impl FlagMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outlier => "outlier",
            Self::Novelty => "novelty",
            Self::Both => "both",
        }
    }

    const fn includes_outlier(self) -> bool {
        matches!(self, Self::Outlier | Self::Both)
    }

    const fn includes_novelty(self) -> bool {
        matches!(self, Self::Novelty | Self::Both)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CytoScanConfig {
    pub inputs: Vec<PathBuf>,
    pub reference_inputs: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub channels: Vec<String>,
    pub feature_method: FeatureMethod,
    pub flag_method: FlagMethod,
    pub debug_dir: Option<PathBuf>,
    /// Fail instead of warning when a file has no spillover metadata or a
    /// selected channel is not in it, so no file is scored on raw values.
    pub strict: bool,
    /// Publish into an `output_dir` that already holds files, replacing them;
    /// otherwise a non-empty `output_dir` is refused before any input is read.
    pub overwrite: bool,
    /// Seeds the per-file event subsample and the isolation trees
    /// ([`DEFAULT_RANDOM_SEED`], as upstream). Results depend on it; vary it
    /// to measure how much.
    pub random_seed: u64,
}

impl CytoScanConfig {
    #[must_use]
    pub fn new(inputs: Vec<PathBuf>, output_dir: PathBuf, channels: Vec<String>) -> Self {
        Self {
            inputs,
            reference_inputs: Vec::new(),
            output_dir,
            channels,
            feature_method: FeatureMethod::Quantiles,
            flag_method: FlagMethod::Outlier,
            debug_dir: None,
            strict: false,
            overwrite: false,
            random_seed: DEFAULT_RANDOM_SEED,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanResult {
    pub parameters: CytoScanParameters,
    pub test_features: CytoScanFeatureMatrix,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_features: Option<CytoScanFeatureMatrix>,
    pub summary_rows: Vec<CytoScanSummaryRow>,
    pub diagnostics: CytoScanDiagnostics,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanParameters {
    pub tool: String,
    pub version: String,
    pub feature_method: FeatureMethod,
    pub flag_method: FlagMethod,
    pub requested_channels: Vec<String>,
    pub resolved_test_inputs: Vec<PathBuf>,
    pub resolved_reference_inputs: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub debug_dir: Option<PathBuf>,
    pub constants: CytoScanConstants,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CytoScanConstants {
    pub random_seed: u64,
    pub events_per_file: usize,
    pub aggregate_events: usize,
    pub quantile_step: f64,
    pub outlier_threshold: f64,
    pub novelty_quantile: f64,
    pub isolation_trees: usize,
    pub fingerprint_recursions: usize,
    pub arcsinh_cofactor: f64,
}

impl CytoScanConstants {
    /// The constants of a run with `config`: the defaults with its seed.
    #[must_use]
    pub fn for_config(config: &CytoScanConfig) -> Self {
        Self {
            random_seed: config.random_seed,
            ..Self::default()
        }
    }
}

impl Default for CytoScanConstants {
    fn default() -> Self {
        Self {
            random_seed: DEFAULT_RANDOM_SEED,
            events_per_file: DEFAULT_EVENTS_PER_FILE,
            aggregate_events: DEFAULT_AGGREGATE_EVENTS,
            quantile_step: DEFAULT_QUANTILE_STEP,
            outlier_threshold: DEFAULT_OUTLIER_THRESHOLD,
            novelty_quantile: DEFAULT_NOVELTY_QUANTILE,
            isolation_trees: DEFAULT_ISOLATION_TREES,
            fingerprint_recursions: DEFAULT_FINGERPRINT_RECURSIONS,
            arcsinh_cofactor: DEFAULT_ARCSINH_COFACTOR,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanFeatureMatrix {
    pub feature_names: Vec<String>,
    pub rows: Vec<CytoScanFeatureRow>,
}

impl CytoScanFeatureMatrix {
    #[must_use]
    pub fn values(&self) -> Vec<Vec<f64>> {
        self.rows.iter().map(|row| row.values.clone()).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanFeatureRow {
    pub path: PathBuf,
    pub filename: String,
    pub values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanSummaryRow {
    pub filename: String,
    pub path: PathBuf,
    pub flag_method: FlagMethod,
    pub feature_method: FeatureMethod,
    pub is_outlier: Option<bool>,
    pub outlier_score: Option<f64>,
    pub is_novelty: Option<bool>,
    pub novelty_log_likelihood: Option<f64>,
    pub channels: String,
    pub warnings: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelPreprocessing {
    pub channel: String,
    pub marker: String,
    pub compensated: bool,
    pub arcsinh_cofactor: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CytoScanDiagnostics {
    pub reference_count: usize,
    pub reference_effective_rank: Option<usize>,
    pub constant_reference_features: Vec<String>,
    /// Threshold is trained in-sample; no held-out calibration is implied.
    pub novelty_calibration_status: String,
    pub novelty_component_effective_counts: Option<Vec<f64>>,
    pub preprocessing: Vec<ChannelPreprocessing>,
    pub aggregate_source: String,
    pub aggregate_files: Vec<PathBuf>,
    pub aggregate_events: usize,
    pub feature_count: usize,
    pub outlier_tree_count: Option<usize>,
    pub novelty_components: Option<usize>,
    /// mclust's name of the fitted covariance structure (`VVI`, `XXX`, ...).
    pub novelty_model: Option<MclustModel>,
    pub novelty_bic: Option<f64>,
    pub novelty_threshold: Option<f64>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedFile {
    pub(crate) path: PathBuf,
    pub(crate) filename: String,
    pub(crate) channel_names: Vec<String>,
    pub(crate) matrix: Vec<Vec<f64>>,
    warnings: Vec<String>,
    preprocessing: Vec<ChannelPreprocessing>,
}

#[derive(Clone, Debug)]
struct ResolvedChannel {
    requested_name: String,
    parameter_name: String,
    display_name: String,
    parameter_index: usize,
    max_range: Option<f64>,
}

#[derive(Clone, Debug)]
struct SpilloverMatrix {
    channels: Vec<String>,
    /// Row-major inverse of the spillover matrix.
    compensation: Vec<f64>,
}

#[derive(Clone, Debug)]
enum FeatureModel {
    None,
    Emd { aggregate_columns: Vec<Vec<f64>> },
    Binning { boundaries: Vec<Vec<f64>> },
    Fingerprint(FingerprintModel),
}

/// `flowFP` probability-binning model: for every level, one split per bin.
#[derive(Clone, Debug, PartialEq)]
struct FingerprintModel {
    levels: Vec<Vec<FingerprintSplit>>,
    bins: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FingerprintSplit {
    channel_index: usize,
    threshold: f64,
}

#[derive(Clone, Debug)]
enum IsolationNode {
    Leaf {
        size: usize,
    },
    Split {
        feature_index: usize,
        threshold: f64,
        left: Box<IsolationNode>,
        right: Box<IsolationNode>,
    },
}

pub struct CytoScanRunner;

impl CytoScanRunner {
    /// Run the CytoScan-style anomaly detection pipeline and write outputs.
    ///
    /// # Errors
    ///
    /// Returns an error when inputs cannot be resolved/read, selected channels
    /// cannot be processed, model fitting fails, or output files cannot be
    /// written.
    pub fn run(config: &CytoScanConfig) -> Result<CytoScanResult, CytoScanError> {
        Self::run_inner(config)
    }

    /// The FCS files a run with `config` reads, `(test, reference)`, with
    /// directories and globs resolved as [`Self::run`] does.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration is invalid or an input cannot
    /// be resolved.
    pub fn input_files(
        config: &CytoScanConfig,
    ) -> Result<(Vec<PathBuf>, Vec<PathBuf>), CytoScanError> {
        validate_config(config)?;
        resolve_test_and_reference(config)
    }

    fn run_inner(config: &CytoScanConfig) -> Result<CytoScanResult, CytoScanError> {
        validate_config(config)?;
        cytiq_publish::check_output_dir(
            &config.output_dir,
            cytiq_publish::Existing::overwrite(config.overwrite),
        )?;
        let (test_paths, reference_paths) = resolve_test_and_reference(config)?;

        info!(
            "cytoscan pipeline: loading {} test file(s), {} reference file(s)",
            test_paths.len(),
            reference_paths.len()
        );
        let test_files = load_files(&test_paths, &config.channels, config.strict)?;
        let reference_files = load_files(&reference_paths, &config.channels, config.strict)?;
        ensure_compatible_preprocessing(&test_files, &reference_files)?;
        let aggregate_files = if config.flag_method.includes_novelty() {
            &reference_files
        } else {
            &test_files
        };
        let aggregate_source = if config.flag_method.includes_novelty() {
            "reference"
        } else {
            "test"
        };
        let seed = config.random_seed;
        let feature_model = build_feature_model(config.feature_method, aggregate_files, seed)?;
        let test_features =
            build_feature_matrix(&test_files, config.feature_method, &feature_model, seed)?;
        let reference_features = if config.flag_method.includes_novelty() {
            Some(build_feature_matrix(
                &reference_files,
                config.feature_method,
                &feature_model,
                seed,
            )?)
        } else {
            None
        };
        let outlier_scores = if config.flag_method.includes_outlier() {
            Some(score_outliers(&test_features.values(), seed))
        } else {
            None
        };
        let novelty = if config.flag_method.includes_novelty() {
            let reference_features = reference_features
                .as_ref()
                .expect("reference features should exist for novelty");
            ensure_feature_shape(reference_features, &test_features)?;
            let model = novelty::fit(&reference_features.values(), DEFAULT_NOVELTY_QUANTILE)?;
            let test_log_likelihoods = test_features
                .values()
                .iter()
                .map(|row| model.log_density(row))
                .collect::<Vec<_>>();
            Some((model, test_log_likelihoods))
        } else {
            None
        };

        let mut warnings = collect_warnings(&test_files, &reference_files);
        let reference_count = reference_features
            .as_ref()
            .map_or(0, |matrix| matrix.rows.len());
        if novelty.is_some() && reference_count <= test_features.feature_names.len() {
            warnings.push(format!(
                "novelty: {reference_count} reference file(s) for {} features: as in mclust, only spherical and diagonal covariance models can be fitted, and the {}% threshold rests on {reference_count} log-densities; add reference files (more than features) for a reliable novelty flag",
                test_features.feature_names.len(),
                DEFAULT_NOVELTY_QUANTILE * 100.0
            ));
        }
        let summary_rows = build_summary_rows(
            config,
            &test_files,
            &test_features,
            outlier_scores.as_deref(),
            novelty.as_ref().map(|(_, values)| values.as_slice()),
            novelty.as_ref().map(|(model, _)| model.threshold),
        );
        let diagnostics = CytoScanDiagnostics {
            reference_count,
            reference_effective_rank: reference_features
                .as_ref()
                .map(|features| reference_rank(&features.values())),
            constant_reference_features: reference_features.as_ref().map_or_else(
                Vec::new,
                |features| {
                    (0..features.feature_names.len())
                        .filter(|&i| {
                            // Constancy is exact, with +0 and -0 equivalent.
                            #[allow(clippy::float_cmp)]
                            features
                                .rows
                                .iter()
                                .all(|row| row.values[i] == features.rows[0].values[i])
                        })
                        .map(|i| features.feature_names[i].clone())
                        .collect()
                },
            ),
            novelty_calibration_status: if novelty.is_none() {
                "not_assessed"
            } else if reference_count <= test_features.feature_names.len()
                || (reference_count as f64 * DEFAULT_NOVELTY_QUANTILE) < 1.0
                || novelty.as_ref().is_some_and(|(model, _)| {
                    model
                        .effective_counts(reference_count)
                        .iter()
                        .any(|&n| n <= test_features.feature_names.len() as f64)
                })
            {
                "insufficient_reference"
            } else {
                "in_sample_only"
            }
            .into(),
            novelty_component_effective_counts: novelty
                .as_ref()
                .map(|(model, _)| model.effective_counts(reference_count)),
            preprocessing: test_files
                .first()
                .map_or_else(Vec::new, |file| file.preprocessing.clone()),
            aggregate_source: aggregate_source.to_owned(),
            aggregate_files: aggregate_files
                .iter()
                .map(|file| file.path.clone())
                .collect(),
            aggregate_events: if config.feature_method.uses_aggregate() {
                aggregate_event_count(aggregate_files)
            } else {
                0
            },
            feature_count: test_features.feature_names.len(),
            outlier_tree_count: config
                .flag_method
                .includes_outlier()
                .then_some(DEFAULT_ISOLATION_TREES),
            novelty_components: novelty.as_ref().map(|(model, _)| model.components()),
            novelty_model: novelty.as_ref().map(|(model, _)| model.model()),
            novelty_bic: novelty.as_ref().map(|(model, _)| model.bic()),
            novelty_threshold: novelty.as_ref().map(|(model, _)| model.threshold),
            warnings,
        };
        let result = CytoScanResult {
            parameters: build_parameters(config, &test_paths, &reference_paths),
            test_features,
            reference_features,
            summary_rows,
            diagnostics,
        };
        write_outputs(config, &result, &test_files, &reference_files)?;
        info!(
            "cytoscan pipeline: wrote outputs for {} test file(s) to {}",
            result.summary_rows.len(),
            config.output_dir.display()
        );

        Ok(result)
    }
}

fn validate_config(config: &CytoScanConfig) -> Result<(), CytoScanError> {
    if config.inputs.is_empty() {
        return Err(CytoScanError::EmptyInputs);
    }
    if config.channels.is_empty() {
        return Err(CytoScanError::EmptyChannels);
    }
    if config.flag_method.includes_novelty() && config.reference_inputs.is_empty() {
        return Err(CytoScanError::MissingReference {
            flag_method: config.flag_method.as_str(),
        });
    }
    Ok(())
}

fn build_parameters(
    config: &CytoScanConfig,
    test_paths: &[PathBuf],
    reference_paths: &[PathBuf],
) -> CytoScanParameters {
    CytoScanParameters {
        tool: "cytiq cytoscan".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        feature_method: config.feature_method,
        flag_method: config.flag_method,
        requested_channels: config.channels.clone(),
        resolved_test_inputs: test_paths.to_vec(),
        resolved_reference_inputs: reference_paths.to_vec(),
        output_dir: config.output_dir.clone(),
        debug_dir: config.debug_dir.clone(),
        constants: CytoScanConstants::for_config(config),
    }
}

fn resolve_test_and_reference(
    config: &CytoScanConfig,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), CytoScanError> {
    let test_paths = resolve_inputs(&config.inputs)?;
    let reference_paths = if config.flag_method.includes_novelty() {
        let paths = resolve_inputs(&config.reference_inputs)?;
        if paths.is_empty() {
            return Err(CytoScanError::EmptyReference);
        }
        paths
    } else {
        Vec::new()
    };
    Ok((test_paths, reference_paths))
}

fn resolve_inputs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, CytoScanError> {
    cytiq_fcs::inputs::resolve_inputs(
        inputs,
        cytiq_fcs::inputs::ResolveOptions {
            order: cytiq_fcs::inputs::InputOrder::Global,
            require_regular_file: true,
        },
    )
    .map_err(Into::into)
}

fn load_files(
    paths: &[PathBuf],
    channels: &[String],
    strict: bool,
) -> Result<Vec<LoadedFile>, CytoScanError> {
    parallel_try_map(paths, "CytoScan FCS preprocessing", |_, path| {
        load_processed_file(path, channels, strict)
    })
}

fn load_processed_file(
    path: &Path,
    requested_channels: &[String],
    strict: bool,
) -> Result<LoadedFile, CytoScanError> {
    debug!("loading FCS file {}", path.display());
    let dataset = FcsReader::open(path)
        .and_then(FcsReader::read)
        .and_then(|dataset| cytiq_fcs::ensure_linear_scale(&dataset).map(|()| dataset))
        .map_err(|source| CytoScanError::FcsRead {
            path: path.to_path_buf(),
            source,
        })?;
    if dataset.event_count() == 0 {
        return Err(CytoScanError::EmptyFcs {
            path: path.to_path_buf(),
        });
    }
    // CytoScan compensates and arcsinh-transforms raw values itself. A file
    // a cytiq stage already compensated, unmixed or transformed has no
    // spillover left and would be scored on a different scale than raw files
    // of the same cohort.
    let state =
        cytiq_fcs::SignalState::of(&dataset.text).map_err(|source| CytoScanError::FcsRead {
            path: path.to_path_buf(),
            source,
        })?;
    if !state.is_raw() {
        return Err(CytoScanError::ProcessedInput {
            path: path.to_path_buf(),
            state: state.to_string(),
        });
    }
    let channels = resolve_channels(path, &dataset, requested_channels)?;
    let spillover = parse_spillover(path, &dataset)?;
    let preprocessing = channels
        .iter()
        .map(|channel| {
            let compensated = spillover
                .as_ref()
                .is_some_and(|matrix| matrix.channels.contains(&channel.parameter_name));
            let marker = dataset.parameters[channel.parameter_index]
                .label
                .as_deref()
                .unwrap_or("");
            ChannelPreprocessing {
                channel: channel.parameter_name.clone(),
                marker: marker.to_owned(),
                compensated,
                arcsinh_cofactor: compensated.then_some(DEFAULT_ARCSINH_COFACTOR),
            }
        })
        .collect();
    let (matrix, warnings) =
        preprocess_matrix(path, &dataset, &channels, spillover.as_ref(), strict)?;
    Ok(LoadedFile {
        path: path.to_path_buf(),
        filename: file_name(path),
        channel_names: channels
            .iter()
            .map(|channel| channel.display_name.clone())
            .collect(),
        matrix,
        warnings,
        preprocessing,
    })
}

fn resolve_channels(
    path: &Path,
    dataset: &FcsDataset,
    requested_channels: &[String],
) -> Result<Vec<ResolvedChannel>, CytoScanError> {
    if requested_channels.is_empty() {
        return Err(CytoScanError::EmptyChannels);
    }

    let mut channels = Vec::with_capacity(requested_channels.len());
    for channel in requested_channels {
        let Some(parameter_index) = find_parameter_index(&dataset.parameters, channel) else {
            return Err(CytoScanError::MissingChannel {
                path: path.to_path_buf(),
                channel: channel.clone(),
                available_channels: dataset
                    .parameters
                    .iter()
                    .map(Parameter::display_identifier)
                    .collect(),
            });
        };
        let parameter = &dataset.parameters[parameter_index];
        channels.push(ResolvedChannel {
            requested_name: channel.clone(),
            parameter_name: parameter.name.clone(),
            display_name: parameter.display_identifier(),
            parameter_index,
            max_range: parameter_max_range(parameter),
        });
    }
    ensure_unique_channels(&channels)?;
    Ok(channels)
}

fn ensure_unique_channels(channels: &[ResolvedChannel]) -> Result<(), CytoScanError> {
    let mut seen = HashSet::new();
    for channel in channels {
        if !seen.insert(channel.parameter_index) {
            return Err(CytoScanError::DuplicateChannel {
                channel: channel.requested_name.clone(),
            });
        }
    }

    Ok(())
}

fn preprocess_matrix(
    path: &Path,
    dataset: &FcsDataset,
    channels: &[ResolvedChannel],
    spillover: Option<&SpilloverMatrix>,
    strict: bool,
) -> Result<(Vec<Vec<f64>>, Vec<String>), CytoScanError> {
    let event_count = dataset.event_count();
    let mut warnings = Vec::new();
    let mut output_columns = selected_raw_columns(path, dataset, channels)?;
    truncate_columns(channels, &mut output_columns);

    if let Some(spillover) = spillover {
        let compensated = compensate_selected_columns(path, dataset, channels, spillover)?;
        for (channel_index, compensated_column) in compensated.into_iter().enumerate() {
            if let Some(column) = compensated_column {
                output_columns[channel_index] = column
                    .into_iter()
                    .map(|value| (value / DEFAULT_ARCSINH_COFACTOR).asinh())
                    .collect();
            } else if strict {
                return Err(CytoScanError::ChannelNotCompensated {
                    path: path.to_path_buf(),
                    channel: channels[channel_index].display_name.clone(),
                });
            } else {
                warnings.push(format!(
                    "selected channel `{}` is not present in spillover metadata; leaving raw values unchanged",
                    channels[channel_index].display_name
                ));
            }
        }
    } else if strict {
        return Err(CytoScanError::MissingSpillover {
            path: path.to_path_buf(),
        });
    } else {
        warnings.push("FCS spillover metadata was not found; selected channels were not compensated or arcsinh transformed".to_owned());
    }

    let matrix = (0..event_count)
        .map(|event_index| {
            output_columns
                .iter()
                .map(|column| column[event_index])
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    Ok((matrix, warnings))
}

fn selected_raw_columns(
    path: &Path,
    dataset: &FcsDataset,
    channels: &[ResolvedChannel],
) -> Result<Vec<Vec<f64>>, CytoScanError> {
    channels
        .iter()
        .map(|channel| {
            let values = dataset
                .data
                .column(channel.parameter_index)
                .expect("parameter index should have a data column")
                .to_f64_vec();
            if values.iter().any(|value| !value.is_finite()) {
                return Err(CytoScanError::NonFiniteChannel {
                    path: path.to_path_buf(),
                    channel: channel.display_name.clone(),
                });
            }
            Ok(values)
        })
        .collect()
}

fn truncate_columns(channels: &[ResolvedChannel], columns: &mut [Vec<f64>]) {
    for (channel, column) in channels.iter().zip(columns) {
        if let Some(max_range) = channel.max_range.filter(|value| value.is_finite()) {
            for value in column {
                if *value > max_range {
                    *value = max_range;
                }
            }
        }
    }
}

fn compensate_selected_columns(
    path: &Path,
    dataset: &FcsDataset,
    selected_channels: &[ResolvedChannel],
    spillover: &SpilloverMatrix,
) -> Result<Vec<Option<Vec<f64>>>, CytoScanError> {
    let spill_indexes = spillover
        .channels
        .iter()
        .map(|channel| {
            find_parameter_index(&dataset.parameters, channel).ok_or_else(|| {
                CytoScanError::InvalidSpillover {
                    path: path.to_path_buf(),
                    reason: format!(
                        "spillover channel `{channel}` was not found in FCS parameters"
                    ),
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut spill_columns = spill_indexes
        .iter()
        .map(|index| {
            dataset
                .data
                .column(*index)
                .expect("spillover parameter index should have a data column")
                .to_f64_vec()
        })
        .collect::<Vec<_>>();
    let spill_channels = spill_indexes
        .iter()
        .map(|index| ResolvedChannel {
            requested_name: dataset.parameters[*index].name.clone(),
            parameter_name: dataset.parameters[*index].name.clone(),
            display_name: dataset.parameters[*index].display_identifier(),
            parameter_index: *index,
            max_range: parameter_max_range(&dataset.parameters[*index]),
        })
        .collect::<Vec<_>>();
    truncate_columns(&spill_channels, &mut spill_columns);

    let compensated_columns = compensate(&spill_columns, &spillover.compensation);
    let spill_lookup = spillover
        .channels
        .iter()
        .enumerate()
        .map(|(index, channel)| (channel.as_str(), index))
        .collect::<HashMap<_, _>>();

    Ok(selected_channels
        .iter()
        .map(|channel| {
            spill_lookup
                .get(channel.parameter_name.as_str())
                .map(|index| compensated_columns[*index].clone())
        })
        .collect())
}

fn parse_spillover(
    path: &Path,
    dataset: &FcsDataset,
) -> Result<Option<SpilloverMatrix>, CytoScanError> {
    let Some(spillover) =
        Spillover::from_dataset(dataset).map_err(|error| invalid_spillover(path, &error))?
    else {
        return Ok(None);
    };
    let size = spillover.size();
    let matrix = DMatrix::from_row_slice(size, size, spillover.matrix());
    let compensation = matrix
        .try_inverse()
        .ok_or_else(|| CytoScanError::SingularSpillover {
            path: path.to_path_buf(),
        })?;

    Ok(Some(SpilloverMatrix {
        channels: spillover.channels().to_vec(),
        compensation: compensation.transpose().as_slice().to_vec(),
    }))
}

fn invalid_spillover(path: &Path, error: &FcsError) -> CytoScanError {
    let reason = match error {
        FcsError::InvalidSpillover(reason) => reason.clone(),
        other => other.to_string(),
    };
    CytoScanError::InvalidSpillover {
        path: path.to_path_buf(),
        reason,
    }
}

fn parameter_max_range(parameter: &Parameter) -> Option<f64> {
    keyword_value(parameter, &format!("$P{}R", parameter.index))
        .or_else(|| keyword_value(parameter, "maxRange"))
        .or_else(|| keyword_value(parameter, "max"))
        .and_then(|value| value.trim().parse::<f64>().ok())
}

fn keyword_value<'a>(parameter: &'a Parameter, key: &str) -> Option<&'a str> {
    parameter
        .keywords
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}

fn collect_warnings(test_files: &[LoadedFile], reference_files: &[LoadedFile]) -> Vec<String> {
    test_files
        .iter()
        .chain(reference_files)
        .flat_map(|file| {
            file.warnings
                .iter()
                .map(|warning| format!("{}: {warning}", file.path.display()))
        })
        .collect()
}

fn ensure_compatible_preprocessing(
    test: &[LoadedFile],
    reference: &[LoadedFile],
) -> Result<(), CytoScanError> {
    if let Some(first) = reference.first().or_else(|| test.first()) {
        for file in test.iter().chain(reference) {
            if file.preprocessing != first.preprocessing {
                return Err(CytoScanError::IncompatiblePreprocessing {
                    path: file.path.clone(),
                    reference: first.path.clone(),
                });
            }
        }
    }
    Ok(())
}

fn reference_rank(values: &[Vec<f64>]) -> usize {
    let n = values.len();
    let d = values.first().map_or(0, Vec::len);
    if n == 0 || d == 0 {
        return 0;
    }
    let means = (0..d)
        .map(|j| values.iter().map(|row| row[j]).sum::<f64>() / n as f64)
        .collect::<Vec<_>>();
    let matrix = DMatrix::from_fn(n, d, |i, j| values[i][j] - means[j]);
    let singular = matrix.svd(false, false).singular_values;
    let max = singular.iter().copied().fold(0.0, f64::max);
    singular
        .iter()
        .filter(|&&v| v > f64::EPSILON * n.max(d) as f64 * max)
        .count()
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

    debug!("parallelizing {stage} over {} item(s)", items.len());
    let results = items
        .par_iter()
        .enumerate()
        .map(|(index, item)| worker(index, item))
        .collect::<Vec<_>>();
    results.into_iter().collect()
}

fn build_feature_model(
    method: FeatureMethod,
    aggregate_files: &[LoadedFile],
    seed: u64,
) -> Result<FeatureModel, CytoScanError> {
    if !method.uses_aggregate() {
        return Ok(FeatureModel::None);
    }
    let aggregate = aggregate_matrix(aggregate_files, seed)?;
    if aggregate.is_empty() {
        return Err(CytoScanError::EmptyAggregate {
            method: method.as_str(),
        });
    }
    match method {
        FeatureMethod::Emd => Ok(FeatureModel::Emd {
            aggregate_columns: transpose(&aggregate),
        }),
        FeatureMethod::Binning => Ok(FeatureModel::Binning {
            boundaries: aggregate_boundaries(&aggregate),
        }),
        FeatureMethod::Fingerprint => Ok(FeatureModel::Fingerprint(FingerprintModel::fit(
            &aggregate,
            DEFAULT_FINGERPRINT_RECURSIONS,
        )?)),
        FeatureMethod::Summary | FeatureMethod::Quantiles => Ok(FeatureModel::None),
    }
}

fn build_feature_matrix(
    files: &[LoadedFile],
    method: FeatureMethod,
    model: &FeatureModel,
    seed: u64,
) -> Result<CytoScanFeatureMatrix, CytoScanError> {
    if files.is_empty() {
        return Err(CytoScanError::EmptyFeatureMatrix);
    }
    let feature_names = feature_names(method, &files[0].channel_names, model);
    let rows = files
        .iter()
        .map(|file| {
            let values = file_features(file, method, model, seed)?;
            Ok(CytoScanFeatureRow {
                path: file.path.clone(),
                filename: file.filename.clone(),
                values,
            })
        })
        .collect::<Result<Vec<_>, CytoScanError>>()?;
    Ok(CytoScanFeatureMatrix {
        feature_names,
        rows,
    })
}

fn file_features(
    file: &LoadedFile,
    method: FeatureMethod,
    model: &FeatureModel,
    seed: u64,
) -> Result<Vec<f64>, CytoScanError> {
    let sample = sampled_matrix(file, DEFAULT_EVENTS_PER_FILE, seed);
    let features = match method {
        FeatureMethod::Summary => summary_features(&sample),
        FeatureMethod::Quantiles => quantile_features(&sample),
        FeatureMethod::Emd => {
            let FeatureModel::Emd { aggregate_columns } = model else {
                return Err(CytoScanError::FeatureShapeMismatch);
            };
            emd_features(&sample, aggregate_columns)
        }
        FeatureMethod::Binning => {
            let FeatureModel::Binning { boundaries } = model else {
                return Err(CytoScanError::FeatureShapeMismatch);
            };
            binning_features(&sample, boundaries)
        }
        FeatureMethod::Fingerprint => {
            let FeatureModel::Fingerprint(model) = model else {
                return Err(CytoScanError::FeatureShapeMismatch);
            };
            fingerprint_features(&sample, model)
        }
    };

    Ok(features)
}

fn feature_names(method: FeatureMethod, channels: &[String], model: &FeatureModel) -> Vec<String> {
    match method {
        FeatureMethod::Summary => channels
            .iter()
            .flat_map(|channel| {
                ["mean", "sd", "median", "IQR"]
                    .into_iter()
                    .map(move |suffix| format!("{channel}_{suffix}"))
            })
            .collect(),
        FeatureMethod::Quantiles => channels
            .iter()
            .flat_map(|channel| {
                (1..=9).map(move |index| format!("{channel}_{:.1}", f64::from(index) * 0.1))
            })
            .collect(),
        FeatureMethod::Emd => channels
            .iter()
            .map(|channel| format!("{channel}_EMD"))
            .collect(),
        FeatureMethod::Binning => channels
            .iter()
            .flat_map(|channel| (1..=10).map(move |index| format!("{channel}_bin{index}")))
            .collect(),
        FeatureMethod::Fingerprint => {
            let bins = match model {
                FeatureModel::Fingerprint(model) => model.bins,
                FeatureModel::None | FeatureModel::Emd { .. } | FeatureModel::Binning { .. } => {
                    1_usize << DEFAULT_FINGERPRINT_RECURSIONS
                }
            };
            (1..=bins)
                .map(|index| format!("fingerprint_bin{index}"))
                .collect()
        }
    }
}

/// Per-file subsample as `CytoScan::readInput`: the generator is reseeded with
/// the run's seed for every file (`set.seed(42)` before
/// `read.FCS(which.lines = n)` upstream), so the sample does not depend on the
/// path or on the other files.
pub(crate) fn sampled_matrix(file: &LoadedFile, limit: usize, seed: u64) -> Vec<Vec<f64>> {
    sample_indices(file.matrix.len(), limit, seed)
        .into_iter()
        .map(|index| file.matrix[index].clone())
        .collect()
}

/// `getAggregate`: `ceiling(aggSize / n_files)` events from every file,
/// concatenated in file order without further resampling.
fn aggregate_matrix(files: &[LoadedFile], seed: u64) -> Result<Vec<Vec<f64>>, CytoScanError> {
    if files.is_empty() {
        return Err(CytoScanError::EmptyAggregate {
            method: "aggregate",
        });
    }
    let per_file = DEFAULT_AGGREGATE_EVENTS.div_ceil(files.len());
    Ok(files
        .iter()
        .flat_map(|file| sampled_matrix(file, per_file, seed))
        .collect())
}

/// Number of aggregate events `aggregate_matrix` uses for `files`.
fn aggregate_event_count(files: &[LoadedFile]) -> usize {
    if files.is_empty() {
        return 0;
    }
    let per_file = DEFAULT_AGGREGATE_EVENTS.div_ceil(files.len());
    files
        .iter()
        .map(|file| file.matrix.len().min(per_file))
        .sum()
}

fn summary_features(sample: &[Vec<f64>]) -> Vec<f64> {
    let columns = transpose(sample);
    columns
        .into_iter()
        .flat_map(|mut column| {
            let mean = mean(&column);
            let sd = sample_standard_deviation(&column, mean);
            let median = quantile_sorted(&mut column, 0.5);
            let q25 = quantile_sorted(&mut column, 0.25);
            let q75 = quantile_sorted(&mut column, 0.75);
            [mean, sd, median, q75 - q25]
        })
        .collect()
}

fn quantile_features(sample: &[Vec<f64>]) -> Vec<f64> {
    transpose(sample)
        .into_iter()
        .flat_map(|mut column| {
            (1..=9).map(move |index| quantile_sorted(&mut column, f64::from(index) * 0.1))
        })
        .collect()
}

fn emd_features(sample: &[Vec<f64>], aggregate_columns: &[Vec<f64>]) -> Vec<f64> {
    transpose(sample)
        .iter()
        .zip(aggregate_columns)
        .map(|(column, aggregate)| wasserstein_1d(column, aggregate))
        .collect()
}

/// `CytoScan::calculateBins`: `table(cut(x, breaks = deciles))`. Intervals are
/// right-closed `(b[i], b[i+1]]`; values at or below the lowest break or above
/// the highest are `NA` and leave both the counts and the denominator.
fn binning_features(sample: &[Vec<f64>], boundaries: &[Vec<f64>]) -> Vec<f64> {
    let columns = transpose(sample);
    columns
        .iter()
        .zip(boundaries)
        .flat_map(|(column, boundary)| {
            let mut counts = [0_usize; 10];
            for value in column {
                if let Some(bin) = cut_bin(*value, boundary) {
                    counts[bin] += 1;
                }
            }
            let total = counts.iter().sum::<usize>();
            counts.map(|count| {
                if total == 0 {
                    0.0
                } else {
                    count as f64 / total as f64
                }
            })
        })
        .collect()
}

/// `CytoScan::calculateFingerprint`: `flowFP(ff, model)` counts per bin
/// divided by their sum.
fn fingerprint_features(sample: &[Vec<f64>], model: &FingerprintModel) -> Vec<f64> {
    let tags = model.tag(&dequantize(sample));
    let mut counts = vec![0_usize; model.bins];
    for tag in tags {
        counts[tag] += 1;
    }
    let total = counts.iter().sum::<usize>().max(1) as f64;
    counts
        .into_iter()
        .map(|count| count as f64 / total)
        .collect()
}

fn aggregate_boundaries(aggregate: &[Vec<f64>]) -> Vec<Vec<f64>> {
    transpose(aggregate)
        .into_iter()
        .map(|mut column| {
            (0..=10)
                .map(|index| quantile_sorted(&mut column, f64::from(index) * 0.1))
                .collect()
        })
        .collect()
}

fn cut_bin(value: f64, boundaries: &[f64]) -> Option<usize> {
    if boundaries.len() < 2 || value <= boundaries[0] {
        return None;
    }
    boundaries
        .windows(2)
        .position(|pair| value > pair[0] && value <= pair[1])
}

fn transpose(matrix: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let Some(first) = matrix.first() else {
        return Vec::new();
    };
    (0..first.len())
        .map(|column_index| matrix.iter().map(|row| row[column_index]).collect())
        .collect()
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn sample_standard_deviation(values: &[f64], mean: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let variance = values
        .iter()
        .map(|value| {
            let delta = value - mean;
            delta * delta
        })
        .sum::<f64>()
        / (values.len() - 1) as f64;
    variance.sqrt()
}

fn quantile_sorted(values: &mut [f64], probability: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let probability = probability.clamp(0.0, 1.0);
    if values.len() == 1 {
        return values[0];
    }
    let position = probability * (values.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        values[lower]
    } else {
        let fraction = position - lower as f64;
        values[lower] * (1.0 - fraction) + values[upper] * fraction
    }
}

fn wasserstein_1d(left: &[f64], right: &[f64]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_by(f64::total_cmp);
    right.sort_by(f64::total_cmp);

    let left_weight = 1.0 / left.len() as f64;
    let right_weight = 1.0 / right.len() as f64;
    let mut left_index = 0;
    let mut right_index = 0;
    let mut left_cdf: f64 = 0.0;
    let mut right_cdf: f64 = 0.0;
    let mut previous = left[0].min(right[0]);
    let mut distance = 0.0;

    while left_index < left.len() || right_index < right.len() {
        let next_left = left.get(left_index).copied().unwrap_or(f64::INFINITY);
        let next_right = right.get(right_index).copied().unwrap_or(f64::INFINITY);
        let next = next_left.min(next_right);
        distance += (left_cdf - right_cdf).abs() * (next - previous);
        while left_index < left.len() && left[left_index] <= next {
            left_cdf += left_weight;
            left_index += 1;
        }
        while right_index < right.len() && right[right_index] <= next {
            right_cdf += right_weight;
            right_index += 1;
        }
        previous = next;
    }

    distance
}

/// `flowFP::dequantize`: event `i` (1-based) gets `i * 1e-8` added to every
/// parameter, which breaks ties before median splits.
fn dequantize(rows: &[Vec<f64>]) -> Vec<Vec<f64>> {
    const ALPHA: f64 = 1.0e-8;
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let offset = ALPHA * (index + 1) as f64;
            row.iter().map(|value| value + offset).collect()
        })
        .collect()
}

impl FingerprintModel {
    /// `flowFP::flowFPModel(agg, nRecursions)` on the dequantized aggregate:
    /// level by level, every bin is split at the median of its
    /// highest-variance parameter.
    fn fit(rows: &[Vec<f64>], recursions: usize) -> Result<Self, CytoScanError> {
        let rows = dequantize(rows);
        let mut tags = vec![0_usize; rows.len()];
        let mut levels = Vec::with_capacity(recursions);
        for level in 0..recursions {
            let bin_count = 1_usize << level;
            let members = bin_members(&tags, bin_count);
            let mut splits = Vec::with_capacity(bin_count);
            for (bin, events) in members.iter().enumerate() {
                let channel_index = find_split_axis(&rows, events).ok_or(
                    CytoScanError::FingerprintBinTooSmall {
                        level: level + 1,
                        bin: bin + 1,
                        events: events.len(),
                        recursions,
                    },
                )?;
                let threshold = median_of(events.iter().map(|&index| rows[index][channel_index]));
                splits.push(FingerprintSplit {
                    channel_index,
                    threshold,
                });
            }
            split_tags(&rows, &mut tags, &splits);
            levels.push(splits);
        }
        Ok(Self {
            levels,
            bins: 1_usize << recursions,
        })
    }

    /// `flowFP`'s `tag_events`: 0-based bin per (already dequantized) event.
    fn tag(&self, rows: &[Vec<f64>]) -> Vec<usize> {
        let mut tags = vec![0_usize; rows.len()];
        for splits in &self.levels {
            split_tags(rows, &mut tags, splits);
        }
        tags
    }
}

fn bin_members(tags: &[usize], bin_count: usize) -> Vec<Vec<usize>> {
    let mut members = vec![Vec::new(); bin_count];
    for (event, tag) in tags.iter().enumerate() {
        members[*tag].push(event);
    }
    members
}

/// `split_bin`: bin `b` becomes `2b` (value <= threshold) or `2b + 1`.
fn split_tags(rows: &[Vec<f64>], tags: &mut [usize], splits: &[FingerprintSplit]) {
    for (row, tag) in rows.iter().zip(tags.iter_mut()) {
        let split = splits[*tag];
        *tag = 2 * *tag + usize::from(row[split.channel_index] > split.threshold);
    }
}

/// `find_split_axis`: the parameter with the largest one-pass sample variance
/// `(sum_sq - sum^2 / n) / (n - 1)`; the first maximum wins and the running
/// maximum starts at `DBL_MIN`, so empty or single-event bins have no axis.
#[allow(clippy::needless_range_loop)]
fn find_split_axis(rows: &[Vec<f64>], events: &[usize]) -> Option<usize> {
    let channel_count = rows.first().map_or(0, Vec::len);
    let n = events.len() as f64;
    let mut best = None;
    let mut max_variance = f64::MIN_POSITIVE;
    for channel_index in 0..channel_count {
        let (sum, sum_squares) = events
            .iter()
            .fold((0.0, 0.0), |(sum, sum_squares), &event| {
                let value = rows[event][channel_index];
                (sum + value, sum_squares + value * value)
            });
        let variance = (sum_squares - sum * sum / n) / (n - 1.0);
        if max_variance < variance {
            max_variance = variance;
            best = Some(channel_index);
        }
    }
    best
}

/// `median` in flowFP's `split_utils.c`.
fn median_of(values: impl Iterator<Item = f64>) -> f64 {
    let mut values = values.collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        f64::midpoint(values[middle - 1], values[middle])
    }
}

fn ensure_feature_shape(
    reference: &CytoScanFeatureMatrix,
    test: &CytoScanFeatureMatrix,
) -> Result<(), CytoScanError> {
    if reference.feature_names == test.feature_names {
        Ok(())
    } else {
        Err(CytoScanError::FeatureShapeMismatch)
    }
}

fn build_summary_rows(
    config: &CytoScanConfig,
    test_files: &[LoadedFile],
    test_features: &CytoScanFeatureMatrix,
    outlier_scores: Option<&[f64]>,
    novelty_log_likelihoods: Option<&[f64]>,
    novelty_threshold: Option<f64>,
) -> Vec<CytoScanSummaryRow> {
    test_features
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let outlier_score = outlier_scores.and_then(|scores| scores.get(index).copied());
            let novelty_log_likelihood =
                novelty_log_likelihoods.and_then(|scores| scores.get(index).copied());
            let is_outlier = outlier_score.map(|score| score >= DEFAULT_OUTLIER_THRESHOLD);
            let is_novelty = novelty_log_likelihood
                .zip(novelty_threshold)
                .map(|(score, threshold)| score <= threshold);
            CytoScanSummaryRow {
                filename: row.filename.clone(),
                path: row.path.clone(),
                flag_method: config.flag_method,
                feature_method: config.feature_method,
                is_outlier,
                outlier_score,
                is_novelty,
                novelty_log_likelihood,
                channels: config.channels.join(","),
                warnings: test_files
                    .get(index)
                    .map(|file| file.warnings.join("; "))
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// `isotree::isolation.forest(sample_size = 1, ntrees = 1000, ndim = 1)` scored
/// with `predict()`: every tree sees all files, depth is limited to
/// `ceiling(log2(n))`, each split picks a non-constant column uniformly and a
/// threshold uniformly between that column's node minimum and maximum, and the
/// standardized score is `2^(-mean depth / c(n))` with `c(n) = 2 (H_n - 1)`.
/// Tree `t` uses `seed + t` (wrapping), as the upstream default seed 42 does.
fn score_outliers(features: &[Vec<f64>], seed: u64) -> Vec<f64> {
    if features.len() < 2 || features.first().is_none_or(Vec::is_empty) {
        return vec![0.0; features.len()];
    }
    let max_depth = (features.len() as f64).log2().ceil() as usize;
    let normalization = expected_average_depth(features.len());
    let indexes = (0..features.len()).collect::<Vec<_>>();
    let depth_sums = (0..DEFAULT_ISOLATION_TREES)
        .into_par_iter()
        .map(|tree_index| {
            let mut rng = StdRng::seed_from_u64(seed.wrapping_add(tree_index as u64));
            let tree = build_isolation_tree(features, &indexes, 0, max_depth, &mut rng);
            features
                .iter()
                .map(|row| tree.path_length(row, 0.0))
                .collect::<Vec<_>>()
        })
        // collected in tree order and summed sequentially so the result does
        // not depend on rayon's scheduling
        .collect::<Vec<_>>()
        .into_iter()
        .fold(vec![0.0; features.len()], |mut totals, depths| {
            for (total, value) in totals.iter_mut().zip(depths) {
                *total += value;
            }
            totals
        });
    depth_sums
        .into_iter()
        .map(|sum| 2_f64.powf(-(sum / DEFAULT_ISOLATION_TREES as f64) / normalization))
        .collect()
}

fn build_isolation_tree(
    features: &[Vec<f64>],
    indexes: &[usize],
    depth: usize,
    max_depth: usize,
    rng: &mut StdRng,
) -> IsolationNode {
    if indexes.len() <= 1 || depth >= max_depth {
        return IsolationNode::Leaf {
            size: indexes.len(),
        };
    }
    let mut candidates = (0..features.first().map_or(0, Vec::len)).collect::<Vec<_>>();
    while !candidates.is_empty() {
        let feature_index =
            candidates.swap_remove(crate::sampling::random_index(rng, candidates.len()));
        let (minimum, maximum) = feature_min_max(features, indexes, feature_index);
        if maximum <= minimum {
            // constant within this node: isotree drops the column and retries
            continue;
        }
        let threshold = Uniform::new(minimum, maximum)
            .expect("nonconstant feature range must be finite")
            .sample(rng);
        let (left, right): (Vec<usize>, Vec<usize>) = indexes
            .iter()
            .partition(|index| features[**index][feature_index] <= threshold);
        if left.is_empty() || right.is_empty() {
            continue;
        }
        return IsolationNode::Split {
            feature_index,
            threshold,
            left: Box::new(build_isolation_tree(
                features,
                &left,
                depth + 1,
                max_depth,
                rng,
            )),
            right: Box::new(build_isolation_tree(
                features,
                &right,
                depth + 1,
                max_depth,
                rng,
            )),
        };
    }
    IsolationNode::Leaf {
        size: indexes.len(),
    }
}

impl IsolationNode {
    fn path_length(&self, row: &[f64], depth: f64) -> f64 {
        match self {
            Self::Leaf { size } => depth + expected_average_depth(*size),
            Self::Split {
                feature_index,
                threshold,
                left,
                right,
            } => {
                if row[*feature_index] <= *threshold {
                    left.path_length(row, depth + 1.0)
                } else {
                    right.path_length(row, depth + 1.0)
                }
            }
        }
    }
}

fn feature_min_max(features: &[Vec<f64>], indexes: &[usize], feature_index: usize) -> (f64, f64) {
    indexes.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(minimum, maximum), index| {
            let value = features[*index][feature_index];
            (minimum.min(value), maximum.max(value))
        },
    )
}

/// isotree's `expected_avg_depth`: `2 (H_n - 1)` with the exact harmonic number
/// (isotree tabulates it up to 512 and uses an asymptotic series above).
fn expected_average_depth(size: usize) -> f64 {
    if size <= 1 {
        return 0.0;
    }
    let harmonic = (1..=size).rev().map(|k| 1.0 / k as f64).sum::<f64>();
    2.0 * (harmonic - 1.0)
}

#[cfg(test)]
#[allow(
    dead_code,
    clippy::approx_constant,
    clippy::unreadable_literal,
    clippy::excessive_precision
)]
mod tests {
    use super::*;

    include!("../../../data/fixtures/cytoscan/reference.rs");

    fn rows<const N: usize>(matrix: &[[f64; N]]) -> Vec<Vec<f64>> {
        matrix.iter().map(|row| row.to_vec()).collect()
    }

    fn column(values: &[f64]) -> Vec<Vec<f64>> {
        values.iter().map(|value| vec![*value]).collect()
    }

    #[test]
    fn emd_matches_transport_wasserstein1d() {
        let distance = wasserstein_1d(&EMD_A, &EMD_B);
        assert!(
            (distance - EMD_EXPECTED).abs() < 1e-12,
            "{distance} vs {EMD_EXPECTED}"
        );
    }

    #[test]
    fn binning_matches_cytoscan_cut_semantics() {
        // includes values equal to the lowest break and outside the range,
        // which `cut()` maps to NA and drops from the denominator
        let boundaries = aggregate_boundaries(&column(&BIN_AGGREGATE));
        let features = binning_features(&column(&BIN_VALUES), &boundaries);
        for (actual, expected) in features.iter().zip(BIN_EXPECTED) {
            assert!((actual - expected).abs() < 1e-12, "{actual} vs {expected}");
        }
    }

    #[test]
    fn fingerprint_matches_flowfp() {
        let model = FingerprintModel::fit(&rows(&FINGERPRINT_AGGREGATE), 4).expect("model");
        let features = fingerprint_features(&rows(&FINGERPRINT_VALUES), &model);
        assert_eq!(features.len(), FINGERPRINT_EXPECTED.len());
        for (actual, expected) in features.iter().zip(FINGERPRINT_EXPECTED) {
            assert!((actual - expected).abs() < 1e-12, "{actual} vs {expected}");
        }
    }

    #[test]
    fn fingerprint_rejects_bins_without_events_like_flowfp() {
        let tiny = vec![vec![1.0, 2.0], vec![2.0, 1.0], vec![3.0, 0.0]];
        assert!(matches!(
            FingerprintModel::fit(&tiny, 4),
            Err(CytoScanError::FingerprintBinTooSmall { .. })
        ));
    }

    #[test]
    fn expected_depth_matches_isotree_table() {
        // isotree exp_depth_table[n - 1] = 2 (H_n - 1)
        for (size, expected) in [
            (1, 0.0),
            (2, 1.0),
            (3, 1.666_666_666_666_666_5),
            (10, 3.857_936_507_936_507_5),
            (20, 5.195_479_314_287_363),
        ] {
            assert!(
                (expected_average_depth(size) - expected).abs() < 1e-12,
                "n = {size}"
            );
        }
    }

    #[test]
    fn outlier_scores_match_isotree_within_monte_carlo_noise() {
        // 1000-tree isotree scores vary by ~0.014 between independent seeds
        let scores = score_outliers(&rows(&ISOTREE_POINTS), DEFAULT_RANDOM_SEED);
        for (actual, expected) in scores.iter().zip(ISOTREE_SCORES) {
            assert!((actual - expected).abs() < 0.03, "{actual} vs {expected}");
            assert_eq!(
                *actual >= DEFAULT_OUTLIER_THRESHOLD,
                expected >= DEFAULT_OUTLIER_THRESHOLD
            );
        }
        assert_eq!(
            scores,
            score_outliers(&rows(&ISOTREE_POINTS), DEFAULT_RANDOM_SEED)
        );
    }

    #[test]
    fn subsample_does_not_depend_on_path() {
        let matrix = (0..5_000)
            .map(|index| vec![f64::from(index)])
            .collect::<Vec<_>>();
        let file = |path: &str| LoadedFile {
            path: PathBuf::from(path),
            filename: "a.fcs".to_owned(),
            channel_names: vec!["X".to_owned()],
            matrix: matrix.clone(),
            warnings: Vec::new(),
            preprocessing: Vec::new(),
        };
        let relative = sampled_matrix(&file("data/a.fcs"), DEFAULT_EVENTS_PER_FILE, 42);
        let absolute = sampled_matrix(&file("/tmp/x/data/a.fcs"), DEFAULT_EVENTS_PER_FILE, 42);
        assert_eq!(relative.len(), DEFAULT_EVENTS_PER_FILE);
        assert_eq!(relative, absolute);
        assert!(relative.windows(2).all(|pair| pair[0][0] < pair[1][0]));
    }

    #[test]
    fn seed_changes_the_per_file_subsample_and_the_trees() {
        let file = LoadedFile {
            path: PathBuf::from("a.fcs"),
            filename: "a.fcs".to_owned(),
            channel_names: vec!["X".to_owned()],
            matrix: (0..5_000).map(|index| vec![f64::from(index)]).collect(),
            warnings: Vec::new(),
            preprocessing: Vec::new(),
        };
        let default = sampled_matrix(&file, DEFAULT_EVENTS_PER_FILE, DEFAULT_RANDOM_SEED);
        assert_ne!(sampled_matrix(&file, DEFAULT_EVENTS_PER_FILE, 7), default);
        assert_eq!(
            sampled_matrix(&file, DEFAULT_EVENTS_PER_FILE, DEFAULT_RANDOM_SEED),
            default
        );
        let points = rows(&ISOTREE_POINTS);
        assert_ne!(
            score_outliers(&points, 7),
            score_outliers(&points, DEFAULT_RANDOM_SEED)
        );
        // Tree seeds wrap instead of overflowing.
        assert_eq!(score_outliers(&points, u64::MAX).len(), points.len());
    }

    #[test]
    fn aggregate_takes_ceiling_share_from_every_file_without_truncation() {
        let file = |events: usize| LoadedFile {
            path: PathBuf::from("f.fcs"),
            filename: "f.fcs".to_owned(),
            channel_names: vec!["X".to_owned()],
            matrix: (0..events).map(|index| vec![index as f64]).collect(),
            warnings: Vec::new(),
            preprocessing: Vec::new(),
        };
        // ceiling(10000 / 3) = 3334 per file -> 10002 events, as CytoScan
        let files = vec![file(5_000), file(5_000), file(5_000)];
        assert_eq!(
            aggregate_matrix(&files, DEFAULT_RANDOM_SEED)
                .expect("aggregate")
                .len(),
            10_002
        );
        assert_eq!(aggregate_event_count(&files), 10_002);
    }
}
