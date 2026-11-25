async fn invoke_auth_fixture(args: &[&str]) -> Result<(), AgentError> {
    use clap::Parser;
    let cli = Cli::try_parse_from(
        ["hermes-ultra", "auth"]
            .into_iter()
            .chain(args.iter().copied()),
    )
    .expect("parse fixture auth command");
    let Some(CliCommand::Auth(AuthArgs {
        action,
        provider,
        target,
        auth_type,
        label,
        api_key,
        qr,
    })) = cli.command.clone()
    else {
        panic!("expected an auth command");
    };
    run_auth(cli, action, provider, target, auth_type, label, api_key, qr).await
}

#[tokio::test]
async fn auth_pool_reset_and_remove_keep_the_vault_in_sync() {
    if !crate::isolated_test_profile::enter(
        "tests::auth_pool_reset_and_remove_keep_the_vault_in_sync",
    ) {
        return;
    }
    let root = hermes_config::hermes_home();
    let pool_path = root.join("auth/pool.json");
    let vault_path = root.join("auth/tokens.json");
    let provider = "fixture-pool";
    let first = "fixture-first-value";
    let second = "fixture-second-value";
    invoke_auth_fixture(&["list", provider]).await.unwrap();
    assert!(matches!(
        invoke_auth_fixture(&["add", provider, "--api-key", " "]).await,
        Err(AgentError::Config(detail)) if detail == "auth add: empty credential"
    ));
    assert!(!pool_path.exists());
    invoke_auth_fixture(&["add", provider, "--type", "oauth", "--api-key", first])
        .await
        .unwrap();
    invoke_auth_fixture(&["add", provider, "--api-key", second, "--label", "second"])
        .await
        .unwrap();
    invoke_auth_fixture(&["add", "fixture-other", "--api-key", "fixture-other-value"])
        .await
        .unwrap();

    let mut pool = load_auth_pool_store(&pool_path).unwrap();
    let entries = pool.providers.get_mut(provider).unwrap();
    let [first_entry, second_entry] = entries.as_mut_slice() else {
        panic!("expected exactly 2 pooled fixture entries");
    };
    assert_eq!(first_entry.label, "fixture-pool-1");
    assert_eq!(first_entry.auth_type, "api_key");
    assert_eq!(first_entry.source, "manual");
    assert_eq!(second_entry.label, "second");
    let first_id = first_entry.id.clone();
    first_entry.last_status = Some("exhausted".into());
    first_entry.last_status_at = Some(1.0);
    first_entry.last_error_code = Some(429);
    save_auth_pool_store(&pool_path, &pool).unwrap();
    assert!(FileTokenStore::new(vault_path.clone())
        .await
        .unwrap()
        .get(provider)
        .await
        .is_some_and(|entry| entry.access_token == second));

    invoke_auth_fixture(&["list", provider]).await.unwrap();
    invoke_auth_fixture(&["list", "fixture-nonexistent"])
        .await
        .unwrap();
    invoke_auth_fixture(&["reset", "fixture-nonexistent"])
        .await
        .unwrap();
    invoke_auth_fixture(&["reset", provider]).await.unwrap();
    let saved = load_auth_pool_store(&pool_path).unwrap();
    let reset = &saved.providers[provider][0];
    assert_eq!(reset.id, first_id);
    assert!(reset.last_status.is_none());
    assert!(reset.last_status_at.is_none());
    assert!(reset.last_error_code.is_none());
    assert!(reset.access_token == first);
    assert!(FileTokenStore::new(vault_path.clone())
        .await
        .unwrap()
        .get(provider)
        .await
        .is_some_and(|entry| entry.access_token == second));

    for (args, expected) in [
        (&["remove", provider][..], "auth remove usage"),
        (
            &["remove", provider, "absent"][..],
            "Could not resolve auth remove target",
        ),
        (
            &["remove", "fixture-nonexistent", "1"][..],
            "No pooled credentials",
        ),
    ] {
        assert!(matches!(
            invoke_auth_fixture(args).await,
            Err(AgentError::Config(detail)) if detail.contains(expected)
        ));
    }
    invoke_auth_fixture(&["remove", provider, "second"])
        .await
        .unwrap();
    let saved = load_auth_pool_store(&pool_path).unwrap();
    let [remaining] = saved.providers[provider].as_slice() else {
        panic!("expected the first pooled credential to remain");
    };
    assert_eq!(remaining.id, first_id);
    assert!(FileTokenStore::new(vault_path.clone())
        .await
        .unwrap()
        .get(provider)
        .await
        .is_some_and(|entry| entry.access_token == first));

    invoke_auth_fixture(&["remove", provider, &first_id])
        .await
        .unwrap();
    let saved = load_auth_pool_store(&pool_path).unwrap();
    assert!(!saved.providers.contains_key(provider));
    assert_eq!(saved.providers["fixture-other"].len(), 1);
    let vault = FileTokenStore::new(vault_path).await.unwrap();
    assert!(vault.get(provider).await.is_none());
    assert!(vault.get("fixture-other").await.is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn auth_pool_imports_openai_from_the_isolated_fixture() {
    if !crate::isolated_test_profile::enter(
        "tests::auth_pool_imports_openai_from_the_isolated_fixture",
    ) {
        return;
    }
    let root = hermes_config::hermes_home();
    assert_eq!(dirs::home_dir().as_deref(), Some(root.as_path()));
    // Keep a regression in discovery from starting a live device flow.
    std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:0");
    std::env::set_var("ALL_PROXY", "http://127.0.0.1:0");
    let fixture = root.join("fixture-auth.json");
    let access = "fixture-local-access";
    let refresh = "fixture-local-refresh";
    std::fs::write(
        &fixture,
        serde_json::json!({
            "auth_mode":"chatgpt",
            "last_refresh":"2026-01-01T00:00:00Z",
            "tokens":{"access_token":access,"refresh_token":refresh}
        })
        .to_string(),
    )
    .unwrap();
    std::env::set_var("HERMES_OPENAI_OAUTH_FILE", &fixture);
    assert!(discover_existing_openai_oauth()
        .unwrap()
        .is_some_and(|entry| entry.source_path == fixture));
    assert!(discover_existing_openai_codex_oauth()
        .unwrap()
        .is_some_and(|entry| entry.source_path == fixture));
    let vault_path = root.join("auth/tokens.json");
    let pool_path = root.join("auth/pool.json");

    for provider in ["openai", "openai-codex"] {
        invoke_auth_fixture(&["login", provider]).await.unwrap();
        let vault = FileTokenStore::new(vault_path.clone()).await.unwrap();
        assert!(vault
            .get(provider)
            .await
            .is_some_and(|entry| entry.access_token == access
                && entry.refresh_token.as_deref() == Some(refresh)));
        assert!(read_provider_auth_state(provider)
            .unwrap()
            .is_some_and(|value| value["tokens"]["access_token"] == access
                && value["source"] == "discovered_external"));

        invoke_auth_fixture(&["add", provider]).await.unwrap();
        let pool = load_auth_pool_store(&pool_path).unwrap();
        let [entry] = pool.providers[provider].as_slice() else {
            panic!("expected one imported fixture credential for {provider}");
        };
        assert_eq!(entry.auth_type, "oauth");
        assert_eq!(entry.source, "discovered_external");
        assert!(entry.access_token == access);

        invoke_auth_fixture(&["remove", provider, "1"])
            .await
            .unwrap();
        assert!(!load_auth_pool_store(&pool_path)
            .unwrap()
            .providers
            .contains_key(provider));
        assert!(read_provider_auth_state(provider).unwrap().is_none());
        assert!(FileTokenStore::new(vault_path.clone())
            .await
            .unwrap()
            .get(provider)
            .await
            .is_none());
    }
}
