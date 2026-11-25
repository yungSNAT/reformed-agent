//! Publish automation summaries with ContextLattice-first sink fallback.
//! Port of `scripts/publish_automation_summary.py`.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use clap::Args;

use crate::automation_summary_sinks::try_sinks;
use crate::py::{self, Py};
use crate::script_helpers::{require_existing_dir, resolve_repo_root};

const DEFAULT_CONTEXTLATTICE_URL: &str = "http://127.0.0.1:8075";
const DEFAULT_SINK_ORDER: &str = "contextlattice,github,local";
const SUPPORTED_SINKS: &[&str] = &["contextlattice", "github", "local", "stdout"];
const MAX_PREFORMATTED_STDIN_BYTES: usize = 4 * 1024 * 1024;

fn env_or(keys: &[&str], default: &str) -> String {
    for key in keys {
        if let Ok(val) = std::env::var(key) {
            if !val.is_empty() {
                return val;
            }
        }
    }
    default.to_string()
}

fn default_issue() -> i64 {
    env_or(&["AUTOMATION_SUMMARY_GITHUB_ISSUE"], "0")
        .trim()
        .parse::<i64>()
        .unwrap_or(0)
}

fn default_timeout_secs() -> f64 {
    env_or(&["AUTOMATION_SUMMARY_CONTEXT_TIMEOUT_SECS"], "8")
        .trim()
        .parse::<f64>()
        .unwrap_or(8.0)
}

#[derive(Args, Debug)]
pub struct AutomationSummaryArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Summary kind label
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_KIND"], "automation"))]
    summary_kind: String,
    /// Run status label
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_STATUS"], "unknown"))]
    status: String,
    /// Optional summary title
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_TITLE"], ""), allow_hyphen_values = true)]
    title: String,
    /// Inline summary markdown/text body
    #[arg(long, default_value = "", allow_hyphen_values = true)]
    summary_body: String,
    /// Path to summary markdown/text body file
    #[arg(long, default_value = "")]
    summary_body_file: String,
    /// Preformatted summary markdown override (internal compatibility helper)
    #[arg(long, hide = true, default_value = "", allow_hyphen_values = true)]
    preformatted_markdown: String,
    /// Inline JSON object merged into summary metadata
    #[arg(long, default_value = "", allow_hyphen_values = true)]
    metadata_json: String,
    /// Path to JSON object merged into summary metadata
    #[arg(long, default_value = "")]
    metadata_file: String,
    /// Comma-separated sink order: contextlattice,github,local,stdout
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_SINK_ORDER"], DEFAULT_SINK_ORDER))]
    pub(crate) sink_order: String,
    /// ContextLattice orchestrator base URL
    #[arg(long, default_value_t = env_or(&["CONTEXTLATTICE_ORCHESTRATOR_URL", "MEMMCP_ORCHESTRATOR_URL"], DEFAULT_CONTEXTLATTICE_URL))]
    pub(crate) contextlattice_url: String,
    /// ContextLattice project name (default: repo dir name)
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_CONTEXT_PROJECT"], ""))]
    context_project: String,
    /// ContextLattice topic path for writes
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_CONTEXT_TOPIC_PATH"], "ops/automation-summary"))]
    pub(crate) context_topic_path: String,
    /// ContextLattice file key for writes
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_CONTEXT_FILE_NAME"], "ops/automation-summary.md"))]
    pub(crate) context_file_name: String,
    /// Agent id recorded in summary metadata
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_CONTEXT_AGENT_ID"], "hermes_ultra_automation"))]
    context_agent_id: String,
    /// ContextLattice write timeout seconds
    #[arg(long, default_value_t = default_timeout_secs(), allow_negative_numbers = true)]
    pub(crate) context_timeout_secs: f64,
    /// Optional ContextLattice API key
    #[arg(long, allow_hyphen_values = true)]
    context_api_key: Option<String>,
    /// Fallback GitHub issue number for comment sink
    #[arg(long, default_value_t = default_issue(), allow_negative_numbers = true)]
    pub(crate) github_issue: i64,
    /// Fallback local file path
    #[arg(long, default_value_t = env_or(&["AUTOMATION_SUMMARY_LOCAL_PATH"], ""))]
    local_path: String,
    /// Emit result as JSON
    #[arg(long)]
    pub(crate) json: bool,
}

impl AutomationSummaryArgs {
    pub(crate) fn resolved_api_key(&self) -> String {
        self.context_api_key.clone().unwrap_or_else(|| {
            env_or(
                &[
                    "AUTOMATION_SUMMARY_CONTEXT_API_KEY",
                    "CONTEXTLATTICE_API_KEY",
                ],
                "",
            )
        })
    }
}

pub fn parse_sink_order(raw: &str) -> Vec<String> {
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    for token in raw.split(',') {
        let sink = token.trim().to_ascii_lowercase();
        if sink.is_empty() || !SUPPORTED_SINKS.contains(&sink.as_str()) {
            continue;
        }
        if seen.insert(sink.clone()) {
            order.push(sink);
        }
    }
    if order.is_empty() {
        vec!["local".to_string()]
    } else {
        order
    }
}

pub fn load_json_object(raw_json: &str, path: &str) -> Py {
    let mut payload = Py::dict(Vec::<(&str, Py)>::new());
    for source in [
        (!path.is_empty())
            .then(|| fs::read_to_string(path).ok())
            .flatten(),
        (!raw_json.is_empty()).then(|| raw_json.to_string()),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(Py::Dict(entries)) = py::loads(&source) {
            for (k, v) in entries {
                payload.set(&k, v);
            }
        }
    }
    payload
}

const SUMMARY_META_KEYS: &[&str] = &[
    "timestamp_utc",
    "status",
    "summary_kind",
    "repo",
    "head_sha",
    "delivery_id",
];

fn append_metadata_lines(lines: &mut Vec<String>, metadata: &Py) {
    for &key in SUMMARY_META_KEYS {
        if let Some(val) = metadata
            .get(key)
            .filter(|v| !matches!(v, Py::Null) && !matches!(v, Py::Str(s) if s.is_empty()))
        {
            let formatted = py::str_of(val);
            lines.push(format!("- {key}: `{formatted}`"));
        }
    }
    if let Py::Dict(pairs) = metadata {
        let extra: Vec<(String, Py)> = pairs
            .iter()
            .filter(|(k, v)| {
                !SUMMARY_META_KEYS.contains(&k.as_str())
                    && !matches!(v, Py::Null)
                    && !matches!(v, Py::Str(s) if s.is_empty())
            })
            .cloned()
            .collect();
        if !extra.is_empty() {
            let serialized = py::dumps(&Py::Dict(extra), None, true);
            lines.push(format!("- metadata: `{serialized}`"));
        }
    }
}

pub fn build_summary_markdown(
    title: &str,
    summary_kind: &str,
    metadata: &Py,
    body: &str,
) -> String {
    let heading = if title.trim().is_empty() {
        let kind = if summary_kind.trim().is_empty() {
            "automation"
        } else {
            summary_kind.trim()
        };
        format!("{kind} summary")
    } else {
        title.trim().to_string()
    };
    let mut lines = vec![format!("## {heading}"), String::new()];
    append_metadata_lines(&mut lines, metadata);
    let body_trimmed = body.trim();
    lines.push(String::new());
    lines.push(if body_trimmed.is_empty() {
        "_No summary body provided._".to_string()
    } else {
        body_trimmed.to_string()
    });
    lines.push(String::new());
    lines.join("\n")
}

fn read_preformatted_stdin() -> Result<String, String> {
    let mut buf = Vec::new();
    io::stdin()
        .take((MAX_PREFORMATTED_STDIN_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .map_err(|e| format!("failed reading preformatted markdown from stdin: {e}"))?;
    if buf.len() > MAX_PREFORMATTED_STDIN_BYTES {
        return Err(format!(
            "preformatted markdown from stdin exceeds {MAX_PREFORMATTED_STDIN_BYTES} bytes"
        ));
    }
    String::from_utf8(buf).map_err(|e| format!("invalid UTF-8 on preformatted markdown stdin: {e}"))
}

fn resolve_markdown_and_metadata(
    args: &AutomationSummaryArgs,
    repo_root: &Path,
) -> Result<(Py, String), String> {
    let mut summary_body = String::new();
    if !args.summary_body_file.is_empty() {
        let p = py::expanduser(&args.summary_body_file)?;
        summary_body =
            fs::read_to_string(&p).map_err(|e| format!("failed reading {}: {e}", p.display()))?;
    }
    if !args.summary_body.is_empty() {
        summary_body = args.summary_body.clone();
    }
    let mut metadata = load_json_object(&args.metadata_json, &args.metadata_file);
    let ts_default = jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    for (k, v) in [
        ("timestamp_utc", ts_default),
        ("status", args.status.clone()),
        ("summary_kind", args.summary_kind.clone()),
        ("repo", py::path_str(repo_root)),
        ("agent_id", args.context_agent_id.clone()),
    ] {
        if metadata.get(k).is_none() {
            metadata.set(k, Py::Str(v));
        }
    }
    let markdown = if args.preformatted_markdown == "-" {
        read_preformatted_stdin()?
    } else if args.preformatted_markdown.is_empty() {
        build_summary_markdown(&args.title, &args.summary_kind, &metadata, &summary_body)
    } else {
        args.preformatted_markdown.clone()
    };
    Ok((metadata, markdown))
}

fn print_summary_output(
    args: &AutomationSummaryArgs,
    project: &str,
    local_path: &Path,
    ok: bool,
    primary_sink: &str,
    attempts: &[Py],
    metadata: &Py,
) {
    if !args.json {
        let status = if ok { "ok" } else { "failed" };
        let sink_label = if primary_sink.is_empty() {
            "<none>"
        } else {
            primary_sink
        };
        println!(
            "[publish-automation-summary] {status} sink={sink_label} attempts={}",
            attempts.len()
        );
        return;
    }
    let payload = Py::dict([
        ("ok", Py::from(ok)),
        ("primary_sink", Py::from(primary_sink)),
        ("attempts", Py::List(attempts.to_vec())),
        ("metadata", metadata.clone()),
        (
            "context",
            Py::dict([
                ("project", Py::from(project)),
                ("topic_path", Py::from(args.context_topic_path.as_str())),
                ("file_name", Py::from(args.context_file_name.as_str())),
                (
                    "orchestrator_url",
                    Py::from(args.contextlattice_url.as_str()),
                ),
            ]),
        ),
        ("local_path", Py::from(py::path_str(local_path))),
    ]);
    println!("{}", py::dumps(&payload, Some(2), true));
}

pub fn run(args: AutomationSummaryArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    require_existing_dir(&repo_root, "repo root")?;
    let (metadata, markdown) = resolve_markdown_and_metadata(&args, &repo_root)?;
    let project = if args.context_project.trim().is_empty() {
        repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo")
            .to_string()
    } else {
        args.context_project.trim().to_string()
    };

    let local_path: PathBuf = if args.local_path.is_empty() {
        repo_root.join(".sync-reports/automation-summary-fallback.log")
    } else {
        py::expanduser(&args.local_path)?
    };

    let (ok, primary_sink, attempts) =
        try_sinks(&args, &repo_root, &project, &local_path, &markdown);
    print_summary_output(
        &args,
        &project,
        &local_path,
        ok,
        &primary_sink,
        &attempts,
        &metadata,
    );
    Ok(if ok { 0 } else { 1 })
}
