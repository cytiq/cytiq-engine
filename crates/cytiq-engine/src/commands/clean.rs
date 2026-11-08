use std::path::PathBuf;

use clap::Args;
use cytiq_clean::{
    ChannelOverrides, CleanConfig, CleanRunner, DEFAULT_FSC_A, DEFAULT_FSC_H, DEFAULT_FSC_W,
    DEFAULT_MAX_FIT_EVENTS, DEFAULT_SINGLET_MAD, DEFAULT_SSC_A, DEFAULT_VIABILITY_COFACTOR,
    DEFAULT_VIABILITY_CONTROL_QUANTILE, DebrisConfig, FilterKind, FilterStatus, ViabilityConfig,
};
use cytiq_provenance::{files_under, record_path_for_dir};
use log::info;
use serde::Serialize;

use crate::cli::{CliDebrisMode, CliFilterKind};
use crate::parsers::{parse_finite_f64, parse_positive_f64, parse_threads};
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Args)]
pub(crate) struct CleanArgs {
    /// Input FCS file.
    #[arg(long)]
    input: PathBuf,
    /// Output directory for masks.parquet, metrics.json, gates.gatingml.xml
    /// and report.html.
    #[arg(long)]
    output: PathBuf,
    /// Event mask Parquet from an earlier stage; filters are fitted on kept
    /// events only and `keep` is false outside it. Repeat to combine masks.
    #[arg(long = "mask")]
    masks: Vec<PathBuf>,
    /// Comma-separated filters that must be applied (`debris`, `singlets`,
    /// `viability`); unmet checks block acceptance and retain diagnostics.
    #[arg(long, value_enum, value_delimiter = ',')]
    require: Vec<CliFilterKind>,
    /// Re-apply these Gating-ML gates (e.g. an edited gates.gatingml.xml)
    /// instead of fitting new ones.
    #[arg(long)]
    gates: Option<PathBuf>,
    #[arg(long, default_value = DEFAULT_FSC_A)]
    fsc: String,
    #[arg(long, default_value = DEFAULT_FSC_H)]
    fsc_h: String,
    #[arg(long, default_value = DEFAULT_FSC_W)]
    fsc_w: String,
    #[arg(long, default_value = DEFAULT_SSC_A)]
    ssc: String,
    #[arg(long, value_enum, default_value_t = CliDebrisMode::Adaptive)]
    debris_mode: CliDebrisMode,
    /// Minimum FSC for `--debris-mode fixed`.
    #[arg(long, value_parser = parse_finite_f64)]
    debris_fsc_min: Option<f64>,
    /// Minimum SSC for `--debris-mode fixed`.
    #[arg(long, value_parser = parse_finite_f64)]
    debris_ssc_min: Option<f64>,
    /// Singlet band half-width, in MADs of the FSC-A/FSC-H ratio (or FSC-W).
    #[arg(long, value_parser = parse_positive_f64, default_value_t = DEFAULT_SINGLET_MAD)]
    singlet_mad: f64,
    /// Viability dye channel; auto-detected from $PnN/$PnS when omitted.
    #[arg(long, conflicts_with = "no_viability")]
    viability_channel: Option<String>,
    /// Skip the viability filter.
    #[arg(long)]
    no_viability: bool,
    /// Unstained/FMO control FCS; its upper quantile sets the live/dead threshold.
    #[arg(long, conflicts_with = "viability_threshold")]
    viability_control: Option<PathBuf>,
    #[arg(long, value_parser = parse_positive_f64, default_value_t = DEFAULT_VIABILITY_CONTROL_QUANTILE)]
    viability_control_quantile: f64,
    /// Explicit live/dead threshold in compensated intensity units.
    #[arg(long, value_parser = parse_finite_f64)]
    viability_threshold: Option<f64>,
    /// Arcsinh cofactor for the viability channel.
    #[arg(long, value_parser = parse_positive_f64, default_value_t = DEFAULT_VIABILITY_COFACTOR)]
    viability_cofactor: f64,
    /// Fit gates on at most this many evenly spaced events.
    #[arg(long, default_value_t = DEFAULT_MAX_FIT_EVENTS)]
    max_fit_events: usize,
    /// Worker thread limit; defaults to all available logical CPUs.
    #[arg(long, value_parser = parse_threads)]
    threads: Option<usize>,
}

impl From<CleanArgs> for CleanConfig {
    fn from(args: CleanArgs) -> Self {
        Self {
            input: args.input,
            output: args.output,
            channels: ChannelOverrides {
                fsc_a: args.fsc,
                fsc_h: args.fsc_h,
                fsc_w: args.fsc_w,
                ssc_a: args.ssc,
                viability: args.viability_channel,
                no_viability: args.no_viability,
            },
            debris: DebrisConfig {
                mode: args.debris_mode.into(),
                fsc_min: args.debris_fsc_min,
                ssc_min: args.debris_ssc_min,
            },
            singlet_mad: args.singlet_mad,
            viability: ViabilityConfig {
                cofactor: args.viability_cofactor,
                control: args.viability_control,
                control_quantile: args.viability_control_quantile,
                threshold: args.viability_threshold,
            },
            gates: args.gates,
            max_fit_events: args.max_fit_events,
            threads: args.threads,
            masks: args.masks,
            required: args.require.into_iter().map(FilterKind::from).collect(),
        }
    }
}

pub(crate) fn run(
    args: CleanArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    info!(
        "starting clean: input={}, output={}",
        args.input.display(),
        args.output.display()
    );
    recorder.snapshot_inputs("input", std::slice::from_ref(&args.input))?;
    recorder.snapshot_inputs("mask", &args.masks)?;
    recorder.snapshot_inputs("gating_ml", args.gates.as_slice())?;
    recorder.snapshot_inputs("control", args.viability_control.as_slice())?;
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::Dir(args.output.clone())
    );
    let config: CleanConfig = args.into();
    let result = match CleanRunner::run(&config) {
        Ok(result) => result,
        Err(error @ cytiq_clean::CleanError::RequiredFilterNotApplied { .. }) => {
            let mut outcome = crate::provenance::record_with_reason(
                recorder,
                &files_under(&config.output),
                &record_path_for_dir(&config.output),
                cytiq_provenance::outcome::OutcomeReason::ChecksFailed,
            )?;
            if let Some(outcome) = &mut outcome {
                outcome.message = error.to_string();
            }
            log::error!("{error}");
            return Ok(outcome);
        }
        Err(error) => return Err(error.into()),
    };
    let metrics = &result.metrics;
    for (name, report) in [
        ("debris", &metrics.filters.debris),
        ("singlets", &metrics.filters.singlets),
        ("viability", &metrics.filters.viability),
    ] {
        match report.status {
            FilterStatus::Applied => info!(
                "{name}: removed {} event(s)",
                report.removed.unwrap_or_default()
            ),
            _ => log::warn!(
                "{name}: {:?} ({})",
                report.status,
                report.reason.as_deref().unwrap_or_default()
            ),
        }
    }
    info!(
        "clean kept {} of {} event(s); wrote {}",
        metrics.kept,
        metrics.event_count,
        result.output.display()
    );
    record_provenance(
        recorder,
        &files_under(&result.output),
        &record_path_for_dir(&result.output),
    )
}
