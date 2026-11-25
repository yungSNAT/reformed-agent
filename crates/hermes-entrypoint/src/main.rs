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
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

#[derive(Debug, PartialEq, Eq)]
struct EntryConfig {
    app_home: PathBuf,
    target_uid: u32,
    target_gid: u32,
    explicit_non_default_uid: bool,
}

fn non_empty(var: Option<&str>) -> Option<&str> {
    var.map(str::trim).filter(|s| !s.is_empty())
}

fn parse_id(name: &str, raw: Option<&str>, default: u32) -> Result<u32, String> {
    match non_empty(raw) {
        Some(v) => v
            .parse::<u32>()
            .map_err(|_| format!("invalid numeric {name}: {v}")),
        None => Ok(default),
    }
}

fn build_config(
    home: Option<&str>,
    uid: Option<&str>,
    gid: Option<&str>,
) -> Result<EntryConfig, String> {
    let app_home = PathBuf::from(non_empty(home).unwrap_or("/data"));
    let target_uid = parse_id("HERMES_UID", uid, 10_000)?;
    let target_gid = parse_id("HERMES_GID", gid, target_uid)?;
    let explicit_non_default_uid = non_empty(uid).is_some_and(|v| v != "10000");
    Ok(EntryConfig {
        app_home,
        target_uid,
        target_gid,
        explicit_non_default_uid,
    })
}

fn config_from_env() -> Result<EntryConfig, String> {
    let home = env::var("HERMES_HOME").ok();
    let uid = env::var("HERMES_UID").ok();
    let gid = env::var("HERMES_GID").ok();
    build_config(home.as_deref(), uid.as_deref(), gid.as_deref())
}

fn run_quiet_status(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_ignore(program: &str, args: &[&str]) {
    let _ = run_quiet_status(program, args);
}

fn capture_u32(program: &str, args: &[&str]) -> Option<u32> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

#[cfg(unix)]
fn dir_owner_uid(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|m| m.uid())
}

#[cfg(not(unix))]
fn dir_owner_uid(_path: &Path) -> Option<u32> {
    capture_u32("stat", &["-c", "%u", "."])
}

fn should_chown(explicit_non_default_uid: bool, owner_uid: Option<u32>, actual_uid: u32) -> bool {
    explicit_non_default_uid || owner_uid.unwrap_or(0) != actual_uid
}

fn reconcile_hermes_identity(cfg: &EntryConfig) -> (u32, u32) {
    let uid_str = cfg.target_uid.to_string();
    let gid_str = cfg.target_gid.to_string();
    if !run_quiet_status("getent", &["group", "hermes"]) {
        run_ignore("groupadd", &["-o", "-g", &gid_str, "hermes"]);
    }
    if !run_quiet_status("id", &["-u", "hermes"]) {
        run_ignore(
            "useradd",
            &[
                "-m", "-o", "-u", &uid_str, "-g", &gid_str, "-s", "/bin/sh", "hermes",
            ],
        );
    }
    if capture_u32("id", &["-g", "hermes"]) != Some(cfg.target_gid) {
        run_ignore("groupmod", &["-o", "-g", &gid_str, "hermes"]);
    }
    if capture_u32("id", &["-u", "hermes"]) != Some(cfg.target_uid) {
        run_ignore("usermod", &["-o", "-u", &uid_str, "-g", &gid_str, "hermes"]);
    }
    let actual_uid = capture_u32("id", &["-u", "hermes"]).unwrap_or(cfg.target_uid);
    let actual_gid = capture_u32("id", &["-g", "hermes"]).unwrap_or(cfg.target_gid);
    (actual_uid, actual_gid)
}

#[cfg(unix)]
fn exec_command(program: &OsString, args: &[OsString]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let err = Command::new(program).args(args).exec();
    Err(format!(
        "failed to exec {}: {err}",
        program.to_string_lossy()
    ))
}

#[cfg(not(unix))]
fn exec_command(program: &OsString, args: &[OsString]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|err| format!("failed to run {}: {err}", program.to_string_lossy()))?;
    if !status.success() {
        return Err(format!("command exited with {status}"));
    }
    Ok(())
}

fn prepare_command(args: &[OsString]) -> Result<(OsString, Vec<OsString>), String> {
    let cfg = config_from_env()?;
    fs::create_dir_all(&cfg.app_home)
        .map_err(|e| format!("failed to create {}: {e}", cfg.app_home.display()))?;

    if capture_u32("id", &["-u"]) == Some(0) {
        let (actual_uid, actual_gid) = reconcile_hermes_identity(&cfg);
        if should_chown(
            cfg.explicit_non_default_uid,
            dir_owner_uid(&cfg.app_home),
            actual_uid,
        ) {
            let owner = format!("{actual_uid}:{actual_gid}");
            let home_str = cfg.app_home.to_string_lossy().into_owned();
            run_ignore("chown", &["-RP", "--", &owner, &home_str]);
        }
        let mut gosu_args = vec![OsString::from(format!("{actual_uid}:{actual_gid}"))];
        gosu_args.extend(args.iter().cloned());
        return Ok((OsString::from("gosu"), gosu_args));
    }

    let Some((prog, rest)) = args.split_first() else {
        return Err("usage: hermes-entrypoint <command> [args...]".to_string());
    };
    Ok((prog.clone(), rest.to_vec()))
}

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: hermes-entrypoint <command> [args...]");
        return ExitCode::from(2);
    }
    match prepare_command(&args).and_then(|(prog, argv)| exec_command(&prog, &argv)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hermes-entrypoint: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_config, should_chown};
    use std::path::PathBuf;

    #[test]
    fn defaults_and_overrides_match_entrypoint_contract() {
        let def = build_config(None, None, None).expect("default config");
        assert_eq!(def.app_home, PathBuf::from("/data"));
        assert_eq!(def.target_uid, 10_000);
        assert_eq!(def.target_gid, 10_000);
        assert!(!def.explicit_non_default_uid);

        let custom = build_config(Some("/srv/hermes"), Some("1001"), None).expect("custom uid");
        assert_eq!(custom.app_home, PathBuf::from("/srv/hermes"));
        assert_eq!(custom.target_uid, 1001);
        assert_eq!(custom.target_gid, 1001);
        assert!(custom.explicit_non_default_uid);

        let split_gid = build_config(Some(""), Some("10000"), Some("2000")).expect("split gid");
        assert_eq!(split_gid.app_home, PathBuf::from("/data"));
        assert_eq!(split_gid.target_uid, 10_000);
        assert_eq!(split_gid.target_gid, 2000);
        assert!(!split_gid.explicit_non_default_uid);
    }

    #[test]
    fn rejects_invalid_numeric_ids() {
        assert!(build_config(None, Some("not-a-uid"), None).is_err());
        assert!(build_config(None, Some("1000"), Some("bad-gid")).is_err());
    }

    #[test]
    fn chown_predicate_matches_shell_branches() {
        assert!(should_chown(true, Some(10_000), 10_000));
        assert!(should_chown(false, Some(0), 10_000));
        assert!(should_chown(false, None, 10_000));
        assert!(!should_chown(false, Some(10_000), 10_000));
    }
}
