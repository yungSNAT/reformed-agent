    async fn reports(home: &Path) {
        for action in ["list", "stats", "prune", "browse"] {
            handle_cli_sessions(options(action)).await.unwrap();
        }
        handle_cli_insights(1, None).await.unwrap();

        let dir = home.join("sessions");
        std::fs::create_dir(&dir).unwrap();
        for action in ["list", "browse"] {
            handle_cli_sessions(options(action)).await.unwrap();
        }

        write_snapshot(
            &dir,
            "recent",
            &json!({
                "source": "cli", "model": "fixture-model",
                "messages": [{"role":"user"}, {"role":"assistant"}],
                "usage": {"input_tokens":121, "output_tokens":79, "cost":12.5}
            }),
        );
        let earlier = write_snapshot(
            &dir,
            "earlier",
            &json!({"source":"cli", "model":"fixture-model", "messages":[{"role":"user"}]}),
        );
        let old = write_snapshot(&dir, "old", &json!({"source":"cli", "model":"old-model"}));
        write_snapshot(&dir, "other", &json!({"source":"web", "model":"web-model"}));
        let now = SystemTime::now();
        set_modified(&earlier, now - Duration::from_secs(2 * 3600 + 60));
        set_modified(&old, now - Duration::from_secs(5 * 86_400 + 60));
        std::fs::write(dir.join("invalid.json"), "not JSON").unwrap();
        std::fs::write(dir.join("ignored.txt"), "{}").unwrap();

        for action in ["list", "stats", "browse", "bogus"] {
            handle_cli_sessions(options(action)).await.unwrap();
        }
        handle_cli_insights(1, Some("cli".into())).await.unwrap();
    }

    #[test]
    fn cli_export_disambiguates_subjects_and_output_paths() {
        let output = run_scenario("exports", "");
        assert_output(
            &output,
            &[
                "Exported 2 sessions to",
                "Exported 1 prompt to",
                "Exported 2 sessions to positional.html",
                "Session 'missing' not found.",
                "No sessions found.",
                "\"id\": \"first-id\"",
            ],
        );
    }

    async fn exports(home: &Path) {
        let dir = home.join("sessions");
        std::fs::create_dir(&dir).unwrap();
        write_snapshot(
            &dir,
            "first",
            &json!({
                "id": "first-id",
                "messages": [{"role":"user", "content":"export me"}, {"role":"assistant", "content":"reply"}]
            }),
        );
        write_snapshot(&dir, "second", &json!({"id":"second-id"}));

        let mut args = options("export");
        args.format = Some("html".into());
        args.output = Some(" - ".into());
        assert_config_error(
            handle_cli_sessions(args).await.unwrap_err(),
            "HTML export requires an output file path.",
        );

        let mut args = options("export");
        args.session = Some(" \t ".into());
        args.output = Some("all.json".into());
        handle_cli_sessions(args).await.unwrap();
        let all: Value =
            serde_json::from_str(&std::fs::read_to_string("all.json").unwrap()).unwrap();
        let mut ids: Vec<_> = all
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["first-id", "second-id"]);

        let mut args = options("export");
        args.session = Some(" first ".into());
        args.format = Some("jsonl".into());
        args.only = Some("user-prompts".into());
        args.output = Some("prompts.jsonl".into());
        handle_cli_sessions(args).await.unwrap();
        let prompt: Value =
            serde_json::from_str(&std::fs::read_to_string("prompts.jsonl").unwrap()).unwrap();
        assert_eq!(
            prompt,
            json!({"index":1, "role":"user", "session_id":"first-id", "text":"export me"})
        );

        let mut args = options("export");
        args.format = Some("html".into());
        args.session = Some("positional.html".into());
        handle_cli_sessions(args).await.unwrap();
        let html = std::fs::read_to_string("positional.html").unwrap();
        assert_output(
            &html,
            &[
                "<title>Hermes sessions export (2 sessions)</title>",
                "<pre>export me</pre>",
            ],
        );

        let mut args = options("export");
        args.session = Some("missing".into());
        args.output = Some("missing.json".into());
        handle_cli_sessions(args).await.unwrap();
        assert!(!Path::new("missing.json").exists());

        let mut args = options("export");
        args.source = Some("no-such-source".into());
        args.output = Some("empty.json".into());
        handle_cli_sessions(args).await.unwrap();
        assert!(!Path::new("empty.json").exists());

        let mut args = options("export");
        args.id = Some("first".into());
        args.output = Some("-".into());
        handle_cli_sessions(args).await.unwrap();
    }

    #[test]
    fn delete_rename_and_prune_preserve_unselected_snapshots() {
        let output = run_scenario("mutations", "n\nno\n");
        assert_output(
            &output,
            &[
                "Session 'missing' not found.",
                "Delete session 'before'?",
                "Session renamed: before -> after",
                "0 session(s) match.",
                "Prune matching sessions?",
                "Pruned: after",
                "Pruned 1 session(s).",
            ],
        );
        assert_eq!(output.matches("Cancelled.").count(), 2);
    }

    async fn mutations(home: &Path) {
        let dir = home.join("sessions");
        std::fs::create_dir(&dir).unwrap();
        let before = write_snapshot(&dir, "before", &json!({"source":"CLI"}));
        let keep = write_snapshot(&dir, "keep", &json!({"source":"web"}));
        let original = std::fs::read(&before).unwrap();

        assert_config_error(
            handle_cli_sessions(options("delete")).await.unwrap_err(),
            "Missing session ID",
        );
        let mut args = options("delete");
        args.id = Some("missing".into());
        handle_cli_sessions(args).await.unwrap();
        let mut args = options("delete");
        args.id = Some("befo".into());
        handle_cli_sessions(args).await.unwrap();
        assert_eq!(std::fs::read(&before).unwrap(), original);

        assert_config_error(
            handle_cli_sessions(options("rename")).await.unwrap_err(),
            "Missing session ID",
        );
        let mut args = options("rename");
        args.id = Some("before".into());
        assert_config_error(
            handle_cli_sessions(args).await.unwrap_err(),
            "Missing new name",
        );

        let mut args = options("rename");
        args.id = Some("missing".into());
        args.name = Some("after".into());
        handle_cli_sessions(args).await.unwrap();
        let after = dir.join("after.json");
        assert!(!after.exists());

        let mut args = options("rename");
        args.id = Some("before".into());
        args.name = Some("nonexistent-parent/after".into());
        assert!(matches!(
            handle_cli_sessions(args).await,
            Err(hermes_core::AgentError::Io(_))
        ));
        assert!(before.is_file());

        let mut args = options("rename");
        args.id = Some("before".into());
        args.name = Some("after".into());
        handle_cli_sessions(args).await.unwrap();
        assert!(!before.exists());
        assert_eq!(std::fs::read(&after).unwrap(), original);

        let mut args = options("prune");
        args.source = Some("absent-source".into());
        handle_cli_sessions(args).await.unwrap();
        let mut args = options("prune");
        args.source = Some(" cli ".into());
        handle_cli_sessions(args).await.unwrap();
        assert_eq!(std::fs::read(&after).unwrap(), original);

        let mut args = options("prune");
        args.source = Some("cli".into());
        args.yes = true;
        handle_cli_sessions(args).await.unwrap();
        assert!(!after.exists());
        assert_eq!(
            std::fs::read_to_string(keep).unwrap(),
            r#"{"source":"web"}"#
        );
    }

    #[test]
    fn repair_reports_missing_and_unsupported_database_states() {
        let output = run_scenario("repair", "");
        assert_output(
            &output,
            &[
                "nothing to repair",
                "not the targeted malformed-schema repair class: no such table: sessions",
                "not the targeted malformed-schema repair class: file is not a database",
            ],
        );
        assert!(!output.contains("Repairing with a raw backup first"));
    }

    async fn repair(home: &Path) {
        let store = SessionPersistence::new(home);
        let db = store.db_path();
        assert!(db.starts_with(home) && !db.exists());
        handle_cli_sessions(options("repair")).await.unwrap();
        assert!(!db.exists());

        // A valid SQLite file without Hermes's sessions table is not a healthy store.
        rusqlite::Connection::open(db)
            .unwrap()
            .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY)")
            .unwrap();
        let incomplete = std::fs::read(db).unwrap();
        handle_cli_sessions(options("repair")).await.unwrap();
        assert_eq!(std::fs::read(db).unwrap(), incomplete);

        let corrupt = [b'!'; 4096];
        std::fs::write(db, corrupt.as_slice()).unwrap();
        handle_cli_sessions(options("repair")).await.unwrap();
        assert_eq!(std::fs::read(db).unwrap(), corrupt);
    }

    #[test]
    fn whatsapp_offline_paths_and_qr_block_art() {
        let output = run_scenario("whatsapp-status", "");
        assert_eq!(output.matches("WhatsApp: not configured").count(), 2);
        assert_output(
            &output,
            &[
                "Enabled:        true",
                "Phone Number ID: fixture-phone",
                "Access Token:   missing",
                "WhatsApp action 'bogus' is not recognized.",
                "(empty QR data)",
            ],
        );
        assert!(output.lines().any(|line| line == "  █▀▄ "));
        assert!(output.lines().any(|line| line == "  ▀  "));
        assert!(!output.contains("API Connectivity:"));

        for (input, message) in [
            (" \n", "phone number ID is required"),
            ("fixture-phone\n\n", "business account ID is required"),
            (
                "fixture-phone\nfixture-business\n \n",
                "access token is required",
            ),
        ] {
            assert_output(&run_scenario("whatsapp-setup", input), &[message]);
        }
    }

    async fn whatsapp_status_and_art(home: &Path) {
        let config = home.join("config.yaml");
        handle_cli_whatsapp(None).await.unwrap();
        std::fs::create_dir(&config).unwrap();
        assert!(matches!(
            handle_cli_whatsapp(None).await,
            Err(hermes_core::AgentError::Io(_))
        ));
        std::fs::remove_dir(&config).unwrap();
        std::fs::write(&config, "platforms: [").unwrap();
        handle_cli_whatsapp(None).await.unwrap();

        std::fs::write(
            &config,
            "platforms:\n  whatsapp:\n    enabled: true\n    phone_number_id: fixture-phone\n    access_token: ''\n",
        )
        .unwrap();
        handle_cli_whatsapp(Some("status".into())).await.unwrap();
        handle_cli_whatsapp(Some("bogus".into())).await.unwrap();
        render_qr_to_terminal("");
        render_qr_to_terminal("aabbababxx");
        render_qr_to_terminal("bbbbbba");
    }

    #[test]
    fn pairing_reports_empty_store_and_requires_device_ids() {
        assert_output(
            &run_scenario("pairing", ""),
            &[
                "No paired devices.",
                "No pending pairing requests to clear.",
                "Pairing action 'bogus' is not recognized.",
            ],
        );
    }

    async fn pairing(home: &Path) {
        use crate::pairing_store::PairingStore;

        let store_path = PairingStore::default_path();
        assert!(store_path.starts_with(home.parent().unwrap()));
        handle_cli_pairing(None, None).await.unwrap();
        for action in ["approve", "revoke"] {
            assert_config_error(
                handle_cli_pairing(Some(action.into()), None)
                    .await
                    .unwrap_err(),
                &format!("Missing --device-id. Usage: hermes pairing {action}"),
            );
        }
        for action in ["clear-pending", "bogus"] {
            handle_cli_pairing(Some(action.into()), None).await.unwrap();
        }
        assert!(PairingStore::open_default().list().unwrap().is_empty());
    }

    include!("tests/pairing_cli_contract.rs");

    #[test]
    fn claw_reports_legacy_files_and_only_cleans_after_confirmation() {
        assert_output(
            &run_scenario("claw", "no\n YeS \n"),
            &[
                "No OpenClaw directory found.",
                "Nothing to migrate.",
                "Nothing to clean up.",
                "config.yaml:       not found",
                "config.yaml:       present",
                ".env:              present",
                "skills/:           present",
                "sessions/:         1 file(s)",
                "Shell config references found:",
                "Cleanup cancelled.",
                "✓ Cleaned ~/.bashrc",
                "Cleanup complete.",
                "Claw action 'bogus' is not recognized.",
            ],
        );
    }

    async fn claw(hermes: &Path) {
        use crate::claw_migrate::find_openclaw_dir;

        let home = hermes.parent().unwrap();
        assert_eq!(dirs::home_dir().as_deref(), Some(home));
        assert!(find_openclaw_dir(None).unwrap().is_none());
        handle_cli_claw(None).await.unwrap();
        handle_cli_claw(Some("migrate".into())).await.unwrap();
        handle_cli_claw(Some("cleanup".into())).await.unwrap();

        let legacy = home.join(".openclaw");
        std::fs::create_dir(&legacy).unwrap();
        assert_eq!(
            find_openclaw_dir(None).unwrap().as_deref(),
            Some(legacy.as_path())
        );
        handle_cli_claw(None).await.unwrap();
        std::fs::create_dir(legacy.join("skills")).unwrap();
        std::fs::create_dir(legacy.join("sessions")).unwrap();
        std::fs::write(legacy.join("sessions").join("example.json"), "{}").unwrap();
        std::fs::write(legacy.join("config.yaml"), "{}").unwrap();
        std::fs::write(legacy.join(".env"), "FIXTURE=1\n").unwrap();
        let shell = home.join(".bashrc");
        let original = "# openclaw path\n# CLAWDBOT marker\nexport KEEP=1\n";
        std::fs::write(&shell, original).unwrap();

        handle_cli_claw(None).await.unwrap();
        handle_cli_claw(Some("cleanup".into())).await.unwrap();
        assert!(legacy.is_dir());
        assert_eq!(std::fs::read_to_string(&shell).unwrap(), original);
        handle_cli_claw(Some("cleanup".into())).await.unwrap();
        assert!(!legacy.exists());
        assert_eq!(std::fs::read_to_string(&shell).unwrap(), "export KEEP=1\n");
        handle_cli_claw(Some("bogus".into())).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "invoked by the isolated CLI tests"]
    async fn isolated_worker() {
        let Ok(scenario) = std::env::var(SCENARIO_ENV) else {
            return;
        };
        let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
        assert_eq!(hermes_config::hermes_home(), home);
        match scenario.as_str() {
            "reports" => reports(&home).await,
            "exports" => exports(&home).await,
            "mutations" => mutations(&home).await,
            "repair" => repair(&home).await,
            "whatsapp-status" => whatsapp_status_and_art(&home).await,
            "whatsapp-setup" => {
                handle_cli_whatsapp(Some("setup".into())).await.unwrap();
                assert!(!home.join("config.yaml").exists());
            }
            "pairing" => pairing(&home).await,
            "pairing-lifecycle" => pairing_lifecycle(&home).await,
            "claw" => claw(&home).await,
            other => panic!("unknown coverage scenario: {other}"),
        }
    }
