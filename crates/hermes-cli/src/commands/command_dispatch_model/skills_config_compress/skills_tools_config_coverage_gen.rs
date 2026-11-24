use super::*;

fn assert_invocation(args: &[&str], action: Option<&str>, name: Option<&str>, extra: Option<&str>) {
    let parsed = parse_skills_slash_invocation(args).expect("valid /skills arguments");
    assert_eq!(
        (
            parsed.action.as_deref(),
            parsed.name.as_deref(),
            parsed.extra.as_deref(),
        ),
        (action, name, extra),
        "arguments: {args:?}",
    );
}

#[test]
fn skills_parser_handles_empty_and_unknown_commands() {
    assert_invocation(&[], None, None, None);

    let error = parse_skills_slash_invocation(&["Not-A-Command", "ignored"])
        .expect_err("unknown subcommands must be rejected");
    assert!(error.starts_with("Unknown /skills subcommand 'not-a-command'."));
    assert!(error.contains("`/skills list`"));
    assert!(error.contains("`/skills search <query>`"));
}

#[test]
fn skills_parser_joins_names_for_list_and_hub_actions() {
    for action in [
        "list",
        "browse",
        "audit",
        "quality",
        "search",
        "install",
        "inspect",
        "uninstall",
        "remove",
        "publish",
        "subscribe",
        "reset",
    ] {
        let uppercase = action.to_ascii_uppercase();
        assert_invocation(
            &[&uppercase, "  Team", "Tool  "],
            Some(action),
            Some("Team Tool"),
            None,
        );
        assert_invocation(&[action, " ", "\t"], Some(action), None, None);
    }
}

#[test]
fn skills_parser_uses_only_the_first_name_for_check() {
    assert_invocation(&["check"], Some("check"), None, None);
    assert_invocation(
        &["CHECK", "MySkill", "ignored"],
        Some("check"),
        Some("MySkill"),
        None,
    );
}

#[test]
fn skills_parser_normalizes_update_apply_flags() {
    for flag in ["--apply", "--APPLY", "-a", "-A"] {
        assert_invocation(
            &["UPDATE", "ignored", flag],
            Some("update"),
            None,
            Some("--apply"),
        );
    }

    assert_invocation(&["update"], Some("update"), None, None);
    assert_invocation(
        &["update", "--apply=false", "-all"],
        Some("update"),
        None,
        None,
    );
}

#[test]
fn skills_parser_keeps_names_and_extras_separate() {
    for (input, action, expected_name) in [
        ("SNAPSHOT", "snapshot", "MixedCase"),
        ("TAP", "tap", "mixedcase"),
        ("CONFIG", "config", "MixedCase"),
    ] {
        assert_invocation(&[input], Some(action), None, None);
        assert_invocation(
            &[input, "MixedCase"],
            Some(action),
            Some(expected_name),
            None,
        );
        assert_invocation(
            &[input, "MixedCase", "  Keep", "This Case  "],
            Some(action),
            Some(expected_name),
            Some("Keep This Case"),
        );
    }
}

#[tokio::test]
async fn skills_cli_passes_extra_arguments_and_combines_output() {
    // The runner launches current_exe. In a unit test that is libtest, so use
    // its exact filter to re-enter only this test as the subprocess fixture.
    let (_, test_name) = concat!(
        module_path!(),
        "::skills_cli_passes_extra_arguments_and_combines_output"
    )
    .split_once("::")
    .expect("the test is in a child module");

    let args: Vec<_> = std::env::args().skip(1).collect(); // nosemgrep: rust.lang.security.args.args
    if args.first().is_some_and(|arg| arg == "skills") {
        if args.get(1).is_some_and(|arg| arg == "opt-in") {
            assert_eq!(
                args,
                ["skills", "opt-in", test_name, "--exact", "--nocapture"]
            );
            println!("skills fixture stdout");
            eprintln!("  skills fixture stderr  ");
        } else {
            assert_eq!(args, ["skills", "--exact", "--", "--extra", test_name]);
        }
        return;
    }

    let opt_in = parse_skills_slash_invocation(&["opt-in", test_name, "--exact", "--nocapture"])
        .expect("valid opt-in invocation");
    let output = run_skills_subcommand_via_cli(&opt_in)
        .await
        .expect("run the opt-in argument fixture");
    let (stdout, stderr) = output
        .split_once("\n\nstderr:\n")
        .expect("stdout and labeled stderr should be separated by a blank line");
    assert!(stdout.contains("skills fixture stdout"), "{output}");
    assert!(stdout.contains("1 passed; 0 failed"), "{output}");
    assert_eq!(stderr, "skills fixture stderr");

    // Stop libtest option parsing before --extra so it reaches the fixture as
    // a literal argument rather than being rejected as a libtest option.
    let other = SkillsSlashInvocation {
        action: Some("--exact".into()),
        name: Some("--".into()),
        extra: Some(test_name.into()),
    };
    let output = run_skills_subcommand_via_cli(&other)
        .await
        .expect("run the regular extra argument fixture");
    assert!(output.contains("1 passed; 0 failed"), "{output}");
}

#[tokio::test]
async fn skills_cli_reports_no_output_for_a_quiet_success() {
    // Terse listing with no matches emits nothing and never runs any tests.
    let invocation = parse_skills_slash_invocation(&[
        "opt-out",
        "--list",
        "--format=terse",
        "--exact",
        "--skip=skills",
        "--skip=opt-out",
    ])
    .expect("valid opt-out invocation");
    let output = run_skills_subcommand_via_cli(&invocation)
        .await
        .expect("run the quiet listing fixture");
    assert_eq!(output, "No output.");
}
