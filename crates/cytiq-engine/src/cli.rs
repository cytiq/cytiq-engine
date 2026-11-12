use clap::{Parser, Subcommand, ValueEnum};
use cytiq_autospill::MatrixKind;
use cytiq_clean::{DebrisMode, FilterKind};
use cytiq_compensate::MatrixKind as CompensateMatrixKind;
use cytiq_cytoscan::{FeatureMethod, FlagMethod};
use cytiq_gating::OutputFormat as GateOutputFormat;
use cytiq_peacoqc::PeacoQcMethod;
use cytiq_plot::{OutputFormat, Palette, PlotKind, RemovedLayer, ScaleChoice};
use cytiq_transform::{TransformFormat, TransformMethod};
use serde::Serialize;
use std::path::PathBuf;

use crate::commands::autospectral::AutospectralArgs;
use crate::commands::autospill::AutospillArgs;
use crate::commands::autospread::AutospreadArgs;
use crate::commands::classify::ClassifyArgs;
use crate::commands::clean::CleanArgs;
use crate::commands::compensate::CompensateArgs;
use crate::commands::cytonorm::CytoNormCommand;
use crate::commands::cytoscan::CytoscanArgs;
use crate::commands::flowsom::{FlowSomCommand, FlowsomArgs};
use crate::commands::gate::GateArgs;
use crate::commands::mask::MaskCommand;
use crate::commands::meta::MetaArgs;
use crate::commands::peacoqc::PeacoqcArgs;
use crate::commands::plot::PlotArgs;
use crate::commands::preflight::PreflightArgs;
use crate::commands::qc::QcArgs;
use crate::commands::spillqc::SpillqcArgs;
use crate::commands::stats::StatsArgs;
use crate::commands::transform::TransformArgs;

#[derive(Debug, Parser)]
#[command(name = "cytiq")]
#[command(about = "Command-line tools for cytiq workflows.")]
#[command(
    after_help = "Exit codes: 0 success, 1 runtime error (retryable), 2 invalid input, \
configuration or data, 3 completed but checks failed, 4 output conflict, missing input or \
incomplete result."
)]
pub(crate) struct Cli {
    /// Also write the command's JSON outcome (exit code, reason, message) to
    /// this file; it is replaced when it exists.
    #[arg(long, global = true, value_name = "PATH", value_parser = crate::parsers::parse_outcome_path)]
    pub(crate) outcome: Option<PathBuf>,
    /// Reuse verified matching outputs, or reject stale outputs, instead of recomputing.
    #[arg(long, global = true, value_enum, default_value = "recompute")]
    pub(crate) reuse: CliReusePolicy,
    #[command(subcommand)]
    pub(crate) command: Commands,
}

/// Serialized untagged into provenance as the effective configuration:
/// every option with clap's defaults applied.
#[derive(Debug, Serialize, Subcommand)]
#[serde(untagged)]
pub(crate) enum Commands {
    /// Validate and summarize event annotations with typed measurement status.
    #[command(subcommand)]
    Population(crate::commands::population::PopulationCommand),
    /// Plan and reconcile panels belonging to a biological specimen.
    #[command(subcommand)]
    Specimen(crate::commands::specimen::SpecimenCommand),
    /// Learn and apply population-specific batch normalization after transform.
    #[command(subcommand)]
    Cytonorm(CytoNormCommand),
    /// Estimate a spillover or compensation matrix from single-color controls.
    Autospill(AutospillArgs),
    /// Estimate a spillover spreading matrix from single-color controls.
    Autospread(AutospreadArgs),
    /// Unmix spectral flow cytometry samples from single-stain controls.
    Autospectral(AutospectralArgs),
    /// Remove unstable acquisition regions using PeacoQC-style peak-based QC.
    Peacoqc(PeacoqcArgs),
    /// Flag anomalous FCS files using a CytoScan-style cohort workflow.
    Cytoscan(CytoscanArgs),
    /// Transform FCS signal intensities into analysis-ready values.
    Transform(TransformArgs),
    /// Classify FCS events using a panel-specific ONNX model bundle.
    Classify(ClassifyArgs),
    /// Apply a Gating-ML 2.0 gating strategy to an FCS file and report per-gate counts.
    Gate(GateArgs),
    /// Work with per-event masks (`masks.parquet`) written by cytiq stages.
    #[command(subcommand)]
    Mask(MaskCommand),
    /// Render 2D density, scatter or contour plots of an FCS file as vector
    /// SVG (or Plotly JSON), with removed events, gates and classes overlaid.
    Plot(PlotArgs),
    /// Check a run manifest and its FCS files before any computation; exits
    /// with 3 after writing the report when a check fails.
    Preflight(PreflightArgs),
    /// Check a sample's final event mask against its panel's `qc` policy in
    /// the run manifest; exits with 3 after writing the report when a check
    /// fails.
    Qc(QcArgs),
    /// Dump FCS metadata (file hash, parameters, all TEXT keywords) as JSON.
    Meta(MetaArgs),
    /// Apply a spillover or compensation matrix to FCS events; also writes a
    /// raw detector-margin mask next to the output.
    Compensate(CompensateArgs),
    /// Remove debris, doublets and dead cells; writes per-event masks,
    /// metrics, Gating-ML gates and an HTML report.
    Clean(CleanArgs),
    /// Check the spillover matrix applied to fully stained samples with
    /// `CompensAID`'s Secondary Stain Index; flags need expert review.
    Spillqc(SpillqcArgs),
    /// Per-channel statistics for technical QC: range, distribution, detector-limit and zero counts,
    /// histograms, and the Time channel's monotonicity and event rate.
    Stats(StatsArgs),
    /// Cluster cytometry events using a deterministic FlowSOM-style workflow.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Flowsom(FlowsomArgs),
}

impl Commands {
    /// `--threads` of the commands whose libraries run on rayon. The CLI
    /// sizes the global pool once; libraries use the caller's pool.
    /// `classify --threads` configures ONNX Runtime instead.
    pub(crate) const fn rayon_threads(&self) -> Option<usize> {
        match self {
            Self::Autospill(args) => args.threads,
            Self::Autospread(args) => args.threads,
            Self::Autospectral(args) => args.threads,
            Self::Peacoqc(args) => args.threads,
            Self::Cytoscan(args) => args.threads,
            Self::Transform(args) => args.threads,
            Self::Gate(args) => args.threads,
            Self::Clean(args) => args.threads,
            Self::Spillqc(args) => args.threads,
            Self::Flowsom(
                FlowsomArgs {
                    command:
                        Some(
                            FlowSomCommand::Train { threads, .. }
                            | FlowSomCommand::Apply { threads, .. },
                        ),
                    ..
                }
                | FlowsomArgs {
                    command: None,
                    threads,
                    ..
                },
            )
            | Self::Cytonorm(
                CytoNormCommand::Train { threads, .. } | CytoNormCommand::Apply { threads, .. },
            ) => *threads,
            Self::Population(_)
            | Self::Specimen(_)
            | Self::Classify(_)
            | Self::Mask(_)
            | Self::Plot(_)
            | Self::Preflight(_)
            | Self::Qc(_)
            | Self::Meta(_)
            | Self::Compensate(_)
            | Self::Stats(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
pub(crate) enum CliMetaclustering {
    Average,
    Consensus,
}

impl From<CliMetaclustering> for cytiq_flowsom::Metaclustering {
    fn from(value: CliMetaclustering) -> Self {
        match value {
            CliMetaclustering::Average => Self::Average,
            CliMetaclustering::Consensus => Self::Consensus,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliFilterKind {
    Debris,
    Singlets,
    Viability,
}

impl From<CliFilterKind> for FilterKind {
    fn from(value: CliFilterKind) -> Self {
        match value {
            CliFilterKind::Debris => Self::Debris,
            CliFilterKind::Singlets => Self::Singlets,
            CliFilterKind::Viability => Self::Viability,
        }
    }
}

impl From<CliMatrixKind> for CompensateMatrixKind {
    fn from(value: CliMatrixKind) -> Self {
        match value {
            CliMatrixKind::Spillover => Self::Spillover,
            CliMatrixKind::Compensation => Self::Compensation,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliDebrisMode {
    /// FSC-A threshold at the density valley above the debris peak.
    Adaptive,
    /// FSC/SSC auto-gate around the dominant population (beads, cell lines).
    Population,
    /// Thresholds from --debris-fsc-min / --debris-ssc-min.
    Fixed,
    /// No debris filter (`disabled`), for files another tool already gated.
    Off,
}

impl From<CliDebrisMode> for DebrisMode {
    fn from(value: CliDebrisMode) -> Self {
        match value {
            CliDebrisMode::Adaptive => Self::Adaptive,
            CliDebrisMode::Population => Self::Population,
            CliDebrisMode::Fixed => Self::Fixed,
            CliDebrisMode::Off => Self::Off,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliGateOutputFormat {
    Csv,
    Json,
}

impl From<CliGateOutputFormat> for GateOutputFormat {
    fn from(value: CliGateOutputFormat) -> Self {
        match value {
            CliGateOutputFormat::Csv => Self::Csv,
            CliGateOutputFormat::Json => Self::Json,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliPlotKind {
    /// Pseudocolor: events binned and coloured by log density.
    Density,
    /// Dots, sampled evenly to `--max-points`.
    Scatter,
    /// Equal-probability contours with outlier dots.
    Contour,
    /// Event counts of the `--x` channel.
    Histogram,
}

impl From<CliPlotKind> for PlotKind {
    fn from(value: CliPlotKind) -> Self {
        match value {
            CliPlotKind::Density => Self::Density,
            CliPlotKind::Scatter => Self::Scatter,
            CliPlotKind::Contour => Self::Contour,
            CliPlotKind::Histogram => Self::Histogram,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliPlotScale {
    /// Linear for scatter, time and transformed channels; logicle otherwise.
    Auto,
    Linear,
    Logicle,
    /// `asinh(x / 150)`.
    Arcsinh,
}

impl From<CliPlotScale> for ScaleChoice {
    fn from(value: CliPlotScale) -> Self {
        match value {
            CliPlotScale::Auto => Self::Auto,
            CliPlotScale::Linear => Self::Linear,
            CliPlotScale::Logicle => Self::Logicle,
            CliPlotScale::Arcsinh => Self::Arcsinh,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliPalette {
    /// FlowJo-style blue to red.
    Pseudocolor,
    Viridis,
}

impl From<CliPalette> for Palette {
    fn from(value: CliPalette) -> Self {
        match value {
            CliPalette::Pseudocolor => Self::Pseudocolor,
            CliPalette::Viridis => Self::Viridis,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliRemovedLayer {
    Overlay,
    Hide,
}

impl From<CliRemovedLayer> for RemovedLayer {
    fn from(value: CliRemovedLayer) -> Self {
        match value {
            CliRemovedLayer::Overlay => Self::Overlay,
            CliRemovedLayer::Hide => Self::Hide,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliPlotFormat {
    /// Vector SVG.
    Svg,
    /// Plotly figure JSON.
    Plotly,
}

impl From<CliPlotFormat> for OutputFormat {
    fn from(value: CliPlotFormat) -> Self {
        match value {
            CliPlotFormat::Svg => Self::Svg,
            CliPlotFormat::Plotly => Self::Plotly,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliMatrixKind {
    Compensation,
    Spillover,
}

impl From<CliMatrixKind> for MatrixKind {
    fn from(value: CliMatrixKind) -> Self {
        match value {
            CliMatrixKind::Compensation => Self::Compensation,
            CliMatrixKind::Spillover => Self::Spillover,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliPeacoQcMethod {
    All,
    Mad,
    It,
    PeaksOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliTransformMethod {
    Arcsinh,
    Biexponential,
    Hyperlog,
    Logicle,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliCytoScanFeatureMethod {
    Summary,
    Quantiles,
    Emd,
    Binning,
    Fingerprint,
}

impl From<CliCytoScanFeatureMethod> for FeatureMethod {
    fn from(value: CliCytoScanFeatureMethod) -> Self {
        match value {
            CliCytoScanFeatureMethod::Summary => Self::Summary,
            CliCytoScanFeatureMethod::Quantiles => Self::Quantiles,
            CliCytoScanFeatureMethod::Emd => Self::Emd,
            CliCytoScanFeatureMethod::Binning => Self::Binning,
            CliCytoScanFeatureMethod::Fingerprint => Self::Fingerprint,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliCytoScanFlagMethod {
    Outlier,
    Novelty,
    Both,
}

impl From<CliCytoScanFlagMethod> for FlagMethod {
    fn from(value: CliCytoScanFlagMethod) -> Self {
        match value {
            CliCytoScanFlagMethod::Outlier => Self::Outlier,
            CliCytoScanFlagMethod::Novelty => Self::Novelty,
            CliCytoScanFlagMethod::Both => Self::Both,
        }
    }
}

impl From<CliTransformMethod> for TransformMethod {
    fn from(value: CliTransformMethod) -> Self {
        match value {
            CliTransformMethod::Arcsinh => Self::Arcsinh,
            CliTransformMethod::Biexponential => Self::Biexponential,
            CliTransformMethod::Hyperlog => Self::Hyperlog,
            CliTransformMethod::Logicle => Self::Logicle,
            CliTransformMethod::None => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliTransformFormat {
    Fcs,
    Csv,
    Parquet,
}

impl From<CliTransformFormat> for TransformFormat {
    fn from(value: CliTransformFormat) -> Self {
        match value {
            CliTransformFormat::Fcs => Self::Fcs,
            CliTransformFormat::Csv => Self::Csv,
            CliTransformFormat::Parquet => Self::Parquet,
        }
    }
}

impl From<CliPeacoQcMethod> for PeacoQcMethod {
    fn from(value: CliPeacoQcMethod) -> Self {
        match value {
            CliPeacoQcMethod::All => Self::All,
            CliPeacoQcMethod::Mad => Self::Mad,
            CliPeacoQcMethod::It => Self::It,
            CliPeacoQcMethod::PeaksOnly => Self::PeaksOnly,
        }
    }
}

pub(crate) const fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Population(crate::commands::population::PopulationCommand::Validate(_)) => {
            "population validate"
        }
        Commands::Population(crate::commands::population::PopulationCommand::Observe(_)) => {
            "population observe"
        }
        Commands::Specimen(crate::commands::specimen::SpecimenCommand::Validate(_)) => {
            "specimen validate"
        }
        Commands::Specimen(crate::commands::specimen::SpecimenCommand::Plan(_)) => "specimen plan",
        Commands::Specimen(crate::commands::specimen::SpecimenCommand::Run(_)) => "specimen run",
        Commands::Specimen(crate::commands::specimen::SpecimenCommand::Compare(_)) => {
            "specimen compare"
        }
        Commands::Specimen(crate::commands::specimen::SpecimenCommand::Report(_)) => {
            "specimen report"
        }
        Commands::Autospill(_) => "autospill",
        Commands::Autospread(_) => "autospread",
        Commands::Autospectral(_) => "autospectral",
        Commands::Peacoqc(_) => "peacoqc",
        Commands::Cytoscan(_) => "cytoscan",
        Commands::Transform(_) => "transform",
        Commands::Classify(_) => "classify",
        Commands::Gate(_) => "gate",
        Commands::Mask(MaskCommand::Combine { .. }) => "mask combine",
        Commands::Meta(_) => "meta",
        Commands::Plot(_) => "plot",
        Commands::Preflight(_) => "preflight",
        Commands::Qc(_) => "qc",
        Commands::Compensate(_) => "compensate",
        Commands::Clean(_) => "clean",
        Commands::Spillqc(_) => "spillqc",
        Commands::Stats(_) => "stats",
        Commands::Flowsom(FlowsomArgs {
            command: Some(FlowSomCommand::Train { .. }),
            ..
        }) => "flowsom train",
        Commands::Flowsom(FlowsomArgs {
            command: Some(FlowSomCommand::Apply { .. }),
            ..
        }) => "flowsom apply",
        Commands::Flowsom(FlowsomArgs { command: None, .. }) => "flowsom",
        Commands::Cytonorm(CytoNormCommand::Train { .. }) => "cytonorm train",
        Commands::Cytonorm(CytoNormCommand::Apply { .. }) => "cytonorm apply",
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum CliReusePolicy {
    Recompute,
    Skip,
    Reject,
}
impl From<CliReusePolicy> for cytiq_provenance::reuse::ReusePolicy {
    fn from(value: CliReusePolicy) -> Self {
        match value {
            CliReusePolicy::Recompute => Self::Recompute,
            CliReusePolicy::Skip => Self::Skip,
            CliReusePolicy::Reject => Self::Reject,
        }
    }
}
