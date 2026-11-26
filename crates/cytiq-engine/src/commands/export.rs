use std::io;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use cytiq_population::events::{
    BaseState, Codec, DEFAULT_ROW_GROUP_ROWS, DEFAULT_ZSTD_LEVEL, ExportConfig, ExportRunner,
    LayerState, WriteSettings,
};
use cytiq_population::labels::{
    ClusterModel, ExportLabelsConfig, GatingDefinition, LabelKind, LabelSettings, LabelsRunner,
};
use cytiq_provenance::outcome::OutcomeReason;
use log::info;
use serde::Serialize;

use crate::parsers::{parse_byte_size, parse_channels, parse_positive_usize};
use crate::provenance::publish;

#[derive(Debug, Serialize, Subcommand)]
pub(crate) enum ExportCommand {
    /// Write one FCS file's events as a `cytiq.events.v1` Parquet table:
    /// acquisition, signal layer and raw event id in every row, then the
    /// channels in `$Pn` order, bit-exact. Every raw event is kept; no
    /// identifying FCS keyword is written. The table is read back and
    /// verified before it is published.
    Events(ExportEventsArgs),
    /// Write what one cytiq stage decided about each event (a QC mask, gate
    /// membership, `FlowSOM` assignments or classifier predictions) as a
    /// `cytiq.events.labels.v1` Parquet table: one row per raw event with
    /// its state, keyed like events tables by acquisition and raw event id.
    /// Nothing is recomputed. The input must be listed by its producer's
    /// provenance record; the table is read back and verified before it is
    /// published.
    Labels(ExportLabelsArgs),
}

#[derive(Debug, Serialize, Args)]
pub(crate) struct ExportLabelsArgs {
    /// What the input holds.
    #[arg(long, value_enum)]
    kind: CliLabelKind,
    /// The per-event output: a mask (`qc`), a `cytiq gate --membership`
    /// table (`gate`), `FlowSOM` assignments (`cluster`) or
    /// `predictions.parquet` (`classifier`).
    #[arg(long)]
    input: PathBuf,
    /// The provenance record of the run that wrote the input.
    #[arg(long)]
    producer_provenance: PathBuf,
    /// The events table, or parts directory, holding every raw event of the
    /// acquisition. When it was exported from the file the producer read,
    /// the labels record its signal layer.
    #[arg(long)]
    events: PathBuf,
    /// A QC mask the producer read; can be repeated. Given all of them, the
    /// events they exclude are `qc_rejected` rather than `not_assessed`.
    #[arg(long = "qc-mask")]
    qc_masks: Vec<PathBuf>,
    /// The Gating-ML file `cytiq gate` evaluated (`--kind gate`).
    #[arg(long)]
    gating_ml: Option<PathBuf>,
    /// The model `cytiq flowsom apply` applied (`--kind cluster`).
    #[arg(long)]
    flowsom_model: Option<PathBuf>,
    /// Leave out the classifier's per-class probabilities.
    #[arg(long)]
    no_probabilities: bool,
    /// Label table (`.parquet`); its provenance record is written beside it.
    #[arg(long)]
    output: PathBuf,
    /// Replace an existing table.
    #[arg(long)]
    overwrite: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliLabelKind {
    Qc,
    Gate,
    Cluster,
    Classifier,
}

impl From<CliLabelKind> for LabelKind {
    fn from(value: CliLabelKind) -> Self {
        match value {
            CliLabelKind::Qc => Self::Qc,
            CliLabelKind::Gate => Self::Gate,
            CliLabelKind::Cluster => Self::Cluster,
            CliLabelKind::Classifier => Self::Classifier,
        }
    }
}

#[derive(Debug, Serialize, Args)]
pub(crate) struct ExportEventsArgs {
    /// FCS file: raw, or written by a cytiq stage.
    #[arg(long)]
    input: PathBuf,
    /// Events table (`.parquet`), or the parts directory with
    /// `--max-file-bytes`; its provenance record is written beside it (in
    /// it for a directory).
    #[arg(long)]
    output: PathBuf,
    /// The signal layer the input holds; a file holding another layer is
    /// refused.
    #[arg(long, value_enum)]
    layer: CliLayer,
    /// What the values were before any cytiq stage, for files compensated
    /// or unmixed by other software (the panel's `signal_state`).
    #[arg(long, value_enum, default_value = "raw")]
    base_state: CliBaseState,
    /// The matrix file `cytiq compensate --matrix` applied to the input;
    /// required for such files, recorded only as its SHA-256.
    #[arg(long)]
    compensation_matrix: Option<PathBuf>,
    /// The reference `cytiq autospectral` unmixed the input with; required
    /// for unmixed files, recorded only as its SHA-256.
    #[arg(long)]
    unmixing_reference: Option<PathBuf>,
    /// Store only these channels (comma-separated `$PnN`), in `$Pn` order;
    /// the keys are always stored. Default: every channel.
    #[arg(long)]
    channels: Option<String>,
    /// A channel to store; can be repeated.
    #[arg(long = "channel")]
    channel: Vec<String>,
    /// Events decoded per batch. Default: as many as fit in 16 MiB, at most
    /// 65,536; a batch may not exceed 256 MiB.
    #[arg(long, value_parser = parse_positive_usize)]
    batch_size: Option<usize>,
    /// Most events per row group; a row group is also cut at 64 MiB encoded.
    #[arg(long, value_parser = parse_positive_usize, default_value_t = DEFAULT_ROW_GROUP_ROWS)]
    row_group_size: usize,
    #[arg(long, value_enum, default_value_t = CliCompression::Zstd)]
    compression: CliCompression,
    /// Zstandard level, 1 to 22 [default: 1].
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..=22))]
    compression_level: Option<i32>,
    /// Split the layer into part tables of at most this many bytes of
    /// uncompressed values each (`512MiB`; units B, KiB, MiB, GiB, TiB).
    /// `--output` is then a directory holding the parts and `parts.json`.
    #[arg(long, value_parser = parse_byte_size)]
    max_file_bytes: Option<u64>,
    /// Accept an input whose rows are a subset of the acquisition (it has
    /// `Original_ID`, as QC-filtered and normalized files do). Without it,
    /// export the unfiltered layer and keep filters as masks.
    #[arg(long)]
    allow_subset: bool,
    /// Replace an existing table or parts directory.
    #[arg(long)]
    overwrite: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliLayer {
    Raw,
    Compensated,
    Unmixed,
    Transformed,
    Normalized,
}

impl From<CliLayer> for LayerState {
    fn from(value: CliLayer) -> Self {
        match value {
            CliLayer::Raw => Self::Raw,
            CliLayer::Compensated => Self::Compensated,
            CliLayer::Unmixed => Self::Unmixed,
            CliLayer::Transformed => Self::Transformed,
            CliLayer::Normalized => Self::Normalized,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliBaseState {
    Raw,
    Compensated,
    Unmixed,
}

impl From<CliBaseState> for BaseState {
    fn from(value: CliBaseState) -> Self {
        match value {
            CliBaseState::Raw => Self::Raw,
            CliBaseState::Compensated => Self::Compensated,
            CliBaseState::Unmixed => Self::Unmixed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliCompression {
    Zstd,
    Snappy,
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl TryFrom<ExportEventsArgs> for ExportConfig {
    type Error = io::Error;

    fn try_from(args: ExportEventsArgs) -> io::Result<Self> {
        let channels = if args.channels.is_none() && args.channel.is_empty() {
            None
        } else {
            Some(parse_channels(args.channels.as_deref(), &args.channel)?)
        };
        let codec = match (args.compression, args.compression_level) {
            (CliCompression::Zstd, level) => Codec::Zstd(level.unwrap_or(DEFAULT_ZSTD_LEVEL)),
            (CliCompression::Snappy, None) => Codec::Snappy,
            (CliCompression::Snappy, Some(_)) => {
                return Err(invalid_input(
                    "`--compression-level` applies to `--compression zstd` only",
                ));
            }
        };
        Ok(Self {
            input: args.input,
            output: args.output,
            layer: args.layer.into(),
            base_state: args.base_state.into(),
            compensation_matrix: args.compensation_matrix,
            unmixing_reference: args.unmixing_reference,
            allow_subset: args.allow_subset,
            settings: WriteSettings {
                batch_rows: args.batch_size.and_then(NonZeroUsize::new),
                row_group_rows: NonZeroUsize::new(args.row_group_size).unwrap_or(NonZeroUsize::MIN),
                codec,
                max_file_bytes: args.max_file_bytes.and_then(NonZeroU64::new),
                channels,
            },
            overwrite: args.overwrite,
        })
    }
}

/// The label export's configuration, with the Gating-ML strategy and the
/// `FlowSOM` model parsed by their own crates.
fn labels_config(args: ExportLabelsArgs) -> Result<ExportLabelsConfig, Box<dyn std::error::Error>> {
    let kind = LabelKind::from(args.kind);
    if args.gating_ml.is_some() && kind != LabelKind::Gate {
        return Err(invalid_input("`--gating-ml` applies to `--kind gate` only").into());
    }
    if args.flowsom_model.is_some() && kind != LabelKind::Cluster {
        return Err(invalid_input("`--flowsom-model` applies to `--kind cluster` only").into());
    }
    if args.no_probabilities && kind != LabelKind::Classifier {
        return Err(
            invalid_input("`--no-probabilities` applies to `--kind classifier` only").into(),
        );
    }
    let gating = match args.gating_ml {
        Some(path) => {
            let strategy = cytiq_gating::parse_gatingml(&cytiq_gating::read_gatingml(&path)?)?;
            let gates = strategy
                .gates()
                .iter()
                .map(|gate| (gate.id.clone(), gate.parent_id.clone()))
                .collect();
            Some(GatingDefinition { path, gates })
        }
        None => None,
    };
    let cluster_model = match args.flowsom_model {
        Some(path) => {
            let model = cytiq_flowsom::FlowSomModel::load(&path)?;
            let node_metaclusters: Vec<u64> =
                model.node_metaclusters.iter().map(|m| *m as u64).collect();
            Some(ClusterModel {
                path,
                nodes: node_metaclusters.len() as u64,
                metaclusters: node_metaclusters.iter().copied().max().unwrap_or(0),
                node_metaclusters,
            })
        }
        None => None,
    };
    Ok(ExportLabelsConfig {
        kind,
        input: args.input,
        producer_provenance: args.producer_provenance,
        events: args.events,
        qc_masks: args.qc_masks,
        gating,
        cluster_model,
        probabilities: !args.no_probabilities,
        output: args.output,
        overwrite: args.overwrite,
        settings: LabelSettings::default(),
    })
}

fn labels(
    args: ExportLabelsArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let config = labels_config(args)?;
    info!(
        "starting labels export: kind={}, input={}, output={}",
        config.kind.as_str(),
        config.input.display(),
        config.output.display()
    );
    recorder.snapshot_roles(&LabelsRunner::input_files(&config))?;
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    );
    let prepared = LabelsRunner::prepare(&config)?;
    let outcome = publish(
        recorder,
        prepared.publication,
        std::slice::from_ref(&config.output),
        &config.record_path(),
        OutcomeReason::Success,
    )?;
    let written = prepared.summary;
    let states = written
        .summary
        .states
        .iter()
        .map(|(state, count)| format!("{}={count}", state.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    info!(
        "exported {} {} label row(s) of acquisition {} (annotation {}{}): {states}; wrote {}",
        written.rows,
        config.kind.as_str(),
        written.summary.descriptor.acquisition_id,
        written.annotation_id,
        written
            .definition_id
            .as_ref()
            .map_or_else(String::new, |id| format!(", definition {id}")),
        config.output.display()
    );
    Ok(outcome)
}

pub(crate) fn run(
    command: ExportCommand,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let args = match command {
        ExportCommand::Events(args) => args,
        ExportCommand::Labels(args) => return labels(args, recorder),
    };
    let config = ExportConfig::try_from(args)?;
    info!(
        "starting events export: input={}, output={}, layer={}",
        config.input.display(),
        config.output.display(),
        config.layer.as_str()
    );
    recorder.snapshot_roles(&ExportRunner::input_files(&config))?;
    if let Some(basis) = ExportRunner::layer_basis(&config) {
        recorder.set_constants(&serde_json::json!({ "layer_basis": basis }));
    }
    let destination = if config.splits() {
        cytiq_provenance::reuse::Destination::Dir(config.output.clone())
    } else {
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    };
    let recorder = crate::provenance::begin!(recorder, destination);
    let prepared = ExportRunner::prepare(&config)?;
    let summary = prepared.summary;
    let outcome = publish(
        recorder,
        prepared.publication,
        &ExportRunner::outputs(&config, &summary),
        &config.record_path(),
        OutcomeReason::Success,
    )?;
    info!(
        "exported {} event(s) of acquisition {} as a {} layer ({} of {} channel(s), layer {}) \
         into {} table(s); wrote {}",
        summary.rows,
        summary.descriptor.acquisition_id,
        summary.descriptor.state.as_str(),
        summary.channels.len(),
        summary.descriptor.channels.len(),
        summary.layer_id,
        summary.parts.as_ref().map_or(1, |parts| parts.parts.len()),
        config.output.display()
    );
    Ok(outcome)
}
