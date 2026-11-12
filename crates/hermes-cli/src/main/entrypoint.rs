/// Stack size for the thread that runs the async runtime's main future.
///
/// Windows gives the main thread 1 MiB of stack; Linux and macOS give it 8 MiB.
/// A debug build of this binary needs more than 1 MiB for ordinary commands, so
/// every platform runs the runtime on a thread with the Linux and macOS size.
const MAIN_RUNTIME_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() {
    let runtime_thread = std::thread::Builder::new()
        .name("hermes-main".to_string())
        .stack_size(MAIN_RUNTIME_STACK_BYTES)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed building the Runtime")
                .block_on(run_main())
        })
        .expect("failed to spawn the main runtime thread");
    if let Err(panic) = runtime_thread.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn run_main() {
    let mut cli = Cli::parse();
    let effective_command = cli.effective_command();
    if cli.oneshot.is_some() && matches!(effective_command, CliCommand::Resume(_)) {
        eprintln!(
            "Error: `resume` cannot be combined with -z/--oneshot, which starts a fresh session \
             without the saved history. Run `hermes-ultra resume <id>` in a terminal instead."
        );
        std::process::exit(1);
    }
    if let CliCommand::ImportHermesSessions(ImportHermesSessionsArgs {
        export,
        profile_dir,
    }) = &effective_command
    {
        match hermes_cli::commands::import_hermes_sessions(export, profile_dir) {
            Ok(summary) => println!(
                "Imported {} sessions with {} active messages into {}. Excluded {} inactive or metadata rows.",
                summary.sessions, summary.messages, profile_dir.display(), summary.excluded_rows,
            ),
            Err(error) => {
                eprintln!("Error: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Some(config_dir) = cli.config_dir.as_deref() {
        std::env::set_var("HERMES_HOME", config_dir);
    }
    hermes_config::normalize_hermes_home_env();
    if let Some(notice) = hermes_config::upstream_home_notice() {
        eprintln!("{notice}");
    }
    // The flag's later readers get the folder the environment now names, so a
    // quoted `~/x` is one folder for the configuration and for the state.
    if let (Some(config_dir), Ok(home)) = (cli.config_dir.as_mut(), std::env::var("HERMES_HOME")) {
        *config_dir = home;
    }
    if cli.ignore_user_config {
        std::env::set_var("HERMES_IGNORE_USER_CONFIG", "1");
    }
    if cli.ignore_rules {
        std::env::set_var("HERMES_IGNORE_RULES", "1");
        std::env::set_var("HERMES_AGENT_SKIP_CONTEXT_FILES", "1");
    }
    let global_model_override = cli.model.clone();
    let global_provider_override = cli.provider.clone();
    let global_allow_tools_override = cli.allow_tools;
    let oneshot_auth_model_hint = global_model_override.clone().or_else(|| {
        load_config(cli.config_dir.as_deref())
            .ok()
            .and_then(|config| config.model)
    });

    // Initialize tracing
    init_tracing(
        cli.verbose,
        matches!(
            effective_command,
            CliCommand::Hermes | CliCommand::Resume(_)
        ),
    );
    if let Err(err) = hydrate_provider_env_from_vault_for_cli(&cli).await {
        tracing::warn!("Secret-vault hydration skipped: {}", err);
    }
    if let Ok(cfg) = load_config(cli.config_dir.as_deref()) {
        let applied = hydrate_env_from_config(&cfg);
        tracing::debug!(
            applied_env_vars = applied,
            "Hydrated environment from config.yaml"
        );
    }
    let route_autotune_applied = apply_route_autotune_env_overrides(&cli);
    if !route_autotune_applied.is_empty() {
        tracing::debug!(
            applied_env_vars = ?route_autotune_applied,
            "Hydrated environment from route-autotune overrides"
        );
    }
    if let Err(err) = scrub_unusable_nous_api_key_for_oauth_state() {
        tracing::warn!("Nous API-key scrub skipped: {}", err);
    }

    tracing::debug!("Hermes Agent starting");

    if let Some(prompt) = cli.oneshot.clone() {
        match handle_local_slash_query(cli.clone(), &prompt).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(err) => {
                eprintln!("Error: {}", err);
                std::process::exit(1);
            }
        }
        if oneshot_should_use_app_runtime(&prompt) {
            let mut app = match App::new(cli.clone()).await {
                Ok(app) => app,
                Err(err) => {
                    eprintln!("Error: {}", err);
                    std::process::exit(1);
                }
            };
            if let Err(err) = app.handle_input(&prompt).await {
                eprintln!("Error: {}", err);
                std::process::exit(1);
            }
            print_app_oneshot_result(&app);
            return;
        }
        let mut result = hermes_cli::commands::handle_cli_chat(
            Some(prompt),
            None,
            false,
            global_model_override.clone(),
            global_provider_override.clone(),
            global_allow_tools_override,
        )
        .await;
        if let Err(err) = &result {
            if let Some(provider) = oneshot_auto_verify_oauth_provider(
                err,
                global_provider_override.as_deref(),
                oneshot_auth_model_hint.as_deref(),
            ) {
                eprintln!(
                    "Detected OAuth auth failure for provider '{}' in one-shot mode; running `hermes-ultra auth verify {}` and retrying once...",
                    provider, provider
                );
                if let Err(verify_err) = run_auth(
                    cli.clone(),
                    Some("verify".to_string()),
                    Some(provider.clone()),
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .await
                {
                    eprintln!(
                        "Warning: automatic `auth verify {}` failed: {}",
                        provider, verify_err
                    );
                }
                if let Err(hydrate_err) = hydrate_provider_env_from_vault_for_cli(&cli).await {
                    eprintln!(
                        "Warning: automatic credential hydration after `auth verify {}` failed: {}",
                        provider, hydrate_err
                    );
                }
                result = hermes_cli::commands::handle_cli_chat(
                    Some(cli.oneshot.clone().unwrap_or_default()),
                    None,
                    false,
                    global_model_override.clone(),
                    global_provider_override.clone(),
                    global_allow_tools_override,
                )
                .await;
                if let Err(retry_err) = &result {
                    if let Some(login_provider) = oneshot_auto_verify_oauth_provider(
                        retry_err,
                        Some(provider.as_str()),
                        oneshot_auth_model_hint.as_deref(),
                    ) {
                        if !oneshot_oauth_provider_supports_login(&login_provider) {
                            eprintln!(
                                "OAuth still invalid for provider '{}'. Run `hermes-ultra auth login {}` in a real terminal, then retry.",
                                login_provider, login_provider
                            );
                        } else if !oneshot_login_prompt_available() {
                            eprintln!(
                                "OAuth still invalid for provider '{}', but this process is not attached to an interactive terminal. Run `hermes-ultra auth login {}` in a real terminal, then retry.",
                                login_provider, login_provider
                            );
                        } else {
                            let force_fresh = oneshot_auth_requires_fresh_login(retry_err);
                            let freshness = if force_fresh { "fresh " } else { "" };
                            eprintln!(
                                "OAuth still invalid for provider '{}'; launching `hermes-ultra auth login {}` for a {}login and retrying once...",
                                login_provider, login_provider, freshness
                            );
                            if let Err(login_err) = run_oneshot_oauth_login_repair(
                                cli.clone(),
                                &login_provider,
                                force_fresh,
                            )
                            .await
                            {
                                eprintln!(
                                    "Warning: automatic `auth login {}` failed: {}",
                                    login_provider, login_err
                                );
                                eprintln!(
                                    "Action required: run `hermes-ultra auth login {}` in a real terminal, then retry this command.",
                                    login_provider
                                );
                            } else {
                                if let Err(hydrate_err) =
                                    hydrate_provider_env_from_vault_for_cli(&cli).await
                                {
                                    eprintln!(
                                        "Warning: automatic credential hydration after `auth login {}` failed: {}",
                                        login_provider, hydrate_err
                                    );
                                }
                                result = hermes_cli::commands::handle_cli_chat(
                                    Some(cli.oneshot.clone().unwrap_or_default()),
                                    None,
                                    false,
                                    global_model_override.clone(),
                                    global_provider_override.clone(),
                                    global_allow_tools_override,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
        }
        if let Err(e) = result {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    let result = match effective_command {
        CliCommand::Hermes => run_interactive(cli).await,
        CliCommand::Chat(ChatArgs {
            query,
            preload_skill,
            yolo,
        }) => {
            if let Some(prompt) = query.clone() {
                match handle_local_slash_query(cli.clone(), &prompt).await {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        hermes_cli::commands::handle_cli_chat(
                            query,
                            preload_skill,
                            yolo,
                            global_model_override.clone(),
                            global_provider_override.clone(),
                            global_allow_tools_override,
                        )
                        .await
                    }
                    Err(err) => Err(err),
                }
            } else {
                hermes_cli::commands::handle_cli_chat(
                    query,
                    preload_skill,
                    yolo,
                    global_model_override.clone(),
                    global_provider_override.clone(),
                    global_allow_tools_override,
                )
                .await
            }
        }
        CliCommand::Model(ModelArgs {
            completion_values,
            completion_providers,
            provider_model,
        }) => {
            if completion_values || completion_providers {
                run_model_completion_values(cli, completion_providers).await
            } else {
                run_model(cli, provider_model).await
            }
        }
        CliCommand::Tools(ToolsArgs {
            action,
            name,
            platform,
            summary,
        }) => run_tools(cli, action, name, platform, summary).await,
        CliCommand::ComputerUse(ComputerUseArgs {
            action,
            json,
            include,
            skip,
        }) => run_computer_use(action, json, include, skip).await,
        CliCommand::Config(ConfigArgs { action, key, value }) => {
            run_config(cli, action, key, value).await
        }
        CliCommand::Gateway(GatewayArgs {
            action,
            platform,
            system,
            all,
            force,
            run_as_user,
            replace,
            dry_run,
            yes,
            deep,
        }) => {
            run_gateway(
                cli,
                action,
                platform,
                system,
                all,
                force,
                run_as_user,
                replace,
                dry_run,
                yes,
                deep,
            )
            .await
        }
        CliCommand::Up(UpArgs { force, dry_run }) => run_up(force, dry_run).await,
        CliCommand::Setup(SetupArgs { portal }) => {
            if portal {
                run_portal(cli, Some("setup".to_string())).await
            } else {
                run_setup(cli).await
            }
        }
        CliCommand::Portal(PortalArgs { action }) => run_portal(cli, action).await,
        CliCommand::Billing(BillingArgs { args }) => run_billing(args).await,
        CliCommand::Doctor(DoctorArgs {
            deep,
            self_heal,
            snapshot,
            snapshot_path,
            bundle,
        }) => run_doctor(cli, deep, self_heal, snapshot, snapshot_path, bundle).await,
        CliCommand::Update(UpdateArgs { check }) => run_update(check).await,
        CliCommand::EliteCheck(EliteCheckArgs { json, strict }) => {
            run_elite_check(cli, json, strict).await
        }
        CliCommand::Systems(SystemsArgs {
            action,
            topic,
            json,
            output,
            host,
            port,
            once,
        }) => {
            hermes_cli::systems::handle_cli_systems(hermes_cli::systems::SystemsCliOptions {
                config_dir: cli.config_dir.clone(),
                action,
                topic,
                json_only: json,
                output,
                host,
                port,
                once,
            })
            .await
        }
        CliCommand::VerifyProvenance(VerifyProvenanceArgs {
            path,
            signature,
            strict,
            json,
        }) => run_verify_provenance(cli, path, signature, strict, json).await,
        CliCommand::RotateProvenanceKey(RotateProvenanceKeyArgs { json }) => {
            run_rotate_provenance_key(cli, json).await
        }
        CliCommand::RouteLearning(RouteLearningArgs { action, json }) => {
            run_route_learning(cli, action, json).await
        }
        CliCommand::RouteHealth(RouteHealthArgs { action, json }) => {
            run_route_health(cli, action, json).await
        }
        CliCommand::RouteAutotune(RouteAutotuneArgs {
            action,
            apply,
            strict,
            json,
        }) => run_route_autotune(cli, action, apply, strict, json).await,
        CliCommand::IncidentPack(IncidentPackArgs {
            snapshot,
            output,
            json,
        }) => run_incident_pack(cli, snapshot, output, json).await,
        CliCommand::Status => run_status(cli).await,
        CliCommand::Kanban(KanbanArgs { args }) => run_kanban(args),
        CliCommand::Dashboard(DashboardArgs {
            host,
            port,
            no_open,
            insecure,
        })
        | CliCommand::Serve(DashboardArgs {
            host,
            port,
            no_open,
            insecure,
        }) => run_dashboard(cli, host, port, no_open, insecure).await,
        CliCommand::Debug(DebugArgs {
            action,
            url,
            lines,
            expire,
            local,
        }) => run_debug(cli, action, url, lines, expire, local).await,
        CliCommand::Logs(LogsArgs { lines, follow }) => run_logs(cli, lines, follow).await,
        CliCommand::Profile(ProfileArgs {
            action,
            name,
            secondary,
            output,
            import_name,
            alias_name,
            remove,
            yes,
            clone,
            clone_all,
            clone_from,
            no_alias,
            no_skills,
        }) => {
            run_profile(
                cli,
                action,
                name,
                secondary,
                output,
                import_name,
                alias_name,
                remove,
                yes,
                clone,
                clone_all,
                clone_from,
                no_alias,
                no_skills,
            )
            .await
        }
        CliCommand::Auth(AuthArgs {
            action,
            provider,
            target,
            auth_type,
            label,
            api_key,
            qr,
        }) => run_auth(cli, action, provider, target, auth_type, label, api_key, qr).await,
        CliCommand::Secrets(SecretsArgs {
            action,
            provider,
            value,
            show,
        }) => run_secrets(cli, action, provider, value, show).await,
        CliCommand::Skills(SkillsArgs {
            action,
            name,
            extra,
            remove,
            yes,
            sync,
        }) => hermes_cli::commands::handle_cli_skills(action, name, extra, remove, yes, sync).await,
        CliCommand::Plugins(PluginsArgs {
            action,
            name,
            git_ref,
            allow_untrusted_git_host,
        }) => {
            hermes_cli::commands::handle_cli_plugins(
                action,
                name,
                git_ref,
                allow_untrusted_git_host,
            )
            .await
        }
        CliCommand::Memory(MemoryArgs {
            action,
            target,
            yes,
            mode,
            host,
            api_key,
            dry_run,
        }) => {
            hermes_cli::commands::handle_cli_memory(
                action,
                target,
                hermes_cli::commands::MemorySetupCliOptions {
                    yes,
                    mode,
                    host,
                    api_key,
                    dry_run,
                },
            )
            .await
        }
        CliCommand::Mcp(McpArgs {
            action,
            name,
            server,
            url,
            command,
            parallel_tools,
        }) => {
            hermes_cli::commands::handle_cli_mcp(action, name, server, url, command, parallel_tools)
                .await
        }
        CliCommand::Sessions(SessionsArgs {
            action,
            session,
            id,
            session_id,
            name,
            format,
            only,
            output,
            redact,
            yes,
            source,
            older_than,
        }) => {
            hermes_cli::commands::handle_cli_sessions(hermes_cli::commands::CliSessionsOptions {
                action,
                session,
                id,
                session_id,
                name,
                format,
                only,
                output,
                redact,
                yes,
                source,
                older_than,
            })
            .await
        }
        CliCommand::Resume(ResumeArgs { session_id }) => run_resume(cli, session_id).await,
        CliCommand::Insights(InsightsArgs { days, source }) => {
            hermes_cli::commands::handle_cli_insights(days, source).await
        }
        CliCommand::Login(LoginArgs { provider }) => {
            run_auth(
                cli,
                Some("login".to_string()),
                provider,
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        CliCommand::Logout(LogoutArgs { provider }) => {
            run_auth(
                cli,
                Some("logout".to_string()),
                provider,
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        CliCommand::Whatsapp(WhatsappArgs { action }) => {
            hermes_cli::commands::handle_cli_whatsapp(action).await
        }
        CliCommand::Pairing(PairingArgs { action, device_id }) => {
            hermes_cli::commands::handle_cli_pairing(action, device_id).await
        }
        CliCommand::Claw(ClawArgs { action }) => {
            hermes_cli::commands::handle_cli_claw(action).await
        }
        CliCommand::Acp(AcpArgs {
            action,
            check,
            setup,
            setup_browser,
            version,
            yes,
        }) => {
            let action = acp_action_from_flags(action, check, setup, setup_browser, version);
            if action.as_deref() == Some("setup") {
                let mut result = run_model(cli, None).await;
                if result.is_ok() {
                    if yes {
                        result = hermes_cli::commands::handle_cli_acp(
                            Some("setup-browser".to_string()),
                            true,
                        )
                        .await;
                    } else if std::io::stdin().is_terminal() {
                        print!("Set up ACP browser tools now? [y/N] ");
                        let _ = std::io::stdout().flush();
                        let mut answer = String::new();
                        if std::io::stdin().read_line(&mut answer).is_ok()
                            && acp_setup_browser_answer_is_yes(&answer)
                        {
                            result = hermes_cli::commands::handle_cli_acp(
                                Some("setup-browser".to_string()),
                                false,
                            )
                            .await;
                        }
                    }
                }
                result
            } else {
                hermes_cli::commands::handle_cli_acp(action, yes).await
            }
        }
        CliCommand::Backup(BackupArgs { output }) => {
            hermes_cli::commands::handle_cli_backup(output).await
        }
        CliCommand::Import(ImportArgs { path }) => {
            hermes_cli::commands::handle_cli_import(path).await
        }
        CliCommand::ImportHermesSessions(_) => unreachable!("offline import already handled"),
        CliCommand::Version => hermes_cli::commands::handle_cli_version(),
        CliCommand::Cron(CronArgs {
            action,
            job_id,
            id,
            schedule,
            prompt,
            name,
            deliver,
            deliver_chat_id,
            repeat,
            skills,
            add_skills,
            remove_skills,
            clear_skills,
            script,
            no_agent,
            agent,
            script_timeout_seconds,
            script_shell,
            workdir,
            all,
        }) => {
            run_cron(
                cli,
                action,
                job_id,
                id,
                schedule,
                prompt,
                name,
                deliver,
                deliver_chat_id,
                repeat,
                skills,
                add_skills,
                remove_skills,
                clear_skills,
                script,
                no_agent,
                agent,
                script_timeout_seconds,
                script_shell,
                workdir,
                all,
            )
            .await
        }
        CliCommand::Webhook(WebhookArgs {
            action,
            name,
            url,
            id,
            prompt,
            events,
            description,
            skills,
            deliver,
            deliver_chat_id,
            secret,
            deliver_only,
            payload,
        }) => {
            run_webhook(
                cli,
                action,
                name,
                url,
                id,
                prompt,
                events,
                description,
                skills,
                deliver,
                deliver_chat_id,
                secret,
                deliver_only,
                payload,
            )
            .await
        }
        CliCommand::Cloudflare(CloudflareArgs { action, selftest }) => {
            hermes_cli::cloudflare::handle_cli_cloudflare(action, selftest).await
        }
        CliCommand::TeamsPipeline(TeamsPipelineArgs {
            action,
            id,
            limit,
            status,
            store_path,
            meeting_id,
            join_web_url,
            tenant_id,
            call_record_id,
            resource,
            notification_url,
            change_type,
            expiration,
            client_state,
            lifecycle_notification_url,
            latest_supported_tls_version,
            force_refresh,
            renew_within_hours,
            extend_hours,
            dry_run,
        }) => {
            hermes_cli::teams_pipeline_cli::handle_cli_teams_pipeline(
                hermes_cli::teams_pipeline_cli::TeamsPipelineCliOptions {
                    config_dir: cli.config_dir.clone(),
                    action,
                    id,
                    limit,
                    status,
                    store_path,
                    meeting_id,
                    join_web_url,
                    tenant_id,
                    call_record_id,
                    resource,
                    notification_url,
                    change_type,
                    expiration,
                    client_state,
                    lifecycle_notification_url,
                    latest_supported_tls_version,
                    force_refresh,
                    renew_within_hours,
                    extend_hours,
                    dry_run,
                },
            )
            .await
        }
        CliCommand::Dump(DumpArgs { session, output }) => run_dump(cli, session, output).await,
        CliCommand::Completion(CompletionArgs { shell }) => run_completion(shell),
        CliCommand::Uninstall(UninstallArgs { yes }) => run_uninstall(yes).await,
        CliCommand::Lumio(LumioArgs { action, model }) => run_lumio(action, model).await,
        CliCommand::PluginExternal(raw) => {
            hermes_cli::commands::handle_cli_external_plugin_subcommand(raw).await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// Initialize the tracing subscriber with env filter.
fn init_tracing(verbose: bool, interactive_tui: bool) {
    let default = if interactive_tui {
        if verbose {
            "info"
        } else {
            "error"
        }
    } else if verbose {
        "debug"
    } else {
        "warn"
    };
    if interactive_tui
        && std::env::var("HERMES_TUI_ALLOW_STDERR_LOGS")
            .ok()
            .as_deref()
            != Some("1")
    {
        std::env::set_var("RUST_LOG", default);
    }
    init_telemetry_from_env("hermes-cli", default);
}
