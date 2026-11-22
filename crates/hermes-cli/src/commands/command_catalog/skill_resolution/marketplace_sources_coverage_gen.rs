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
