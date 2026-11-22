//! Load the previous queue and apply configured disposition overrides.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::git::git;
use crate::py::{self, Py};

#[derive(Clone)]
pub(super) struct Disposition {
    pub status: String,
    pub notes: String,
    pub owner: String,
}

impl Default for Disposition {
    fn default() -> Self {
        Self {
            status: "pending".to_owned(),
            notes: String::new(),
            owner: String::new(),
        }
    }
}

impl Disposition {
    pub(super) fn append_note(&mut self, note: &str) {
        if note.is_empty() {
            return;
        }
        if !self.notes.is_empty() {
            self.notes.push_str(" | ");
        }
        self.notes.push_str(note);
    }

    pub(super) fn apply_override(&mut self, value: &Self) {
        if !value.status.is_empty() {
            self.status.clone_from(&value.status);
        }
        if !value.notes.is_empty() {
            self.notes.clone_from(&value.notes);
        }
        if !value.owner.is_empty() {
            self.owner.clone_from(&value.owner);
        }
    }
}

fn text(item: &Py, key: &str, default: &str) -> String {
    item.get(key).map_or_else(|| default.to_owned(), py::str_of)
}

fn trimmed(item: &Py, key: &str) -> String {
    py::strip(&text(item, key, "")).to_owned()
}

fn read_json(path: &Path) -> Py {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| py::loads(&text).ok())
        .unwrap_or_else(|| Py::Dict(Vec::new()))
}

fn nonempty(value: &Py) -> bool {
    match value {
        Py::Null => false,
        Py::Bool(value) => *value,
        Py::Int(value) => *value != 0,
        Py::Float(value) => *value != 0.0,
        Py::Str(value) => !value.is_empty(),
        Py::List(value) => !value.is_empty(),
        Py::Dict(value) => !value.is_empty(),
    }
}

pub(super) fn shown_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| py::path_str(path),
        |relative| py::path_str(relative).replace('\\', "/"),
    )
}

fn unmodified_prior(root: &Path, prior_ref: &str, output: &Path) -> Result<Option<Py>, String> {
    if !output.starts_with(root) {
        return Ok(None);
    }
    let previous_file = format!("{prior_ref}:{}", shown_path(output, root));
    let result = git(root, &["show", &previous_file])?;
    Ok(result
        .ok
        .then(|| py::loads(&result.stdout).ok())
        .flatten()
        .filter(nonempty))
}

pub(super) struct Prior {
    pub source: String,
    pub first_sha: String,
    pub by_sha: HashMap<String, Disposition>,
}

impl Prior {
    pub(super) fn load(root: &Path, prior_ref: &str, output: &Path) -> Result<Self, String> {
        let previous = unmodified_prior(root, prior_ref, output)?;
        let source = if previous.is_some() {
            prior_ref
        } else {
            "worktree"
        }
        .to_owned();
        let previous = previous.unwrap_or_else(|| read_json(output));
        if !previous.is_dict() {
            return Err(format!("previous queue in {source} must contain an object"));
        }
        let commits = match previous.get("commits") {
            Some(Py::List(commits)) => commits.as_slice(),
            Some(value)
                if nonempty(value)
                    || matches!(value, Py::Null | Py::Bool(_) | Py::Int(_) | Py::Float(_)) =>
            {
                return Err(format!("previous queue commits in {source} must be a list"));
            }
            _ => &[],
        };
        let first_sha = commits
            .first()
            .map_or_else(String::new, |row| trimmed(row, "sha"));
        let mut by_sha = HashMap::new();
        for commit in commits {
            if !commit.is_dict() {
                return Err(format!(
                    "previous queue commit in {source} must contain an object"
                ));
            }
            let sha = trimmed(commit, "sha");
            if !sha.is_empty() {
                by_sha.insert(
                    sha,
                    Disposition {
                        status: text(commit, "disposition", "pending"),
                        notes: text(commit, "notes", ""),
                        owner: text(commit, "owner", ""),
                    },
                );
            }
        }
        Ok(Self {
            source,
            first_sha,
            by_sha,
        })
    }
}

struct SubjectPattern {
    regex: Result<Regex, fancy_regex::Error>,
    disposition: String,
    notes: String,
}

pub(super) struct Overrides {
    shas: Vec<(String, Disposition)>,
    subjects: Vec<SubjectPattern>,
}

const DEFAULT_SUBJECTS: &[(&str, &str)] = &[
    (
        r"^chore\(release\): map .+ in AUTHOR_MAP$",
        "release metadata-only AUTHOR_MAP update from upstream Python pipeline",
    ),
    (
        r"^chore\(release\): add .+ to AUTHOR_MAP",
        "release metadata-only AUTHOR_MAP update from upstream Python pipeline",
    ),
    (
        r"^review\(.+\): .+$",
        "review-only upstream nit commit; no Rust-runtime behavior delta",
    ),
];

static DEFAULT_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    DEFAULT_SUBJECTS
        .iter()
        .map(|(pattern, note)| {
            (
                Regex::new(&format!("(?i){pattern}")).expect("valid default pattern"),
                *note,
            )
        })
        .collect()
});

impl Overrides {
    pub(super) fn load(path: &Path) -> Self {
        let document = read_json(path);
        let mut shas: Vec<(String, Disposition)> = Vec::new();
        if let Some(Py::Dict(rows)) = document.get("sha") {
            for (sha, row) in rows.iter().filter(|(_, row)| row.is_dict()) {
                let sha = py::strip(sha);
                let value = Disposition {
                    status: trimmed(row, "disposition"),
                    notes: trimmed(row, "notes"),
                    owner: trimmed(row, "owner"),
                };
                if let Some((_, existing)) = shas.iter_mut().find(|(key, _)| key == sha) {
                    *existing = value;
                } else {
                    shas.push((sha.to_owned(), value));
                }
            }
        }
        let mut subjects = Vec::new();
        if let Some(Py::List(rows)) = document.get("subject_patterns") {
            for row in rows.iter().filter(|row| row.is_dict()) {
                let pattern = trimmed(row, "pattern");
                let disposition = trimmed(row, "disposition");
                if !pattern.is_empty() && !disposition.is_empty() {
                    subjects.push(SubjectPattern {
                        regex: Regex::new(&format!("(?i){pattern}")),
                        disposition,
                        notes: trimmed(row, "notes"),
                    });
                }
            }
        }
        Self { shas, subjects }
    }

    pub(super) fn sha(&self, sha: &str) -> Option<&Disposition> {
        self.shas
            .iter()
            .find(|(key, _)| key == sha)
            .or_else(|| {
                self.shas
                    .iter()
                    .find(|(key, _)| !key.is_empty() && sha.starts_with(key))
            })
            .map(|(_, value)| value)
    }

    pub(super) fn subject(
        &self,
        subject: &str,
    ) -> Result<Option<(&str, &str, &'static str)>, String> {
        for pattern in &self.subjects {
            let regex = pattern
                .regex
                .as_ref()
                .map_err(|error| format!("invalid override pattern: {error}"))?;
            if regex.is_match(subject).map_err(|error| error.to_string())? {
                return Ok(Some((
                    &pattern.disposition,
                    &pattern.notes,
                    "subject_override_pattern",
                )));
            }
        }
        let subject = py::strip(subject);
        for (pattern, note) in DEFAULT_PATTERNS.iter() {
            if pattern
                .is_match(subject)
                .map_err(|error| error.to_string())?
            {
                return Ok(Some(("superseded", note, "default_subject_pattern")));
            }
        }
        Ok(None)
    }
}
