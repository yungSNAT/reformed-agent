//! Run the local release scanner, SBOM inventory and security regression checks.

use std::fs;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Instant;

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_report};

#[derive(Args, Debug)]
#[command(args_override_self = true, infer_long_args = true)]
pub struct SecurityReleaseGateArgs {
    /// Repository root.
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Secret scan command.
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- release_secret_scan --repo-root . --report .sync-reports/release-secret-scan.json"
    )]
    secret_scan_cmd: String,
    /// Signature verification regression tests.
    #[arg(
        long,
        default_value = "cargo test -p hermes-cli provenance_verify_detects_ -- --nocapture"
    )]
    signature_tests_cmd: String,
    /// Redaction regression tests.
    #[arg(
        long,
        default_value = "cargo test -p hermes-gateway test_redact_pii -- --nocapture"
    )]
    redaction_tests_cmd: String,
    /// ACP multimodal routing regression tests.
    #[arg(
        long,
        default_value = "cargo test -p hermes-agent decodes_acp_multimodal -- --nocapture"
    )]
    multimodal_tests_cmd: String,
    /// SBOM output path.
    #[arg(long, default_value = ".sync-reports/release-sbom-metadata.json")]
    sbom_output: String,
    /// Gate report output path.
    #[arg(long, default_value = "")]
    report: String,
    /// Print the JSON report.
    #[arg(long)]
    json: bool,
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl ProcessOutput {
    fn capture(command: &mut Command, root: &Path) -> Result<Self, String> {
        let output = command
            .current_dir(root)
            .stdin(Stdio::inherit())
            .output()
            .map_err(|error| error.to_string())?;
        let decode = |bytes| {
            String::from_utf8(bytes)
                .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
                .map_err(|error| format!("could not decode command output: {error}"))
        };
        Ok(Self {
            status: output.status,
            stdout: decode(output.stdout)?,
            stderr: decode(output.stderr)?,
        })
    }

    fn report(&self, command: &str, details: impl IntoIterator<Item = (&'static str, Py)>) -> Py {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(unix)]
        let status = self
            .status
            .code()
            .or_else(|| self.status.signal().map(|signal| -signal));
        #[cfg(not(unix))]
        let status = self.status.code();
        Py::dict(
            [
                ("command", command.into()),
                (
                    "exit_code",
                    status.map_or(Py::Null, |code| Py::Int(i128::from(code))),
                ),
                ("ok", self.status.success().into()),
            ]
            .into_iter()
            .chain(details)
            .chain([
                ("stdout_tail", tail(&self.stdout).into()),
                ("stderr_tail", tail(&self.stderr).into()),
            ]),
        )
    }
}

fn tail(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .nth(5999)
        .map_or(0, |(index, _)| index);
    &text[start..]
}

fn shell_check(root: &Path, command: &str) -> Result<Py, String> {
    let started_at = py::utc_isoformat();
    let start = Instant::now();
    let output = ProcessOutput::capture(Command::new("bash").args(["-lc", command]), root)?;
    let finished_at = py::utc_isoformat();
    Ok(output.report(
        command,
        [
            ("started_at", started_at.into()),
            ("finished_at", finished_at.into()),
            ("duration_ms", Py::Int(start.elapsed().as_millis() as i128)),
        ],
    ))
}

fn list_len(value: Option<&Py>, key: &str) -> Result<usize, String> {
    match value {
        None | Some(Py::Null) => Ok(0),
        Some(Py::List(values)) => Ok(values.len()),
        _ => Err(format!("cargo metadata: {key} must be an array")),
    }
}

fn write_json(path: &Path, report: &Py) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    write_report(path, py::dumps(report, Some(2), false) + "\n")
}

fn sbom(root: &Path, path: &Path) -> Result<Py, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let args = ["metadata", "--format-version", "1", "--locked"];
    let output = ProcessOutput::capture(Command::new("cargo").args(args), root)?;
    let mut result = output.report(
        "cargo metadata --format-version 1 --locked",
        [("output", py::path_str(path).into())],
    );
    if !output.status.success() {
        return Ok(result);
    }
    let metadata = match py::loads(&output.stdout) {
        Ok(metadata) => metadata,
        Err(error) => {
            result.set("ok", false.into());
            result.set(
                "error",
                format!("failed to parse cargo metadata json: {error}").into(),
            );
            return Ok(result);
        }
    };
    if !metadata.is_dict() {
        return Err("cargo metadata must be an object".to_string());
    }
    let packages = match metadata.get("packages") {
        Some(Py::List(packages)) => packages.as_slice(),
        None | Some(Py::Null) => &[],
        _ => return Err("cargo metadata: packages must be an array".to_string()),
    };
    if packages.iter().any(|package| !package.is_dict()) {
        return Err("cargo metadata: each package must be an object".to_string());
    }
    let members = list_len(metadata.get("workspace_members"), "workspace_members")?;
    let nodes = match metadata.get("resolve") {
        None | Some(Py::Null) => 0,
        Some(resolve) if resolve.is_dict() => list_len(resolve.get("nodes"), "nodes")?,
        _ => return Err("cargo metadata: resolve must be an object".to_string()),
    };
    let packages_json = packages
        .iter()
        .map(|pkg| {
            Py::dict(
                ["name", "version", "id", "license", "repository"]
                    .map(|field| (field, pkg.get(field).cloned().unwrap_or(Py::Null))),
            )
        })
        .collect();
    let inventory = Py::dict([
        ("generated_at", py::utc_isoformat().into()),
        ("format", "cargo-metadata-v1".into()),
        ("package_count", packages.len().into()),
        ("workspace_member_count", members.into()),
        ("resolve_node_count", nodes.into()),
        ("packages", Py::List(packages_json)),
    ]);
    write_report(path, py::dumps(&inventory, Some(2), false) + "\n")?;
    result.set("package_count", packages.len().into());
    result.set("workspace_member_count", members.into());
    Ok(result)
}

pub fn run(args: SecurityReleaseGateArgs) -> Result<i32, String> {
    let root = resolve_argument(&py::expanduser(&args.repo_root)?)?;
    if !root.exists() {
        return Err(format!("repo root does not exist: {}", py::path_str(&root)));
    }
    let sbom_path = py::join(&root, &args.sbom_output);
    let sections = [
        ("secret_scan", shell_check(&root, &args.secret_scan_cmd)?),
        ("sbom", sbom(&root, &sbom_path)?),
        (
            "signature_tests",
            shell_check(&root, &args.signature_tests_cmd)?,
        ),
        (
            "redaction_tests",
            shell_check(&root, &args.redaction_tests_cmd)?,
        ),
        (
            "multimodal_tests",
            shell_check(&root, &args.multimodal_tests_cmd)?,
        ),
    ];
    let total = sections.len();
    let passed = sections
        .iter()
        .filter(|(_, section)| section.get("ok") == Some(&Py::Bool(true)))
        .count();
    let ok = passed == total;
    let report_path = if args.report.is_empty() {
        root.join(".sync-reports")
            .join(format!("security-release-gate-v2-{}.json", py::utc_stamp()))
    } else {
        resolve_argument(&py::expanduser(&args.report)?)?
    };
    let report = Py::dict([
        ("generated_at", py::utc_isoformat().into()),
        ("repo_root", py::path_str(&root).into()),
        ("ok", ok.into()),
        (
            "summary",
            Py::dict([
                ("passed_sections", passed.into()),
                ("failed_sections", (total - passed).into()),
                ("total_sections", total.into()),
            ]),
        ),
        ("sections", Py::dict(sections)),
        ("report_path", py::path_str(&report_path).into()),
    ]);
    write_json(&report_path, &report)?;
    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        let status = if ok { "PASSED" } else { "FAILED" };
        println!("[security-release-gate-v2] {status} (passed={passed}/{total})");
        println!(
            "[security-release-gate-v2] SBOM: {}",
            py::path_str(&sbom_path)
        );
        println!(
            "[security-release-gate-v2] Report: {}",
            py::path_str(&report_path)
        );
    }
    Ok(i32::from(!ok))
}

#[cfg(test)]
mod tests {
    use super::tail;

    #[test]
    fn captures_the_last_6000_unicode_characters() {
        let text = format!("prefix{}!", "é".repeat(5999));
        assert_eq!(tail(&text), &text["prefix".len()..]);
        assert_eq!(tail("short"), "short");
    }
}
