use super::*;
use clap::Parser;
use std::path::Path;

fn opt(v: &str) -> Option<String> {
    Some(v.to_string())
}

/// Builds a `Cli` whose state root, secret vault and auth pool all live under `dir`.
/// Returns `None` if no supported flag redirects every path there; callers then
/// skip the test so the real home directory is never touched.
fn cli_in(dir: &Path) -> Option<Cli> {
    let d = dir.to_string_lossy().to_string();
    for flag in [
        "--config-dir",
        "--home",
        "--state-dir",
        "--hermes-home",
        "--data-dir",
    ] {
        if let Ok(cli) = Cli::try_parse_from(["hermes-ultra", flag, d.as_str()]) {
            if secret_vault_path_for_cli(&cli).starts_with(dir)
                && auth_pool_path_for_cli(&cli).starts_with(dir)
                && hermes_state_root(&cli).starts_with(dir)
            {
                return Some(cli);
            }
        }
    }
    None
}

fn config_msg(r: Result<(), AgentError>) -> String {
    match r {
        Err(AgentError::Config(m)) => m,
        Err(_) => panic!("expected a config error"),
        Ok(()) => panic!("expected an error, got Ok"),
    }
}

async fn vault_provider_count(dir: &Path) -> usize {
    let cli = cli_in(dir).expect("cli");
    let path = secret_vault_path_for_cli(&cli);
    let Ok(store) = FileTokenStore::new(&path).await else {
        panic!("open vault");
    };
    store.list_providers().await.len()
}

fn pool_entry_count(dir: &Path) -> usize {
    let cli = cli_in(dir).expect("cli");
    let Ok(pool) = load_auth_pool_store(&auth_pool_path_for_cli(&cli)) else {
        panic!("load pool");
    };
    pool.providers.values().map(|v| v.len()).sum::<usize>()
}

#[tokio::test]
async fn secrets_lifecycle_set_get_list_remove() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let Some(cli) = cli_in(dir) else {
        return;
    };

    // Empty vault listing works.
    assert!(run_secrets(cli, None, None, None, false).await.is_ok());
    assert_eq!(vault_provider_count(dir).await, 0);

    // Set a secret.
    let r = run_secrets(
        cli_in(dir).unwrap(),
        opt("set"),
        opt("openrouter"),
        opt("  sk-coverage-secret-value  "),
        false,
    )
    .await;
    assert!(r.is_ok());
    assert_eq!(vault_provider_count(dir).await, 1);

    // Non-empty listing via "status".
    assert!(
        run_secrets(cli_in(dir).unwrap(), opt("status"), None, None, false)
            .await
            .is_ok()
    );

    // Get (masked).
    assert!(run_secrets(
        cli_in(dir).unwrap(),
        opt("get"),
        opt("openrouter"),
        None,
        false
    )
    .await
    .is_ok());

    // Get missing provider.
    let msg = config_msg(
        run_secrets(
            cli_in(dir).unwrap(),
            opt("get"),
            opt("nonexistent-coverage-provider"),
            None,
            false,
        )
        .await,
    );
    assert!(msg.contains("No secret stored"));

    // Remove existing.
    assert!(run_secrets(
        cli_in(dir).unwrap(),
        opt("remove"),
        opt("openrouter"),
        None,
        false
    )
    .await
    .is_ok());
    assert_eq!(vault_provider_count(dir).await, 0);

    // Remove again: nothing found but still Ok.
    assert!(run_secrets(
        cli_in(dir).unwrap(),
        opt("rm"),
        opt("openrouter"),
        None,
        false
    )
    .await
    .is_ok());
    assert_eq!(vault_provider_count(dir).await, 0);
}

#[tokio::test]
async fn secrets_error_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    if cli_in(dir).is_none() {
        return;
    }

    let msg = config_msg(
        run_secrets(
            cli_in(dir).unwrap(),
            opt("set"),
            opt("openrouter"),
            opt("    "),
            false,
        )
        .await,
    );
    assert!(msg.contains("Secret cannot be empty"));
    assert_eq!(vault_provider_count(dir).await, 0);

    let msg = config_msg(run_secrets(cli_in(dir).unwrap(), opt("set"), None, None, false).await);
    assert!(msg.contains("secrets set"));

    let msg = config_msg(run_secrets(cli_in(dir).unwrap(), opt("get"), None, None, false).await);
    assert!(msg.contains("secrets get"));

    let msg = config_msg(run_secrets(cli_in(dir).unwrap(), opt("delete"), None, None, false).await);
    assert!(msg.contains("secrets remove"));

    let msg =
        config_msg(run_secrets(cli_in(dir).unwrap(), opt("bogus-action"), None, None, false).await);
    assert!(msg.contains("Unknown secrets action: bogus-action"));
}

#[tokio::test]
async fn auth_pool_add_list_reset_remove() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    if cli_in(dir).is_none() {
        return;
    }

    // Empty list.
    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("list"),
        opt("openrouter"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());
    assert_eq!(pool_entry_count(dir), 0);

    // Empty credential is rejected.
    let msg = config_msg(
        run_auth(
            cli_in(dir).unwrap(),
            opt("add"),
            opt("openrouter"),
            None,
            opt("api_key"),
            None,
            opt("   "),
            false,
        )
        .await,
    );
    assert!(msg.contains("empty credential"));
    assert_eq!(pool_entry_count(dir), 0);

    // Add two credentials.
    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("add"),
        opt("openrouter"),
        None,
        opt("api_key"),
        opt("first"),
        opt("  sk-pool-one  "),
        false
    )
    .await
    .is_ok());
    assert_eq!(pool_entry_count(dir), 1);
    {
        let cli = cli_in(dir).unwrap();
        let Ok(pool) = load_auth_pool_store(&auth_pool_path_for_cli(&cli)) else {
            panic!("load pool");
        };
        let entry = &pool.providers.values().next().expect("provider")[0];
        assert_eq!(entry.access_token, concat!("sk-", "pool-one"));
        assert_eq!(entry.label, "first");
        assert_eq!(entry.source, "manual");
    }

    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("add"),
        opt("openrouter"),
        None,
        opt("api_key"),
        None,
        opt(concat!("sk-", "pool-two")),
        false
    )
    .await
    .is_ok());
    assert_eq!(pool_entry_count(dir), 2);

    // Mark first entry exhausted so list/reset have work to do.
    {
        let cli = cli_in(dir).unwrap();
        let pool_path = auth_pool_path_for_cli(&cli);
        let Ok(mut pool) = load_auth_pool_store(&pool_path) else {
            panic!("load pool");
        };
        if let Some(entries) = pool.providers.values_mut().next() {
            entries[0].last_status = Some("exhausted".to_string());
        }
        assert!(save_auth_pool_store(&pool_path, &pool).is_ok());
    }

    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("list"),
        opt("openrouter"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());

    // List for a provider with no entries.
    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("list"),
        opt("coverage-unknown-provider"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());

    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("reset"),
        opt("openrouter"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());
    {
        let cli = cli_in(dir).unwrap();
        let Ok(pool) = load_auth_pool_store(&auth_pool_path_for_cli(&cli)) else {
            panic!("load pool");
        };
        for entries in pool.providers.values() {
            for e in entries {
                assert!(e.last_status.is_none());
                assert!(e.last_status_at.is_none());
            }
        }
    }

    // Reset for a provider without entries.
    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("reset"),
        opt("coverage-unknown-provider"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());

    // Remove error paths.
    let msg = config_msg(
        run_auth(
            cli_in(dir).unwrap(),
            opt("remove"),
            opt("openrouter"),
            None,
            None,
            None,
            None,
            false,
        )
        .await,
    );
    assert!(msg.contains("auth remove usage"));

    let msg = config_msg(
        run_auth(
            cli_in(dir).unwrap(),
            opt("remove"),
            opt("coverage-unknown-provider"),
            opt("1"),
            None,
            None,
            None,
            false,
        )
        .await,
    );
    assert!(msg.contains("No pooled credentials"));

    let msg = config_msg(
        run_auth(
            cli_in(dir).unwrap(),
            opt("remove"),
            opt("openrouter"),
            opt("zzz-no-such-target"),
            None,
            None,
            None,
            false,
        )
        .await,
    );
    assert!(msg.contains("Could not resolve auth remove target"));
    assert_eq!(pool_entry_count(dir), 2);

    // Remove one, then the last.
    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("remove"),
        opt("openrouter"),
        opt("1"),
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());
    assert_eq!(pool_entry_count(dir), 1);

    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("remove"),
        opt("openrouter"),
        opt("1"),
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());
    assert_eq!(pool_entry_count(dir), 0);
}

#[tokio::test]
async fn auth_status_and_platform_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    if cli_in(dir).is_none() {
        return;
    }

    // Generic provider status.
    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("status"),
        opt("openrouter"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());

    // Telegram status (read-only).
    assert!(run_auth(
        cli_in(dir).unwrap(),
        None,
        opt("telegram"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());

    // Gateway platform status (read-only).
    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("status"),
        opt("discord"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());

    // Telegram logout writes the config file under the temp root.
    let cfg_path = hermes_state_root(&cli_in(dir).unwrap()).join("config.yaml");
    assert!(run_auth(
        cli_in(dir).unwrap(),
        opt("logout"),
        opt("telegram"),
        None,
        None,
        None,
        None,
        false
    )
    .await
    .is_ok());
    assert!(cfg_path.exists());
}
