use super::*;
use std::{fs, path::Path, process::Command};

#[test]
fn renders_all_backend_profiles_in_catalog_order() {
    let rendered = render_backend_profiles(None);
    let profiles: Vec<_> = rendered
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .collect();

    assert_eq!(
        profiles,
        [
            "vllm:balanced",
            "vllm:throughput",
            "vllm:reliability",
            "llama-cpp:balanced",
            "mlx:balanced",
            "apple-ane:balanced",
            "sglang:balanced",
            "tgi:balanced",
            "lmstudio:balanced",
            "lmdeploy:balanced",
            "localai:balanced",
            "koboldcpp:balanced",
            "text-generation-webui:balanced",
            "tabbyapi:balanced",
            "mistral-rs:balanced",
        ]
    );
}

#[test]
fn renders_only_the_requested_backend_with_launch_and_environment_details() {
    let rendered = render_backend_profiles(Some(" \tLlAmA.CpP  "));

    assert_eq!(
        rendered,
        concat!(
            "Backend best-practice profiles\n",
            "-------------------------------\n",
            "- llama-cpp:balanced\n",
            "  General local GGUF serving profile with predictable latency.\n",
            "  launch: llama-server -m MODEL.gguf -c 8192 -t 8 -b 512 --host 127.0.0.1 --port 8080\n",
            "  env: LLAMA_CPP_THREADS=8, LLAMA_CPP_CTX_SIZE=8192, LLAMA_CPP_BATCH=512\n",
            "\nUse `/model backend apply <provider> [profile]` to load env overrides for current runtime.",
        )
    );
    assert_eq!(render_backend_profiles(Some("  LLAMA-CPP  ")), rendered);
}

#[test]
fn reports_an_unknown_backend_using_the_original_input() {
    assert_eq!(
        render_backend_profiles(Some("  Unknown-Backend  ")),
        concat!(
            "Backend best-practice profiles\n",
            "-------------------------------\n",
            "No backend profile presets found for '  Unknown-Backend  '.",
        )
    );
}

#[test]
fn model_spec_splits_only_the_first_colon_and_preserves_id_case() {
    let (provider, id) = model_current_provider_and_id("  VLLM :  Team/Model:Q4_K_M  ");
    assert_eq!(provider, "vllm");
    assert_eq!(id, "Team/Model:Q4_K_M");
}

#[test]
fn model_spec_without_provider_defaults_to_openai() {
    let (provider, id) = model_current_provider_and_id("  gpt-4o-mini \t");
    assert_eq!(provider, "openai");
    assert_eq!(id, "gpt-4o-mini");
}

#[test]
fn backend_profile_persistence_with_isolated_home() {
    const TEST_HOME: &str = "HERMES_CLI_COVERAGE_BACKEND_PROFILE_HOME";

    if let Some(home) = std::env::var_os(TEST_HOME) {
        check_backend_profile_persistence(Path::new(&home));
        return;
    }

    // Re-run just this test so home overrides cannot affect parallel tests.
    let temp = tempfile::tempdir().expect("create isolated home");
    let home = temp.path();
    let (_, module) = module_path!()
        .split_once("::")
        .expect("test module path includes the crate name");
    let output = Command::new(std::env::current_exe().expect("locate test binary"))
        .arg("--exact")
        .arg(format!(
            "{module}::backend_profile_persistence_with_isolated_home"
        ))
        .env(TEST_HOME, home)
        .env("HERMES_HOME", home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("run persistence checks in a subprocess");

    assert!(
        output.status.success(),
        "persistence subprocess failed ({})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read_to_string(home.join("runtime/backend_profiles/vllm-throughput.env"))
            .expect("subprocess wrote the normalized profile file"),
        "VLLM_MAX_NUM_SEQS=64\n"
    );
}

fn check_backend_profile_persistence(home: &Path) {
    assert_eq!(hermes_config::hermes_home().as_path(), home);
    let runtime = home.join("runtime");
    let directory = runtime.join("backend_profiles");

    fs::write(&runtime, "not a directory").expect("block profile directory creation");
    let error = persist_backend_profile_env("llvm", "throughput", &[])
        .expect_err("a regular file at runtime must prevent directory creation");
    assert_profile_io_error(
        error,
        "Failed to create backend profile directory",
        &directory,
    );
    fs::remove_file(&runtime).expect("unblock profile directory creation");

    let expected_path = directory.join("vllm-throughput.env");
    let path = persist_backend_profile_env(
        "  LLVM  ",
        "  ThRoUgHpUt  ",
        &[
            ("VLLM_GPU_MEMORY_UTILIZATION", "0.92"),
            ("VLLM_MAX_NUM_SEQS", "256"),
        ],
    )
    .expect("create the profile directory and write the env file");
    assert_eq!(path, expected_path);
    assert_eq!(
        fs::read_to_string(&path).expect("read the profile env file"),
        "VLLM_GPU_MEMORY_UTILIZATION=0.92\nVLLM_MAX_NUM_SEQS=256\n"
    );

    let rewritten =
        persist_backend_profile_env("vllm", "throughput", &[("VLLM_MAX_NUM_SEQS", "64")])
            .expect("replace the existing profile env file");
    assert_eq!(rewritten, expected_path);

    let blocked_path = directory.join("lmstudio-balanced.env");
    fs::create_dir(&blocked_path).expect("occupy the output path with a directory");
    let error = persist_backend_profile_env("LM Studio", "balanced", &[])
        .expect_err("writing an env file over a directory must fail");
    assert_profile_io_error(error, "Failed to write backend profile file", &blocked_path);
    assert!(blocked_path.is_dir());
}

fn assert_profile_io_error(error: AgentError, operation: &str, path: &Path) {
    let AgentError::Io(message) = error else {
        panic!("expected an I/O error, got {error:?}");
    };
    let prefix = format!("{operation} {}: ", path.display());
    let cause = message
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("expected error prefix {prefix:?}, got {message:?}"));
    assert!(!cause.trim().is_empty(), "missing filesystem error cause");
}
