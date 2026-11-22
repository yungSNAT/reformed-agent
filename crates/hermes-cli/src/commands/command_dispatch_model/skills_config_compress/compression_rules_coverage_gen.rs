use super::*;

fn policy_limits(policy: &CompressionRenderPolicy) -> [usize; 4] {
    [
        policy.max_assistant_render_lines,
        policy.max_tool_output_lines,
        policy.max_tool_output_line_chars,
        policy.max_tool_output_total_chars,
    ]
}

fn plane_limits(plane: &CompressionRulePlaneConfig) -> [Option<usize>; 4] {
    [
        plane.max_assistant_render_lines,
        plane.max_tool_output_lines,
        plane.max_tool_output_line_chars,
        plane.max_tool_output_total_chars,
    ]
}

#[test]
fn applying_planes_clamps_present_limits_and_preserves_omitted_limits() {
    let mut policy = CompressionRenderPolicy::builtin_defaults();
    policy.apply_plane(&CompressionRulePlaneConfig {
        max_assistant_render_lines: None,
        max_tool_output_lines: Some(0),
        max_tool_output_line_chars: Some(usize::MAX),
        max_tool_output_total_chars: Some(0),
    });
    assert_eq!(policy_limits(&policy), [260, 20, 4000, 2000]);

    policy.apply_plane(&CompressionRulePlaneConfig {
        max_assistant_render_lines: Some(usize::MAX),
        max_tool_output_lines: Some(usize::MAX),
        max_tool_output_line_chars: Some(0),
        max_tool_output_total_chars: Some(usize::MAX),
    });
    assert_eq!(policy_limits(&policy), [4000, 5000, 120, 500_000]);

    policy.apply_plane(&CompressionRulePlaneConfig {
        max_tool_output_lines: Some(73),
        ..CompressionRulePlaneConfig::default()
    });
    assert_eq!(policy_limits(&policy), [4000, 73, 120, 500_000]);
}

#[test]
fn rule_key_aliases_are_case_insensitive_and_trimmed() {
    for (aliases, canonical) in [
        (
            [
                "assistant_lines",
                " MAX_ASSISTANT_RENDER_LINES ",
                "Assistant",
            ],
            "assistant_lines",
        ),
        (
            ["tool_lines", " MAX_TOOL_OUTPUT_LINES ", "Tool"],
            "tool_lines",
        ),
        (
            [
                "tool_line_chars",
                " MAX_TOOL_OUTPUT_LINE_CHARS ",
                "Tool_Chars",
            ],
            "tool_line_chars",
        ),
        (
            [
                "tool_total_chars",
                " MAX_TOOL_OUTPUT_TOTAL_CHARS ",
                "Tool_Total",
            ],
            "tool_total_chars",
        ),
    ] {
        for alias in aliases {
            assert_eq!(
                parse_compression_rule_key(alias),
                Some(canonical),
                "{alias}"
            );
        }
    }
    assert_eq!(parse_compression_rule_key("tool_bytes"), None);
}

#[test]
fn setting_rules_clamps_each_field_and_rejects_unknown_keys_without_mutating() {
    let mut plane = CompressionRulePlaneConfig::default();
    for (value, expected) in [
        (0, [40, 20, 120, 2000]),
        (usize::MAX, [4000, 5000, 4000, 500_000]),
        (2345, [2345; 4]),
    ] {
        for key in [" Assistant ", " TOOL ", " Tool_Chars ", " Tool_Total "] {
            set_compression_rule_field(&mut plane, key, value).expect("set a known rule");
        }
        assert_eq!(plane_limits(&plane), expected.map(Some));
    }

    let error = set_compression_rule_field(&mut plane, "tool_bytes", 1)
        .expect_err("reject an unknown rule");
    assert!(matches!(
        error,
        AgentError::Config(message)
            if message == "Unknown compression rule key 'tool_bytes'. Use assistant_lines|tool_lines|tool_line_chars|tool_total_chars."
    ));
    assert_eq!(plane_limits(&plane), [Some(2345); 4]);
}

#[test]
fn saving_a_plane_creates_parent_directories_and_preserves_unset_fields() {
    let temp = tempfile::tempdir().expect("create scratch directory");
    let path = temp
        .path()
        .join("nested")
        .join("compression")
        .join("rules.json");
    let plane = CompressionRulePlaneConfig {
        max_tool_output_line_chars: Some(811),
        ..CompressionRulePlaneConfig::default()
    };

    assert!(load_compression_plane(&path).is_none());
    save_compression_plane(&path, &plane).expect("save to a new directory");
    let loaded = load_compression_plane(&path).expect("load the saved plane");
    assert_eq!(plane_limits(&loaded), [None, None, Some(811), None]);

    std::fs::write(&path, "not JSON").expect("replace rules with malformed data");
    assert!(load_compression_plane(&path).is_none());

    let error = save_compression_plane(&path.join("child.json"), &plane)
        .expect_err("a regular file cannot be a parent directory");
    assert!(matches!(
        error,
        AgentError::Io(message) if message.starts_with(&format!("Failed to create {}:", path.display()))
    ));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read original file"),
        "not JSON"
    );
}

fn assert_policy_status(user: &str, project: &str, limits: [usize; 4]) {
    let [assistant, tool, line_chars, total_chars] = limits;
    let expected = format!(
        "Compression policy planes\n\
         -------------------------\n\
         builtin: max_assistant_render_lines=260, max_tool_output_lines=180, max_tool_output_line_chars=600, max_tool_output_total_chars=48000\n\
         user: {user}\n\
         project: {project}\n\
         \nmerged:\n  - max_assistant_render_lines={assistant}\n  - max_tool_output_lines={tool}\n  - max_tool_output_line_chars={line_chars}\n  - max_tool_output_total_chars={total_chars}\n\
         \nUse `/compress rules recommend` to generate heuristics from current transcript shape.\n\
         Use `/compress rules autotune` for dry-run tuning, or `/compress rules autotune apply [user|project]` to persist + apply.\n\
         Use `/compress rules apply` to push merged settings into live runtime env.\n\
         Use `/compress rules set user <key> <value>` or `/compress rules set project <key> <value>`.\n\
         Keys: assistant_lines | tool_lines | tool_line_chars | tool_total_chars"
    );
    assert_eq!(render_compression_policy_status(), expected);
}

fn check_policy_status_fixture(scenario: &str) {
    let home = PathBuf::from(std::env::var_os("HERMES_HOME").expect("isolated home"));
    let user_path = home.join("compression").join("user-rules.json");
    assert_eq!(
        resolve_compression_plane_path(" UsEr ").expect("resolve the user plane"),
        user_path
    );
    assert!(matches!(
        resolve_compression_plane_path("global"),
        Err(AgentError::Config(message)) if message == "Plane must be `user` or `project`."
    ));

    let defaults = [260, 180, 600, 48_000];
    let unconfigured_user = format!("not configured ({})", user_path.display());
    if scenario == "outside" {
        assert!(matches!(
            resolve_compression_plane_path(" PrOjEcT "),
            Err(AgentError::Config(message))
                if message == "Project plane unavailable: run inside a repository checkout."
        ));
        assert_policy_status(
            &unconfigured_user,
            "not configured ((repo root unavailable))",
            defaults,
        );
        return;
    }

    assert_eq!(scenario, "checkout");
    let cwd = std::env::current_dir().expect("read isolated working directory");
    let project_path = cwd
        .parent()
        .expect("working directory is a subdirectory of the checkout")
        .join(".hermes-ultra")
        .join("compression-rules.json");
    assert_eq!(
        resolve_compression_plane_path(" PrOjEcT ").expect("resolve the project plane"),
        project_path
    );
    let unconfigured_project = format!("not configured ({})", project_path.display());
    assert_policy_status(&unconfigured_user, &unconfigured_project, defaults);

    save_compression_plane(
        &user_path,
        &CompressionRulePlaneConfig {
            max_assistant_render_lines: Some(125),
            max_tool_output_lines: Some(75),
            max_tool_output_total_chars: Some(80_000),
            ..CompressionRulePlaneConfig::default()
        },
    )
    .expect("save user rules");
    let configured_user = format!("configured ({})", user_path.display());
    assert_policy_status(
        &configured_user,
        &unconfigured_project,
        [125, 75, 600, 80_000],
    );

    save_compression_plane(
        &project_path,
        &CompressionRulePlaneConfig {
            max_tool_output_lines: Some(1),
            max_tool_output_line_chars: Some(100),
            ..CompressionRulePlaneConfig::default()
        },
    )
    .expect("save project rules");
    let (merged, user, project) = merged_compression_policy();
    assert_eq!(policy_limits(&merged), [125, 20, 120, 80_000]);
    assert_eq!(
        plane_limits(&user.expect("loaded user plane")),
        [Some(125), Some(75), None, Some(80_000)]
    );
    assert_eq!(
        plane_limits(&project.expect("loaded project plane")),
        [None, Some(1), Some(100), None]
    );
    assert_policy_status(
        &configured_user,
        &format!("configured ({})", project_path.display()),
        [125, 20, 120, 80_000],
    );
}

#[test]
fn status_reports_discovered_planes_and_project_precedence() {
    const SCENARIO: &str = "HERMES_TEST_COMPRESSION_RULES_STATUS_SCENARIO";
    const CONFIRMED: &str = "isolated compression status assertions completed";

    // The subprocess keeps both the working directory and home overrides
    // private to this fixture, even when the other unit tests run in parallel.
    if let Ok(scenario) = std::env::var(SCENARIO) {
        check_policy_status_fixture(&scenario);
        println!("{CONFIRMED}");
        return;
    }

    let temp = tempfile::tempdir().expect("create isolated workspace");
    let root = temp.path().canonicalize().expect("canonicalize workspace");
    let checkout = root.join("checkout");
    let git = checkout.join(".git");
    std::fs::create_dir_all(git.join("objects")).expect("create Git objects directory");
    std::fs::create_dir_all(git.join("refs").join("heads")).expect("create Git refs directory");
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("write Git HEAD");

    let (_, module) = module_path!().split_once("::").expect("test module path");
    let test_name = format!("{module}::status_reports_discovered_planes_and_project_precedence");
    let exe = std::env::current_exe().expect("locate unit-test executable");

    for (scenario, cwd) in [
        ("outside", root.join("outside")),
        ("checkout", checkout.join("subdirectory")),
    ] {
        let home = root.join(format!("{scenario}-home"));
        std::fs::create_dir(&cwd).expect("create isolated working directory");
        std::fs::create_dir(&home).expect("create isolated home");

        let output = std::process::Command::new(&exe)
            .args(["--exact", &test_name, "--nocapture"])
            .current_dir(&cwd)
            .env(SCENARIO, scenario)
            .env("HERMES_HOME", &home)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &root)
            .output()
            .expect("run isolated status test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(CONFIRMED),
            "{scenario} status test failed ({}):\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
