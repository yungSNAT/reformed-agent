//! Path routing and the legacy guards used by the patch queue.

use crate::py;

const PATH_TO_TICKET: &[(&str, usize)] = &[
    ("tests/", 20),
    ("skills/", 21),
    ("optional-skills/", 21),
    ("web/", 22),
    ("ui-tui/", 22),
    ("website/", 22),
    ("gateway/platforms/", 23),
    ("plugins/memory/", 23),
    ("environments/benchmarks/", 24),
    ("environments/tool_call_parsers/", 24),
    ("tools/environments/", 24),
    ("packaging/", 25),
    ("scripts/install.sh", 25),
    ("README.md", 25),
    ("Dockerfile", 25),
    ("flake.nix", 25),
    (".github/workflows/", 25),
];

const PYTHON_TEST_SURFACE_PREFIXES: &[&str] = &["tests/", "test/"];

const RUST_PRIMARY_SUPERSEDE_PREFIXES: &[&str] = &[
    "tests/",
    "test/",
    "acp_adapter/",
    "agent/",
    "batch_runner.py",
    "hermes_cli/",
    "hermes_state.py",
    "hermes_constants.py",
    "hermes_logging.py",
    "mini_swe_runner.py",
    "minisweagent_path.py",
    "mixture_of_agents_tool.py",
    "model_tools.py",
    "gateway/",
    "rl_cli.py",
    "tools/",
    "cron/",
    "providers/",
    "terminal_tool.py",
    "tui_gateway.py",
    "run_agent.py",
    "cli.py",
    "utils.py",
    "web_tools.py",
    "pyproject.toml",
    "uv.lock",
    "requirements.txt",
    "requirements-dev.txt",
    "mypy.ini",
    "ruff.toml",
    "scripts/",
    "locales/",
    "nix/",
    ".github/",
    "ui-tui/",
    "web/",
    "website/",
];

const RUST_PRIMARY_RETAINED_PREFIXES: &[&str] = &[
    "skills/",
    "optional-skills/",
    "plugins/",
    "docs/",
    "packaging/",
    "scripts/install.sh",
    "README.md",
    "Dockerfile",
    "flake.nix",
    ".github/workflows/",
];

const RUST_PRIMARY_NEUTRAL_PREFIXES: &[&str] = &[
    ".dockerignore",
    ".env.example",
    ".envrc",
    ".gitattributes",
    ".gitignore",
    ".cursorrules",
    "__pycache__/",
    "assets/",
    "configs/",
    "datagen-config-examples/",
    "docs/agents.md",
    "docs/llm_client.md",
    "docs/message_graph.md",
    "docs/tools.md",
    "package-lock.json",
    "package.json",
    "requirements.txt",
    "requirements-dev.txt",
    "setup-hermes.sh",
    "test_run.sh",
    "toolset_distributions.py",
    "TODO.md",
    "VISION.md",
];

pub(super) fn ticket_name(ticket: usize) -> &'static str {
    match ticket {
        20 => "GPAR-01 tests+CI parity",
        21 => "GPAR-02 skills parity",
        22 => "GPAR-03 UX parity",
        23 => "GPAR-04 gateway/plugin-memory parity",
        24 => "GPAR-05 environments+parsers+benchmarks",
        25 => "GPAR-06 packaging/docs/install parity",
        26 => "GPAR-07 upstream queue backfill",
        _ => "unknown",
    }
}

pub(super) fn classify_ticket(files: &[String]) -> usize {
    let mut votes: Vec<(usize, usize)> = Vec::new();
    for file in files {
        if let Some((_, ticket)) = PATH_TO_TICKET
            .iter()
            .find(|(prefix, _)| file.starts_with(prefix))
        {
            match votes.iter_mut().find(|(candidate, _)| candidate == ticket) {
                Some((_, count)) => *count += 1,
                None => votes.push((*ticket, 1)),
            }
        }
    }
    // Python Counter resolves equal counts in insertion order.
    votes
        .into_iter()
        .rev()
        .max_by_key(|(_, count)| *count)
        .map_or(26, |(ticket, _)| ticket)
}

fn matches_any(path: &str, prefixes: &[&str]) -> bool {
    let path = py::strip(path);
    let path = path.strip_prefix("./").unwrap_or(path);
    prefixes.iter().any(|prefix| path.starts_with(prefix))
}

pub(super) fn python_test_only(files: &[String]) -> bool {
    !files.is_empty()
        && files
            .iter()
            .all(|path| matches_any(path, PYTHON_TEST_SURFACE_PREFIXES))
}

pub(super) fn rust_primary_surface_only(files: &[String]) -> bool {
    !files.is_empty()
        && files.iter().all(|path| {
            matches_any(path, RUST_PRIMARY_NEUTRAL_PREFIXES)
                || (!matches_any(path, RUST_PRIMARY_RETAINED_PREFIXES)
                    && matches_any(path, RUST_PRIMARY_SUPERSEDE_PREFIXES))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn recognises_the_python_test_surface() {
        assert!(python_test_only(&files(&[
            "tests/gateway/test_slack.py",
            " ./test/readme.txt "
        ])));
        assert!(!python_test_only(&files(&["tests/a.py", "src/a.rs"])));
        assert!(!python_test_only(&files(&["Test/sample.py"])));
        assert!(!python_test_only(&[]));
    }

    #[test]
    fn guard_precedence_matches_the_legacy_queue() {
        assert!(rust_primary_surface_only(&files(&[
            "docs/tools.md",
            "gateway/a.py"
        ])));
        assert!(!rust_primary_surface_only(&files(&[
            "docs/a.md",
            "gateway/a.py"
        ])));
        assert!(!rust_primary_surface_only(&files(&[
            ".github/workflows/test.yml"
        ])));
        assert!(!rust_primary_surface_only(&files(&["src/a.rs"])));
    }

    #[test]
    fn first_ticket_to_reach_a_tie_wins() {
        assert_eq!(
            classify_ticket(&files(&["docs/no-vote", "skills/a", "tests/b"])),
            21
        );
        assert_eq!(
            classify_ticket(&files(&["skills/a", "tests/b", "tests/c"])),
            20
        );
        assert_eq!(classify_ticket(&files(&["docs/no-vote"])), 26);
    }
}
