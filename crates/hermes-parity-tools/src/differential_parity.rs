//! Focused CLI-surface parity drift gate against upstream.
//! Port of `scripts/run-differential-parity-gate.py`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use clap::Args;
use regex::Regex;

use crate::py::{self, Py};
use crate::script_helpers::{
    py_splitlines, require_existing_dir, resolve_repo_root, resolve_report_path, run_bash_lc,
    run_process, tail_chars, write_json_pretty,
};

const DEFAULT_FALLBACK_CMD: &str =
    "cargo run --locked -q -p hermes-parity-tools -- generate-test-coverage-audit && \
cargo run --locked -q -p hermes-parity-tools -- generate-test-coverage-audit --check && \
cargo run --locked -q -p hermes-parity-tools -- generate-global-parity-proof --check-ci";

#[derive(Args, Debug)]
pub struct DifferentialParityArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Upstream git ref for parity comparison
    #[arg(long, default_value = "upstream/main")]
    upstream_ref: String,
    /// Local git ref to evaluate (default HEAD)
    #[arg(long, default_value = "HEAD")]
    local_ref: String,
    /// Maximum allowed commits behind upstream
    #[arg(long, default_value_t = 0)]
    max_commits_behind: i64,
    /// Fallback command when CLI surface extraction is unavailable for compared refs
    #[arg(long, default_value = DEFAULT_FALLBACK_CMD)]
    fallback_cmd: String,
    /// Optional explicit JSON report path
    #[arg(long, default_value = "")]
    report_path: String,
    /// Print JSON report
    #[arg(long)]
    json: bool,
}

fn run_git(
    repo_root: &Path,
    args: &[&str],
) -> Result<crate::script_helpers::CaptureResult, String> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push("git".to_string());
    argv.extend(args.iter().map(|s| (*s).to_string()));
    run_process(&argv, repo_root, None)
}

fn git_show_file(repo_root: &Path, git_ref: &str, path: &str) -> Result<Option<String>, String> {
    let spec = format!("{git_ref}:{path}");
    let res = run_git(repo_root, &["show", &spec])?;
    if res.ok {
        Ok(Some(res.stdout))
    } else {
        Ok(None)
    }
}

fn git_show_text_surface(
    repo_root: &Path,
    git_ref: &str,
    paths: &[&str],
) -> Result<Option<String>, String> {
    let mut parts = Vec::new();
    for path in paths {
        if path.ends_with(".rs") {
            if let Some(single) = git_show_file(repo_root, git_ref, path)? {
                parts.push(single);
                continue;
            }
        }
        let listed = run_git(
            repo_root,
            &["ls-tree", "-r", "--name-only", git_ref, "--", path],
        )?;
        if !listed.ok || py::strip(&listed.stdout).is_empty() {
            continue;
        }
        let mut rs_paths: Vec<&str> = py_splitlines(&listed.stdout)
            .into_iter()
            .filter(|line| line.ends_with(".rs"))
            .collect();
        rs_paths.sort();
        for rel in rs_paths {
            if let Some(content) = git_show_file(repo_root, git_ref, rel)? {
                parts.push(content);
            }
        }
    }
    if parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parts.join("\n")))
    }
}

fn rust_fn_block<'a>(source: &'a str, fn_name: &str) -> &'a str {
    let pat = format!(
        r"(?m)^(?:pub\s+)?(?:async\s+)?fn\s+{}\b",
        regex::escape(fn_name)
    );
    let start_re = Regex::new(&pat).expect("valid fn regex");
    let Some(m) = start_re.find(source) else {
        return "";
    };
    let next_re =
        Regex::new(r"(?m)^(?:pub\s+)?(?:async\s+)?fn\s+[A-Za-z0-9_]+").expect("valid fn regex");
    let end = next_re
        .find(&source[m.end()..])
        .map_or(source.len(), |n| m.end() + n.start());
    &source[m.start()..end]
}

fn extract_actions_from_rust_fn(source: &str, fn_name: &str) -> Vec<String> {
    let block = rust_fn_block(source, fn_name);
    if block.is_empty() {
        return Vec::new();
    }
    let unwrap_re = Regex::new(r#"unwrap_or\("([A-Za-z0-9_-]+)"\)"#).expect("valid regex");
    let str_re = Regex::new(r#""([A-Za-z0-9_-]+)""#).expect("valid regex");
    let mut actions = BTreeSet::new();
    for cap in unwrap_re.captures_iter(block) {
        actions.insert(cap[1].to_string());
    }
    for raw in py_splitlines(block) {
        let line = py::strip(raw);
        if !line.contains("=>") {
            continue;
        }
        if !(line.starts_with("Some(") || line.starts_with("None") || line.starts_with('"')) {
            continue;
        }
        for cap in str_re.captures_iter(line) {
            actions.insert(cap[1].to_string());
        }
    }
    actions.into_iter().collect()
}

fn camel_to_kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (idx, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && idx > 0 {
            out.push('-');
        }
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
    }
    out
}

fn extract_top_level_cli_commands(cli_source: &str) -> Vec<String> {
    let re = Regex::new(r"(?m)^\s{4}([A-Z][A-Za-z0-9_]*)\s*(?:\{|,)").expect("valid regex");
    let mut names = BTreeSet::new();
    for cap in re.captures_iter(cli_source) {
        names.insert(camel_to_kebab(&cap[1]));
    }
    names.into_iter().collect()
}

fn extract_slash_commands_from_rust(source: &str) -> Vec<String> {
    let table_re =
        Regex::new(r"pub\s+const\s+SLASH_COMMANDS\s*:[\s\S]*?=\s*&\[(?P<body>[\s\S]*?)\];")
            .expect("valid regex");
    let tuple_re = Regex::new(r#"\(\s*"/([A-Za-z0-9_-]+)"\s*,"#).expect("valid regex");
    let name_re = Regex::new(r#"name:\s*"/([A-Za-z0-9_-]+)""#).expect("valid regex");

    let mut commands = BTreeSet::new();
    if let Some(cap) = table_re.captures(source) {
        if let Some(body) = cap.name("body") {
            for hit in tuple_re.captures_iter(body.as_str()) {
                commands.insert(hit[1].to_string());
            }
        }
    }
    if commands.is_empty() {
        for hit in name_re.captures_iter(source) {
            commands.insert(hit[1].to_string());
        }
    }
    commands.into_iter().collect()
}

fn extract_slash_commands_from_python(source: &str) -> Vec<String> {
    let def_re = Regex::new(r#"CommandDef\("([A-Za-z0-9_-]+)""#).expect("valid regex");
    let alias_re = Regex::new(r"aliases=\(([^)]*)\)").expect("valid regex");
    let qstr_re = Regex::new(r#""([A-Za-z0-9_-]+)""#).expect("valid regex");
    let slash_re = Regex::new(r#"["']/([A-Za-z0-9_-]+)["']"#).expect("valid regex");

    let mut commands = BTreeSet::new();
    let mut current_block = Vec::new();
    let mut in_entry = false;

    for raw in py_splitlines(source) {
        let line = raw.trim_end();
        if !in_entry {
            if line.contains("CommandDef(") {
                in_entry = true;
                current_block = vec![line.to_string()];
            }
        } else {
            current_block.push(line.to_string());
            if py::strip(line).ends_with("),") {
                let block = current_block.join("\n");
                if let Some(cap) = def_re.captures(&block) {
                    commands.insert(cap[1].to_string());
                }
                if let Some(alias_cap) = alias_re.captures(&block) {
                    for cap in qstr_re.captures_iter(&alias_cap[1]) {
                        commands.insert(cap[1].to_string());
                    }
                }
                in_entry = false;
                current_block.clear();
            }
        }
    }

    if commands.is_empty() {
        for hit in slash_re.captures_iter(source) {
            commands.insert(hit[1].to_string());
        }
    }
    commands.into_iter().collect()
}

struct CliSurface {
    git_ref: String,
    surface_kind: &'static str,
    top_level: Vec<String>,
    slash_commands: Vec<String>,
    actions: Vec<(String, Vec<String>)>,
}

impl CliSurface {
    fn to_py(&self) -> Py {
        if self.surface_kind == "python" {
            Py::dict([
                ("ref", Py::from(self.git_ref.as_str())),
                ("surface_kind", Py::from("python")),
                ("slash_commands", Py::strs(&self.slash_commands)),
                ("top_level", Py::List(Vec::new())),
                ("actions", Py::Dict(Vec::new())),
            ])
        } else {
            let actions_dict =
                Py::dict(self.actions.iter().map(|(k, v)| (k.as_str(), Py::strs(v))));
            Py::dict([
                ("ref", Py::from(self.git_ref.as_str())),
                ("surface_kind", Py::from("rust")),
                ("top_level", Py::strs(&self.top_level)),
                ("slash_commands", Py::strs(&self.slash_commands)),
                ("actions", actions_dict),
            ])
        }
    }
}

fn collect_python_cli_surface(
    repo_root: &Path,
    git_ref: &str,
) -> Result<Option<CliSurface>, String> {
    let Some(cli_py) = git_show_file(repo_root, git_ref, "cli.py")? else {
        return Ok(None);
    };
    if cli_py.is_empty() {
        return Ok(None);
    }
    let commands_py = git_show_file(repo_root, git_ref, "hermes_cli/commands.py")?;
    let gateway_py = git_show_file(repo_root, git_ref, "gateway/commands.py")?;
    let parts: Vec<String> = [commands_py, gateway_py, Some(cli_py)]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    let command_source = parts.join("\n");
    Ok(Some(CliSurface {
        git_ref: git_ref.to_string(),
        surface_kind: "python",
        slash_commands: extract_slash_commands_from_python(&command_source),
        top_level: Vec::new(),
        actions: Vec::new(),
    }))
}

fn collect_cli_surface(repo_root: &Path, git_ref: &str) -> Result<Option<CliSurface>, String> {
    let cli_rs = git_show_file(repo_root, git_ref, "crates/hermes-cli/src/cli.rs")?;
    let main_rs = git_show_file(repo_root, git_ref, "crates/hermes-cli/src/main.rs")?;
    let commands_rs = git_show_text_surface(
        repo_root,
        git_ref,
        &[
            "crates/hermes-cli/src/commands.rs",
            "crates/hermes-cli/src/commands",
        ],
    )?;

    let (Some(cli_rs), Some(main_rs), Some(commands_rs)) = (cli_rs, main_rs, commands_rs) else {
        return collect_python_cli_surface(repo_root, git_ref);
    };
    if cli_rs.is_empty() || main_rs.is_empty() || commands_rs.is_empty() {
        return collect_python_cli_surface(repo_root, git_ref);
    }

    let fn_map: &[(&str, &str, &str)] = &[
        ("tools", "main", "run_tools"),
        ("gateway", "main", "run_gateway"),
        ("auth", "main", "run_auth"),
        ("cron", "main", "run_cron"),
        ("webhook", "main", "run_webhook"),
        ("profile", "main", "run_profile"),
        ("memory", "commands", "handle_cli_memory"),
        ("mcp", "commands", "handle_cli_mcp"),
        ("skills", "commands", "handle_cli_skills"),
    ];
    let mut actions = Vec::with_capacity(fn_map.len());
    for (cmd, src_key, fn_name) in fn_map {
        let src = if *src_key == "main" {
            &main_rs
        } else {
            &commands_rs
        };
        actions.push((
            (*cmd).to_string(),
            extract_actions_from_rust_fn(src, fn_name),
        ));
    }

    Ok(Some(CliSurface {
        git_ref: git_ref.to_string(),
        surface_kind: "rust",
        top_level: extract_top_level_cli_commands(&cli_rs),
        slash_commands: extract_slash_commands_from_rust(&commands_rs),
        actions,
    }))
}

struct DriftSummary {
    has_drift: bool,
    surface_kind: String,
    missing_top_level: Vec<String>,
    extra_top_level: Vec<String>,
    missing_slash: Vec<String>,
    extra_slash: Vec<String>,
    actions: BTreeMap<String, (Vec<String>, Vec<String>)>,
    missing_action_count: usize,
}

impl DriftSummary {
    fn to_py(&self) -> Py {
        let actions_dict = Py::dict(self.actions.iter().map(|(k, (missing, extra))| {
            (
                k.as_str(),
                Py::dict([
                    ("missing_in_local", Py::strs(missing)),
                    ("extra_in_local", Py::strs(extra)),
                ]),
            )
        }));
        Py::dict([
            ("has_drift", Py::from(self.has_drift)),
            ("surface_kind", Py::from(self.surface_kind.as_str())),
            (
                "top_level",
                Py::dict([
                    ("missing_in_local", Py::strs(&self.missing_top_level)),
                    ("extra_in_local", Py::strs(&self.extra_top_level)),
                ]),
            ),
            (
                "slash_commands",
                Py::dict([
                    ("missing_in_local", Py::strs(&self.missing_slash)),
                    ("extra_in_local", Py::strs(&self.extra_slash)),
                ]),
            ),
            ("actions", actions_dict),
            ("missing_action_count", Py::from(self.missing_action_count)),
        ])
    }
}

fn compute_cli_surface_drift(local: &CliSurface, upstream: &CliSurface) -> DriftSummary {
    let local_slash: BTreeSet<&String> = local.slash_commands.iter().collect();
    let upstream_slash: BTreeSet<&String> = upstream.slash_commands.iter().collect();
    let missing_slash: Vec<String> = upstream_slash
        .difference(&local_slash)
        .map(|s| (*s).clone())
        .collect();
    let extra_slash: Vec<String> = local_slash
        .difference(&upstream_slash)
        .map(|s| (*s).clone())
        .collect();

    if local.surface_kind != upstream.surface_kind {
        return DriftSummary {
            has_drift: !missing_slash.is_empty(),
            surface_kind: format!("{}_vs_{}", local.surface_kind, upstream.surface_kind),
            missing_top_level: Vec::new(),
            extra_top_level: Vec::new(),
            missing_slash,
            extra_slash,
            actions: BTreeMap::new(),
            missing_action_count: 0,
        };
    }

    let local_top: BTreeSet<&String> = local.top_level.iter().collect();
    let upstream_top: BTreeSet<&String> = upstream.top_level.iter().collect();
    let missing_top_level: Vec<String> = upstream_top
        .difference(&local_top)
        .map(|s| (*s).clone())
        .collect();
    let extra_top_level: Vec<String> = local_top
        .difference(&upstream_top)
        .map(|s| (*s).clone())
        .collect();

    let all_commands: BTreeSet<&String> = local
        .actions
        .iter()
        .map(|(k, _)| k)
        .chain(upstream.actions.iter().map(|(k, _)| k))
        .collect();
    let mut actions = BTreeMap::new();
    let mut missing_total = 0usize;
    for cmd in all_commands {
        let local_actions: BTreeSet<&String> = local
            .actions
            .iter()
            .find(|(k, _)| k == cmd)
            .into_iter()
            .flat_map(|(_, v)| v.iter())
            .collect();
        let upstream_actions: BTreeSet<&String> = upstream
            .actions
            .iter()
            .find(|(k, _)| k == cmd)
            .into_iter()
            .flat_map(|(_, v)| v.iter())
            .collect();
        let missing: Vec<String> = upstream_actions
            .difference(&local_actions)
            .map(|s| (*s).clone())
            .collect();
        let extra: Vec<String> = local_actions
            .difference(&upstream_actions)
            .map(|s| (*s).clone())
            .collect();
        if !missing.is_empty() || !extra.is_empty() {
            missing_total += missing.len();
            actions.insert(cmd.clone(), (missing, extra));
        }
    }

    let has_drift = !missing_top_level.is_empty() || missing_total > 0;
    DriftSummary {
        has_drift,
        surface_kind: "rust".to_string(),
        missing_top_level,
        extra_top_level,
        missing_slash,
        extra_slash,
        actions,
        missing_action_count: missing_total,
    }
}

fn git_ref_exists(repo_root: &Path, git_ref: &str) -> Result<bool, String> {
    Ok(run_git(repo_root, &["rev-parse", "--verify", git_ref])?.ok)
}

fn ahead_behind(
    repo_root: &Path,
    local_ref: &str,
    upstream_ref: &str,
) -> Result<(i64, i64), String> {
    let range = format!("{local_ref}...{upstream_ref}");
    let res = run_git(repo_root, &["rev-list", "--left-right", "--count", &range])?;
    if !res.ok {
        return Ok((0, 0));
    }
    let parts: Vec<&str> = res.stdout.split_whitespace().collect();
    if parts.len() != 2 {
        return Ok((0, 0));
    }
    match (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
        (Ok(ahead), Ok(behind)) => Ok((ahead, behind)),
        _ => Ok((0, 0)),
    }
}

pub fn run(args: DifferentialParityArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    require_existing_dir(&repo_root, "repo root")?;

    let report_path =
        resolve_report_path(&repo_root, &args.report_path, "differential-parity-gate")?;

    let local_exists = git_ref_exists(&repo_root, &args.local_ref)?;
    let upstream_exists = git_ref_exists(&repo_root, &args.upstream_ref)?;
    if !local_exists || !upstream_exists {
        let report = Py::dict([
            ("generated_at", Py::from(py::utc_isoformat())),
            ("ok", Py::from(false)),
            ("reason", Py::from("missing_git_ref")),
            ("local_ref", Py::from(args.local_ref.as_str())),
            ("upstream_ref", Py::from(args.upstream_ref.as_str())),
            ("local_ref_exists", Py::from(local_exists)),
            ("upstream_ref_exists", Py::from(upstream_exists)),
        ]);
        write_json_pretty(&report_path, &report, false)?;
        if args.json {
            println!("{}", py::dumps(&report, Some(2), false));
        } else {
            println!("[differential-parity] FAILED (missing ref)");
            println!(
                "[differential-parity] Report: {}",
                py::path_str(&report_path)
            );
        }
        return Ok(1);
    }

    let local_surface = collect_cli_surface(&repo_root, &args.local_ref)?;
    let upstream_surface = collect_cli_surface(&repo_root, &args.upstream_ref)?;
    let (Some(local_surface), Some(upstream_surface)) = (local_surface, upstream_surface) else {
        let fb = run_bash_lc(&args.fallback_cmd, &repo_root)?;
        let fallback_ok = fb.ok;
        let fallback_py = Py::dict([
            ("command", Py::from(args.fallback_cmd.as_str())),
            ("exit_code", Py::Int(i128::from(fb.exit_code))),
            ("ok", Py::from(fallback_ok)),
            ("elapsed_ms", Py::Int(fb.elapsed_ms)),
            ("stdout_tail", Py::from(tail_chars(&fb.stdout, 4000))),
            ("stderr_tail", Py::from(tail_chars(&fb.stderr, 4000))),
        ]);
        let mut report = Py::dict([
            ("generated_at", Py::from(py::utc_isoformat())),
            ("ok", Py::from(fallback_ok)),
            ("reason", Py::from("cli_surface_unavailable_fallback")),
            ("local_ref", Py::from(args.local_ref.as_str())),
            ("upstream_ref", Py::from(args.upstream_ref.as_str())),
            ("fallback", fallback_py),
        ]);
        write_json_pretty(&report_path, &report, false)?;
        report.set("report_path", Py::from(py::path_str(&report_path)));
        if args.json {
            println!("{}", py::dumps(&report, Some(2), false));
        } else {
            let status = if fallback_ok { "PASSED" } else { "FAILED" };
            println!("[differential-parity] {status} (fallback)");
            println!(
                "[differential-parity] Report: {}",
                py::path_str(&report_path)
            );
        }
        return Ok(i32::from(!fallback_ok));
    };

    let drift = compute_cli_surface_drift(&local_surface, &upstream_surface);
    let (ahead, behind) = ahead_behind(&repo_root, &args.local_ref, &args.upstream_ref)?;
    let comparable_surface = drift.surface_kind == "rust";
    let commits_gate_ok = (!comparable_surface) || (behind <= args.max_commits_behind.max(0));
    let gate_ok = !drift.has_drift && commits_gate_ok;

    let missing_top = drift.missing_top_level.len();
    let missing_actions = drift.missing_action_count;
    let extra_top = drift.extra_top_level.len();

    let mut report = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("repo_root", Py::from(py::path_str(&repo_root))),
        ("ok", Py::from(gate_ok)),
        ("local_ref", Py::from(args.local_ref.as_str())),
        ("upstream_ref", Py::from(args.upstream_ref.as_str())),
        (
            "commits",
            Py::dict([
                ("ahead", Py::Int(i128::from(ahead))),
                ("behind", Py::Int(i128::from(behind))),
                (
                    "max_commits_behind",
                    Py::Int(i128::from(args.max_commits_behind)),
                ),
                ("behind_gate_enforced", Py::from(comparable_surface)),
                ("behind_gate_ok", Py::from(commits_gate_ok)),
            ]),
        ),
        ("drift", drift.to_py()),
        ("local_surface", local_surface.to_py()),
        ("upstream_surface", upstream_surface.to_py()),
        (
            "summary",
            Py::dict([
                ("missing_top_level", Py::from(missing_top)),
                ("missing_actions", Py::from(missing_actions)),
                ("extra_top_level", Py::from(extra_top)),
            ]),
        ),
    ]);

    write_json_pretty(&report_path, &report, false)?;
    report.set("report_path", Py::from(py::path_str(&report_path)));

    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        let status = if gate_ok { "PASSED" } else { "FAILED" };
        println!(
            "[differential-parity] {status} (missing_top={missing_top} missing_actions={missing_actions} behind={behind})"
        );
        println!(
            "[differential-parity] Report: {}",
            py::path_str(&report_path)
        );
    }

    Ok(i32::from(!gate_ok))
}
