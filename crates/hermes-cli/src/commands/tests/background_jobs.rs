    /// A background task reaches its child on standard input, never on the
    /// command line, which other local users can read while the child runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn background_child_gets_its_task_on_stdin_not_argv() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let args_file = tmp.path().join("args");
        let stdin_file = tmp.path().join("stdin");
        let exe = tmp.path().join("child.sh");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > '{}'\n",
                args_file.display(),
                stdin_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();

        // Far larger than a pipe buffer, so the child must read whilst the
        // parent writes.
        let task = format!(
            "review canary 7f3a: {}",
            "the conversation so far ".repeat(50_000)
        );
        let child = spawn_background_child(&exe, &task).expect("spawn");
        let output = child.wait_with_output().await.expect("wait");
        assert!(output.status.success(), "{output:?}");

        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(!args.contains("canary"), "task on the command line: {args}");
        assert_eq!(std::fs::read_to_string(&stdin_file).unwrap(), task);
    }

    /// Job files hold the task and its output, so the folder is the owner's
    /// alone, including one an older version created open to others.
    #[cfg(unix)]
    #[tokio::test]
    async fn background_jobs_folder_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = test_env_lock::lock_async().await;
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let jobs = tmp.path().join("background_jobs");
        std::fs::create_dir(&jobs).unwrap();
        std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(background_jobs_dir().unwrap(), jobs);
        let mode = std::fs::metadata(&jobs).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "background_jobs mode {mode:o}");
    }

    /// Startup recovery narrows a jobs folder an older version created open,
    /// before reading the jobs it holds, without a new job being queued.
    #[cfg(unix)]
    #[tokio::test]
    async fn recovering_jobs_makes_an_open_jobs_folder_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = test_env_lock::lock_async().await;
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let jobs = tmp.path().join("background_jobs");
        std::fs::create_dir(&jobs).unwrap();
        std::fs::write(
            jobs.join("bg_old.json"),
            r#"{"status":"completed","task":"an old task"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(recover_queued_background_jobs(8), 0);
        let mode = std::fs::metadata(&jobs).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "background_jobs mode {mode:o}");
    }

    /// The CLI does not start with a jobs folder it cannot make owner-only:
    /// its job files would stay readable by other users while it runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_app_refuses_to_start_when_the_jobs_folder_cannot_be_narrowed() {
        // Root may change the mode of `/`, so the failure cannot be staged.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let _guard = test_env_lock::lock_async().await;
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        // A folder owned by another user: its mode cannot be changed.
        std::os::unix::fs::symlink("/", tmp.path().join("background_jobs")).unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let cli = crate::cli::Cli::try_parse_from([
            "hermes",
            "-C",
            config_dir.to_str().unwrap(),
            "--ignore-user-config",
            "--ignore-rules",
        ])
        .expect("parse cli");

        match App::new(cli).await {
            Ok(_) => panic!("the app started with a jobs folder open to other users"),
            Err(err) => assert!(err.to_string().contains("background_jobs"), "{err}"),
        }
    }

    /// `--query -` takes the query from standard input; any other query, or
    /// none, is left as given, and an empty input is refused.
    #[test]
    fn chat_query_dash_reads_standard_input() {
        let read = |query: Option<&str>, input: &str| {
            read_chat_query(query.map(str::to_string), input.as_bytes())
        };
        assert_eq!(
            read(Some("-"), "  the task\n").unwrap().as_deref(),
            Some("the task")
        );
        assert_eq!(
            read(Some("hello"), "ignored").unwrap().as_deref(),
            Some("hello")
        );
        assert_eq!(read(None, "ignored").unwrap(), None);
        assert!(read(Some("-"), " \n").is_err());
    }
