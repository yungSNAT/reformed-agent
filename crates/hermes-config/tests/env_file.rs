//! The `.env` helpers other crates call: one key's write or removal, the round-trip check and
//! the values start-up would load.

use hermes_config::loader::{dotenv_values_in, env_value_round_trips, set_env_file_value};

#[test]
fn a_variable_that_steers_the_next_process_is_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    let original = "PATH=/usr/bin\n";
    std::fs::write(&path, original).unwrap();
    for name in [
        "PATH",
        "LD_PRELOAD",
        "LD_ANYTHING",
        "DYLD_INSERT_LIBRARIES",
        "GIT_CONFIG_KEY_0",
        "NODE_OPTIONS",
        "PYTHONPATH",
        "HERMES_HOME",
        "HERMES_YOLO_MODE",
        "HERMES_MANAGED_DIR",
    ] {
        assert!(
            set_env_file_value(&path, name, Some("k")).is_err(),
            "{name}"
        );
        assert!(set_env_file_value(&path, name, None).is_err(), "{name}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original, "{name}");
    }
    // Windows reads `path` as `PATH`.
    assert_eq!(
        set_env_file_value(&path, "path", Some("k")).is_err(),
        cfg!(windows)
    );
    // A credential with a Hermes prefix is not one of them.
    assert!(set_env_file_value(&path, "HERMES_LANGFUSE_PUBLIC_KEY", Some("k")).unwrap());
}

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
    unsafe { std::env::set_var("HERMES_MANAGED_DIR", managed.path()) }; // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let values = dotenv_values_in(home.path());
    unsafe { std::env::remove_var("HERMES_MANAGED_DIR") }; // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    assert_eq!(values["ENV_FILE_TEST_PINNED"], "managed");
    assert_eq!(values["ENV_FILE_TEST_OWN"], "quoted");
    assert!(std::env::var_os("ENV_FILE_TEST_PINNED").is_none());
}

#[test]
fn a_env_file_that_cannot_be_read_is_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    // One byte that is not UTF-8, in a comment, beside other keys.
    let original = b"# caf\xe9\nOTHER_KEY=keep-me\n".to_vec();
    std::fs::write(&path, &original).unwrap();
    assert!(set_env_file_value(&path, "A", Some("k")).is_err());
    assert!(set_env_file_value(&path, "OTHER_KEY", None).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn a_key_that_is_not_a_variable_name_is_refused_and_nothing_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    let original = "OPENAI_API_KEY=keep-me\n";
    std::fs::write(&path, original).unwrap();
    assert!(env_value_round_trips("_a1", "k"));
    for name in [
        "X\nOPENAI_API_KEY",
        "X\rY",
        "X\u{0}Y",
        " VAR ",
        "A B",
        "A=B",
        "1A",
        "",
        "A}${B",
        "VAR:-x",
        "VÄR",
    ] {
        assert!(!env_value_round_trips(name, "k"), "{name:?}");
        assert!(
            set_env_file_value(&path, name, Some("k")).is_err(),
            "{name:?}"
        );
        assert!(set_env_file_value(&path, name, None).is_err(), "{name:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "{name:?}"
        );
    }
}
