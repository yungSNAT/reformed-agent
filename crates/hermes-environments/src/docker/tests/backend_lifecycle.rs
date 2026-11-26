use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use super::*;

const CHILD: &str = "HERMES_DOCKER_BACKEND_TEST_CHILD";

#[test]
fn docker_backend_recreates_the_container_and_round_trips_files() {
    if std::env::var_os(CHILD).is_none() {
        let fixture = tempfile::tempdir().unwrap();
        let docker = fixture.path().join("docker");
        std::fs::write(
            &docker,
            r#"#!/bin/sh
{
    printf 'call\n'
    printf '%s\n' "$@"
} >> "$HOME/docker-calls"
case "$1" in
    container)
        if [ "$2" != "inspect" ] || [ "$3" != "--format" ] || [ "$4" != "{{.State.Running}}" ] || [ "$5" != "--" ]; then
            exit 77
        fi
        if [ "$6" = "stale-container" ]; then
            printf 'Error: No such container: stale-container\n' >&2
            exit 1
        fi
        if [ "$6" != "fixture-container" ]; then exit 78; fi
        printf 'true\n'
        ;;
    run)
        printf 'fixture-container\n'
        ;;
    exec)
        shift
        if [ "$1" = "-i" ]; then shift; fi
        if [ "$1" = "-d" ]; then shift; fi
        if [ "$1" = "-it" ]; then shift; fi
        if [ "$1" = "-w" ]; then shift 2; fi
        if [ "$1" = "stale-container" ]; then
            printf 'No such container: stale-container\n' >&2
            exit 1
        fi
        if [ "$1" != "fixture-container" ]; then exit 75; fi
        shift
        if [ "$1" = "sh" ] && [ "$2" = "-c" ] && [ "$3" = "fixture-command" ]; then
            printf 'café\n'
            exit 0
        fi
        if [ "$1" = "sh" ] && [ "$2" = "-c" ] && [ "$3" = "ordinary-failure" ]; then
            printf 'A service is not running\n' >&2
            exit 2
        fi
        PATH=/usr/bin:/bin exec "$@"
        ;;
    *)
        exit 76
        ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("HOME", fixture.path())
            .env("PATH", fixture.path())
            .env(CHILD, "1")
            .current_dir(fixture.path())
            .args([
                "--exact",
                "docker::backend_lifecycle::docker_backend_recreates_the_container_and_round_trips_files",
                "--color",
                "never",
            ]);
        if let Some(path) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", path);
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "isolated Docker fixture exited {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(verify_docker_backend());
}

async fn verify_docker_backend() {
    let backend = DockerBackend::new(
        Some("stale-container".into()),
        Some("fixture-image".into()),
        false,
        false,
        None,
        None,
        None,
        true,
        None,
        Vec::new(),
        Vec::new(),
        5,
        4,
    );
    let result = backend
        .execute_command("fixture-command", None, Some("/fixture work"), true, true)
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "caf");
    let failure = backend
        .execute_command("ordinary-failure", None, None, false, false)
        .await
        .unwrap();
    assert_eq!(failure.exit_code, 2);
    assert_eq!(failure.stderr, "A se");
    let calls = std::fs::read_to_string("docker-calls").unwrap();
    assert_eq!(
        calls,
        concat!(
            "call\nexec\n-d\n-it\n-w\n/fixture work\nstale-container\nsh\n-c\nfixture-command\n",
            "call\ncontainer\ninspect\n--format\n{{.State.Running}}\n--\nstale-container\n",
            "call\nrun\n-d\n--tty\nfixture-image\ntail\n-f\n/dev/null\n",
            "call\nexec\n-d\n-it\n-w\n/fixture work\nfixture-container\nsh\n-c\nfixture-command\n",
            "call\nexec\nfixture-container\nsh\n-c\nordinary-failure\n",
            "call\ncontainer\ninspect\n--format\n{{.State.Running}}\n--\nfixture-container\n",
        )
    );

    let path = std::env::current_dir()
        .unwrap()
        .join("files with spaces/report's.txt");
    let path = path.to_str().unwrap();
    let contents = "first\nworker's café\nlast";
    assert!(!backend.file_exists(path).await.unwrap());
    backend.write_file(path, contents).await.unwrap();
    assert!(backend.file_exists(path).await.unwrap());
    assert_eq!(backend.read_file(path, None, None).await.unwrap(), contents);
    assert_eq!(
        backend.read_file(path, Some(1), Some(1)).await.unwrap(),
        "worker's café\n"
    );
    let remainder = backend.read_file(path, Some(1), None).await.unwrap();
    assert_eq!(remainder.trim_end_matches('\n'), "worker's café\nlast");
}
