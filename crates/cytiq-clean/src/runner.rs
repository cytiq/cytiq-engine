use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use cytiq_density::sample_indices;
use cytiq_fcs::{FcsDataset, FcsReader};
use cytiq_gating::{
    BooleanOp, Gate, GateKind, GateRef, GatingResults, GatingStrategy, Transform, parse_gatingml,
    to_gatingml,
};
use log::{debug, info};
use rayon::ThreadPoolBuilder;
use serde::Serialize;
use serde_json::json;

use crate::channels::{ChannelOverrides, ChannelRoles};
use crate::debris::{self, DebrisConfig, DebrisMode};
use crate::error::CleanError;
use crate::export::write_outputs;
use crate::filter::{
    CLEAN_GATE, FilterFit, FilterReport, FilterStatus, LIVE_GATE, NON_DEBRIS_GATE, SINGLETS_GATE,
};
use crate::singlets;
use crate::viability::{self, ViabilityConfig};

pub const DEFAULT_FSC_A: &str = "FSC-A";
pub const DEFAULT_FSC_H: &str = "FSC-H";
pub const DEFAULT_FSC_W: &str = "FSC-W";
pub const DEFAULT_SSC_A: &str = "SSC-A";
pub const DEFAULT_SINGLET_MAD: f64 = 3.0;
/// Gates are fitted on an evenly spaced subset of at most this many events
/// and then evaluated on every event.
pub const DEFAULT_MAX_FIT_EVENTS: usize = 50_000;

#[derive(Clone, Debug, PartialEq)]
pub struct CleanConfig {
    pub input: PathBuf,
    /// Output directory for `masks.parquet`, `metrics.json`,
    /// `gates.gatingml.xml` and `report.html`.
    pub output: PathBuf,
    pub channels: ChannelOverrides,
    pub debris: DebrisConfig,
    pub singlet_mad: f64,
    pub viability: ViabilityConfig,
    /// Re-apply these (e.g. hand-edited) gates instead of fitting new ones.
    pub gates: Option<PathBuf>,
    pub max_fit_events: usize,
    pub threads: Option<usize>,
    /// Event masks (`masks.parquet`) from earlier stages. Filters are fitted
    /// on events every mask keeps, and `keep` is false for the others.
    pub masks: Vec<PathBuf>,
    /// Filters that must be `applied`; any other status fails the run before
    /// outputs are written.
    pub required: Vec<FilterKind>,
}

/// One of the three clean filters.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    Debris,
    Singlets,
    Viability,
}

impl FilterKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debris => "debris",
            Self::Singlets => "singlets",
            Self::Viability => "viability",
        }
    }
}

impl CleanConfig {
    #[must_use]
    pub fn new(input: PathBuf, output: PathBuf) -> Self {
        Self {
            input,
            output,
            channels: ChannelOverrides {
                fsc_a: DEFAULT_FSC_A.to_owned(),
                fsc_h: DEFAULT_FSC_H.to_owned(),
                fsc_w: DEFAULT_FSC_W.to_owned(),
                ssc_a: DEFAULT_SSC_A.to_owned(),
                viability: None,
                no_viability: false,
            },
            debris: DebrisConfig::default(),
            singlet_mad: DEFAULT_SINGLET_MAD,
            viability: ViabilityConfig::default(),
            gates: None,
            max_fit_events: DEFAULT_MAX_FIT_EVENTS,
            threads: None,
            masks: Vec::new(),
            required: Vec::new(),
        }
    }
}

/// The three filter sections of `metrics.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FilterReports {
    pub debris: FilterReport,
    pub singlets: FilterReport,
    pub viability: FilterReport,
}

impl FilterReports {
    /// Filter names as used in `removal_overlap`, in column order.
    const NAMES: [&'static str; 3] = ["debris", "singlets", "viability"];

    fn each_mut(&mut self) -> [&mut FilterReport; 3] {
        [&mut self.debris, &mut self.singlets, &mut self.viability]
    }

    #[must_use]
    pub const fn get(&self, kind: FilterKind) -> &FilterReport {
        match kind {
            FilterKind::Debris => &self.debris,
            FilterKind::Singlets => &self.singlets,
            FilterKind::Viability => &self.viability,
        }
    }
}

/// Contents of `metrics.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CleanMetrics {
    pub input: String,
    /// `fitted`, or the path of the `GatingML` passed with `--gates`.
    pub gates_source: String,
    pub event_count: usize,
    pub event_id: &'static str,
    /// Present when the input carries `Original_ID` (e.g. `PeacoQC` output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<&'static str>,
    pub kept: usize,
    pub kept_fraction: f64,
    /// Which gates define the `keep` column.
    pub keep_rule: String,
    pub channels: ChannelRoles,
    pub filters: FilterReports,
    /// Removed events by the exact combination of filters that removed them.
    pub removal_overlap: BTreeMap<String, usize>,
    /// Event masks the run was restricted to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_mask: Option<InputMaskReport>,
}

/// The `input_mask` section of `metrics.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InputMaskReport {
    pub paths: Vec<String>,
    /// Events every input mask keeps; only these can have `keep = true`.
    pub kept: usize,
}

/// Per-event filter results; `None` columns were not assessed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanMasks {
    pub non_debris: Option<Vec<bool>>,
    pub singlets: Option<Vec<bool>>,
    pub live: Option<Vec<bool>>,
    pub keep: Vec<bool>,
    /// 0-based raw-file row of each event, from the input's `Original_ID`.
    pub source_event_ids: Option<Vec<u64>>,
    /// The input's raw acquisition (`cytiq_fcs::acquisition_id`).
    pub acquisition: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CleanResult {
    pub metrics: CleanMetrics,
    pub output: PathBuf,
}

#[derive(Debug, Default)]
pub struct CleanRunner;

impl CleanRunner {
    /// Fit (or re-apply) debris, singlet and viability gates on one FCS file
    /// and write masks, metrics, `GatingML` and an HTML report.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration is invalid, inputs cannot be
    /// read, required channels are missing, or outputs cannot be written.
    pub fn run(config: &CleanConfig) -> Result<CleanResult, CleanError> {
        validate(config)?;
        match config.threads {
            Some(threads) => {
                debug!("using local rayon thread pool with {threads} threads");
                ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .map_err(|source| CleanError::ThreadPoolBuild { threads, source })?
                    .install(|| run_inner(config))
            }
            None => run_inner(config),
        }
    }
}

fn validate(config: &CleanConfig) -> Result<(), CleanError> {
    let invalid = |message: &str| Err(CleanError::InvalidConfig(message.to_owned()));
    if config.threads == Some(0) {
        return Err(CleanError::InvalidThreads);
    }
    if config.debris.mode == DebrisMode::Fixed
        && config.debris.fsc_min.is_none()
        && config.debris.ssc_min.is_none()
    {
        return invalid("--debris-mode fixed needs --debris-fsc-min and/or --debris-ssc-min");
    }
    if config.debris.mode != DebrisMode::Fixed
        && (config.debris.fsc_min.is_some() || config.debris.ssc_min.is_some())
    {
        return invalid("--debris-fsc-min/--debris-ssc-min require --debris-mode fixed");
    }
    if config.debris.mode == DebrisMode::Off && config.required.contains(&FilterKind::Debris) {
        return invalid("--require debris and --debris-mode off are mutually exclusive");
    }
    // `--gates` replays the file's `non_debris` gate whatever the mode, so `off`
    // would be silently ignored.
    if config.debris.mode == DebrisMode::Off && config.gates.is_some() {
        return invalid(
            "--debris-mode off and --gates are mutually exclusive; remove the `non_debris` gate from the file instead",
        );
    }
    if !(config.singlet_mad.is_finite() && config.singlet_mad > 0.0) {
        return invalid("--singlet-mad must be positive");
    }
    let viability = &config.viability;
    if !(viability.cofactor.is_finite() && viability.cofactor > 0.0) {
        return invalid("--viability-cofactor must be positive");
    }
    if !(viability.control_quantile > 0.0 && viability.control_quantile < 1.0) {
        return invalid("--viability-control-quantile must be in (0, 1)");
    }
    if viability.control.is_some() && viability.threshold.is_some() {
        return invalid("--viability-control and --viability-threshold are mutually exclusive");
    }
    if config.channels.no_viability && config.channels.viability.is_some() {
        return invalid("--no-viability and --viability-channel are mutually exclusive");
    }
    if config.channels.no_viability && config.required.contains(&FilterKind::Viability) {
        return invalid("--require viability and --no-viability are mutually exclusive");
    }
    if config.max_fit_events < crate::filter::MIN_FIT_EVENTS {
        return invalid("--max-fit-events is too small");
    }
    Ok(())
}

/// The input's raw acquisition and the raw row of each of its events.
fn lineage(
    config: &CleanConfig,
    dataset: &FcsDataset,
) -> Result<(String, Option<Vec<u64>>), CleanError> {
    let read_error = |source| CleanError::FcsRead {
        path: config.input.clone(),
        source,
    };
    Ok((
        cytiq_fcs::acquisition_id(&config.input, &dataset.text).map_err(read_error)?,
        cytiq_fcs::source_event_ids(dataset).map_err(read_error)?,
    ))
}

/// Gates to evaluate plus the report sections and the `GatingML` to write.
struct Plan {
    strategy: GatingStrategy,
    reports: FilterReports,
    gatingml: String,
    source: String,
}

fn run_inner(config: &CleanConfig) -> Result<CleanResult, CleanError> {
    let dataset = read_fcs(&config.input)?;
    let roles = ChannelRoles::resolve(&dataset, &config.channels, &config.input)?;
    ensure_raw_roles(config, &dataset, &roles)?;
    info!(
        "clean: {} event(s); FSC-A={}, FSC-H={:?}, FSC-W={:?}, SSC-A={:?}, viability={:?} ({})",
        dataset.event_count(),
        roles.fsc_a,
        roles.fsc_h,
        roles.fsc_w,
        roles.ssc_a,
        roles.viability,
        roles.viability_source
    );

    let (acquisition, source_event_ids) = lineage(config, &dataset)?;
    let input_mask = cytiq_mask::read_combined(&config.masks)?
        .map(|mask| mask.keep_for(&dataset, &acquisition))
        .transpose()?;
    let Plan {
        strategy,
        mut reports,
        gatingml,
        source,
    } = match &config.gates {
        Some(path) => supplied_plan(path)?,
        None => fitted_plan(&dataset, &roles, config, input_mask.as_deref())?,
    };
    let results = strategy.gate_sample(&dataset)?;
    let mut masks = collect_masks(&results, &reports);
    // Filter columns stay evaluated on every event, so `cytiq gate` on the
    // written GatingML still reproduces them; only `keep` honours the mask.
    if let Some(input_mask) = &input_mask {
        for (keep, kept) in masks.keep.iter_mut().zip(input_mask) {
            *keep &= *kept;
        }
    }
    masks.source_event_ids = source_event_ids;
    masks.acquisition = acquisition;
    let event_count = dataset.event_count();
    let removal_overlap = count_masks(&mut reports, &masks, event_count);
    let kept = masks.keep.iter().filter(|keep| **keep).count();
    let keep_rule = if strategy.gate(CLEAN_GATE).is_some() {
        format!("gate `{CLEAN_GATE}`")
    } else {
        let applied = [
            (&masks.non_debris, NON_DEBRIS_GATE),
            (&masks.singlets, SINGLETS_GATE),
            (&masks.live, LIVE_GATE),
        ]
        .into_iter()
        .filter_map(|(mask, id)| mask.as_ref().map(|_| id))
        .collect::<Vec<_>>();
        if applied.is_empty() {
            "all events (no filter applied)".to_owned()
        } else {
            applied.join(" AND ")
        }
    };

    let metrics = CleanMetrics {
        input: config.input.display().to_string(),
        gates_source: source,
        event_count,
        event_id: "0-based row index of the event in the input FCS",
        source_event_id: masks
            .source_event_ids
            .as_ref()
            .map(|_| "0-based row index of the event in the raw FCS, from the input's Original_ID"),
        kept,
        kept_fraction: fraction(kept, event_count),
        keep_rule,
        channels: roles,
        filters: reports,
        removal_overlap,
        input_mask: input_mask.map(|mask| InputMaskReport {
            paths: config
                .masks
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            kept: mask.iter().filter(|kept| **kept).count(),
        }),
    };
    write_outputs(
        &config.output,
        &metrics,
        &masks,
        &gatingml,
        &strategy,
        &dataset,
        &results,
    )?;
    info!(
        "clean kept {kept} of {event_count} event(s); wrote {}",
        config.output.display()
    );
    // An unmet assessment is a completed diagnostic run, not an I/O or
    // computation failure. Preserve the evidence before returning the error.
    ensure_required(&config.required, &metrics.filters)?;
    Ok(CleanResult {
        metrics,
        output: config.output.clone(),
    })
}

/// Scatter filters are fitted on raw linear scatter, and the viability axis
/// compensates and arcsinh-scales the dye itself, so channels `cytiq
/// transform` already transformed cannot be fitted.
fn ensure_raw_roles(
    config: &CleanConfig,
    dataset: &FcsDataset,
    roles: &ChannelRoles,
) -> Result<(), CleanError> {
    let state =
        cytiq_fcs::SignalState::of(&dataset.text).map_err(|source| CleanError::FcsRead {
            path: config.input.clone(),
            source,
        })?;
    let scatter = std::iter::once(&roles.fsc_a)
        .chain(roles.fsc_h.iter())
        .chain(roles.fsc_w.iter())
        .chain(roles.ssc_a.iter());
    if let Some(channel) = scatter
        .into_iter()
        .find(|channel| state.transformed(channel))
    {
        return Err(CleanError::TransformedScatter {
            channel: channel.clone(),
        });
    }
    match &roles.viability {
        Some(channel) if state.transformed(channel) => Err(CleanError::TransformedViability {
            channel: channel.clone(),
        }),
        _ => Ok(()),
    }
}

/// `--require`: reject the result when a required filter could not be applied.
fn ensure_required(required: &[FilterKind], reports: &FilterReports) -> Result<(), CleanError> {
    for kind in required {
        let report = reports.get(*kind);
        if report.status != FilterStatus::Applied {
            return Err(CleanError::RequiredFilterNotApplied {
                filter: kind.as_str(),
                status: report.status.as_str(),
                reason: report.reason.clone().unwrap_or_default(),
            });
        }
    }
    Ok(())
}

fn read_fcs(path: &Path) -> Result<FcsDataset, CleanError> {
    FcsReader::open(path)
        .and_then(FcsReader::read)
        .map_err(|source| CleanError::FcsRead {
            path: path.to_path_buf(),
            source,
        })
}

/// Fit the filters in turn: singlets are fitted on non-debris events and
/// viability on non-debris singlets, so that debris and doublets do not
/// distort the fits. Every gate is still evaluated on all events.
fn fitted_plan(
    dataset: &FcsDataset,
    roles: &ChannelRoles,
    config: &CleanConfig,
    input_mask: Option<&[bool]>,
) -> Result<Plan, CleanError> {
    let limit = config.max_fit_events;
    let mut eligible =
        input_mask.map_or_else(|| vec![true; dataset.event_count()], <[bool]>::to_vec);

    let debris = debris::fit(
        dataset,
        roles,
        &config.debris,
        &sample_indices(&eligible, limit),
    )?;
    restrict(&mut eligible, dataset, &debris, &HashMap::new())?;
    let singlets = singlets::fit(
        dataset,
        roles,
        config.singlet_mad,
        &sample_indices(&eligible, limit),
    )?;
    restrict(&mut eligible, dataset, &singlets, &HashMap::new())?;
    let (viability, axis) = viability::fit(
        dataset,
        roles,
        &config.viability,
        &sample_indices(&eligible, limit),
    )?;

    let transforms = match (&viability.gate, &axis) {
        (Some(_), Some(axis)) => axis.transforms(),
        _ => HashMap::new(),
    };
    let mut gates = [&debris, &singlets, &viability]
        .into_iter()
        .filter_map(|fit| fit.gate.clone())
        .collect::<Vec<_>>();
    if !gates.is_empty() {
        let mut refs = gates
            .iter()
            .map(|gate| GateRef {
                id: gate.id.clone(),
                complement: false,
            })
            .collect::<Vec<_>>();
        if refs.len() == 1 {
            // GatingML `and` needs two operands; AND-ing a gate with itself
            // keeps `clean` present whenever any filter applied.
            refs.push(refs[0].clone());
        }
        gates.push(Gate {
            id: CLEAN_GATE.to_owned(),
            parent_id: None,
            kind: GateKind::Boolean {
                op: BooleanOp::And,
                refs,
            },
        });
    }

    let strategy = GatingStrategy::new(gates, transforms, HashMap::new())?;
    let gatingml = to_gatingml(&strategy)?;
    Ok(Plan {
        strategy,
        reports: FilterReports {
            debris: debris.report,
            singlets: singlets.report,
            viability: viability.report,
        },
        gatingml,
        source: "fitted".to_owned(),
    })
}

/// Narrow `eligible` to the events inside `fit`'s gate, if it has one.
fn restrict(
    eligible: &mut [bool],
    dataset: &FcsDataset,
    fit: &FilterFit,
    transforms: &HashMap<String, Transform>,
) -> Result<(), CleanError> {
    let Some(gate) = &fit.gate else {
        return Ok(());
    };
    let strategy = GatingStrategy::new(vec![gate.clone()], transforms.clone(), HashMap::new())?;
    let results = strategy.gate_sample(dataset)?;
    let membership = results
        .membership(&gate.id)
        .expect("the gate was just evaluated");
    for (event, inside) in eligible.iter_mut().zip(membership) {
        *event &= *inside;
    }
    Ok(())
}

/// Re-apply a `GatingML` file, typically `gates.gatingml.xml` from an earlier
/// run after manual edits.
fn supplied_plan(path: &Path) -> Result<Plan, CleanError> {
    let gatingml = fs::read_to_string(path).map_err(|source| CleanError::GatingMlRead {
        path: path.to_path_buf(),
        source,
    })?;
    let strategy = parse_gatingml(&gatingml)?;
    let report = |gate_id: &'static str| match strategy.gate(gate_id) {
        Some(gate) => FilterReport::applied(
            gate_id,
            "supplied_gatingml",
            gate.dimensions()
                .iter()
                .map(|dimension| dimension.name.clone())
                .collect(),
            json!({ "gate_type": gate.gate_type() }),
        ),
        None => FilterReport::skipped(
            gate_id,
            FilterStatus::NotAssessed,
            format!("gate `{gate_id}` is absent from `{}`", path.display()),
        ),
    };
    let reports = FilterReports {
        debris: report(NON_DEBRIS_GATE),
        singlets: report(SINGLETS_GATE),
        viability: report(LIVE_GATE),
    };
    Ok(Plan {
        strategy,
        reports,
        gatingml,
        source: path.display().to_string(),
    })
}

fn collect_masks(results: &GatingResults, reports: &FilterReports) -> CleanMasks {
    let mask = |report: &FilterReport| {
        (report.status == FilterStatus::Applied)
            .then(|| results.membership(report.gate_id).map(<[bool]>::to_vec))
            .flatten()
    };
    let non_debris = mask(&reports.debris);
    let singlets = mask(&reports.singlets);
    let live = mask(&reports.viability);
    let keep = results.membership(CLEAN_GATE).map_or_else(
        || {
            (0..results.event_count())
                .map(|event| {
                    [&non_debris, &singlets, &live]
                        .into_iter()
                        .flatten()
                        .all(|mask| mask[event])
                })
                .collect()
        },
        <[bool]>::to_vec,
    );
    CleanMasks {
        non_debris,
        singlets,
        live,
        keep,
        source_event_ids: None,
        acquisition: String::new(),
    }
}

/// Fill per-filter counts and return the removal-overlap table.
fn count_masks(
    reports: &mut FilterReports,
    masks: &CleanMasks,
    event_count: usize,
) -> BTreeMap<String, usize> {
    let columns = [&masks.non_debris, &masks.singlets, &masks.live];
    for (report, mask) in reports.each_mut().into_iter().zip(columns) {
        if let Some(mask) = mask {
            let passed = mask.iter().filter(|inside| **inside).count();
            report.passed = Some(passed);
            report.removed = Some(event_count - passed);
            report.removed_fraction = Some(fraction(event_count - passed, event_count));
        }
    }

    let mut overlap = BTreeMap::new();
    for event in 0..event_count {
        let removed_by = FilterReports::NAMES
            .iter()
            .zip(columns)
            .filter_map(|(name, mask)| mask.as_ref().filter(|mask| !mask[event]).map(|_| *name))
            .collect::<Vec<_>>();
        if !removed_by.is_empty() {
            *overlap.entry(removed_by.join("+")).or_insert(0) += 1;
        }
    }
    overlap
}

#[allow(clippy::cast_precision_loss)]
fn fraction(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}
