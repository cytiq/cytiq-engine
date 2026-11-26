use std::io;
use std::path::PathBuf;

use clap::Args;
use cytiq_plot::{
    AxisOptions, ClassSource, DEFAULT_BINS, DEFAULT_CONTOUR_STEP, DEFAULT_HEIGHT,
    DEFAULT_MAX_POINTS, DEFAULT_WIDTH, OutputFormat, PlotConfig, PlotJob, PlotRequest, PlotRunner,
    load_batch, plot_name_for,
};
use cytiq_provenance::outcome::OutcomeReason;
use cytiq_provenance::{record_path_for_dir, record_path_for_file};
use log::info;
use serde::Serialize;

use crate::cli::{CliPalette, CliPlotFormat, CliPlotKind, CliPlotScale, CliRemovedLayer};
use crate::parsers::{
    parse_bins, parse_contour_step, parse_max_points, parse_plot_size, parse_range,
};
use crate::provenance::publish;

#[derive(Debug, Serialize, Args)]
pub(crate) struct PlotArgs {
    /// Input FCS file.
    #[arg(long)]
    input: PathBuf,
    /// Output file; with `--config`, the directory that receives
    /// `<name>.svg` / `<name>.plotly.json` for every plot.
    #[arg(long)]
    output: PathBuf,
    /// Event mask Parquet (`masks.parquet`); repeat to combine several masks
    /// with AND. Plots show the kept events with the removed ones overlaid.
    #[arg(long = "mask")]
    masks: Vec<PathBuf>,
    /// Gating-ML 2.0 file holding the gates of `--gate` and `--population`.
    #[arg(long)]
    gating_ml: Option<PathBuf>,
    /// Class labels to colour events by: `cytiq classify` predictions
    /// (`.parquet`) or a `cytiq flowsom` CSV.
    #[arg(long)]
    color_by: Option<PathBuf>,
    /// Column of a `FlowSOM` CSV holding the class [default: `flowsom_metacluster`].
    #[arg(long, requires = "color_by")]
    class_column: Option<String>,
    /// YAML or JSON file describing several plots of the input; `--output`
    /// is then a directory.
    #[arg(
        long,
        conflicts_with_all = [
            "kind", "x", "y", "gates", "population", "x_scale", "y_scale", "x_range",
            "y_range", "bins", "max_points", "palette", "removed", "contour_step", "width",
            "height", "title",
        ]
    )]
    config: Option<PathBuf>,
    /// Output format; with `--config`, repeat it to write several formats.
    #[arg(long = "format", value_enum, default_values_t = [CliPlotFormat::Svg])]
    formats: Vec<CliPlotFormat>,
    /// Plot type.
    #[arg(long = "type", value_enum, default_value_t = CliPlotKind::Density)]
    kind: CliPlotKind,
    /// Channel on the x axis (`$PnN`, `$PnS` or a gate dimension); defaults to
    /// the first gate's first dimension.
    #[arg(long)]
    x: Option<String>,
    /// Channel on the y axis; defaults to the first gate's other dimension.
    #[arg(long)]
    y: Option<String>,
    /// Gate to draw with its percentage of parent; repeatable.
    #[arg(long = "gate", requires = "gating_ml")]
    gates: Vec<String>,
    /// Plot only the events of this gate.
    #[arg(long, requires = "gating_ml")]
    population: Option<String>,
    /// x axis scale.
    #[arg(long, value_enum, default_value_t = CliPlotScale::Auto)]
    x_scale: CliPlotScale,
    /// y axis scale.
    #[arg(long, value_enum, default_value_t = CliPlotScale::Auto)]
    y_scale: CliPlotScale,
    /// x axis range in data units, as `LO,HI`.
    #[arg(long, value_parser = parse_range, allow_hyphen_values = true)]
    x_range: Option<(f64, f64)>,
    /// y axis range in data units, as `LO,HI`.
    #[arg(long, value_parser = parse_range, allow_hyphen_values = true)]
    y_range: Option<(f64, f64)>,
    /// Cells per axis of the density and contour grid.
    #[arg(long, value_parser = parse_bins, default_value_t = DEFAULT_BINS)]
    bins: usize,
    /// Most events drawn as dots per layer; more are sampled evenly.
    #[arg(long, value_parser = parse_max_points, default_value_t = DEFAULT_MAX_POINTS)]
    max_points: usize,
    /// Density colours.
    #[arg(long, value_enum, default_value_t = CliPalette::Pseudocolor)]
    palette: CliPalette,
    /// Whether events removed by the mask are drawn over the plot.
    #[arg(long, value_enum, default_value_t = CliRemovedLayer::Overlay)]
    removed: CliRemovedLayer,
    /// Event fraction between two equal-probability contours.
    #[arg(long, value_parser = parse_contour_step, default_value_t = DEFAULT_CONTOUR_STEP)]
    contour_step: f64,
    /// Image width in pixels.
    #[arg(long, value_parser = parse_plot_size, default_value_t = DEFAULT_WIDTH)]
    width: u32,
    /// Image height in pixels.
    #[arg(long, value_parser = parse_plot_size, default_value_t = DEFAULT_HEIGHT)]
    height: u32,
    /// Plot title [default: `<sample>: <y> vs <x>`].
    #[arg(long)]
    title: Option<String>,
    /// Replace existing output files.
    #[arg(long)]
    overwrite: bool,
}

pub(crate) fn run(
    args: PlotArgs,
    mut recorder: crate::provenance::Invocation,
) -> crate::provenance::CommandResult {
    info!(
        "starting plot: input={}, output={}",
        args.input.display(),
        args.output.display()
    );
    let formats = args
        .formats
        .iter()
        .map(|&format| OutputFormat::from(format))
        .collect::<Vec<_>>();
    let jobs = match &args.config {
        Some(config) => load_batch(config, &args.output, &formats)?,
        None => vec![single_job(&args, &formats)?],
    };
    let PlotArgs {
        input,
        output,
        masks,
        gating_ml,
        color_by,
        class_column,
        config: batch,
        overwrite,
        ..
    } = args;
    let config = PlotConfig {
        input,
        masks,
        gating_ml,
        color_by: color_by.map(|path| ClassSource {
            path,
            column: class_column,
        }),
        jobs,
        overwrite,
    };
    recorder.snapshot_inputs("input", std::slice::from_ref(&config.input))?;
    recorder.snapshot_inputs("mask", &config.masks)?;
    recorder.snapshot_inputs("gating_ml", config.gating_ml.as_slice())?;
    if let Some(source) = &config.color_by {
        recorder.snapshot_inputs("classes", std::slice::from_ref(&source.path))?;
    }
    recorder.snapshot_inputs("config", batch.as_slice())?;
    let destination = if batch.is_some() {
        cytiq_provenance::reuse::Destination::Dir(output.clone())
    } else {
        cytiq_provenance::reuse::Destination::File(output.clone())
    };
    let recorder = crate::provenance::begin!(recorder, destination);
    let prepared = PlotRunner::prepare(&config)?;
    let summary = prepared.summary;
    let record = if batch.is_some() {
        record_path_for_dir(&output)
    } else {
        record_path_for_file(&output)
    };
    let outcome = publish(
        recorder,
        prepared.publication,
        &summary.outputs(),
        &record,
        OutcomeReason::Success,
    )?;
    for plot in &summary.plots {
        info!(
            "plot {}: {} of {} vs {}: {} event(s) shown, {} removed by mask, {} drawn as dots; wrote {}",
            plot.name,
            plot.kind.as_str(),
            plot.y,
            plot.x,
            plot.stats.shown,
            plot.stats.removed,
            plot.stats.drawn,
            plot.outputs
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(outcome)
}

fn single_job(args: &PlotArgs, formats: &[OutputFormat]) -> Result<PlotJob, io::Error> {
    let [format] = formats else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a single plot writes one `--format`; use `--config` to write several",
        ));
    };
    let mut request = PlotRequest::new(plot_name_for(&args.output));
    request.kind = args.kind.into();
    request.x = AxisOptions {
        channel: args.x.clone(),
        scale: args.x_scale.into(),
        range: args.x_range,
    };
    request.y = AxisOptions {
        channel: args.y.clone(),
        scale: args.y_scale.into(),
        range: args.y_range,
    };
    request.gates.clone_from(&args.gates);
    request.population.clone_from(&args.population);
    request.color_by = args.color_by.is_some();
    request.bins = args.bins;
    request.max_points = args.max_points;
    request.palette = args.palette.into();
    request.removed = args.removed.into();
    request.contour_step = args.contour_step;
    request.width = args.width;
    request.height = args.height;
    request.title.clone_from(&args.title);
    Ok(PlotJob {
        request,
        outputs: vec![(*format, args.output.clone())],
    })
}
