//! The repository's parity gates and report generators, ported from the
//! Python scripts under `scripts/`. Each subcommand keeps its script's name,
//! flags, exit codes and report bytes (timestamps aside):
//! `cargo run --locked -q -p hermes-parity-tools -- <script name> <flags>`.

mod adapter_matrix;
mod coverage_audit;
mod coverage_tests;
mod decimal_limit;
mod divergence;
mod fork_carry;
mod git;
mod global_proof;
mod parity_matrix;
mod report_io;
mod shared_diff_backlog;
mod slash_gate;
mod surface_gate;
mod test_intent_mapping;
mod upstream_queue;
mod workstream_status;

use clap::{Parser, Subcommand};
use hermes_py as py;

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
    /// Generate the gateway platform and memory plugin feature matrix.
    #[command(name = "generate-adapter-matrix")]
    AdapterMatrix(adapter_matrix::AdapterMatrixArgs),
    /// Map upstream test domains to evidence in the local tree.
    #[command(name = "generate-test-intent-mapping")]
    TestIntentMapping(test_intent_mapping::TestIntentMappingArgs),
    /// Audit the cited local tests and mapped upstream behaviour.
    #[command(name = "generate-test-coverage-audit")]
    CoverageAudit(coverage_audit::CoverageAuditArgs),
    /// Generate a parity gap matrix for local vs upstream.
    #[command(
        name = "generate-parity-matrix",
        args_override_self = true,
        infer_long_args = true
    )]
    ParityMatrix(parity_matrix::ParityMatrixArgs),
    /// Classify the paths changed in both Git trees.
    #[command(
        name = "generate-shared-diff-backlog",
        args_override_self = true,
        infer_long_args = true
    )]
    SharedDiffBacklog(shared_diff_backlog::SharedDiffBacklogArgs),
    /// Report evidence for parity workstreams.
    #[command(
        name = "generate-workstream-status",
        args_override_self = true,
        infer_long_args = true
    )]
    WorkstreamStatus(workstream_status::WorkstreamStatusArgs),
    /// Verify the required fork behaviour contracts by executing their exact Rust tests.
    CheckForkCarry(fork_carry::ForkCarryArgs),
    /// Generate the upstream missing patch queue.
    #[command(
        name = "generate-upstream-patch-queue",
        args_override_self = true,
        infer_long_args = true
    )]
    UpstreamQueue(upstream_queue::UpstreamQueueArgs),
    /// Evaluate CI and release parity proof thresholds.
    #[command(
        name = "generate-global-parity-proof",
        args_override_self = true,
        infer_long_args = true
    )]
    GlobalProof(global_proof::GlobalProofArgs),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::SlashGate(args) => slash_gate::run(args),
        Command::SurfaceGate(args) => surface_gate::run(args),
        Command::ValidateDivergence(args) => divergence::run(args),
        Command::AdapterMatrix(args) => adapter_matrix::run(args),
        Command::TestIntentMapping(args) => test_intent_mapping::run(args),
        Command::CoverageAudit(args) => coverage_audit::run(args),
        Command::ParityMatrix(args) => parity_matrix::run(args),
        Command::SharedDiffBacklog(args) => shared_diff_backlog::run(args),
        Command::WorkstreamStatus(args) => workstream_status::run(args),
        Command::CheckForkCarry(args) => fork_carry::run(args),
        Command::UpstreamQueue(args) => upstream_queue::run(args),
        Command::GlobalProof(args) => global_proof::run(args),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}
