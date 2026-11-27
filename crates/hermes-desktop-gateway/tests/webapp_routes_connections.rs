//! The saved gateways and relay routes through the full local runtime: who may call them, and
//! that a signed-in Webapp tab reaches them.
#![cfg(unix)]

mod webapp_support;

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
    assert!(!home.join("gateway-connections.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn no_route_exists_for_a_native_client() {
    let (_dir, home) = private_home();
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
    }
    assert!(!home.join("gateway-connections.json").exists());
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
    assert_eq!(body["ok"], true);
    assert!(body["registry"]["connections"].is_array(), "{body}");
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
}
