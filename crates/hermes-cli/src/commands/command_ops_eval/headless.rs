/// Options for `hermes slo-rollback`.
#[derive(Debug, Clone, Default)]
pub struct SloRollbackOptions {
    pub repo_root: Option<PathBuf>,
    pub check_cmd: Option<String>,
    pub rollback_cmd: Option<String>,
    pub dry_run: bool,
    pub report_path: Option<PathBuf>,
    pub json: bool,
}

/// Options for `hermes session-eval`.
#[derive(Debug, Clone)]
pub struct SessionEvalOptions {
    pub sessions_dir: Option<PathBuf>,
    pub max_sessions: usize,
    pub out_json: Option<PathBuf>,
    pub json: bool,
}

/// `~` and `~/...` name the home folder, as a shell would expand them.
fn expand_cli_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    match raw.strip_prefix('~') {
        Some("") => hermes_config::user_home_dir(),
        Some(rest) if rest.starts_with(std::path::is_separator) => {
            hermes_config::user_home_dir().join(rest.trim_start_matches(std::path::is_separator))
        }
        _ => path.to_path_buf(),
    }
}

fn absolute_cli_path(path: &Path) -> Result<PathBuf, AgentError> {
    std::path::absolute(expand_cli_path(path))
        .map_err(|e| AgentError::Io(format!("resolve {}: {e}", path.display())))
}

/// A flag wins over its environment variable; blank values count as unset.
fn flag_or_env(flag: Option<String>, var: &str) -> Option<String> {
    flag.filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var(var).ok().filter(|v| !v.trim().is_empty()))
}

/// Run the SLO check and, when it fails, the rollback command; print the
/// result. Returns whether the SLO was violated.
pub async fn run_slo_rollback_cli(opts: SloRollbackOptions) -> Result<bool, AgentError> {
    let check = flag_or_env(opts.check_cmd, "HERMES_SLO_CHECK_CMD")
        .ok_or_else(|| AgentError::Config("set --check-cmd or HERMES_SLO_CHECK_CMD".to_string()))?;
    let rollback = flag_or_env(opts.rollback_cmd, "HERMES_SLO_ROLLBACK_CMD").ok_or_else(|| {
        AgentError::Config("set --rollback-cmd or HERMES_SLO_ROLLBACK_CMD".to_string())
    })?;
    let repo_root = absolute_cli_path(opts.repo_root.as_deref().unwrap_or(Path::new(".")))?;
    if !repo_root.is_dir() {
        return Err(AgentError::Config(format!(
            "repo root does not exist: {}",
            repo_root.display()
        )));
    }
    let report_path = opts
        .report_path
        .as_deref()
        .map(absolute_cli_path)
        .transpose()?;
    let (report, path) = run_slo_auto_rollback_native(
        &repo_root,
        &check,
        &rollback,
        opts.dry_run,
        report_path.as_deref(),
    )
    .await?;
    let violated = report["violated"].as_bool().unwrap_or(true);
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        if violated {
            let status = if report["rollback"]["ok"].as_bool() == Some(true) {
                "executed"
            } else {
                "failed_or_skipped"
            };
            println!("[slo-auto-rollback] VIOLATED (rollback={status})");
        } else {
            println!("[slo-auto-rollback] PASSED");
        }
        println!("[slo-auto-rollback] Report: {}", path.display());
    }
    Ok(violated)
}

/// Write a session eval report and print it or its summary.
pub fn run_session_eval_cli(opts: SessionEvalOptions) -> Result<(), AgentError> {
    let sessions_dir = match opts.sessions_dir.as_deref() {
        Some(dir) => absolute_cli_path(dir)?,
        None => hermes_config::hermes_home().join("sessions"),
    };
    let out_path = match opts.out_json.as_deref() {
        Some(path) => absolute_cli_path(path)?,
        None => session_eval_report_dir()
            .join(format!("session-eval-harness-{}.json", utc_compact_stamp())),
    };
    let sessions = load_latest_session_eval_stats(&sessions_dir, opts.max_sessions);
    let report = build_session_eval_report(&sessions_dir, &sessions);
    write_json_report(&out_path, &report)?;
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        eprintln!("report_path={}", out_path.display());
    } else {
        let summary = &report["summary"];
        let status = if report["ok"].as_bool() == Some(true) {
            "PASS"
        } else {
            "FAIL"
        };
        println!(
            "[session-eval-harness] {status} sessions={} avg_msgs={} tool_sessions={}",
            summary["sessions_analyzed"],
            summary["avg_messages_per_session"],
            summary["tool_activity_sessions"]
        );
        println!("[session-eval-harness] Report: {}", out_path.display());
    }
    Ok(())
}

#[cfg(test)]
mod headless_report_tests {
    use super::*;

    fn slo(
        repo: &Path,
        check: &str,
        rollback: &str,
        dry_run: bool,
        report: &Path,
    ) -> SloRollbackOptions {
        SloRollbackOptions {
            repo_root: Some(repo.to_path_buf()),
            check_cmd: Some(check.to_string()),
            rollback_cmd: Some(rollback.to_string()),
            dry_run,
            report_path: Some(report.to_path_buf()),
            json: false,
        }
    }

    #[tokio::test]
    async fn slo_rollback_reports_pass_violation_and_dry_run() {
        let repo = tempfile::tempdir().expect("repo");
        let marker = repo.path().join("rolled-back");
        let rollback = format!("touch '{}'", marker.display());
        let report = repo.path().join("out/slo.json");

        let violated = run_slo_rollback_cli(slo(repo.path(), "true", &rollback, false, &report))
            .await
            .expect("pass");
        assert!(!violated);
        assert!(!marker.exists());

        let violated = run_slo_rollback_cli(slo(repo.path(), "false", &rollback, true, &report))
            .await
            .expect("dry run");
        assert!(violated);
        assert!(!marker.exists(), "dry run must not roll back");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report).expect("report")).expect("json");
        assert_eq!(written["rollback"]["reason"], "dry_run");

        let violated = run_slo_rollback_cli(slo(repo.path(), "false", &rollback, false, &report))
            .await
            .expect("rollback");
        assert!(violated);
        assert!(marker.exists());
    }

    #[tokio::test]
    async fn slo_rollback_rejects_a_missing_repo_root() {
        let repo = tempfile::tempdir().expect("repo");
        let missing = repo.path().join("missing");
        let err = run_slo_rollback_cli(slo(
            &missing,
            "true",
            "true",
            false,
            &repo.path().join("r.json"),
        ))
        .await
        .expect_err("missing root");
        assert!(
            err.to_string().contains("repo root does not exist"),
            "{err}"
        );
    }

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn flags_win_over_environment_and_blanks_count_as_unset() {
        let _lock = crate::test_env_lock::lock();
        let var = "HERMES_HEADLESS_REPORT_TEST_VAR";
        {
            let _unset = ScopedEnvVar::remove(var);
            assert_eq!(
                flag_or_env(Some("flag".into()), var).as_deref(),
                Some("flag")
            );
            assert_eq!(flag_or_env(Some("  ".into()), var), None);
            assert_eq!(flag_or_env(None, var), None);
        }
        {
            let _populated = ScopedEnvVar::set(var, "env");
            assert_eq!(
                flag_or_env(Some("flag".into()), var).as_deref(),
                Some("flag")
            );
            assert_eq!(
                flag_or_env(Some("  ".into()), var).as_deref(),
                Some("env")
            );
            assert_eq!(flag_or_env(None, var).as_deref(), Some("env"));
        }
        {
            let _blank_env = ScopedEnvVar::set(var, "   ");
            assert_eq!(
                flag_or_env(Some("flag".into()), var).as_deref(),
                Some("flag")
            );
            assert_eq!(flag_or_env(Some("  ".into()), var), None);
            assert_eq!(flag_or_env(None, var), None);
        }
    }

    #[tokio::test]
    async fn slo_rollback_uses_env_on_blank_flags_and_reports_rollback_failure() {
        let _lock = crate::test_env_lock::lock_async().await;
        let _check = ScopedEnvVar::set("HERMES_SLO_CHECK_CMD", "false");
        let _rollback = ScopedEnvVar::set("HERMES_SLO_ROLLBACK_CMD", "false");
        let repo = tempfile::tempdir().expect("repo");
        let report = repo.path().join("out/slo-env.json");
        let violated = run_slo_rollback_cli(SloRollbackOptions {
            repo_root: Some(repo.path().to_path_buf()),
            check_cmd: Some("  ".into()),
            rollback_cmd: None,
            dry_run: false,
            report_path: Some(report.clone()),
            json: false,
        })
        .await
        .expect("env rollback failure");
        assert!(violated);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report).expect("report")).expect("json");
        assert_eq!(written["violated"], true);
        assert_eq!(written["rollback"]["ok"], false);
    }

    #[test]
    fn session_eval_empty_dir_writes_fail_report() {
        let dir = tempfile::tempdir().expect("dir");
        let sessions = dir.path().join("empty-sessions");
        std::fs::create_dir_all(&sessions).expect("sessions");
        let out = dir.path().join("reports/empty-eval.json");
        run_session_eval_cli(SessionEvalOptions {
            sessions_dir: Some(sessions),
            max_sessions: 5,
            out_json: Some(out.clone()),
            json: false,
        })
        .expect("empty eval succeeds");
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).expect("report")).expect("json");
        assert_eq!(report["ok"], false);
        assert!(report["reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .any(|r| r == "no_saved_sessions"));
    }

    #[test]
    fn session_eval_writes_the_named_report_and_reads_at_least_one_session() {
        let dir = tempfile::tempdir().expect("dir");
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions");
        for name in ["a", "b"] {
            std::fs::write(
                sessions.join(format!("{name}.json")),
                r#"{"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"ok"}]}"#,
            )
            .expect("session");
        }
        let out = dir.path().join("reports/eval.json");
        run_session_eval_cli(SessionEvalOptions {
            sessions_dir: Some(sessions),
            max_sessions: 0,
            out_json: Some(out.clone()),
            json: false,
        })
        .expect("eval");
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).expect("report")).expect("json");
        assert_eq!(report["summary"]["sessions_analyzed"], 1);
    }

    #[test]
    fn tilde_names_the_home_folder() {
        let home = hermes_config::user_home_dir();
        assert_eq!(expand_cli_path(Path::new("~")), home);
        assert_eq!(expand_cli_path(Path::new("~/x")), home.join("x"));
        assert_eq!(expand_cli_path(Path::new("a/~")), PathBuf::from("a/~"));
    }
}
