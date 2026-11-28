#[test]
fn composer_drafts_keep_unreadable_or_invalid_data() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
    let drafts = app.composer_drafts_path();

    let invalid_utf8 = [0xff];
    std::fs::write(&drafts, invalid_utf8).unwrap();
    assert!(matches!(
        app.load_current_composer_draft(),
        Err(AgentError::Io(message)) if message.contains("Failed to read composer drafts")
    ));
    assert!(app.persist_current_composer_draft("replacement").is_err());
    assert_eq!(std::fs::read(&drafts).unwrap(), invalid_utf8);

    let truncated_json = b"{";
    std::fs::write(&drafts, truncated_json).unwrap();
    assert!(matches!(
        app.load_current_composer_draft(),
        Err(AgentError::Config(message)) if message.contains("Failed to parse composer drafts")
    ));
    assert!(app.clear_current_composer_draft().is_err());
    assert_eq!(std::fs::read(&drafts).unwrap(), truncated_json.as_slice());
}

#[test]
fn composer_drafts_keep_unsent_text_before_session_creation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
    app.session_id = " ".into();
    let drafts = app.composer_drafts_path();
    app.clear_current_composer_draft().unwrap();
    assert!(!drafts.exists());

    app.persist_current_composer_draft("unsent text").unwrap();
    app.session_id.clear();
    assert_eq!(
        app.load_current_composer_draft().unwrap().as_deref(),
        Some("unsent text")
    );
    app.clear_current_composer_draft().unwrap();
    assert!(!drafts.exists());
}
