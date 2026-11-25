//! Cargo surface audit, clippy allowlist gate and targeted Rust suite runners.
//! Ports of `scripts/clippy-warning-gate.sh`, `scripts/audit-cargo-build-surface.sh`,
//! `scripts/run-deterministic-replay-suite.sh`, `scripts/run-openhuman-p0-p1-gate.sh` and
//! `scripts/run-ultra-feature-benchmarks.sh`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::Args;
use serde_json::Value;

use crate::py;
use crate::script_helpers::{
    collect_rs_files, py_splitlines, read_utf8, resolve_repo_root, run_process,
    write_text_creating_parents,
};

#[derive(Args, Debug)]
pub struct RepoRootOnlyArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
}

#[derive(Args, Debug)]
pub struct ClippyWarningGateArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Path to allowlist file
    #[arg(long, default_value = ".ci/clippy-allowlist.txt")]
    allowlist: String,
    /// Refresh allowlist from current clippy warnings
    #[arg(long, overrides_with = "check")]
    update: bool,
    /// Validate current warnings against allowlist (default)
    #[arg(long, overrides_with = "update")]
    check: bool,
    /// Extra cargo clippy arguments (after `--`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    extra_args: Vec<String>,
}

fn parse_clippy_warning_entries(stdout: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    for raw_line in py_splitlines(stdout) {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(root) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if root.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        let Some(msg) = root.get("message") else {
            continue;
        };
        if msg.get("level").and_then(Value::as_str) != Some("warning") {
            continue;
        }
        let target = root
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("unknown_target");
        let spans = msg.get("spans").and_then(Value::as_array);
        let primary_span = spans.and_then(|list| {
            list.iter()
                .find(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true))
                .or_else(|| list.first())
        });
        let file = primary_span
            .and_then(|s| s.get("file_name"))
            .and_then(Value::as_str)
            .unwrap_or("unknown_file");
        let lint = msg
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                let first = msg
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .split(':')
                    .next()
                    .unwrap_or("");
                format!("rustc:{first}")
            });
        seen.insert(format!("{target}|{lint}|{file}"));
    }
    seen
}

pub fn run_clippy_warning_gate(args: ClippyWarningGateArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let _ = args.check;

    println!("[clippy-gate] collecting warnings...");
    let output = Command::new("cargo")
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--message-format=json",
        ])
        .args(&args.extra_args)
        .current_dir(&repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("failed to run cargo clippy: {e}"))?;

    if !output.status.success() {
        return Ok(output.status.code().unwrap_or(1));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("clippy JSON output is not UTF-8: {e}"))?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let seen = parse_clippy_warning_entries(&stdout);

    let allow_arg = Path::new(&args.allowlist);
    let allowlist_path = if allow_arg.is_absolute() {
        allow_arg.to_path_buf()
    } else {
        py::join(&repo_root, &args.allowlist)
    };

    if args.update {
        let body = if seen.is_empty() {
            String::new()
        } else {
            seen.iter().cloned().collect::<Vec<_>>().join("\n") + "\n"
        };
        write_text_creating_parents(&allowlist_path, &body)?;
        println!(
            "[clippy-gate] updated {} ({} entries)",
            args.allowlist,
            seen.len()
        );
        return Ok(0);
    }

    if !allowlist_path.is_file() {
        eprintln!("[clippy-gate] allowlist not found: {}", args.allowlist);
        eprintln!("[clippy-gate] run: scripts/clippy-warning-gate.sh --update");
        return Ok(1);
    }

    let allow_raw = read_utf8(&allowlist_path)?;
    let mut allow = BTreeSet::new();
    for line in py_splitlines(&allow_raw) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        allow.insert(line.to_string());
    }

    let new_entries: Vec<&String> = seen.difference(&allow).collect();
    let stale_entries: Vec<&String> = allow.difference(&seen).collect();

    println!(
        "[clippy-gate] seen={} allowlist={} new={} stale={}",
        seen.len(),
        allow.len(),
        new_entries.len(),
        stale_entries.len()
    );

    if !new_entries.is_empty() {
        eprintln!("[clippy-gate] new warnings detected:");
        for entry in new_entries {
            eprintln!("  + {entry}");
        }
        return Ok(1);
    }

    if !stale_entries.is_empty() {
        println!("[clippy-gate] stale allowlist entries (safe to prune via --update):");
        for entry in stale_entries {
            println!("  - {entry}");
        }
    }

    println!("[clippy-gate] pass");
    Ok(0)
}

fn cargo_tree_block(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let mut argv = vec!["cargo".to_string(), "tree".to_string()];
    argv.extend(args.iter().map(|s| (*s).to_string()));
    let res = run_process(&argv, repo_root, None)?;
    if !res.ok {
        return Err(format!(
            "cargo {} failed (exit {}): {}",
            args.join(" "),
            res.exit_code,
            res.stderr
        ));
    }
    Ok(res.stdout)
}

pub fn run_audit_cargo_build_surface(args: RepoRootOnlyArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let cli_manifest = repo_root.join("crates/hermes-cli/Cargo.toml");
    let manifest_raw = read_utf8(&cli_manifest)?;

    let meta_res = run_process(
        &[
            "cargo".to_string(),
            "metadata".to_string(),
            "--format-version=1".to_string(),
            "--no-deps".to_string(),
        ],
        &repo_root,
        None,
    )?;
    if !meta_res.ok {
        return Err(format!("cargo metadata failed: {}", meta_res.stderr));
    }
    let meta: Value = serde_json::from_str(&meta_res.stdout)
        .map_err(|e| format!("failed to parse cargo metadata: {e}"))?;
    let target_dir = meta
        .get("target_directory")
        .and_then(Value::as_str)
        .unwrap_or("");

    let mut bins = Vec::new();
    let mut in_bin = false;
    let mut bin_name = String::new();
    let mut direct_deps = Vec::new();
    let mut in_deps = false;
    let mut in_gateway = false;
    let mut gateway_feature_count = 0usize;

    for line in py_splitlines(&manifest_raw) {
        if line.starts_with("[[bin]]") {
            in_bin = true;
            bin_name.clear();
            continue;
        }
        if in_bin {
            if let Some(rest) = line.strip_prefix("name = ") {
                bin_name = rest.trim_matches('"').to_string();
            } else if let Some(rest) = line.strip_prefix("path = ") {
                let path = rest.trim_matches('"').to_string();
                bins.push((bin_name.clone(), path));
                in_bin = false;
            }
        }
        if line.starts_with("[dependencies]") {
            in_deps = true;
        } else if line.starts_with('[') && in_deps {
            in_deps = false;
        } else if in_deps && line.starts_with("hermes-") {
            if let Some(dep) = line.split_whitespace().next() {
                direct_deps.push(dep.to_string());
            }
        }

        if line.starts_with("gateway-adapters-all = [") {
            in_gateway = true;
            continue;
        }
        if in_gateway {
            if line.starts_with(']') {
                in_gateway = false;
            } else {
                let mut rest = line;
                while let Some(start) = rest.find("\"gateway-") {
                    let after = &rest[start + 9..];
                    if let Some(end) = after.find('"') {
                        gateway_feature_count += 1;
                        rest = &after[end + 1..];
                    } else {
                        break;
                    }
                }
            }
        }
    }

    println!("# Hermes Cargo Build Surface Audit\n");
    println!("## Workspace\n");
    println!("- Root: `{}`", py::path_str(&repo_root));
    println!("- Cargo target directory: `{target_dir}`\n");
    println!("## hermes-cli Targets\n");
    for (name, path) in &bins {
        println!("- `{name}` -> `{path}`");
    }
    println!();
    println!("## hermes-cli Direct Internal Dependencies\n");
    for dep in &direct_deps {
        println!("- `{dep}`");
    }
    println!();
    println!("## Gateway Adapter Feature Surface\n");
    println!("- Default `gateway-adapters-all` feature count: `{gateway_feature_count}`");
    println!(
        "- Narrow gateway build path: `cargo check -p hermes-cli --bin hermes-agent-ultra --no-default-features`\n"
    );

    let trees: &[(&str, &[&str])] = &[
        (
            "hermes-cli --edges normal --depth 1",
            &["-p", "hermes-cli", "--edges", "normal", "--depth", "1"],
        ),
        (
            "hermes-cli --no-default-features --edges normal --depth 1",
            &[
                "-p",
                "hermes-cli",
                "--no-default-features",
                "--edges",
                "normal",
                "--depth",
                "1",
            ],
        ),
        (
            "hermes-cli-ui --edges normal --depth 1",
            &["-p", "hermes-cli-ui", "--edges", "normal", "--depth", "1"],
        ),
        (
            "hermes-tool-planning --edges normal --depth 1",
            &[
                "-p",
                "hermes-tool-planning",
                "--edges",
                "normal",
                "--depth",
                "1",
            ],
        ),
        (
            "hermes-parity-tests --edges dev --depth 1",
            &[
                "-p",
                "hermes-parity-tests",
                "--edges",
                "dev",
                "--depth",
                "1",
            ],
        ),
        (
            "hermes-source-parity-tests --edges all --depth 1",
            &[
                "-p",
                "hermes-source-parity-tests",
                "--edges",
                "all",
                "--depth",
                "1",
            ],
        ),
        (
            "hermes-protocol-parity-tests --edges dev --depth 1",
            &[
                "-p",
                "hermes-protocol-parity-tests",
                "--edges",
                "dev",
                "--depth",
                "1",
            ],
        ),
    ];

    for (heading, cmd_args) in trees {
        println!("## cargo tree: {heading}\n");
        println!("```text");
        let out = cargo_tree_block(&repo_root, cmd_args)?;
        print!("{out}");
        if !out.ends_with('\n') {
            println!();
        }
        println!("```\n");
    }

    println!("## Interpretation\n");
    println!("- `hermes-cli` is currently the invalidation root for wrappers, the main runtime binary, TUI/clipboard UI dependencies, cron, ACP, MCP, tools, skills, and telemetry.");
    println!("- Gateway adapter features are now behind explicit `gateway-*` CLI features. Default builds keep `gateway-adapters-all` for behavior compatibility, while `--no-default-features` checks compile the gateway core without adapter modules.");
    println!("- Slash-command rendering, autocomplete ranking, alias canonicalization, command catalog presentation, and gateway/tool preview rendering now have a narrow presentation home in `hermes-cli-ui`.");
    println!("- Runtime platform tool-planning policy now has a narrower home in `hermes-tool-planning`; `hermes-cli::platform_toolsets` remains as an explicit compatibility re-export.");
    println!("- Provider/auth routing now has a narrower home in `hermes-provider-runtime`; agent configuration, query-mode provider/model/env/tool policy, model remediation, noninteractive agent-loop wiring, reply extraction, and runtime prompt reformulation policy now have a narrower home in `hermes-app-runtime`.");
    println!("- Source/governance parity now lives in `hermes-source-parity-tests`, so command-contract checks avoid `hermes-cli`, `clap`, fixture-harness runtime crates, and protocol stack crates.");
    println!("- Protocol differential parity now lives in `hermes-protocol-parity-tests`, isolating ACP/MCP/gateway/tool dependencies to tests that need them.");
    println!("- Broader terminal UI, checklist, theme, and clipboard extraction should wait for a dedicated TUI crate boundary.");
    println!("- Parity tests that only validate provider, auth, app-runtime, or command contracts should keep moving to narrower crates instead of pulling the full CLI binary surface.");
    Ok(0)
}

fn run_cargo_inherit(
    repo_root: &Path,
    args: &[&str],
    target_dir_override: Option<&str>,
) -> Result<i32, String> {
    let mut cmd = Command::new("cargo");
    cmd.args(args).current_dir(repo_root);
    if let Some(dir) = target_dir_override {
        cmd.env("CARGO_TARGET_DIR", dir);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run cargo {}: {e}", args.join(" ")))?;
    Ok(status.code().unwrap_or(1))
}

pub fn run_deterministic_replay_suite(args: RepoRootOnlyArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    println!("[deterministic-replay-suite] running hermes-cli e2e replay suite");
    run_cargo_inherit(
        &repo_root,
        &[
            "test",
            "-p",
            "hermes-cli",
            "--test",
            "e2e_replay_suite",
            "--",
            "--nocapture",
        ],
        None,
    )
}

fn any_rs_contains(paths: &[PathBuf], needle: &str) -> Result<bool, String> {
    for path in paths {
        if !path.exists() {
            continue;
        }
        if path.is_file() {
            if read_utf8(path)?.contains(needle) {
                return Ok(true);
            }
        } else if path.is_dir() {
            let mut files = Vec::new();
            collect_rs_files(path, &mut files)?;
            for file in files {
                if read_utf8(&file)?.contains(needle) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

pub fn run_openhuman_p0_p1_gate(args: RepoRootOnlyArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let commands_dir = repo_root.join("crates/hermes-cli/src/commands");
    let tui_roots = [
        commands_dir.clone(),
        repo_root.join("crates/hermes-cli/src/tui.rs"),
        repo_root.join("crates/hermes-cli/src/tui"),
    ];

    let required_commands = [
        "/commands",
        "/boot",
        "/walkthrough",
        "/triage",
        "/subconscious",
        "/integrations",
    ];
    for cmd in required_commands {
        let needle = format!("\"{cmd}\",");
        let found = if commands_dir.exists() {
            any_rs_contains(std::slice::from_ref(&commands_dir), &needle)?
        } else {
            false
        };
        if !found {
            eprintln!("[gate] missing slash command registration: {cmd}");
            return Ok(1);
        }
    }

    let required_tests = [
        "test_p0_p1_surface_commands_registered_and_completable",
        "p0_walkthrough_and_integrations_commands_emit_expected_sections",
        "p0_compress_rules_set_and_apply_updates_runtime_env",
        "p1_trigger_triage_escalates_high_severity_events",
        "test_format_tool_message_lines_truncates_large_payload",
    ];
    for test_name in required_tests {
        let needle = format!("fn {test_name}(");
        if !any_rs_contains(&tui_roots, &needle)? {
            eprintln!("[gate] missing required test: {test_name}");
            return Ok(1);
        }
    }

    println!("[gate] static surface checks passed");
    for test_name in required_tests {
        let code = run_cargo_inherit(
            &repo_root,
            &["test", "-p", "hermes-cli", "--lib", test_name],
            None,
        )?;
        if code != 0 {
            return Ok(code);
        }
    }

    println!("[gate] openhuman P0/P1 gate passed");
    Ok(0)
}

pub fn run_ultra_feature_benchmarks(args: RepoRootOnlyArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/Volumes/wd_black/hermes-agent-ultra/target".to_string());

    let steps: &[&[&str]] = &[
        &[
            "test",
            "-p",
            "hermes-tools",
            "tools::ultra_features",
            "--lib",
        ],
        &[
            "test",
            "-p",
            "hermes-tools",
            "register_builtins::tests::builtin_registry_registers_core_tool_surfaces_for_parity",
            "--lib",
        ],
        &["fmt", "--all", "--check"],
    ];

    for step in steps {
        let code = run_cargo_inherit(&repo_root, step, Some(&target_dir))?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::ClippyWarningGateArgs;
    use clap::Parser;

    #[derive(Parser)]
    struct GateWrapper {
        #[command(flatten)]
        args: ClippyWarningGateArgs,
    }

    #[test]
    fn clippy_gate_flags_preserve_last_wins_and_double_dash_passthrough() {
        let check_last = GateWrapper::try_parse_from(["gate", "--update", "--check"]).unwrap();
        assert!(!check_last.args.update);
        assert!(check_last.args.check);

        let update_last = GateWrapper::try_parse_from(["gate", "--check", "--update"]).unwrap();
        assert!(update_last.args.update);
        assert!(!update_last.args.check);

        let passthrough =
            GateWrapper::try_parse_from(["gate", "--check", "--", "--", "-D", "warnings"]).unwrap();
        assert_eq!(passthrough.args.extra_args, vec!["--", "-D", "warnings"]);
    }
}
