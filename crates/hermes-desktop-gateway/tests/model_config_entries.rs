//! Provider entry selection across case siblings and aliases, and `llm.oneshot` key updates.

mod model_support;

use model_support::*;
use serde_json::{json, Value};

async fn save_key(client: &mut Client, slug: &str, api_key: &str) -> Value {
    let reply = client
        .call(
            3,
            "model.save_key",
            json!({"slug": slug, "api_key": api_key}),
        )
        .await;
    assert!(reply.get("error").is_none(), "{reply}");
    reply
}

async fn disconnect(client: &mut Client, slug: &str) {
    let reply = client
        .call(4, "model.disconnect", json!({"slug": slug}))
        .await;
    assert_eq!(reply["result"]["disconnected"], true, "{reply}");
}

async fn turn_on(client: &mut Client, session: &str, seen: &Seen, model: &str) -> String {
    let before = seen.lock().unwrap().len();
    let reply = client
        .call(
            5,
            "config.set",
            json!({"session_id": session, "key": "model", "value": model}),
        )
        .await;
    assert!(reply.get("error").is_none(), "{reply}");
    client.turn(session).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), before + 1, "turn on {model} did not hit model");
    seen.last().unwrap().1.clone()
}

/// The `Authorization` header a turn on `model` sends after `model.save_key` for `slug`, and the
/// slug of the row the save answers, with `llm_providers` `entries` (name, key variable), each
/// pointed at the scripted model.
async fn sent_after_save(
    entries: &[(&str, Option<&str>)],
    model: &str,
    slug: &str,
) -> (String, Value) {
    let (_home, mut client, seen) = start_providers(model, entries).await;
    let session = client.create().await;
    let reply = save_key(&mut client, slug, "saved-key").await;
    client.turn(&session).await;
    let sent = auth_sent(&seen).pop().unwrap_or_default();
    (sent, reply["result"]["provider"]["slug"].clone())
}

async fn assert_turns(client: &mut Client, session: &str, seen: &Seen, expected: &[(&str, &str)]) {
    for &(model, auth) in expected {
        assert_eq!(turn_on(client, session, seen, model).await, auth, "{model}");
    }
}

fn assert_env(home: &std::path::Path, present: &[&str], absent: &[&str]) {
    let env = env_lines(home);
    for line in present {
        assert!(
            env.contains(&format!("{line}\n")),
            "missing {line} in:\n{env}"
        );
    }
    for var in absent {
        assert!(!env.contains(var), "unexpected {var} in:\n{env}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_key_goes_to_the_entry_named_as_the_slug_first() {
    assert!(
        std::env::var_os("XIAOMI_API_KEY").is_none(),
        "this test needs XIAOMI_API_KEY unset"
    );
    const EXACT_KEY: &str = "DESKTOP_MODEL_CONFIG_EXACT_KEY";
    const CANONICAL_KEY: &str = "DESKTOP_MODEL_CONFIG_CANONICAL_KEY";
    let entries = [("xiaomi", Some(CANONICAL_KEY)), ("mimo", Some(EXACT_KEY))];
    let canonical_line = format!("{CANONICAL_KEY}=canonical-key");
    let exact_line = format!("{EXACT_KEY}=exact-key");
    let exact_line_2 = format!("{EXACT_KEY}=exact-key-2");
    // `llm_providers` is a `HashMap`: each new map iterates in its own order.
    for _ in 0..8 {
        let (home, mut client, seen) = start_providers("mimo:m", &entries).await;
        let session = client.create().await;

        for (slug, key, var) in [
            ("xiaomi", "canonical-key", CANONICAL_KEY),
            ("mimo", "exact-key", EXACT_KEY),
        ] {
            let saved = save_key(&mut client, slug, key).await;
            assert_eq!(saved["result"]["provider"]["slug"], slug);
            assert_eq!(saved["result"]["provider"]["key_env"], var);
        }
        assert_env(home.path(), &[&canonical_line, &exact_line], &[]);
        assert_turns(
            &mut client,
            &session,
            &seen,
            &[
                ("mimo:m", "Bearer exact-key"),
                ("xiaomi:m", "Bearer canonical-key"),
            ],
        )
        .await;

        // Disconnecting `mimo` clears only `mimo`'s key: `xiaomi`'s `.env` line and turns stay.
        disconnect(&mut client, "mimo").await;
        assert_env(home.path(), &[&canonical_line], &[EXACT_KEY]);
        assert_turns(
            &mut client,
            &session,
            &seen,
            &[("xiaomi:m", "Bearer canonical-key"), ("mimo:m", "")],
        )
        .await;

        // And disconnecting `xiaomi` leaves `mimo`'s key alone.
        save_key(&mut client, "mimo", "exact-key-2").await;
        disconnect(&mut client, "xiaomi").await;
        assert_env(home.path(), &[&exact_line_2], &[CANONICAL_KEY]);
        assert_turns(
            &mut client,
            &session,
            &seen,
            &[("mimo:m", "Bearer exact-key-2"), ("xiaomi:m", "")],
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_never_takes_the_key_variable_of_one_named_so_in_another_case() {
    assert!(
        std::env::var_os("XIAOMI_API_KEY").is_none(),
        "this test needs XIAOMI_API_KEY unset"
    );
    const UPPER_MIMO_KEY: &str = "DESKTOP_MODEL_CONFIG_SIBLING_MIMO_KEY";
    const UPPER_XIAOMI_KEY: &str = "DESKTOP_MODEL_CONFIG_SIBLING_UPPER_XIAOMI_KEY";
    const LOWER_XIAOMI_KEY: &str = "DESKTOP_MODEL_CONFIG_SIBLING_LOWER_XIAOMI_KEY";
    let entries = [
        ("mimo", None),
        ("MIMO", Some(UPPER_MIMO_KEY)),
        ("XIAOMI", Some(UPPER_XIAOMI_KEY)),
        ("xiaomi", Some(LOWER_XIAOMI_KEY)),
    ];
    let upper_mimo_line = format!("{UPPER_MIMO_KEY}=upper-mimo-key");
    let upper_xiaomi_line = format!("{UPPER_XIAOMI_KEY}=upper-xiaomi-key");
    let lower_xiaomi_line = format!("{LOWER_XIAOMI_KEY}=lower-xiaomi-key");
    let registry_line = "XIAOMI_API_KEY=registry-mimo-key";
    // `llm_providers` is a `HashMap`: each new map iterates in its own order.
    for _ in 0..8 {
        let (home, mut client, seen) = start_providers("mimo:m", &entries).await;
        let session = client.create().await;

        for (slug, key, var) in [
            ("MIMO", "upper-mimo-key", Some(UPPER_MIMO_KEY)),
            ("XIAOMI", "upper-xiaomi-key", Some(UPPER_XIAOMI_KEY)),
            ("xiaomi", "lower-xiaomi-key", Some(LOWER_XIAOMI_KEY)),
            // `mimo` names no variable: it writes to the registry's `XIAOMI_API_KEY`, never a sibling's.
            ("mimo", "registry-mimo-key", None),
        ] {
            let saved = save_key(&mut client, slug, key).await;
            assert_eq!(saved["result"]["provider"]["slug"], slug);
            if let Some(var) = var {
                assert_eq!(saved["result"]["provider"]["key_env"], var);
            }
        }
        assert_env(
            home.path(),
            &[
                &upper_mimo_line,
                &upper_xiaomi_line,
                &lower_xiaomi_line,
                registry_line,
            ],
            &[],
        );
        assert_turns(
            &mut client,
            &session,
            &seen,
            &[
                ("mimo:m", "Bearer registry-mimo-key"),
                ("MIMO:m", "Bearer upper-mimo-key"),
                ("XIAOMI:m", "Bearer upper-xiaomi-key"),
                ("xiaomi:m", "Bearer lower-xiaomi-key"),
            ],
        )
        .await;

        // Disconnecting `mimo` removes only `XIAOMI_API_KEY`; every sibling's `.env` line and
        // turn header stay untouched.
        disconnect(&mut client, "mimo").await;
        assert_env(
            home.path(),
            &[&upper_mimo_line, &upper_xiaomi_line, &lower_xiaomi_line],
            &["XIAOMI_API_KEY"],
        );
        assert_turns(
            &mut client,
            &session,
            &seen,
            &[
                ("mimo:m", ""),
                ("MIMO:m", "Bearer upper-mimo-key"),
                ("XIAOMI:m", "Bearer upper-xiaomi-key"),
                ("xiaomi:m", "Bearer lower-xiaomi-key"),
            ],
        )
        .await;

        // Disconnecting `MIMO` leaves `XIAOMI` and `xiaomi` untouched.
        disconnect(&mut client, "MIMO").await;
        assert_env(
            home.path(),
            &[&upper_xiaomi_line, &lower_xiaomi_line],
            &[UPPER_MIMO_KEY],
        );
        assert_turns(
            &mut client,
            &session,
            &seen,
            &[
                ("MIMO:m", ""),
                ("XIAOMI:m", "Bearer upper-xiaomi-key"),
                ("xiaomi:m", "Bearer lower-xiaomi-key"),
            ],
        )
        .await;

        // Disconnecting `XIAOMI` leaves `xiaomi` untouched.
        disconnect(&mut client, "XIAOMI").await;
        assert_env(home.path(), &[&lower_xiaomi_line], &[UPPER_XIAOMI_KEY]);
        assert_turns(
            &mut client,
            &session,
            &seen,
            &[("XIAOMI:m", ""), ("xiaomi:m", "Bearer lower-xiaomi-key")],
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_sends_the_key_saved_for_its_provider_whichever_entry_spelling_it_matches() {
    let (a, b) = (
        Some("DESKTOP_MODEL_CONFIG_SPELLING_A_KEY"),
        Some("DESKTOP_MODEL_CONFIG_SPELLING_B_KEY"),
    );
    // No entry spelt as the provider: the build reads the runtime name's (`mimo` runs as
    // `xiaomi`); an entry with no key variable reads the registry's, never another entry's.
    // The save answers the row of the entry the key went to.
    for (entries, model, row) in [
        ([("mimo", a), ("xiaomi", b)], "MIMO:m", "xiaomi"),
        ([("MIMO", a), ("xiaomi", b)], "mimo:m", "xiaomi"),
        ([("mimo", None), ("xiaomi", b)], "mimo:m", "mimo"),
    ] {
        let (slug, _) = model.split_once(':').unwrap();
        let sent = sent_after_save(&entries, model, slug).await;
        assert_eq!(
            sent,
            ("Bearer saved-key".into(), json!(row)),
            "{entries:?} {model}"
        );
    }
    // Only other spellings: the same one every time, though each new map iterates in its own
    // order.
    for _ in 0..32 {
        let sent = sent_after_save(&[("Mimo", a), ("MIMO", b)], "mimo:m", "mimo").await;
        assert_eq!(sent, ("Bearer saved-key".into(), json!("MIMO")));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn llm_oneshot_sends_the_key_a_turn_would_and_none_once_disconnected() {
    // The scripted model answers in SSE, which `llm.oneshot` cannot parse, so its replies are
    // errors by design: only the headers the model saw matter.
    let (_home, mut client, seen) = start_providers(
        "xiaomi:m",
        &[("xiaomi", Some("DESKTOP_MODEL_CONFIG_ONESHOT_KEY"))],
    )
    .await;
    let oneshot = json!({"input": "hello"});

    save_key(&mut client, "xiaomi", "saved-key").await;
    client.call(2, "llm.oneshot", oneshot.clone()).await;
    disconnect(&mut client, "xiaomi").await;
    client.call(4, "llm.oneshot", oneshot).await;
    // As a turn's: the key just saved, then none once it is removed.
    assert_eq!(auth_sent(&seen), ["Bearer saved-key", ""]);
}
