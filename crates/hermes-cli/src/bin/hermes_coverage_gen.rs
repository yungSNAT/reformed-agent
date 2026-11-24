use super::{candidate_targets, run_target, unique_targets};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const PROBE_CASE: &str = "HERMES_COVERAGE_GEN_CANDIDATE_CASE";
const PROBE_PASSED: &str = "hermes candidate assertions passed";

#[test]
fn candidate_targets_search_order_and_self_guard() {
    if let Ok(case) = std::env::var(PROBE_CASE) {
        let mut expected = Vec::new();
        match case.as_str() {
            "explicit" => expected.push(OsString::from("./explicit-target")),
            "self" | "fallback" => {}
            _ => panic!("unexpected candidate probe case: {case}"),
        }
        if case != "fallback" {
            expected.push(
                std::env::current_exe() // nosemgrep: rust.lang.security.current-exe.current-exe
                    .expect("locate the running probe")
                    .with_file_name("hermes-agent-ultra")
                    .into_os_string(),
            );
            expected.push(
                PathBuf::from("home")
                    .join(".cargo/bin/hermes-agent-ultra")
                    .into_os_string(),
            );
        }
        expected.push(OsString::from("hermes-agent-ultra"));
        assert_eq!(candidate_targets(), expected);
        println!("{PROBE_PASSED}");
        return;
    }

    // Set environment variables only in the child, and give its executable
    // a private directory so sibling discovery does not depend on build artifacts.
    let temp = tempfile::tempdir().expect("create probe directory");
    let root = temp.path().canonicalize().expect("resolve probe directory");
    let probe_name = format!("hermes-coverage-probe{}", std::env::consts::EXE_SUFFIX);
    let probe = root.join(&probe_name);
    fs::copy(
        std::env::current_exe().expect("locate the test executable"), // nosemgrep: rust.lang.security.current-exe.current-exe
        &probe,
    )
    .expect("copy the test executable");

    let sibling = root.join("hermes-agent-ultra");
    let cargo_bin = root.join("home/.cargo/bin");
    let home_target = cargo_bin.join("hermes-agent-ultra");
    fs::create_dir_all(&cargo_bin).expect("create the fixture Cargo bin directory");
    fs::write(&sibling, b"").expect("create sibling fixture");
    fs::write(&home_target, b"").expect("create home fixture");
    fs::write(root.join("explicit-target"), b"").expect("create explicit fixture");

    let check = |case: &str, explicit: &str, warn_about_self: bool| {
        let output = Command::new(&probe)
            .args([
                "--exact",
                "coverage_gen_bin_hermes::candidate_targets_search_order_and_self_guard",
                "--nocapture",
            ])
            .current_dir(&root)
            .env(PROBE_CASE, case)
            .env("HOME", "home")
            .env("HERMES_ULTRA_BIN", explicit)
            .output()
            .expect("run candidate probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains(PROBE_PASSED),
            "{case} probe failed: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status
        );
        assert_eq!(
            stderr.contains("Ignoring HERMES_ULTRA_BIN because it points back to this wrapper."),
            warn_about_self,
            "{case} probe stderr: {stderr}"
        );
    };

    check("explicit", " \t./explicit-target \n", false);
    check("self", &format!("./{probe_name}"), true);

    fs::remove_file(sibling).expect("remove sibling fixture");
    fs::remove_file(home_target).expect("remove home fixture");
    check("fallback", " \t ", false);
}

#[test]
fn unique_targets_keeps_first_occurrences_in_input_order() {
    let candidates =
        Vec::from(["z-agent", "a-agent", "z-agent", "./z-agent", "a-agent"].map(OsString::from));
    assert_eq!(
        unique_targets(candidates),
        Vec::from(["z-agent", "a-agent", "./z-agent"].map(OsString::from))
    );
    assert!(unique_targets(Vec::new()).is_empty());
}

#[cfg(unix)]
#[test]
fn unique_targets_keeps_original_bytes_of_first_lossy_match() {
    use std::os::unix::ffi::OsStringExt;

    let first = OsString::from_vec(b"agent-\xff".to_vec());
    let second = OsString::from_vec(b"agent-\xfe".to_vec());
    let replacement = OsString::from("agent-\u{fffd}");

    assert_eq!(
        unique_targets(vec![first.clone(), second, replacement]),
        vec![first]
    );
}

#[test]
fn run_target_reports_missing_executable_and_its_path() {
    let temp = tempfile::tempdir().expect("create directory for a missing target");
    let target = temp
        .path()
        .canonicalize()
        .expect("resolve target directory")
        .join("missing-hermes-agent-ultra.exe");
    let prefix = format!("{} (", target.display());
    let error = run_target(target.into_os_string(), &[OsString::from("--help")])
        .expect_err("a nonexistent executable cannot be launched");
    let reason = error
        .strip_prefix(&prefix)
        .and_then(|detail| detail.strip_suffix(')'))
        .expect("launch error should include the target path and a parenthesized reason");

    #[cfg(unix)]
    assert_eq!(
        reason,
        std::io::Error::from_raw_os_error(libc::ENOENT).to_string()
    );
    #[cfg(not(unix))]
    assert!(
        !reason.is_empty(),
        "launch error should explain the failure"
    );
}
