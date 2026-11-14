    /// Runs `body` with `HOME` set to a fresh folder and neither home variable set.
    fn with_clean_home(body: impl FnOnce(&std::path::Path)) {
        let _guard = env_lock();
        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            ["HOME", "HERMES_HOME", "HERMES_AGENT_ULTRA_HOME"]
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("HERMES_HOME");
        std::env::remove_var("HERMES_AGENT_ULTRA_HOME");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(tmp.path())));
        for (key, value) in saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// Every file under `root` with its bytes, in path order.
    fn tree_snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = std::fs::read(&path).expect("read file");
                    files.push((path.strip_prefix(root).unwrap().to_path_buf(), bytes));
                }
            }
        }
        files.sort();
        files
    }

    fn make_upstream_home(user_home: &std::path::Path) -> std::path::PathBuf {
        let upstream = user_home.join(".hermes");
        std::fs::create_dir_all(upstream.join("sessions")).unwrap();
        std::fs::write(upstream.join("config.yaml"), "model: upstream\n").unwrap();
        std::fs::write(upstream.join("sessions").join("s1.json"), b"{\"id\":\"s1\"}").unwrap();
        upstream
    }

    #[test]
    fn uninstall_yes_leaves_upstream_hermes_home_intact() {
        with_clean_home(|user_home| {
            let upstream = make_upstream_home(user_home);
            let before = tree_snapshot(&upstream);

            uninstall_home(&hermes_config::hermes_home(), true).expect("uninstall");

            assert!(upstream.is_dir(), "uninstall removed {}", upstream.display());
            assert_eq!(tree_snapshot(&upstream), before);
        });
    }

    #[test]
    fn uninstall_without_yes_names_the_home_and_removes_nothing() {
        with_clean_home(|user_home| {
            let home = user_home.join(".hermes-agent-ultra");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(home.join("config.yaml"), "model: own\n").unwrap();

            let message = uninstall_home(&home, false).expect("dry run");

            assert!(
                message.contains(&home.display().to_string()),
                "message does not name {}: {message}",
                home.display()
            );
            assert!(home.join("config.yaml").is_file());
        });
    }

    #[test]
    fn uninstall_yes_names_the_home_it_removes() {
        with_clean_home(|user_home| {
            let home = user_home.join(".hermes-agent-ultra");
            std::fs::create_dir_all(&home).unwrap();

            let message = uninstall_home(&home, true).expect("uninstall");

            assert!(message.contains(&home.display().to_string()), "{message}");
            assert!(!home.exists());
        });
    }

    #[test]
    fn uninstall_yes_refuses_upstream_hermes_home_when_it_is_the_home() {
        with_clean_home(|user_home| {
            let upstream = make_upstream_home(user_home);
            let before = tree_snapshot(&upstream);

            assert!(uninstall_home(&upstream, true).is_err());
            assert!(uninstall_home(&upstream, false).is_err());

            assert_eq!(tree_snapshot(&upstream), before);
        });
    }

    #[test]
    fn uninstall_yes_refuses_the_user_home_and_folders_that_hold_it() {
        let outer = tempfile::tempdir().expect("tempdir");
        let user_home = outer.path().join("users").join("me");
        std::fs::create_dir_all(user_home.join(".hermes-agent-ultra")).unwrap();
        std::fs::write(user_home.join("notes.txt"), "keep").unwrap();
        let _guard = env_lock();
        let saved = std::env::var_os("HOME");
        std::env::set_var("HOME", &user_home);
        let refused: Vec<bool> = [
            user_home.clone(),
            user_home.join(".hermes-agent-ultra").join(".."),
            outer.path().join("users"),
            outer.path().to_path_buf(),
        ]
        .iter()
        .map(|home| uninstall_home(home, true).is_err())
        .collect();
        match saved {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(refused, [true; 4]);
        assert!(user_home.join("notes.txt").is_file());
    }

    #[test]
    fn uninstall_refuses_when_the_user_home_is_unknown() {
        // With neither HOME nor USERPROFILE set, the user's home cannot be
        // protected, so HERMES_HOME could name it.
        let real_home = tempfile::tempdir().expect("tempdir");
        std::fs::write(real_home.path().join("notes.txt"), "keep").unwrap();
        let _guard = env_lock();
        let saved: Vec<_> = ["HOME", "USERPROFILE"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");
        let refused = [false, true].map(|yes| uninstall_home(real_home.path(), yes).is_err());
        for (key, value) in saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        assert_eq!(refused, [true, true]);
        assert!(real_home.path().join("notes.txt").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_refuses_folders_that_hold_upstreams_linked_home() {
        let store = tempfile::tempdir().expect("tempdir");
        let linked = store.path().join("upstream");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join("config.yaml"), "model: upstream\n").unwrap();
        with_clean_home(|user_home| {
            std::os::unix::fs::symlink(&linked, user_home.join(".hermes")).unwrap();

            let refused = [false, true].map(|yes| uninstall_home(store.path(), yes).is_err());

            assert_eq!(refused, [true, true]);
            assert!(linked.join("config.yaml").is_file());
        });
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_refuses_an_own_home_that_links_to_upstreams() {
        with_clean_home(|user_home| {
            let upstream = make_upstream_home(user_home);
            let before = tree_snapshot(&upstream);
            std::os::unix::fs::symlink(&upstream, user_home.join(".hermes-agent-ultra")).unwrap();

            let refused =
                [false, true].map(|yes| uninstall_home(&hermes_config::hermes_home(), yes).is_err());

            assert_eq!(refused, [true, true]);
            assert_eq!(tree_snapshot(&upstream), before);
        });
    }
