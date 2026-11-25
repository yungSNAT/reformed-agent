#![cfg(all(unix, feature = "singularity"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hermes_core::AgentError;
use hermes_environments::{singularity::SingularityBackend, TerminalBackend};

fn fixture_backend(root: &Path, output_limit: usize) -> SingularityBackend {
    let image = root.join("fixture.sif");
    fs::write(&image, []).unwrap();
    let binary = root.join("fixture-apptainer");
    fs::write(
        &binary,
        r#"#!/bin/sh
set -eu
[ "$1" = exec ] && [ "$2" = --bind ] && [ "$3" = /reference:/reference:ro ] || exit 64
shift 3
if [ "$1" = --pwd ]; then
    cd "$2"
    shift 2
fi
[ -f "$1" ] && [ "${1##*/}" = fixture.sif ] && [ "$2" = sh ] && [ "$3" = -c ] || exit 65
PATH=/usr/bin:/bin exec /bin/sh -c "$4"
"#,
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    SingularityBackend::new(
        Some(image.to_str().unwrap().to_owned()),
        Some(binary.to_str().unwrap().to_owned()),
        5,
        output_limit,
    )
    .with_bind("/reference:/reference:ro")
}

#[tokio::test]
async fn singularity_routes_commands_and_files_through_the_configured_runtime() {
    let root = tempfile::tempdir().unwrap();
    let backend = fixture_backend(root.path(), 4);
    verify_exec(&backend, root.path()).await;
    verify_files(&backend, root.path()).await;
}

async fn verify_exec(backend: &SingularityBackend, root: &Path) {
    fs::write(root.join("marker"), []).unwrap();
    let output = backend
        .execute_command(
            "test -f marker || exit 20; printf 'été'; printf 'note' >&2; exit 7",
            None,
            root.to_str(),
            true,
            false,
        )
        .await
        .unwrap();
    assert_eq!(output.exit_code, 7);
    assert_eq!(output.stdout, "ét");
    assert_eq!(output.stderr, "note");
}

async fn verify_files(backend: &SingularityBackend, root: &Path) {
    let file = root.join("survey notes").join("records.txt");
    let file = file.to_str().unwrap();
    assert!(!backend.file_exists(file).await.unwrap());
    assert!(matches!(
        backend.read_file(file, None, None).await,
        Err(AgentError::Io(message)) if message.contains("Failed to read file")
    ));
    backend
        .write_file(file, "first\nsecond\nthird")
        .await
        .unwrap();
    assert!(backend.file_exists(file).await.unwrap());
    assert_eq!(
        backend.read_file(file, None, None).await.unwrap(),
        "first\nsecond\nthird\n"
    );
    assert_eq!(
        backend.read_file(file, Some(1), Some(1)).await.unwrap(),
        "second\n"
    );
}
