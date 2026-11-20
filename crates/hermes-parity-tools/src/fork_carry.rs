//! Require actual execution of the fork behaviour contracts after an upstream sync.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use clap::Args;

use crate::git::git_checked;
use crate::py::{self, Py};

const REGISTRY: &str = "docs/parity/fork-carry-contracts.v1.json";

#[derive(Args, Debug)]
pub struct ForkCarryArgs {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
}

struct Required<'a> {
    id: &'a str,
    package: &'a str,
    test: &'a str,
}

struct Uncovered<'a> {
    id: &'a str,
    reason: &'a str,
}

struct Registry<'a> {
    required: Vec<Required<'a>>,
    uncovered: Vec<Uncovered<'a>>,
}

fn entries<'a>(document: &'a Py, field: &str) -> Result<&'a [Py], String> {
    match document.get(field) {
        Some(Py::List(values)) => Ok(values),
        _ => Err(format!("carry registry requires a {field} array")),
    }
}

fn field<'a>(entry: &'a Py, field: &str, context: &str) -> Result<&'a str, String> {
    match entry.get(field) {
        Some(Py::Str(value)) if !value.trim().is_empty() => Ok(value),
        _ => Err(format!("{context} requires a nonempty {field}")),
    }
}

fn id_and_reason<'a>(
    entry: &'a Py,
    seen: &mut BTreeSet<&'a str>,
) -> Result<(&'a str, &'a str), String> {
    let id = field(entry, "id", "carry entry")?;
    if !id.starts_with("carry-")
        || !id
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
        || !seen.insert(id)
    {
        return Err(format!("invalid or duplicate carry ID {id:?}"));
    }
    let reason = field(entry, "behaviour", id)?;
    Ok((id, reason))
}

fn registry(document: &Py) -> Result<Registry<'_>, String> {
    if !matches!(document.get("version"), Some(Py::Int(1))) {
        return Err("unsupported carry registry version".to_owned());
    }
    let required = entries(document, "required")?;
    if required.is_empty() {
        return Err("carry registry must contain required contracts".to_owned());
    }
    let mut ids = BTreeSet::new();
    let mut tests = BTreeSet::new();
    let mut result = Registry {
        required: Vec::new(),
        uncovered: Vec::new(),
    };
    for entry in required {
        let (id, _) = id_and_reason(entry, &mut ids)?;
        let package = field(entry, "package", id)?;
        let test = field(entry, "test", id)?;
        let simple_package = package.as_bytes()[0].is_ascii_lowercase()
            && package
                .bytes()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-');
        let qualified_test = test.contains("::")
            && test.split("::").all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
            });
        if !simple_package || !qualified_test || !tests.insert((package, test)) {
            return Err(format!(
                "{id} has an invalid or duplicate library test: {package:?} {test:?}"
            ));
        }
        result.required.push(Required { id, package, test });
    }
    for entry in entries(document, "uncovered")? {
        let (id, reason) = id_and_reason(entry, &mut ids)?;
        result.uncovered.push(Uncovered { id, reason });
    }
    Ok(result)
}

fn execution_result(test: &str, success: bool, stdout: &str) -> Result<(), &'static str> {
    let mut lines = stdout.lines();
    let result = lines.find(|line| line.starts_with("test result: "));
    if result.is_some() && lines.any(|line| line.starts_with("test result: ")) {
        return Err("ambiguous libtest result");
    }
    if result
        .is_some_and(|line| line.starts_with("test result: ok. 0 passed; 0 failed; 0 ignored;"))
    {
        return Err("exact test missing");
    }
    if result
        .is_some_and(|line| line.starts_with("test result: ok. 0 passed; 0 failed; 1 ignored;"))
    {
        return Err("exact test ignored");
    }
    if !success {
        return if result.is_some() {
            Err("test execution failed")
        } else {
            Err("Cargo test failed before a libtest result")
        };
    }
    let expected = format!("test {test} ... ok");
    if result.is_some_and(|line| {
        line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;")
    }) && stdout
        .lines()
        .filter(|line| *line == "running 1 test")
        .count()
        == 1
        && stdout.lines().filter(|line| *line == expected).count() == 1
    {
        Ok(())
    } else {
        Err("unrecognised libtest result")
    }
}

fn run_test(root: &Path, required: &Required<'_>) -> std::io::Result<Output> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Command::new(cargo)
        .current_dir(root)
        .args([
            "test",
            "--locked",
            "--color",
            "never",
            "-p",
            required.package,
            "--lib",
            required.test,
            "--",
            "--exact",
            "--format",
            "pretty",
        ])
        .output()
}

pub fn run(args: ForkCarryArgs) -> Result<i32, String> {
    let path = args.repo_root.join(REGISTRY);
    let input =
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let document = py::loads(&input).map_err(|error| format!("{}: {error}", path.display()))?;
    let registry = registry(&document)?;
    let revision = git_checked(&args.repo_root, &["rev-parse", "HEAD"])?;
    println!("Fork carry contracts at {}", revision.trim());
    let mut passed = 0;
    for contract in &registry.required {
        let id = contract.id;
        let output = match run_test(&args.repo_root, contract) {
            Ok(output) => output,
            Err(error) => {
                println!("- FAIL {id}: could not launch Cargo: {error}");
                continue;
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        match execution_result(contract.test, output.status.success(), &stdout) {
            Ok(()) => {
                passed += 1;
                println!("- PASS {id} ({} {})", contract.package, contract.test);
            }
            Err(error) => {
                println!(
                    "- FAIL {id}: {error} ({} {})",
                    contract.package, contract.test
                );
                eprintln!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
            }
        }
    }
    for gap in &registry.uncovered {
        println!("- UNCOVERED {}: {}", gap.id, gap.reason);
    }
    println!(
        "\n{passed}/{} required contracts passed. {} known gaps.",
        registry.required.len(),
        registry.uncovered.len()
    );
    Ok(i32::from(passed != registry.required.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST: &str = "carry::tests::contract";
    const PASSED: &str = "running 1 test\ntest carry::tests::contract ... ok\n\n\
                         test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s\n";

    #[test]
    fn only_the_exact_executed_test_counts() {
        assert_eq!(execution_result(TEST, true, PASSED), Ok(()));
        assert!(execution_result("carry::tests::missing", true, PASSED).is_err());
        assert!(execution_result(TEST, false, PASSED).is_err());
        assert!(execution_result(TEST, true, &PASSED.repeat(2)).is_err());
        assert!(execution_result(TEST, true, "an incidental 1 passed message").is_err());
    }

    #[test]
    fn zero_or_ignored_selection_fails_even_when_cargo_succeeds() {
        let result = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s\n";
        assert_eq!(
            execution_result(TEST, true, result),
            Err("exact test missing")
        );
        let ignored = result.replace("0 ignored", "1 ignored");
        assert_eq!(
            execution_result(TEST, true, &ignored),
            Err("exact test ignored")
        );
    }

    #[test]
    fn registry_keeps_required_contracts_and_explicit_gaps() {
        let json = include_str!("../../../docs/parity/fork-carry-contracts.v1.json");
        let parsed = py::loads(json).unwrap();
        let registry = registry(&parsed).unwrap();
        assert!(registry
            .required
            .iter()
            .any(|entry| entry.id == "carry-replay-hash-chain"));
        assert!(registry
            .uncovered
            .iter()
            .any(|entry| entry.id == "carry-launchd-webhook-lifecycle"));
    }

    #[test]
    fn registry_rejects_duplicate_contract_ids_and_tests() {
        let json = r#"{
            "version": 1,
            "required": [
                {"id": "carry-first", "behaviour": "first", "package": "hermes-agent", "test": "module::test"},
                {"id": "carry-second", "behaviour": "second", "package": "hermes-agent", "test": "module::test"}
            ],
            "uncovered": []
        }"#;
        let document = py::loads(json).unwrap();
        assert!(registry(&document)
            .err()
            .unwrap()
            .contains("duplicate library test"));
        let duplicate = json.replacen("carry-second", "carry-first", 1);
        let document = py::loads(&duplicate).unwrap();
        assert!(registry(&document)
            .err()
            .unwrap()
            .contains("duplicate carry ID"));
    }
}
