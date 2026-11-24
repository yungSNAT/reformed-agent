#[path = "support/isolated_hermes.rs"]
mod isolated_hermes;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

async fn credential_cli(home: &Path, args: &[&str], success: bool, expected: &str) -> String {
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        isolated_hermes::isolated_hermes(home)
            .stdin(Stdio::null())
            .args(args)
            .output(),
    )
    .await
    .expect("credential CLI timeout")
    .expect("run credential CLI");
    let stdout = String::from_utf8(result.stdout).expect("UTF-8 stdout");
    let stderr = String::from_utf8(result.stderr).expect("UTF-8 stderr");
    assert_eq!(
        result.status.success(),
        success,
        "{args:?}\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(expected) || stderr.contains(expected),
        "{args:?}\n{stdout}\n{stderr}",
    );
    format!("{stdout}\n{stderr}")
}

#[tokio::test]
async fn auth_pool_adds_lists_and_removes_synthetic_credentials() {
    const PROVIDER: &str = "fixture-provider";
    const FIRST: &str = "placeholder-first-value";
    const SECOND: &str = "placeholder-second-value";
    let sandbox = tempfile::tempdir().expect("isolated CLI profile");
    let home = sandbox.path();
    credential_cli(
        home,
        &["auth", "list", PROVIDER],
        true,
        "No pooled credentials configured.",
    )
    .await;
    credential_cli(
        home,
        &["auth", "remove", PROVIDER],
        false,
        "auth remove usage",
    )
    .await;
    credential_cli(
        home,
        &["auth", "remove", PROVIDER, "1"],
        false,
        "No pooled credentials for provider",
    )
    .await;

    for (label, fixture) in [("primary", FIRST), ("secondary", SECOND)] {
        let output = credential_cli(
            home,
            &[
                "auth",
                "add",
                PROVIDER,
                "--type",
                "api-key",
                "--label",
                label,
                "--api-key",
                fixture,
            ],
            true,
            &format!("label='{label}'"),
        )
        .await;
        assert!(!output.contains(fixture));
    }

    let entries = credential_cli(
        home,
        &["auth", "list", PROVIDER],
        true,
        "fixture-provider (2 credentials)",
    )
    .await;
    assert!(entries.contains("primary"), "{entries}");
    assert!(entries.contains("secondary"), "{entries}");
    assert!(!entries.contains(FIRST) && !entries.contains(SECOND));
    credential_cli(
        home,
        &["auth", "list", "other-fixture"],
        true,
        "No pooled credentials for provider",
    )
    .await;
    credential_cli(
        home,
        &["auth", "remove", PROVIDER, "9"],
        false,
        "Could not resolve auth remove target",
    )
    .await;
    credential_cli(
        home,
        &["auth", "remove", PROVIDER, "secondary"],
        true,
        "label='secondary'",
    )
    .await;
    let remaining = credential_cli(
        home,
        &["auth", "list", PROVIDER],
        true,
        "fixture-provider (1 credentials)",
    )
    .await;
    assert!(remaining.contains("primary"), "{remaining}");
    assert!(!remaining.contains("secondary"), "{remaining}");
    credential_cli(
        home,
        &["auth", "reset", PROVIDER],
        true,
        "Reset status on 0 pooled credential(s)",
    )
    .await;
    credential_cli(
        home,
        &["auth", "remove", PROVIDER, "1"],
        true,
        "label='primary'",
    )
    .await;
    credential_cli(
        home,
        &["auth", "list", PROVIDER],
        true,
        "No pooled credentials configured.",
    )
    .await;
}

#[tokio::test]
async fn secret_vault_masks_and_removes_the_isolated_fixture() {
    const PROVIDER: &str = "fixture-provider";
    const VALUE: &str = "fixture-value";
    let sandbox = tempfile::tempdir().expect("isolated CLI profile");
    let home = sandbox.path();
    credential_cli(home, &["secrets", "list"], true, "(empty)").await;
    credential_cli(
        home,
        &["secrets", "set", PROVIDER, "--value", " "],
        false,
        "Secret cannot be empty",
    )
    .await;
    credential_cli(
        home,
        &["secrets", "get", PROVIDER],
        false,
        "No secret stored",
    )
    .await;
    let added = credential_cli(
        home,
        &["secrets", "set", PROVIDER, "--value", VALUE],
        true,
        "Saved secret for provider 'fixture-provider'",
    )
    .await;
    assert!(!added.contains(VALUE));

    let listed = credential_cli(home, &["secrets", "list"], true, "Stored providers (1)").await;
    assert!(listed.contains(PROVIDER), "{listed}");
    assert!(!listed.contains(VALUE));
    let masked = credential_cli(home, &["secrets", "get", PROVIDER], true, "********").await;
    assert!(!masked.contains(VALUE));
    let refused = credential_cli(
        home,
        &["secrets", "get", PROVIDER, "--show"],
        false,
        "Refusing plaintext secret output",
    )
    .await;
    assert!(!refused.contains(VALUE));
    credential_cli(
        home,
        &["secrets", "remove", PROVIDER],
        true,
        "Removed secret for provider",
    )
    .await;
    credential_cli(
        home,
        &["secrets", "remove", PROVIDER],
        true,
        "No secret found for provider",
    )
    .await;
    credential_cli(home, &["secrets", "list"], true, "(empty)").await;
}
