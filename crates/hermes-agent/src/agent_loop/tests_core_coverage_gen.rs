use super::hook_context_spill_writes_file_for_oversized_payload;

fn check_spill_threshold_restoration(test_name: &str) {
    const CHILD: &str = "REFORMED_AGENT_SPILL_RESTORE_COVERAGE_CHILD";
    const THRESHOLD: &str = "HERMES_HOOK_CONTEXT_SPILL_CHARS";
    const ORIGINAL: &str = "4096";

    if matches!(std::env::var(CHILD).as_deref(), Ok("1")) {
        assert_eq!(
            std::env::var(THRESHOLD).expect("initial threshold"),
            ORIGINAL
        );
        hook_context_spill_writes_file_for_oversized_payload();
        assert_eq!(
            std::env::var(THRESHOLD).expect("restored threshold"),
            ORIGINAL
        );
        return;
    }

    // The spill test does not take the environment lock. Run it in a separate
    // process so its normal test invocation cannot race this restoration check.
    let home = tempfile::tempdir().expect("isolated child home");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", test_name])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("HERMES_HOME", home.path())
        .env("HERMES_AGENT_ULTRA_HOME", home.path())
        .env("TERMINAL_CWD", home.path())
        .env(CHILD, "1")
        .env(THRESHOLD, ORIGINAL)
        .output()
        .expect("run isolated spill test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "expected one passing test for {test_name}:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

// Make the filename-based test filter match even when the containing module
// has a different name from tests_core.rs.
mod agent_loop {
    mod tests_core {
        mod coverage_gen {
            #[test]
            fn hook_context_spill_restores_existing_threshold() {
                let (_, module) = module_path!().split_once("::").expect("test module path");
                let test_name = format!("{module}::hook_context_spill_restores_existing_threshold");
                super::super::super::check_spill_threshold_restoration(&test_name);
            }
        }
    }
}
