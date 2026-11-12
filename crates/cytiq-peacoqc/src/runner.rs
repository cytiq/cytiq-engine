#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use cytiq_fcs::{
    ColumnData, FcsDataset, FcsReader, FcsWriteOptions, FcsWriteParameter, MARGINS_KEYWORD,
    MarginBounds, Parameter, SignalState,
};
use log::{debug, info, warn};
use rayon::prelude::*;
use serde::Serialize;

use crate::error::PeacoQcError;
use crate::export::{stage_debug_artifacts, stage_masks_parquet};
use cytiq_density::{density, smooth_spline_fitted};
use cytiq_publish::{Existing, Publication};

pub const DEFAULT_MAD_THRESHOLD: f64 = 6.0;
/// `PeacoQC::MADOutliers` smooths each peak series with `smooth.spline(spar = 0.5)`.
const MAD_SMOOTHING_SPAR: f64 = 0.5;
pub const DEFAULT_IT_LIMIT: f64 = 0.6;
pub const DEFAULT_CONSECUTIVE_BINS: usize = 5;
pub const DEFAULT_MIN_CELLS: usize = 150;
pub const DEFAULT_MAX_BINS: usize = 500;
pub const DEFAULT_STEP: usize = 500;
pub const DEFAULT_FORCE_IT_BINS: usize = 150;
pub const DEFAULT_PEAK_REMOVAL: f64 = 1.0 / 3.0;
pub const DEFAULT_MIN_PEAK_BIN_PERCENT: f64 = 10.0;
pub const DEFAULT_TIME_CHANNEL: &str = "Time";

/// The fixed algorithm constants above, for provenance records.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PeacoQcConstants {
    pub mad: f64,
    pub mad_smoothing_spar: f64,
    pub it_limit: f64,
    pub consecutive_bins: usize,
    pub min_cells: usize,
    pub max_bins: usize,
    pub step: usize,
    pub force_it: usize,
    pub peak_removal: f64,
    pub min_peak_bin_percent: f64,
    pub time_channel: &'static str,
}

impl Default for PeacoQcConstants {
    fn default() -> Self {
        Self {
            mad: DEFAULT_MAD_THRESHOLD,
            mad_smoothing_spar: MAD_SMOOTHING_SPAR,
            it_limit: DEFAULT_IT_LIMIT,
            consecutive_bins: DEFAULT_CONSECUTIVE_BINS,
            min_cells: DEFAULT_MIN_CELLS,
            max_bins: DEFAULT_MAX_BINS,
            step: DEFAULT_STEP,
            force_it: DEFAULT_FORCE_IT_BINS,
            peak_removal: DEFAULT_PEAK_REMOVAL,
            min_peak_bin_percent: DEFAULT_MIN_PEAK_BIN_PERCENT,
            time_channel: DEFAULT_TIME_CHANNEL,
        }
    }
}

const MEDIAN_ABSOLUTE_DEVIATION_SCALE: f64 = 1.4826;
const EQUALITY_EPSILON: f64 = 1.0e-12;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeacoQcMethod {
    #[default]
    All,
    Mad,
    It,
    PeaksOnly,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PeacoQcConfig {
    pub channels: Vec<String>,
    pub method: PeacoQcMethod,
    pub remove_margins: bool,
    pub remove_zeros: bool,
    pub debug_dir: Option<PathBuf>,
    /// Replace an existing output, mask, summary or debug artifact;
    /// otherwise the output, mask and summary are refused before the input is
    /// read, and debug artifacts when they are staged.
    pub overwrite: bool,
}

impl PeacoQcConfig {
    #[must_use]
    pub fn new(channels: Vec<String>) -> Self {
        Self {
            channels,
            method: PeacoQcMethod::All,
            remove_margins: false,
            remove_zeros: false,
            debug_dir: None,
            overwrite: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PeacoQcResult {
    pub summary: PeacoQcSummary,
    pub mask: Vec<EventMaskRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PeacoQcSummary {
    pub tool: String,
    pub version: String,
    pub algorithm: String,
    pub input: String,
    pub output: String,
    pub channels: Vec<String>,
    pub parameters: PeacoQcParameters,
    pub metrics: PeacoQcMetrics,
    pub warnings: Vec<String>,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PeacoQcParameters {
    pub method: PeacoQcMethod,
    pub mad: f64,
    pub it_limit: f64,
    pub consecutive_bins: usize,
    pub min_cells: usize,
    pub max_bins: usize,
    pub step: usize,
    pub force_it: usize,
    pub peak_removal: f64,
    pub min_peak_bin_percent: f64,
    pub remove_zeros: bool,
    pub remove_margins: bool,
    pub time_channel: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PeacoQcMetrics {
    pub events_before: usize,
    pub events_after: usize,
    pub removed_total: usize,
    pub removed_total_fraction: f64,
    pub margin_removed_count: usize,
    pub margin_removed_fraction: f64,
    pub per_channel_margin_counts: Vec<ChannelMarginCounts>,
    pub mad_outlier_bins: Vec<usize>,
    pub mad_removed_event_count: usize,
    pub mad_removed_fraction: f64,
    pub mad_channel_contributions: Vec<MadContribution>,
    pub it_outlier_bins: Vec<usize>,
    pub it_removed_event_count: usize,
    pub it_removed_fraction: f64,
    pub consecutive_removed_bins: Vec<usize>,
    pub consecutive_removed_event_count: usize,
    pub consecutive_removed_fraction: f64,
    pub n_bins: usize,
    pub events_per_bin: usize,
    pub peak_feature_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelMarginCounts {
    pub channel: String,
    pub min_threshold: f64,
    pub max_threshold: f64,
    pub min_count: usize,
    pub max_count: usize,
    pub removed_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MadContribution {
    pub feature: String,
    pub removed_event_count: usize,
    pub removed_fraction: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct EventMaskRecord {
    pub event_id: usize,
    pub original_event_id: usize,
    pub bin_id: Option<usize>,
    pub is_good: bool,
    pub removed_by_margin: bool,
    pub removed_by_lower_margin: bool,
    pub removed_by_upper_margin: bool,
    pub removed_by_mad: bool,
    pub removed_by_it: bool,
    pub removed_by_consecutive_bins: bool,
}

#[derive(Clone, Debug)]
struct ResolvedChannel {
    display_name: String,
    parameter: Parameter,
    values: Vec<f64>,
}

/// The part of a channel the algorithm reads: its name for messages and
/// feature labels, and its values in event order.
#[derive(Clone, Copy, Debug)]
struct ChannelData<'a> {
    name: &'a str,
    values: &'a [f64],
}

impl ResolvedChannel {
    fn data(&self) -> ChannelData<'_> {
        ChannelData {
            name: &self.display_name,
            values: &self.values,
        }
    }
}

/// Bin-level result shared by [`PeacoQcRunner::run`] and [`peacoqc_mask`].
#[derive(Clone, Debug)]
struct MaskComputation {
    working: WorkingData,
    bins: Vec<Vec<usize>>,
    events_per_bin: usize,
    peak_matrix: PeakMatrix,
    mad_bad_bins: Vec<bool>,
    it_bad_bins: Vec<bool>,
    consecutive_bad_bins: Vec<bool>,
    mad_contributions: Vec<MadContribution>,
    mask: Vec<EventMaskRecord>,
}

#[derive(Clone, Debug)]
struct WorkingData {
    original_indices: Vec<usize>,
    margin_removed: Vec<bool>,
    lower_margin_removed: Vec<bool>,
    upper_margin_removed: Vec<bool>,
    margin_counts: Vec<ChannelMarginCounts>,
}

#[derive(Clone, Debug)]
struct PeakMatrix {
    values: Vec<Vec<f64>>,
    feature_names: Vec<String>,
}

#[derive(Clone, Debug)]
struct MadResult {
    bad_bins: Vec<bool>,
    contributions: Vec<MadContribution>,
}

#[derive(Clone, Debug)]
struct ItNode {
    selection: Vec<bool>,
    depth: usize,
    to_split: bool,
    path_length: Option<f64>,
}

pub struct PeacoQcRunner;

impl PeacoQcRunner {
    /// Run the PeacoQC-compatible pipeline and write the cleaned FCS output.
    ///
    /// # Errors
    ///
    /// Returns an error if the input cannot be read, channels cannot be
    /// resolved, the algorithm cannot produce peak features, or outputs cannot
    /// be written.
    pub fn run(
        input: impl AsRef<Path>,
        output: impl AsRef<Path>,
        config: &PeacoQcConfig,
    ) -> Result<PeacoQcResult, PeacoQcError> {
        let input = input.as_ref();
        let output = output.as_ref();
        Self::run_inner(input, output, config)
    }

    fn run_inner(
        input: &Path,
        output: &Path,
        config: &PeacoQcConfig,
    ) -> Result<PeacoQcResult, PeacoQcError> {
        validate_output_path(output)?;
        // The cleaned FCS, its mask and summary appear together or not at all.
        let mut publication = Publication::new(Existing::overwrite(config.overwrite));
        for target in [output, &masks_path(output), &summary_path(output)] {
            publication.check(target)?;
        }
        let dataset = load_dataset(input)?;
        let events_before = dataset.event_count();
        let mut channels = resolve_channels(input, &dataset, &config.channels)?;
        let mut warnings = input_warnings(input, &channels, &dataset)?;
        for warning in &warnings {
            warn!("peacoqc: {warning}");
        }

        info!(
            "peacoqc pipeline: loaded {} events and {} selected channels from {}",
            events_before,
            channels.len(),
            input.display()
        );
        let margin_bounds = if config.remove_margins {
            let warning = "legacy PeacoQC margin filtering is enabled: lower bounds (including flowCore's -111 floor) and observed extrema can remove negative fluorescence and rare populations".to_owned();
            warn!("peacoqc: {warning}");
            warnings.push(warning);
            Some(selected_margin_bounds(input, &dataset, &channels)?)
        } else {
            None
        };
        let channel_data = channels
            .iter()
            .map(ResolvedChannel::data)
            .collect::<Vec<_>>();
        let MaskComputation {
            working,
            bins,
            events_per_bin,
            peak_matrix,
            mad_bad_bins,
            it_bad_bins,
            consecutive_bad_bins,
            mad_contributions,
            mask,
        } = compute_mask(
            input,
            &channel_data,
            events_before,
            margin_bounds.as_deref(),
            config,
            &mut warnings,
        )?;
        // Past the mask only channel names are read: release the f64 copies
        // before the cleaned FCS is built.
        drop(channel_data);
        for channel in &mut channels {
            channel.values = Vec::new();
        }
        let read_error = |source| PeacoQcError::FcsRead {
            path: input.to_path_buf(),
            source,
        };
        let source_event_ids = cytiq_fcs::source_event_ids(&dataset).map_err(read_error)?;
        let acquisition = cytiq_fcs::acquisition_id(input, &dataset.text).map_err(read_error)?;
        publication.stage_path(output, |path| {
            write_cleaned_fcs(input, output, path, &dataset, &mask, &acquisition)
        })?;
        stage_masks_parquet(
            &mut publication,
            &masks_path(output),
            &acquisition,
            &mask,
            source_event_ids.as_deref(),
            config.remove_margins,
        )?;
        let summary = build_summary(
            input,
            output,
            config,
            &channels,
            &mask,
            &working,
            &bins,
            events_per_bin,
            &peak_matrix,
            &mad_bad_bins,
            &it_bad_bins,
            &consecutive_bad_bins,
            mad_contributions,
            warnings,
        );
        let summary_path = summary_path(output);
        publication.stage_file(&summary_path, |writer| {
            serde_json::to_writer_pretty(writer, &summary).map_err(|source| {
                PeacoQcError::JsonSerialize {
                    path: summary_path.clone(),
                    source,
                }
            })
        })?;
        // Debug artifacts share the publication: a failed debug write
        // publishes nothing, and they follow the same `overwrite` rule.
        if let Some(debug_dir) = config.debug_dir.as_deref() {
            stage_debug_artifacts(&mut publication, &summary, &mask, debug_dir)?;
        }
        publication.commit()?;
        info!(
            "peacoqc pipeline: retained {} of {} events",
            summary.metrics.events_after, summary.metrics.events_before
        );

        Ok(PeacoQcResult { summary, mask })
    }
}

/// `PeacoQC`'s good-event mask of already compensated and transformed
/// channels held in memory, as [`PeacoQcRunner::run`] computes it from a file.
/// `channels` pairs a name with its values in event order; all have the same
/// length. `source` only names the data in errors. Margins are the caller's
/// business: `config.remove_margins` and `config.debug_dir` are ignored.
///
/// # Errors
///
/// Returns an error when `channels` is empty, a channel is not finite or has
/// a different length, no peak features can be built, or the thread pool
/// cannot be created.
pub fn peacoqc_mask(
    source: &Path,
    channels: &[(&str, &[f64])],
    config: &PeacoQcConfig,
) -> Result<Vec<bool>, PeacoQcError> {
    let Some((_, first)) = channels.first() else {
        return Err(PeacoQcError::EmptyChannels);
    };
    let events = first.len();
    for (name, values) in channels {
        if values.len() != events || values.iter().any(|value| !value.is_finite()) {
            return Err(PeacoQcError::NonFiniteChannel {
                path: source.to_path_buf(),
                channel: (*name).to_owned(),
            });
        }
    }
    if events == 0 {
        return Err(PeacoQcError::EmptyInput {
            path: source.to_path_buf(),
        });
    }
    let data = channels
        .iter()
        .map(|(name, values)| ChannelData { name, values })
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    let computation = compute_mask(source, &data, events, None, config, &mut warnings)?;
    for warning in &warnings {
        warn!("peacoqc: {warning}");
    }
    Ok(computation
        .mask
        .iter()
        .map(|record| record.is_good)
        .collect())
}

fn compute_mask(
    input: &Path,
    channels: &[ChannelData<'_>],
    events_before: usize,
    margin_bounds: Option<&[MarginBounds]>,
    config: &PeacoQcConfig,
    warnings: &mut Vec<String>,
) -> Result<MaskComputation, PeacoQcError> {
    let working = prepare_working_data(channels, events_before, margin_bounds);
    if working.original_indices.is_empty() {
        return Err(PeacoQcError::AllEventsRemoved {
            path: input.to_path_buf(),
        });
    }
    let (events_per_bin, bins, peak_matrix) = {
        // The events margins kept, in order: the channels themselves when
        // nothing was removed (the default), a copy otherwise. Only binning
        // and the peak features read them, so they are released here.
        let values = channels
            .iter()
            .map(|channel| {
                if working.original_indices.len() == channel.values.len() {
                    Cow::Borrowed(channel.values)
                } else {
                    Cow::Owned(
                        working
                            .original_indices
                            .iter()
                            .map(|&index| channel.values[index])
                            .collect(),
                    )
                }
            })
            .collect::<Vec<_>>();
        // Upstream divides the event count by the fewest non-zero events of
        // any channel; with none the bin size is infinite (R yields `Inf`).
        if config.remove_zeros
            && let Some(index) = values
                .iter()
                .position(|values| values.iter().all(|value| *value == 0.0))
        {
            return Err(PeacoQcError::AllZeroChannel {
                path: input.to_path_buf(),
                channel: channels[index].name.to_owned(),
            });
        }
        let events_per_bin =
            find_events_per_bin(config.remove_zeros, &values, working.original_indices.len());
        let bins = make_bins(events_per_bin, working.original_indices.len());
        let peak_matrix = build_peak_matrix(input, channels, &values, &bins, config.remove_zeros)?;
        (events_per_bin, bins, peak_matrix)
    };
    info!(
        "peacoqc pipeline: built {} bins and {} peak features",
        bins.len(),
        peak_matrix.feature_names.len()
    );

    let mut good_bins = vec![true; bins.len()];
    let mut it_bad_bins = vec![false; bins.len()];
    if matches!(config.method, PeacoQcMethod::All | PeacoQcMethod::It) {
        if bins.len() >= DEFAULT_FORCE_IT_BINS {
            let it_good_bins = isolation_tree_good_bins(&peak_matrix.values, DEFAULT_IT_LIMIT);
            for (index, good) in it_good_bins.iter().copied().enumerate() {
                if !good {
                    it_bad_bins[index] = true;
                }
                good_bins[index] = good;
            }
        } else {
            warnings.push(format!(
                "Isolation Tree skipped: {} bins is below the default force_IT threshold of {}",
                bins.len(),
                DEFAULT_FORCE_IT_BINS
            ));
        }
    }

    let mut mad_bad_bins = vec![false; bins.len()];
    let mut mad_contributions = Vec::new();
    if matches!(config.method, PeacoQcMethod::All | PeacoQcMethod::Mad) {
        let mad = mad_bad_bins_for_features(
            &peak_matrix,
            &good_bins,
            &bins,
            working.original_indices.len(),
            events_before,
        );
        for (index, bad) in mad.bad_bins.iter().copied().enumerate() {
            if bad {
                mad_bad_bins[index] = true;
                good_bins[index] = false;
            }
        }
        mad_contributions = mad.contributions;
    }

    let pre_consecutive_good_bins = good_bins.clone();
    let mut consecutive_bad_bins = vec![false; bins.len()];
    if !matches!(config.method, PeacoQcMethod::PeaksOnly) {
        good_bins = remove_short_good_regions(&good_bins, DEFAULT_CONSECUTIVE_BINS);
        for (index, (before, after)) in pre_consecutive_good_bins
            .iter()
            .copied()
            .zip(good_bins.iter().copied())
            .enumerate()
        {
            consecutive_bad_bins[index] = before && !after;
        }
    }

    let removed_by_it_working =
        working_event_mask_from_bins(&it_bad_bins, &bins, working.original_indices.len());
    let removed_by_mad_working =
        working_event_mask_from_bins(&mad_bad_bins, &bins, working.original_indices.len());
    let removed_by_consecutive_working =
        working_event_mask_from_bins(&consecutive_bad_bins, &bins, working.original_indices.len());

    let mask = build_event_mask(
        events_before,
        &working,
        &bins,
        &removed_by_mad_working,
        &removed_by_it_working,
        &removed_by_consecutive_working,
    );
    Ok(MaskComputation {
        working,
        bins,
        events_per_bin,
        peak_matrix,
        mad_bad_bins,
        it_bad_bins,
        consecutive_bad_bins,
        mad_contributions,
        mask,
    })
}

fn validate_output_path(output: &Path) -> Result<(), PeacoQcError> {
    let is_fcs = output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fcs"));
    if is_fcs {
        Ok(())
    } else {
        Err(PeacoQcError::UnsupportedOutputFormat {
            path: output.to_path_buf(),
        })
    }
}

fn load_dataset(path: &Path) -> Result<FcsDataset, PeacoQcError> {
    if !path.is_file() {
        return Err(PeacoQcError::MissingInputFile {
            path: path.to_path_buf(),
        });
    }
    debug!("loading FCS file {}", path.display());
    let dataset = FcsReader::open(path)
        .and_then(FcsReader::read)
        .and_then(|dataset| cytiq_fcs::ensure_linear_scale(&dataset).map(|()| dataset))
        .map_err(|source| PeacoQcError::FcsRead {
            path: path.to_path_buf(),
            source,
        })?;
    if dataset.event_count() == 0 {
        return Err(PeacoQcError::EmptyInput {
            path: path.to_path_buf(),
        });
    }

    Ok(dataset)
}

fn resolve_channels(
    path: &Path,
    dataset: &FcsDataset,
    requested_channels: &[String],
) -> Result<Vec<ResolvedChannel>, PeacoQcError> {
    if requested_channels.is_empty() {
        return Err(PeacoQcError::EmptyChannels);
    }

    let mut seen = HashSet::new();
    let mut channels = Vec::with_capacity(requested_channels.len());
    for channel in requested_channels {
        let Some(parameter_index) = cytiq_fcs::find_parameter_index(&dataset.parameters, channel)
        else {
            return Err(PeacoQcError::MissingChannel {
                path: path.to_path_buf(),
                channel: channel.clone(),
                available_channels: dataset
                    .parameters
                    .iter()
                    .map(display_parameter_identifier)
                    .collect(),
            });
        };
        let parameter = dataset.parameters[parameter_index].clone();
        if !seen.insert(parameter.name.clone()) {
            return Err(PeacoQcError::DuplicateChannel {
                channel: parameter.name,
            });
        }
        let values = dataset
            .data
            .column(parameter_index)
            .expect("parameter index should have a data column")
            .to_f64_vec();
        if values.iter().any(|value| !value.is_finite()) {
            return Err(PeacoQcError::NonFiniteChannel {
                path: path.to_path_buf(),
                channel: channel.clone(),
            });
        }
        channels.push(ResolvedChannel {
            display_name: parameter.name.clone(),
            parameter,
            values,
        });
    }

    Ok(channels)
}

/// `PeacoQC` expects compensated, transformed signal. Other input still runs,
/// so it is reported, not refused.
fn input_warnings(
    path: &Path,
    channels: &[ResolvedChannel],
    dataset: &FcsDataset,
) -> Result<Vec<String>, PeacoQcError> {
    let state = SignalState::of(&dataset.text).map_err(|source| PeacoQcError::FcsRead {
        path: path.to_path_buf(),
        source,
    })?;
    let mut warnings = Vec::new();
    let pending_spillover = dataset
        .text
        .iter()
        .any(|(key, _)| cytiq_fcs::is_spillover_keyword(key));
    if pending_spillover {
        warnings.push(
            "the file's spillover matrix was not applied; PeacoQC expects compensated input (run `cytiq compensate` first)".to_owned(),
        );
    }
    let untransformed = channels
        .iter()
        .filter(|channel| !state.transformed(&channel.parameter.name))
        .map(|channel| channel.display_name.as_str())
        .collect::<Vec<_>>();
    if !untransformed.is_empty() {
        warnings.push(format!(
            "channels {} were not transformed by `cytiq transform`; PeacoQC expects transformed input",
            untransformed.join(", ")
        ));
    }
    Ok(warnings)
}

fn known_margin_bounds(
    path: &Path,
    dataset: &FcsDataset,
) -> Result<BTreeMap<String, MarginBounds>, PeacoQcError> {
    let read_error = |source| PeacoQcError::FcsRead {
        path: path.to_path_buf(),
        source,
    };
    let mut known =
        cytiq_fcs::dataset_margin_bounds(&dataset.text, &dataset.parameters).map_err(read_error)?;
    let recorded = cytiq_fcs::recorded_margin_bounds(&dataset.text)
        .map_err(read_error)?
        .unwrap_or_default();
    for parameter in &dataset.parameters {
        if known.contains_key(&parameter.name) && !recorded.contains_key(&parameter.name) {
            let bounds = MarginBounds::from_flowcore_parameter(&dataset.text, parameter);
            bounds.validate().map_err(read_error)?;
            known.insert(parameter.name.clone(), bounds);
        }
    }
    Ok(known)
}

fn selected_margin_bounds(
    path: &Path,
    dataset: &FcsDataset,
    channels: &[ResolvedChannel],
) -> Result<Vec<MarginBounds>, PeacoQcError> {
    let known = known_margin_bounds(path, dataset)?;
    channels
        .iter()
        .map(|channel| {
            known.get(&channel.parameter.name).copied().ok_or_else(|| {
                PeacoQcError::UnknownMargins {
                    path: path.to_path_buf(),
                    channel: channel.display_name.clone(),
                }
            })
        })
        .collect()
}

fn prepare_working_data(
    channels: &[ChannelData<'_>],
    event_count: usize,
    margin_bounds: Option<&[MarginBounds]>,
) -> WorkingData {
    let mut margin_removed = vec![false; event_count];
    let mut lower_margin_removed = vec![false; event_count];
    let mut upper_margin_removed = vec![false; event_count];
    let mut margin_counts = Vec::new();

    if let Some(margin_bounds) = margin_bounds {
        for (channel, bounds) in channels.iter().zip(margin_bounds) {
            let (lower, upper, counts) = margin_events_for_channel(channel, bounds);
            for (index, (low, high)) in lower.into_iter().zip(upper).enumerate() {
                lower_margin_removed[index] |= low;
                upper_margin_removed[index] |= high;
                margin_removed[index] |= low || high;
            }
            margin_counts.push(counts);
        }
    }

    let original_indices = margin_removed
        .iter()
        .enumerate()
        .filter_map(|(index, removed)| (!removed).then_some(index))
        .collect::<Vec<_>>();

    WorkingData {
        original_indices,
        margin_removed,
        lower_margin_removed,
        upper_margin_removed,
        margin_counts,
    }
}

fn margin_events_for_channel(
    channel: &ChannelData<'_>,
    bounds: &MarginBounds,
) -> (Vec<bool>, Vec<bool>, ChannelMarginCounts) {
    let minimum = channel.values.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = channel
        .values
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    // PeacoQC::RemoveMargins keeps e > max(min(minRange, 0), min(e)) and
    // e < min(maxRange, max(e)); see `MarginBounds::from_raw_parameter`.
    let (min_threshold, max_threshold) = bounds.thresholds(minimum, maximum);

    let min_flags = channel
        .values
        .iter()
        .map(|value| *value <= min_threshold)
        .collect::<Vec<_>>();
    let max_flags = channel
        .values
        .iter()
        .map(|value| *value >= max_threshold)
        .collect::<Vec<_>>();
    let removed = min_flags
        .iter()
        .zip(&max_flags)
        .map(|(min_removed, max_removed)| *min_removed || *max_removed)
        .filter(|removed| *removed)
        .count();
    let counts = ChannelMarginCounts {
        channel: channel.name.to_owned(),
        min_threshold,
        max_threshold,
        min_count: min_flags.iter().filter(|removed| **removed).count(),
        max_count: max_flags.iter().filter(|removed| **removed).count(),
        removed_count: removed,
    };

    (min_flags, max_flags, counts)
}

fn find_events_per_bin(
    remove_zeros: bool,
    channel_values: &[Cow<'_, [f64]>],
    event_count: usize,
) -> usize {
    let mut max_bins = DEFAULT_MAX_BINS.max(1) as f64;
    if remove_zeros {
        let min_non_zero = channel_values
            .iter()
            .map(|values| values.iter().filter(|value| **value != 0.0).count())
            .min()
            .unwrap_or(event_count);
        max_bins = max_bins.min(min_non_zero as f64 / DEFAULT_MIN_CELLS as f64);
    }
    debug_assert!(
        max_bins > 0.0 && max_bins.is_finite(),
        "callers reject channels without non-zero events"
    );

    let raw_max_cells = ((event_count as f64 / max_bins) * 2.0).ceil() as usize;
    let rounded_max_cells = ((raw_max_cells / DEFAULT_STEP) * DEFAULT_STEP) + DEFAULT_STEP;
    DEFAULT_MIN_CELLS.max(rounded_max_cells)
}

fn make_bins(events_per_bin: usize, event_count: usize) -> Vec<Vec<usize>> {
    if event_count == 0 {
        return Vec::new();
    }
    let overlap = events_per_bin.div_ceil(2);
    let step = events_per_bin.saturating_sub(overlap).max(1);
    let mut bins = Vec::new();
    let mut start = 0;
    while start < event_count {
        let end = start.saturating_add(events_per_bin).min(event_count);
        bins.push((start..end).collect());
        start = start.saturating_add(step);
    }

    bins
}

fn build_peak_matrix(
    input: &Path,
    channels: &[ChannelData<'_>],
    values: &[Cow<'_, [f64]>],
    bins: &[Vec<usize>],
    remove_zeros: bool,
) -> Result<PeakMatrix, PeacoQcError> {
    let channel_features = channels
        .par_iter()
        .enumerate()
        .filter_map(|(index, channel)| {
            build_channel_peak_features(channel.name, &values[index], bins, remove_zeros)
        })
        .collect::<Vec<_>>();

    let feature_count = channel_features
        .iter()
        .map(|features| features.names.len())
        .sum::<usize>();
    if feature_count == 0 {
        return Err(PeacoQcError::NoPeakFeatures {
            path: input.to_path_buf(),
        });
    }

    let mut values = vec![Vec::with_capacity(feature_count); bins.len()];
    let mut feature_names = Vec::with_capacity(feature_count);
    for features in channel_features {
        feature_names.extend(features.names);
        for (row, row_values) in values.iter_mut().zip(features.values) {
            row.extend(row_values);
        }
    }

    Ok(PeakMatrix {
        values,
        feature_names,
    })
}

#[derive(Clone, Debug)]
struct ChannelPeakFeatures {
    names: Vec<String>,
    values: Vec<Vec<f64>>,
}

fn build_channel_peak_features(
    channel_name: &str,
    values: &[f64],
    bins: &[Vec<usize>],
    remove_zeros: bool,
) -> Option<ChannelPeakFeatures> {
    let full_peaks = find_peaks(values, remove_zeros, DEFAULT_PEAK_REMOVAL);
    if full_peaks.is_empty() {
        return None;
    }

    let peaks_by_bin = bins
        .iter()
        .map(|bin| {
            let bin_values = bin.iter().map(|&index| values[index]).collect::<Vec<_>>();
            find_peaks(&bin_values, remove_zeros, DEFAULT_PEAK_REMOVAL)
        })
        .collect::<Vec<_>>();
    let non_empty = peaks_by_bin
        .iter()
        .filter(|peaks| !peaks.is_empty())
        .collect::<Vec<_>>();
    if non_empty.is_empty() {
        return None;
    }

    let mut count_frequencies = HashMap::<usize, usize>::new();
    for peaks in &non_empty {
        *count_frequencies.entry(peaks.len()).or_default() += 1;
    }
    let min_bins = (DEFAULT_MIN_PEAK_BIN_PERCENT / 100.0) * (non_empty.len() as f64);
    let target_peak_count = count_frequencies
        .into_iter()
        .filter(|(_, frequency)| (*frequency as f64) > min_bins)
        .map(|(count, _)| count)
        .max()
        .unwrap_or(1);
    if target_peak_count == 0 {
        return None;
    }

    let mut centers = Vec::with_capacity(target_peak_count);
    if target_peak_count == 1 {
        // PeacoQC::DetermineAllPeaks uses the median of all detected peaks.
        centers.push(median(
            non_empty
                .iter()
                .flat_map(|peaks| peaks.iter().copied())
                .collect(),
        ));
    } else {
        for peak_index in 0..target_peak_count {
            let values_for_peak = non_empty
                .iter()
                .filter(|peaks| peaks.len() == target_peak_count)
                .map(|peaks| peaks[peak_index])
                .collect::<Vec<_>>();
            centers.push(median(values_for_peak));
        }
    }

    let assigned = assign_peak_clusters(&peaks_by_bin, &centers);
    // TooSmallClusters compares against the largest bin id that has peaks.
    let peak_bin_count = peaks_by_bin
        .iter()
        .rposition(|peaks| !peaks.is_empty())
        .map_or(0, |index| index + 1);
    let kept_clusters = kept_peak_clusters(&assigned, peak_bin_count, centers.len());
    if kept_clusters.is_empty() {
        return None;
    }

    let mut names = Vec::with_capacity(kept_clusters.len());
    let mut columns = Vec::with_capacity(kept_clusters.len());
    for cluster in kept_clusters {
        let observed = assigned
            .iter()
            .filter_map(|bin| {
                bin.iter()
                    .find_map(|(candidate, value)| (*candidate == cluster).then_some(*value))
            })
            .collect::<Vec<_>>();
        let fallback = median(observed);
        let column = assigned
            .iter()
            .map(|bin| {
                bin.iter()
                    .find_map(|(candidate, value)| (*candidate == cluster).then_some(*value))
                    .unwrap_or(fallback)
            })
            .collect::<Vec<_>>();
        names.push(format!("{}:peak{}", channel_name, cluster + 1));
        columns.push(column);
    }

    let mut rows = vec![Vec::with_capacity(columns.len()); bins.len()];
    for column in columns {
        for (row, value) in rows.iter_mut().zip(column) {
            row.push(value);
        }
    }

    Some(ChannelPeakFeatures {
        names,
        values: rows,
    })
}

/// `PeacoQC::FindThemPeaks`: local maxima of `stats::density(x)` above
/// `peak_removal * max(density)`, falling back to the global maximum.
fn find_peaks(values: &[f64], remove_zeros: bool, peak_removal: f64) -> Vec<f64> {
    let data = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .filter(|value| !remove_zeros || *value != 0.0)
        .collect::<Vec<_>>();
    if data.len() < 3 {
        return Vec::new();
    }
    let estimate = density(&data);
    let max_height = estimate.y.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    let mut peaks = Vec::new();
    for index in 1..(estimate.y.len() - 1) {
        if estimate.y[index] > estimate.y[index - 1]
            && estimate.y[index] > estimate.y[index + 1]
            && estimate.y[index] > peak_removal * max_height
        {
            peaks.push(estimate.x[index]);
        }
    }
    if peaks.is_empty() {
        let max_index = estimate
            .y
            .iter()
            .enumerate()
            .fold((0, f64::NEG_INFINITY), |best, (index, value)| {
                if *value > best.1 {
                    (index, *value)
                } else {
                    best
                }
            })
            .0;
        peaks.push(estimate.x[max_index]);
    }

    peaks
}

fn assign_peak_clusters(peaks_by_bin: &[Vec<f64>], centers: &[f64]) -> Vec<Vec<(usize, f64)>> {
    peaks_by_bin
        .iter()
        .map(|peaks| {
            let mut by_cluster = HashMap::<usize, f64>::new();
            for peak in peaks {
                let cluster = centers
                    .iter()
                    .enumerate()
                    .min_by(|(_, left), (_, right)| {
                        ((*peak - **left).abs()).total_cmp(&(*peak - **right).abs())
                    })
                    .map_or(0, |(index, _)| index);
                by_cluster
                    .entry(cluster)
                    .and_modify(|existing| {
                        let current_distance = (*peak - centers[cluster]).abs();
                        let existing_distance = (*existing - centers[cluster]).abs();
                        if current_distance < existing_distance {
                            *existing = *peak;
                        }
                    })
                    .or_insert(*peak);
            }
            let mut assigned = by_cluster.into_iter().collect::<Vec<_>>();
            assigned.sort_by_key(|(cluster, _)| *cluster);
            assigned
        })
        .collect()
}

fn kept_peak_clusters(
    assigned: &[Vec<(usize, f64)>],
    bin_count: usize,
    cluster_count: usize,
) -> Vec<usize> {
    let mut counts = vec![0_usize; cluster_count];
    for bin in assigned {
        for (cluster, _) in bin {
            counts[*cluster] += 1;
        }
    }
    counts
        .into_iter()
        .enumerate()
        .filter_map(|(cluster, count)| (count * 2 >= bin_count).then_some(cluster))
        .collect()
}

fn isolation_tree_good_bins(matrix: &[Vec<f64>], gain_limit: f64) -> Vec<bool> {
    if matrix.is_empty() {
        return Vec::new();
    }
    let max_depth = (matrix.len() as f64).log2().ceil() as usize;
    let mut current_gain_limit = gain_limit;
    let mut nodes = vec![ItNode {
        selection: vec![true; matrix.len()],
        depth: 0,
        to_split: true,
        path_length: None,
    }];

    while let Some(node_index) = nodes.iter().position(|node| node.to_split) {
        let rows = nodes[node_index]
            .selection
            .iter()
            .enumerate()
            .filter_map(|(index, selected)| selected.then_some(index))
            .collect::<Vec<_>>();
        if rows.len() > 3 && nodes[node_index].depth < max_depth {
            if let Some(split) = best_isolation_split(matrix, &rows, current_gain_limit) {
                let Some([left, right]) =
                    isolation_children(matrix, &nodes[node_index].selection, split)
                else {
                    close_it_node(&mut nodes[node_index], rows.len());
                    continue;
                };

                nodes[node_index].to_split = false;
                current_gain_limit = split.gain;
                let child_depth = nodes[node_index].depth + 1;
                nodes.push(ItNode {
                    selection: left,
                    depth: child_depth,
                    to_split: true,
                    path_length: None,
                });
                nodes.push(ItNode {
                    selection: right,
                    depth: child_depth,
                    to_split: true,
                    path_length: None,
                });
            } else {
                close_it_node(&mut nodes[node_index], rows.len());
            }
        } else {
            close_it_node(&mut nodes[node_index], rows.len());
        }
    }

    nodes
        .iter()
        .filter(|node| node.path_length.is_some())
        .min_by_key(|node| {
            std::cmp::Reverse(node.selection.iter().filter(|selected| **selected).count())
        })
        .map_or_else(|| vec![true; matrix.len()], |node| node.selection.clone())
}

#[derive(Clone, Copy, Debug)]
struct IsolationSplit {
    column: usize,
    value: f64,
    gain: f64,
}

fn isolation_children(
    matrix: &[Vec<f64>],
    selection: &[bool],
    split: IsolationSplit,
) -> Option<[Vec<bool>; 2]> {
    let left = selection
        .iter()
        .enumerate()
        .map(|(row, selected)| *selected && matrix[row][split.column] <= split.value)
        .collect::<Vec<_>>();
    let right = selection
        .iter()
        .enumerate()
        .map(|(row, selected)| *selected && matrix[row][split.column] > split.value)
        .collect::<Vec<_>>();
    let left_count = left.iter().filter(|selected| **selected).count();
    let right_count = right.iter().filter(|selected| **selected).count();
    if left_count == 0 || right_count == 0 {
        None
    } else {
        Some([left, right])
    }
}

#[allow(clippy::needless_range_loop)]
fn best_isolation_split(
    matrix: &[Vec<f64>],
    rows: &[usize],
    gain_limit: f64,
) -> Option<IsolationSplit> {
    let column_count = matrix.first().map_or(0, Vec::len);
    let mut best = None;
    let mut gain_max = gain_limit;
    for column in 0..column_count {
        let mut values = rows
            .iter()
            .map(|&row| matrix[row][column])
            .collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        let base_sd = sample_standard_deviation(&values);
        if base_sd <= EQUALITY_EPSILON {
            continue;
        }

        let mut best_col_gain = 0.0;
        let mut best_col_value = None;
        for split_index in 1..values.len() {
            let left_sd = if split_index == 1 {
                0.0
            } else {
                sample_standard_deviation(&values[..split_index])
            };
            let right_sd = if split_index == values.len() - 1 {
                0.0
            } else {
                sample_standard_deviation(&values[split_index..])
            };
            let gain = (base_sd - f64::midpoint(left_sd, right_sd)) / base_sd;
            if gain.is_finite() && gain >= best_col_gain {
                best_col_gain = gain;
                best_col_value = Some(values[split_index - 1]);
            }
        }
        if best_col_gain > gain_max {
            gain_max = best_col_gain;
            best = Some(IsolationSplit {
                column,
                value: best_col_value.unwrap_or(values[0]),
                gain: best_col_gain,
            });
        }
    }

    best
}

fn close_it_node(node: &mut ItNode, selected_count: usize) {
    node.to_split = false;
    node.path_length = Some(avg_path_length(selected_count) + node.depth as f64);
}

fn avg_path_length(n_datapoints: usize) -> f64 {
    if n_datapoints <= 1 {
        0.0
    } else {
        let n = n_datapoints as f64;
        // PeacoQC's avgPL hard-codes Euler's constant to 10 digits
        #[allow(clippy::approx_constant)]
        let euler_gamma = 0.577_215_664_9;
        2.0 * ((n - 1.0).ln() + euler_gamma) - (2.0 * (n - 1.0)) / n
    }
}

fn mad_bad_bins_for_features(
    matrix: &PeakMatrix,
    candidate_good_bins: &[bool],
    bins: &[Vec<usize>],
    working_event_count: usize,
    denominator_event_count: usize,
) -> MadResult {
    let candidate_rows = candidate_good_bins
        .iter()
        .enumerate()
        .filter_map(|(index, good)| good.then_some(index))
        .collect::<Vec<_>>();
    let mut bad_bins = vec![false; matrix.values.len()];
    let mut contributions = Vec::new();
    if candidate_rows.len() < 3 {
        return MadResult {
            bad_bins,
            contributions,
        };
    }

    for feature_index in 0..matrix.feature_names.len() {
        let series = candidate_rows
            .iter()
            .map(|&row| matrix.values[row][feature_index])
            .collect::<Vec<_>>();
        let smoothed = smooth_spline_fitted(&series, MAD_SMOOTHING_SPAR);
        let median_value = median(smoothed.clone());
        let mad_value = median_absolute_deviation(&smoothed);
        let mut feature_bad_bins = vec![false; matrix.values.len()];
        let lower = median_value - (DEFAULT_MAD_THRESHOLD * mad_value);
        let upper = median_value + (DEFAULT_MAD_THRESHOLD * mad_value);
        for (&row, value) in candidate_rows.iter().zip(smoothed) {
            let outlier = if mad_value <= EQUALITY_EPSILON {
                (value - median_value).abs() > EQUALITY_EPSILON
            } else {
                value < lower || value > upper
            };
            if outlier {
                bad_bins[row] = true;
                feature_bad_bins[row] = true;
            }
        }

        let removed_event_count =
            count_working_events_from_bins(&feature_bad_bins, bins, working_event_count);
        if removed_event_count > 0 {
            contributions.push(MadContribution {
                feature: matrix.feature_names[feature_index].clone(),
                removed_event_count,
                removed_fraction: fraction(removed_event_count, denominator_event_count),
            });
        }
    }

    MadResult {
        bad_bins,
        contributions,
    }
}

fn remove_short_good_regions(good_bins: &[bool], consecutive_bins: usize) -> Vec<bool> {
    let mut output = good_bins.to_vec();
    let mut start = 0;
    while start < good_bins.len() {
        let value = good_bins[start];
        let mut end = start + 1;
        while end < good_bins.len() && good_bins[end] == value {
            end += 1;
        }
        if value && end - start < consecutive_bins {
            output[start..end].fill(false);
        }
        start = end;
    }

    output
}

fn working_event_mask_from_bins(
    bad_bins: &[bool],
    bins: &[Vec<usize>],
    working_event_count: usize,
) -> Vec<bool> {
    let mut mask = vec![false; working_event_count];
    for (bad, bin) in bad_bins.iter().zip(bins) {
        if *bad {
            for &event_index in bin {
                mask[event_index] = true;
            }
        }
    }

    mask
}

fn build_event_mask(
    original_event_count: usize,
    working: &WorkingData,
    bins: &[Vec<usize>],
    removed_by_mad_working: &[bool],
    removed_by_it_working: &[bool],
    removed_by_consecutive_working: &[bool],
) -> Vec<EventMaskRecord> {
    let primary_bin_ids = primary_bin_ids(bins, working.original_indices.len());
    let mut records = (0..original_event_count)
        .map(|index| EventMaskRecord {
            event_id: index + 1,
            original_event_id: index + 1,
            bin_id: None,
            is_good: !working.margin_removed[index],
            removed_by_margin: working.margin_removed[index],
            removed_by_lower_margin: working.lower_margin_removed[index],
            removed_by_upper_margin: working.upper_margin_removed[index],
            removed_by_mad: false,
            removed_by_it: false,
            removed_by_consecutive_bins: false,
        })
        .collect::<Vec<_>>();

    for (working_index, &original_index) in working.original_indices.iter().enumerate() {
        let record = &mut records[original_index];
        record.bin_id = primary_bin_ids[working_index];
        record.removed_by_mad = removed_by_mad_working[working_index];
        record.removed_by_it = removed_by_it_working[working_index];
        record.removed_by_consecutive_bins = removed_by_consecutive_working[working_index];
        record.is_good = !(record.removed_by_margin
            || record.removed_by_mad
            || record.removed_by_it
            || record.removed_by_consecutive_bins);
    }

    records
}

fn primary_bin_ids(bins: &[Vec<usize>], event_count: usize) -> Vec<Option<usize>> {
    let mut ids = vec![None; event_count];
    for (bin_index, bin) in bins.iter().enumerate() {
        for &event_index in bin {
            ids[event_index].get_or_insert(bin_index + 1);
        }
    }

    ids
}

/// `sample.peacoqc.fcs` -> `sample.peacoqc.masks.parquet`.
#[must_use]
pub fn masks_path(output: &Path) -> PathBuf {
    output.with_extension("masks.parquet")
}

#[must_use]
pub fn summary_path(output: &Path) -> PathBuf {
    output.with_extension("summary.json")
}

/// Write the retained events to the staged `path` that becomes `output`.
fn write_cleaned_fcs(
    input: &Path,
    output: &Path,
    path: &Path,
    dataset: &FcsDataset,
    mask: &[EventMaskRecord],
    acquisition: &str,
) -> Result<(), PeacoQcError> {
    let retained = mask
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record.is_good.then_some(index))
        .collect::<Vec<_>>();
    if retained.is_empty() {
        return Err(PeacoQcError::AllEventsRemoved {
            path: output.to_path_buf(),
        });
    }

    // Values are written unchanged, so every source keyword, `$PnR` and the
    // spillover matrix stay valid for the cleaned file. The writer declares
    // `$PnE=0,0`, which holds because `load_dataset` rejects log-amplified
    // input.
    let mut parameters = dataset
        .parameters
        .iter()
        .map(FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    let mut columns = dataset
        .data
        .columns
        .iter()
        .map(|column| {
            retained
                .iter()
                .map(|&index| column_value_to_f64(column, index))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    if !dataset
        .parameters
        .iter()
        .any(|parameter| parameter.name == cytiq_fcs::ORIGINAL_ID_PARAMETER)
    {
        parameters.push(FcsWriteParameter::new(cytiq_fcs::ORIGINAL_ID_PARAMETER));
        columns.push(
            retained
                .iter()
                .map(|&index| (index + 1) as f64)
                .collect::<Vec<_>>(),
        );
    }
    if columns
        .iter()
        .flat_map(|column| column.iter())
        .any(|value| !value.is_finite())
    {
        return Err(PeacoQcError::NonFiniteOutputValues {
            path: output.to_path_buf(),
        });
    }

    let mut margin_bounds = known_margin_bounds(input, dataset)?;
    margin_bounds.remove(cytiq_fcs::ORIGINAL_ID_PARAMETER);
    let mut keywords = cytiq_fcs::carried_keywords(&dataset.text);
    cytiq_fcs::set_acquisition(&mut keywords, acquisition);
    let options = FcsWriteOptions::new()
        .with_text_keywords(keywords)
        .with_text_keyword(
            MARGINS_KEYWORD,
            cytiq_fcs::encode_margin_bounds(&margin_bounds),
        );
    // Event identifiers above 2^24, 32-bit integer data and f64 data do not
    // survive f32; the writer uses doubles only when a value would change.
    cytiq_fcs::write_fcs3_1_exact_with_options(path, &parameters, &columns, &options).map_err(
        |source| PeacoQcError::FcsWrite {
            path: output.to_path_buf(),
            source,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn build_summary(
    input: &Path,
    output: &Path,
    config: &PeacoQcConfig,
    channels: &[ResolvedChannel],
    mask: &[EventMaskRecord],
    working: &WorkingData,
    bins: &[Vec<usize>],
    events_per_bin: usize,
    peak_matrix: &PeakMatrix,
    mad_bad_bins: &[bool],
    it_bad_bins: &[bool],
    consecutive_bad_bins: &[bool],
    mad_contributions: Vec<MadContribution>,
    warnings: Vec<String>,
) -> PeacoQcSummary {
    let events_before = mask.len();
    let events_after = mask.iter().filter(|record| record.is_good).count();
    let removed_total = events_before - events_after;
    let margin_removed_count = mask
        .iter()
        .filter(|record| record.removed_by_margin)
        .count();
    let mad_removed_event_count = mask.iter().filter(|record| record.removed_by_mad).count();
    let it_removed_event_count = mask.iter().filter(|record| record.removed_by_it).count();
    let consecutive_removed_event_count = mask
        .iter()
        .filter(|record| record.removed_by_consecutive_bins)
        .count();
    let metrics = PeacoQcMetrics {
        events_before,
        events_after,
        removed_total,
        removed_total_fraction: fraction(removed_total, events_before),
        margin_removed_count,
        margin_removed_fraction: fraction(margin_removed_count, events_before),
        per_channel_margin_counts: working.margin_counts.clone(),
        mad_outlier_bins: one_based_bin_indexes(mad_bad_bins),
        mad_removed_event_count,
        mad_removed_fraction: fraction(mad_removed_event_count, events_before),
        mad_channel_contributions: mad_contributions,
        it_outlier_bins: one_based_bin_indexes(it_bad_bins),
        it_removed_event_count,
        it_removed_fraction: fraction(it_removed_event_count, events_before),
        consecutive_removed_bins: one_based_bin_indexes(consecutive_bad_bins),
        consecutive_removed_event_count,
        consecutive_removed_fraction: fraction(consecutive_removed_event_count, events_before),
        n_bins: bins.len(),
        events_per_bin,
        peak_feature_count: peak_matrix.feature_names.len(),
    };

    PeacoQcSummary {
        tool: "cytiq peacoqc".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        algorithm: "PeacoQC-compatible MVP".to_owned(),
        input: input.display().to_string(),
        output: output.display().to_string(),
        channels: channels
            .iter()
            .map(|channel| channel.display_name.clone())
            .collect(),
        parameters: PeacoQcParameters {
            method: config.method,
            mad: DEFAULT_MAD_THRESHOLD,
            it_limit: DEFAULT_IT_LIMIT,
            consecutive_bins: DEFAULT_CONSECUTIVE_BINS,
            min_cells: DEFAULT_MIN_CELLS,
            max_bins: DEFAULT_MAX_BINS,
            step: DEFAULT_STEP,
            force_it: DEFAULT_FORCE_IT_BINS,
            peak_removal: DEFAULT_PEAK_REMOVAL,
            min_peak_bin_percent: DEFAULT_MIN_PEAK_BIN_PERCENT,
            remove_zeros: config.remove_zeros,
            remove_margins: config.remove_margins,
            time_channel: DEFAULT_TIME_CHANNEL.to_owned(),
        },
        metrics,
        // Acceptance thresholds belong to `cytiq qc`; this only says whether
        // the run met inputs it did not expect.
        status: if warnings.is_empty() { "PASS" } else { "WARN" }.to_owned(),
        warnings,
    }
}

fn one_based_bin_indexes(bad_bins: &[bool]) -> Vec<usize> {
    bad_bins
        .iter()
        .enumerate()
        .filter_map(|(index, bad)| bad.then_some(index + 1))
        .collect()
}

fn count_working_events_from_bins(
    bad_bins: &[bool],
    bins: &[Vec<usize>],
    working_event_count: usize,
) -> usize {
    working_event_mask_from_bins(bad_bins, bins, working_event_count)
        .iter()
        .filter(|removed| **removed)
        .count()
}

fn display_parameter_identifier(parameter: &Parameter) -> String {
    match parameter.label.as_deref() {
        Some(label) if label != parameter.name => format!("{} [{}]", parameter.name, label),
        _ => parameter.name.clone(),
    }
}

fn column_value_to_f64(column: &ColumnData, index: usize) -> f64 {
    match column {
        ColumnData::UInt(values) => values[index] as f64,
        ColumnData::F32(values) => f64::from(values[index]),
        ColumnData::F64(values) => values[index],
    }
}

fn sample_standard_deviation(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let average = mean(values);
    let variance = values
        .iter()
        .map(|value| {
            let delta = value - average;
            delta * delta
        })
        .sum::<f64>()
        / ((values.len() - 1) as f64);
    variance.sqrt()
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// R's `median`, with `0` for no values.
fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        cytiq_density::median_mut(&mut values)
    }
}

fn median_absolute_deviation(values: &[f64]) -> f64 {
    let median_value = median(values.to_vec());
    let deviations = values
        .iter()
        .map(|value| (value - median_value).abs())
        .collect::<Vec<_>>();
    median(deviations) * MEDIAN_ABSOLUTE_DEVIATION_SCALE
}

fn fraction(count: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        count as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_rules_report_lower_floor_extrema_and_overlap_separately() {
        let values = [-500.0, -111.0, -110.0, 0.0, 1022.0, 1023.0];
        let channel = super::ChannelData {
            name: "FL1-A",
            values: &values,
        };
        let bounds = super::MarginBounds {
            lower: -111.0,
            upper: Some(1023.0),
        };
        let (low, high, counts) = super::margin_events_for_channel(&channel, &bounds);
        assert_eq!(low, [true, true, false, false, false, false]);
        assert_eq!(high, [false, false, false, false, false, true]);
        assert_eq!(
            (counts.min_count, counts.max_count, counts.removed_count),
            (2, 1, 3)
        );
        let discrete = super::ChannelData {
            name: "rare",
            values: &[1.0, 1.0, 2.0],
        };
        let (_, _, counts) = super::margin_events_for_channel(&discrete, &bounds);
        assert_eq!(counts.removed_count, 3); // Observed extrema, not physical saturation.
    }
    use super::*;

    #[test]
    fn computes_overlapping_bins() {
        let bins = make_bins(6, 14);
        assert_eq!(
            bins,
            vec![
                vec![0, 1, 2, 3, 4, 5],
                vec![3, 4, 5, 6, 7, 8],
                vec![6, 7, 8, 9, 10, 11],
                vec![9, 10, 11, 12, 13],
                vec![12, 13]
            ]
        );
    }

    #[test]
    fn removes_short_good_regions_at_file_edges() {
        // R: rle() marks every good run shorter than `consecutive_bins` as bad.
        let good = [
            true, true, false, true, true, true, true, true, true, false, true,
        ];
        let cleaned = remove_short_good_regions(&good, 5);
        assert_eq!(
            cleaned,
            vec![
                false, false, false, true, true, true, true, true, true, false, false
            ]
        );
    }

    #[test]
    fn isolation_split_picks_best_column_not_last_qualifying() {
        // column 0 has a clean two-cluster split, column 1 is a weak ramp
        let matrix = (0..20)
            .map(|row| {
                let first = if row < 10 { 0.0 } else { 10.0 } + f64::from(row) * 0.01;
                vec![first, f64::from(row)]
            })
            .collect::<Vec<_>>();
        let rows = (0..20).collect::<Vec<_>>();
        let split = best_isolation_split(&matrix, &rows, 0.0).expect("split");
        assert_eq!(split.column, 0);
    }

    #[test]
    fn removes_short_good_regions() {
        let cleaned = remove_short_good_regions(
            &[
                false, false, true, true, false, true, true, true, true, true,
            ],
            5,
        );
        assert_eq!(
            cleaned,
            vec![
                false, false, false, false, false, true, true, true, true, true
            ]
        );
    }

    #[test]
    fn detects_histogram_peaks() {
        let values = [1.0, 1.1, 1.2, 1.1, 10.0, 10.1, 10.2, 10.1, 10.0];
        let peaks = find_peaks(&values, false, DEFAULT_PEAK_REMOVAL);
        assert_eq!(peaks.len(), 2);
    }

    #[test]
    fn mad_detects_feature_outlier() {
        // R: which(PeacoQC:::MADOutliers(c(rep(1, 20), 1000, rep(1, 20)), 6)) == 13:29
        let matrix = PeakMatrix {
            values: (0..41)
                .map(|index| {
                    if index == 20 {
                        vec![1_000.0]
                    } else {
                        vec![1.0]
                    }
                })
                .collect(),
            feature_names: vec!["CD3:peak1".to_owned()],
        };
        let bins = (0..41).map(|index| vec![index]).collect::<Vec<_>>();
        let result = mad_bad_bins_for_features(&matrix, &[true; 41], &bins, 41, 41);
        let flagged = (0..41)
            .filter(|index| result.bad_bins[*index])
            .collect::<Vec<_>>();
        assert_eq!(flagged, (12..29).collect::<Vec<_>>());
    }

    #[test]
    fn isolation_children_partition_only_selected_rows_and_require_two_nonempty_children() {
        let matrix = vec![
            vec![0.0, 3.0],
            vec![1.0, 2.0],
            vec![2.0, 1.0],
            vec![3.0, 0.0],
        ];
        let selection = [false, true, true, false];
        let split = IsolationSplit {
            column: 1,
            value: 1.0,
            gain: 0.5,
        };
        assert_eq!(
            isolation_children(&matrix, &selection, split),
            Some([
                vec![false, false, true, false],
                vec![false, true, false, false]
            ])
        );
        for value in [-1.0, 3.0] {
            assert_eq!(
                isolation_children(&matrix, &selection, IsolationSplit { value, ..split }),
                None
            );
        }
        assert_eq!(isolation_children(&matrix, &[false; 4], split), None);
    }

    #[test]
    fn isolation_tree_preserves_small_and_unsplittable_samples() {
        assert_eq!(isolation_tree_good_bins(&[], 0.0), Vec::<bool>::new());
        for count in 1..=3 {
            let values = (0..count).map(|i| vec![f64::from(i)]).collect::<Vec<_>>();
            assert_eq!(
                isolation_tree_good_bins(&values, 0.0),
                vec![true; values.len()]
            );
        }
        assert_eq!(
            isolation_tree_good_bins(&vec![vec![2.0]; 8], 0.0),
            vec![true; 8]
        );
    }

    #[test]
    fn isolation_tree_keeps_the_largest_leaf_and_respects_gain_limit() {
        let matrix = vec![vec![0.0], vec![0.0], vec![10.0], vec![0.0]];
        assert_eq!(
            isolation_tree_good_bins(&matrix, 0.0),
            [true, true, false, true]
        );
        assert_eq!(isolation_tree_good_bins(&matrix, 1.0), [true; 4]);
        let tied = vec![vec![0.0], vec![0.0], vec![10.0], vec![10.0]];
        assert_eq!(
            isolation_tree_good_bins(&tied, 0.0),
            [true, true, false, false]
        );
    }

    #[test]
    fn isolation_tree_observes_depth_limit_and_can_keep_the_right_leaf() {
        let matrix = (0..8)
            .map(|i| vec![f64::from(i).sqrt().exp()])
            .collect::<Vec<_>>();
        assert_eq!(
            isolation_tree_good_bins(&matrix, 0.0),
            [true, true, true, true, true, false, false, false]
        );
        let right = vec![vec![10.0], vec![10.0], vec![0.0], vec![10.0]];
        assert_eq!(
            isolation_tree_good_bins(&right, 0.0),
            [true, true, false, true]
        );
    }

    #[test]
    fn remove_zeros_with_an_all_zero_channel_is_a_typed_error() {
        let signal = (0..2_000).map(f64::from).collect::<Vec<_>>();
        let zeros = vec![0.0; signal.len()];
        let mut config = super::PeacoQcConfig::new(vec!["FL1-A".into(), "FL2-A".into()]);
        config.remove_zeros = true;
        let error = super::peacoqc_mask(
            std::path::Path::new("memory"),
            &[("FL1-A", &signal), ("FL2-A", &zeros)],
            &config,
        )
        .expect_err("an all-zero channel leaves no events to bin under remove_zeros");
        assert!(
            matches!(
                &error,
                super::PeacoQcError::AllZeroChannel { channel, .. } if channel == "FL2-A"
            ),
            "{error}"
        );
    }
}
