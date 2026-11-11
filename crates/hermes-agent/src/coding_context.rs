//! Coding-context posture detection and prompt blocks.
//!
//! This mirrors upstream's `agent.coding_context` contract in the Rust runtime:
//! interactive coding surfaces in a code workspace get a coding operating brief
//! plus a frozen workspace snapshot. `focus` mode additionally lets callers
//! collapse tools to the coding toolset; `auto` and `on` stay prompt-only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_core::subprocess::CommandNoWindowExt;
use serde::{Deserialize, Serialize};

pub const CODING_TOOLSET: &str = "coding";

const INTERACTIVE_CODING_PLATFORMS: &[&str] = &["", "cli", "tui", "acp", "desktop", "local"];
const PROJECT_MARKERS: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "package.json",
    "tsconfig.json",
    "deno.json",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "pubspec.yaml",
    "CMakeLists.txt",
    "Makefile",
    "Dockerfile",
    "AGENTS.md",
    "CLAUDE.md",
    ".cursorrules",
];
const CONTEXT_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", ".cursorrules"];
const VERIFY_TARGETS: &[&str] = &[
    "test",
    "tests",
    "lint",
    "typecheck",
    "check",
    "build",
    "fmt",
    "format",
];
const MAX_VERIFY_COMMANDS: usize = 8;
const MAX_FACT_FILE_BYTES: u64 = 256 * 1024;

const NON_CODING_SKILL_CATEGORIES: &[&str] = &[
    "apple",
    "communication",
    "cooking",
    "creative",
    "email",
    "finance",
    "gaming",
    "gifs",
    "health",
    "media",
    "music",
    "note-taking",
    "productivity",
    "shopping",
    "smart-home",
    "social-media",
    "travel",
    "yuanbao",
];

const CODING_AGENT_GUIDANCE: &str =
    "You are a coding agent pairing with the user inside their codebase. Operate like a careful senior engineer.\n\
\n\
Gather context first:\n\
- Read the relevant files and locate code before changing anything. Trace a symbol to its definition and usages rather than guessing its shape.\n\
- Batch independent reads/searches when they do not depend on each other.\n\
- If ContextLattice tools are available, use them as the project memory and retrieval backbone before redoing broad discovery.\n\
- Never invent files, symbols, APIs, imports, or dependencies. Check manifests and nearby imports before using a library.\n\
\n\
Make changes through the tools, not the chat:\n\
- Edit with patch/write_file. Do not print code blocks to the user as a substitute for applying the change.\n\
- Match the project's existing style and instructions. Touch only what the task needs; avoid drive-by refactors, renames, or formatting churn.\n\
- If a patch fails, re-read the exact file contents before retrying. If the same region fails twice, rewrite the enclosing function or file with write_file instead of attempting a third stale patch.\n\
\n\
Verify, and know when to stop:\n\
- Use the terminal for git, builds, tests, and inspection. Run the relevant tests, linter, or build before claiming the work is done.\n\
- Fix root causes and sibling call paths for the same bug class, not only the reported site.\n\
- When fixing linter/type errors on a file, stop after about three attempts on the same file and ask the user rather than looping.\n\
- Track multi-step work with todo. Reference code as path:line instead of pasting whole files.\n\
\n\
Respect the user's repo: do not commit, push, or rewrite history unless asked, and never read, print, or commit secrets. The Workspace block below is a session-start snapshot; re-run git status/git branch before relying on it. Be concise: lead with the change or answer, not a preamble.";

const EDIT_FORMAT_PATCH: &str =
    "- Edit format: author new files with write_file; for edits to existing code prefer patch with mode='patch' (V4A multi-file diff) for structured or multi-file changes. Use mode='replace' for a single small swap.";
const EDIT_FORMAT_REPLACE: &str =
    "- Edit format: author new files with write_file; for edits to existing code prefer patch in mode='replace' by matching a unique snippet and swapping it. Reach for mode='patch' (V4A) only when an edit genuinely spans several files at once.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingContextMode {
    Auto,
    Focus,
    On,
    Off,
}

impl CodingContextMode {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
            "focus" | "strict" | "lean" => Self::Focus,
            "on" | "true" | "yes" | "1" | "always" => Self::On,
            "off" | "false" | "no" | "0" | "never" => Self::Off,
            _ => Self::Auto,
        }
    }

    pub fn is_focus(self) -> bool {
        self == Self::Focus
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextProfileKind {
    General,
    Coding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMode {
    pub profile: ContextProfileKind,
    pub mode: CodingContextMode,
    pub workspace_root: Option<PathBuf>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFacts {
    pub root: String,
    pub manifests: Vec<String>,
    pub package_managers: Vec<String>,
    pub verify_commands: Vec<String>,
    pub context_files: Vec<String>,
}

impl RuntimeMode {
    pub fn is_coding(&self) -> bool {
        self.profile == ContextProfileKind::Coding
    }

    pub fn toolset_selection(&self) -> Option<&'static str> {
        (self.is_coding() && self.mode.is_focus()).then_some(CODING_TOOLSET)
    }

    pub fn hidden_skill_categories(&self) -> &'static [&'static str] {
        if self.is_coding() {
            NON_CODING_SKILL_CATEGORIES
        } else {
            &[]
        }
    }

    pub fn system_blocks(&self) -> Vec<String> {
        if !self.is_coding() {
            return Vec::new();
        }
        let mut blocks = Vec::new();
        let mut guidance = CODING_AGENT_GUIDANCE.to_string();
        if let Some(line) = edit_format_line(self.model.as_deref()) {
            guidance.push('\n');
            guidance.push_str(line);
        }
        blocks.push(guidance);
        if let Some(root) = &self.workspace_root {
            if let Some(block) = build_coding_workspace_block(root) {
                blocks.push(block);
            }
        }
        blocks
    }
}

pub fn resolve_runtime_mode(
    platform: Option<&str>,
    cwd: Option<&Path>,
    raw_mode: Option<&str>,
    model: Option<&str>,
) -> RuntimeMode {
    let mode = CodingContextMode::parse(raw_mode);
    let resolved_cwd = resolve_cwd(cwd);
    let workspace_root = workspace_root(&resolved_cwd);
    let platform = normalize_platform(platform);
    let active = match mode {
        CodingContextMode::Off => false,
        CodingContextMode::On => true,
        CodingContextMode::Auto | CodingContextMode::Focus => {
            is_interactive_coding_platform(&platform) && workspace_root.is_some()
        }
    };
    RuntimeMode {
        profile: if active {
            ContextProfileKind::Coding
        } else {
            ContextProfileKind::General
        },
        mode,
        workspace_root: if active {
            workspace_root.or(Some(resolved_cwd))
        } else {
            None
        },
        model: model.map(str::to_string),
    }
}

pub fn coding_toolset_selection(
    platform: Option<&str>,
    cwd: Option<&Path>,
    raw_mode: Option<&str>,
) -> Option<&'static str> {
    resolve_runtime_mode(platform, cwd, raw_mode, None).toolset_selection()
}

pub fn coding_hidden_skill_categories(
    platform: Option<&str>,
    cwd: Option<&Path>,
    raw_mode: Option<&str>,
) -> &'static [&'static str] {
    resolve_runtime_mode(platform, cwd, raw_mode, None).hidden_skill_categories()
}

pub fn model_family(model: Option<&str>) -> Option<&'static str> {
    let lowered = model?.to_ascii_lowercase();
    if ["gpt", "codex"]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        return Some("patch");
    }
    if [
        "claude", "sonnet", "opus", "haiku", "gemini", "gemma", "deepseek", "qwen", "kimi", "glm",
        "grok", "hermes", "llama", "mistral", "devstral", "minimax",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        return Some("replace");
    }
    None
}

pub fn edit_format_line(model: Option<&str>) -> Option<&'static str> {
    match model_family(model) {
        Some("patch") => Some(EDIT_FORMAT_PATCH),
        Some("replace") => Some(EDIT_FORMAT_REPLACE),
        _ => None,
    }
}

pub fn build_coding_workspace_block(root: &Path) -> Option<String> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !root.exists() {
        return None;
    }
    let mut lines = vec![
        "## Workspace".to_string(),
        format!("Root: {}", root.display()),
    ];

    let manifests = detected_project_markers(&root);
    if !manifests.is_empty() {
        lines.push(format!("Project: {}", manifests.join(", ")));
    }

    let verify = verify_commands(&root);
    if !verify.is_empty() {
        lines.push(format!("Verify: {}", verify.join("; ")));
    }

    let context_files = CONTEXT_FILES
        .iter()
        .copied()
        .filter(|name| root.join(name).is_file())
        .collect::<Vec<_>>();
    if !context_files.is_empty() {
        lines.push(format!("Context files: {}", context_files.join(", ")));
    }

    if git_root(&root).as_deref() == Some(root.as_path()) {
        if let Some(guard) = lazy_fetch_guard() {
            append_git_snapshot(&root, guard, &mut lines);
        }
    }

    (lines.len() > 2).then(|| lines.join("\n"))
}

pub fn detect_project_facts(root: &Path) -> ProjectFacts {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let manifests = project_manifest_names(&root);
    let package_managers = package_managers(&root);
    let verify_commands = verify_commands(&root);
    let context_files = CONTEXT_FILES
        .iter()
        .copied()
        .filter(|name| root.join(name).is_file())
        .map(str::to_string)
        .collect();

    ProjectFacts {
        root: root.display().to_string(),
        manifests,
        package_managers,
        verify_commands,
        context_files,
    }
}

pub fn project_facts_for(cwd: Option<&Path>) -> Option<ProjectFacts> {
    let resolved = resolve_cwd(cwd);
    let root = workspace_root(&resolved)?;
    Some(detect_project_facts(&root))
}

fn normalize_platform(platform: Option<&str>) -> String {
    platform
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
}

fn is_interactive_coding_platform(platform: &str) -> bool {
    INTERACTIVE_CODING_PLATFORMS.contains(&platform)
}

fn resolve_cwd(cwd: Option<&Path>) -> PathBuf {
    cwd.map(Path::to_path_buf)
        .or_else(|| std::env::var_os("TERMINAL_CWD").map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn workspace_root(cwd: &Path) -> Option<PathBuf> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let home = home_dir();
    let marker = marker_root(&cwd);
    if let Some(marker) = marker {
        if home.as_deref() != Some(marker.as_path()) {
            return Some(marker);
        }
    }
    let git = git_root(&cwd);
    if let Some(git) = git {
        if home.as_deref() != Some(git.as_path()) {
            return Some(git);
        }
    }
    None
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir().and_then(|path| path.canonicalize().ok())
}

fn ancestors_nearest_first(start: &Path) -> Vec<PathBuf> {
    let mut out = vec![start.to_path_buf()];
    out.extend(start.ancestors().skip(1).map(Path::to_path_buf));
    out
}

fn marker_root(cwd: &Path) -> Option<PathBuf> {
    ancestors_nearest_first(cwd)
        .into_iter()
        .take(12)
        .find(|parent| {
            PROJECT_MARKERS
                .iter()
                .any(|marker| parent.join(marker).exists())
        })
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    ancestors_nearest_first(cwd)
        .into_iter()
        .find(|parent| parent.join(".git").exists())
}

fn detected_project_markers(root: &Path) -> Vec<String> {
    let mut markers = Vec::new();
    for marker in PROJECT_MARKERS {
        if root.join(marker).exists() {
            if *marker == "package.json" {
                if let Some(pm) = js_package_manager(root) {
                    markers.push(format!("package.json ({pm})"));
                } else {
                    markers.push((*marker).to_string());
                }
            } else if *marker == "pyproject.toml" {
                if let Some(pm) = python_package_manager(root) {
                    markers.push(format!("pyproject.toml ({pm})"));
                } else {
                    markers.push((*marker).to_string());
                }
            } else {
                markers.push((*marker).to_string());
            }
        }
    }
    markers
}

fn project_manifest_names(root: &Path) -> Vec<String> {
    PROJECT_MARKERS
        .iter()
        .copied()
        .filter(|marker| !CONTEXT_FILES.contains(marker))
        .filter(|marker| root.join(marker).exists())
        .map(str::to_string)
        .collect()
}

fn package_managers(root: &Path) -> Vec<String> {
    let mut managers = Vec::new();
    if let Some(pm) = js_package_manager(root) {
        managers.push(pm.to_string());
    }
    if let Some(pm) = python_package_manager(root) {
        managers.push(pm.to_string());
    }
    dedup_truncate(managers)
}

fn js_package_manager(root: &Path) -> Option<&'static str> {
    [
        ("pnpm-lock.yaml", "pnpm"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
        ("yarn.lock", "yarn"),
        ("package-lock.json", "npm"),
    ]
    .iter()
    .find_map(|(file, manager)| root.join(file).exists().then_some(*manager))
}

fn python_package_manager(root: &Path) -> Option<&'static str> {
    [
        ("uv.lock", "uv"),
        ("poetry.lock", "poetry"),
        ("Pipfile.lock", "pipenv"),
    ]
    .iter()
    .find_map(|(file, manager)| root.join(file).exists().then_some(*manager))
}

fn verify_commands(root: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    append_package_json_verify(root, &mut commands);
    append_makefile_verify(root, &mut commands);
    if root.join("scripts").join("run_tests.sh").is_file() {
        commands.push("scripts/run_tests.sh".to_string());
    }
    if root.join("Cargo.toml").is_file() {
        commands.push("cargo test".to_string());
    }
    if root.join("go.mod").is_file() {
        commands.push("go test ./...".to_string());
    }
    if has_pytest_config(root) {
        commands.push("pytest".to_string());
    }
    dedup_truncate(commands)
}

fn append_package_json_verify(root: &Path, commands: &mut Vec<String>) {
    let path = root.join("package.json");
    if !small_file(&path) {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    let Some(scripts) = value.get("scripts").and_then(|v| v.as_object()) else {
        return;
    };
    let manager = js_package_manager(root).unwrap_or("npm");
    for target in VERIFY_TARGETS {
        if scripts.contains_key(*target) {
            commands.push(format!("{manager} run {target}"));
        }
    }
}

fn append_makefile_verify(root: &Path, commands: &mut Vec<String>) {
    let path = root.join("Makefile");
    if !small_file(&path) {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let targets = VERIFY_TARGETS
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for line in raw.lines() {
        let Some((name, _)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty()
            && !name.contains(char::is_whitespace)
            && !name.starts_with('.')
            && targets.contains(name)
        {
            commands.push(format!("make {name}"));
        }
    }
}

fn has_pytest_config(root: &Path) -> bool {
    if root.join("pytest.ini").is_file() || root.join("tox.ini").is_file() {
        return true;
    }
    let pyproject = root.join("pyproject.toml");
    if !small_file(&pyproject) {
        return false;
    }
    std::fs::read_to_string(pyproject)
        .map(|raw| raw.contains("[tool.pytest"))
        .unwrap_or(false)
}

fn small_file(path: &Path) -> bool {
    path.metadata()
        .map(|meta| meta.is_file() && meta.len() <= MAX_FACT_FILE_BYTES)
        .unwrap_or(false)
}

fn dedup_truncate(commands: Vec<String>) -> Vec<String> {
    let mut seen = BTreeMap::new();
    for command in commands {
        seen.entry(command).or_insert(());
    }
    seen.into_keys().take(MAX_VERIFY_COMMANDS).collect()
}

/// How `workspace_git` keeps git from fetching an object the repository lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LazyFetchGuard {
    /// Git 2.45 and later take `--no-lazy-fetch`.
    Flag,
    /// Git 2.31 to 2.44 have no such option, and fetch only from a promisor
    /// remote, so the snapshot is kept for a repository whose config names none.
    NoPromisorRemote,
}

/// The guard the installed git supports, or `None` for a git that cannot
/// produce the snapshot safely.
fn lazy_fetch_guard() -> Option<LazyFetchGuard> {
    // `git version` reads no repository.
    let accepts = |option: &str| {
        Command::new("git")
            .args([option, "version"])
            .env("HERMES_GIT_PROBE", "1")
            .stdin(std::process::Stdio::null())
            .suppress_windows_console()
            .output()
            .is_ok_and(|output| output.status.success())
    };
    if accepts("--no-lazy-fetch") {
        return Some(LazyFetchGuard::Flag);
    }
    // Git 2.31 brought `--config-env` together with the indexed `GIT_CONFIG_*`
    // variables that carry the filter overrides. An older git ignores those
    // variables without a word and would run the repository's filters, so it
    // gets no snapshot.
    accepts("--config-env=hermes.probe=HERMES_GIT_PROBE")
        .then_some(LazyFetchGuard::NoPromisorRemote)
}

/// A `git` command for reading the workspace that the workspace's own
/// `.git/config` cannot use to run a program: an empty `core.fsmonitor`
/// disables the fsmonitor program on every git version, `log.showSignature`
/// off stops `git log` running `gpg.program`, and `--no-optional-locks` keeps
/// `git status` from writing the index, so no index hook runs. Disable every
/// configured filter too: status can run clean/process commands while hashing
/// tracked files. Reading config names itself does not execute those commands.
/// Git must not fetch an object the repository lacks from a promisor remote
/// either, which runs `core.sshCommand` or a remote helper: `guard` says how.
fn workspace_git(root: &Path, guard: LazyFetchGuard) -> Option<Command> {
    // Every config key git reads for this repository, from every scope and
    // included file. The keys are matched here, byte for byte: git's own
    // `--get-regexp` passes over a key holding a byte its locale cannot
    // decode, so a repository could hide a filter or a remote from it.
    let listed = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--includes", "--null", "--name-only", "--list"])
        .stdin(std::process::Stdio::null())
        .suppress_windows_console()
        .output()
        .ok()?;
    // Refuse the snapshot if the listing fails, rather than running status
    // with an incomplete set of overrides.
    if !listed.status.success() {
        return None;
    }
    let mut drivers = BTreeMap::new();
    let mut filter_keys = 0;
    for key in listed.stdout.split(|byte| *byte == 0) {
        // Git prints a key as `section.name` or `section.subsection.name`, with
        // the section and the name in lower case. Only a subsection holds dots.
        let Some(first) = key.iter().position(|byte| *byte == b'.') else {
            continue;
        };
        let last = key.iter().rposition(|byte| *byte == b'.').unwrap_or(first);
        match (&key[..first], &key[last + 1..]) {
            (b"filter", b"clean" | b"smudge" | b"process" | b"required") => {
                if filter_keys == 256 {
                    return None;
                }
                filter_keys += 1;
                drivers.insert(std::str::from_utf8(&key[..last]).ok()?, ());
            }
            // Any of these keys can make a promisor remote, with or without a
            // remote name, and nothing on the command line switches one off.
            // Whatever its value, without `--no-lazy-fetch` the repository
            // gets no snapshot.
            (b"remote", b"promisor" | b"partialclonefilter") | (b"extensions", b"partialclone")
                if guard == LazyFetchGuard::NoPromisorRemote =>
            {
                return None;
            }
            _ => {}
        }
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=", "-c", "log.showSignature=false"])
        .arg("--no-optional-locks")
        .stdin(std::process::Stdio::null())
        .suppress_windows_console();
    if guard == LazyFetchGuard::Flag {
        cmd.arg("--no-lazy-fetch");
    }
    let mut config_count = 0;
    for driver in drivers.keys() {
        for (setting, value) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            // -c splits on '=', which is also valid inside a driver name.
            // Indexed environment entries keep the key separate from its value.
            cmd.env(
                format!("GIT_CONFIG_KEY_{config_count}"),
                format!("{driver}.{setting}"),
            )
            .env(format!("GIT_CONFIG_VALUE_{config_count}"), value);
            config_count += 1;
        }
    }
    cmd.env("GIT_CONFIG_COUNT", config_count.to_string())
        .env_remove("GIT_CONFIG_PARAMETERS");
    Some(cmd)
}

fn append_git_snapshot(root: &Path, guard: LazyFetchGuard, lines: &mut Vec<String>) {
    let Some(mut git) = workspace_git(root, guard) else {
        return;
    };
    // A submodule has its own config and filter drivers. Read its committed
    // gitlink state, but never run status inside its dirty worktree.
    let status = git
        .args(["status", "--short", "--branch", "--ignore-submodules=dirty"])
        .output();
    let Ok(output) = status else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut status_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for line in stdout.lines() {
        if let Some(branch) = line.strip_prefix("## ") {
            if !branch.trim().is_empty() {
                lines.push(format!("Branch: {}", branch.trim()));
            }
            continue;
        }
        let code = line.get(..2).unwrap_or(line).trim();
        let key = if line.starts_with("??") {
            "untracked"
        } else if code == "UU" || code == "AA" || code == "DD" {
            "conflicts"
        } else if code.chars().next().is_some_and(|c| c != ' ') {
            "staged"
        } else {
            "modified"
        };
        *status_counts.entry(key).or_default() += 1;
    }
    if status_counts.is_empty() {
        lines.push("Status: clean".to_string());
    } else {
        let rendered = status_counts
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Status: {rendered}"));
    }
    if let Some(commit) = git_last_commit(root, guard) {
        lines.push(format!("Last commit: {commit}"));
    }
}

fn git_last_commit(root: &Path, guard: LazyFetchGuard) -> Option<String> {
    let output = workspace_git(root, guard)?
        .args(["log", "-1", "--oneline"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_accepts(option: &str) -> bool {
        Command::new("git")
            .args([option, "version"])
            .env("HERMES_GIT_PROBE", "1")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// Whether this git can produce the snapshot of a repository without a
    /// promisor remote: it needs `--no-lazy-fetch` (Git 2.45) or, failing
    /// that, `--config-env` (Git 2.31).
    fn git_snapshot_supported() -> bool {
        git_accepts("--no-lazy-fetch") || git_accepts("--config-env=hermes.probe=HERMES_GIT_PROBE")
    }

    /// Each way a repository's config can name a promisor remote. Git reads
    /// the 2 `remote` keys without a remote name too.
    #[cfg(unix)]
    const PROMISOR_CONFIG: [[&str; 2]; 5] = [
        ["remote.croft.promisor", "true"],
        ["remote.croft.partialclonefilter", "blob:none"],
        ["extensions.partialclone", "croft"],
        ["remote.promisor", "true"],
        ["remote.partialclonefilter", "blob:none"],
    ];

    /// A repository that lacks an object its snapshot reads, with a promisor
    /// remote to fetch it from through `core.sshCommand`. `mailmap.blob` sends
    /// `git log` after a missing object; a HEAD commit without its tree sends
    /// `git status`. Returns the repository and the marker file the
    /// `core.sshCommand` program leaves when it runs.
    #[cfg(unix)]
    fn repo_missing_an_object(
        tmp: &Path,
        missing: &str,
        promisor: [&str; 2],
    ) -> (PathBuf, PathBuf) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let repo = tmp.join("repo");
        git_init(&repo);
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        // Before the remote is configured, so the fixture itself fetches nothing.
        if missing == "HEAD tree" {
            let commit = format!(
                "tree {}\nauthor T <t@example.com> 1700000000 +0000\n\
                 committer T <t@example.com> 1700000000 +0000\n\nno tree\n",
                "2".repeat(40)
            );
            let mut hash = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["hash-object", "-t", "commit", "-w", "--stdin"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            hash.stdin
                .take()
                .unwrap()
                .write_all(commit.as_bytes())
                .unwrap();
            let sha = String::from_utf8(hash.wait_with_output().unwrap().stdout).unwrap();
            git(&["update-ref", "HEAD", sha.trim()]);
        } else {
            git(&["config", "mailmap.blob", &"1".repeat(40)]);
        }
        let marker = tmp.join("ssh-ran");
        let program = tmp.join("ssh.sh");
        std::fs::write(
            &program,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&["config", promisor[0], promisor[1]]);
        git(&[
            "config",
            "remote.croft.url",
            "ssh://manor.invalid/croft.git",
        ]);
        // A remote without a name is fetched as the empty URL, which an empty
        // `insteadOf` rewrites to this one.
        if matches!(promisor[0], "remote.promisor" | "remote.partialclonefilter") {
            git(&["config", "url.ssh://manor.invalid/croft.git.insteadOf", ""]);
        }
        git(&["config", "core.sshCommand", program.to_str().unwrap()]);
        (repo, marker)
    }

    fn git_init(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["init", "-q"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.email", "test@example.com"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.name", "Hermes Test"])
            .status()
            .unwrap();
        std::fs::write(path.join("README.md"), "test").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["add", "README.md"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["commit", "-qm", "init commit"])
            .status()
            .unwrap();
    }

    #[test]
    fn coding_detection_respects_platform_and_modes() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        assert!(
            resolve_runtime_mode(Some("cli"), Some(tmp.path()), Some("auto"), None).is_coding()
        );
        assert!(
            !resolve_runtime_mode(Some("telegram"), Some(tmp.path()), Some("auto"), None)
                .is_coding()
        );
        assert!(
            resolve_runtime_mode(Some("telegram"), Some(tmp.path()), Some("on"), None).is_coding()
        );
        assert!(
            !resolve_runtime_mode(Some("cli"), Some(tmp.path()), Some("off"), None).is_coding()
        );
    }

    #[test]
    fn focus_mode_selects_coding_toolset_only_inside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            coding_toolset_selection(Some("cli"), Some(tmp.path()), Some("focus")),
            None
        );
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        assert_eq!(
            coding_toolset_selection(Some("cli"), Some(tmp.path()), Some("focus")),
            Some(CODING_TOOLSET)
        );
        assert_eq!(
            coding_toolset_selection(Some("cli"), Some(tmp.path()), Some("auto")),
            None
        );
    }

    #[test]
    fn workspace_block_reports_verify_loop_facts() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","lint":"eslint .","dev":"vite"}}"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "# rules").unwrap();
        let block = build_coding_workspace_block(tmp.path()).unwrap();
        assert!(block.contains("Workspace"));
        assert!(block.contains("package.json (pnpm)"));
        assert!(block.contains("pnpm run test"));
        assert!(block.contains("pnpm run lint"));
        assert!(!block.contains("run dev"));
        assert!(block.contains("Context files: AGENTS.md"));
        assert_eq!(block.contains("Status:"), git_snapshot_supported());
    }

    #[test]
    fn project_facts_are_structured_from_workspace_detector() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","lint":"eslint .","dev":"vite"}}"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "# rules").unwrap();

        let facts = project_facts_for(Some(tmp.path())).expect("project facts");

        assert_eq!(
            facts.root,
            tmp.path().canonicalize().unwrap().display().to_string()
        );
        assert!(facts.manifests.contains(&"package.json".to_string()));
        assert_eq!(facts.package_managers, vec!["pnpm".to_string()]);
        assert!(facts.verify_commands.contains(&"pnpm run test".to_string()));
        assert!(facts.verify_commands.contains(&"pnpm run lint".to_string()));
        assert!(!facts.verify_commands.iter().any(|cmd| cmd.contains("dev")));
        assert_eq!(facts.context_files, vec!["AGENTS.md".to_string()]);

        let rendered = build_coding_workspace_block(tmp.path()).unwrap();
        for command in &facts.verify_commands {
            assert!(rendered.contains(command));
        }
    }

    #[test]
    fn marker_only_project_gets_snapshot_without_git() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        let mode = resolve_runtime_mode(Some("cli"), Some(tmp.path()), Some("auto"), None);
        assert!(mode.is_coding());
        let block = mode.system_blocks().join("\n");
        assert!(block.contains("Cargo.toml"));
        assert!(block.contains("cargo test"));
        assert!(!block.contains("Branch:"));
    }

    #[test]
    fn edit_format_family_detection_covers_open_and_closed_models() {
        assert_eq!(model_family(Some("openai/gpt-5.4")), Some("patch"));
        assert_eq!(model_family(Some("openai/codex-mini")), Some("patch"));
        for model in [
            "anthropic/claude-sonnet-4",
            "google/gemini-3-pro",
            "deepseek-v3.2",
            "qwen3-coder",
            "moonshot/kimi-k2",
            "nousresearch/hermes-4-405b",
        ] {
            assert_eq!(model_family(Some(model)), Some("replace"));
        }
        assert_eq!(model_family(Some("acme/foo-1")), None);
        assert_eq!(model_family(None), None);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_snapshot_does_not_run_submodule_filters() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let source = tmp.path().join("source");
        git_init(&repo);
        git_init(&source);
        let git = |root: &Path, args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                source.to_str().unwrap(),
                "nested",
            ],
        );
        git(&repo, &["commit", "-qm", "add submodule"]);
        let nested = repo.join("nested");
        let marker = tmp.path().join("submodule-filter-ran");
        let program = tmp.path().join("filter.sh");
        std::fs::write(
            &program,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &nested,
            &[
                "config",
                "filter.croft-submodule.clean",
                program.to_str().unwrap(),
            ],
        );
        git(
            &nested,
            &["config", "filter.croft-submodule.required", "true"],
        );
        std::fs::write(
            nested.join(".gitattributes"),
            "README.md filter=croft-submodule\n",
        )
        .unwrap();
        std::fs::write(nested.join("README.md"), "best").unwrap();

        let block = build_coding_workspace_block(&repo);
        assert!(
            !marker.exists(),
            "the parent snapshot ran a submodule filter"
        );
        assert_eq!(
            block.is_some_and(|block| block.contains("Status:")),
            git_snapshot_supported()
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_snapshot_does_not_run_repository_filters() {
        use std::os::unix::fs::PermissionsExt;

        for kind in ["clean", "process"] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            git_init(&repo);
            let git = |args: &[&str]| {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "git {args:?}: {output:?}");
            };
            let marker = tmp.path().join("filter-ran");
            let program = tmp.path().join("filter.sh");
            std::fs::write(
                &program,
                format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::write(
                repo.join(".gitattributes"),
                "README.md filter=croft-manor=x\n",
            )
            .unwrap();
            // Include a separate config file, as filter commands need not live
            // directly in .git/config. Git must not execute either variant.
            let filter_config = tmp.path().join("filters.config");
            std::fs::write(
                &filter_config,
                format!(
                    "[filter \"croft-manor=x\"]\n\t{kind} = {}\n\trequired = true\n",
                    program.display()
                ),
            )
            .unwrap();
            git(&["config", "include.path", filter_config.to_str().unwrap()]);
            // Keep its size unchanged so status must hash it instead of
            // reporting a size mismatch without consulting the filter.
            std::fs::write(repo.join("README.md"), "best").unwrap();

            let block = build_coding_workspace_block(&repo);
            assert!(
                !marker.exists(),
                "git status ran the repository's {kind} filter"
            );
            assert_eq!(
                block.is_some_and(|block| block.contains("Status:")),
                git_snapshot_supported(),
                "{kind}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_snapshot_does_not_run_a_filter_whose_name_is_not_utf8() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        git_init(&repo);
        let marker = tmp.path().join("filter-ran");
        let program = tmp.path().join("filter.sh");
        std::fs::write(
            &program,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        // In a UTF-8 locale git's own key matching passes over this driver,
        // yet git runs it, so only there does this test tell the full listing
        // from `--get-regexp`. Its name cannot go into an override either, so
        // the repository gets no snapshot.
        let mut config = b"[filter \"croft\x80\"]\n\tclean = ".to_vec();
        config.extend_from_slice(program.to_str().unwrap().as_bytes());
        config.extend_from_slice(b"\n\trequired = true\n");
        let filter_config = tmp.path().join("filters.config");
        std::fs::write(&filter_config, config).unwrap();
        let included = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "include.path"])
            .arg(&filter_config)
            .status()
            .unwrap();
        assert!(included.success());
        std::fs::write(repo.join(".gitattributes"), b"README.md filter=croft\x80\n").unwrap();
        // The same size, so status must hash the file through the filter.
        std::fs::write(repo.join("README.md"), "best").unwrap();

        let block = build_coding_workspace_block(&repo);
        assert!(
            !marker.exists(),
            "git status ran a filter the snapshot could not override"
        );
        assert!(!block.is_some_and(|block| block.contains("Status:")));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_snapshot_runs_no_program_the_repository_config_names() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        git_init(&repo);
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let marker_program = |name: &str| {
            let marker = tmp.path().join(format!("{name}-ran"));
            let program = tmp.path().join(format!("{name}.sh"));
            std::fs::write(
                &program,
                format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            (marker, program.display().to_string())
        };

        // A HEAD commit carrying a signature makes `git log` verify it with
        // `gpg.program` when `log.showSignature` is on.
        let tree = git(&["rev-parse", "HEAD^{tree}"]);
        let commit = format!(
            "tree {tree}\nauthor T <t@example.com> 1700000000 +0000\n\
             committer T <t@example.com> 1700000000 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n \n -----END PGP SIGNATURE-----\n\nsigned\n"
        );
        let mut hash = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        hash.stdin
            .take()
            .unwrap()
            .write_all(commit.as_bytes())
            .unwrap();
        let sha = String::from_utf8(hash.wait_with_output().unwrap().stdout).unwrap();
        git(&["update-ref", "HEAD", sha.trim()]);

        let (fsmonitor_ran, fsmonitor) = marker_program("fsmonitor");
        let (gpg_ran, gpg) = marker_program("gpg");
        git(&["config", "core.fsmonitor", &fsmonitor]);
        git(&["config", "log.showSignature", "true"]);
        git(&["config", "gpg.program", &gpg]);

        let block = build_coding_workspace_block(&repo);
        assert_eq!(
            block.is_some_and(|block| block.contains("Status:")),
            git_snapshot_supported()
        );
        assert!(
            !fsmonitor_ran.exists(),
            "git status ran the repository's core.fsmonitor program"
        );
        assert!(
            !gpg_ran.exists(),
            "git log ran the repository's gpg.program"
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_snapshot_does_not_fetch_missing_objects() {
        for missing in ["mailmap blob", "HEAD tree"] {
            for promisor in PROMISOR_CONFIG {
                let tmp = tempfile::tempdir().unwrap();
                let (repo, marker) = repo_missing_an_object(tmp.path(), missing, promisor);

                let block = build_coding_workspace_block(&repo);
                assert!(
                    !marker.exists(),
                    "the snapshot fetched a missing {missing} through core.sshCommand ({promisor:?})"
                );
                // The mailmap is optional, so git still reports the commit
                // without it. A git without `--no-lazy-fetch` is not asked.
                if missing == "mailmap blob" {
                    assert_eq!(
                        block.is_some_and(|block| block.contains("Last commit:")),
                        git_accepts("--no-lazy-fetch"),
                        "{promisor:?}"
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn older_git_gets_no_snapshot_of_a_repository_with_a_promisor_remote() {
        // Without `--no-lazy-fetch`, git would fetch through `core.sshCommand`.
        for missing in ["mailmap blob", "HEAD tree"] {
            for promisor in PROMISOR_CONFIG {
                let tmp = tempfile::tempdir().unwrap();
                let (repo, marker) = repo_missing_an_object(tmp.path(), missing, promisor);

                let mut lines = Vec::new();
                append_git_snapshot(&repo, LazyFetchGuard::NoPromisorRemote, &mut lines);
                assert!(
                    !marker.exists(),
                    "the snapshot fetched a missing {missing} through core.sshCommand ({promisor:?})"
                );
                assert_eq!(lines, Vec::<String>::new(), "{promisor:?}");
            }
        }
    }

    #[test]
    fn older_git_finds_a_promisor_remote_wherever_it_is_named() {
        // In an included file, and under a name that is not UTF-8, which git's
        // own key matching passes over in a UTF-8 locale.
        for config in [
            &b"[remote \"croft\"]\n\tpromisor = true\n"[..],
            b"[remote \"croft\x80\"]\n\tpromisor = true\n",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            git_init(&repo);
            assert!(workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some());

            let included = tmp.path().join("promisor.config");
            std::fs::write(&included, config).unwrap();
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["config", "include.path"])
                .arg(&included)
                .status()
                .unwrap();
            assert!(status.success());
            assert!(
                workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_none(),
                "{}",
                String::from_utf8_lossy(config)
            );
        }
    }

    #[test]
    fn older_git_gets_the_snapshot_without_a_promisor_remote() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());

        // An older git rejects the option, so the command must leave it off.
        let git = workspace_git(tmp.path(), LazyFetchGuard::NoPromisorRemote).unwrap();
        assert!(!git.get_args().any(|arg| arg == "--no-lazy-fetch"));

        let mut lines = Vec::new();
        append_git_snapshot(tmp.path(), LazyFetchGuard::NoPromisorRemote, &mut lines);
        assert!(
            lines.iter().any(|line| line.starts_with("Branch: ")),
            "{lines:?}"
        );
        assert!(lines.contains(&"Status: clean".to_string()), "{lines:?}");
        assert!(
            lines.iter().any(|line| line.ends_with("init commit")),
            "{lines:?}"
        );
    }

    #[test]
    fn workspace_snapshot_is_omitted_past_the_filter_key_limit() {
        // Each driver has 4 keys, so 65 pass the 256 keys the snapshot will
        // override. The machine's own config can add drivers (Git LFS adds
        // one), so the kept case stays well under the limit.
        for (drivers, snapshot) in [(8, true), (65, false)] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            git_init(&repo);
            let config: String = (0..drivers)
                .map(|n| {
                    format!(
                        "[filter \"croft-{n}\"]\n\tclean = cat\n\tsmudge = cat\n\
                         \tprocess = cat\n\trequired = false\n"
                    )
                })
                .collect();
            let filter_config = tmp.path().join("filters.config");
            std::fs::write(&filter_config, config).unwrap();
            let included = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["config", "include.path"])
                .arg(&filter_config)
                .status()
                .unwrap();
            assert!(included.success());

            let block = build_coding_workspace_block(&repo);
            assert_eq!(
                block.is_some_and(|block| block.contains("Status:")),
                snapshot && git_snapshot_supported(),
                "{drivers} filter drivers"
            );
        }
    }
}
