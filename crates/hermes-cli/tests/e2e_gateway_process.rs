//! E2E：短生命周期真实 `hermes gateway start` 子进程（无聊天适配器，仅 cron + webhook 侧车）。
//!
//! - 等待 `gateway.pid` 出现即视为就绪；
//! - `hermes gateway status` 应报告 running；
//! - 对子进程发 `SIGINT`，与交互式 Ctrl+C 一致，触发清理并删除 PID 文件。
//!
//! Windows 上信号语义与 `kill` 不同，测试仅在 Unix 注册。

#[cfg(unix)]
mod unix {
    use assert_cmd::cargo::cargo_bin;
    use assert_cmd::Command;
    use std::fs;
    use std::path::Path;
    use std::process::{Command as SysCommand, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const READY_TIMEOUT: Duration = Duration::from_secs(60);
    const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

    fn wait_gateway_ready(pid_path: &Path, child: &mut std::process::Child) -> u32 {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!(
                    "timeout waiting for {}; child id {:?}",
                    pid_path.display(),
                    child.id()
                );
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "gateway exited before ready (status={status}); expected {}",
                    pid_path.display()
                );
            }
            if let Ok(raw) = fs::read_to_string(pid_path) {
                if let Ok(pid) = raw.trim().parse::<u32>() {
                    return pid;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_child_exit(
        child: &mut std::process::Child,
        deadline: Instant,
    ) -> std::process::ExitStatus {
        loop {
            if let Ok(Some(s)) = child.try_wait() {
                return s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("timeout waiting for gateway child to exit");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn e2e_gateway_subprocess_lifecycle_start_status_sigint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let pid_path = home.join("gateway.pid");

        let mut child = SysCommand::new(cargo_bin("hermes"))
            .env("HERMES_HOME", home)
            .args(["gateway", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn hermes gateway start");

        let file_pid = wait_gateway_ready(&pid_path, &mut child);
        assert!(file_pid > 0, "gateway.pid should contain a positive PID");

        let mut status = Command::cargo_bin("hermes").expect("hermes binary");
        status.env("HERMES_HOME", home).args(["gateway", "status"]);
        let out = status.assert().success().get_output().stdout.clone();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("running"),
            "expected 'running' in gateway status, got: {text:?}"
        );

        let kill_st = SysCommand::new("kill")
            .args(["-INT", &file_pid.to_string()])
            .status()
            .expect("kill -INT");
        assert!(kill_st.success(), "kill -INT failed: {kill_st:?}");

        let status = wait_child_exit(&mut child, Instant::now() + EXIT_TIMEOUT);
        assert!(
            status.success(),
            "gateway should exit 0 after SIGINT shutdown, got {status:?}"
        );

        assert!(
            !pid_path.exists(),
            "gateway.pid should be removed after graceful shutdown"
        );
    }

    fn isolated_gateway_cli(home: &std::path::Path, args: &[&str]) -> assert_cmd::Command {
        let managed = home.join("managed");
        std::fs::create_dir_all(&managed).expect("create empty managed config directory");
        let mut cmd = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("hermes"));
        cmd.env_clear()
            .current_dir(home)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("HERMES_HOME", home)
            .env("HERMES_MANAGED_DIR", managed)
            .timeout(std::time::Duration::from_secs(15))
            .args(args);
        for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        cmd
    }

    #[test]
    fn gateway_cli_rejects_missing_requirements_before_starting_adapters() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join("config.yaml"),
            "platforms:\n  discord:\n    enabled: true\n  ntfy:\n    enabled: true\n",
        )
        .unwrap();

        let mut command = isolated_gateway_cli(home, &["gateway", "run"]);
        let assertion = command.assert().failure();
        let output = assertion.get_output();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("Gateway requirement check failed"),
            "{error}"
        );
        assert!(error.contains("discord.enabled=true"), "{error}");
        assert!(
            error.contains("ntfy.enabled=true but topic is missing"),
            "{error}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("no gateway user allowlist configured"),
            "{stdout}"
        );
        assert!(!home.join("gateway.pid").exists());
        assert!(!home.join("gateway.lock").exists());
    }

    #[test]
    fn gateway_cli_status_cleans_invalid_pid_without_unlinking_owner_lock() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let pid_file = home.join("gateway.pid");
        let lock_file = home.join("gateway.lock");
        std::fs::write(&pid_file, "historical metadata").unwrap();
        std::fs::write(&lock_file, "owner marker").unwrap();

        let mut command = isolated_gateway_cli(home, &["gateway", "status"]);
        let assertion = command.assert().success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout);
        assert!(stdout.contains("invalid PID file"), "{stdout}");
        assert!(!pid_file.exists());
        assert_eq!(std::fs::read_to_string(&lock_file).unwrap(), "owner marker");
    }

    #[test]
    fn gateway_cli_setup_saves_only_selected_platform_choices() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config_path = home.join("config.yaml");

        let mut empty = isolated_gateway_cli(home, &["gateway", "setup"]);
        let assertion = empty.write_stdin("\n").assert().success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout);
        assert!(stdout.contains("No valid platforms selected"), "{stdout}");
        assert!(!config_path.exists());

        // Choose Webhook and API Server in the numbered fallback. Prompts are
        // presented in alphabetical order after the selection is confirmed.
        let mut configure = isolated_gateway_cli(home, &["gateway", "setup"]);
        configure
            .write_stdin("19\n20\n\n127.0.0.1\n18090\n\n\n19000\n/fixture-hook\n")
            .assert()
            .success();

        let raw = std::fs::read_to_string(config_path).unwrap();
        let config: hermes_config::GatewayConfig = serde_yaml::from_str(&raw).unwrap();
        let api = &config.platforms["api_server"];
        assert!(api.enabled);
        assert_eq!(api.extra["host"], "127.0.0.1");
        assert_eq!(api.extra["port"], 18090);
        let hook = &config.platforms["webhook"];
        assert!(hook.enabled);
        assert_eq!(hook.extra["port"], 19000);
        assert_eq!(hook.extra["path"], "/fixture-hook");
        assert_eq!(
            config
                .platforms
                .values()
                .filter(|platform| platform.enabled)
                .count(),
            2
        );
    }
}
