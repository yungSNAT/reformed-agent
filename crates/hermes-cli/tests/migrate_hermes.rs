//! `hermes migrate` end to end on a fixture Hermes home with fake secrets.

use assert_cmd::Command;

const SECRET: &str = "fake-openrouter-key-0123456789";

fn migrate(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("hermes")
        .unwrap()
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("HERMES_HOME", root.join("unused-home"))
        .env_remove("HERMES_AGENT_ULTRA_HOME")
        .arg("migrate")
        .arg("--from")
        .arg(root.join("hermes"))
        .arg("--to")
        .arg(root.join("reformed"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn migrate_plans_then_applies_without_printing_secrets() {
    let root = tempfile::tempdir().unwrap();
    let hermes = root.path().join("hermes");
    std::fs::create_dir(&hermes).unwrap();
    std::fs::write(hermes.join(".env"), format!("OPENROUTER_API_KEY={SECRET}\n")).unwrap();
    std::fs::write(hermes.join("config.yaml"), format!("api_key: {SECRET}\n")).unwrap();

    let plan = migrate(root.path(), &[]);
    let stdout = String::from_utf8_lossy(&plan.stdout);
    assert!(plan.status.success(), "{stdout}{}", String::from_utf8_lossy(&plan.stderr));
    assert!(stdout.contains(".env: to copy"), "{stdout}");
    assert!(!root.path().join("reformed").exists());

    let applied = migrate(root.path(), &["--apply"]);
    let stdout = String::from_utf8_lossy(&applied.stdout);
    assert!(applied.status.success(), "{stdout}{}", String::from_utf8_lossy(&applied.stderr));
    assert!(stdout.contains(".env: copied"), "{stdout}");
    for output in [&plan, &applied] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    }
    assert_eq!(
        std::fs::read(root.path().join("reformed/.env")).unwrap(),
        std::fs::read(hermes.join(".env")).unwrap()
    );
    assert!(!root.path().join("unused-home").exists());

    let again = migrate(root.path(), &["--apply"]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("--into-existing"));
}
