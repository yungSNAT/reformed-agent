//! A terminal backend chosen in `config.yaml`, not only through the
//! environment, turns the stale-write guard off: its files are not on this
//! machine. Its own binary, as loading the config sets process environment.

use hermes_tools::terminal_requirements::non_local_terminal_backend;

#[test]
fn a_backend_chosen_in_config_is_not_local() {
    for key in [
        "TERMINAL_ENV",
        "TERMINAL_BACKEND",
        "HERMES_IGNORE_USER_CONFIG",
    ] {
        std::env::remove_var(key);
    }
    assert_eq!(non_local_terminal_backend(), None);

    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        "terminal:\n  backend: docker\n",
    )
    .unwrap();
    hermes_config::load_config(Some(home.path().to_str().unwrap())).unwrap();
    assert_eq!(non_local_terminal_backend().as_deref(), Some("docker"));
}
