//! The repository's parity gates and report generators, ported from the
//! Python scripts under `scripts/`. Each subcommand keeps its script's name,
//! flags, exit codes and report bytes (timestamps aside):
//! `cargo run --locked -q -p hermes-parity-tools -- <script name> <flags>`.

mod adapter_matrix;
mod automation_summary;
mod automation_summary_sinks;
mod background_queue;
mod behavioral_parity;
mod cargo_surface_gates;
mod chaos_harness;
mod coverage_audit;
mod coverage_tests;
mod dashboard;
mod decimal_limit;
mod deep_problem_solving;
mod differential_parity;
mod divergence;
mod elite_sync_gate;
mod elite_gate_launchd;
mod eval_trend_gate;
mod fork_carry;
mod git;
mod global_proof;
mod golden_parity;
mod golden_parity_upstream;
mod gpar_closure;
mod harness_hardening;
mod hotpath_bench;
mod llms_txt;
mod nightly_elite_gate;
mod parity_matrix;
mod performance_autopilot;
mod readme_sync_status;
mod redteam_gate;
mod release_readiness;
mod release_secret_scan;
mod report_io;
mod rustsec_patches;
mod script_helpers;
mod security_release_gate;
mod self_evolution_loop;
mod shared_diff_backlog;
mod skill_docs;
mod skills_index;
mod slash_gate;
mod surface_gate;
mod test_intent_mapping;
mod upstream_queue;
mod website;
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
    /// Summarise public parity evidence for a release decision.
    #[command(
        name = "generate-release-readiness-summary",
        args_override_self = true,
        infer_long_args = true
    )]
    ReleaseReadiness(release_readiness::ReadinessArgs),
    /// Scan tracked release source and redact all findings.
    #[command(name = "release_secret_scan")]
    ReleaseSecretScan(release_secret_scan::ScanArgs),
    #[command(name = "run-security-release-gate-v2")]
    SecurityReleaseGate(security_release_gate::SecurityReleaseGateArgs),
    #[command(name = "generate-gpar-01-04-proof")]
    GparClosure(gpar_closure::ClosureArgs),
    #[command(name = "generate-deep-problem-solving-diff", args_override_self = true)]
    DeepProblemSolving(deep_problem_solving::DeepProblemSolvingArgs),
    #[command(
        name = "generate-behavioral-similarity-diff",
        args_override_self = true
    )]
    BehavioralSimilarity(deep_problem_solving::BehavioralSimilarityArgs),
    #[command(name = "generate-parity-dashboard", args_override_self = true)]
    Dashboard(dashboard::DashboardArgs),
    #[command(name = "generate-sota-harness-hardening", args_override_self = true)]
    HarnessHardening(harness_hardening::HarnessHardeningArgs),
    /// Write the docs site's Skills Hub data (website/src/data/skills.json and skills-meta.json).
    #[command(name = "extract-skills")]
    ExtractSkills(website::WebsiteArgs),
    /// Write the docs site's llms.txt and llms-full.txt.
    #[command(name = "generate-llms-txt")]
    GenerateLlmsTxt(website::WebsiteArgs),
    /// Write the docs site's per-skill pages, the 2 skill catalogs and the Skills sidebar block.
    #[command(name = "generate-skill-docs")]
    GenerateSkillDocs(website::WebsiteArgs),
    /// Check freshness and source floors of website/static/api/skills-index.json.
    #[command(name = "check-skills-index")]
    CheckSkillsIndex(skills_index::CheckSkillsIndexArgs),
    /// Audit and optionally repair Hermes background job queue manifests.
    #[command(name = "audit-background-queue", alias = "audit_background_queue")]
    AuditBackgroundQueue(background_queue::AuditBackgroundQueueArgs),
    /// Compare two adapter chaos harness reports for retry/fallback regressions.
    #[command(name = "compare-adapter-chaos-reports")]
    CompareAdapterChaosReports(chaos_harness::CompareAdapterChaosArgs),
    /// Render or update README upstream sync status block from .sync-reports.
    #[command(name = "generate-readme-sync-status")]
    GenerateReadmeSyncStatus(readme_sync_status::ReadmeSyncStatusArgs),
    /// Evaluate benchmark trend regressions and gate on configurable thresholds.
    #[command(name = "run-eval-trend-gate")]
    RunEvalTrendGate(eval_trend_gate::EvalTrendGateArgs),
    /// Run zero-copy hot-path benchmark tests and emit a JSON report.
    #[command(name = "run-zero-copy-hotpath-bench")]
    RunZeroCopyHotpathBench(hotpath_bench::ZeroCopyHotpathArgs),
    /// Run deterministic adversarial regression gate suite.
    #[command(name = "run-redteam-gate")]
    RunRedteamGate(redteam_gate::RedteamGateArgs),
    /// Run deterministic adapter chaos harness scenarios.
    #[command(name = "run-adapter-chaos-harness")]
    RunAdapterChaosHarness(chaos_harness::RunAdapterChaosArgs),
    /// Run focused CLI-surface parity drift gate against upstream.
    #[command(name = "run-differential-parity-gate")]
    RunDifferentialParityGate(differential_parity::DifferentialParityArgs),
    /// Run performance autopilot checks and emit actionable tuning recommendations.
    #[command(name = "run-performance-autopilot")]
    RunPerformanceAutopilot(performance_autopilot::PerformanceAutopilotArgs),
    /// Run integrated self-evolution loop across parity, eval trend and elite gates.
    #[command(name = "run-self-evolution-loop")]
    RunSelfEvolutionLoop(self_evolution_loop::SelfEvolutionLoopArgs),
    /// Run behavior-level parity contract for Hermes Ultra command and TUI surfaces.
    #[command(name = "run-behavioral-parity-suite")]
    RunBehavioralParitySuite(behavioral_parity::BehavioralParityArgs),
    /// Run cargo clippy and enforce the warning allowlist.
    #[command(name = "clippy-warning-gate")]
    ClippyWarningGate(cargo_surface_gates::ClippyWarningGateArgs),
    /// Print markdown audit of Cargo workspace binaries, dependencies and tree depth.
    #[command(name = "audit-cargo-build-surface")]
    AuditCargoBuildSurface(cargo_surface_gates::RepoRootOnlyArgs),
    /// Run hermes-cli deterministic replay suite tests.
    #[command(name = "run-deterministic-replay-suite")]
    RunDeterministicReplaySuite(cargo_surface_gates::RepoRootOnlyArgs),
    /// Run OpenHuman P0/P1 static surface checks and targeted tests.
    #[command(name = "run-openhuman-p0-p1-gate")]
    RunOpenhumanP0P1Gate(cargo_surface_gates::RepoRootOnlyArgs),
    /// Run ultra-feature benchmark tests and workspace formatting check.
    #[command(name = "run-ultra-feature-benchmarks")]
    RunUltraFeatureBenchmarks(cargo_surface_gates::RepoRootOnlyArgs),
    /// Download crates.io source, reapply local RustSec patches and check/refresh overrides.
    #[command(name = "refresh-rustsec-patches")]
    RefreshRustsecPatches(rustsec_patches::RefreshRustsecPatchesArgs),
    /// Run consolidated ELITE sync gates and emit a single pass/fail report.
    #[command(name = "run-elite-sync-gate")]
    RunEliteSyncGate(elite_sync_gate::EliteSyncGateArgs),
    /// Publish automation summaries with ContextLattice-first sink fallback.
    #[command(name = "publish-automation-summary")]
    PublishAutomationSummary(automation_summary::AutomationSummaryArgs),
    /// Golden parity harness for CLI command surface and key TUI flow contracts.
    #[command(name = "run-golden-parity-harness")]
    RunGoldenParityHarness(golden_parity::GoldenParityArgs),
    /// Run the nightly elite validation gate on local main.
    #[command(name = "run-nightly-elite-gate")]
    RunNightlyEliteGate(nightly_elite_gate::NightlyEliteGateArgs),
    /// Install or update the launchd user agent that runs the nightly elite gate.
    #[command(name = "install-elite-gate-launchd")]
    InstallEliteGateLaunchd(elite_gate_launchd::InstallArgs),
    /// Show launchd status and logs for the nightly elite gate.
    #[command(name = "status-elite-gate-launchd")]
    StatusEliteGateLaunchd(elite_gate_launchd::StatusArgs),
    /// Unload and remove the nightly elite gate launchd agent.
    #[command(name = "uninstall-elite-gate-launchd")]
    UninstallEliteGateLaunchd(elite_gate_launchd::UninstallArgs),
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
        Command::ReleaseReadiness(args) => release_readiness::run(args),
        Command::ReleaseSecretScan(args) => release_secret_scan::run(args),
        Command::SecurityReleaseGate(args) => security_release_gate::run(args),
        Command::GparClosure(args) => gpar_closure::run(args),
        Command::DeepProblemSolving(args) => deep_problem_solving::run(args),
        Command::BehavioralSimilarity(args) => deep_problem_solving::run_behavioral(args),
        Command::Dashboard(args) => dashboard::run(args),
        Command::HarnessHardening(args) => harness_hardening::run(args),
        Command::ExtractSkills(args) => website::run_extract_skills(args),
        Command::GenerateLlmsTxt(args) => llms_txt::run(args),
        Command::GenerateSkillDocs(args) => skill_docs::run(args),
        Command::CheckSkillsIndex(args) => skills_index::run(args),
        Command::AuditBackgroundQueue(args) => background_queue::run(args),
        Command::CompareAdapterChaosReports(args) => chaos_harness::run_compare(args),
        Command::GenerateReadmeSyncStatus(args) => readme_sync_status::run(args),
        Command::RunEvalTrendGate(args) => eval_trend_gate::run(args),
        Command::RunZeroCopyHotpathBench(args) => hotpath_bench::run(args),
        Command::RunRedteamGate(args) => redteam_gate::run(args),
        Command::RunAdapterChaosHarness(args) => chaos_harness::run_harness(args),
        Command::RunDifferentialParityGate(args) => differential_parity::run(args),
        Command::RunPerformanceAutopilot(args) => performance_autopilot::run(args),
        Command::RunSelfEvolutionLoop(args) => self_evolution_loop::run(args),
        Command::RunBehavioralParitySuite(args) => behavioral_parity::run(args),
        Command::ClippyWarningGate(args) => cargo_surface_gates::run_clippy_warning_gate(args),
        Command::AuditCargoBuildSurface(args) => {
            cargo_surface_gates::run_audit_cargo_build_surface(args)
        }
        Command::RunDeterministicReplaySuite(args) => {
            cargo_surface_gates::run_deterministic_replay_suite(args)
        }
        Command::RunOpenhumanP0P1Gate(args) => cargo_surface_gates::run_openhuman_p0_p1_gate(args),
        Command::RunUltraFeatureBenchmarks(args) => {
            cargo_surface_gates::run_ultra_feature_benchmarks(args)
        }
        Command::RefreshRustsecPatches(args) => rustsec_patches::run(args),
        Command::RunEliteSyncGate(args) => elite_sync_gate::run(args),
        Command::PublishAutomationSummary(args) => automation_summary::run(args),
        Command::RunGoldenParityHarness(args) => golden_parity::run(args),
        Command::RunNightlyEliteGate(args) => nightly_elite_gate::run(args),
        Command::InstallEliteGateLaunchd(args) => elite_gate_launchd::run_install(args),
        Command::StatusEliteGateLaunchd(args) => elite_gate_launchd::run_status(args),
        Command::UninstallEliteGateLaunchd(args) => elite_gate_launchd::run_uninstall(args),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}
