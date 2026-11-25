// Tests are nested in `loader::user_config_patch::coverage_gen` so the test filter
// `loader::user_config_patch::coverage_gen::` matches them, whatever the actual
// parent module path is.
mod loader {
    mod user_config_patch {
        mod coverage_gen {
            use super::super::super::super::*;

            fn set(config: &mut GatewayConfig, key: &str, value: &str) {
                assert!(
                    apply_user_config_patch(config, key, value).is_ok(),
                    "expected {key}={value} to apply"
                );
            }

            fn get(config: &GatewayConfig, key: &str) -> String {
                match user_config_field_display(config, key) {
                    Ok(s) => s,
                    Err(_) => panic!("expected display of {key} to succeed"),
                }
            }

            fn validation_msg(result: Result<(), ConfigError>) -> String {
                match result {
                    Err(ConfigError::ValidationError(m)) => m,
                    _ => panic!("expected a validation error"),
                }
            }

            fn not_found_msg(result: Result<(), ConfigError>) -> String {
                match result {
                    Err(ConfigError::NotFound(m)) => m,
                    _ => panic!("expected a not found error"),
                }
            }

            fn display_not_found_msg(result: Result<String, ConfigError>) -> String {
                match result {
                    Err(ConfigError::NotFound(m)) => m,
                    _ => panic!("expected a not found error"),
                }
            }

            #[test]
            fn mask_secret_short_and_empty() {
                assert_eq!(mask_secret(""), "(empty)");
                assert_eq!(mask_secret("abc"), "***");
                assert_eq!(mask_secret("abcd"), "***");
                assert_eq!(mask_secret("abcdefgh"), "***efgh");
            }

            #[test]
            fn flat_keys_apply_and_display() {
                let mut c = GatewayConfig {
                    personality: None,
                    system_prompt: None,
                    prefill_messages_file: None,
                    ..GatewayConfig::default()
                };
                assert_eq!(get(&c, "personality"), "(not set)");
                assert_eq!(get(&c, "system_prompt"), "(not set)");
                assert_eq!(get(&c, "prefill_messages_file"), "(not set)");

                set(&mut c, "personality", "pirate");
                assert_eq!(c.personality.as_deref(), Some("pirate"));
                assert_eq!(get(&c, "personality"), "pirate");

                set(&mut c, "max_turns", "7");
                assert_eq!(get(&c, "max_turns"), "7");
                let msg = validation_msg(apply_user_config_patch(&mut c, "max_turns", "-x"));
                assert!(msg.contains("max_turns must be a non-negative integer: -x"));
                assert_eq!(get(&c, "max_turns"), "7");

                set(&mut c, "system_prompt", "be nice");
                assert_eq!(c.system_prompt.as_deref(), Some("be nice"));
                assert_eq!(get(&c, "system_prompt"), "be nice");

                set(&mut c, "prefill_messages_file", "/tmp/prefill.json");
                assert_eq!(
                    c.prefill_messages_file.as_deref(),
                    Some("/tmp/prefill.json")
                );
                assert_eq!(get(&c, "prefill_messages_file"), "/tmp/prefill.json");

                let msg = not_found_msg(apply_user_config_patch(&mut c, "bogus", "1"));
                assert!(msg.contains("unknown config key: bogus"));
                assert!(msg.contains("supported:"));
                let msg = display_not_found_msg(user_config_field_display(&c, "bogus"));
                assert!(msg.contains("unknown config key: bogus"));
            }

            #[test]
            fn budget_errors_and_display() {
                let mut c = GatewayConfig::default();
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "budget.max_result_size_chars",
                    "abc",
                ));
                assert!(msg.contains("budget.max_result_size_chars must be a usize: abc"));
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "budget.max_aggregate_chars",
                    "-1",
                ));
                assert!(msg.contains("budget.max_aggregate_chars must be a usize: -1"));
                set(&mut c, "budget.max_aggregate_chars", "12345");
                assert_eq!(get(&c, "budget.max_aggregate_chars"), "12345");
            }

            #[test]
            fn proxy_set_and_display() {
                let mut c = GatewayConfig {
                    proxy: None,
                    ..GatewayConfig::default()
                };
                assert_eq!(get(&c, "proxy.http"), "(not set)");
                assert_eq!(get(&c, "proxy.socks"), "(not set)");
                set(&mut c, "proxy.socks", "socks5://127.0.0.1:1080");
                assert_eq!(get(&c, "proxy.socks_proxy"), "socks5://127.0.0.1:1080");
                assert_eq!(get(&c, "proxy.http_proxy"), "(not set)");
                set(&mut c, "proxy.socks_proxy", "socks5://h:2");
                assert_eq!(get(&c, "proxy.socks"), "socks5://h:2");
                set(&mut c, "proxy.http", "http://p:8080");
                assert_eq!(get(&c, "proxy.http"), "http://p:8080");
            }

            #[test]
            fn security_allow_private_urls() {
                let mut c = GatewayConfig::default();
                set(&mut c, "security.allow_private_urls", " Yes ");
                assert!(c.security.allow_private_urls);
                set(&mut c, "security.allow_private_urls", "off");
                assert!(!c.security.allow_private_urls);
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "security.allow_private_urls",
                    "maybe",
                ));
                assert!(msg.contains("security.allow_private_urls must be a boolean: maybe"));
                assert!(!c.security.allow_private_urls);
            }

            #[test]
            fn web_backends_set_and_display() {
                let mut c = GatewayConfig::default();
                for field in [
                    "backend",
                    "search_backend",
                    "extract_backend",
                    "crawl_backend",
                ] {
                    let key = format!("web.{field}");
                    set(&mut c, &key, "  tavily  ");
                    assert_eq!(get(&c, &key), "tavily");
                    set(&mut c, &key, "   ");
                    assert_eq!(get(&c, &key), "(not set)");
                }
                set(&mut c, "web.backend", "exa");
                assert_eq!(c.web.backend, "exa");
                set(&mut c, "web.search_backend", "brave");
                assert_eq!(c.web.search_backend, "brave");
                set(&mut c, "web.extract_backend", "firecrawl");
                assert_eq!(c.web.extract_backend, "firecrawl");
                set(&mut c, "web.crawl_backend", "jina");
                assert_eq!(c.web.crawl_backend, "jina");
            }

            #[test]
            fn display_section_set_and_display() {
                let mut c = GatewayConfig::default();
                set(&mut c, "display.busy_ack_enabled", "false");
                assert_eq!(c.display.busy_ack_enabled, Some(false));
                assert_eq!(get(&c, "display.busy_ack_enabled"), "false");
                set(&mut c, "display.busy_ack_enabled", "true");
                assert_eq!(get(&c, "display.busy_ack_enabled"), "true");
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "display.busy_ack_enabled",
                    "nah",
                ));
                assert!(msg.contains("display.busy_ack_enabled must be a boolean: nah"));

                c.display.busy_input_mode = None;
                assert_eq!(get(&c, "display.busy_input_mode"), "interrupt");

                set(&mut c, "display.busy_input_mode", "Queued");
                assert_eq!(c.display.busy_input_mode.as_deref(), Some("queue"));
                assert_eq!(get(&c, "display.busy_input_mode"), "queue");
                set(&mut c, "display.busy_input_mode", "steering");
                assert_eq!(get(&c, "display.busy_input_mode"), "steer");
                set(&mut c, "display.busy_input_mode", "");
                assert_eq!(c.display.busy_input_mode.as_deref(), Some("interrupt"));
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "display.busy_input_mode",
                    "wild",
                ));
                assert!(msg.contains(
                    "display.busy_input_mode must be one of interrupt, queue, steer: wild"
                ));

                set(&mut c, "display.memory_notifications", "no");
                assert_eq!(c.display.memory_notifications, Some(false));
                assert_eq!(get(&c, "display.memory_notifications"), "false");
                set(&mut c, "display.memory_notifications", "1");
                assert_eq!(get(&c, "display.memory_notifications"), "true");

                set(&mut c, "display.friendly_tool_labels", "off");
                assert!(!c.display.friendly_tool_labels);
                assert_eq!(get(&c, "display.friendly_tool_labels"), "false");
                set(&mut c, "display.friendly_tool_labels", "on");
                assert!(c.display.friendly_tool_labels);
                assert_eq!(get(&c, "display.friendly_tool_labels"), "true");
            }

            #[test]
            fn sessions_set_errors_and_display() {
                let mut c = GatewayConfig::default();
                set(&mut c, "sessions.auto_prune", "true");
                assert!(c.sessions.auto_prune);
                set(&mut c, "sessions.auto_prune", "no");
                assert!(!c.sessions.auto_prune);
                let msg =
                    validation_msg(apply_user_config_patch(&mut c, "sessions.auto_prune", "x"));
                assert!(msg.contains("sessions.auto_prune must be a boolean: x"));

                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "sessions.retention_days",
                    "ten",
                ));
                assert!(msg.contains("sessions.retention_days must be a non-negative integer: ten"));

                set(&mut c, "sessions.vacuum_after_prune", "yes");
                assert_eq!(get(&c, "sessions.vacuum_after_prune"), "true");
                set(&mut c, "sessions.vacuum_after_prune", "0");
                assert_eq!(get(&c, "sessions.vacuum_after_prune"), "false");
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "sessions.vacuum_after_prune",
                    "perhaps",
                ));
                assert!(msg.contains("sessions.vacuum_after_prune must be a boolean: perhaps"));

                set(&mut c, "sessions.min_interval_hours", "6");
                assert_eq!(get(&c, "sessions.min_interval_hours"), "6");
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "sessions.min_interval_hours",
                    "-2",
                ));
                assert!(
                    msg.contains("sessions.min_interval_hours must be a non-negative integer: -2")
                );
                assert_eq!(get(&c, "sessions.min_interval_hours"), "6");
            }

            #[test]
            fn model_switch_alias_set_and_display() {
                let mut c = GatewayConfig::default();
                set(&mut c, "model.persist_switch_by_default", "yes");
                assert!(c.model_switch.persist_switch_by_default);
                assert_eq!(get(&c, "model.persist_switch_by_default"), "true");
                set(&mut c, "model_switch.persist_switch_by_default", "no");
                assert_eq!(get(&c, "model.persist_switch_by_default"), "false");
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "model.persist_switch_by_default",
                    "zzz",
                ));
                assert!(
                    msg.contains("model_switch.persist_switch_by_default must be a boolean: zzz")
                );
            }

            #[test]
            fn auxiliary_errors_and_display() {
                let mut c = GatewayConfig::default();
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "auxiliary.covtask.timeout",
                    "soon",
                ));
                assert!(
                    msg.contains("auxiliary.covtask.timeout must be a non-negative integer: soon")
                );
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "auxiliary.covtask.timeout_secs",
                    "-1",
                ));
                assert!(msg
                    .contains("auxiliary.covtask.timeout_secs must be a non-negative integer: -1"));
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "auxiliary.covtask.download_timeout",
                    "later",
                ));
                assert!(msg.contains(
                    "auxiliary.covtask.download_timeout must be a non-negative integer: later"
                ));
                let msg = not_found_msg(apply_user_config_patch(
                    &mut c,
                    "auxiliary.covtask.colour",
                    "red",
                ));
                assert!(msg.contains("unknown auxiliary field: auxiliary.covtask.colour"));

                assert_eq!(get(&c, "auxiliary.covtask.download_timeout"), "(not set)");
                assert_eq!(
                    get(&c, "auxiliary.nonexistent_cov.download_timeout"),
                    "(not set)"
                );
                set(&mut c, "auxiliary.covtask.download_timeout", "45");
                assert_eq!(get(&c, "auxiliary.covtask.download_timeout"), "45");

                assert_eq!(get(&c, "auxiliary.nonexistent_cov.base_url"), "(not set)");
                set(
                    &mut c,
                    "auxiliary.covtask.base_url",
                    "  https://aux.example  ",
                );
                assert_eq!(get(&c, "auxiliary.covtask.base_url"), "https://aux.example");
            }

            #[test]
            fn llm_errors_and_display() {
                let mut c = GatewayConfig::default();
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "llm.covprov.max_tokens",
                    "lots",
                ));
                assert!(msg.contains("llm.covprov.max_tokens must be a positive integer: lots"));
                let msg =
                    not_found_msg(apply_user_config_patch(&mut c, "llm.covprov.flavour", "x"));
                assert!(msg.contains("unknown llm field: llm.covprov.flavour"));

                assert_eq!(get(&c, "llm.nonexistent_cov.api_key"), "(not set)");
                assert_eq!(get(&c, "llm.nonexistent_cov.base_url"), "(not set)");
                assert_eq!(get(&c, "llm.nonexistent_cov.model"), "(not set)");
                assert_eq!(get(&c, "llm.nonexistent_cov.oauth_token_url"), "(not set)");
                assert_eq!(get(&c, "llm.nonexistent_cov.oauth_client_id"), "(not set)");

                set(
                    &mut c,
                    "llm.covprov.oauth_token_url",
                    "https://auth.example/token",
                );
                set(&mut c, "llm.covprov.oauth_client_id", "client-123");
                set(&mut c, "llm.covprov.base_url", "https://api.example/v1");
                set(&mut c, "llm.covprov.model", "big-model");
                set(&mut c, "llm.covprov.api_key", concat!("sk-", "secret-abcd"));

                let entry = c.llm_providers.get("covprov").expect("provider entry");
                assert_eq!(
                    entry.oauth_token_url.as_deref(),
                    Some("https://auth.example/token")
                );
                assert_eq!(entry.oauth_client_id.as_deref(), Some("client-123"));

                assert_eq!(
                    get(&c, "llm.covprov.oauth_token_url"),
                    "https://auth.example/token"
                );
                assert_eq!(get(&c, "llm.covprov.oauth_client_id"), "client-123");
                assert_eq!(get(&c, "llm.covprov.base_url"), "https://api.example/v1");
                assert_eq!(get(&c, "llm.covprov.model"), "big-model");
                assert_eq!(get(&c, "llm.covprov.api_key"), "***abcd");
            }

            #[test]
            fn smart_model_routing_set_errors_and_display() {
                let mut c = GatewayConfig::default();
                set(&mut c, "smart_model_routing.enabled", "on");
                assert!(c.smart_model_routing.enabled);
                assert_eq!(get(&c, "smart_model_routing.enabled"), "true");
                set(&mut c, "smart_model_routing.enabled", "false");
                assert_eq!(get(&c, "smart_model_routing.enabled"), "false");
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "smart_model_routing.enabled",
                    "kinda",
                ));
                assert!(msg.contains("smart_model_routing.enabled must be a boolean: kinda"));

                set(&mut c, "smart_model_routing.max_simple_chars", "160");
                assert_eq!(get(&c, "smart_model_routing.max_simple_chars"), "160");
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "smart_model_routing.max_simple_chars",
                    "many",
                ));
                assert!(msg.contains("smart_model_routing.max_simple_chars must be a usize: many"));

                set(&mut c, "smart_model_routing.max_simple_words", "28");
                assert_eq!(get(&c, "smart_model_routing.max_simple_words"), "28");
                let msg = validation_msg(apply_user_config_patch(
                    &mut c,
                    "smart_model_routing.max_simple_words",
                    "-3",
                ));
                assert!(msg.contains("smart_model_routing.max_simple_words must be a usize: -3"));

                c.smart_model_routing.cheap_model = None;
                assert_eq!(
                    get(&c, "smart_model_routing.cheap_model.model"),
                    "(not set)"
                );
                assert_eq!(
                    get(&c, "smart_model_routing.cheap_model.provider"),
                    "(not set)"
                );

                set(&mut c, "smart_model_routing.cheap_model.model", "tiny");
                set(
                    &mut c,
                    "smart_model_routing.cheap_model.provider",
                    "cheapco",
                );
                let cheap = c
                    .smart_model_routing
                    .cheap_model
                    .as_ref()
                    .expect("cheap model set");
                assert_eq!(cheap.model.as_deref(), Some("tiny"));
                assert_eq!(cheap.provider.as_deref(), Some("cheapco"));
                assert_eq!(get(&c, "smart_model_routing.cheap_model.model"), "tiny");
                assert_eq!(
                    get(&c, "smart_model_routing.cheap_model.provider"),
                    "cheapco"
                );
            }

            #[test]
            fn unknown_dotted_keys_are_not_found() {
                let mut c = GatewayConfig::default();
                let msg = not_found_msg(apply_user_config_patch(&mut c, "foo.bar", "1"));
                assert!(msg.contains("unknown config key: foo.bar"));
                let msg = not_found_msg(apply_user_config_patch(&mut c, "llm.only_two", "1"));
                assert!(msg.contains("unknown config key: llm.only_two"));
                let msg = display_not_found_msg(user_config_field_display(&c, "foo.bar"));
                assert!(msg.contains("unknown config key: foo.bar"));
                assert!(msg.contains("supported:"));
            }
        }
    }
}
