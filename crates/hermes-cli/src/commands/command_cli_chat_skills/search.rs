use super::*;

pub(super) async fn search<T: SkillTapSource>(
    query: &str,
    home: &Path,
    [index_url, hub_url, skills_sh_url]: [&str; 3],
    create_client: impl FnOnce() -> reqwest::Client,
    tap_source: impl FnOnce(&reqwest::Client) -> T,
    mut display: impl FnMut(std::fmt::Arguments<'_>),
) -> Result<(), AgentError> {
    if query.is_empty() {
        display(format_args!("Usage: hermes skills search <query>"));
        return Ok(());
    }
    display(format_args!("Searching registries for: \"{}\"...", query));
    let client = create_client();
    let mut displayed_results = false;

    if let Ok(results) = search_multi_registry(&client, index_url, query, 40).await {
        if !results.is_empty() {
            displayed_results = true;
            display(format_args!("Multi-registry matches:"));
            for rec in results {
                let short_desc = if rec.description.trim().is_empty() {
                    "(no description)"
                } else {
                    rec.description.trim()
                };
                display(format_args!(
                    "  • [{}] {} — {}",
                    rec.source, rec.identifier, short_desc
                ));
            }
            display(format_args!(
                "\nInstall with: hermes skills install <identifier> (example: skills.sh/anthropics/skills/skill-creator)"
            ));
        }
    }

    // Legacy hub path retained for compatibility.
    match client
        .get(hub_url)
        .query(&[("q", query)])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                if let Some(results) = data.get("results").and_then(|r| r.as_array()) {
                    if results.is_empty() {
                        if !displayed_results {
                            display(format_args!("No skills found matching \"{}\".", query));
                        }
                    } else {
                        displayed_results = true;
                        display(format_args!("\nLegacy Skills Hub matches:"));
                        for skill in results {
                            let name = skill.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            let desc = skill
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("");
                            let version =
                                skill.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                            display(format_args!("  • {} (v{}) — {}", name, version, desc));
                        }
                        display(format_args!("\nInstall with: hermes skills install <name>"));
                    }
                } else if !displayed_results {
                    display(format_args!("Unexpected response format from Skills Hub."));
                }
            }
        }
        Ok(resp) => {
            if !displayed_results {
                display(format_args!("Skills Hub returned status {}", resp.status()));
            }
        }
        Err(e) => {
            if !displayed_results {
                display(format_args!("Could not reach Skills Hub: {}", e));
            }
        }
    }
    if !displayed_results {
        if let Ok(skills_sh_hits) =
            search_skills_sh_registry(&client, skills_sh_url, query, 20).await
        {
            if !skills_sh_hits.is_empty() {
                displayed_results = true;
                display(format_args!("\nSkills.sh fallback matches:"));
                for (name, identifier) in skills_sh_hits {
                    display(format_args!("  • {} — {}", name, identifier));
                }
                display(format_args!(
                    "\nInstall with: hermes skills install skills.sh/<owner/repo/skill>"
                ));
            }
        }
    }
    if !displayed_results {
        let taps_file = home.join("skill_taps.json");
        let subscriptions_file = home.join("skills").join("subscriptions.json");
        let taps = effective_skill_taps(&taps_file, &subscriptions_file);
        let fallback = search_skills_via_taps(&tap_source(&client), &taps, query, 20).await?;
        if fallback.is_empty() {
            display(format_args!(
                "No tap-backed matches found for \"{}\".",
                query
            ));
        } else {
            display(format_args!("\nTap-backed matches:"));
            for (name, source) in fallback {
                display(format_args!("  • {} — {}", name, source));
            }
            display(format_args!(
                "\nInstall with: hermes skills install <name> or hermes skills install <owner/repo/path>"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fmt::Write;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct FixtureTap;

    impl SkillTapSource for FixtureTap {
        async fn branch_and_tree(
            &self,
            repo: &str,
        ) -> Result<(String, Vec<GitHubTreeEntry>), AgentError> {
            let entries = if repo == "fixture/reporting" {
                vec![GitHubTreeEntry {
                    kind: "blob".into(),
                    path: "skills/report-review/SKILL.md".into(),
                }]
            } else {
                Vec::new()
            };
            Ok(("main".into(), entries))
        }
    }

    fn index_with_matches() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "skills": [
                {
                    "name": "report-audit",
                    "identifier": "report-audit",
                    "source": "clawhub",
                    "description": " "
                },
                {
                    "name": "report",
                    "identifier": "official/report",
                    "source": "official",
                    "path": "skills/report",
                    "description": " Renders report summaries. "
                }
            ]
        }))
    }

    async fn search_fixture(
        index: ResponseTemplate,
        hub: Option<ResponseTemplate>,
        fallback: Option<ResponseTemplate>,
        configure_tap: bool,
    ) -> String {
        let home = tempfile::tempdir().unwrap();
        if configure_tap {
            write_skill_taps(
                &home.path().join("skill_taps.json"),
                &["fixture/reporting".into()],
            )
            .unwrap();
        }
        let server = MockServer::start().await;
        let mut expected_paths = vec!["/index"];
        Mock::given(method("GET"))
            .and(path("/index"))
            .respond_with(index)
            .expect(1)
            .mount(&server)
            .await;
        let hub_url = if let Some(response) = hub {
            expected_paths.push("/hub");
            Mock::given(method("GET"))
                .and(path("/hub"))
                .and(query_param("q", "report"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            format!("{}/hub", server.uri())
        } else {
            "invalid URL".into()
        };
        let requests = if fallback.is_some() {
            expected_paths.push("/fallback");
            1
        } else {
            0
        };
        Mock::given(method("GET"))
            .and(path("/fallback"))
            .and(query_param("q", "report"))
            .and(query_param("limit", "20"))
            .respond_with(fallback.unwrap_or_else(|| ResponseTemplate::new(500)))
            .expect(requests)
            .mount(&server)
            .await;

        let mut output = String::new();
        search(
            "report",
            home.path(),
            [
                &format!("{}/index", server.uri()),
                &hub_url,
                &format!("{}/fallback", server.uri()),
            ],
            || {
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap()
            },
            |_| {
                assert_eq!(requests, 1, "tap lookup followed a registry match");
                FixtureTap
            },
            |line| writeln!(&mut output, "{line}").unwrap(),
        )
        .await
        .unwrap();
        server.verify().await;
        let received = server.received_requests().await.unwrap();
        let actual_paths: Vec<_> = received.iter().map(|request| request.url.path()).collect();
        assert_eq!(actual_paths, expected_paths);
        output
    }

    #[tokio::test]
    async fn search_displays_ranked_index_matches_before_legacy_matches() {
        let hub = ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"name": "report-archive", "description": "Older releases", "version": "1.2"},
                {}
            ]
        }));
        let output = search_fixture(index_with_matches(), Some(hub), None, false).await;
        assert_eq!(
            output,
            concat!(
                "Searching registries for: \"report\"...\n",
                "Multi-registry matches:\n",
                "  • [official] official/report — Renders report summaries.\n",
                "  • [clawhub] report-audit — (no description)\n",
                "\nInstall with: hermes skills install <identifier> (example: skills.sh/anthropics/skills/skill-creator)\n",
                "\nLegacy Skills Hub matches:\n",
                "  • report-archive (v1.2) — Older releases\n",
                "  • ? (v?) — \n",
                "\nInstall with: hermes skills install <name>\n",
            )
        );
    }

    #[tokio::test]
    async fn search_keeps_index_matches_when_the_legacy_hub_has_no_results() {
        for hub in [
            Some(ResponseTemplate::new(200).set_body_json(json!({"results": []}))),
            Some(ResponseTemplate::new(200).set_body_json(json!({"entries": []}))),
            Some(ResponseTemplate::new(503)),
            None,
        ] {
            let output = search_fixture(index_with_matches(), hub, None, false).await;
            assert!(output.contains("[official] official/report — Renders report summaries."));
            assert!(!output.contains("No skills found"));
            assert!(!output.contains("Skills Hub"));
            assert!(!output.contains("tap-backed"));
        }
    }

    #[tokio::test]
    async fn search_stops_after_a_match_from_the_legacy_hub_alone() {
        let output = search_fixture(
            ResponseTemplate::new(200).set_body_json(json!({"skills": []})),
            Some(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"name": "report-archive", "version": "1.2"}]
            }))),
            None,
            false,
        )
        .await;
        assert!(output.contains("Legacy Skills Hub matches:\n  • report-archive (v1.2)"));
    }

    #[tokio::test]
    async fn search_uses_skills_sh_after_an_empty_legacy_hub() {
        let hub = ResponseTemplate::new(200).set_body_json(json!({"results": []}));
        let fallback = ResponseTemplate::new(200).set_body_json(json!({
            "skills": [{"id": "fixture/catalogue/report-snapshot", "name": "Report snapshots"}]
        }));
        let output = search_fixture(
            ResponseTemplate::new(200).set_body_json(json!({"skills": []})),
            Some(hub),
            Some(fallback),
            false,
        )
        .await;
        assert!(output.contains(concat!(
            "No skills found matching \"report\".\n",
            "\nSkills.sh fallback matches:\n",
            "  • Report snapshots — skills.sh/fixture/catalogue/report-snapshot\n",
            "\nInstall with: hermes skills install skills.sh/<owner/repo/skill>\n",
        )));
        assert!(!output.contains("tap-backed"));
    }

    #[tokio::test]
    async fn search_reports_hub_diagnostics_when_no_registry_has_matches() {
        for (hub, diagnostic) in [
            (
                Some(ResponseTemplate::new(200).set_body_json(json!({"entries": []}))),
                "Unexpected response format from Skills Hub.",
            ),
            (
                Some(ResponseTemplate::new(503)),
                "Skills Hub returned status 503",
            ),
            (None, "Could not reach Skills Hub:"),
        ] {
            let output = search_fixture(
                ResponseTemplate::new(503),
                hub,
                Some(ResponseTemplate::new(200).set_body_json(json!({"skills": []}))),
                false,
            )
            .await;
            assert!(output.contains(diagnostic), "{output}");
            assert!(output.ends_with("No tap-backed matches found for \"report\".\n"));
        }
    }

    #[tokio::test]
    async fn search_uses_the_configured_tap_when_remote_results_are_unusable() {
        let output = search_fixture(
            ResponseTemplate::new(503),
            Some(ResponseTemplate::new(200).set_body_string("invalid JSON")),
            Some(ResponseTemplate::new(503)),
            true,
        )
        .await;
        assert!(output.ends_with(concat!(
            "\nTap-backed matches:\n",
            "  • report-review — fixture/reporting/skills/report-review\n",
            "\nInstall with: hermes skills install <name> or hermes skills install <owner/repo/path>\n",
        )));
    }
}
