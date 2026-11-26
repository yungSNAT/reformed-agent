//! The `worker` subcommand's flags, with the script's defaults and
//! environment fallbacks.

use std::path::PathBuf;
use std::time::Duration;

use clap::ValueEnum;

use crate::upkeep::UpkeepConfig;
use crate::worker::{SyncOptions, WorkerConfig};
use crate::{queue, worker, BackendArgs};

#[derive(Clone, Copy, ValueEnum)]
enum Strategy {
    Merge,
    CherryPick,
}

#[derive(clap::Args)]
pub(crate) struct WorkerArgs {
    /// Default: the checkout this binary was built from.
    #[arg(long)]
    repo_root: Option<PathBuf>,
    #[command(flatten)]
    backend: BackendArgs,
    /// Default: UPSTREAM_SYNC_KAFKA_GROUP, else hermes-upstream-worker.
    #[arg(long = "kafka-group-id")]
    _kafka_group_id: Option<String>,
    #[arg(long, default_value_t = 10)]
    poll_interval_sec: u64,
    #[arg(long, default_value_t = 3)]
    max_attempts: i64,
    #[arg(long, default_value_t = 1800)]
    sync_timeout_sec: u64,
    #[arg(long, value_enum, default_value = "merge")]
    strategy: Strategy,
    /// Always on, as in the script; kept for callers that pass it.
    #[arg(long = "strict-risk-gate")]
    _strict_risk_gate: bool,
    #[arg(long)]
    allow_risk_paths: bool,
    #[arg(long, default_value = "upstream-sync-conflict")]
    conflict_label: String,
    #[arg(long)]
    no_tests: bool,
    #[arg(long)]
    no_redteam_gate: bool,
    /// Default: UPSTREAM_SYNC_REDTEAM_CMD, else the parity tools' red-team gate.
    #[arg(long)]
    redteam_cmd: Option<String>,
    #[arg(long)]
    elite_gate: bool,
    /// Default: UPSTREAM_SYNC_ELITE_CMD, else python3 scripts/run-elite-sync-gate.py.
    #[arg(long)]
    elite_cmd: Option<String>,
    /// Default: UPSTREAM_SYNC_ELITE_ROLLBACK_CMD.
    #[arg(long)]
    elite_rollback_cmd: Option<String>,
    #[arg(long)]
    no_pr: bool,
    #[arg(long)]
    draft_pr: bool,
    /// Default: UPSTREAM_SYNC_PR_LABELS, else upstream-sync,parity-sync.
    #[arg(long)]
    pr_labels: Option<String>,
    #[arg(long)]
    disable_parity_drift_check: bool,
    #[arg(long)]
    disable_global_parity_check: bool,
    #[arg(long)]
    disable_summary_publish: bool,
    #[arg(long)]
    allow_python_test_surfaces: bool,
    /// Default: UPSTREAM_SYNC_ASSIST_CMD.
    #[arg(long, default_value = "")]
    assist_cmd: String,
    #[arg(long)]
    disable_upkeep_commit_queue: bool,
    #[arg(long, default_value_t = 43)]
    upkeep_commit_parent_issue: i64,
    #[arg(long, default_value = "parity,parity-upkeep,upstream-sync")]
    upkeep_commit_labels: String,
    /// Default: UPSTREAM_SYNC_UPKEEP_COMMIT_STATE_PATH, else
    /// .sync-reports/upkeep-commit-queue-state.json; relative to --repo-root.
    #[arg(long)]
    upkeep_commit_state_path: Option<String>,
    #[arg(long, default_value_t = 40)]
    upkeep_commit_max_per_event: i64,
}

fn env_or(flag: Option<String>, var: &str, default: &str) -> String {
    flag.unwrap_or_else(|| std::env::var(var).unwrap_or_else(|_| default.to_string()))
}

pub(crate) fn run(args: WorkerArgs) -> Result<(), (u8, String)> {
    args.backend.check()?;
    if !(args.disable_parity_drift_check
        && args.disable_global_parity_check
        && args.disable_summary_publish)
    {
        return Err((
            2,
            "the drift checks and summary publishing are not ported to Rust yet; pass \
             --disable-parity-drift-check --disable-global-parity-check --disable-summary-publish"
                .into(),
        ));
    }
    let repo_root = args
        .repo_root
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")));
    let report_dir = repo_root.join(".sync-reports");
    let state_path = env_or(
        args.upkeep_commit_state_path,
        "UPSTREAM_SYNC_UPKEEP_COMMIT_STATE_PATH",
        ".sync-reports/upkeep-commit-queue-state.json",
    );
    let state_path = match state_path.as_str() {
        "" => report_dir.join("upkeep-commit-queue-state.json"),
        rel => repo_root.join(rel),
    };
    let assist = match args.assist_cmd.as_str() {
        "" => std::env::var("UPSTREAM_SYNC_ASSIST_CMD").unwrap_or_default(),
        given => given.to_string(),
    };
    let assist = hermes_py::strip(&assist);
    let labels = args
        .upkeep_commit_labels
        .split(',')
        .map(hermes_py::strip)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let config = WorkerConfig {
        poll_interval: Duration::from_secs(args.poll_interval_sec),
        max_attempts: args.max_attempts,
        sync: SyncOptions {
            strategy: match args.strategy {
                Strategy::Merge => "merge",
                Strategy::CherryPick => "cherry-pick",
            }
            .to_string(),
            allow_risk_paths: args.allow_risk_paths,
            conflict_label: args.conflict_label,
            no_tests: args.no_tests,
            no_pr: args.no_pr,
            draft_pr: args.draft_pr,
            pr_labels: env_or(
                args.pr_labels,
                "UPSTREAM_SYNC_PR_LABELS",
                "upstream-sync,parity-sync",
            ),
            run_redteam_gate: !args.no_redteam_gate,
            redteam_cmd: env_or(
                args.redteam_cmd,
                "UPSTREAM_SYNC_REDTEAM_CMD",
                "cargo run --locked -q -p hermes-parity-tools -- run-redteam-gate",
            ),
            run_elite_gate: args.elite_gate,
            elite_cmd: env_or(
                args.elite_cmd,
                "UPSTREAM_SYNC_ELITE_CMD",
                "python3 scripts/run-elite-sync-gate.py",
            ),
            elite_rollback_cmd: env_or(
                args.elite_rollback_cmd,
                "UPSTREAM_SYNC_ELITE_ROLLBACK_CMD",
                "",
            ),
            timeout: Duration::from_secs(args.sync_timeout_sec),
        },
        upkeep: UpkeepConfig {
            enabled: !args.disable_upkeep_commit_queue,
            labels,
            parent_issue: args.upkeep_commit_parent_issue,
            state_path,
            max_commits_per_event: args.upkeep_commit_max_per_event,
            allow_python_test_surfaces: args.allow_python_test_surfaces,
        },
        assist_cmd: (!assist.is_empty()).then(|| assist.to_string()),
        repo_root,
    };
    let queue = queue::SqliteQueue::open(&args.backend.sqlite_path).map_err(|e| (1, e))?;
    worker::run(&config, &queue).map_err(|e| (1, e))
}
