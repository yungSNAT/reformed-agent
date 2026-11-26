use super::*;

#[tokio::test]
async fn auth_manager_refreshes_an_expired_credential_and_persists_the_result() {
    let dir = tempdir().expect("isolated credential store");
    let path = dir.path().join("synthetic-credentials.json");
    let fixture_key = [17; TOKEN_STORE_KEY_BYTES];
    let store = FileTokenStore {
        path: path.clone(),
        key: Arc::new(fixture_key),
        cache: Arc::new(RwLock::new(HashMap::new())),
    };
    let manager = AuthManager::new(store);
    assert!(manager.get_access_token("absent").await.unwrap().is_none());

    let mut expired = sample_credential("refresh-fixture", "expired-placeholder");
    expired.expires_at = Some("2000-01-01T00:00:00Z".parse().unwrap());
    assert!(expired.is_expired(0));
    let mut without_refresh = expired.clone();
    without_refresh.provider = "no-refresh-fixture".into();
    without_refresh.refresh_token = None;
    manager.save_credential(without_refresh).await.unwrap();
    assert!(manager
        .get_access_token("no-refresh-fixture")
        .await
        .unwrap()
        .is_none());

    manager.save_credential(expired).await.unwrap();
    assert!(matches!(
        manager.get_access_token("refresh-fixture").await,
        Err(AgentError::AuthFailed(message)) if message.contains("no refresh handler")
    ));

    manager
        .register_refresh_handler(
            "refresh-fixture",
            Arc::new(|provider, refresh| {
                Box::pin(async move {
                    assert_eq!(provider, "refresh-fixture");
                    assert_eq!(refresh, "refresh-token");
                    Ok(sample_credential(&provider, "refreshed-placeholder"))
                })
            }),
        )
        .await;
    assert_eq!(
        manager
            .get_access_token("refresh-fixture")
            .await
            .unwrap()
            .as_deref(),
        Some("refreshed-placeholder")
    );
    let (persisted, migrated) = load_token_cache(&path, &fixture_key).await.unwrap();
    assert!(!migrated);
    let restored = &persisted["refresh-fixture"];
    assert_eq!(restored.access_token, "refreshed-placeholder");
    assert!(!restored.is_expired(30));
    assert_eq!(
        manager
            .get_access_token("refresh-fixture")
            .await
            .unwrap()
            .as_deref(),
        Some("refreshed-placeholder")
    );
}

#[tokio::test]
async fn stored_credentials_distinguish_empty_and_malformed_inputs() {
    let dir = tempdir().expect("isolated credential store");
    let path = dir.path().join("synthetic-credentials.json");
    let fixture_key = [5; TOKEN_STORE_KEY_BYTES];
    std::fs::write(&path, b" \n").unwrap();
    let (cache, migrated) = load_token_cache(&path, &fixture_key).await.unwrap();
    assert!(cache.is_empty());
    assert!(!migrated);

    std::fs::write(&path, b"{broken").unwrap();
    assert!(matches!(
        load_token_cache(&path, &fixture_key).await,
        Err(AgentError::Config(_))
    ));
    assert!(matches!(
        load_token_cache(dir.path(), &fixture_key).await,
        Err(AgentError::Io(_))
    ));

    let base = encrypt_token_cache(&fixture_key, &HashMap::new()).unwrap();
    let mut unknown_version = base.clone();
    unknown_version.version = TOKEN_STORE_ENVELOPE_VERSION + 1;
    let mut bad_nonce = base.clone();
    bad_nonce.nonce_b64 = "$".into();
    let mut short_nonce = base.clone();
    short_nonce.nonce_b64 = "AA==".into();
    let mut bad_payload = base.clone();
    bad_payload.ciphertext_b64 = "$".into();
    for (envelope, expected) in [
        (unknown_version, "unsupported token store envelope version"),
        (bad_nonce, "invalid token store nonce"),
        (short_nonce, "invalid token store nonce length"),
        (bad_payload, "invalid token store payload"),
    ] {
        std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(
            matches!(
                load_token_cache(&path, &fixture_key).await,
                Err(AgentError::Config(message)) if message.contains(expected)
            ),
            "{expected}"
        );
    }
    std::fs::write(&path, serde_json::to_vec(&base).unwrap()).unwrap();
    assert!(matches!(
        load_token_cache(&path, &[9; TOKEN_STORE_KEY_BYTES]).await,
        Err(AgentError::AuthFailed(message)) if message == "failed to decrypt token store"
    ));
    assert!(matches!(
        decode_store_key("AAAA"),
        Err(AgentError::Config(message)) if message.contains("must decode to exactly")
    ));
}
