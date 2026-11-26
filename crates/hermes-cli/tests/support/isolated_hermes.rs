pub(crate) fn isolated_hermes(sandbox: &std::path::Path) -> tokio::process::Command {
    let managed = sandbox.join("managed");
    std::fs::create_dir_all(&managed).expect("empty managed configuration");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_hermes-agent-ultra"));
    command
        .env_clear()
        .env("HOME", sandbox)
        .env("USERPROFILE", sandbox)
        .env("HERMES_HOME", sandbox.join("hermes"))
        .env("HERMES_MANAGED_DIR", managed)
        .current_dir(sandbox)
        .kill_on_drop(true);
    for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}
