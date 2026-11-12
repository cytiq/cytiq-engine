use std::io::{self};
use std::path::PathBuf;

use clap::Args;
use cytiq_compensate::MatrixKind as CompensateMatrixKind;
use cytiq_provenance::record_path_for_dir;
use cytiq_spillqc::{
    BatchSettings, CompensaidParameters, DEFAULT_BATCH_MIN_FILES, DEFAULT_BATCH_MIN_FRACTION,
    DEFAULT_MIN_EVENTS, DEFAULT_SEGMENTS, PreprocessOptions, SpillQcConfig, SpillQcRunner,
};
use log::{info, warn};
use serde::Serialize;

use crate::cli::CliMatrixKind;
use crate::parsers::{parse_channels, parse_fraction, parse_positive_usize, parse_threads};

#[derive(Debug, Serialize, Args)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct SpillqcArgs {
    /// Raw .fcs file, glob pattern, or directory. Can be passed multiple times.
    #[arg(short, long, required = true)]
    input: Vec<PathBuf>,
    /// Output directory: `summary.json`, `batch.csv`, `report.html` and one
    /// directory of SSI tables per input.
    #[arg(short, long)]
    output: PathBuf,
    /// Square matrix CSV/TSV as written by `cytiq autospill`, assessed
    /// instead of each input's `$SPILLOVER` keyword.
    #[arg(long, requires = "matrix_kind")]
    matrix: Option<PathBuf>,
    /// What `--matrix` holds.
    #[arg(long, value_enum, requires = "matrix")]
    matrix_kind: Option<CliMatrixKind>,
    /// The inputs are already compensated and transformed (`CompensAID`'s own
    /// input); skip margins, compensation, logicle and `PeacoQC`.
    #[arg(long, conflicts_with_all = ["matrix", "no_margins", "no_peacoqc"])]
    preprocessed: bool,
    /// Keep detector-margin events.
    #[arg(long)]
    no_margins: bool,
    /// Skip `PeacoQC`.
    #[arg(long)]
    no_peacoqc: bool,
    /// Comma-separated channels to assess; defaults to the matrix channels
    /// with a `$PnS` marker name.
    #[arg(long)]
    channels: Option<String>,
    /// Channel to assess; can be repeated.
    #[arg(long = "channel")]
    channel: Vec<String>,
    /// Segments of the positive population.
    #[arg(long, default_value_t = DEFAULT_SEGMENTS, value_parser = parse_positive_usize)]
    segments: usize,
    /// Minimum events per population and per segment.
    #[arg(long, default_value_t = DEFAULT_MIN_EVENTS, value_parser = parse_positive_usize)]
    min_events: usize,
    /// Files sharing a matrix that must assess a pair before it can recur.
    #[arg(long, default_value_t = DEFAULT_BATCH_MIN_FILES, value_parser = parse_positive_usize)]
    batch_min_files: usize,
    /// Share of those files that must flag a pair in one direction.
    #[arg(long, default_value_t = DEFAULT_BATCH_MIN_FRACTION, value_parser = parse_fraction)]
    batch_min_fraction: f64,
    /// Exit with 3 when any pair is flagged (after writing every output).
    #[arg(long)]
    fail_on_flag: bool,
    /// Worker thread limit; defaults to all available logical CPUs.
    #[arg(long, value_parser = parse_threads)]
    pub(crate) threads: Option<usize>,
    /// Replace the outputs of an earlier run in the output directory.
    #[arg(long)]
    overwrite: bool,
}

impl SpillqcArgs {
    fn config(&self) -> Result<SpillQcConfig, String> {
        let channels = if self.channels.is_none() && self.channel.is_empty() {
            None
        } else {
            Some(parse_channels(self.channels.as_deref(), &self.channel)?)
        };
        Ok(SpillQcConfig {
            inputs: self.input.clone(),
            output: self.output.clone(),
            matrix: self
                .matrix
                .clone()
                .zip(self.matrix_kind.map(CompensateMatrixKind::from)),
            preprocess: PreprocessOptions {
                preprocessed: self.preprocessed,
                remove_margins: !self.no_margins,
                peacoqc: !self.no_peacoqc,
                channels,
            },
            parameters: CompensaidParameters {
                segments: self.segments,
                min_events: self.min_events,
            },
            batch: BatchSettings {
                min_files: self.batch_min_files,
                min_fraction: self.batch_min_fraction,
            },
            overwrite: self.overwrite,
        })
    }
}

pub(crate) fn run(
    args: &SpillqcArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    info!(
        "starting spillqc: inputs={}, output={}",
        args.input.len(),
        args.output.display()
    );
    let config = args
        .config()
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    recorder.snapshot_inputs("input", &SpillQcRunner::input_files(&config)?)?;
    if let Some((matrix, _)) = &config.matrix {
        recorder.snapshot_inputs("matrix", std::slice::from_ref(matrix))?;
    }
    recorder.set_algorithms(cytiq_spillqc::UPSTREAM);
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::Dir(config.output.clone())
    );
    let result = SpillQcRunner::run(&config)?;
    let counts = &result.summary.counts;
    for file in &result.summary.files {
        for flag in &file.flags {
            warn!(
                "{}: {} -> {}: SSI {} ({:?})",
                file.name,
                flag.primary_marker,
                flag.secondary_marker,
                flag.ssi
                    .map_or_else(|| "NA".to_owned(), |ssi| ssi.to_string()),
                flag.status
            );
        }
    }
    info!(
        "spillqc: {} of {} file(s) assessed, {} skipped; {} flagged pair(s), {} recurrent; wrote {}",
        counts.assessed,
        counts.files,
        counts.skipped,
        counts.flagged_pairs,
        counts.recurrent_pairs,
        config.output.display()
    );
    let failure = args
        .fail_on_flag
        .then(|| counts.ensure_unflagged(&config.output).err())
        .flatten();
    let reason = if failure.is_some() {
        cytiq_provenance::outcome::OutcomeReason::ChecksFailed
    } else {
        cytiq_provenance::outcome::OutcomeReason::Success
    };
    let mut outcome = crate::provenance::record_with_reason(
        recorder,
        &result.outputs,
        &record_path_for_dir(&config.output),
        reason,
    )?;
    if let (Some(outcome), Some(failure)) = (&mut outcome, failure) {
        outcome.message = failure.to_string();
    }
    Ok(outcome)
}
