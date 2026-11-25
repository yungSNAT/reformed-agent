//! Container entrypoint binary replacing shell logic in `docker/entrypoint.sh`.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unimplemented,
        clippy::todo,
        clippy::unreachable
    )
)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const MAX_CAPTURE_BYTES: u64 = 1024;

#[derive(Debug, PartialEq, Eq)]
struct EntryConfig {
    app_home: PathBuf,
    target_uid: u32,
    target_gid: u32,
    explicit_non_default_uid: bool,
}

/// `${NAME:-default}`: unset or empty gives the default; anything else must be
/// ASCII digits (no sign, padding or whitespace) that fit a u32.
fn parse_id(name: &str, raw: Option<&OsStr>, default: u32) -> Result<u32, String> {
    let Some(raw) = raw.filter(|v| !v.is_empty()) else {
        return Ok(default);
    };
    let text = raw
        .to_str()
        .ok_or_else(|| format!("invalid numeric {name}: not UTF-8"))?;
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid numeric {name}: {text:?}"));
    }
    text.parse::<u32>()
        .map_err(|_| format!("invalid numeric {name}: {text:?}"))
}

fn build_config(
    home: Option<&OsStr>,
    uid: Option<&OsStr>,
    gid: Option<&OsStr>,
) -> Result<EntryConfig, String> {
    // Preserve exact non-empty HERMES_HOME bytes (whitespace and non-UTF-8 paths).
    let app_home = match home {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/data"),
    };
    let target_uid = parse_id("HERMES_UID", uid, 10_000)?;
    let target_gid = parse_id("HERMES_GID", gid, target_uid)?;
    let explicit_non_default_uid = uid.is_some_and(|v| !v.is_empty() && v != "10000");
    Ok(EntryConfig {
        app_home,
        target_uid,
        target_gid,
        explicit_non_default_uid,
    })
}

fn config_from_env() -> Result<EntryConfig, String> {
    let home = env::var_os("HERMES_HOME");
    let uid = env::var_os("HERMES_UID");
    let gid = env::var_os("HERMES_GID");
    build_config(home.as_deref(), uid.as_deref(), gid.as_deref())
}

fn run_quiet_status(program: &str, args: &[&OsStr]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_ignore_str(program: &str, args: &[&str]) {
    let os_args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let _ = run_quiet_status(program, &os_args);
}

fn capture_u32(program: &str, args: &[&OsStr]) -> Option<u32> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut buf = Vec::new();
    let read_ok = child
        .stdout
        .take()?
        .take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut buf)
        .is_ok();
    let status = child.wait().ok()?;
    if !read_ok || buf.len() as u64 > MAX_CAPTURE_BYTES || !status.success() {
        return None;
    }
    String::from_utf8(buf).ok()?.trim().parse::<u32>().ok()
}

#[cfg(unix)]
fn dir_owner_uid(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).ok().map(|m| m.uid())
}

#[cfg(not(unix))]
fn dir_owner_uid(path: &Path) -> Option<u32> {
    capture_u32(
        "stat",
        &[OsStr::new("-c"), OsStr::new("%u"), path.as_os_str()],
    )
}

fn should_chown(explicit_non_default_uid: bool, owner_uid: Option<u32>, actual_uid: u32) -> bool {
    explicit_non_default_uid || owner_uid.unwrap_or(0) != actual_uid
}

fn reconcile_hermes_identity(cfg: &EntryConfig) -> Result<(u32, u32), String> {
    let uid_str = cfg.target_uid.to_string();
    let gid_str = cfg.target_gid.to_string();
    if !run_quiet_status("getent", &[OsStr::new("group"), OsStr::new("hermes")]) {
        run_ignore_str("groupadd", &["-o", "-g", &gid_str, "hermes"]);
    }
    if !run_quiet_status("id", &[OsStr::new("-u"), OsStr::new("hermes")]) {
        run_ignore_str(
            "useradd",
            &[
                "-m", "-o", "-u", &uid_str, "-g", &gid_str, "-s", "/bin/sh", "hermes",
            ],
        );
    }
    if capture_u32("id", &[OsStr::new("-g"), OsStr::new("hermes")]) != Some(cfg.target_gid) {
        run_ignore_str("groupmod", &["-o", "-g", &gid_str, "hermes"]);
    }
    if capture_u32("id", &[OsStr::new("-u"), OsStr::new("hermes")]) != Some(cfg.target_uid) {
        run_ignore_str("usermod", &["-o", "-u", &uid_str, "-g", &gid_str, "hermes"]);
    }
    let actual_uid = capture_u32("id", &[OsStr::new("-u"), OsStr::new("hermes")])
        .ok_or_else(|| "failed to determine uid for user 'hermes'".to_string())?;
    let actual_gid = capture_u32("id", &[OsStr::new("-g"), OsStr::new("hermes")])
        .ok_or_else(|| "failed to determine gid for user 'hermes'".to_string())?;
    Ok((actual_uid, actual_gid))
}

/// The shell's `exec` status when the program cannot run: 127 when it is not
/// found, 126 when it is found but cannot be executed.
fn exec_failure_code(err: &std::io::Error) -> u8 {
    if err.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

#[cfg(unix)]
fn exec_command(program: &OsString, args: &[OsString]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let err = Command::new(program).args(args).exec();
    eprintln!(
        "hermes-entrypoint: exec: {}: {err}",
        program.to_string_lossy()
    );
    ExitCode::from(exec_failure_code(&err))
}

#[cfg(not(unix))]
fn exec_command(program: &OsString, args: &[OsString]) -> ExitCode {
    match Command::new(program).args(args).status() {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(err) => {
            eprintln!(
                "hermes-entrypoint: exec: {}: {err}",
                program.to_string_lossy()
            );
            ExitCode::from(exec_failure_code(&err))
        }
    }
}

/// The program and arguments to exec; `None` when not root and no command was
/// given (the shell's bare `exec` then exits 0).
fn prepare_command(argv: &[OsString]) -> Result<Option<(OsString, Vec<OsString>)>, String> {
    let cfg = config_from_env()?;
    fs::create_dir_all(&cfg.app_home)
        .map_err(|e| format!("failed to create {}: {e}", cfg.app_home.display()))?;

    if capture_u32("id", &[OsStr::new("-u")]) == Some(0) {
        let (actual_uid, actual_gid) = reconcile_hermes_identity(&cfg)?;
        if should_chown(
            cfg.explicit_non_default_uid,
            dir_owner_uid(&cfg.app_home),
            actual_uid,
        ) {
            let owner = OsString::from(format!("{actual_uid}:{actual_gid}"));
            let _ = run_quiet_status(
                "chown",
                &[
                    OsStr::new("-RP"),
                    OsStr::new("--"),
                    owner.as_os_str(),
                    cfg.app_home.as_os_str(),
                ],
            );
        }
        let mut gosu_args = vec![OsString::from(format!("{actual_uid}:{actual_gid}"))];
        gosu_args.extend(argv.iter().cloned());
        return Ok(Some((OsString::from("gosu"), gosu_args)));
    }

    Ok(argv
        .split_first()
        .map(|(program, rest)| (program.clone(), rest.to_vec())))
}

fn main() -> ExitCode {
    let argv: Vec<OsString> = env::args_os().skip(1).collect();
    match prepare_command(&argv) {
        Ok(Some((program, args))) => exec_command(&program, &args),
        Ok(None) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hermes-entrypoint: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_config, capture_u32, exec_failure_code, should_chown};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    #[test]
    fn defaults_and_overrides_match_entrypoint_contract() {
        let def = build_config(None, None, None).expect("default config");
        assert_eq!(def.app_home, PathBuf::from("/data"));
        assert_eq!(def.target_uid, 10_000);
        assert_eq!(def.target_gid, 10_000);
        assert!(!def.explicit_non_default_uid);

        let custom = build_config(
            Some(OsStr::new("/srv/hermes")),
            Some(OsStr::new("1001")),
            None,
        )
        .expect("custom uid");
        assert_eq!(custom.app_home, PathBuf::from("/srv/hermes"));
        assert_eq!(custom.target_uid, 1001);
        assert_eq!(custom.target_gid, 1001);
        assert!(custom.explicit_non_default_uid);

        let split_gid = build_config(
            Some(OsStr::new("")),
            Some(OsStr::new("10000")),
            Some(OsStr::new("2000")),
        )
        .expect("split gid");
        assert_eq!(split_gid.app_home, PathBuf::from("/data"));
        assert_eq!(split_gid.target_uid, 10_000);
        assert_eq!(split_gid.target_gid, 2000);
        assert!(!split_gid.explicit_non_default_uid);
    }

    #[test]
    fn preserves_non_empty_whitespace_home_and_rejects_bad_ids() {
        let ws = build_config(Some(OsStr::new(" /srv/hermes ")), None, None).expect("ws home");
        assert_eq!(ws.app_home, PathBuf::from(" /srv/hermes "));
        for bad in ["not-a-uid", "  ", " 1001 ", "+1000", "-1", "4294967296"] {
            assert!(
                build_config(None, Some(OsStr::new(bad)), None).is_err(),
                "{bad:?}"
            );
        }
        assert!(build_config(None, Some(OsStr::new("1000")), Some(OsStr::new("bad-gid"))).is_err());
        let padded = build_config(None, Some(OsStr::new("010000")), None).expect("leading zero");
        assert_eq!(padded.target_uid, 10_000);
        assert!(
            padded.explicit_non_default_uid,
            "shell compares the text, not the number"
        );
    }

    #[test]
    fn exec_failures_use_the_shell_status() {
        use std::io::{Error, ErrorKind};
        assert_eq!(exec_failure_code(&Error::from(ErrorKind::NotFound)), 127);
        assert_eq!(
            exec_failure_code(&Error::from(ErrorKind::PermissionDenied)),
            126
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_ids_are_rejected_not_defaulted() {
        use std::os::unix::ffi::OsStrExt;
        let bad = OsStr::from_bytes(b"10\xff01");
        assert!(build_config(None, Some(bad), None).is_err());
        assert!(build_config(None, Some(OsStr::new("1000")), Some(bad)).is_err());
    }

    #[test]
    fn chown_predicate_matches_shell_branches() {
        assert!(should_chown(true, Some(10_000), 10_000));
        assert!(should_chown(false, Some(0), 10_000));
        assert!(should_chown(false, None, 10_000));
        assert!(!should_chown(false, Some(10_000), 10_000));
    }

    #[test]
    fn missing_id_command_returns_none() {
        assert_eq!(
            capture_u32("__nonexistent_entrypoint_tool__", &[OsStr::new("-u")]),
            None
        );
    }
}
