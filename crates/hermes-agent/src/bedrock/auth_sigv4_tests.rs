use super::*;

#[test]
fn bedrock_auth_prefers_bearer_then_environment_then_selected_profile() {
    const CHILD: &str = "HERMES_BEDROCK_PROFILE_FIXTURE";
    const TEST: &str =
        "bedrock::auth_sigv4_tests::bedrock_auth_prefers_bearer_then_environment_then_selected_profile";
    if std::env::var_os(CHILD).is_none() || !std::env::args().any(|arg| arg == "--exact") {
        let home = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap(); // nosemgrep: rust.lang.security.current-exe.current-exe
        let mut child = std::process::Command::new(executable);
        child
            .env_clear()
            .env(CHILD, "1")
            .env("HOME", home.path())
            .current_dir(home.path())
            .args(["--exact", TEST, "--color", "never"]);
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            child.env("LLVM_PROFILE_FILE", profile);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&format!("test {TEST} ... ok")),
            "{output:?}"
        );
        return;
    }

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    for var in [
        "AWS_BEARER_TOKEN_BEDROCK",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
    ] {
        std::env::remove_var(var);
    }
    assert_eq!(resolve_bedrock_auth(), None);
    assert_eq!(resolve_bedrock_region(), BEDROCK_DEFAULT_REGION);

    let aws = home.path().join(".aws");
    std::fs::create_dir(&aws).unwrap();
    std::fs::write(
        aws.join("credentials"),
        "[default]\naws_access_key_id = incomplete\naws_secret_access_key = \n\
         [work]\n; fixture comment\nignored line\nAWS_ACCESS_KEY_ID = profile-access\n\
         aws_secret_access_key = profile-secret\naws_session_token = profile-session\n",
    )
    .unwrap();
    let config = home.path().join("regional-config");
    std::fs::write(
        &config,
        "[default]\nregion=us-west-2\n\n[profile work]\n# fixture region\n\
         region = eu-west-1 \n",
    )
    .unwrap();
    std::env::set_var("AWS_CONFIG_FILE", &config);
    assert_eq!(resolve_bedrock_auth(), None);
    assert_eq!(resolve_bedrock_region(), "us-west-2");

    std::env::set_var("AWS_DEFAULT_PROFILE", " work ");
    let profile_auth = BedrockAuth::SigV4(AwsCredentials {
        access_key_id: "profile-access".into(),
        secret_access_key: "profile-secret".into(),
        session_token: Some("profile-session".into()),
    });
    assert_eq!(resolve_bedrock_auth(), Some(profile_auth.clone()));
    assert_eq!(resolve_bedrock_region(), "eu-west-1");
    std::env::set_var("AWS_PROFILE", "default");
    assert_eq!(resolve_bedrock_auth(), None);
    std::env::set_var("AWS_PROFILE", "work");

    std::env::set_var("AWS_ACCESS_KEY_ID", " env-access ");
    assert_eq!(resolve_bedrock_auth(), Some(profile_auth));
    std::env::set_var("AWS_SECRET_ACCESS_KEY", " env-secret ");
    std::env::set_var("AWS_SESSION_TOKEN", " env-session ");
    assert_eq!(
        resolve_bedrock_auth(),
        Some(BedrockAuth::SigV4(AwsCredentials {
            access_key_id: "env-access".into(),
            secret_access_key: "env-secret".into(),
            session_token: Some("env-session".into()),
        }))
    );
    std::env::set_var("AWS_BEARER_TOKEN_BEDROCK", " fixture-bearer ");
    assert_eq!(
        resolve_bedrock_auth(),
        Some(BedrockAuth::Bearer("fixture-bearer".into()))
    );
    std::env::set_var("AWS_DEFAULT_REGION", " ap-southeast-2 ");
    assert_eq!(resolve_bedrock_region(), "ap-southeast-2");
    std::env::set_var("AWS_REGION", " ap-northeast-1 ");
    assert_eq!(resolve_bedrock_region(), "ap-northeast-1");
}
