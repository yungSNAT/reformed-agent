use super::*;

#[tokio::test]
async fn semantic_graph_counts_weighted_imports_and_skips_generated_sources() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::graph_journey_contract::semantic_graph_counts_weighted_imports_and_skips_generated_sources",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let repo = tempdir().unwrap();
    let empty = repo.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let missing = repo.path().join("missing");
    let output = command_output(&mut app, "/graph", &["repo", missing.to_str().unwrap()]).await;
    assert!(output.contains("Repo path does not exist:"), "{output}");
    let output = command_output(
        &mut app,
        "/graph",
        &["repo", empty.to_str().unwrap(), "--max-files", "5000"],
    )
    .await;
    assert!(
        output.contains("No candidate source files found"),
        "{output}"
    );
    assert!(output.contains("max_files=1500"), "{output}");

    for directory in ["src", "target", ".git", "node_modules"] {
        std::fs::create_dir(repo.path().join(directory)).unwrap();
    }
    for (name, content) in [
        ("café.rs", "use crate::input;\nuse crate::input;\nmod help;"),
        (
            "src/work.py",
            "import os, json as j\nfrom pathlib import Path",
        ),
        (
            "view.tsx",
            "import { Widget } from './widget';\nconst core = require(\"fixture-core\");",
        ),
        ("target/built.rs", "use crate::generated;"),
        (".git/ignored.js", "import x from 'generated';"),
        ("node_modules/ignored.js", "import x from 'generated';"),
        ("notes.txt", "use crate::generated;"),
    ] {
        std::fs::write(repo.path().join(name), content).unwrap();
    }

    let output = command_output(
        &mut app,
        "/graph",
        &[
            "semantic",
            repo.path().to_str().unwrap(),
            "--max-files",
            "3",
        ],
    )
    .await;
    for expected in [
        "files_scanned=3 (cap=20)",
        "semantic_edges=7",
        "1. café.rs -> crate::input (2)",
        "café.rs -> help (1)",
        "src/work.py -> json (1)",
        "src/work.py -> os (1)",
        "src/work.py -> pathlib (1)",
        "view.tsx -> ./widget (1)",
        "view.tsx -> fixture-core (1)",
        "caf_.rs[\"café.rs\"] --> crate::input[\"crate::input\"]",
    ] {
        assert!(output.contains(expected), "{expected}: {output}");
    }
    assert!(!output.contains("generated"), "{output}");
}

#[tokio::test]
async fn journey_reads_local_decisions_and_forwards_graph_diagnostics() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    if !crate::isolated_test_profile::enter(
        "commands::tests::graph_journey_contract::journey_reads_local_decisions_and_forwards_graph_diagnostics",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let empty = command_output(&mut app, "/journey", &[]).await;
    assert!(empty.contains("objective_learning_entries: 0"), "{empty}");
    assert!(empty.contains("latest_learning_entry: none"), "{empty}");

    command_output(&mut app, "/objective", &["Review synthetic graph imports"]).await;
    command_output(&mut app, "/objective", &["pause", "fixture review"]).await;
    let timeline = command_output(&mut app, "/journey", &["timeline"]).await;
    assert!(
        timeline.contains("state=paused decision=objective_status_paused"),
        "{timeline}"
    );
    let latest = command_output(&mut app, "/journey", &["tail", "1"]).await;
    assert!(
        latest.contains("Objective learning ledger tail"),
        "{latest}"
    );
    let decisions: Vec<_> = latest
        .lines()
        .filter(|line| line.starts_with("- "))
        .collect();
    assert_eq!(decisions.len(), 1, "{latest}");
    assert!(decisions[0].contains("objective_status_paused"), "{latest}");
    let ledger = command_output(&mut app, "/journey", &["ledger"]).await;
    assert!(
        ledger.starts_with("Objective learning ledger\n"),
        "{ledger}"
    );
    assert!(
        ledger.contains("paused objective_status_paused"),
        "{ledger}"
    );
    assert_eq!(
        command_output(&mut app, "/journey", &["ledger", "tail", "1"]).await,
        latest
    );

    for (args, expected) in [
        (
            &["walkthrough", "start", "full"][..],
            "Started full walkthrough",
        ),
        (&["walkthrough"][..], "Walkthrough insights"),
        (&["snapshots"][..], "No snapshots found in"),
        (
            &["timetravel", "goto"][..],
            "Usage: /timetravel goto <snapshot-name>",
        ),
    ] {
        let output = command_output(&mut app, "/journey", args).await;
        assert!(output.starts_with(expected), "{output}");
    }
    let unknown = command_output(&mut app, "/journey", &["missing-action"]).await;
    assert!(
        unknown.contains("Unknown /journey action 'missing-action'"),
        "{unknown}"
    );

    for telemetry_status in [200, 503] {
        let server = MockServer::start().await;
        std::env::set_var("CONTEXTLATTICE_ORCHESTRATOR_URL", server.uri());
        std::env::set_var("CONTEXTLATTICE_API_KEY", "fixture-graph-access");
        for (endpoint, response) in [
            ("/health", ResponseTemplate::new(200)),
            (
                "/telemetry/embeddings",
                ResponseTemplate::new(telemetry_status).set_body_json(serde_json::json!({
                    "embeddings": {"backend": "fixture", "model": "fixture-embedding", "dimension": 12}
                })),
            ),
            (
                "/telemetry/recall",
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "qps": 2.5,
                    "grounded_hit_rate": 0.75
                })),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .and(header("x-api-key", "fixture-graph-access"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
        }
        let (args, heading, expected) = if telemetry_status == 200 {
            std::sync::Arc::make_mut(&mut app.config).mcp_servers = vec![
                hermes_config::McpServerEntry {
                    name: "fixture-unrelated".into(),
                    ..Default::default()
                },
                hermes_config::McpServerEntry {
                    name: "fixture-memory".into(),
                    url: Some(format!("{}/ContextLattice", server.uri())),
                    ..Default::default()
                },
            ];
            (
                &["graph"][..],
                "Graph-memory status",
                "embedding_diagnostics: backend=fixture model=fixture-embedding dimension=12",
            )
        } else {
            (
                &["graph", "diag"][..],
                "ContextLattice embedding diagnostics",
                "embedding_diagnostics: fallback=recall_telemetry",
            )
        };
        let output = command_output(&mut app, "/journey", args).await;
        assert!(output.contains(heading), "{output}");
        assert!(output.contains("health_status: 200"), "{output}");
        assert!(output.contains(expected), "{output}");
        if telemetry_status == 200 {
            assert!(output.contains("contextlattice_mcp: yes"), "{output}");
        } else {
            assert!(
                output.contains("telemetry/embeddings status=503"),
                "{output}"
            );
        }
        assert!(
            output.contains("recall_telemetry: qps=2.500 hit_rate=0.750"),
            "{output}"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }
}
