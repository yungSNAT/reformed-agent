use super::*;

fn config_message(err: AgentError) -> String {
    match err {
        AgentError::Config(msg) => msg,
        other => panic!("expected AgentError::Config, got {:?}", other),
    }
}

#[test]
fn official_skill_path_candidates_empty_input_returns_empty() {
    assert!(official_skill_path_candidates("").is_empty());
    assert!(official_skill_path_candidates("  //  ").is_empty());
}

#[test]
fn official_skill_path_candidates_prefixed_and_plain() {
    assert_eq!(
        official_skill_path_candidates("/skills/security/1password/"),
        vec!["skills/security/1password".to_string()]
    );
    assert_eq!(
        official_skill_path_candidates("optional-skills/foo"),
        vec!["optional-skills/foo".to_string()]
    );
    assert_eq!(
        official_skill_path_candidates("security/1password"),
        vec![
            "skills/security/1password".to_string(),
            "optional-skills/security/1password".to_string()
        ]
    );
}

#[test]
fn parse_repo_skill_identifier_valid_with_github_prefix() {
    assert_eq!(
        parse_repo_skill_identifier("  github/owner/repo/skills/foo "),
        Some(("owner/repo".to_string(), "skills/foo".to_string()))
    );
}

#[test]
fn parse_repo_skill_identifier_filters_empty_segments() {
    assert_eq!(
        parse_repo_skill_identifier("owner//repo//x/"),
        Some(("owner/repo".to_string(), "x".to_string()))
    );
}

#[test]
fn parse_repo_skill_identifier_too_short_returns_none() {
    assert_eq!(parse_repo_skill_identifier("owner/repo"), None);
    assert_eq!(parse_repo_skill_identifier("github/owner/repo/"), None);
    assert_eq!(parse_repo_skill_identifier(""), None);
}

#[test]
fn canonicalize_skills_sh_identifier_strips_prefixes() {
    assert_eq!(
        canonicalize_skills_sh_identifier("  skills.sh/owner/repo/x "),
        "owner/repo/x"
    );
    assert_eq!(canonicalize_skills_sh_identifier("skills-sh/a"), "a");
    assert_eq!(
        canonicalize_skills_sh_identifier("skills.sh/skills-sh/a"),
        "a"
    );
    assert_eq!(canonicalize_skills_sh_identifier("plain/id"), "plain/id");
}

#[tokio::test]
async fn resolve_official_skill_source_empty_identifier_errors() {
    let client = reqwest::Client::new();
    let err = match resolve_official_skill_source(&client, "  //  ").await {
        Ok(_) => panic!("expected error for empty identifier"),
        Err(e) => e,
    };
    let msg = config_message(err);
    assert!(msg.contains("Missing official skill identifier"), "{}", msg);
}

#[tokio::test]
async fn resolve_official_skill_source_blank_after_prefix_errors() {
    let client = reqwest::Client::new();
    let err = match resolve_official_skill_source(&client, "official/ /").await {
        Ok(_) => panic!("expected error for blank identifier after prefix"),
        Err(e) => e,
    };
    let msg = config_message(err);
    assert!(msg.contains("Missing official skill identifier"), "{}", msg);
}

#[tokio::test]
async fn resolve_skills_sh_source_empty_identifier_errors() {
    let client = reqwest::Client::new();
    let err = match resolve_skills_sh_source(&client, " / ").await {
        Ok(_) => panic!("expected error for empty skills.sh identifier"),
        Err(e) => e,
    };
    let msg = config_message(err);
    assert_eq!(msg, "Missing skills.sh skill identifier.");
}

#[tokio::test]
async fn skills_sh_search_filters_and_preserves_the_first_result_for_each_identifier() {
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    assert!(server.address().ip().is_loopback());
    let search_url = format!("{}/api/search", server.uri());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("q", "bird survey"))
        .and(query_param("limit", "50"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "skills": [
                {"id": " ", "name": "Missing identifier"},
                {"id": " field/notes/birds ", "name": " Bird notes "},
                {"id": "field/notes/birds", "name": "Duplicate"},
                {"id": "field/notes/weather", "name": " "},
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let hits = search_skills_sh_registry(&client, &search_url, "bird survey", 99)
        .await
        .unwrap();
    assert_eq!(
        hits,
        [
            (
                "Bird notes".to_owned(),
                "skills.sh/field/notes/birds".to_owned()
            ),
            (
                "field/notes/weather".to_owned(),
                "skills.sh/field/notes/weather".to_owned()
            )
        ]
    );
}

#[tokio::test]
async fn skills_sh_search_reports_http_and_payload_errors() {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    assert!(server.address().ip().is_loopback());
    let search_url = format!("{}/api/search", server.uri());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for (query, response, expected) in [
        (
            "offline",
            ResponseTemplate::new(503).set_body_string("catalogue unavailable"),
            "skills.sh search failed (503 Service Unavailable): catalogue unavailable",
        ),
        (
            "malformed",
            ResponseTemplate::new(200).set_body_string("invalid JSON"),
            "Invalid skills.sh payload:",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/search"))
            .and(query_param("q", query))
            .and(query_param("limit", "1"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let error = search_skills_sh_registry(&client, &search_url, query, 0)
            .await
            .unwrap_err();
        assert!(config_message(error).starts_with(expected));
    }
}
