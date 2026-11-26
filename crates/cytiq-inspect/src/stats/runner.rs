use std::io::Write;
use std::path::{Path, PathBuf};

use cytiq_fcs::{
    FcsDataset, FcsReader, MarginBounds, Parameter, dataset_margin_bounds, is_scatter_channel,
};
use cytiq_publish::{Existing, Prepared, Publication};
use log::debug;
use serde::Serialize;

use super::error::StatsError;
use super::summary::{Summary, bin, decreases, summarize};

const SCHEMA_VERSION: u32 = 1;
/// Histogram bins per channel unless `--bins` says otherwise.
pub const DEFAULT_BINS: usize = 256;
/// Bins of the event-rate histogram over the `Time` channel.
pub const TIME_BINS: usize = 100;
/// arcsinh cofactor of fluorescence histograms (the conventional value for
/// conventional cytometry, as `cytiq plot` uses).
const ARCSINH_COFACTOR: f64 = 150.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatsConfig {
    pub input: PathBuf,
    pub output: PathBuf,
    pub bins: usize,
    pub overwrite: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StatsReport {
    pub schema_version: u32,
    pub tool: String,
    pub version: String,
    pub file: FileIdentity,
    pub fcs_version: String,
    pub event_count: usize,
    pub parameter_count: usize,
    pub bins: usize,
    pub channels: Vec<ChannelStats>,
    /// `None` when the file has no `Time` channel.
    pub time: Option<TimeStats>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileIdentity {
    pub name: Option<String>,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Scatter,
    Time,
    Fluorescence,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelStats {
    pub index: u16,
    pub name: String,
    pub label: Option<String>,
    pub kind: ChannelKind,
    /// `$PnR`.
    pub range: Option<f64>,
    /// Raw values at or above this are at the detector limit (`$PnR - 1`).
    pub detector_limit: Option<f64>,
    #[serde(flatten)]
    pub summary: Summary,
    /// Events at or above `detector_limit`: saturated or margin events.
    pub at_detector_limit: usize,
    pub at_or_below_zero: usize,
    pub histogram: Histogram,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistogramScale {
    Linear,
    Arcsinh,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Histogram {
    pub scale: HistogramScale,
    pub cofactor: Option<f64>,
    /// Bin edges `[lo, hi]` on the histogram's scale.
    pub lo: f64,
    pub hi: f64,
    pub counts: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TimeStats {
    pub channel: String,
    pub monotonic: bool,
    pub decreases: usize,
    pub first: Option<f64>,
    pub last: Option<f64>,
    /// `$TIMESTEP` (seconds per time unit), when the file has one.
    pub timestep: Option<f64>,
    pub duration_seconds: Option<f64>,
    /// Events per equal-width time bin over `[first, last]`.
    pub counts: Vec<u64>,
}

#[derive(Debug, Default)]
pub struct StatsRunner;

impl StatsRunner {
    /// [`Self::prepare`], then publish, without a provenance record.
    ///
    /// # Errors
    ///
    /// As [`Self::prepare`], and what publishing returns.
    pub fn run(config: &StatsConfig) -> Result<StatsReport, StatsError> {
        Ok(Self::prepare(config)?.commit()?)
    }

    /// Read one FCS file and stage its statistics as pretty JSON. Nothing is
    /// published.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid bin count, an existing output without
    /// `overwrite`, an unreadable input or a failed write.
    pub fn prepare(config: &StatsConfig) -> Result<Prepared<StatsReport>, StatsError> {
        if !(2..=4096).contains(&config.bins) {
            return Err(StatsError::InvalidBins(config.bins));
        }
        let mut publication = Publication::new(Existing::overwrite(config.overwrite));
        publication.check(&config.output)?;
        let report = read_stats(&config.input, config.bins)?;
        let mut document = serde_json::to_vec_pretty(&report)?;
        document.push(b'\n');
        publication.stage_file(&config.output, |writer| {
            writer
                .write_all(&document)
                .map_err(|source| StatsError::OutputWrite {
                    path: config.output.clone(),
                    source,
                })
        })?;
        Ok(Prepared {
            summary: report,
            publication,
        })
    }
}

/// Statistics of one FCS file, in memory.
///
/// # Errors
///
/// Returns an error when the file cannot be read, hashed or parsed as FCS.
pub fn read_stats(path: impl AsRef<Path>, bins: usize) -> Result<StatsReport, StatsError> {
    let path = path.as_ref();
    debug!("reading {} for statistics", path.display());
    let (bytes, sha256) = cytiq_provenance::hash_file(path).map_err(|source| StatsError::Hash {
        path: path.to_path_buf(),
        source,
    })?;
    let read_error = |source| StatsError::FcsRead {
        path: path.to_path_buf(),
        source,
    };
    let dataset = FcsReader::open(path)
        .and_then(FcsReader::read)
        .map_err(read_error)?;
    let bounds = dataset_margin_bounds(&dataset.text, &dataset.parameters).map_err(read_error)?;
    let columns: Vec<Vec<f64>> = (0..dataset.parameters.len())
        .map(|index| {
            dataset
                .data
                .column(index)
                .map(cytiq_fcs::ColumnData::to_f64_vec)
                .unwrap_or_default()
        })
        .collect();
    let channels = dataset
        .parameters
        .iter()
        .zip(&columns)
        .map(|(parameter, values)| {
            let limit = bounds
                .get(&parameter.name)
                .copied()
                .unwrap_or_else(|| MarginBounds::from_raw_parameter(parameter))
                .detector_limit();
            channel_stats(&dataset, parameter, values, limit, bins)
        })
        .collect();
    Ok(StatsReport {
        schema_version: SCHEMA_VERSION,
        tool: "cytiq stats".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        file: FileIdentity {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            bytes,
            sha256,
        },
        fcs_version: dataset.version.as_str().to_owned(),
        event_count: dataset.event_count(),
        parameter_count: dataset.parameters.len(),
        bins,
        channels,
        time: time_stats(&dataset, &columns),
    })
}

fn kind(parameter: &Parameter) -> ChannelKind {
    if parameter.name.eq_ignore_ascii_case("time") {
        ChannelKind::Time
    } else if is_scatter_channel(&parameter.name) {
        ChannelKind::Scatter
    } else {
        ChannelKind::Fluorescence
    }
}

fn keyword_f64(dataset: &FcsDataset, key: &str) -> Option<f64> {
    dataset.text.get(key)?.trim().parse().ok()
}

fn channel_stats(
    dataset: &FcsDataset,
    parameter: &Parameter,
    values: &[f64],
    limit: f64,
    bins: usize,
) -> ChannelStats {
    let summary = summarize(values);
    let range = keyword_f64(dataset, &format!("$P{}R", parameter.index));
    let kind = kind(parameter);
    ChannelStats {
        index: parameter.index,
        name: parameter.name.clone(),
        label: parameter.label.clone(),
        kind,
        range,
        detector_limit: limit.is_finite().then_some(limit),
        at_detector_limit: values.iter().filter(|value| **value >= limit).count(),
        at_or_below_zero: values.iter().filter(|value| **value <= 0.0).count(),
        histogram: histogram(kind, values, &summary, range, bins),
        summary,
    }
}

/// Scatter and time on a linear scale over `[0, $PnR]`; fluorescence on
/// arcsinh (cofactor 150), which keeps negative values visible.
fn histogram(
    kind: ChannelKind,
    values: &[f64],
    summary: &Summary,
    range: Option<f64>,
    bins: usize,
) -> Histogram {
    let low = summary.min.unwrap_or(0.0).min(0.0);
    let high = range.unwrap_or(0.0).max(summary.max.unwrap_or(0.0));
    if kind == ChannelKind::Fluorescence {
        let transform = |value: f64| (value / ARCSINH_COFACTOR).asinh();
        let (lo, hi) = (transform(low), transform(high));
        return Histogram {
            scale: HistogramScale::Arcsinh,
            cofactor: Some(ARCSINH_COFACTOR),
            lo,
            hi,
            counts: bin(values, lo, hi, bins, transform),
        };
    }
    Histogram {
        scale: HistogramScale::Linear,
        cofactor: None,
        lo: low,
        hi: high,
        counts: bin(values, low, high, bins, |value| value),
    }
}

fn time_stats(dataset: &FcsDataset, columns: &[Vec<f64>]) -> Option<TimeStats> {
    let index = dataset
        .parameters
        .iter()
        .position(|parameter| kind(parameter) == ChannelKind::Time)?;
    let values = &columns[index];
    let timestep = keyword_f64(dataset, "$TIMESTEP");
    let (first, last) = (values.first().copied(), values.last().copied());
    let duration = match (first, last, timestep) {
        (Some(first), Some(last), Some(step)) => Some((last - first) * step),
        _ => None,
    };
    let decreases = decreases(values);
    Some(TimeStats {
        channel: dataset.parameters[index].name.clone(),
        monotonic: decreases == 0,
        decreases,
        first,
        last,
        timestep,
        duration_seconds: duration,
        counts: bin(
            values,
            first.unwrap_or(0.0),
            last.unwrap_or(0.0),
            TIME_BINS,
            |value| value,
        ),
    })
}
