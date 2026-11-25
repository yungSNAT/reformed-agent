//! Download crates.io source, reapply local RustSec patches and check/refresh overrides.
//! Port of `scripts/refresh-rustsec-patches.sh`.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::Args;
use jiff::Timestamp;

use crate::py;
use crate::report_io::resolve_argument;
use crate::script_helpers::{py_splitlines, read_utf8, run_process};

#[derive(Args, Debug)]
#[command(disable_help_flag = true)]
pub struct RefreshRustsecPatchesArgs {
    /// Repository root override (defaults to git toplevel)
    #[arg(long, default_value = "")]
    repo_root: String,
    /// Subcommand and arguments: list | check [all|crate] | refresh <crate> [version]
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

struct PatchSpec {
    configured_version: &'static str,
    configured_dir: &'static str,
    patch_rel: &'static str,
}

fn patch_spec(crate_name: &str) -> Option<PatchSpec> {
    match crate_name {
        "tungstenite" => Some(PatchSpec {
            configured_version: "0.29.0",
            configured_dir: "tungstenite-0.29.0",
            patch_rel: "patches/tungstenite-0.29.0-use-getrandom.patch",
        }),
        _ => None,
    }
}

const KNOWN_CRATES: &[&str] = &["tungstenite"];

const USAGE: &str = "\
Usage:
  cargo run --locked -q -p hermes-parity-tools -- refresh-rustsec-patches check [all|crate-name]
  cargo run --locked -q -p hermes-parity-tools -- refresh-rustsec-patches refresh <crate-name> [version]
  cargo run --locked -q -p hermes-parity-tools -- refresh-rustsec-patches list

Downloads the configured crates.io crate tarball, reapplies the local RustSec
patch file, and either checks patched files against the repository copy or
refreshes third_party/rustsec-patches/<crate>-<version>.

Resolution workflow:
  1. Try a newer official crate version with refresh <crate> <version>.
  2. Update Cargo.toml [patch.crates-io] to the refreshed directory if still needed.
  3. If the official crate no longer needs the local patch, remove the patch entry
     instead and rerun cargo audit/check/test/build gates.";

fn is_semver_ident(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn validate_crate_version(version: &str) -> Result<(), String> {
    if version.is_empty()
        || version.contains('/')
        || version.contains('\\')
        || version.contains('\0')
        || version.contains("..")
    {
        return Err(format!("invalid crate version: {version:?}"));
    }
    let (main_and_pre, build) = match version.split_once('+') {
        Some((left, right)) => {
            if right.is_empty() || !right.split('.').all(is_semver_ident) {
                return Err(format!("invalid crate version build metadata: {version:?}"));
            }
            (left, true)
        }
        None => (version, false),
    };
    let _ = build;
    let (core, pre) = match main_and_pre.split_once('-') {
        Some((left, right)) => {
            if right.is_empty() || !right.split('.').all(is_semver_ident) {
                return Err(format!("invalid crate version prerelease: {version:?}"));
            }
            (left, true)
        }
        None => (main_and_pre, false),
    };
    let _ = pre;
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(format!(
            "invalid semver version (expected MAJOR.MINOR.PATCH): {version:?}"
        ));
    }
    Ok(())
}

fn safe_child_dir(parent: &Path, child_name: &str) -> Result<PathBuf, String> {
    if child_name.is_empty()
        || child_name.contains('/')
        || child_name.contains('\\')
        || child_name.contains("..")
        || child_name.contains('\0')
    {
        return Err(format!("unsafe directory component: {child_name:?}"));
    }
    let joined = parent.join(child_name);
    if joined.parent() != Some(parent) {
        return Err(format!(
            "resolved path {} escapes {}",
            joined.display(),
            parent.display()
        ));
    }
    Ok(joined)
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self, String> {
        let base = std::env::temp_dir().join(format!(
            "hermes-rustsec-{}-{}",
            std::process::id(),
            Timestamp::now().as_nanosecond()
        ));
        fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
        Ok(Self { path: base })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn resolve_repo_root_from_flag(raw: &str) -> Result<PathBuf, String> {
    if !raw.is_empty() {
        return resolve_argument(&py::expanduser(raw)?);
    }
    let cwd = std::env::current_dir().map_err(|e| format!("current_dir: {e}"))?;
    let res = run_process(
        &[
            "git".to_string(),
            "rev-parse".to_string(),
            "--show-toplevel".to_string(),
        ],
        &cwd,
        None,
    )?;
    if !res.ok {
        return Err(format!(
            "git rev-parse --show-toplevel failed: {}",
            res.stderr
        ));
    }
    Ok(PathBuf::from(py::strip(&res.stdout)))
}

fn download_and_patch(
    crate_name: &str,
    version: &str,
    patch_file: &Path,
    workdir: &Path,
) -> Result<(), String> {
    validate_crate_version(version)?;
    let archive = safe_child_dir(workdir, &format!("{crate_name}-{version}.crate"))?;
    let url = format!("https://crates.io/api/v1/crates/{crate_name}/{version}/download");

    let curl_res = run_process(
        &[
            "curl".to_string(),
            "-fsSL".to_string(),
            "-A".to_string(),
            "hermes-agent-ultra-rustsec-patch-refresh/1.0".to_string(),
            "-H".to_string(),
            "Accept: application/octet-stream".to_string(),
            url,
            "-o".to_string(),
            py::path_str(&archive),
        ],
        workdir,
        None,
    )?;
    if !curl_res.ok {
        return Err(format!(
            "curl failed ({}): {}",
            curl_res.exit_code, curl_res.stderr
        ));
    }

    let tar_res = run_process(
        &[
            "tar".to_string(),
            "-xzf".to_string(),
            py::path_str(&archive),
            "-C".to_string(),
            py::path_str(workdir),
        ],
        workdir,
        None,
    )?;
    if !tar_res.ok {
        return Err(format!(
            "tar failed ({}): {}",
            tar_res.exit_code, tar_res.stderr
        ));
    }

    let patch_reader =
        File::open(patch_file).map_err(|e| format!("{}: {e}", patch_file.display()))?;
    let crate_dir = workdir.join(format!("{crate_name}-{version}"));
    let patch_out = Command::new("patch")
        .args(["-p1", "-d", &py::path_str(&crate_dir)])
        .stdin(Stdio::from(patch_reader))
        .output()
        .map_err(|e| format!("failed to run patch: {e}"))?;
    if !patch_out.status.success() {
        return Err(format!(
            "patch failed: {}",
            String::from_utf8_lossy(&patch_out.stderr)
        ));
    }
    Ok(())
}

fn patched_files(patch_file: &Path) -> Result<Vec<String>, String> {
    let raw = read_utf8(patch_file)?;
    let mut files = BTreeSet::new();
    for line in py_splitlines(&raw) {
        if let Some(rel) = line.strip_prefix("+++ b/") {
            files.insert(rel.to_string());
        }
    }
    Ok(files.into_iter().collect())
}

fn check_one(patch_dir: &Path, crate_name: &str) -> Result<(), String> {
    let spec = patch_spec(crate_name).ok_or_else(|| format!("unknown crate: {crate_name}"))?;
    let patch_file = patch_dir.join(spec.patch_rel);
    let tmp = TempDir::new()?;
    download_and_patch(crate_name, spec.configured_version, &patch_file, &tmp.path)?;

    let tmp_root = tmp
        .path
        .join(format!("{crate_name}-{}", spec.configured_version));
    let dest_root = patch_dir.join(spec.configured_dir);
    for file in patched_files(&patch_file)? {
        let diff_res = run_process(
            &[
                "diff".to_string(),
                "-u".to_string(),
                py::path_str(&tmp_root.join(&file)),
                py::path_str(&dest_root.join(&file)),
            ],
            patch_dir,
            None,
        )?;
        if !diff_res.ok {
            return Err(format!("diff mismatch in {file}"));
        }
    }

    println!("ok {crate_name} {}", spec.configured_version);
    Ok(())
}

fn remove_snapshots_dirs(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() == "snapshots" {
                fs::remove_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            } else {
                remove_snapshots_dirs(&path)?;
            }
        }
    }
    Ok(())
}

fn prune_non_runtime_artifacts(dir: &Path) -> Result<(), String> {
    for rel in [
        ".cargo-ok",
        ".cargo_vcs_info.json",
        "Cargo.lock",
        "Cargo.toml.orig",
        ".cargo",
        ".config",
        ".github",
        ".vscode",
        "afl",
        "benches",
        "examples",
        "contrib",
    ] {
        let target = dir.join(rel);
        if target.is_dir() {
            fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        } else if target.exists() {
            fs::remove_file(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    remove_snapshots_dirs(dir)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", src.display()))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", src_path.display()))?;
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("{} -> {}: {e}", src_path.display(), dst_path.display()))?;
        }
    }
    Ok(())
}

fn refresh_one(
    patch_dir: &Path,
    crate_name: &str,
    requested_version: Option<&str>,
) -> Result<(), String> {
    let spec = patch_spec(crate_name).ok_or_else(|| format!("unknown crate: {crate_name}"))?;
    let version = requested_version.unwrap_or(spec.configured_version);
    validate_crate_version(version)?;
    let patch_file = patch_dir.join(spec.patch_rel);
    let tmp = TempDir::new()?;
    let folder_name = format!("{crate_name}-{version}");
    let src = safe_child_dir(&tmp.path, &folder_name)?;
    let dest = safe_child_dir(patch_dir, &folder_name)?;

    download_and_patch(crate_name, version, &patch_file, &tmp.path)?;
    prune_non_runtime_artifacts(&src)?;
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    copy_dir_recursive(&src, &dest)?;

    println!(
        "refreshed {crate_name} {version} -> {}",
        py::path_str(&dest)
    );
    if version != spec.configured_version {
        println!(
            "note: update Cargo.toml [patch.crates-io] from {} to {crate_name}-{version} before verifying the workspace",
            spec.configured_dir
        );
    }
    Ok(())
}

pub fn run(args: RefreshRustsecPatchesArgs) -> Result<i32, String> {
    let command = args.args.first().map_or("", String::as_str);
    match command {
        "-h" | "--help" | "help" | "" => {
            println!("{USAGE}");
            Ok(0)
        }
        "list" => {
            for crate_name in KNOWN_CRATES {
                println!("{crate_name}");
            }
            Ok(0)
        }
        "check" => {
            let target = args.args.get(1).map_or("all", String::as_str);
            if target != "all" && patch_spec(target).is_none() {
                eprintln!("unknown crate: {target}");
                return Ok(2);
            }
            let patch_dir =
                resolve_repo_root_from_flag(&args.repo_root)?.join("third_party/rustsec-patches");
            if target == "all" {
                for crate_name in KNOWN_CRATES {
                    check_one(&patch_dir, crate_name)?;
                }
            } else {
                check_one(&patch_dir, target)?;
            }
            Ok(0)
        }
        "refresh" => {
            let Some(crate_name) = args.args.get(1).map(String::as_str) else {
                eprintln!("{USAGE}");
                return Ok(2);
            };
            if crate_spec_exists(crate_name) {
                let patch_dir = resolve_repo_root_from_flag(&args.repo_root)?
                    .join("third_party/rustsec-patches");
                let requested_version = args.args.get(2).map(String::as_str);
                refresh_one(&patch_dir, crate_name, requested_version)?;
                Ok(0)
            } else {
                eprintln!("unknown crate: {crate_name}");
                Ok(2)
            }
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

fn crate_spec_exists(crate_name: &str) -> bool {
    patch_spec(crate_name).is_some()
}

#[cfg(test)]
mod tests {
    use super::{safe_child_dir, validate_crate_version, USAGE};
    use std::path::Path;

    #[test]
    fn usage_names_cargo_command_not_deleted_shell_script() {
        assert!(USAGE.contains("refresh-rustsec-patches"));
        assert!(!USAGE.contains("scripts/refresh-rustsec-patches.sh"));
    }

    #[test]
    fn version_validation_accepts_semver_and_rejects_traversal() {
        assert!(validate_crate_version("0.29.0").is_ok());
        assert!(validate_crate_version("1.2.3-rc.1+meta-2").is_ok());
        for bad in [
            "",
            "../evil",
            "0.29.0/../../x",
            "0.29.0\\..\\x",
            "0.29.0..1",
            "0.29",
            "0.29.0-beta.",
            "0.29.0 a",
        ] {
            assert!(
                validate_crate_version(bad).is_err(),
                "expected error for {bad}"
            );
        }
        assert!(safe_child_dir(Path::new("root"), "tungstenite-0.29.0").is_ok());
        assert!(safe_child_dir(Path::new("root"), "../escape").is_err());
    }
}
