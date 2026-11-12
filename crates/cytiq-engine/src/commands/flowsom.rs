use std::io::{self};
use std::path::PathBuf;

use clap::{Args, Subcommand};
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
        #[arg(long, default_value_t = FLOWSOM_XDIM, value_parser = parse_positive_usize)]
        xdim: usize,
        #[arg(long, default_value_t = FLOWSOM_YDIM, value_parser = parse_positive_usize)]
        ydim: usize,
        #[arg(long, default_value_t = FLOWSOM_METACLUSTERS, value_parser = parse_positive_usize)]
        metaclusters: usize,
        #[arg(long, default_value_t = FLOWSOM_RANDOM_SEED)]
        seed: u64,
        #[arg(long)]
        no_scale: bool,
        #[arg(long, value_enum, default_value_t = CliMetaclustering::Average)]
        metaclustering: CliMetaclustering,
        #[arg(long, value_parser = parse_positive_usize)]
        training_events: Option<usize>,
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
        xdim,
        ydim,
        metaclusters,
        seed,
        no_scale,
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
    let channels = parse_channels(channels.as_deref(), &channel)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let config = FlowSomConfig {
        channels,
        debug_dir,
        xdim,
        ydim,
        metaclusters,
        seed,
        scale: !no_scale,
        training_events,
        masks,
    };
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
            xdim,
            ydim,
            metaclusters,
            seed,
            no_scale,
            metaclustering,
            training_events,
            masks,
            threads: _,
            overwrite,
        } => {
            recorder.snapshot_inputs("input", &input)?;
            recorder.snapshot_inputs("mask", &masks)?;
            let mut config = FlowSomConfig::new(
                cytiq_fcs::parse_channel_list(&channels)
                    .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?,
            );
            config.xdim = xdim;
            config.ydim = ydim;
            config.metaclusters = metaclusters;
            config.seed = seed;
            config.scale = !no_scale;
            config.training_events = training_events;
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
