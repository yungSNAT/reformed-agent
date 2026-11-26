//! Website/domain blocklist policy for web-facing tools.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsitePolicyError {
    message: String,
}

impl WebsitePolicyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for WebsitePolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WebsitePolicyError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsiteBlockRule {
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsiteBlocklistPolicy {
    pub enabled: bool,
    pub rules: Vec<WebsiteBlockRule>,
}

impl WebsiteBlocklistPolicy {
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "rules": self.rules.iter().map(|rule| json!({"pattern": rule.pattern})).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsiteAccessBlock {
    pub url: String,
    pub host: String,
    pub rule: String,
}

impl WebsiteAccessBlock {
    pub fn to_json(&self) -> Value {
        json!({
            "url": self.url,
            "host": self.host,
            "rule": self.rule,
        })
    }
}

pub fn load_website_blocklist(
    config_path: Option<&Path>,
) -> Result<WebsiteBlocklistPolicy, WebsitePolicyError> {
    let path = config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(hermes_config::config_path);
    let user = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| WebsitePolicyError::new(format!("Failed to read config YAML: {e}")))?;
        let root: serde_yaml::Value = serde_yaml::from_str(&raw)
            .map_err(|e| WebsitePolicyError::new(format!("Invalid config YAML: {e}")))?;
        if !root.is_mapping() {
            return Err(WebsitePolicyError::new("config root must be a mapping"));
        }
        Some((raw, root))
    } else {
        None
    };
    // With the managed scope laid over it, as `load_config` reads it, so a
    // blocklist the administrator pins applies and the profile cannot lift it.
    let root = match hermes_config::managed_scope_dir() {
        Some(dir) => {
            // The overlay reads the profile as JSON, so a profile it cannot
            // read that way would lose its own rules.
            if let Some((raw, _)) = &user {
                if serde_yaml::from_str::<serde_json::Value>(raw).is_err() {
                    return Err(WebsitePolicyError::new(
                        "config YAML cannot be merged with the managed config",
                    ));
                }
            }
            // No fallback to the profile alone: the overlay was required.
            with_managed_config(&dir, || {
                hermes_config::load_effective_config_yaml_value(&path)
            })
            .map_err(WebsitePolicyError::new)?
            .map(serde_yaml::to_value)
            .transpose()
            .map_err(|e| WebsitePolicyError::new(e.to_string()))?
        }
        None => user.map(|(_, root)| root),
    };
    let Some(serde_yaml::Value::Mapping(root)) = root else {
        return Ok(WebsiteBlocklistPolicy {
            enabled: false,
            rules: Vec::new(),
        });
    };

    let Some(security) = mapping_get(&root, "security") else {
        return Ok(WebsiteBlocklistPolicy {
            enabled: false,
            rules: Vec::new(),
        });
    };
    let serde_yaml::Value::Mapping(security) = security else {
        return Err(WebsitePolicyError::new("security must be a mapping"));
    };
    let Some(blocklist) = mapping_get(security, "website_blocklist") else {
        return Ok(WebsiteBlocklistPolicy {
            enabled: false,
            rules: Vec::new(),
        });
    };
    let serde_yaml::Value::Mapping(blocklist) = blocklist else {
        return Err(WebsitePolicyError::new(
            "security.website_blocklist must be a mapping",
        ));
    };

    let enabled = match mapping_get(blocklist, "enabled") {
        Some(serde_yaml::Value::Bool(value)) => *value,
        Some(_) => {
            return Err(WebsitePolicyError::new(
                "security.website_blocklist.enabled must be a boolean",
            ))
        }
        None => false,
    };

    let mut rules = Vec::new();
    if let Some(domains) = mapping_get(blocklist, "domains") {
        let serde_yaml::Value::Sequence(domains) = domains else {
            return Err(WebsitePolicyError::new(
                "security.website_blocklist.domains must be a list",
            ));
        };
        for domain in domains {
            if let Some(rule) = domain.as_str().and_then(normalize_domain_rule) {
                rules.push(WebsiteBlockRule { pattern: rule });
            }
        }
    }

    if let Some(shared_files) = mapping_get(blocklist, "shared_files") {
        let serde_yaml::Value::Sequence(shared_files) = shared_files else {
            return Err(WebsitePolicyError::new(
                "security.website_blocklist.shared_files must be a list",
            ));
        };
        for shared in shared_files {
            let Some(path) = shared.as_str().map(PathBuf::from) else {
                continue;
            };
            match std::fs::read_to_string(&path) {
                Ok(raw) => {
                    for line in raw.lines() {
                        let line = line.split('#').next().unwrap_or("").trim();
                        if let Some(rule) = normalize_domain_rule(line) {
                            rules.push(WebsiteBlockRule { pattern: rule });
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        "Skipping unreadable website blocklist {}: {err}",
                        path.display()
                    );
                }
            }
        }
    }

    dedupe_rules(&mut rules);
    Ok(WebsiteBlocklistPolicy { enabled, rules })
}

pub fn check_website_access(
    url: &str,
    config_path: Option<&Path>,
) -> Result<Option<WebsiteAccessBlock>, WebsitePolicyError> {
    let policy = load_website_blocklist(config_path)?;
    Ok(check_website_access_with_policy(url, &policy))
}

pub fn check_website_access_with_policy(
    url: &str,
    policy: &WebsiteBlocklistPolicy,
) -> Option<WebsiteAccessBlock> {
    if !policy.enabled {
        return None;
    }
    let host = url_host(url)?;
    for rule in &policy.rules {
        if domain_rule_matches(&host, &rule.pattern) {
            return Some(WebsiteAccessBlock {
                url: url.to_string(),
                host,
                rule: rule.pattern.clone(),
            });
        }
    }
    None
}

/// Runs `read`, which lays the managed scope's `config.yaml` in `dir` over a profile's through
/// the shared loader. The loader skips a managed file it cannot use, with a warning; security
/// readers refuse instead, as it may pin a restriction, and refuse too when the file changed
/// during `read`, as the loader may then have skipped it.
pub(crate) fn with_managed_config<T>(dir: &Path, read: impl FnOnce() -> T) -> Result<T, String> {
    let before = usable_managed_config(dir)?;
    let value = read();
    if usable_managed_config(dir)? != before {
        return Err(format!(
            "{} changed while it was read",
            dir.join("config.yaml").display()
        ));
    }
    Ok(value)
}

/// The managed `config.yaml` in `dir` when the shared loader can lay it over a profile's, or
/// `None` when it names nothing (missing, empty or only comments).
fn usable_managed_config(dir: &Path) -> Result<Option<String>, String> {
    let path = dir.join("config.yaml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Failed to read {}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    match serde_yaml::from_str::<serde_yaml::Value>(&text) {
        Err(e) => Err(format!("Invalid YAML in {}: {e}", path.display())),
        Ok(serde_yaml::Value::Null) => Ok(None),
        Ok(root) if !root.is_mapping() => Err(format!("{} must be a mapping", path.display())),
        Ok(root) => match serde_json::to_value(root) {
            Ok(_) => Ok(Some(text)),
            Err(e) => Err(format!("{} cannot be read as JSON: {e}", path.display())),
        },
    }
}

fn mapping_get<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.get(serde_yaml::Value::String(key.to_string()))
}

fn normalize_domain_rule(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let host = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Url::parse(trimmed)
            .ok()
            .and_then(|url| url.host_str().map(ToString::to_string))?
    } else {
        trimmed
            .trim_start_matches("//")
            .split('/')
            .next()
            .unwrap_or(trimmed)
            .to_string()
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn url_host(input: &str) -> Option<String> {
    Url::parse(input)
        .ok()
        .and_then(|url| url.host_str().map(ToString::to_string))
        .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|host| !host.is_empty())
}

fn domain_rule_matches(host: &str, rule: &str) -> bool {
    if let Some(suffix) = rule.strip_prefix("*.") {
        return host.ends_with(&format!(".{suffix}")) && host != suffix;
    }
    host == rule || host.ends_with(&format!(".{rule}"))
}

fn dedupe_rules(rules: &mut Vec<WebsiteBlockRule>) {
    let mut seen = std::collections::BTreeSet::new();
    rules.retain(|rule| seen.insert(rule.pattern.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    #[test]
    fn load_website_blocklist_merges_config_and_shared_file() {
        let _env = test_lock::lock();
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("community-blocklist.txt");
        std::fs::write(&shared, "# comment\nexample.org\nsub.bad.net\n").unwrap();
        let config = tmp.path().join("config.yaml");
        std::fs::write(
            &config,
            format!(
                r#"
security:
  website_blocklist:
    enabled: true
    domains:
      - example.com
      - https://www.evil.test/path
    shared_files:
      - {}
"#,
                shared.display()
            ),
        )
        .unwrap();

        let policy = load_website_blocklist(Some(&config)).unwrap();
        assert!(policy.enabled);
        let rules = policy
            .rules
            .iter()
            .map(|rule| rule.pattern.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            rules,
            ["evil.test", "example.com", "example.org", "sub.bad.net"]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn check_website_access_matches_parent_domain_subdomains() {
        let policy = WebsiteBlocklistPolicy {
            enabled: true,
            rules: vec![WebsiteBlockRule {
                pattern: "example.com".to_string(),
            }],
        };
        let blocked =
            check_website_access_with_policy("https://docs.example.com/page", &policy).unwrap();
        assert_eq!(blocked.host, "docs.example.com");
        assert_eq!(blocked.rule, "example.com");
    }

    #[test]
    fn check_website_access_supports_wildcard_subdomains_only() {
        let policy = WebsiteBlocklistPolicy {
            enabled: true,
            rules: vec![WebsiteBlockRule {
                pattern: "*.tracking.example".to_string(),
            }],
        };
        assert!(check_website_access_with_policy("https://a.tracking.example", &policy).is_some());
        assert!(
            check_website_access_with_policy("https://www.tracking.example", &policy).is_some()
        );
        assert!(check_website_access_with_policy("https://tracking.example", &policy).is_none());
    }

    #[test]
    fn load_website_blocklist_uses_disabled_default_when_section_missing() {
        let _env = test_lock::lock();
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.yaml");
        std::fs::write(&config, "display:\n  tool_progress: all\n").unwrap();
        let policy = load_website_blocklist(Some(&config)).unwrap();
        assert!(!policy.enabled);
        assert!(policy.rules.is_empty());
    }

    #[test]
    fn load_website_blocklist_rejects_invalid_shapes_cleanly() {
        let _env = test_lock::lock();
        let tmp = tempfile::tempdir().unwrap();
        let cases = [
            ("[]", "config root must be a mapping"),
            ("security: []\n", "security must be a mapping"),
            (
                "security:\n  website_blocklist: block everything\n",
                "security.website_blocklist must be a mapping",
            ),
            (
                "security:\n  website_blocklist:\n    enabled: \"false\"\n",
                "security.website_blocklist.enabled must be a boolean",
            ),
            (
                "security:\n  website_blocklist:\n    domains: example.com\n",
                "security.website_blocklist.domains must be a list",
            ),
            (
                "security:\n  website_blocklist:\n    shared_files: community.txt\n",
                "security.website_blocklist.shared_files must be a list",
            ),
        ];
        for (idx, (yaml, message)) in cases.iter().enumerate() {
            let config = tmp.path().join(format!("case-{idx}.yaml"));
            std::fs::write(&config, yaml).unwrap();
            let err = load_website_blocklist(Some(&config)).unwrap_err();
            assert!(err.to_string().contains(message), "{yaml:?} produced {err}");
        }
    }

    #[test]
    fn load_website_blocklist_rejects_malformed_yaml() {
        let _env = test_lock::lock();
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.yaml");
        std::fs::write(&config, "security: [oops\n").unwrap();
        let err = load_website_blocklist(Some(&config)).unwrap_err();
        assert!(err.to_string().contains("Invalid config YAML"));
    }

    /// Points `HERMES_MANAGED_DIR` at a folder until dropped. Holders keep `test_lock`.
    struct ManagedDir(Option<std::ffi::OsString>);

    impl ManagedDir {
        fn set(dir: &Path) -> Self {
            let original = std::env::var_os("HERMES_MANAGED_DIR");
            std::env::set_var("HERMES_MANAGED_DIR", dir);
            Self(original)
        }
    }

    impl Drop for ManagedDir {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("HERMES_MANAGED_DIR", value),
                None => std::env::remove_var("HERMES_MANAGED_DIR"),
            }
        }
    }

    #[test]
    fn load_website_blocklist_honours_the_managed_scope() {
        let _env = test_lock::lock();
        let home = tempfile::tempdir().unwrap();
        let managed = tempfile::tempdir().unwrap();
        std::fs::write(
            managed.path().join("config.yaml"),
            "security:\n  website_blocklist:\n    enabled: true\n    domains:\n      - managed-only.invalid\n",
        )
        .unwrap();
        let _managed = ManagedDir::set(managed.path());
        let config = home.path().join("config.yaml");

        // A blocklist only the administrator sets applies with no profile config.
        let policy = load_website_blocklist(Some(&config)).unwrap();
        assert!(policy.enabled);
        assert!(
            check_website_access_with_policy("https://managed-only.invalid/", &policy).is_some()
        );

        // The profile cannot switch it off.
        std::fs::write(
            &config,
            "security:\n  website_blocklist:\n    enabled: false\n    domains: []\n",
        )
        .unwrap();
        let policy = load_website_blocklist(Some(&config)).unwrap();
        assert!(policy.enabled);
        assert!(
            check_website_access_with_policy("https://managed-only.invalid/", &policy).is_some()
        );
    }

    #[test]
    fn load_website_blocklist_refuses_a_managed_config_it_cannot_use() {
        let _env = test_lock::lock();
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.yaml");
        std::fs::write(
            &config,
            "security:\n  website_blocklist:\n    enabled: false\n",
        )
        .unwrap();
        for managed_config in ["security: [unclosed\n", "- a list\n"] {
            let managed = tempfile::tempdir().unwrap();
            std::fs::write(managed.path().join("config.yaml"), managed_config).unwrap();
            let _managed = ManagedDir::set(managed.path());
            assert!(
                load_website_blocklist(Some(&config)).is_err(),
                "{managed_config:?} was used"
            );
        }
        let managed = tempfile::tempdir().unwrap();
        std::fs::create_dir(managed.path().join("config.yaml")).unwrap();
        let _managed = ManagedDir::set(managed.path());
        assert!(load_website_blocklist(Some(&config)).is_err());

        // One holding only comments names nothing, as the shared loader reads it.
        let managed = tempfile::tempdir().unwrap();
        std::fs::write(managed.path().join("config.yaml"), "# none yet\n").unwrap();
        let _managed = ManagedDir::set(managed.path());
        assert!(!load_website_blocklist(Some(&config)).unwrap().enabled);
    }

    #[test]
    fn a_managed_config_that_changes_while_it_is_read_refuses() {
        let managed = tempfile::tempdir().unwrap();
        let file = managed.path().join("config.yaml");
        std::fs::write(&file, "security: {}\n").unwrap();
        assert_eq!(with_managed_config(managed.path(), || 1), Ok(1));
        for changed in [
            "security: [unclosed\n",
            "security:\n  website_blocklist:\n    enabled: true\n",
        ] {
            std::fs::write(&file, "security: {}\n").unwrap();
            let read = with_managed_config(managed.path(), || std::fs::write(&file, changed));
            assert!(read.is_err(), "{changed:?} went unnoticed");
        }
    }

    #[test]
    fn load_website_blocklist_never_drops_profile_rules_under_a_managed_scope() {
        let _env = test_lock::lock();
        let home = tempfile::tempdir().unwrap();
        let managed = tempfile::tempdir().unwrap();
        std::fs::write(
            managed.path().join("config.yaml"),
            "display:\n  tool_progress: all\n",
        )
        .unwrap();
        let config = home.path().join("config.yaml");
        // A sequence key parses as YAML but cannot be read as JSON.
        std::fs::write(
            &config,
            "? [a, b]\n: x\nsecurity:\n  website_blocklist:\n    enabled: true\n    domains: [profile-only.invalid]\n",
        )
        .unwrap();
        let policy = load_website_blocklist(Some(&config)).unwrap();
        assert!(
            policy.enabled,
            "the profile's rules apply without a managed scope"
        );

        let _managed = ManagedDir::set(managed.path());
        assert!(load_website_blocklist(Some(&config)).is_err());
    }
}
