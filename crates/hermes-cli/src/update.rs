//! Release version check against GitHub.

use hermes_core::AgentError;
use serde::Deserialize;

const DEFAULT_REPO: &str = "yungSNAT/reformed-agent";

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    name: Option<String>,
}

fn normalize_version(tag: &str) -> String {
    tag.trim().trim_start_matches('v').to_string()
}

/// Compare `tag_name` from GitHub with this binary's `CARGO_PKG_VERSION`.
pub async fn check_for_updates() -> Result<String, AgentError> {
    let repo = std::env::var("HERMES_UPDATE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo);

    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AgentError::Io(e.to_string()))?;

    check_release(client.get(&url), &repo).await
}

async fn check_release(request: reqwest::RequestBuilder, repo: &str) -> Result<String, AgentError> {
    let resp = request
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| AgentError::Io(format!("Update check HTTP error: {e}")))?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(format!(
            "No published GitHub release found for repository '{}'.\n\
             Override with HERMES_UPDATE_REPO if needed.",
            repo
        ));
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Ok(format!(
            "Could not reach GitHub releases for '{}': {} — {}\n\
             Set HERMES_UPDATE_REPO if your fork uses a different path.",
            repo, status, body
        ));
    }

    let rel: GitHubRelease = resp
        .json()
        .await
        .map_err(|e| AgentError::Io(format!("Update check parse error: {e}")))?;

    let remote = normalize_version(&rel.tag_name);
    let current = env!("CARGO_PKG_VERSION").to_string();

    if remote == current {
        Ok(format!(
            "Hermes v{} is up to date with latest GitHub release {} ({}).",
            current, rel.tag_name, rel.html_url
        ))
    } else {
        Ok(format!(
            "Hermes local version: {}\n\
             Latest GitHub release: {} ({}) — {}\n\
             See {} for release notes and install instructions.",
            current,
            rel.tag_name,
            rel.name.as_deref().unwrap_or(""),
            rel.html_url,
            rel.html_url
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn release_check_distinguishes_versions_and_github_failures() {
        let server = MockServer::start().await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let repo = "fixture/releases";
        let current = env!("CARGO_PKG_VERSION");
        let release_url = "https://example.invalid/release";
        for (route, response, expected) in [
            (
                "/current",
                ResponseTemplate::new(200).set_body_json(json!({
                    "tag_name": format!(" v{current} "),
                    "html_url": release_url,
                })),
                format!("Hermes v{current} is up to date"),
            ),
            (
                "/different",
                ResponseTemplate::new(200).set_body_json(json!({
                    "tag_name": "v999.0.0",
                    "name": "Preview",
                    "html_url": release_url,
                })),
                format!("Latest GitHub release: v999.0.0 (Preview) — {release_url}"),
            ),
            (
                "/missing",
                ResponseTemplate::new(404),
                format!("No published GitHub release found for repository '{repo}'"),
            ),
            (
                "/unavailable",
                ResponseTemplate::new(503).set_body_string("please retry"),
                "503 Service Unavailable — please retry".to_string(),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(route))
                .and(header("Accept", "application/vnd.github+json"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            let report = check_release(client.get(format!("{}{route}", server.uri())), repo)
                .await
                .unwrap();
            assert!(report.contains(&expected), "{route}: {report}");
        }

        Mock::given(method("GET"))
            .and(path("/invalid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tag_name": 1})))
            .expect(1)
            .mount(&server)
            .await;
        let invalid = client.get(format!("{}/invalid", server.uri()));
        for (request, context) in [
            (invalid, "Update check parse error:"),
            (client.get("not a URL"), "Update check HTTP error:"),
        ] {
            let error = check_release(request, repo).await.unwrap_err();
            assert!(matches!(error, AgentError::Io(message) if message.starts_with(context)));
        }
    }
}
