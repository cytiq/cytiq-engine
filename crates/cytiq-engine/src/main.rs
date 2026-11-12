//! The `cytiq` binary: parse the command line, record provenance and run one command.

mod cli;
#[cfg(test)]
mod cli_surface;
mod commands;
mod error;
mod logging;
mod parsers;
mod provenance;

use std::env;
use std::process::ExitCode;

use clap::Parser;
use cytiq_provenance::RunRecorder;
use cytiq_provenance::outcome::CommandOutcome;

use crate::cli::{Cli, Commands, command_name};
use crate::logging::init_logging;
use crate::provenance::build_info;

fn main() -> ExitCode {
    if let Err(error) = init_logging() {
        eprintln!("failed to initialize logging: {error}");
    }

    // Usage errors exit here with clap's code 2 and write no outcome.
    let Cli {
        outcome,
        reuse,
        command,
    } = Cli::parse();
    let name = format!("cytiq {}", command_name(&command));
    let result = match run(command, &name, reuse.into()) {
        Ok(Some(result)) => {
            if result.exit_code != 0 {
                eprintln!("{}: {}", result.reason.as_str(), result.message);
            }
            result
        }
        Ok(None) => error::success(&name),
        Err(failure) => {
            eprintln!("{failure}");
            error::failure(&name, failure.as_ref())
        }
    };
    if let Some(path) = outcome {
        // A failed outcome write must not turn a published result into a
        // failure that a retry would then trip over.
        if let Err(write_error) = result.write(&path) {
            eprintln!(
                "failed to write outcome `{}`: {write_error}",
                path.display()
            );
        }
    }
    ExitCode::from(result.exit_code)
}

/// `Some` when a command publishes or reuses a typed outcome; `None` for
/// a successful command without durable output.
fn run(
    command: Commands,
    name: &str,
    policy: cytiq_provenance::reuse::ReusePolicy,
) -> Result<Option<CommandOutcome>, Box<dyn std::error::Error>> {
    if let Some(threads) = command.rayon_threads() {
        // Before anything else touches rayon; a build error exits 1.
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()?;
    }
    let mut recorder = RunRecorder::start(
        name,
        env!("CARGO_PKG_VERSION"),
        env::args().skip(1).collect(),
    )
    .with_build(build_info());
    recorder.set_config(&command);
    let recorder = crate::provenance::Invocation { recorder, policy };
    match command {
        Commands::Population(command) => {
            commands::population::run(command, recorder.recorder, policy).map(Some)
        }
        Commands::Specimen(command) => {
            commands::specimen::run(command, recorder.recorder, policy).map(Some)
        }
        Commands::Autospill(args) => commands::autospill::run(args, recorder),
        Commands::Autospread(args) => commands::autospread::run(args, recorder),
        Commands::Autospectral(args) => commands::autospectral::run(args, recorder),
        Commands::Peacoqc(args) => commands::peacoqc::run(args, recorder),
        Commands::Cytoscan(args) => commands::cytoscan::run(args, recorder),
        Commands::Transform(args) => commands::transform::run(args, recorder),
        Commands::Classify(args) => commands::classify::run(args, recorder),
        Commands::Gate(args) => commands::gate::run(args, recorder),
        Commands::Compensate(args) => commands::compensate::run(args, recorder),
        Commands::Mask(command) => commands::mask::run(command, recorder),
        Commands::Plot(args) => commands::plot::run(args, recorder),
        Commands::Preflight(args) => commands::preflight::run(args, recorder),
        Commands::Qc(args) => commands::qc::run(args, recorder),
        Commands::Meta(args) => commands::meta::run(args, recorder),
        Commands::Spillqc(args) => commands::spillqc::run(&args, recorder),
        Commands::Stats(args) => commands::stats::run(args, recorder),
        Commands::Clean(args) => commands::clean::run(args, recorder),
        Commands::Flowsom(args) => commands::flowsom::run(args, recorder),
        Commands::Cytonorm(command) => commands::cytonorm::run(command, recorder),
    }
}
