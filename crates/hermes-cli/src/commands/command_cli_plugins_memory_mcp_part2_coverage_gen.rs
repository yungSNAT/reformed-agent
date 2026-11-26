#[test]
fn memory_filesystem_failures_are_reported() {
    let _stdout = run_isolated("memory-errors");
}

async fn exercise_memory_errors(home: &Path) {
    fs::write(home, "not a directory").unwrap();
    assert_io_error(memory("off", None, Default::default()).await, "");
    assert_io_error(memory("setup", None, Default::default()).await, "");
    assert_io_error(memory("reset", None, confirmed()).await, "");
    assert_eq!(fs::read_to_string(home).unwrap(), "not a directory");

    fs::remove_file(home).unwrap();
    let memories = home.join("memories");
    let marker = home.join(".memory_disabled");
    fs::create_dir_all(&memories).unwrap();
    fs::create_dir(&marker).unwrap();
    assert_io_error(memory("off", None, Default::default()).await, "");
    assert!(marker.is_dir());

    for (target, file) in [("memory", "MEMORY.md"), ("user", "USER.md")] {
        let obstruction = memories.join(file);
        fs::create_dir(&obstruction).unwrap();
        assert_io_error(memory("reset", Some(target), confirmed()).await, "");
        assert!(obstruction.is_dir());
        fs::remove_dir(obstruction).unwrap();
    }
}

#[cfg(unix)]
fn mock_git(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let git = root.join("bin").join("git");
    // The worker's PATH contains only this directory. No real Git or remotes
    // are involved, including when this mock is removed to exercise spawn errors.
    fs::write(
        &git,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$HERMES_COVERAGE_GIT_LOG"
case "$1" in
    clone)
        [ "$#" -eq 5 ] && [ "$2" = --depth ] && [ "$3" = 1 ] || exit 2
        target=$5
        case "${target##*/}" in
            *-clone-fail) printf 'simulated clone rejection\n' >&2; exit 1 ;;
        esac
        /bin/mkdir "$target" || exit 1
        case "${target##*/}" in
            *-missing-manifest) ;;
            *-bad-manifest) printf '\377\n' > "$target/plugin.yaml" ;;
            *) printf 'name: Mock fixture\nversion: "4.5.6"\ndescription: Offline clone\n' > "$target/plugin.yaml" ;;
        esac
        ;;
    -C)
        cd "$2" || exit 2
        repo=${PWD##*/}
        case "$3" in
            rev-parse)
                [ "$#" -eq 4 ] && [ "$4" = HEAD ] || exit 2
                case "$repo" in
                    coverage-bad-head) printf 'no initial revision\n' >&2; exit 1 ;;
                    coverage-bad-final)
                        if [ -f .mock-pulled ]; then
                            printf 'no final revision\n' >&2
                            exit 1
                        fi
                        ;;
                esac
                if [ "$repo" = coverage-changed ] && [ -f .mock-pulled ]; then
                    printf 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n'
                else
                    printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n'
                fi
                ;;
            pull)
                [ "$#" -eq 4 ] && [ "$4" = --ff-only ] || exit 2
                if [ "$repo" = coverage-pull-fail ]; then
                    printf 'simulated pull rejection\n' >&2
                    exit 1
                fi
                printf 'pulled\n' > .mock-pulled
                ;;
            *) printf 'simulated checkout rejection\n' >&2; exit 1 ;;
        esac
        ;;
    *) printf 'simulated checkout rejection\n' >&2; exit 1 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
    git
}

#[cfg(unix)]
#[test]
fn plugin_git_installs_short_circuit_or_use_the_mock_clone() {
    let stdout = run_isolated("plugins-install-git");
    assert_output(
        &stdout,
        &[
            "Git host is not on the default allow-list",
            "Plugin 'coverage-existing' is already installed",
            "Cloning from GitHub: https://github.com/example/coverage-short-ok.git",
            "git clone failed: simulated clone rejection",
            "No plugin.yaml found in cloned repository.",
            "Plugin installed successfully!",
            "Name:        Mock fixture",
            "Version:     4.5.6",
            "Description: Offline clone",
            "Name:        coverage-short-bad-manifest",
            "Version:     unknown",
        ],
    );
    assert_eq!(stdout.matches("Plugin installed successfully!").count(), 3);
    assert_eq!(stdout.matches("No plugin.yaml found").count(), 2);
    assert_eq!(stdout.matches("simulated clone rejection").count(), 2);
}

#[cfg(unix)]
async fn exercise_git_installs(root: &Path, home: &Path) {
    let git = mock_git(root);
    let log = root.join("git.log");
    let plugin_root = home.join("plugins");

    plugins(
        "install",
        Some("https://git-host.example.invalid/coverage-blocked.git"),
    )
    .await
    .unwrap();
    assert!(!plugin_root.exists());

    fs::create_dir_all(home).unwrap();
    fs::write(&plugin_root, "obstruction").unwrap();
    for source in [
        "https://github.com/example/coverage-obstructed.git",
        "gh:example/coverage-obstructed",
    ] {
        assert_io_error(
            plugins("install", Some(source)).await,
            "Failed to create plugins dir:",
        );
    }
    assert_eq!(fs::read_to_string(&plugin_root).unwrap(), "obstruction");
    fs::remove_file(&plugin_root).unwrap();

    let existing = plugin_root.join("coverage-existing");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join("keep"), "original").unwrap();
    for source in [
        "coverage-existing",
        "gh:example/coverage-existing",
        "https://github.com/example/coverage-existing.git/",
        "git@github.com:coverage-existing.git",
    ] {
        handle_cli_plugins(Some("install".into()), Some(source.into()), None, true)
            .await
            .unwrap();
    }
    assert!(!log.exists());
    assert_eq!(
        fs::read_to_string(existing.join("keep")).unwrap(),
        "original"
    );

    for (source, repo) in [
        (
            "https://github.com/example/coverage-url-ok.git",
            "coverage-url-ok",
        ),
        ("gh:example/coverage-short-ok", "coverage-short-ok"),
    ] {
        plugins("install", Some(source)).await.unwrap();
        let manifest = fs::read_to_string(plugin_root.join(repo).join("plugin.yaml")).unwrap();
        assert!(manifest.contains("description: Offline clone"));
    }

    for prefix in ["url", "short"] {
        for failure in ["clone-fail", "missing-manifest", "checkout-fail"] {
            let repo = format!("coverage-{prefix}-{failure}");
            let source = if prefix == "url" {
                format!("https://github.com/example/{repo}.git")
            } else {
                format!("gh:example/{repo}")
            };
            let git_ref = (failure == "checkout-fail").then(|| "coverage-missing-ref".to_owned());
            handle_cli_plugins(Some("install".into()), Some(source), git_ref, false)
                .await
                .unwrap();
            assert!(!plugin_root.join(repo).exists());
        }
    }

    assert_io_error(
        plugins(
            "install",
            Some("https://github.com/example/coverage-url-bad-manifest.git"),
        )
        .await,
        "Read error:",
    );
    plugins("install", Some("example/coverage-short-bad-manifest"))
        .await
        .unwrap();
    assert_eq!(
        fs::read(plugin_root.join("coverage-short-bad-manifest/plugin.yaml")).unwrap(),
        [0xff, b'\n']
    );

    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("clone --depth 1 https://github.com/example/coverage-url-ok.git "));
    assert!(calls.contains("clone --depth 1 https://github.com/example/coverage-short-ok.git "));

    fs::remove_file(git).unwrap();
    for source in [
        "https://github.com/example/coverage-spawn-error.git",
        "gh:example/coverage-spawn-error",
    ] {
        assert_io_error(plugins("install", Some(source)).await, "git clone failed:");
    }
    assert!(!plugin_root.join("coverage-spawn-error").exists());
}

#[cfg(unix)]
#[test]
fn plugin_git_update_distinguishes_changed_unchanged_and_failed_revisions() {
    let stdout = run_isolated("plugins-update-git");
    assert_output(
        &stdout,
        &[
            "Skipped: cannot read current revision (no initial revision)",
            "Update failed: simulated pull rejection",
            "Updated but could not read final revision (no final revision)",
            "Up to date (",
            "Updated: ",
            "Checked 5 plugin(s); updated 1.",
        ],
    );
}

#[cfg(unix)]
async fn exercise_git_updates(root: &Path, home: &Path) {
    let git = mock_git(root);
    let plugin_root = home.join("plugins");
    for repo in [
        "coverage-bad-head",
        "coverage-pull-fail",
        "coverage-bad-final",
        "coverage-unchanged",
        "coverage-changed",
    ] {
        let path = plugin_root.join(repo);
        fs::create_dir_all(path.join(".git")).unwrap();
        fs::write(path.join("plugin.yaml"), PLUGIN_MANIFEST).unwrap();
    }

    plugins("update", None).await.unwrap();
    for repo in [
        "coverage-bad-final",
        "coverage-unchanged",
        "coverage-changed",
    ] {
        assert_eq!(
            fs::read_to_string(plugin_root.join(repo).join(".mock-pulled")).unwrap(),
            "pulled\n"
        );
    }
    for repo in ["coverage-bad-head", "coverage-pull-fail"] {
        assert!(!plugin_root.join(repo).join(".mock-pulled").exists());
    }
    let calls = fs::read_to_string(root.join("git.log")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.ends_with("pull --ff-only"))
            .count(),
        4
    );
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.ends_with("rev-parse HEAD"))
            .count(),
        8
    );

    fs::remove_file(git).unwrap();
    assert_io_error(
        plugins("update", Some("coverage-changed")).await,
        "git rev-parse failed for coverage-changed:",
    );
}
