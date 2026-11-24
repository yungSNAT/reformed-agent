//! `generate-llms-txt`: writes the documentation site's `llms.txt` (a curated
//! index) and `llms-full.txt` (every page concatenated). Run from
//! `website/scripts/prebuild.mjs`.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::py;
use crate::report_io::resolve_argument;
use crate::website::{read_text, rel_posix, rstrip, write_text, WebsiteArgs};

const SITE_BASE: &str = "https://hermes-agent.nousresearch.com/docs";

type Section = (&'static str, &'static [(&'static str, &'static str, Option<&'static str>)]);

const SECTIONS: &[Section] = &[
    (
        "Getting Started",
        &[
            ("getting-started/installation", "Installation", None),
            ("getting-started/quickstart", "Quickstart", None),
            ("getting-started/learning-path", "Learning Path", None),
            ("getting-started/updating", "Updating", None),
            ("getting-started/termux", "Termux (Android)", None),
            ("getting-started/nix-setup", "Nix Setup", None),
        ],
    ),
    (
        "Using Hermes",
        &[
            ("user-guide/cli", "CLI", None),
            ("user-guide/tui", "TUI (Ink terminal UI)", None),
            ("user-guide/configuration", "Configuration", None),
            ("user-guide/configuring-models", "Configuring Models", None),
            ("user-guide/sessions", "Sessions", None),
            ("user-guide/profiles", "Profiles", None),
            ("user-guide/git-worktrees", "Git Worktrees", None),
            ("user-guide/docker", "Docker Backend", None),
            ("user-guide/security", "Security", None),
            ("user-guide/checkpoints-and-rollback", "Checkpoints & Rollback", None),
        ],
    ),
    (
        "Core Features",
        &[
            ("user-guide/features/overview", "Features Overview", None),
            ("user-guide/features/tools", "Tools", None),
            ("user-guide/features/skills", "Skills System", None),
            ("user-guide/features/curator", "Curator", None),
            ("user-guide/features/memory", "Memory", None),
            ("user-guide/features/memory-providers", "Memory Providers", None),
            ("user-guide/features/context-files", "Context Files", None),
            ("user-guide/features/context-references", "Context References", None),
            ("user-guide/features/personality", "Personality & SOUL.md", None),
            ("user-guide/features/plugins", "Plugins", None),
            ("user-guide/features/built-in-plugins", "Built-in Plugins", None),
        ],
    ),
    (
        "Automation",
        &[
            ("user-guide/features/cron", "Cron Jobs", None),
            ("user-guide/features/delegation", "Delegation", None),
            ("user-guide/features/kanban", "Kanban Multi-Agent", None),
            ("user-guide/features/kanban-tutorial", "Kanban Tutorial", None),
            ("user-guide/features/goals", "Persistent Goals", None),
            ("user-guide/features/code-execution", "Code Execution", None),
            ("user-guide/features/hooks", "Hooks", None),
            ("user-guide/features/batch-processing", "Batch Processing", None),
        ],
    ),
    (
        "Media & Web",
        &[
            ("user-guide/features/voice-mode", "Voice Mode", None),
            ("user-guide/features/browser", "Browser", None),
            ("user-guide/features/vision", "Vision", None),
            ("user-guide/features/image-generation", "Image Generation", None),
            ("user-guide/features/tts", "Text-to-Speech", None),
        ],
    ),
    (
        "Messaging Platforms",
        &[
            ("user-guide/messaging/index", "Overview", None),
            ("user-guide/messaging/telegram", "Telegram", None),
            ("user-guide/messaging/discord", "Discord", None),
            ("user-guide/messaging/slack", "Slack", None),
            ("user-guide/messaging/whatsapp", "WhatsApp", None),
            ("user-guide/messaging/signal", "Signal", None),
            ("user-guide/messaging/email", "Email", None),
            ("user-guide/messaging/sms", "SMS", None),
            ("user-guide/messaging/matrix", "Matrix", None),
            ("user-guide/messaging/mattermost", "Mattermost", None),
            ("user-guide/messaging/homeassistant", "Home Assistant", None),
            ("user-guide/messaging/webhooks", "Webhooks", None),
        ],
    ),
    (
        "Integrations",
        &[
            ("integrations/index", "Integrations Overview", None),
            ("integrations/providers", "Providers", None),
            ("user-guide/features/mcp", "MCP (Model Context Protocol)", None),
            ("user-guide/features/acp", "ACP (Agent Context Protocol)", None),
            ("user-guide/features/api-server", "API Server", None),
            ("user-guide/features/honcho", "Honcho Memory", None),
            ("user-guide/features/provider-routing", "Provider Routing", None),
            ("user-guide/features/fallback-providers", "Fallback Providers", None),
            ("user-guide/features/credential-pools", "Credential Pools", None),
        ],
    ),
    (
        "Guides & Tutorials",
        &[
            ("guides/tips", "Tips & Best Practices", None),
            ("guides/local-llm-on-mac", "Local LLMs on Mac", None),
            ("guides/daily-briefing-bot", "Daily Briefing Bot", None),
            ("guides/team-telegram-assistant", "Team Telegram Assistant", None),
            ("guides/python-library", "Use Hermes as a Python Library", None),
            ("guides/use-mcp-with-hermes", "Use MCP with Hermes", None),
            ("guides/use-voice-mode-with-hermes", "Use Voice Mode with Hermes", None),
            ("guides/use-soul-with-hermes", "Use SOUL.md with Hermes", None),
            ("guides/build-a-hermes-plugin", "Build a Hermes Plugin", None),
            ("guides/automate-with-cron", "Automate with Cron", None),
            ("guides/work-with-skills", "Work with Skills", None),
            ("guides/delegation-patterns", "Delegation Patterns", None),
            ("guides/github-pr-review-agent", "GitHub PR Review Agent", None),
        ],
    ),
    (
        "Developer Guide",
        &[
            ("developer-guide/contributing", "Contributing", None),
            ("developer-guide/architecture", "Architecture", None),
            ("developer-guide/agent-loop", "Agent Loop", None),
            ("developer-guide/prompt-assembly", "Prompt Assembly", None),
            (
                "developer-guide/context-compression-and-caching",
                "Context Compression & Caching",
                None,
            ),
            ("developer-guide/gateway-internals", "Gateway Internals", None),
            ("developer-guide/session-storage", "Session Storage", None),
            ("developer-guide/provider-runtime", "Provider Runtime", None),
            ("developer-guide/adding-tools", "Adding Tools", None),
            ("developer-guide/adding-providers", "Adding Providers", None),
            ("developer-guide/adding-platform-adapters", "Adding Platform Adapters", None),
            ("developer-guide/creating-skills", "Creating Skills", None),
            ("developer-guide/extending-the-cli", "Extending the CLI", None),
        ],
    ),
    (
        "Reference",
        &[
            ("reference/cli-commands", "CLI Commands", None),
            ("reference/slash-commands", "Slash Commands", None),
            ("reference/profile-commands", "Profile Commands", None),
            ("reference/environment-variables", "Environment Variables", None),
            ("reference/tools-reference", "Tools Reference", None),
            ("reference/toolsets-reference", "Toolsets Reference", None),
            ("reference/mcp-config-reference", "MCP Config Reference", None),
            ("reference/model-catalog", "Model Catalog", None),
            (
                "reference/skills-catalog",
                "Bundled Skills Catalog",
                Some("Table of all ~90 skills bundled with Hermes"),
            ),
            (
                "reference/optional-skills-catalog",
                "Optional Skills Catalog",
                Some("Table of ~60 additional installable skills"),
            ),
            ("reference/faq", "FAQ & Troubleshooting", None),
        ],
    ),
];

struct DocMeta {
    title: Option<String>,
    description: Option<String>,
}

struct Patterns {
    frontmatter: Regex,
    description: Regex,
    title: Regex,
}

impl Patterns {
    fn new() -> Self {
        Self {
            frontmatter: Regex::new(r"(?s)\A---\s*\n(.*?)\n---\s*\n").expect("valid pattern"),
            description: Regex::new(r#"(?m)^description:\s*["'](.+?)["']\s*$"#)
                .expect("valid pattern"),
            title: Regex::new(r#"(?m)^title:\s*["'](.+?)["']\s*$"#).expect("valid pattern"),
        }
    }

    /// The frontmatter's quoted `title` and `description`, and the body after it.
    fn read(&self, path: &Path) -> Result<(DocMeta, String), String> {
        let text = read_text(path)?;
        let Some(m) = self.frontmatter.captures(&text) else {
            return Ok((DocMeta { title: None, description: None }, text));
        };
        let fm = m.get(1).map_or("", |g| g.as_str());
        let capture = |re: &Regex| re.captures(fm).map(|c| c[1].to_string());
        let meta = DocMeta {
            title: capture(&self.title),
            description: capture(&self.description),
        };
        let body = text[m.get(0).map_or(0, |g| g.end())..].to_string();
        Ok((meta, body))
    }
}

/// `docs/<slug>.md`, else `docs/<slug>/index.md`, if either exists.
fn doc_path(docs: &Path, slug: &str) -> Option<PathBuf> {
    let file = docs.join(format!("{slug}.md"));
    if file.exists() {
        return Some(file);
    }
    let index = docs.join(slug).join("index.md");
    index.exists().then_some(index)
}

fn emit_llms_index(docs: &Path, patterns: &Patterns) -> Result<String, String> {
    let mut lines: Vec<String> = vec![
        "# Hermes Agent".into(),
        String::new(),
        "> The self-improving AI agent built by Nous Research. A terminal-native \
         autonomous coding and task agent with persistent memory, agent-created skills, \
         and a messaging gateway that lives on 21+ messaging platforms — 19 native to \
         the gateway plus IRC and Microsoft Teams via plugins (Telegram, Discord, Slack, \
         SMS, Matrix, ...). Runs on local, Docker, SSH, Daytona, Modal, or Singularity \
         backends. Works with Nous Portal, OpenRouter, OpenAI, Anthropic, Google, or any \
         OpenAI-compatible endpoint."
            .into(),
        String::new(),
        "Install from source (no release yet; needs Git and a Rust toolchain): \
         `cargo install --git https://github.com/yungSNAT/reformed-agent hermes-cli \
         --locked --bin hermes-agent-ultra --bin hermes-ultra`  \
         (Linux, macOS, WSL2)"
            .into(),
        String::new(),
        "Repo: https://github.com/yungSNAT/reformed-agent".into(),
        String::new(),
    ];
    for (section, items) in SECTIONS {
        lines.push(format!("## {section}"));
        lines.push(String::new());
        for (slug, title, provided) in *items {
            let desc = match provided {
                Some(desc) => (*desc).to_string(),
                None => match doc_path(docs, slug) {
                    Some(path) => patterns.read(&path)?.0.description.unwrap_or_default(),
                    None => String::new(),
                },
            };
            let url = format!("{SITE_BASE}/{slug}");
            if desc.is_empty() {
                lines.push(format!("- [{title}]({url})"));
            } else {
                lines.push(format!("- [{title}]({url}): {desc}"));
            }
        }
        lines.push(String::new());
    }
    Ok(format!("{}\n", rstrip(&lines.join("\n"))))
}

/// Every `*.md` file under `dir`, relative to `docs`, as `Path.rglob` finds
/// them (symlinked directories are not entered).
fn markdown_files(docs: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))? {
        let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if kind.is_dir() {
            markdown_files(docs, &path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path.strip_prefix(docs).unwrap_or(&path).to_path_buf());
        }
    }
    Ok(())
}

fn emit_llms_full(docs: &Path, patterns: &Patterns) -> Result<String, String> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut chunks = vec![
        "# Hermes Agent — Full Documentation\n".to_string(),
        "This file is the entire Hermes Agent documentation concatenated for LLM \
         context ingestion. Section order reflects docs-site navigation: Getting \
         Started, Using Hermes, Features, Messaging, Integrations, Guides, \
         Developer Guide, Reference, then everything else.\n"
            .to_string(),
        "Canonical site: https://hermes-agent.nousresearch.com/docs\n".to_string(),
        "Short index: https://hermes-agent.nousresearch.com/docs/llms.txt\n".to_string(),
        "\n---\n\n".to_string(),
    ];
    let mut emit = |rel: &Path, fallback_title: String, chunks: &mut Vec<String>| -> Result<(), String> {
        let (meta, body) = patterns.read(&docs.join(rel))?;
        let title = meta.title.filter(|t| !t.is_empty()).unwrap_or(fallback_title);
        chunks.push(format!("<!-- source: website/docs/{} -->\n", rel_posix(rel, Path::new(""))));
        chunks.push(format!("# {title}\n\n"));
        chunks.push(format!("{}\n\n---\n\n", rstrip(&body)));
        Ok(())
    };
    for (_, items) in SECTIONS {
        for (slug, _, _) in *items {
            let Some(path) = doc_path(docs, slug) else {
                continue;
            };
            let rel = path.strip_prefix(docs).unwrap_or(&path).to_path_buf();
            if seen.contains(&rel) {
                continue;
            }
            emit(&rel, (*slug).to_string(), &mut chunks)?;
            seen.push(rel);
        }
    }
    let mut rest = Vec::new();
    markdown_files(docs, docs, &mut rest)?;
    rest.sort();
    for rel in rest {
        if seen.contains(&rel) {
            continue;
        }
        let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect();
        if parts.len() >= 3
            && parts[0] == "user-guide"
            && parts[1] == "skills"
            && (parts[2] == "bundled" || parts[2] == "optional")
        {
            continue;
        }
        let name = rel_posix(&rel, Path::new(""));
        emit(&rel, name, &mut chunks)?;
        seen.push(rel);
    }
    Ok(format!("{}\n", rstrip(&chunks.concat())))
}

/// Python's `f"{n:,}"`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn run(args: WebsiteArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let docs = root.join("website/docs");
    let stat = root.join("website/static");
    if !stat.is_dir() {
        fs::create_dir(&stat).map_err(|error| format!("{}: {error}", stat.display()))?;
    }
    let patterns = Patterns::new();
    let index = emit_llms_index(&docs, &patterns)?;
    let full = emit_llms_full(&docs, &patterns)?;
    let index_path = stat.join("llms.txt");
    let full_path = stat.join("llms-full.txt");
    write_text(&index_path, &index)?;
    write_text(&full_path, &full)?;
    for (path, text) in [(&index_path, &index), (&full_path, &full)] {
        println!(
            "Wrote {} ({} bytes)",
            py::path_str(path),
            thousands(text.chars().count())
        );
    }
    Ok(0)
}
