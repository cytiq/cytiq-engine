use std::io::{self};
use std::path::PathBuf;

use clap::Args;
use cytiq_provenance::{record_path_for_dir, record_path_for_file};
use cytiq_transform::{
    BiexponentialParameters, LogicleParameters, TransformConfig, TransformParameters,
    TransformRunResult, TransformRunner,
};
use log::info;
use serde::Serialize;

use crate::cli::{CliTransformFormat, CliTransformMethod};
use crate::parsers::{
    parse_finite_f64, parse_non_negative_f64, parse_optional_transform_channels,
    parse_positive_f64, parse_threads,
};
use crate::provenance::record_provenance;

// Each bool is an independent command-line flag.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Serialize, Args)]
pub(crate) struct TransformArgs {
    /// Input .fcs file, glob pattern, or directory. Can be passed multiple times.
    #[arg(short, long, required = true)]
    input: Vec<PathBuf>,
    /// Output file path for one input, or output directory for multiple inputs.
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long, value_enum, default_value_t = CliTransformMethod::Arcsinh)]
    method: CliTransformMethod,
    /// Cofactor for arcsinh transformation.
    #[arg(long, default_value_t = 150.0, value_parser = parse_positive_f64)]
    cofactor: f64,
    /// Logicle/hyperlog T parameter, top of scale.
    #[arg(long, default_value_t = 262_144.0, value_parser = parse_positive_f64)]
    logicle_t: f64,
    /// Logicle/hyperlog M parameter, number of decades.
    #[arg(long, default_value_t = 4.5, value_parser = parse_positive_f64)]
    logicle_m: f64,
    /// Logicle/hyperlog W parameter, width of linear region.
    #[arg(long, default_value_t = 0.5, value_parser = parse_non_negative_f64)]
    logicle_w: f64,
    /// Logicle/hyperlog A parameter, additional negative decades (-W <= A <= M - 2W).
    #[arg(long, default_value_t = 0.0, value_parser = parse_finite_f64, allow_negative_numbers = true)]
    logicle_a: f64,
    /// Worker thread limit; defaults to all available logical CPUs.
    #[arg(long, value_parser = parse_threads)]
    pub(crate) threads: Option<usize>,
    /// Comma-separated channel names to transform.
    #[arg(long)]
    channels: Option<String>,
    /// Comma-separated channel names to leave untransformed.
    #[arg(long)]
    exclude_channels: Option<String>,
    #[arg(long, value_enum, default_value_t = CliTransformFormat::Fcs)]
    format: CliTransformFormat,
    /// Output filename suffix when writing to a directory.
    #[arg(long, default_value = ".transformed")]
    suffix: String,
    /// Allow replacing existing output files.
    #[arg(long)]
    overwrite: bool,
    /// Print resolved inputs, channels, and outputs without writing files.
    #[arg(long)]
    dry_run: bool,
    /// Optional path to write transformation metadata as JSON.
    #[arg(long)]
    metadata_json: Option<PathBuf>,
    /// Optional directory for transformation metrics and Plotly report.
    #[arg(long)]
    debug_dir: Option<PathBuf>,
    /// Transform inputs that `cytiq transform` already transformed.
    #[arg(long)]
    allow_retransform: bool,
    /// Transform channels whose `$SPILLOVER` matrix was not applied yet
    /// (normally `cytiq compensate` runs first).
    #[arg(long)]
    allow_uncompensated: bool,
}

pub(crate) fn run(
    args: TransformArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    let TransformArgs {
        input,
        output,
        method,
        cofactor,
        logicle_t,
        logicle_m,
        logicle_w,
        logicle_a,
        threads: _,
        channels,
        exclude_channels,
        format,
        suffix,
        overwrite,
        dry_run,
        metadata_json,
        debug_dir,
        allow_retransform,
        allow_uncompensated,
    } = args;
    info!(
        "starting transform: inputs={}, output={}, method={:?}",
        input.len(),
        output.display(),
        method
    );
    let channels = parse_optional_transform_channels(channels.as_deref())
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let exclude_channels = parse_optional_transform_channels(exclude_channels.as_deref())
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let config = TransformConfig {
        inputs: input,
        output,
        method: method.into(),
        cofactor,
        logicle: LogicleParameters {
            t: logicle_t,
            m: logicle_m,
            w: logicle_w,
            a: logicle_a,
        },
        biexponential: BiexponentialParameters::default(),
        channels,
        exclude_channels,
        format: format.into(),
        suffix,
        overwrite,
        dry_run,
        metadata_json,
        debug_dir,
        allow_retransform,
        allow_uncompensated,
    };
    if !config.dry_run {
        recorder.snapshot_inputs("input", &TransformRunner::input_files(&config)?)?;
        recorder.set_algorithms(cytiq_transform::UPSTREAM);
    }
    if config.dry_run {
        print_transform_dry_run(&TransformRunner::run(&config)?);
        return Ok(None);
    }
    let directory = config.output.is_dir() || TransformRunner::input_files(&config)?.len() > 1;
    let destination = if directory {
        cytiq_provenance::reuse::Destination::Dir(config.output.clone())
    } else {
        cytiq_provenance::reuse::Destination::File(config.output.clone())
    };
    let recorder = crate::provenance::begin!(recorder, destination);
    let result = TransformRunner::run(&config)?;
    if result.dry_run {
        print_transform_dry_run(&result);
    }
    if let Some(debug_dir) = config.debug_dir.as_ref() {
        info!("wrote transform debug artifacts to {}", debug_dir.display());
    }
    info!("transform completed for {} file(s)", result.files.len());
    if result.dry_run {
        return Ok(None);
    }
    let mut outputs = result
        .files
        .iter()
        .map(|file| file.output_path.clone())
        .collect::<Vec<_>>();
    outputs.extend(result.metadata_path.clone());
    let record_path = if config.output.is_dir() {
        record_path_for_dir(&config.output)
    } else {
        record_path_for_file(&config.output)
    };
    record_provenance(recorder, &outputs, &record_path)
}

fn print_transform_dry_run(result: &TransformRunResult) {
    println!("cytiq transform dry run");
    for file in &result.files {
        println!(
            "{} -> {}",
            file.input_path.display(),
            file.output_path.display()
        );
        println!("  method: {}", file.method.as_str());
        let parameters = format_transform_parameters(&file.parameters);
        if !parameters.is_empty() {
            println!("  parameters: {parameters}");
        }
        println!("  events: {}", file.events);
        println!("  channels: {}", file.channels.join(","));
        if !file.excluded_channels.is_empty() {
            println!("  excluded: {}", file.excluded_channels.join(","));
        }
    }
    if let Some(path) = result.metadata_path.as_ref() {
        println!("metadata: {}", path.display());
    }
}

fn format_transform_parameters(parameters: &TransformParameters) -> String {
    if let Some(cofactor) = parameters.cofactor {
        return format!("cofactor={cofactor}");
    }
    if let Some(logicle) = parameters.logicle {
        return format!(
            "logicle_t={}, logicle_m={}, logicle_w={}, logicle_a={}",
            logicle.t, logicle.m, logicle.w, logicle.a
        );
    }
    if let Some(hyperlog) = parameters.hyperlog {
        return format!(
            "logicle_t={}, logicle_m={}, logicle_w={}, logicle_a={}",
            hyperlog.t, hyperlog.m, hyperlog.w, hyperlog.a
        );
    }
    if let Some(biexponential) = parameters.biexponential {
        return format!(
            "length={}, max_range={}, pos={}, neg={}, width={}",
            biexponential.length,
            biexponential.max_range,
            biexponential.pos,
            biexponential.neg,
            biexponential.width
        );
    }
    String::new()
}
