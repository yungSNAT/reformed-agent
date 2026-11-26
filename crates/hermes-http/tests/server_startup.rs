use std::net::TcpListener;
use std::time::Duration;

async fn assert_startup_error(address: &str, expected: &str) {
    let sandbox = tempfile::tempdir().unwrap();
    let managed = sandbox.path().join("managed");
    std::fs::create_dir(&managed).unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_hermes-http"));
    command
        .env_clear()
        .env("HOME", sandbox.path())
        .env("USERPROFILE", sandbox.path())
        .env("HERMES_HOME", sandbox.path().join("hermes"))
        .env("HERMES_MANAGED_DIR", managed)
        .env("HERMES_HTTP_ADDR", address)
        .current_dir(sandbox.path())
        .kill_on_drop(true);
    for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }

    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("HTTP startup timed out")
        .expect("run HTTP server");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(expected), "{stderr}");
}

#[tokio::test]
async fn server_reports_a_port_that_is_already_bound() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let bind_error = TcpListener::bind(address).unwrap_err().to_string();
    assert_startup_error(&address.to_string(), &bind_error).await;
}

#[tokio::test]
async fn server_reports_an_invalid_listen_address() {
    assert_startup_error("not-a-socket-address", "invalid HERMES_HTTP_ADDR").await;
}
