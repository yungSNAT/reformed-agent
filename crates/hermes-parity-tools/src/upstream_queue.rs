//! Generate the upstream patch queue with the recorded parity disposition policy.

mod classification;
mod state;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use clap::Args;

use crate::decimal_limit::{self, DecimalLimit};
use crate::git::{ensure_remote, fetch_remote_branch, git, git_text, python_lines, python_words};
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_reports};
use classification::{classify_ticket, python_test_only, rust_primary_surface_only, ticket_name};
use state::{shown_path, Disposition, Overrides, Prior};

#[derive(Args, Debug)]
pub struct UpstreamQueueArgs {
    #[arg(long, default_value = ".")]
    repo_root: String,
    #[arg(long, default_value = "main")]
    local_ref: String,
    #[arg(long, default_value = "upstream")]
    upstream_remote: String,
    #[arg(
        long,
        default_value = "https://github.com/NousResearch/hermes-agent.git"
    )]
    upstream_url: String,
    #[arg(long, default_value = "main")]
    upstream_branch: String,
    #[arg(long)]
    no_fetch: bool,
    /// Maximum commits to include. Zero means all.
    #[arg(long, default_value = "0", allow_hyphen_values = true, value_parser = decimal_limit::parse)]
    max_commits: DecimalLimit,
    /// Queue JSON output path relative to the repository root.
    #[arg(long, default_value = "docs/parity/upstream-missing-queue.json")]
    out_json: String,
    /// Queue Markdown output path relative to the repository root.
    #[arg(long, default_value = "docs/parity/upstream-missing-queue.md")]
    out_md: String,
    /// Git ref holding the previous queue. Defaults to --local-ref.
    #[arg(long)]
    prior_ref: Option<String>,
    /// Keep all revisions when local and upstream have no merge base.
    #[arg(long)]
    no_prior_baseline: bool,
    /// Disable the dedicated guard for upstream Python test commits.
    #[arg(long)]
    allow_python_test_surfaces: bool,
    /// JSON with SHA and subject pattern disposition overrides.
    #[arg(long, default_value = "docs/parity/queue-overrides.json")]
    overrides: String,
}

#[derive(Default)]
struct Commit {
    sha: String,
    subject: String,
    files: BTreeSet<String>,
}

fn commits_from_log(raw: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    let mut current = Commit::default();
    for line in python_lines(raw) {
        if let Some(header) = line.strip_prefix("__H__") {
            if !current.sha.is_empty() {
                commits.push(current);
            }
            let (sha, subject) = header.split_once('\t').unwrap_or((header, ""));
            current = Commit {
                sha: py::strip(sha).to_owned(),
                subject: py::strip(subject).to_owned(),
                files: BTreeSet::new(),
            };
        } else {
            let file = py::strip(line);
            if !file.is_empty() {
                current.files.insert(file.to_owned());
            }
        }
    }
    if !current.sha.is_empty() {
        commits.push(current);
    }
    commits
}

fn equivalent_patches(root: &Path, local: &str, upstream: &str) -> Result<HashSet<String>, String> {
    let cherry = git(root, &["cherry", local, upstream])?;
    if !cherry.ok {
        return Ok(HashSet::new());
    }
    Ok(python_lines(&cherry.stdout)
        .filter_map(|line| {
            let mut fields = python_words(line);
            let (marker, sha) = (fields.next()?, fields.next()?);
            (marker == "-" && fields.next().is_none()).then(|| sha.to_owned())
        })
        .collect())
}

fn blob(root: &Path, reference: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
    let object = format!("{reference}:{path}");
    let output = Command::new("git")
        .args(["show", &object])
        .current_dir(root)
        .output()
        .map_err(|error| format!("git show {object}: {error}"))?;
    Ok(output.status.success().then_some(output.stdout))
}

struct Queue<'a> {
    root: &'a Path,
    local: &'a str,
    upstream: &'a str,
    prior: &'a Prior,
    overrides: &'a Overrides,
    patch_equivalent: &'a HashSet<String>,
    allow_python_tests: bool,
}

struct Row {
    sha: String,
    subject: String,
    files: Vec<String>,
    ticket: usize,
    disposition: Disposition,
    rule: &'static str,
    python_only: bool,
    guarded: bool,
    rust_primary: bool,
}

impl Row {
    fn report(&self, allow_python_tests: bool) -> Py {
        Py::dict([
            ("sha", self.sha.as_str().into()),
            ("subject", self.subject.as_str().into()),
            ("target_ticket", self.ticket.into()),
            ("target_ticket_name", ticket_name(self.ticket).into()),
            ("files_touched", self.files.len().into()),
            ("files_sample", Py::strs(self.files.iter().take(20))),
            ("disposition", self.disposition.status.as_str().into()),
            ("owner", self.disposition.owner.as_str().into()),
            ("notes", self.disposition.notes.as_str().into()),
            (
                "rust_only_guard",
                Py::dict([
                    ("python_test_only_commit", self.python_only.into()),
                    ("superseded_by_guard", self.guarded.into()),
                    ("allow_python_test_surfaces", allow_python_tests.into()),
                    ("rust_primary_surface_only_commit", self.rust_primary.into()),
                ]),
            ),
            ("classification_rule", self.rule.into()),
        ])
    }
}

impl Queue<'_> {
    fn mirrored(&self, files: &[String]) -> Result<bool, String> {
        if files.is_empty() {
            return Ok(false);
        }
        for file in files {
            if blob(self.root, self.local, file)? != blob(self.root, self.upstream, file)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn row(&self, commit: Commit) -> Result<Row, String> {
        let files: Vec<_> = commit.files.into_iter().collect();
        let previous = self.prior.by_sha.get(&commit.sha);
        let mut disposition = previous.cloned().unwrap_or_default();
        if disposition.status.is_empty() {
            disposition.status = "pending".to_owned();
        }
        let mut rule = if previous.is_some() {
            "prior_state"
        } else {
            "default_pending"
        };
        if let Some(value) = self.overrides.sha(&commit.sha) {
            rule = "sha_override";
            disposition.apply_override(value);
        }

        let python_only = python_test_only(&files);
        let guarded = python_only && !self.allow_python_tests;
        let rust_primary = rust_primary_surface_only(&files);
        if disposition.status == "pending" {
            // Preserve the existing precedence and release policy during the compatibility port.
            let decision = if self.patch_equivalent.contains(&commit.sha) {
                Some((
                    "ported",
                    format!("patch-equivalent commit already present on {}", self.local),
                    "patch_equivalent_cherry",
                ))
            } else if guarded {
                Some((
                    "superseded",
                    "rust-only parity guard: upstream Python test-only commit not ported"
                        .to_owned(),
                    "rust_only_python_test_guard",
                ))
            } else if rust_primary {
                Some(("superseded", "rust-primary parity policy: upstream Python runtime surface commit is covered by Rust-native architecture and tracked via Rust gates".to_owned(), "rust_primary_surface_guard"))
            } else if self.mirrored(&files)? {
                Some((
                    "mirrored",
                    format!(
                        "all touched files in {} already mirror {}",
                        self.upstream, self.local
                    ),
                    "mirrored_file_state",
                ))
            } else {
                self.overrides
                    .subject(&commit.subject)?
                    .map(|(status, note, rule)| (status, note.to_owned(), rule))
            };
            if let Some((status, note, applied_rule)) = decision {
                disposition.status = status.to_owned();
                disposition.append_note(&note);
                rule = applied_rule;
            }
        }
        let ticket = classify_ticket(&files);
        Ok(Row {
            sha: commit.sha,
            subject: commit.subject,
            files,
            ticket,
            disposition,
            rule,
            python_only,
            guarded,
            rust_primary,
        })
    }
}

struct Counts<'a> {
    tickets: BTreeMap<usize, usize>,
    dispositions: BTreeMap<&'a str, usize>,
}

impl<'a> Counts<'a> {
    fn summarise(rows: &'a [Row]) -> Self {
        let mut tickets = BTreeMap::new();
        let mut dispositions = BTreeMap::new();
        for row in rows {
            *tickets.entry(row.ticket).or_default() += 1;
            *dispositions
                .entry(row.disposition.status.as_str())
                .or_default() += 1;
        }
        Self {
            tickets,
            dispositions,
        }
    }
}

fn markdown(generated: &str, range: &str, rows: &[Row], counts: &Counts<'_>) -> String {
    let total = rows.len();
    let mut output = format!(
        "# Upstream Missing Patch Queue\n\nGenerated: `{generated}`\n\n- Range: `{range}`; total commits tracked: `{total}`.\n\n| Ticket | Label | Commit Count |\n| ---: | --- | ---: |\n"
    );
    for (ticket, count) in &counts.tickets {
        writeln!(output, "| #{ticket} | {} | {count} |", ticket_name(*ticket)).unwrap();
    }
    output.push_str("\n| Disposition | Commit Count |\n| --- | ---: |\n");
    for (disposition, count) in &counts.dispositions {
        writeln!(output, "| {disposition} | {count} |").unwrap();
    }
    output.push_str(
        "\n## First 100 Pending Commits\n\n| SHA | Ticket | Subject |\n| --- | ---: | --- |\n",
    );
    if !counts.dispositions.contains_key("pending") {
        output.push_str("| _(none)_ | - | - |\n");
    }
    for row in rows
        .iter()
        .filter(|row| row.disposition.status == "pending")
        .take(100)
    {
        let sha: String = row.sha.chars().take(12).collect();
        let subject = row.subject.replace('|', "\\|");
        writeln!(output, "| `{sha}` | #{} | {subject} |", row.ticket).unwrap();
    }
    output
}

pub fn run(args: UpstreamQueueArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    ensure_remote(&root, &args.upstream_remote, &args.upstream_url)?;
    if !args.no_fetch {
        fetch_remote_branch(&root, &args.upstream_remote, &args.upstream_branch)?;
    }
    let local = args.local_ref.as_str();
    let upstream = format!("{}/{}", args.upstream_remote, args.upstream_branch);
    let out_json = resolve_argument(&py::join(&root, &args.out_json))?;
    let overrides_path = resolve_argument(&py::join(&root, &args.overrides))?;
    let prior_ref = args
        .prior_ref
        .as_deref()
        .filter(|reference| !reference.is_empty())
        .unwrap_or(local);
    let prior = Prior::load(&root, prior_ref, &out_json)?;

    let range = format!("{local}..{upstream}");
    let log = git_text(
        &root,
        &[
            "log",
            "--reverse",
            "--no-merges",
            "--name-only",
            "--format=__H__%H%x09%s",
            &range,
        ],
        false,
    )?;
    let commits = commits_from_log(&log);
    let mut baseline = "";
    if !args.no_prior_baseline {
        let merge_base = git(&root, &["merge-base", local, &upstream])?;
        if !merge_base.ok || py::strip(&merge_base.stdout).is_empty() {
            baseline = &prior.first_sha;
        }
    }
    let skip = commits
        .iter()
        .position(|commit| commit.sha == baseline)
        .unwrap_or_default();
    let limit = if args.max_commits.count == 0 {
        usize::MAX
    } else {
        args.max_commits.count
    };
    let patch_equivalent = equivalent_patches(&root, local, &upstream)?;
    let overrides = Overrides::load(&overrides_path);
    let queue = Queue {
        root: &root,
        local,
        upstream: &upstream,
        prior: &prior,
        overrides: &overrides,
        patch_equivalent: &patch_equivalent,
        allow_python_tests: args.allow_python_test_surfaces,
    };
    let rows: Vec<_> = commits
        .into_iter()
        .skip(skip)
        .take(limit)
        .map(|commit| queue.row(commit))
        .collect::<Result<_, _>>()?;
    let counts = Counts::summarise(&rows);
    let generated = py::utc_isoformat();
    let refs = Py::dict([
        ("local_ref", local.into()),
        ("upstream_ref", upstream.as_str().into()),
        ("range", range.as_str().into()),
        ("prior_ref", prior_ref.into()),
        ("prior_source", prior.source.as_str().into()),
        ("baseline_start_sha", baseline.into()),
        ("skipped_before_baseline", skip.into()),
    ]);
    let summary = Py::dict([
        ("total_commits", rows.len().into()),
        (
            "by_target_ticket",
            Py::dict(
                counts
                    .tickets
                    .iter()
                    .map(|(ticket, count)| (ticket.to_string(), (*count).into())),
            ),
        ),
        (
            "by_disposition",
            Py::dict(
                counts
                    .dispositions
                    .iter()
                    .map(|(status, count)| (*status, (*count).into())),
            ),
        ),
        ("overrides_path", shown_path(&overrides_path, &root).into()),
        ("patch_equivalent_count", patch_equivalent.len().into()),
    ]);
    let report = Py::dict([
        ("generated_at_utc", generated.as_str().into()),
        ("refs", refs),
        ("summary", summary),
        (
            "commits",
            Py::List(
                rows.iter()
                    .map(|row| row.report(args.allow_python_test_surfaces))
                    .collect(),
            ),
        ),
    ]);
    write_reports(
        &root,
        &args.out_json,
        &args.out_md,
        py::dumps(&report, Some(2), true) + "\n",
        markdown(&generated, &range, &rows, &counts),
    )?;
    Ok(0)
}

#[cfg(test)]
#[path = "upstream_queue_coverage_gen.rs"]
mod coverage_gen;
