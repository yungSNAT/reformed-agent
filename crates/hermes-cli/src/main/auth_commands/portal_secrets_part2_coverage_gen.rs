// Login, logout and status paths that need a disposable profile: they read
// HOME and HERMES_HOME, so each runs in an isolated child process.

const PORTAL_MODULE: &str = "coverage_gen_main_auth_commands_portal_secrets";

fn isolated(test: &str) -> bool {
    crate::isolated_test_profile::enter(&format!("{PORTAL_MODULE}::{test}"))
}

fn isolated_home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("HERMES_HOME").expect("isolated HERMES_HOME"))
}

async fn vault_credential(dir: &Path, provider: &str) -> Option<OAuthCredential> {
    let cli = cli_in(dir).expect("cli");
    let store = FileTokenStore::new(secret_vault_path_for_cli(&cli))
        .await
        .expect("open vault");
    store.get(provider).await
}

async fn auth(dir: &Path, action: &str, provider: &str) -> Result<(), AgentError> {
    run_auth(
        cli_in(dir).expect("cli"),
        opt(action),
        opt(provider),
        None,
        None,
        None,
        None,
        false,
    )
    .await
}

#[tokio::test]
async fn anthropic_login_imports_the_existing_session_into_the_vault_and_auth_store() {
    const TEST: &str =
        "anthropic_login_imports_the_existing_session_into_the_vault_and_auth_store";
    if !isolated(TEST) {
        return;
    }
    let home = isolated_home();
    let access = format!("fixture-anthropic-access-{}", std::process::id());
    let refresh = format!("fixture-anthropic-refresh-{}", std::process::id());
    let expires_at = jiff::Timestamp::now().as_millisecond() + 3_600_000;
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("credentials folder");
    std::fs::write(
        claude_dir.join(".credentials.json"),
        serde_json::json!({
            "claudeAiOauth": {
                "accessToken": access,
                "refreshToken": refresh,
                "expiresAt": expires_at,
            }
        })
        .to_string(),
    )
    .expect("write existing session");

    auth(&home, "login", "anthropic")
        .await
        .expect("import the existing session");

    let state = read_provider_auth_state("anthropic")
        .expect("read auth store")
        .expect("anthropic auth state");
    assert_eq!(state["access_token"], access.as_str());
    assert_eq!(state["refresh_token"], refresh.as_str());
    assert_eq!(state["expires_at_ms"], expires_at);
    assert_eq!(state["source"], "claude_code_credentials_file");

    let credential = vault_credential(&home, "anthropic")
        .await
        .expect("vault credential");
    assert_eq!(credential.access_token, access);
    assert_eq!(credential.refresh_token.as_deref(), Some(refresh.as_str()));
    assert_eq!(credential.token_type, "bearer");
    assert_eq!(
        credential.expires_at,
        parse_unix_millis_utc(Some(expires_at))
    );
    assert!(credential.expires_at.is_some());
}

#[tokio::test]
async fn generic_login_saves_the_vault_secret_and_logout_removes_it() {
    const TEST: &str = "generic_login_saves_the_vault_secret_and_logout_removes_it";
    if !isolated(TEST) {
        return;
    }
    let home = isolated_home();
    let secret = format!("fixture-openrouter-{}", std::process::id());
    run_secrets(
        cli_in(&home).expect("cli"),
        opt("set"),
        opt("openrouter"),
        Some(secret.clone()),
        false,
    )
    .await
    .expect("seed the vault");
    let seeded = vault_credential(&home, "openrouter")
        .await
        .expect("seeded credential");

    // Login takes the key from the vault and stores it as a bearer credential.
    auth(&home, "login", "openrouter")
        .await
        .expect("login with the vault key");
    let credential = vault_credential(&home, "openrouter")
        .await
        .expect("login credential");
    assert_eq!(credential.access_token, secret);
    assert_eq!(credential.token_type, "bearer");
    assert!(credential.refresh_token.is_none());
    assert!(credential.expires_at.is_none());
    assert_eq!(credential.provider, seeded.provider);

    run_secrets(
        cli_in(&home).expect("cli"),
        opt("set"),
        opt("nous"),
        Some(format!("fixture-nous-{}", std::process::id())),
        false,
    )
    .await
    .expect("seed nous");

    // Logout removes the credential and leaves the other provider alone.
    auth(&home, "logout", "openrouter")
        .await
        .expect("logout");
    assert!(vault_credential(&home, "openrouter").await.is_none());
    assert!(vault_credential(&home, "nous").await.is_some());

    // A second logout of a missing credential still succeeds.
    auth(&home, "logout", "openrouter")
        .await
        .expect("repeat logout");
}

#[tokio::test]
async fn generic_login_without_any_key_reports_the_missing_key() {
    const TEST: &str = "generic_login_without_any_key_reports_the_missing_key";
    if !crate::isolated_test_profile::enter_with_input(&format!("{PORTAL_MODULE}::{TEST}"), b"\n")
    {
        return;
    }
    let home = isolated_home();
    let message = config_msg(auth(&home, "login", "openrouter").await);
    assert_eq!(message, "Missing API key for provider 'openrouter'");
    assert!(vault_credential(&home, "openrouter").await.is_none());
}
