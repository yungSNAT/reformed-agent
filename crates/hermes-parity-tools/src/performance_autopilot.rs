//! Run performance autopilot checks and emit actionable tuning recommendations.
//! Port of `scripts/run-performance-autopilot.py`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::{resolve_argument, truth};
use crate::script_helpers::{
    py_int, require_existing_dir, resolve_repo_root, run_bash_lc, tail_chars, write_json_pretty,
    write_text_creating_parents,
};

const DEFAULT_HOTPATH_CMD: &str =
    "cargo run --locked -q -p hermes-parity-tools -- run-zero-copy-hotpath-bench";
const DEFAULT_EVAL_CMD: &str =
    "cargo run --locked -q -p hermes-parity-tools -- run-eval-trend-gate --allow-missing-baseline";
const DEFAULT_MCP_CMD: &str =
    "cargo test -p hermes-mcp stale_transport_marker_detection_matches_known_variants -- --nocapture";
const DEFAULT_CONTEXTLATTICE_CMD: &str =
    "curl -fsS --max-time 5 \"${CONTEXTLATTICE_ORCHESTRATOR_URL:-${MEMMCP_ORCHESTRATOR_URL:-http://127.0.0.1:8075}}/health\"";

#[derive(Args, Debug)]
pub struct PerformanceAutopilotArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Hot-path benchmark command
    #[arg(long, default_value = DEFAULT_HOTPATH_CMD)]
    hotpath_cmd: String,
    /// Eval trend command
    #[arg(long, default_value = DEFAULT_EVAL_CMD)]
    eval_cmd: String,
    /// MCP stale-transport recovery regression command
    #[arg(long, default_value = DEFAULT_MCP_CMD)]
    mcp_cmd: String,
    /// ContextLattice preflight/intelligence telemetry command
    #[arg(long, default_value = DEFAULT_CONTEXTLATTICE_CMD)]
    contextlattice_cmd: String,
    /// Optional JSON report path
    #[arg(long, default_value = "")]
    output_json: String,
    /// Optional markdown report path
    #[arg(long, default_value = "")]
    output_md: String,
    /// Optional env file path to write recommended knobs
    #[arg(long, default_value = "")]
    apply_env: String,
    /// Return non-zero when checks fail (default: advisory mode, always exits 0)
    #[arg(long)]
    strict: bool,
    /// Print JSON report
    #[arg(long)]
    json: bool,
}

struct Section {
    ok: bool,
    exit_code: i32,
    stdout_tail: String,
    stderr_tail: String,
    py_value: Py,
}

fn run_section(command: &str, cwd: &Path, max_tail: usize) -> Result<Section, String> {
    let run = run_bash_lc(command, cwd)?;
    let stdout_tail = tail_chars(&run.stdout, max_tail).to_string();
    let stderr_tail = tail_chars(&run.stderr, max_tail).to_string();
    let duration_ms = run.elapsed_ms;
    let py_value = Py::dict([
        ("command", Py::from(command)),
        ("exit_code", Py::Int(i128::from(run.exit_code))),
        ("ok", Py::from(run.ok)),
        ("started_at", Py::from(py::isoformat(run.started))),
        ("finished_at", Py::from(py::isoformat(run.finished))),
        ("duration_ms", Py::Int(duration_ms)),
        ("stdout_tail", Py::from(stdout_tail.as_str())),
        ("stderr_tail", Py::from(stderr_tail.as_str())),
    ]);
    Ok(Section {
        ok: run.ok,
        exit_code: run.exit_code,
        stdout_tail,
        stderr_tail,
        py_value,
    })
}

fn parse_hotpath_ns(stdout: &str) -> Option<i128> {
    let needle = "tool_policy_hot_path_ns_per_eval=";
    let idx = stdout.rfind(needle)?;
    let after = &stdout[idx + needle.len()..];
    let first_line = after.split('\n').next().unwrap_or("");
    let value = py::strip(first_line);
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    value.parse::<i128>().ok()
}

fn parse_contextlattice_payload(stdout_tail: &str) -> Option<Py> {
    let raw = py::strip(stdout_tail);
    if raw.is_empty() {
        return None;
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    let parsed = py::loads(&raw[start..=end]).ok()?;
    parsed.is_dict().then_some(parsed)
}

struct ContextSummary {
    healthy: bool,
    warnings: usize,
    python_fallbacks: i128,
    has_source_counts_dict: bool,
    source_counts_empty: bool,
    queue_pending_total: i128,
}

fn contextlattice_summary(payload: Option<&Py>) -> ContextSummary {
    let mut summary = ContextSummary {
        healthy: false,
        warnings: 0,
        python_fallbacks: 0,
        has_source_counts_dict: true,
        source_counts_empty: true,
        queue_pending_total: 0,
    };
    let Some(payload) = payload else {
        return summary;
    };
    if let Some(health) = payload.get("health").filter(|v| v.is_dict()) {
        summary.healthy = health.get("ok").is_some_and(truth);
    }
    if let Some(Py::List(warnings)) = payload.get("warnings") {
        summary.warnings = warnings.len();
    }
    if let Some(retrieval) = payload
        .get("context_pack")
        .filter(|v| v.is_dict())
        .and_then(|cp| cp.get("retrieval"))
        .filter(|v| v.is_dict())
    {
        if let Some(Py::Dict(source_counts)) = retrieval.get("source_counts") {
            summary.has_source_counts_dict = true;
            summary.source_counts_empty = source_counts.is_empty();
        } else if retrieval.get("source_counts").is_some() {
            summary.has_source_counts_dict = false;
        }
        if let Some(fallbacks) = retrieval.get("fallback_counts").filter(|v| v.is_dict()) {
            summary.python_fallbacks = py_int(fallbacks.get("python_hot_path_total"), 0);
        }
    }
    if let Some(queue) = payload
        .get("status")
        .filter(|v| v.is_dict())
        .and_then(|s| s.get("queue"))
        .filter(|v| v.is_dict())
    {
        summary.queue_pending_total = py_int(queue.get("pendingTotal"), 0);
    }
    summary
}

#[derive(Clone)]
struct Recommendation {
    id: &'static str,
    severity: &'static str,
    title: &'static str,
    recommendation: &'static str,
}

impl Recommendation {
    fn to_py(&self) -> Py {
        Py::dict([
            ("id", Py::from(self.id)),
            ("severity", Py::from(self.severity)),
            ("title", Py::from(self.title)),
            ("recommendation", Py::from(self.recommendation)),
        ])
    }
}

fn build_recommendations(
    hotpath: &Section,
    eval_gate: &Section,
    mcp_gate: &Section,
    context_gate: &Section,
) -> (Vec<Recommendation>, Option<i128>, ContextSummary) {
    let combined = format!("{}\n{}", hotpath.stdout_tail, hotpath.stderr_tail);
    let ns = parse_hotpath_ns(&combined);
    let ctx_payload = parse_contextlattice_payload(&context_gate.stdout_tail);
    let ctx_summary = contextlattice_summary(ctx_payload.as_ref());
    let mut recs = Vec::new();

    if !hotpath.ok {
        recs.push(Recommendation {
            id: "HOTPATH_FAIL",
            severity: "P0",
            title: "Hot-path benchmark failed",
            recommendation: "Run `cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture` and resolve regressions before release.",
        });
    } else if ns.is_some_and(|v| v > 12000) {
        recs.push(Recommendation {
            id: "HOTPATH_SLOW",
            severity: "P1",
            title: "Tool policy hot-path latency above target",
            recommendation: "Keep `HERMES_TOOL_POLICY_PRESET=standard`, review deny-pattern complexity, and rerun zero-copy bench.",
        });
    }

    if !eval_gate.ok {
        recs.push(Recommendation {
            id: "EVAL_TREND_FAIL",
            severity: "P0",
            title: "Eval trend gate failed",
            recommendation: "Run `cargo run --locked -q -p hermes-parity-tools -- run-self-evolution-loop --json` and address top recommendation before promotion.",
        });
    }

    if !mcp_gate.ok {
        recs.push(Recommendation {
            id: "MCP_STALE_RECOVERY_FAIL",
            severity: "P1",
            title: "MCP stale transport recovery regression",
            recommendation: "Run `cargo test -p hermes-mcp` and restore reconnect-on-stale behavior before promotion.",
        });
    }

    if !context_gate.ok {
        recs.push(Recommendation {
            id: "CONTEXTLATTICE_PREFLIGHT_FAIL",
            severity: "P0",
            title: "ContextLattice preflight failed",
            recommendation: "Check ContextLattice orchestrator health (`/health` on `${CONTEXTLATTICE_ORCHESTRATOR_URL:-http://127.0.0.1:8075}`) and resolve retrieval/service health before promotion.",
        });
    } else if !ctx_summary.healthy {
        recs.push(Recommendation {
            id: "CONTEXTLATTICE_UNHEALTHY",
            severity: "P1",
            title: "ContextLattice health is degraded",
            recommendation: "Use `/objective context max` and confirm orchestrator health/retrieval lanes before long-running objective loops.",
        });
    }

    if ctx_summary.python_fallbacks > 0 {
        recs.push(Recommendation {
            id: "CONTEXTLATTICE_PYTHON_FALLBACK",
            severity: "P1",
            title: "ContextLattice retrieval fallback detected",
            recommendation: "Investigate non-native fallback causes and keep Go/Rust lanes hot to avoid degraded memory-intelligence behavior.",
        });
    }

    if ctx_summary.has_source_counts_dict
        && ctx_summary.source_counts_empty
        && ctx_summary.python_fallbacks == 0
    {
        recs.push(Recommendation {
            id: "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE",
            severity: "P1",
            title: "ContextLattice source coverage is empty",
            recommendation: "Use broader same-project context-pack and ensure topic rollups/primary stores return at least one grounded hit.",
        });
    }

    if ctx_summary.queue_pending_total > 8 {
        recs.push(Recommendation {
            id: "CONTEXTLATTICE_QUEUE_PRESSURE",
            severity: "P2",
            title: "ContextLattice queue pressure elevated",
            recommendation: "Reduce write burst size or raise checkpoint spacing for long loops until pending queue normalizes.",
        });
    }

    if recs.is_empty() {
        recs.push(Recommendation {
            id: "PERF_STABLE",
            severity: "P3",
            title: "Performance checks stable",
            recommendation: "No immediate tuning required. Keep nightly elite gate cadence.",
        });
    }

    (recs, ns, ctx_summary)
}

struct AdaptiveAction {
    key: &'static str,
    value: &'static str,
    reason: &'static str,
}

impl AdaptiveAction {
    fn to_py(&self) -> Py {
        Py::dict([
            ("key", Py::from(self.key)),
            ("value", Py::from(self.value)),
            ("reason", Py::from(self.reason)),
        ])
    }
}

struct AdaptiveIndexes {
    performance_index: f64,
    intelligence_index: f64,
    adaptive_index: f64,
    profile_recommendation: &'static str,
    adaptive_actions: Vec<AdaptiveAction>,
}

fn round_2(v: f64) -> f64 {
    (v * 100.0).round_ties_even() / 100.0
}

fn compute_adaptive_indexes(
    hotpath: &Section,
    eval_gate: &Section,
    mcp_gate: &Section,
    context_gate: &Section,
    recommendations: &[Recommendation],
    ns: Option<i128>,
    ctx_summary: &ContextSummary,
) -> AdaptiveIndexes {
    let checks = [hotpath.ok, eval_gate.ok, mcp_gate.ok, context_gate.ok];
    let pass_count = checks.iter().filter(|&&ok| ok).count();
    let fail_count = checks.len() - pass_count;

    let mut base_performance = 100.0_f64;
    if let Some(ns_val) = ns.filter(|&v| v > 12000) {
        let overflow_ratio = (((ns_val - 12000) as f64) / 12000.0).min(3.0);
        base_performance -= (overflow_ratio * 10.0).min(30.0);
    }
    if !hotpath.ok {
        base_performance -= 35.0;
    }
    if !mcp_gate.ok {
        base_performance -= 20.0;
    }
    if !context_gate.ok {
        base_performance -= 25.0;
    }
    base_performance = base_performance.clamp(0.0, 100.0);

    let penalty: f64 = recommendations
        .iter()
        .map(|rec| match rec.severity {
            "P0" => 22.0,
            "P1" => 12.0,
            "P2" => 6.0,
            "P3" => 2.0,
            _ => 4.0,
        })
        .sum();
    let mut base_intelligence = 100.0_f64 - penalty;
    if !eval_gate.ok {
        base_intelligence -= 18.0;
    }
    if !context_gate.ok {
        base_intelligence -= 20.0;
    }
    if ctx_summary.python_fallbacks > 0 {
        base_intelligence -= (ctx_summary.python_fallbacks as f64).min(12.0);
    }
    if ctx_summary.has_source_counts_dict
        && ctx_summary.source_counts_empty
        && ctx_summary.python_fallbacks == 0
    {
        base_intelligence -= 10.0;
    }
    if ctx_summary.queue_pending_total > 8 {
        base_intelligence -= 8.0;
    }
    base_intelligence = base_intelligence.clamp(0.0, 100.0);

    let adaptive_index = round_2(base_performance * 0.55 + base_intelligence * 0.45);
    let performance_index = round_2(base_performance);
    let intelligence_index = round_2(base_intelligence);

    let profile = if fail_count >= 2 {
        "safety"
    } else if !eval_gate.ok || !context_gate.ok {
        "quality"
    } else if !mcp_gate.ok {
        "reliability"
    } else if ns.is_some_and(|v| v > 12000) {
        "throughput"
    } else {
        "balanced"
    };

    let mut adaptive_actions = vec![AdaptiveAction {
        key: "HERMES_PERF_AUTOPILOT_PROFILE",
        value: profile,
        reason: "profile recommendation from adaptive index",
    }];
    match profile {
        "throughput" => adaptive_actions.extend([
            AdaptiveAction {
                key: "HERMES_TOOL_POLICY_PRESET",
                value: "standard",
                reason: "reduce policy hot-path overhead",
            },
            AdaptiveAction {
                key: "HERMES_MODEL_CATALOG_GUARD",
                value: "1",
                reason: "avoid invalid model retries",
            },
        ]),
        "quality" => adaptive_actions.extend([
            AdaptiveAction {
                key: "HERMES_REPLAY_ENABLED",
                value: "1",
                reason: "capture deterministic replay for eval failures",
            },
            AdaptiveAction {
                key: "HERMES_MODEL_AUTO_REMEDIATE",
                value: "1",
                reason: "promote self-heal recommendation loop",
            },
        ]),
        "reliability" => adaptive_actions.push(AdaptiveAction {
            key: "HERMES_TOOL_POLICY_MODE",
            value: "enforce",
            reason: "stabilize stale transport/recovery behavior",
        }),
        "safety" => adaptive_actions.extend([
            AdaptiveAction {
                key: "HERMES_TOOL_POLICY_MODE",
                value: "enforce",
                reason: "strict policy posture under multi-check failure",
            },
            AdaptiveAction {
                key: "HERMES_REPLAY_ENABLED",
                value: "1",
                reason: "preserve incident evidence during degraded state",
            },
        ]),
        _ => adaptive_actions.push(AdaptiveAction {
            key: "HERMES_PERF_AUTOPILOT_STATUS",
            value: "stable",
            reason: "all checks stable",
        }),
    }

    AdaptiveIndexes {
        performance_index,
        intelligence_index,
        adaptive_index,
        profile_recommendation: profile,
        adaptive_actions,
    }
}

fn write_markdown(
    path: &Path,
    generated_at: &str,
    ok: bool,
    adaptive: &AdaptiveIndexes,
    sections: &[(&str, &Section)],
    recommendations: &[Recommendation],
) -> Result<(), String> {
    let ok_label = if ok { "True" } else { "False" };
    let mut lines = vec![
        "# Performance Autopilot Report".to_string(),
        String::new(),
        format!("- generated_at: `{generated_at}`"),
        format!("- ok: `{ok_label}`"),
        format!("- intelligence_index: `{:.2}`", adaptive.intelligence_index),
        format!("- performance_index: `{:.2}`", adaptive.performance_index),
        format!("- adaptive_index: `{:.2}`", adaptive.adaptive_index),
        format!(
            "- profile_recommendation: `{}`",
            adaptive.profile_recommendation
        ),
        String::new(),
        "## Sections".to_string(),
    ];
    for (name, section) in sections {
        let status = if section.ok { "PASS" } else { "FAIL" };
        lines.push(format!("- `{name}`: {status} (exit={})", section.exit_code));
    }
    lines.push(String::new());
    lines.push("## Recommendations".to_string());
    for rec in recommendations {
        lines.push(format!(
            "- **{} ({})**: {} \u{2014} {}",
            rec.id, rec.severity, rec.title, rec.recommendation
        ));
    }
    if !adaptive.adaptive_actions.is_empty() {
        lines.push(String::new());
        lines.push("## Adaptive Actions".to_string());
        for action in &adaptive.adaptive_actions {
            lines.push(format!(
                "- `{}={}` ({})",
                action.key, action.value, action.reason
            ));
        }
    }
    let body = format!("{}\n", py::strip(&lines.join("\n")));
    write_text_creating_parents(path, &body)
}

fn write_env(
    path: &Path,
    generated_at: &str,
    adaptive: &AdaptiveIndexes,
    recommendations: &[Recommendation],
) -> Result<(), String> {
    let rec_ids: BTreeSet<&str> = recommendations.iter().map(|r| r.id).collect();
    let mut lines = vec![format!("# generated_at={generated_at}")];
    if rec_ids.contains("HOTPATH_SLOW") {
        lines.extend([
            "HERMES_TOOL_POLICY_PRESET=standard".to_string(),
            "HERMES_TOOL_POLICY_MODE=enforce".to_string(),
            "HERMES_MODEL_CATALOG_GUARD=1".to_string(),
        ]);
    }
    if rec_ids.contains("EVAL_TREND_FAIL") {
        lines.extend([
            "HERMES_MODEL_AUTO_REMEDIATE=1".to_string(),
            "HERMES_REPLAY_ENABLED=1".to_string(),
        ]);
    }
    if [
        "CONTEXTLATTICE_PREFLIGHT_FAIL",
        "CONTEXTLATTICE_UNHEALTHY",
        "CONTEXTLATTICE_PYTHON_FALLBACK",
        "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE",
    ]
    .iter()
    .any(|k| rec_ids.contains(k))
    {
        lines.extend([
            "HERMES_CONTEXTLATTICE_MODE=max".to_string(),
            "HERMES_CONTEXTLATTICE_RETRIEVAL_MODE=deep".to_string(),
            "HERMES_CONTEXTLATTICE_REQUIRE_READBACK=1".to_string(),
        ]);
    }
    if rec_ids.len() == 1 && rec_ids.contains("PERF_STABLE") {
        lines.push("HERMES_PERF_AUTOPILOT_STATUS=stable".to_string());
    }
    lines.push(format!(
        "HERMES_PERF_AUTOPILOT_PROFILE={}",
        adaptive.profile_recommendation
    ));
    for action in &adaptive.adaptive_actions {
        lines.push(format!("{}={}", action.key, action.value));
    }
    write_text_creating_parents(path, &(lines.join("\n") + "\n"))
}

pub fn run(args: PerformanceAutopilotArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    require_existing_dir(&repo_root, "repo root")?;

    let stamp = py::utc_stamp();
    let default_json = repo_root
        .join(".sync-reports")
        .join(format!("performance-autopilot-{stamp}.json"));
    let default_md = repo_root
        .join(".sync-reports")
        .join(format!("performance-autopilot-{stamp}.md"));

    let output_json: PathBuf = if args.output_json.is_empty() {
        default_json
    } else {
        resolve_argument(&py::expanduser(&args.output_json)?)?
    };
    let output_md: PathBuf = if args.output_md.is_empty() {
        default_md
    } else {
        resolve_argument(&py::expanduser(&args.output_md)?)?
    };

    let hotpath = run_section(&args.hotpath_cmd, &repo_root, 6000)?;
    let eval_gate = run_section(&args.eval_cmd, &repo_root, 6000)?;
    let mcp_gate = run_section(&args.mcp_cmd, &repo_root, 6000)?;
    let context_gate = run_section(&args.contextlattice_cmd, &repo_root, 240_000)?;

    let (recommendations, ns, ctx_summary) =
        build_recommendations(&hotpath, &eval_gate, &mcp_gate, &context_gate);
    let ok = hotpath.ok && eval_gate.ok && mcp_gate.ok && context_gate.ok;
    let adaptive = compute_adaptive_indexes(
        &hotpath,
        &eval_gate,
        &mcp_gate,
        &context_gate,
        &recommendations,
        ns,
        &ctx_summary,
    );

    let generated_at = py::utc_isoformat();
    let mut report = Py::dict([
        ("generated_at", Py::from(generated_at.as_str())),
        ("repo_root", Py::from(py::path_str(&repo_root))),
        ("ok", Py::from(ok)),
        (
            "sections",
            Py::dict([
                ("hotpath", hotpath.py_value.clone()),
                ("eval_trend", eval_gate.py_value.clone()),
                ("mcp_stale_recovery", mcp_gate.py_value.clone()),
                ("contextlattice_preflight", context_gate.py_value.clone()),
            ]),
        ),
        (
            "recommendations",
            Py::List(recommendations.iter().map(Recommendation::to_py).collect()),
        ),
        ("performance_index", Py::Float(adaptive.performance_index)),
        ("intelligence_index", Py::Float(adaptive.intelligence_index)),
        ("adaptive_index", Py::Float(adaptive.adaptive_index)),
        (
            "profile_recommendation",
            Py::from(adaptive.profile_recommendation),
        ),
        (
            "adaptive_actions",
            Py::List(
                adaptive
                    .adaptive_actions
                    .iter()
                    .map(AdaptiveAction::to_py)
                    .collect(),
            ),
        ),
        ("report_json", Py::from(py::path_str(&output_json))),
        ("report_markdown", Py::from(py::path_str(&output_md))),
    ]);

    write_json_pretty(&output_json, &report, false)?;
    write_markdown(
        &output_md,
        &generated_at,
        ok,
        &adaptive,
        &[
            ("hotpath", &hotpath),
            ("eval_trend", &eval_gate),
            ("mcp_stale_recovery", &mcp_gate),
            ("contextlattice_preflight", &context_gate),
        ],
        &recommendations,
    )?;

    let mut applied_env_str = String::new();
    if !args.apply_env.is_empty() {
        let env_path = resolve_argument(&py::expanduser(&args.apply_env)?)?;
        write_env(&env_path, &generated_at, &adaptive, &recommendations)?;
        applied_env_str = py::path_str(&env_path);
        report.set("applied_env", Py::from(applied_env_str.as_str()));
    }

    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        let status = if ok { "PASSED" } else { "FAILED" };
        println!("[performance-autopilot] {status}");
        println!(
            "[performance-autopilot] JSON: {}",
            py::path_str(&output_json)
        );
        println!(
            "[performance-autopilot] Markdown: {}",
            py::path_str(&output_md)
        );
        if !args.apply_env.is_empty() {
            println!("[performance-autopilot] Env recommendations: {applied_env_str}");
        }
    }

    if args.strict && !ok {
        return Ok(1);
    }
    Ok(0)
}
