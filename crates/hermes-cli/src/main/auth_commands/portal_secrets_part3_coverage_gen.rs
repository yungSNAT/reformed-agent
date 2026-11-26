#[tokio::test]
async fn gemini_oauth_add_and_login_save_pool_vault_and_auth_state() {
    const TEST: &str = "gemini_oauth_add_and_login_save_pool_vault_and_auth_state";
    const PROVIDER: &str = "google-gemini-cli";
    if !isolated(TEST) {
        return;
    }

    let home = isolated_home();
    let expires_at = jiff::Timestamp::now().as_millisecond() + 3_600_000;
    let refresh = format!("fixture-refresh-{}", std::process::id());
    for action in ["add", "login"] {
        let access = format!("fixture-{action}-{}", std::process::id());
        let creds = hermes_cli::auth::GeminiRuntimeCredentials {
            provider: PROVIDER.to_string(),
            base_url: "http://127.0.0.1".to_string(),
            api_key: access.clone(),
            source: "google-oauth".to_string(),
            expires_at_ms: Some(expires_at),
            auth_file: home.join("auth/google_oauth.json"),
            email: None,
            project_id: opt("fixture-project"),
            refresh_token: Some(refresh.clone()),
        };
        let cli = cli_in(&home).expect("cli");
        let pool_path = auth_pool_path_for_cli(&cli);
        run_auth_with_gemini_login(
            cli,
            opt(action),
            opt(PROVIDER),
            None,
            opt("oauth"),
            None,
            None,
            false,
            std::future::ready(Ok(creds)),
        )
        .await
        .expect(action);

        let credential = vault_credential(&home, PROVIDER)
            .await
            .expect("vault credential");
        assert_eq!(credential.access_token, access);
        assert_eq!(credential.refresh_token.as_deref(), Some(refresh.as_str()));
        assert_eq!(credential.token_type, "bearer");
        assert_eq!(
            credential.expires_at,
            parse_unix_millis_utc(Some(expires_at))
        );

        let state = read_provider_auth_state(PROVIDER)
            .expect("read auth store")
            .expect("gemini auth state");
        assert_eq!(state["access_token"], access.as_str());
        assert_eq!(state["refresh_token"], refresh.as_str());
        assert_eq!(state["expires_at_ms"], expires_at);
        assert_eq!(state["source"], "google-oauth");
        assert_eq!(state["project_id"], "fixture-project");
        assert!(state["email"].is_null());

        let pool = load_auth_pool_store(&pool_path).expect("load pool");
        let entries = &pool.providers[PROVIDER];
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "google-gemini-cli-1");
        assert_eq!(entries[0].auth_type, "oauth");
        assert_eq!(entries[0].source, "google-oauth");
        assert_eq!(
            entries[0].access_token,
            format!("fixture-add-{}", std::process::id())
        );
        assert_eq!(entries[0].id.len(), 6);
    }
}
