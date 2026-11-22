use super::*;

#[test]
fn contextual_completion_rejects_non_commands_and_preserves_ambiguous_matches() {
    assert!(autocomplete_contextual(" \ttools enable").is_empty());

    let prefix = ["/", "/s", "/co"]
        .into_iter()
        .find(|&prefix| {
            !SLASH_COMMANDS
                .iter()
                .any(|(name, _)| *name == canonical_command(prefix))
                && autocomplete(prefix).len() > 1
        })
        .expect("the catalog should have an ambiguous command prefix");

    assert_eq!(resolve_completion_command(prefix), None);
    assert_eq!(
        autocomplete_contextual(&format!("{prefix} extra")),
        autocomplete(prefix)
    );
}

#[test]
fn unique_command_prefix_resolves_to_the_full_command() {
    let (prefix, command) = SLASH_COMMANDS
        .iter()
        .flat_map(|&(command, _)| {
            command
                .char_indices()
                .rev()
                .map(move |(end, _)| (&command[..end], command))
        })
        .find(|&(prefix, command)| {
            prefix.len() > 1
                && !SLASH_COMMANDS
                    .iter()
                    .any(|(name, _)| *name == canonical_command(prefix))
                && autocomplete(prefix) == [command]
        })
        .expect("the catalog should have a uniquely completable command prefix");

    assert_eq!(resolve_completion_command(prefix).as_deref(), Some(command));
}

#[test]
fn contextual_completion_filters_nested_fragments_case_insensitively() {
    assert_eq!(
        autocomplete_contextual("  /swarm RUN 4 SE"),
        ["/swarm RUN 4 sequential"]
    );
}

#[test]
fn tools_completion_lists_actions_and_filters_the_current_fragment() {
    assert_eq!(
        autocomplete_contextual("/tools "),
        [
            "/tools list ",
            "/tools trust ",
            "/tools enable ",
            "/tools disable ",
        ]
    );
    assert_eq!(autocomplete_contextual("/tools EN"), ["/tools enable"]);
}

#[test]
fn runtime_dependent_completions_are_empty_without_runtime_context() {
    assert!(autocomplete_contextual("/handoff ").is_empty());
    assert!(autocomplete_contextual("/tools enable ").is_empty());
}

#[test]
fn runtime_tools_completion_does_not_offer_tool_names_for_trust() {
    let config = GatewayConfig::default();
    let tool_registry = hermes_tools::ToolRegistry::new();
    let runtime = CompletionRuntime {
        config: &config,
        tool_registry: &tool_registry,
    };

    assert!(autocomplete_contextual_with_runtime("/tools trust ", Some(runtime)).is_empty());
}

#[test]
fn nested_completions_offer_values_for_the_requested_argument_position() {
    let cases: &[(&str, &str, usize, &[&str])] = &[
        ("/swarm", "RUN", 1, &["1", "2", "4", "8", "16", "32", "64"]),
        ("/swarm", "run", 2, &["concurrent", "sequential", "graph"]),
        ("/swarm", "voters", 1, &["2", "3", "4", "5", "6", "7", "8"]),
        ("/quorum", "voters", 1, &["2", "3", "4", "5", "6", "7", "8"]),
        (
            "/objective",
            "lifecycle",
            1,
            &[
                "status",
                "active",
                "pause",
                "resume",
                "budget-limited",
                "achieved",
                "unmet",
            ],
        ),
        (
            "/objective",
            "profile",
            1,
            &["status", "list", "general", "me", "set"],
        ),
        (
            "/objective",
            "context",
            1,
            &["status", "list", "max", "balanced", "fast"],
        ),
        (
            "/objective",
            "simulator",
            1,
            &["status", "balanced", "strict", "aggressive"],
        ),
        (
            "/objective",
            "ensemble",
            1,
            &["status", "committee", "single", "debate"],
        ),
        ("/objective", "ledger", 1, &["status", "tail", "clear"]),
        ("/objective", "dag", 1, &["status", "rebuild", "clear"]),
        ("/objective", "eval", 1, &["status", "tail"]),
        ("/objective", "wait", 1, &["--session", "--seconds", "for"]),
        (
            "/model",
            "why-not",
            1,
            &[
                "--cap",
                "--min-context",
                "--max-input-cost",
                "--max-output-cost",
                "--budget",
            ],
        ),
    ];

    for &(command, subcommand, position, expected) in cases {
        assert_eq!(
            command_nested_candidates(command, subcommand, position).as_slice(),
            expected,
            "{command} {subcommand}, argument {position}"
        );
    }

    assert!(command_nested_candidates("/swarm", "run", 3).is_empty());
}

#[test]
fn subcommand_overrides_preserve_the_catalog_order() {
    let cases: &[(&str, &[&str])] = &[
        (
            "/pet",
            &[
                "status", "on", "off", "toggle", "list", "set", "mood", "dock", "speed",
            ],
        ),
        (
            "/objective",
            &[
                "status",
                "verify",
                "plan",
                "constraints",
                "counterfactual",
                "wait",
                "unwait",
                "profile",
                "context",
                "simulator",
                "ensemble",
                "ledger",
                "dag",
                "eval",
                "clear",
                "lifecycle",
                "behavior",
            ],
        ),
        ("/simulate", &["status"]),
        ("/timetravel", &["list", "latest", "goto", "undo", "branch"]),
        (
            "/journey",
            &[
                "status",
                "graph",
                "ledger",
                "tail",
                "walkthrough",
                "timetravel",
            ],
        ),
        ("/autocompact", &["status", "now", "governance"]),
        ("/qos", &["status", "health", "autotune"]),
        ("/claims", &["status", "on", "off"]),
    ];

    for &(command, expected) in cases {
        assert_eq!(command_subcommand_overrides(command), expected, "{command}");
    }
    assert!(command_subcommand_overrides("/not-a-catalog-command").is_empty());
}

#[test]
fn description_completions_add_only_distinct_single_word_alternatives() {
    assert!(inferred_subcommands_from_description("/not-a-catalog-command").is_empty());

    let quoted_segment = regex::Regex::new(r"`([^`]*)`").unwrap();
    let single_word =
        regex::Regex::new(r"^[^A-Za-z0-9_-]*([A-Za-z0-9_-]+)[^A-Za-z0-9_-]*$").unwrap();

    for (command, description) in SLASH_COMMANDS.iter().copied() {
        let mut inferred = Vec::new();
        for segment in quoted_segment.captures_iter(description) {
            for alternative in segment[1].split('|') {
                if let Some(capture) = single_word.captures(alternative) {
                    let word = capture[1].to_ascii_lowercase();
                    if word != command.trim_start_matches('/') && !inferred.contains(&word) {
                        inferred.push(word);
                    }
                }
            }
        }
        assert_eq!(
            inferred_subcommands_from_description(command),
            inferred,
            "{command}"
        );

        let mut merged = Vec::new();
        for candidate in command_subcommand_overrides(command)
            .iter()
            .copied()
            .chain(inferred.iter().map(String::as_str))
        {
            if !merged.contains(&candidate) {
                merged.push(candidate);
            }
        }
        assert_eq!(command_subcommand_candidates(command), merged, "{command}");
    }
}
