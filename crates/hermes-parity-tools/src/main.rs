//! The repository's parity gates and report generators, ported from the
//! Python scripts under `scripts/`. Each subcommand keeps its script's name,
//! flags, exit codes and report bytes (timestamps aside):
//! `cargo run --locked -q -p hermes-parity-tools -- <script name> <flags>`.

mod divergence;
mod git;
mod py;
mod slash_gate;
mod surface_gate;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "hermes-parity-tools", infer_long_args = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Fail when upstream slash-command surface drifts without explicit divergence.
    #[command(name = "run-upstream-slash-parity-gate")]
    SlashGate(slash_gate::SlashGateArgs),
    /// Fail when required upstream surface files are missing locally.
    #[command(name = "run-upstream-surface-coverage-gate")]
    SurfaceGate(surface_gate::SurfaceGateArgs),
    /// Validate intentional-divergence registry quality and review freshness.
    #[command(name = "validate-intentional-divergence")]
    ValidateDivergence(divergence::DivergenceArgs),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::SlashGate(args) => slash_gate::run(args),
        Command::SurfaceGate(args) => surface_gate::run(args),
        Command::ValidateDivergence(args) => divergence::run(args),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}
