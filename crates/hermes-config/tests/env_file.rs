//! The `.env` helpers other crates call: one key's write or removal, the round-trip check and
//! the values start-up would load.

use hermes_config::loader::{dotenv_values_in, env_value_round_trips, set_env_file_value};

#[test]
fn only_a_value_that_reads_back_as_itself_round_trips() {
    assert!(env_value_round_trips("K", "sk-abc_123.def/ghi+jkl="));
    for refused in [
        "a\nB_KEY=x",
        "a\rb",
        "a\u{0}b",
        "a\tb",
        " padded",
        "\"quoted\"",
        "'quoted'",
        "${OTHER}",
        "k OPENAI_BASE_URL=https://elsewhere.example/v1",
    ] {
        assert!(!env_value_round_trips("K", refused), "{refused:?}");
    }
}

#[test]
fn one_key_is_set_or_removed_and_the_file_kept_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    assert!(set_env_file_value(&path, "A", Some("1")).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a new file");
    }
    std::fs::write(&path, "# note\nA=1\nexport B=2\nA=3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(set_env_file_value(&path, "A", Some("new")).unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# note\nA=new\nexport B=2\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "an existing file");
    }
    assert!(set_env_file_value(&path, "B", None).unwrap());
    assert!(
        !set_env_file_value(&path, "B", None).unwrap(),
        "already gone"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# note\nA=new\n");

    let before = std::fs::read_to_string(&path).unwrap();
    assert!(set_env_file_value(&path, "A", Some("x\nOPENAI_BASE_URL=elsewhere")).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn the_values_start_up_loads_put_the_managed_env_last() {
    let home = tempfile::tempdir().unwrap();
    let managed = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".env"),
        "ENV_FILE_TEST_PINNED=home\nENV_FILE_TEST_OWN='quoted'\n",
    )
    .unwrap();
    std::fs::write(
        managed.path().join(".env"),
        "ENV_FILE_TEST_PINNED=managed\n",
    )
    .unwrap();
    // SAFETY: no other test in this binary reads the environment.
    unsafe { std::env::set_var("HERMES_MANAGED_DIR", managed.path()) };
    let values = dotenv_values_in(home.path());
    unsafe { std::env::remove_var("HERMES_MANAGED_DIR") };
    assert_eq!(values["ENV_FILE_TEST_PINNED"], "managed");
    assert_eq!(values["ENV_FILE_TEST_OWN"], "quoted");
    assert!(std::env::var_os("ENV_FILE_TEST_PINNED").is_none());
}
