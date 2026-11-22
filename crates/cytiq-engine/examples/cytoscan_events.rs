//! `cytiq cytoscan` with a per-file event sample other than upstream's 1,000
//! (`CytoScanConfig::events_per_file`), for the novelty benchmark's
//! increased-event arms (`parity/scientific/novelty_benchmark.py`). A
//! development tool, not a cytiq subcommand: it records no provenance, and
//! `events_per_file` reaches the CLI only if the benchmark's frozen decision
//! rule makes its arm eligible. With `--events-per-file 1000` its outputs are
//! byte-identical to `cytiq cytoscan`; the benchmark checks this.

use std::error::Error;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use cytiq_cytoscan::{CytoScanConfig, CytoScanRunner, FeatureMethod, FlagMethod};

#[derive(Clone, Copy, ValueEnum)]
enum Method {
    Summary,
    Quantiles,
    Emd,
    Binning,
    Fingerprint,
}

#[derive(Clone, Copy, ValueEnum)]
enum Flag {
    Outlier,
    Novelty,
    Both,
}

#[derive(Parser)]
struct Args {
    #[arg(long, required = true)]
    input: Vec<PathBuf>,
    #[arg(long)]
    reference: Vec<PathBuf>,
    #[arg(long)]
    output: PathBuf,
    /// Comma-separated channel names.
    #[arg(long)]
    channels: String,
    #[arg(long, value_enum, default_value_t = Method::Quantiles)]
    method: Method,
    #[arg(long, value_enum, default_value_t = Flag::Outlier)]
    flag_method: Flag,
    #[arg(long, default_value_t = cytiq_cytoscan::DEFAULT_RANDOM_SEED)]
    seed: u64,
    #[arg(long)]
    stability_seeds: Option<usize>,
    #[arg(long, default_value_t = cytiq_cytoscan::DEFAULT_EVENTS_PER_FILE)]
    events_per_file: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let mut config = CytoScanConfig::new(
        args.input,
        args.output,
        cytiq_fcs::parse_channel_list(&args.channels)?,
    );
    config.reference_inputs = args.reference;
    config.feature_method = match args.method {
        Method::Summary => FeatureMethod::Summary,
        Method::Quantiles => FeatureMethod::Quantiles,
        Method::Emd => FeatureMethod::Emd,
        Method::Binning => FeatureMethod::Binning,
        Method::Fingerprint => FeatureMethod::Fingerprint,
    };
    config.flag_method = match args.flag_method {
        Flag::Outlier => FlagMethod::Outlier,
        Flag::Novelty => FlagMethod::Novelty,
        Flag::Both => FlagMethod::Both,
    };
    config.random_seed = args.seed;
    config.stability_seeds = args.stability_seeds;
    config.events_per_file = args.events_per_file;
    let result = CytoScanRunner::run(&config)?;
    for warning in &result.diagnostics.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}
