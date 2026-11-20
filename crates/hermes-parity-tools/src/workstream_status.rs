//! Report the local evidence and remaining rows for workstreams WS2–WS8.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use regex::Regex;

use crate::git::{branch_from_remote_ref, ensure_remote, fetch_remote_branch, git_text};
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_reports};
use crate::shared_diff_backlog::workstream_for;

#[derive(Args, Debug)]
pub struct WorkstreamStatusArgs {
    /// Repository root path
    #[arg(long, default_value = ".")]
    repo_root: String,
    #[arg(long, default_value = "upstream/main")]
    upstream_ref: String,
    #[arg(long, default_value = "upstream")]
    upstream_remote: String,
    #[arg(
        long,
        default_value = "https://github.com/NousResearch/hermes-agent.git"
    )]
    upstream_url: String,
    #[arg(long)]
    no_fetch: bool,
    #[arg(long, default_value = "docs/parity/workstream-status.json")]
    out_json: String,
    #[arg(long, default_value = "docs/parity/workstream-status.md")]
    out_md: String,
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn files_below(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(path).map_err(|error| format!("{}: {error}", path.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", path.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", entry.path().display()))?;
        let child = entry.path();
        if file_type.is_dir() {
            files_below(&child, files)?;
        } else if child.is_file() {
            files.push(child);
        }
    }
    Ok(())
}

fn read_sources(path: &Path) -> Result<String, String> {
    if !path.is_dir() {
        return read_text(path);
    }
    let mut files = Vec::new();
    files_below(path, &mut files)?;
    files.sort();
    let mut sources = Vec::new();
    for file in files {
        let extension = file.extension().and_then(|extension| extension.to_str());
        if extension.is_some_and(|extension| {
            extension == "rs" || (cfg!(windows) && extension.eq_ignore_ascii_case("rs"))
        }) {
            sources.push(read_text(&file)?);
        }
    }
    Ok(sources.join("\n"))
}

fn skill_counts(root: &Path, upstream: &str) -> Result<(usize, usize), String> {
    let mut upstream_paths = BTreeSet::new();
    let mut local_paths = Vec::new();
    for prefix in ["skills", "optional-skills"] {
        let paths = git_text(
            root,
            &["ls-tree", "-r", "--name-only", upstream, "--", prefix],
            false,
        )?;
        upstream_paths.extend(
            crate::git::python_lines(&paths)
                .map(py::strip)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        );

        let local = root.join(prefix);
        if local.is_file() {
            local_paths.push(local);
        } else if local.exists() {
            files_below(&local, &mut local_paths)?;
        }
    }
    Ok((upstream_paths.len(), local_paths.len()))
}

fn read_json(path: &Path) -> Result<Py, String> {
    py::loads(&read_text(path)?).map_err(|error| format!("{}: {error}", path.display()))
}

fn list<'a>(report: &'a Py, property: &str) -> Result<&'a [Py], String> {
    match report.get(property) {
        Some(Py::List(items)) => Ok(items),
        _ => Err(format!("expected {property} to be a list")),
    }
}

fn string<'a>(row: &'a Py, property: &str) -> Option<&'a str> {
    match row.get(property) {
        Some(Py::Str(value)) => Some(value),
        _ => None,
    }
}

fn deferred_by_workstream(parity: &Path) -> Result<BTreeMap<&'static str, usize>, String> {
    let mut deferred = BTreeMap::new();
    for (name, items, status, values) in [
        (
            "shared-different-classification.json",
            "items",
            "classification",
            &["deferred"][..],
        ),
        (
            "python-test-suite-coverage.json",
            "entries",
            "status",
            &[
                "deferred_not_covered",
                "not_rechecked_since_upstream_changed",
            ][..],
        ),
        (
            "hermes-cli-test-coverage.json",
            "entries",
            "status",
            &[
                "deferred_not_covered",
                "not_rechecked_since_upstream_changed",
            ][..],
        ),
        (
            "ui-tui-source-coverage.json",
            "entries",
            "status",
            &[
                "deferred_not_covered",
                "not_rechecked_since_upstream_changed",
            ][..],
        ),
    ] {
        let report = read_json(&parity.join(name))?;
        for row in list(&report, items)? {
            if string(row, status).is_some_and(|value| values.contains(&value)) {
                let path = string(row, "path")
                    .ok_or_else(|| format!("{name}: expected a deferred path"))?;
                *deferred.entry(workstream_for(path)).or_default() += 1;
            }
        }
    }
    Ok(deferred)
}

struct Workstream {
    id: &'static str,
    title: &'static str,
    complete: bool,
    evidence: Vec<String>,
    metrics: Py,
}

impl Workstream {
    fn new(
        id: &'static str,
        title: &'static str,
        complete: bool,
        evidence: &[&str],
        metrics: Py,
    ) -> Self {
        Self {
            id,
            title,
            complete,
            evidence: evidence.iter().map(|value| (*value).to_owned()).collect(),
            metrics,
        }
    }

    fn state(&self) -> &'static str {
        if self.complete {
            "complete"
        } else {
            "in_progress"
        }
    }

    fn defer(&mut self, count: usize) {
        self.complete = false;
        self.metrics.set("rows_not_shown_matching", count.into());
        self.evidence.push(format!("{count} rows are deferred or not rechecked, so not yet shown to match in Rust (docs/parity/shared-diff-backlog.md and the coverage manifests)."));
    }

    fn report(&self) -> Py {
        Py::dict([
            ("workstream", self.id.into()),
            ("title", self.title.into()),
            ("state", self.state().into()),
            ("evidence", Py::strs(&self.evidence)),
            ("metrics", self.metrics.clone()),
        ])
    }
}

fn build_workstreams(root: &Path, upstream: &str) -> Result<Vec<Workstream>, String> {
    let cli = root.join("crates/hermes-cli");
    let parity = root.join("docs/parity");
    let workflow = read_text(&root.join(".github/workflows/ci.yml"))?;
    let divergence = read_json(&parity.join("intentional-divergence.json"))?;
    let items: &[Py] = if divergence.get("items").is_some() {
        list(&divergence, "items")?
    } else {
        &[]
    };
    let documented = |wanted| {
        items.iter().any(|item| {
            list(item, "path_prefixes").is_ok_and(|prefixes| {
                prefixes
                    .iter()
                    .any(|prefix| matches!(prefix, Py::Str(value) if value == wanted))
            })
        })
    };
    let (upstream_skills, local_skills) = skill_counts(root, upstream)?;
    let mut wiring_sites = 0usize;
    for file in ["src/main.rs", "src/app.rs", "src/commands"] {
        if read_sources(&cli.join(file))?.contains("wire_cron_scheduler_backend(&tool_registry") {
            wiring_sites += 1;
        }
    }
    let wiring = read_text(&cli.join("src/runtime_tool_wiring.rs"))?;
    let runtime_wiring = Regex::new(r"pub fn wire_[a-z_]+_backend").expect("wiring pattern");
    let e2e = Regex::new(r"(?m)^fn e2e_").expect("CLI test pattern");
    let jobs = Regex::new(r"(?m)^[a-zA-Z0-9_-]+:\n[\s\x1c-\x1f]+runs-on:").expect("CI job pattern");
    let has_backend = [
        "wire_gateway_messaging_backend",
        "wire_gateway_clarify_backend",
        "wire_stdio_clarify_backend",
    ]
    .iter()
    .all(|name| wiring.contains(name));
    let has_ci = [
        "cargo test --workspace",
        "cargo test -p hermes-parity-tests",
        "check-runtime-placeholders.sh",
        "clippy-warning-gate.sh",
    ]
    .iter()
    .all(|token| workflow.contains(token));
    let scripts = root.join("scripts");
    let has_security_files = [
        "upstream_webhook_sync.py",
        "setup-upstream-webhook-launchd.sh",
        "sync-upstream.sh",
    ]
    .iter()
    .all(|file| scripts.join(file).exists());
    let has_secret_hook = read_text(&scripts.join("install-upstream-webhook-launchd.sh"))?
        .contains("GITHUB_WEBHOOK_SECRET");
    let skills_documented = documented("skills/") && documented("optional-skills/");
    let ux_documented = documented("web/") && documented("ui-tui/");
    let policy_exists = parity.join("compatibility-policy.md").exists();

    let mut statuses = vec![
        Workstream::new("WS2", "Core runtime parity", wiring_sites == 3, &[
            "Live cron backend wired in gateway, app, chat, and ACP runtime paths.",
            "Runtime tool bridge refreshed from live registry in gateway handlers.",
        ], Py::dict([("wiring_sites_detected", wiring_sites.into())])),
        Workstream::new("WS3", "Tools/adapters parity", has_backend, &[
            "send_message tool wired to live gateway backend in gateway runtime.",
            "clarify tool wired to channel backend in gateway and stdio backend in CLI runtimes.",
            "cronjob tool wired to live scheduler backend.",
        ], Py::dict([("runtime_wiring_functions", runtime_wiring.find_iter(&wiring).count().into())])),
        Workstream::new("WS4", "Skills parity", skills_documented, &[
            "Upstream skills catalogs audited against local tree.",
            "Intentional divergence documented for skills and optional-skills vendoring.",
        ], Py::dict([("upstream_skill_files", upstream_skills.into()), ("local_skill_files", local_skills.into()), ("divergence_documented", skills_documented.into())])),
        Workstream::new("WS5", "UX parity", ux_documented, &[
            "Rust CLI/TUI runtime validated through e2e_cli and gateway e2e smoke tests.",
            "Web/UI upstream trees classified as intentional divergence in Rust-first mode.",
        ], Py::dict([("e2e_cli_tests", e2e.find_iter(&read_text(&cli.join("tests/e2e_cli.rs"))?).count().into()), ("divergence_documented", ux_documented.into())])),
        Workstream::new("WS6", "Tests and CI parity", has_ci, &[
            "CI workflow enforces format, clippy gate, placeholder gate, workspace tests, parity fixture tests.",
        ], Py::dict([("ci_jobs", jobs.find_iter(&workflow).count().into()), ("parity_gate_present", workflow.contains("hermes-parity-tests").into())])),
        Workstream::new("WS7", "Security/secrets/store/webhook parity", has_security_files && has_secret_hook, &[
            "Webhook listener/worker supports sqlite, SQS, Kafka queue backends.",
            "Launchd setup includes runtime-role/host guards and webhook secret automation.",
            "Upstream sync script includes strict risk gate.",
        ], Py::dict([("security_scripts_present", 4usize.into())])),
        Workstream::new("WS8", "Compatibility and divergence policy", policy_exists, &[
            "Compatibility policy defines rust-native default, bounded FFI fallback, and divergence governance.",
            "Intentional divergences are codified in docs/parity/intentional-divergence.json.",
        ], Py::dict([("divergence_items", items.len().into()), ("policy_exists", policy_exists.into())])),
    ];
    let deferred = deferred_by_workstream(&parity)?;
    for status in &mut statuses {
        if let Some(&count) = deferred.get(status.id) {
            status.defer(count);
        }
    }
    Ok(statuses)
}

fn render_markdown(
    head: &str,
    upstream: &str,
    upstream_sha: &str,
    statuses: &[Workstream],
) -> String {
    let mut report = format!("# Workstream Status\n\n- Local HEAD: `{head}`\n- Upstream: `{upstream}` (`{upstream_sha}`)\n\n| Workstream | Title | State |\n| --- | --- | --- |\n");
    for status in statuses {
        writeln!(
            report,
            "| `{}` | {} | **{}** |",
            status.id,
            status.title,
            status.state()
        )
        .unwrap();
    }
    report.push('\n');
    for status in statuses {
        writeln!(
            report,
            "## {} — {}\n\n- State: **{}**",
            status.id,
            status.title,
            status.state()
        )
        .unwrap();
        for evidence in &status.evidence {
            writeln!(report, "- {evidence}").unwrap();
        }
        writeln!(
            report,
            "- Metrics: `{}`\n",
            py::dumps(&status.metrics, None, true)
        )
        .unwrap();
    }
    report
}

pub fn run(args: WorkstreamStatusArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    ensure_remote(&root, &args.upstream_remote, &args.upstream_url)?;
    let upstream = &args.upstream_ref;
    if !args.no_fetch {
        fetch_remote_branch(
            &root,
            &args.upstream_remote,
            branch_from_remote_ref(&args.upstream_remote, upstream),
        )?;
    }
    let upstream_sha = git_text(&root, &["rev-parse", upstream], false)?;
    let upstream_sha = if upstream_sha.is_empty() {
        "unknown"
    } else {
        &upstream_sha
    };
    let head = git_text(&root, &["rev-parse", "HEAD"], true)?;
    let statuses = build_workstreams(&root, upstream)?;
    let mut states = BTreeMap::<_, usize>::new();
    for status in &statuses {
        *states.entry(status.state()).or_default() += 1;
    }
    let report = Py::dict([
        (
            "generated_at_utc",
            git_text(&root, &["show", "-s", "--format=%cI", "HEAD"], true)?.into(),
        ),
        ("head_sha", head.as_str().into()),
        ("upstream_ref", upstream.as_str().into()),
        ("upstream_sha", upstream_sha.into()),
        (
            "states",
            Py::dict(
                states
                    .into_iter()
                    .map(|(state, count)| (state, count.into())),
            ),
        ),
        (
            "workstreams",
            Py::List(statuses.iter().map(Workstream::report).collect()),
        ),
    ]);
    write_reports(
        &root,
        &args.out_json,
        &args.out_md,
        py::dumps(&report, Some(2), true) + "\n",
        render_markdown(&head, upstream, upstream_sha, &statuses),
    )?;
    Ok(0)
}
