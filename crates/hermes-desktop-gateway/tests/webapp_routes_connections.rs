//! The saved gateways and relay routes through the full local runtime: who may call them, and
//! that a signed-in Webapp tab reaches them.
#![cfg(unix)]

mod webapp_support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::{json, Value};
use webapp_support::*;

/// Every route in the group, with a method the page uses.
const ROUTES: [(&str, &str); 6] = [
    ("GET", "/api/gateway-connections"),
    ("POST", "/api/gateway-connections"),
    ("POST", "/api/gateway-connections/last-used"),
    ("DELETE", "/api/gateway-connections/remote-1"),
    ("GET", "/api/relay/remote-1/api/status"),
    ("POST", "/api/relay/remote-1/api/sessions"),
];

const STORE: &str = "gateway-connections.json";

/// Neither the store nor its lock exists: no route wrote or locked it.
fn untouched(home: &Path) {
    assert!(!home.join(STORE).exists(), "the store was written");
    assert!(
        !home.join(format!("{STORE}.lock")).exists(),
        "the store was locked"
    );
}

/// An owner-only store holding the saved gateway `remote-1`, last used, so each route without its
/// guard would answer otherwise: list it, remove it, relay to it. Returns the bytes written.
fn saved_gateway(home: &Path) -> String {
    let doc = json!({
        "version": 2,
        "revision": 1,
        "lastUsed": "remote-1",
        "connections": [{
            "id": "remote-1",
            "kind": "remote",
            "label": "Remote",
            "url": "https://remote.invalid",
            "token": "secret-token-123456",
        }],
    })
    .to_string();
    let path = home.join(STORE);
    std::fs::write(&path, &doc).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    doc
}

#[tokio::test(flavor = "multi_thread")]
async fn every_route_refuses_a_caller_without_the_credential_or_from_another_origin() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    for (method, route) in ROUTES {
        let none = call(&origin, method, route, &[]).send().await.unwrap();
        assert_eq!(none.status(), 401, "{method} {route} without a credential");
        let wrong = call(
            &origin,
            method,
            route,
            &[("X-Hermes-Session-Token", "wrong")],
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            wrong.status(),
            401,
            "{method} {route} with a wrong credential"
        );
        let foreign = call(
            &origin,
            method,
            route,
            &[
                ("X-Hermes-Session-Token", &token),
                ("Origin", "http://evil.example"),
            ],
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            foreign.status(),
            403,
            "{method} {route} from another origin"
        );
        let rebound = call(
            &origin,
            method,
            route,
            &[
                ("X-Hermes-Session-Token", &token),
                ("Origin", &origin),
                ("Host", "evil.example"),
            ],
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            rebound.status(),
            403,
            "{method} {route} under another host name"
        );
    }
    untouched(&home);
}

/// Each reply is the router's own 404 body, which none of the handlers gives when it runs: with
/// `remote-1` saved, an unguarded list answers 200, save 400, last-used 404 with another text,
/// remove 200 and the relay a dial failure.
#[tokio::test(flavor = "multi_thread")]
async fn no_route_exists_for_a_native_client() {
    let (_dir, home) = private_home();
    let saved = saved_gateway(&home);
    let (_runtime, origin) = serving(&home);
    for (method, route) in ROUTES {
        let ticket = native_ticket(&home);
        let reply = call(
            &origin,
            method,
            route,
            &[("X-Hermes-Gateway-Ticket", &ticket)],
        )
        .send()
        .await
        .unwrap();
        assert_eq!(reply.status(), 404, "{method} {route} for a native client");
        let body: Value = reply.json().await.unwrap();
        assert_eq!(body, json!({"detail": "Not Found"}), "{method} {route}");
    }
    assert_eq!(std::fs::read_to_string(home.join(STORE)).unwrap(), saved);
    assert!(!home.join(format!("{STORE}.lock")).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_lists_the_saved_gateways_and_reaches_the_relay() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    let signed_in = [
        ("X-Hermes-Session-Token", token.as_str()),
        ("Origin", origin.as_str()),
    ];
    let list = call(&origin, "GET", "/api/gateway-connections", &signed_in)
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);
    let body: serde_json::Value = list.json().await.unwrap();
    assert_eq!(body["version"], 2, "{body}");
    assert_eq!(body["connections"][0]["id"], "local", "{body}");
    let relay = call(
        &origin,
        "GET",
        "/api/relay/remote-1/api/status",
        &[
            signed_in[0],
            signed_in[1],
            ("Sec-Fetch-Dest", "empty"),
            ("Sec-Fetch-Site", "same-origin"),
        ],
    )
    .send()
    .await
    .unwrap();
    assert_eq!(relay.status(), 404, "an unsaved gateway is not relayed");
    let body: Value = relay.json().await.unwrap();
    assert_eq!(body, json!({"detail": "No such connection"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_saves_uses_and_removes_a_gateway() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    let signed_in = [
        ("X-Hermes-Session-Token", token.as_str()),
        ("Origin", origin.as_str()),
    ];
    let send = |method: &str, route: &str, body: Value| {
        call(&origin, method, route, &signed_in).json(&body).send()
    };
    let gateway =
        json!({"label": "Work", "url": "https://gw.example", "token": "secret-token-123456"});
    let saved = send("POST", "/api/gateway-connections", gateway)
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let body: Value = saved.json().await.unwrap();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["connection"]["id"], "work", "{body}");
    assert_eq!(body["connection"]["tokenPreview"], "set", "{body}");
    assert!(!body.to_string().contains("secret-token-123456"), "{body}");
    assert!(home.join(STORE).exists());

    let used = send(
        "POST",
        "/api/gateway-connections/last-used",
        json!({"id": "work"}),
    )
    .await
    .unwrap();
    assert_eq!(used.status(), 200);
    let body: Value = used.json().await.unwrap();
    assert_eq!(body["registry"]["lastUsed"], "work", "{body}");

    let not_an_object = send("POST", "/api/gateway-connections", json!(["work"]))
        .await
        .unwrap();
    assert_eq!(not_an_object.status(), 400);
    let body: Value = not_an_object.json().await.unwrap();
    assert_eq!(
        body,
        json!({"detail": "A connection save needs a JSON object."})
    );

    let removed = send("DELETE", "/api/gateway-connections/work", json!({}))
        .await
        .unwrap();
    assert_eq!(removed.status(), 200);
    let body: Value = removed.json().await.unwrap();
    assert_eq!(body["registry"]["lastUsed"], "local", "{body}");
    assert_eq!(
        body["registry"]["connections"].as_array().map(Vec::len),
        Some(1)
    );
}
