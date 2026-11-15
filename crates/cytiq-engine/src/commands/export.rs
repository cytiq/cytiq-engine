use std::io;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use cytiq_population::events::{
    BaseState, Codec, DEFAULT_ROW_GROUP_ROWS, DEFAULT_ZSTD_LEVEL, ExportConfig, ExportRunner,
    LayerState, WriteSettings,
};
use log::info;
use serde::Serialize;

use crate::parsers::{parse_byte_size, parse_channels, parse_positive_usize};
use crate::provenance::record_provenance;

#[derive(Debug, Serialize, Subcommand)]
pub(crate) enum ExportCommand {
    /// Write one FCS file's events as a `cytiq.events.v1` Parquet table:
    /// acquisition, signal layer and raw event id in every row, then the
    /// channels in `$Pn` order, bit-exact. Every raw event is kept; no
    /// identifying FCS keyword is written. The table is read back and
    /// verified before it is published.
    Events(ExportEventsArgs),
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

pub(crate) fn run(
    command: ExportCommand,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let ExportCommand::Events(args) = command;
    let config = ExportConfig::try_from(args)?;
    info!(
        "starting events export: input={}, output={}, layer={}",
        config.input.display(),
        config.output.display(),
        config.layer.as_str()
    );
    recorder.snapshot_roles(&ExportRunner::input_files(&config))?;
    let destination = if config.splits() {
        cytiq_provenance::reuse::Destination::Dir(config.output.clone())
    } else {
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    };
    let recorder = crate::provenance::begin!(recorder, destination);
    let summary = ExportRunner::run(&config)?;
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
    record_provenance(
        recorder,
        &ExportRunner::outputs(&config, &summary),
        &config.record_path(),
    )
}
