//! Generate test-intent reports from the evidence domains shared with the coverage audit.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use regex::Regex;

use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_reports};

const DOMAINS: &str = include_str!("../../../docs/parity/test-intent-domains.json");

#[derive(Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct EvidencePath(Vec<u32>);

impl EvidencePath {
    fn from_relative_path(path: &Path) -> Self {
        let mut code_points = Vec::new();
        for (index, component) in path.iter().enumerate() {
            if index != 0 {
                code_points.push(u32::from('/'));
            }
            append_code_points(component, &mut code_points);
        }
        Self(code_points)
    }

    /// The path as Python's `json.dumps` would quote it; the tests use it to show the order.
    #[cfg(test)]
    pub(crate) fn json(&self) -> String {
        let mut units = Vec::new();
        for &point in &self.0 {
            if let Some(character) = char::from_u32(point) {
                units.extend_from_slice(character.encode_utf16(&mut [0; 2]));
            } else {
                units.push(u16::try_from(point).expect("a surrogate fits in one UTF-16 unit"));
            }
        }
        py::json_utf16_string(units)
    }
}

#[cfg(unix)]
fn append_code_points(component: &OsStr, points: &mut Vec<u32>) {
    use std::os::unix::ffi::OsStrExt;

    for chunk in component.as_bytes().utf8_chunks() {
        points.extend(chunk.valid().chars().map(u32::from));
        // Python maps each undecodable Unix filename byte to a low surrogate.
        points.extend(chunk.invalid().iter().map(|&byte| 0xdc00 + u32::from(byte)));
    }
}

#[cfg(not(any(unix, windows)))]
fn append_code_points(component: &OsStr, points: &mut Vec<u32>) {
    points.extend(component.to_string_lossy().chars().map(u32::from));
}

#[cfg(windows)]
fn append_code_points(component: &OsStr, points: &mut Vec<u32>) {
    use std::os::windows::ffi::OsStrExt;

    points.extend(char::decode_utf16(component.encode_wide()).map(|decoded| {
        decoded.map_or_else(|error| u32::from(error.unpaired_surrogate()), u32::from)
    }));
}

#[derive(Args, Debug)]
pub struct TestIntentMappingArgs {
    /// Repository root path
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Output JSON path relative to the repository root
    #[arg(long, default_value = "docs/parity/test-intent-mapping.json")]
    out_json: String,
    /// Output Markdown path relative to the repository root
    #[arg(long, default_value = "docs/parity/test-intent-mapping.md")]
    out_md: String,
}

enum GlobSegment<'a> {
    Literal(&'a str),
    Pattern(Regex),
    Recursive,
}

impl<'a> GlobSegment<'a> {
    fn parse(segment: &'a str) -> Result<Self, String> {
        if segment == "**" {
            return Ok(Self::Recursive);
        }
        if !segment.contains('*') {
            return Ok(Self::Literal(segment));
        }
        let mode = if cfg!(windows) { "(?is)" } else { "(?s)" };
        let expression = regex::escape(segment).replace(r"\*", ".*");
        let pattern = Regex::new(&format!(r"{mode}\A{expression}\z"))
            .map_err(|error| format!("invalid evidence pattern {segment:?}: {error}"))?;
        Ok(Self::Pattern(pattern))
    }
}

fn collect_matches(
    root: &Path,
    current: &Path,
    segments: &[GlobSegment<'_>],
    matches: &mut BTreeMap<EvidencePath, PathBuf>,
) -> Result<(), String> {
    match segments.split_first() {
        None if current.is_file() => {
            let relative = current
                .strip_prefix(root)
                .map_err(|error| error.to_string())?;
            matches.insert(
                EvidencePath::from_relative_path(relative),
                relative.to_owned(),
            );
        }
        None => {}
        Some((GlobSegment::Literal(name), rest)) => {
            collect_matches(root, &current.join(name), rest, matches)?;
        }
        Some((GlobSegment::Pattern(pattern), rest)) => {
            for entry in fs::read_dir(current).into_iter().flatten().flatten() {
                // Lossy decoding preserves the ASCII literals and wildcards
                // used by the fixed patterns. Reports retain the raw names.
                if pattern.is_match(&entry.file_name().to_string_lossy()) {
                    collect_matches(root, &entry.path(), rest, matches)?;
                }
            }
        }
        Some((GlobSegment::Recursive, rest)) => {
            collect_matches(root, current, rest, matches)?;
            // The '**' segment skips symlinked directories, as pathlib does.
            for entry in fs::read_dir(current).into_iter().flatten().flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    collect_matches(root, &entry.path(), segments, matches)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn evidence(
    root: &Path,
    patterns: &[Py],
) -> Result<BTreeMap<EvidencePath, PathBuf>, String> {
    let mut paths = BTreeMap::new();
    for pattern in patterns {
        let Py::Str(pattern) = pattern else {
            return Err("invalid evidence pattern in test-intent-domains.json".into());
        };
        let segments = pattern
            .split('/')
            .map(GlobSegment::parse)
            .collect::<Result<Vec<_>, _>>()?;
        collect_matches(root, root, &segments, &mut paths)?;
    }
    Ok(paths)
}

/// One intent row. The evidence count and sample are left out of the committed report: they
/// change whenever a file matching a domain glob is added, so PRs that regenerate it conflicted.
fn report_row(id: &str, scope: &Py, mapped: bool) -> String {
    let id = py::dumps(&id.into(), None, false);
    let scope = py::dumps(scope, Some(2), true).replace('\n', "\n      ");
    format!(
        r#"    {{
      "id": {id},
      "mapped": {mapped},
      "upstream_scope": {scope}
    }}"#
    )
}

fn generate(root: &Path) -> Result<(String, String), String> {
    let Py::List(domains) = py::loads(DOMAINS)? else {
        return Err("expected test-intent domains to be a list".into());
    };
    let mut rows = Vec::with_capacity(domains.len());
    let mut mapped = 0;
    let mut markdown =
        String::from("# Test Intent Mapping\n\n| Intent | Mapped |\n| --- | --- |\n");
    for domain in &domains {
        let (Some(Py::Str(id)), Some(scope @ Py::List(_)), Some(Py::List(patterns))) = (
            domain.get("id"),
            domain.get("upstream_scope"),
            domain.get("local_evidence_globs"),
        ) else {
            return Err("invalid entry in test-intent-domains.json".into());
        };
        let is_mapped = !evidence(root, patterns)?.is_empty();
        mapped += usize::from(is_mapped);
        rows.push(report_row(id, scope, is_mapped));
        let label = if is_mapped { "yes" } else { "no" };
        writeln!(markdown, "| `{id}` | {label} |").expect("writing to String");
    }
    let total = domains.len();
    let raw_ratio = if total == 0 {
        0.0
    } else {
        mapped as f64 / total as f64
    };
    let ratio = Py::Float((raw_ratio * 10_000.0).round_ties_even() / 10_000.0);
    writeln!(
        markdown,
        "\n- Mapping ratio: `{mapped}` / `{total}` = `{}`.",
        py::dumps(&ratio, None, false)
    )
    .expect("writing to String");
    let summary = Py::dict([
        ("total_intents", total.into()),
        ("mapped_intents", mapped.into()),
        ("unmapped_intents", (total - mapped).into()),
        ("mapping_ratio", ratio),
    ]);
    let summary = py::dumps(&summary, Some(2), true).replace('\n', "\n  ");
    let intents = if rows.is_empty() {
        "[]".to_string()
    } else {
        format!("[\n{}\n  ]", rows.join(",\n"))
    };
    let json = format!(
        r#"{{
  "intent_unit": "domain_intent",
  "intents": {intents},
  "summary": {summary}
}}
"#
    );
    Ok((json, markdown))
}

pub fn run(args: TestIntentMappingArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let (report, markdown) = generate(&root)?;
    write_reports(&root, &args.out_json, &args.out_md, report, markdown)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(paths: &BTreeMap<EvidencePath, PathBuf>) -> Vec<String> {
        paths.keys().map(EvidencePath::json).collect()
    }

    #[test]
    fn evidence_is_sorted_deduplicated_and_matches_each_glob_depth() {
        let root = tempfile::tempdir().unwrap();
        for relative in [
            "src/z.rs",
            "src/core/.hidden.rs",
            "src/core/a.rs",
            "src/core/readme.md",
            "src/.hidden/b.rs",
            "fixture/expected.json",
        ] {
            let path = root.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let direct = evidence(root.path(), &["src/*.rs".into()]).unwrap();
        assert_eq!(quoted(&direct), [r#""src/z.rs""#]);
        let mixed = evidence(
            root.path(),
            &[
                "src/*.rs".into(),
                "src/**/*.rs".into(),
                "fixture/expected.json".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            quoted(&mixed),
            [
                r#""fixture/expected.json""#,
                r#""src/.hidden/b.rs""#,
                r#""src/core/.hidden.rs""#,
                r#""src/core/a.rs""#,
                r#""src/z.rs""#
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn recursive_segments_skip_directory_links_and_keep_file_links() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("source")).unwrap();
        fs::write(root.path().join("source/a.rs"), "").unwrap();
        symlink("source", root.path().join("directory-alias")).unwrap();
        symlink("source/a.rs", root.path().join("file-alias.rs")).unwrap();
        let recursive = evidence(root.path(), &["**/*.rs".into()]).unwrap();
        assert_eq!(
            quoted(&recursive),
            [r#""file-alias.rs""#, r#""source/a.rs""#]
        );
        let explicit = evidence(root.path(), &["directory-alias/**/*.rs".into()]).unwrap();
        assert_eq!(quoted(&explicit), [r#""directory-alias/a.rs""#]);
    }

    #[cfg(unix)]
    #[test]
    fn undecodable_paths_survive_matching_deduplication_and_json() {
        use std::os::unix::ffi::OsStrExt;

        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(OsStr::from_bytes(b"dir-\xff"));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join(OsStr::from_bytes(b"child-\xe2\x82.rs")), "").unwrap();
        fs::write(root.path().join(OsStr::from_bytes(b"dir-\xff.rs")), "").unwrap();
        fs::write(root.path().join("dir-�.rs"), "").unwrap();
        let paths = evidence(root.path(), &["*.rs".into(), "**/*.rs".into()]).unwrap();
        assert_eq!(
            quoted(&paths),
            [
                r#""dir-\udcff.rs""#,
                r#""dir-\udcff/child-\udce2\udc82.rs""#,
                r#""dir-\ufffd.rs""#
            ]
        );
    }

    #[test]
    fn reports_stay_byte_identical_when_an_evidence_file_is_added() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let platforms = root.join("crates/hermes-gateway/src/platforms");
        fs::create_dir_all(&platforms).unwrap();
        let reports = || {
            let args = TestIntentMappingArgs {
                repo_root: root.to_str().unwrap().into(),
                out_json: "mapping.json".into(),
                out_md: "mapping.md".into(),
            };
            assert_eq!(run(args).unwrap(), 0);
            (
                fs::read_to_string(root.join("mapping.json")).unwrap(),
                fs::read_to_string(root.join("mapping.md")).unwrap(),
            )
        };
        fs::write(platforms.join("first.rs"), "// first adapter\n").unwrap();
        let (json, markdown) = reports();

        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(platforms.join("second.rs"), "// second adapter\n").unwrap();
        let (json_after, markdown_after) = reports();

        assert_eq!(json_after, json);
        assert_eq!(markdown_after, markdown);
        for volatile in ["generated_at_utc", "evidence_count", "evidence_sample"] {
            assert!(!json.contains(volatile), "{volatile} in {json}");
        }
        assert!(json.contains("\"mapped\": true"), "{json}");
    }
}
