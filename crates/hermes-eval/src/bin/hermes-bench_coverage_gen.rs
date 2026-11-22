// Cargo also autodiscovers this file as a binary because it lives in src/bin.
#[cfg(not(test))]
fn main() {}

// These placeholders let the autodiscovered test target type-check. The tests
// skip that target and call the real helpers through crate:: in hermes-bench.
#[cfg(test)]
#[allow(dead_code)]
#[derive(clap::Parser)]
struct Args {
    #[arg(long)]
    model: Option<String>,
}

#[cfg(test)]
#[allow(dead_code)]
fn load_gateway_config(
    _: Option<&std::path::Path>,
) -> hermes_eval::EvalResult<hermes_config::GatewayConfig> {
    unreachable!("only the hermes-bench test target has a gateway config loader")
}

#[cfg(test)]
#[allow(dead_code)]
fn resolve_model(_: &Args, _: &hermes_config::GatewayConfig) -> String {
    unreachable!("only the hermes-bench test target has a model resolver")
}

#[cfg(test)]
#[allow(dead_code)]
fn default_output_path(_: &std::path::Path) -> std::path::PathBuf {
    unreachable!("only the hermes-bench test target has an output path helper")
}

#[cfg(all(test, feature = "agent-loop"))]
#[allow(dead_code)]
async fn run_agent_rollout(
    _: std::sync::Arc<hermes_eval::ConfiguredBenchmarkAdapter>,
    _: &hermes_eval::Runner,
    _: &hermes_config::GatewayConfig,
    _: &str,
    _: bool,
) -> hermes_eval::EvalResult<hermes_eval::RunRecord> {
    unreachable!("only the hermes-bench test target has an agent rollout helper")
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "agent-loop")]
    use crate::run_agent_rollout;
    use crate::{default_output_path, load_gateway_config, resolve_model, Args};
    use clap::Parser;
    #[cfg(feature = "agent-loop")]
    use hermes_eval::{ConfiguredBenchmarkAdapter, Runner, RunnerConfig, SmokeRollout};
    use std::path::{Path, PathBuf};
    #[cfg(feature = "agent-loop")]
    use std::sync::Arc;
    use std::{env, fs, process::Command};

    const TEST_HOME: &str = "HERMES_BENCH_COVERAGE_HOME";
    const EXPECTED_MODEL: &str = "HERMES_BENCH_COVERAGE_EXPECTED_MODEL";

    fn is_bench_test_target() -> bool {
        module_path!().ends_with("::coverage_gen::tests")
    }

    fn isolated_home() -> Option<PathBuf> {
        env::var_os(TEST_HOME).map(PathBuf::from)
    }

    // Re-run just the named test so home and model overrides never affect other tests.
    fn run_isolated(test: &str, overrides: &[(&str, &str)]) {
        let home = tempfile::tempdir().expect("create temporary home");
        let (_, module) = module_path!().split_once("::").expect("child module path");
        let test_name = format!("{module}::{test}");
        let mut command = Command::new(env::current_exe().expect("locate test executable"));
        command
            .args(["--exact", &test_name])
            .current_dir(home.path())
            .env_clear()
            .env(TEST_HOME, home.path())
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("HERMES_HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .envs(overrides.iter().copied());

        if let Some(profile) = env::var_os("LLVM_PROFILE_FILE") {
            let profile = env::current_dir()
                .expect("get original working directory")
                .join(profile);
            command.env("LLVM_PROFILE_FILE", profile);
        }

        let output = command.output().expect("run isolated test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "{test_name} did not pass in isolation ({status}):\n{stdout}\n{stderr}",
            status = output.status
        );
    }

    #[test]
    fn gateway_config_uses_explicit_directory() {
        if !is_bench_test_target() {
            return;
        }
        let Some(home) = isolated_home() else {
            run_isolated("gateway_config_uses_explicit_directory", &[]);
            return;
        };

        let config_dir = home.join("explicit-config");
        fs::create_dir(&config_dir).expect("create explicit config directory");
        fs::write(
            config_dir.join("config.yaml"),
            "model: 'nous:fixture-model'\n",
        )
        .expect("write YAML config");
        fs::write(
            config_dir.join("config.toml"),
            "model = 'nous:fixture-model'\n",
        )
        .expect("write TOML config");

        let config = load_gateway_config(Some(&config_dir)).expect("load explicit config");
        assert_eq!(config.model.as_deref(), Some("nous:fixture-model"));
    }

    #[test]
    fn model_resolution_prefers_nonblank_cli_then_config_then_environment() {
        if !is_bench_test_target() {
            return;
        }
        let Some(home) = isolated_home() else {
            for (environment, expected) in [
                (
                    Some(" \t test:environment-model \n"),
                    "test:environment-model",
                ),
                (Some(" \t\n"), "nous:Hermes-4-70B"),
                (None, "nous:Hermes-4-70B"),
            ] {
                let mut overrides = vec![(EXPECTED_MODEL, expected)];
                if let Some(value) = environment {
                    overrides.push(("HERMES_EVAL_MODEL", value));
                }
                run_isolated(
                    "model_resolution_prefers_nonblank_cli_then_config_then_environment",
                    &overrides,
                );
            }
            return;
        };

        let mut config = load_gateway_config(Some(&home)).expect("load isolated config");
        config.model = Some("  test:config-model \t".to_owned());
        let mut args = Args::try_parse_from([
            "hermes-bench",
            "--benchmark",
            "unused.toml",
            "--model",
            "  test:cli-model \n",
        ])
        .expect("parse test arguments");
        assert_eq!(resolve_model(&args, &config), "test:cli-model");

        args.model = Some(" \t ".to_owned());
        assert_eq!(resolve_model(&args, &config), "test:config-model");

        let expected = env::var(EXPECTED_MODEL).expect("expected isolated model");
        config.model = Some("\n ".to_owned());
        assert_eq!(resolve_model(&args, &config), expected);

        args.model = None;
        config.model = None;
        assert_eq!(resolve_model(&args, &config), expected);
    }

    #[test]
    fn default_output_path_uses_the_stem_or_the_benchmark_fallback() {
        if !is_bench_test_target() {
            return;
        }

        for (benchmark, expected_prefix) in [
            (Path::new("datasets/smoke.v2.toml"), "smoke.v2-"),
            (Path::new(""), "benchmark-"),
        ] {
            let before = jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S").to_string();
            let output = default_output_path(benchmark);
            let after = jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S").to_string();

            assert_eq!(output.parent(), Some(Path::new("evals")));
            assert_eq!(
                output.extension().and_then(|ext| ext.to_str()),
                Some("json")
            );
            let stamp = output
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix(expected_prefix))
                .expect("output filename starts with the expected stem");
            assert!(
                (before.as_str()..=after.as_str()).contains(&stamp),
                "unexpected timestamp in {}",
                output.display()
            );
        }
    }

    #[cfg(feature = "agent-loop")]
    #[tokio::test]
    async fn agent_rollout_obeys_an_empty_task_selection() {
        if !is_bench_test_target() {
            return;
        }
        let Some(home) = isolated_home() else {
            run_isolated("agent_rollout_obeys_an_empty_task_selection", &[]);
            return;
        };

        let config = load_gateway_config(Some(&home)).expect("load isolated config");
        let benchmark =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/configured-smoke.toml");
        let adapter = Arc::new(
            ConfiguredBenchmarkAdapter::from_path(&benchmark)
                .expect("load configured smoke fixture"),
        );
        let model = "nous:Hermes-4-70B";
        let runner = Runner::new(RunnerConfig {
            model: model.to_owned(),
            concurrency: 1,
            max_tasks: Some(0),
            task_filter: Some("coverage-no-match-6ce70ed7a8804cf8".to_owned()),
            seed: 0,
            continue_on_error: true,
        });

        // Check selection with the offline backend before constructing the agent.
        let smoke = runner
            .run(Arc::clone(&adapter), Arc::new(SmokeRollout))
            .await;
        if let Ok(record) = &smoke {
            assert_eq!(record.metrics.total, 0, "no task may reach the real agent");
        }

        let agent = run_agent_rollout(adapter, &runner, &config, model, true).await;
        match (smoke, agent) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(actual.benchmark.id, expected.benchmark.id);
                assert_eq!(actual.metrics.total, 0);
                assert_eq!(actual.metrics.passed, 0);
                assert_eq!(actual.metrics.failed, 0);
                assert_eq!(actual.metrics.error, 0);
                assert_eq!(actual.metrics.timeout, 0);
            }
            (Err(expected), Err(actual)) => assert_eq!(actual.to_string(), expected.to_string()),
            (Ok(_), Err(error)) => panic!("agent setup failed for an empty selection: {error}"),
            (Err(error), Ok(_)) => panic!("agent run did not propagate the runner error: {error}"),
        }
    }
}
