//! Report the cases and evidence behind the deep problem-solving gate.

mod markdown;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use clap::Args;
use regex::Regex;

use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_report};

static TEST_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^#\[\s*(?:tokio::)?test(?:\([^]]*\))?\s*\]$").expect("test attribute")
});
static RUST_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(").expect("Rust function"));

const PASSING: [&str; 3] = ["equivalent", "superior", "intentional_divergence"];
const OTHER_CLASSES: [&str; 3] = ["regression", "gap", "unverified"];

#[derive(Args, Debug)]
pub struct DeepProblemSolvingArgs {
    #[arg(long, default_value = ".")]
    repo_root: String,
    #[arg(long, default_value = "docs/parity/deep-problem-solving-cases.json")]
    cases: String,
    #[arg(long, default_value = "docs/parity/deep-problem-solving-diff.json")]
    out_json: String,
    #[arg(long, default_value = "docs/parity/deep-problem-solving-diff.md")]
    out_md: String,
    #[arg(long, default_value_t = 8, allow_negative_numbers = true)]
    min_superiority_cases: i64,
    #[arg(long, default_value_t = 10, allow_negative_numbers = true)]
    min_total_cases: i64,
    /// Return failure for an unmet gate without writing the reports.
    #[arg(long)]
    check: bool,
}

type TestsByFile = BTreeMap<String, BTreeSet<String>>;

fn attached_test_attr(source: &str, function: usize) -> bool {
    source[..function]
        .rsplit_once('\n')
        .is_some_and(|(prefix, _)| {
            prefix
                .lines()
                .rev()
                .map(py::strip)
                .take_while(|line| line.starts_with("#["))
                .any(|line| TEST_ATTR.is_match(line))
        })
}

fn is_rust_source(path: &Path) -> bool {
    path.extension().is_some_and(|suffix| {
        suffix == "rs" || (cfg!(windows) && suffix.eq_ignore_ascii_case("rs"))
    })
}

fn scan_tests(root: &Path, directory: &Path, tests: &mut TestsByFile) -> Result<(), String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", directory.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            scan_tests(root, &path, tests)?;
        } else if is_rust_source(&path) && path.is_file() {
            let bytes = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let text = String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            let names: BTreeSet<String> = RUST_FN
                .captures_iter(&text)
                .filter(|capture| {
                    attached_test_attr(&text, capture.get(0).expect("whole match").start())
                })
                .map(|capture| capture[1].to_string())
                .collect();
            if !names.is_empty() {
                let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
                let mut file = py::path_str(relative);
                if cfg!(windows) {
                    file = file.replace('\\', "/");
                }
                tests.insert(file, names);
            }
        }
    }
    Ok(())
}

fn as_list(value: Option<&Py>) -> &[Py] {
    match value {
        Some(Py::List(values)) => values,
        _ => &[],
    }
}

fn field_text(case: &Py, field: &str) -> String {
    case.get(field).map(py::str_of).unwrap_or_default()
}

fn issue(id: &str, kind: &str, message: &str) -> Py {
    Py::dict([
        ("case_id", id.into()),
        ("kind", kind.into()),
        ("message", message.into()),
    ])
}

fn validate_case(root: &Path, tests: &TestsByFile, mut case: Py) -> Py {
    let raw_id = field_text(&case, "id");
    let id = py::strip(&raw_id);
    let raw_class = field_text(&case, "classification");
    let class = py::strip(&raw_class);
    let mut issues = Vec::new();
    if id.is_empty() {
        issues.push(issue("<missing-id>", "schema", "case id is required"));
    }
    if !PASSING.contains(&class) && !OTHER_CLASSES.contains(&class) {
        let schema_id = if id.is_empty() { "<missing-id>" } else { id };
        let mut invalid = issue(schema_id, "schema", "classification is not allowed");
        invalid.set("classification", class.into());
        issues.push(invalid);
    }

    let artifacts: Vec<_> = as_list(case.get("source_artifacts"))
        .iter()
        .map(py::str_of)
        .map(|path| py::strip(&path).to_owned())
        .filter(|path| !path.is_empty())
        .collect();
    if artifacts.is_empty() {
        issues.push(issue(
            id,
            "missing_source_artifacts",
            "case has no source artifacts",
        ));
    }
    let mut missing_artifacts = Vec::new();
    for path in artifacts
        .into_iter()
        .filter(|path| !py::join(root, path).exists())
    {
        let mut missing = issue(
            id,
            "missing_source_artifact",
            "source artifact does not exist",
        );
        missing.set("path", path.as_str().into());
        issues.push(missing);
        missing_artifacts.push(path);
    }

    let mut refs = as_list(case.get("rust_tests"))
        .iter()
        .filter(|test| test.is_dict())
        .peekable();
    if refs.peek().is_none() {
        issues.push(issue(
            id,
            "missing_rust_tests",
            "case has no Rust test references",
        ));
    }
    let mut missing_refs = Vec::new();
    for reference in refs {
        let raw_file = field_text(reference, "file");
        let raw_name = field_text(reference, "name");
        let file = py::strip(&raw_file);
        let name = py::strip(&raw_name);
        if file.is_empty()
            || name.is_empty()
            || !tests.get(file).is_some_and(|names| names.contains(name))
        {
            let mut missing = issue(
                id,
                "missing_rust_test_ref",
                "referenced Rust test was not found",
            );
            missing.set("file", file.into());
            missing.set("name", name.into());
            issues.push(missing);
            missing_refs.push(Py::dict([("file", file.into()), ("name", name.into())]));
        }
    }

    for field in ["domain", "upstream_behavior", "ultra_behavior", "why"] {
        if py::strip(&field_text(&case, field)).is_empty() {
            issues.push(issue(id, "schema", &format!("{field} is required")));
        }
    }

    let verified = issues.is_empty() && class != "unverified";
    let score = if PASSING.contains(&class) { 1.0 } else { 0.0 };
    case.set("score", score.into());
    case.set("issues", Py::List(issues));
    case.set("missing_rust_test_refs", Py::List(missing_refs));
    case.set("missing_source_artifacts", Py::strs(missing_artifacts));
    case.set("verified", verified.into());
    case
}

fn counts_json(counts: BTreeMap<String, usize>) -> Py {
    Py::dict(counts.into_iter().map(|(key, count)| (key, count.into())))
}

fn count_missing(cases: &[Py], key: &str) -> usize {
    cases.iter().map(|case| as_list(case.get(key)).len()).sum()
}

fn failure(kind: &str, message: &str, actual: Py, limit: Py) -> Py {
    Py::dict([
        ("kind", kind.into()),
        ("message", message.into()),
        ("actual", actual),
        ("limit", limit),
    ])
}

struct Evidence {
    summary: Py,
    gate: Py,
    failures: Vec<Py>,
}

fn evidence(cases: &[Py], min_superior: i128, min_total: i128) -> Result<Evidence, String> {
    let mut classifications = BTreeMap::new();
    let mut domains = BTreeMap::new();
    let mut equal_or_better = 0_usize;
    for case in cases {
        let classification = case
            .get("classification")
            .ok_or("case classification is missing")?;
        if matches!(classification, Py::List(_) | Py::Dict(_)) {
            return Err("case classification must be hashable".to_string());
        }
        if let Py::Str(value) = classification {
            equal_or_better += usize::from(PASSING.contains(&value.as_str()));
        }
        *classifications
            .entry(py::str_of(classification))
            .or_insert(0_usize) += 1;
        *domains.entry(field_text(case, "domain")).or_insert(0_usize) += 1;
    }
    let count = |class| classifications.get(class).copied().unwrap_or(0);
    let total = cases.len();
    let superior = count("superior");
    let missing_refs = count_missing(cases, "missing_rust_test_refs");
    let missing_artifacts = count_missing(cases, "missing_source_artifacts");
    let ratio = if total == 0 {
        0.0
    } else {
        format!("{:.4}", equal_or_better as f64 / total as f64)
            .parse::<f64>()
            .expect("decimal ratio")
    };

    let mut failures = Vec::new();
    if (total as i128) < min_total {
        failures.push(failure(
            "total_cases",
            "deep problem-solving case count is below threshold",
            total.into(),
            Py::Int(min_total),
        ));
    }
    if ratio < 1.0 {
        failures.push(failure(
            "deep_problem_solving_ratio",
            "deep problem-solving ratio is below 1.0",
            ratio.into(),
            1.0.into(),
        ));
    }
    if (superior as i128) < min_superior {
        failures.push(failure(
            "superiority_cases",
            "strictly superior deep problem-solving cases are below threshold",
            superior.into(),
            Py::Int(min_superior),
        ));
    }
    for (kind, message, actual) in [
        (
            "regressions",
            "deep problem-solving regressions are present",
            count("regression"),
        ),
        (
            "gaps",
            "deep problem-solving gaps are present",
            count("gap"),
        ),
        (
            "unverified",
            "unverified deep problem-solving cases are present",
            count("unverified"),
        ),
        (
            "missing_rust_test_refs",
            "deep problem-solving cases reference missing Rust tests",
            missing_refs,
        ),
        (
            "missing_source_artifacts",
            "deep problem-solving cases reference missing source artifacts",
            missing_artifacts,
        ),
    ] {
        if actual > 0 {
            failures.push(failure(kind, message, actual.into(), Py::Int(0)));
        }
    }

    let summary = Py::dict([
        ("total_cases", total.into()),
        ("equal_or_better_cases", equal_or_better.into()),
        ("superior_cases", superior.into()),
        ("equivalent_cases", count("equivalent").into()),
        (
            "intentional_divergence_cases",
            count("intentional_divergence").into(),
        ),
        ("regressions", count("regression").into()),
        ("gaps", count("gap").into()),
        ("unverified_cases", count("unverified").into()),
        ("missing_rust_test_refs", missing_refs.into()),
        ("missing_source_artifacts", missing_artifacts.into()),
        ("deep_problem_solving_ratio", ratio.into()),
        ("classification_counts", counts_json(classifications)),
        ("domain_counts", counts_json(domains)),
        ("min_superiority_cases", Py::Int(min_superior)),
        ("min_total_cases", Py::Int(min_total)),
    ]);
    let gate = Py::dict([
        ("pass", failures.is_empty().into()),
        ("failures", failures.len().into()),
        ("min_deep_problem_solving_ratio", 1.0.into()),
        ("min_superiority_cases", Py::Int(min_superior)),
        ("min_total_cases", Py::Int(min_total)),
        ("max_regressions", Py::Int(0)),
        ("max_gaps", Py::Int(0)),
        ("max_unverified_cases", Py::Int(0)),
        ("max_missing_rust_test_refs", Py::Int(0)),
    ]);
    Ok(Evidence {
        summary,
        gate,
        failures,
    })
}

pub fn run(args: DeepProblemSolvingArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let path = resolve_argument(&py::join(&root, &args.cases))?;
    let source =
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let manifest = py::loads(&source)?;
    if !manifest.is_dict() {
        return Err("deep problem-solving manifest must be an object".to_string());
    }
    let mut tests = TestsByFile::new();
    for directory in ["crates", "tests"] {
        scan_tests(&root, &root.join(directory), &mut tests)?;
    }
    let cases: Vec<_> = as_list(manifest.get("cases"))
        .iter()
        .filter(|case| case.is_dict())
        .cloned()
        .map(|case| validate_case(&root, &tests, case))
        .collect();
    let evidence = evidence(
        &cases,
        args.min_superiority_cases.into(),
        args.min_total_cases.into(),
    )?;
    let json_path = resolve_argument(&py::join(&root, &args.out_json))?;
    let markdown_path = resolve_argument(&py::join(&root, &args.out_md))?;
    if args.check {
        println!("Checked {}", py::path_str(&json_path));
        println!("Checked {}", py::path_str(&markdown_path));
        return Ok(i32::from(!evidence.failures.is_empty()));
    }

    let generated = py::utc_isoformat();
    let manifest = Path::new(&args.cases)
        .components()
        .collect::<std::path::PathBuf>();
    let mut manifest = py::path_str(&manifest);
    if manifest.is_empty() {
        manifest = ".".to_string();
    }
    if cfg!(windows) {
        manifest = manifest.replace('\\', "/");
    }
    let payload = Py::dict([
        ("generated_at_utc", generated.as_str().into()),
        ("schema_version", Py::Int(1)),
        ("manifest", manifest.into()),
        ("summary", evidence.summary.clone()),
        ("gate", evidence.gate.clone()),
        ("gate_failures", Py::List(evidence.failures.clone())),
        ("cases", Py::List(cases.clone())),
    ]);
    if let Some(parent) = json_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    write_report(&json_path, py::dumps(&payload, Some(2), true) + "\n")?;
    if let Some(parent) = markdown_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    if let Some(field) = cases.iter().find_map(|case| {
        ["id", "domain"]
            .into_iter()
            .find(|field| case.get(field).is_none())
    }) {
        return Err(format!("case {field} is missing"));
    }
    let markdown = markdown::render(&generated, &evidence, &cases);
    write_report(&markdown_path, markdown)?;
    println!("Wrote {}", py::path_str(&json_path));
    println!("Wrote {}", py::path_str(&markdown_path));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_glob_treats_uppercase_extensions_as_platform_specific() {
        let fixture = tempfile::tempdir().expect("create scan directory");
        let root = fixture.path();
        for file in ["ordinary.rs", "mixed.Rs"] {
            fs::write(root.join(file), "#[test]\\nfn cited() {}\\n").expect("write fixture");
        }
        let mut tests = TestsByFile::new();
        scan_tests(root, root, &mut tests).expect("scan fixture");
        assert!(tests["ordinary.rs"].contains("cited"));
        assert_eq!(tests.contains_key("mixed.Rs"), cfg!(windows));
    }
}
