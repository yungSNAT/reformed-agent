//! Source catalogue used by the Python coverage audit.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

pub(crate) type TestCatalogue = BTreeMap<PathBuf, BTreeSet<String>>;

static TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#\[\s*(?:tokio::)?test(?:\([^]]*\))?\s*\]").unwrap());
static FUNCTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap());
static INCLUDE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"include!\("([^"]+)"\)"#).unwrap());
static MODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(?:pub\(crate\)\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap()
});

fn sources(
    root: &Path,
    relative: &Path,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(root.join(relative)) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        let path = relative.join(entry.file_name());
        if kind.is_dir() {
            sources(root, &path, files)?;
        } else if path.extension() == Some(OsStr::new("rs")) && entry.path().is_file() {
            let bytes =
                fs::read(entry.path()).map_err(|error| format!("{}: {error}", path.display()))?;
            let text = String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            files.insert(path, text);
        }
    }
    Ok(())
}

fn children(file: &Path, module: &str) -> Vec<PathBuf> {
    let parent = file.parent().unwrap_or(Path::new(""));
    let stem = file.file_stem().and_then(OsStr::to_str).unwrap_or_default();
    let base = if stem == "mod" {
        parent.to_path_buf()
    } else {
        file.with_extension("")
    };
    let filename = format!("{module}.rs");
    let module_file = Path::new(module).join("mod.rs");
    let mut paths = vec![base.join(&filename), base.join(&module_file)];
    if stem == "tests" || stem.ends_with("_tests") {
        paths.extend([
            parent.join("tests").join(filename),
            parent.join("tests").join(module_file),
        ]);
    }
    paths
}

fn included_tests(
    file: &Path,
    files: &BTreeMap<PathBuf, String>,
    tests: &TestCatalogue,
    seen: &mut HashSet<PathBuf>,
    names: &mut BTreeSet<String>,
) {
    let Some(text) = files.get(file) else {
        return;
    };
    if !seen.insert(file.to_path_buf()) {
        return;
    }
    if let Some(own) = tests.get(file) {
        names.extend(own.iter().cloned());
    }
    let parent = file.parent().unwrap_or(Path::new(""));
    for capture in INCLUDE.captures_iter(text) {
        included_tests(&parent.join(&capture[1]), files, tests, seen, names);
    }
    for capture in MODULE.captures_iter(text) {
        for child in children(file, &capture[1]) {
            included_tests(&child, files, tests, seen, names);
        }
    }
}

pub(crate) fn scan(root: &Path) -> Result<TestCatalogue, String> {
    let mut files = BTreeMap::new();
    for directory in ["crates", "tests"] {
        sources(root, Path::new(directory), &mut files)?;
    }
    let mut tests = TestCatalogue::new();
    for (file, text) in &files {
        let mut names = BTreeSet::new();
        for capture in FUNCTION.captures_iter(text) {
            let prefix = &text[..capture.get(0).expect("whole match").start()];
            let start = prefix
                .char_indices()
                .rev()
                .nth(699)
                .map_or(0, |(index, _)| index);
            if TEST.is_match(&prefix[start..]) {
                names.insert(capture[1].to_string());
            }
        }
        if !names.is_empty() {
            tests.insert(file.clone(), names);
        }
    }
    for (file, text) in &files {
        if text.contains("include!(") || MODULE.is_match(text) {
            let mut names = BTreeSet::new();
            included_tests(file, &files, &tests, &mut HashSet::new(), &mut names);
            if !names.is_empty() {
                if file.file_name() == Some(OsStr::new("mod.rs")) {
                    if let Some(parent) = file.parent() {
                        tests
                            .entry(parent.with_extension("rs"))
                            .or_default()
                            .extend(names.iter().cloned());
                    }
                }
                tests.entry(file.clone()).or_default().extend(names);
            }
        }
    }
    Ok(tests)
}
