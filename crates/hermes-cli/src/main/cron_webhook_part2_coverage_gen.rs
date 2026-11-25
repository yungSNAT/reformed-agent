        #[tokio::test]
        async fn cron_job_lifecycle_persists_edits_and_status() {
            let Some(state) = isolated_state(&[], |stdout| {
                for expected in [
                    "(no cron jobs in ",
                    "Created cron job id=",
                    "Updated job ",
                    "Paused job ",
                    "Cron jobs (",
                    "No due jobs at ",
                    "jobs_total=1 active=0",
                    "\"prompt\": \"updated prompt\"",
                    "Resumed job ",
                    "Deleted job ",
                ] {
                    assert!(
                        stdout.contains(expected),
                        "missing {expected:?} in {stdout}"
                    );
                }
            }) else {
                return;
            };

            CronArgs::default().run(&state).await.unwrap();

            let script = state.join("fixture.sh");
            let replacement_script = state.join("replacement.sh");
            fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
            fs::write(&replacement_script, "#!/bin/sh\nexit 0\n").unwrap();
            let script = script.to_str().unwrap();
            let replacement_script = replacement_script.to_str().unwrap();
            let workdir = state.to_str().unwrap();

            CronArgs {
                action: Some("add"),
                name: Some("initial name"),
                deliver: Some("EMAIL: inbox"),
                repeat: Some(3),
                skills: &["original"],
                script: Some(script),
                no_agent: true,
                timeout: Some(11),
                shell: Some("  sh  "),
                workdir: Some(workdir),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();

            let job = sole_job(&state).await;
            let id = job.id.as_str();
            assert_eq!(job.schedule, "0 * * * *");
            assert_eq!(job.prompt, "[script-only cron job]");
            assert_eq!(job.name.as_deref(), Some("initial name"));
            assert_eq!(
                job.deliver.as_ref().unwrap().platform.as_deref(),
                Some("inbox")
            );
            assert_eq!(job.repeat, Some(3));
            assert_eq!(job.skills.as_deref().unwrap(), ["original"]);
            assert_eq!(job.script.as_deref(), Some(script));
            assert!(job.no_agent);
            assert_eq!(job.script_timeout_seconds, Some(11));
            assert_eq!(job.script_shell.as_deref(), Some("sh"));
            assert_eq!(job.workdir.as_deref(), Some(workdir));

            assert_config_error(
                CronArgs {
                    action: Some("edit"),
                    id: Some(id),
                    script: Some(" "),
                    ..CronArgs::default()
                }
                .run(&state)
                .await,
                "no_agent mode requires a non-empty script",
            );
            assert_config_error(
                CronArgs {
                    action: Some("edit"),
                    id: Some(id),
                    deliver: Some("unknown-target"),
                    ..CronArgs::default()
                }
                .run(&state)
                .await,
                "Unknown deliver target 'unknown-target'",
            );

            let unchanged = sole_job(&state).await;
            assert_eq!(unchanged.script.as_deref(), Some(script));
            assert_eq!(
                unchanged.deliver.unwrap().platform.as_deref(),
                Some("inbox")
            );

            CronArgs {
                action: Some("edit"),
                job_id: Some(id),
                id: Some("not-the-job"),
                schedule: Some("*/15 * * * *"),
                prompt: Some("updated prompt"),
                name: Some("changed name"),
                deliver: Some("slack:inline"),
                chat_id: Some(" explicit "),
                repeat: Some(2),
                skills: &["discarded"],
                clear_skills: true,
                add_skills: &["keep", "drop", "keep"],
                remove_skills: &["drop"],
                script: Some(replacement_script),
                no_agent: true,
                agent: false,
                timeout: Some(22),
                shell: Some(" bash "),
                workdir: Some(workdir),
            }
            .run(&state)
            .await
            .unwrap();

            let edited = sole_job(&state).await;
            assert_eq!(edited.id, id);
            assert_eq!(edited.schedule, "*/15 * * * *");
            assert_eq!(edited.prompt, "updated prompt");
            assert_eq!(edited.name.as_deref(), Some("changed name"));
            let delivery = edited.deliver.unwrap();
            assert_eq!(
                cron_deliver_target_platform_name(&delivery.target),
                Some("slack")
            );
            assert_eq!(delivery.platform.as_deref(), Some("explicit"));
            assert_eq!(edited.repeat, Some(2));
            assert_eq!(edited.skills.as_deref().unwrap(), ["keep"]);
            assert_eq!(edited.script.as_deref(), Some(replacement_script));
            assert!(edited.no_agent);
            assert_eq!(edited.script_timeout_seconds, Some(22));
            assert_eq!(edited.script_shell.as_deref(), Some("bash"));
            assert_eq!(edited.workdir.as_deref(), Some(workdir));

            CronArgs {
                action: Some("edit"),
                id: Some(id),
                name: Some(" "),
                remove_skills: &["keep"],
                script: Some(" "),
                agent: true,
                timeout: Some(0),
                shell: Some(" "),
                workdir: Some(" "),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();

            let cleared = sole_job(&state).await;
            assert!(cleared.name.is_none());
            assert!(cleared.skills.is_none());
            assert!(cleared.script.is_none());
            assert!(!cleared.no_agent);
            assert!(cleared.script_timeout_seconds.is_none());
            assert!(cleared.script_shell.is_none());
            assert!(cleared.workdir.is_none());

            for action in ["pause", "status", "tick", "list", "history"] {
                CronArgs {
                    action: Some(action),
                    id: Some(id),
                    ..CronArgs::default()
                }
                .run(&state)
                .await
                .unwrap();
            }
            assert!(matches!(
                sole_job(&state).await.status,
                hermes_cron::JobStatus::Paused
            ));

            CronArgs {
                action: Some("resume"),
                id: Some(id),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            assert!(matches!(
                sole_job(&state).await.status,
                hermes_cron::JobStatus::Active
            ));

            CronArgs {
                action: Some("remove"),
                id: Some(id),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            assert!(persisted_jobs(&state).await.is_empty());
            assert_config_error(
                CronArgs {
                    action: Some("history"),
                    id: Some(id),
                    ..CronArgs::default()
                }
                .run(&state)
                .await,
                "unknown job id:",
            );
        }

        #[tokio::test]
        async fn cron_create_agent_flag_overrides_script_only_mode() {
            let Some(state) = isolated_state(&[], |_| {}) else {
                return;
            };
            CronArgs {
                action: Some("create"),
                schedule: Some("0 12 * * *"),
                prompt: Some("report status"),
                name: Some(" "),
                script: Some(" "),
                no_agent: true,
                agent: true,
                timeout: Some(0),
                shell: Some(" "),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();

            let job = sole_job(&state).await;
            assert_eq!(job.schedule, "0 12 * * *");
            assert_eq!(job.prompt, "report status");
            assert!(!job.no_agent);
            assert!(job.name.is_none());
            assert!(job.script.is_none());
            assert!(job.script_timeout_seconds.is_none());
            assert!(job.script_shell.is_none());
        }

        #[tokio::test]
        async fn cron_rejects_bad_arguments_and_a_blocked_data_directory() {
            let Some(state) = isolated_state(&[], |_| {}) else {
                return;
            };
            let data_dir = cron_data_dir(&state);
            fs::create_dir_all(data_dir.parent().unwrap()).unwrap();
            fs::write(&data_dir, "not a directory").unwrap();
            assert_io_error(CronArgs::default().run(&state).await, "cron dir ");
            assert_eq!(fs::read_to_string(&data_dir).unwrap(), "not a directory");
            fs::remove_file(&data_dir).unwrap();

            for (args, error) in [
                (
                    CronArgs {
                        action: Some("create"),
                        ..CronArgs::default()
                    },
                    "cron create: use --prompt",
                ),
                (
                    CronArgs {
                        action: Some("create"),
                        deliver: Some("unknown-target"),
                        prompt: Some("a prompt"),
                        ..CronArgs::default()
                    },
                    "Unknown deliver target 'unknown-target'",
                ),
                (
                    CronArgs {
                        action: Some("create"),
                        no_agent: true,
                        script: Some(" "),
                        ..CronArgs::default()
                    },
                    "--no-agent requires --script",
                ),
                (
                    CronArgs {
                        action: Some("edit"),
                        job_id: Some(" "),
                        ..CronArgs::default()
                    },
                    "cron edit: use <job-id> or --id",
                ),
                (
                    CronArgs {
                        action: Some("edit"),
                        id: Some("absent"),
                        ..CronArgs::default()
                    },
                    "unknown job id: absent",
                ),
                (
                    CronArgs {
                        action: Some("delete"),
                        ..CronArgs::default()
                    },
                    "delete: use <job-id> or --id",
                ),
                (
                    CronArgs {
                        action: Some("unrecognized"),
                        ..CronArgs::default()
                    },
                    "Unknown cron action: unrecognized",
                ),
            ] {
                assert_config_error(args.run(&state).await, error);
            }
            assert!(persisted_jobs(&state).await.is_empty());
        }

        #[test]
        fn subscription_store_defaults_round_trip_and_reports_io_errors() {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source.json");
            assert!(load_webhook_subscriptions(&source).unwrap().is_empty());

            fs::write(
                &source,
                r#"{"example":{"secret":"test-secret","created_at":"2026-01-02T03:04:05Z"}}"#,
            )
            .unwrap();
            let subs = load_webhook_subscriptions(&source).unwrap();
            let sub = &subs["example"];
            assert_eq!(sub.deliver, "log");
            assert!(sub.description.is_empty());
            assert!(sub.events.is_empty());
            assert!(sub.prompt.is_empty());
            assert!(sub.skills.is_empty());
            assert!(!sub.deliver_only);
            assert!(sub.deliver_extra.is_none());

            let destination = temp.path().join("nested").join("subscriptions.json");
            save_webhook_subscriptions(&destination, &subs).unwrap();
            let saved = load_webhook_subscriptions(&destination).unwrap();
            assert_eq!(saved.len(), 1);
            assert_eq!(saved["example"].secret, "test-secret");
            assert_eq!(saved["example"].created_at, "2026-01-02T03:04:05Z");
            let json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&destination).unwrap()).unwrap();
            assert_eq!(json["example"]["deliver"], "log");
            assert!(json["example"].get("deliver_extra").is_none());

            fs::write(&source, "{unfinished").unwrap();
            assert_config_error(load_webhook_subscriptions(&source), "parse ");
            assert_io_error(load_webhook_subscriptions(temp.path()), "read ");
            assert_io_error(
                save_webhook_subscriptions(&source.join("child"), &subs),
                "mkdir ",
            );
            assert_io_error(save_webhook_subscriptions(temp.path(), &subs), "write ");
        }

        #[derive(Default)]
        struct WebhookArgs<'a> {
            action: Option<&'a str>,
            name: Option<&'a str>,
            url: Option<&'a str>,
            id: Option<&'a str>,
            prompt: Option<&'a str>,
            events: Option<&'a str>,
            description: Option<&'a str>,
            skills: Option<&'a str>,
            deliver: Option<&'a str>,
            chat_id: Option<&'a str>,
            secret: Option<&'a str>,
            deliver_only: bool,
        }
