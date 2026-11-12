use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use cytiq_flowsom::{
    FLOWSOM_METACLUSTERS, FLOWSOM_RANDOM_SEED, FLOWSOM_XDIM, FLOWSOM_YDIM, FlowSomConfig,
    FlowSomConstants, FlowSomRunner,
};
use cytiq_provenance::record_path_for_file;
use log::info;
use serde::Serialize;

use crate::cli::CliMetaclustering;
use crate::parsers::{parse_channels, parse_positive_usize, parse_threads};
use crate::provenance::record_provenance;

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliCohortSampling {
    Pooled,
    PerFile,
}

impl CliCohortSampling {
    /// The default; not recorded, so pooled runs keep their earlier config.
    #[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
    const fn is_pooled(&self) -> bool {
        matches!(self, Self::Pooled)
    }
}

impl From<CliCohortSampling> for cytiq_flowsom::CohortSampling {
    fn from(value: CliCohortSampling) -> Self {
        match value {
            CliCohortSampling::Pooled => Self::Pooled,
            CliCohortSampling::PerFile => Self::PerFile,
        }
    }
}

/// SOM geometry and training flags shared by `flowsom` and `flowsom train`;
/// flattened, so the flags and recorded config keys are those of each command.
#[derive(Debug, Serialize, Args)]
pub(crate) struct SomArgs {
    /// SOM grid width.
    #[arg(long, default_value_t = FLOWSOM_XDIM, value_parser = parse_positive_usize)]
    xdim: usize,
    /// SOM grid height.
    #[arg(long, default_value_t = FLOWSOM_YDIM, value_parser = parse_positive_usize)]
    ydim: usize,
    /// Number of metaclusters (at most xdim × ydim).
    #[arg(long, default_value_t = FLOWSOM_METACLUSTERS, value_parser = parse_positive_usize)]
    metaclusters: usize,
    /// Training seed.
    #[arg(long, default_value_t = FLOWSOM_RANDOM_SEED)]
    seed: u64,
    /// Cluster channel values as given instead of z-scoring them.
    #[arg(long)]
    no_scale: bool,
}

impl SomArgs {
    fn configure(self, config: &mut FlowSomConfig) {
        config.xdim = self.xdim;
        config.ydim = self.ydim;
        config.metaclusters = self.metaclusters;
        config.seed = self.seed;
        config.scale = !self.no_scale;
    }
}

#[derive(Debug, Serialize, Subcommand)]
pub(crate) enum FlowSomCommand {
    /// Save reusable SOM geometry, scaling and metacluster labels.
    Train {
        #[arg(long, required = true)]
        input: Vec<PathBuf>,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        channels: String,
        #[command(flatten)]
        #[serde(flatten)]
        som: SomArgs,
        #[arg(long, value_enum, default_value_t = CliMetaclustering::Average)]
        metaclustering: CliMetaclustering,
        /// Events to train on: a uniform sample of the pooled inputs, or with
        /// `--cohort-sampling per-file` the total split evenly across inputs.
        #[arg(long, value_parser = parse_positive_usize)]
        training_events: Option<usize>,
        /// `pooled` concatenates the inputs, so larger files weigh more;
        /// `per-file` draws `ceil(--training-events / inputs)` events from each
        /// input (all of a smaller one) and scales and trains on that balanced
        /// set, as `FlowSOM`'s `AggregateFlowFrames`.
        #[arg(
            long,
            value_enum,
            default_value_t = CliCohortSampling::Pooled,
            requires_if("per-file", "training_events")
        )]
        #[serde(skip_serializing_if = "CliCohortSampling::is_pooled")]
        cohort_sampling: CliCohortSampling,
        /// Event mask Parquet (`masks.parquet`); repeat to combine masks. With
        /// several inputs each mask applies to the input of its acquisition,
        /// and every input needs at least one.
        #[arg(long = "mask")]
        masks: Vec<PathBuf>,
        #[arg(long, value_parser = parse_threads)]
        threads: Option<usize>,
        #[arg(long)]
        overwrite: bool,
    },
    /// Assign events to a saved SOM without retraining or rescaling the sample.
    Apply {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        model: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long = "mask")]
        masks: Vec<PathBuf>,
        #[arg(long, value_parser = parse_threads)]
        threads: Option<usize>,
        #[arg(long)]
        overwrite: bool,
    },
}

#[derive(Debug, Serialize, Args)]
pub(crate) struct FlowsomArgs {
    #[command(subcommand)]
    pub(crate) command: Option<FlowSomCommand>,
    #[arg(long, required = true)]
    input: Option<PathBuf>,
    #[arg(long, required = true)]
    output: Option<PathBuf>,
    /// Comma-separated channel names used for clustering.
    #[arg(long)]
    channels: Option<String>,
    /// Channel name used for clustering; can be repeated.
    #[arg(long = "channel")]
    channel: Vec<String>,
    #[arg(long)]
    debug_dir: Option<PathBuf>,
    /// Worker thread limit; defaults to all available logical CPUs.
    #[arg(long, value_parser = parse_threads)]
    pub(crate) threads: Option<usize>,
    #[command(flatten)]
    #[serde(flatten)]
    som: SomArgs,
    /// Train the SOM on this many events (a deterministic sample), then
    /// assign every event; default trains on all events.
    #[arg(long, value_parser = parse_positive_usize)]
    training_events: Option<usize>,
    /// Event mask Parquet (`masks.parquet`); only kept events are
    /// clustered. Repeat to combine several masks with AND.
    #[arg(long = "mask")]
    masks: Vec<PathBuf>,
    /// Replace an existing assignment CSV.
    #[arg(long)]
    overwrite: bool,
}

pub(crate) fn run(
    args: FlowsomArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let FlowsomArgs {
        command,
        input,
        output,
        channels,
        channel,
        debug_dir,
        threads: _,
        som,
        training_events,
        masks,
        overwrite,
    } = args;
    if let Some(command) = command {
        return run_flowsom_model(command, recorder);
    }
    let input = input.ok_or("flowsom requires --input")?;
    let output = output.ok_or("flowsom requires --output")?;
    info!(
        "starting flowsom: input={}, output={}",
        input.display(),
        output.display()
    );
    let mut config = FlowSomConfig::new(parse_channels(channels.as_deref(), &channel)?);
    som.configure(&mut config);
    config.debug_dir = debug_dir;
    config.training_events = training_events;
    config.masks = masks;
    recorder.snapshot_inputs("input", std::slice::from_ref(&input))?;
    recorder.snapshot_inputs("mask", &config.masks)?;
    recorder.set_constants(&FlowSomConstants::default());
    recorder.set_algorithms(cytiq_flowsom::UPSTREAM);
    let recorder = crate::provenance::begin!(
        recorder,
        cytiq_provenance::reuse::Destination::File(output.clone())
    );
    let result = FlowSomRunner::run(&input, &output, &config, overwrite)?;
    info!(
        "flowsom assigned {} events across {} SOM nodes and {} metaclusters",
        result.summary.metrics.events,
        result.summary.metrics.som_nodes,
        result.summary.metrics.metaclusters
    );
    if let Some(debug_dir) = config.debug_dir.as_ref() {
        info!("wrote flowsom debug artifacts to {}", debug_dir.display());
    }
    info!("flowsom completed successfully");
    record_provenance(
        recorder,
        std::slice::from_ref(&output),
        &record_path_for_file(&output),
    )
}

fn run_flowsom_model(
    command: FlowSomCommand,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    recorder.set_constants(&FlowSomConstants::default());
    recorder.set_algorithms(cytiq_flowsom::UPSTREAM);
    match command {
        FlowSomCommand::Train {
            input,
            output,
            channels,
            som,
            metaclustering,
            training_events,
            cohort_sampling,
            masks,
            threads: _,
            overwrite,
        } => {
            recorder.snapshot_inputs("input", &input)?;
            recorder.snapshot_inputs("mask", &masks)?;
            let mut config = FlowSomConfig::new(crate::parsers::parse_channel_list(&channels)?);
            som.configure(&mut config);
            config.training_events = training_events;
            config.cohort_sampling = cohort_sampling.into();
            config.masks = masks;
            let recorder = crate::provenance::begin!(
                recorder,
                cytiq_provenance::reuse::Destination::File(output.clone())
            );
            cytiq_flowsom::FlowSomModel::train_to_file(
                &input,
                &output,
                &config,
                metaclustering.into(),
                overwrite,
            )?;
            record_provenance(
                recorder,
                std::slice::from_ref(&output),
                &record_path_for_file(&output),
            )
        }
        FlowSomCommand::Apply {
            input,
            model,
            output,
            masks,
            threads: _,
            overwrite,
        } => {
            recorder.snapshot_inputs("input", std::slice::from_ref(&input))?;
            recorder.snapshot_inputs("model", std::slice::from_ref(&model))?;
            recorder.snapshot_inputs("mask", &masks)?;
            let recorder = crate::provenance::begin!(
                recorder,
                cytiq_provenance::reuse::Destination::File(output.clone())
            );
            let events = cytiq_flowsom::FlowSomModel::apply_model_file(
                &model, &input, &output, &masks, overwrite,
            )?;
            info!("flowsom assigned {events} events with saved model");
            record_provenance(
                recorder,
                std::slice::from_ref(&output),
                &record_path_for_file(&output),
            )
        }
    }
}
