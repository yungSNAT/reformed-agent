use std::sync::{Arc, Mutex};

use super::*;

fn store(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join(STORE_NAME)
}

fn obj(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn save_remote(path: &Path, label: &str, url: &str) -> Map<String, Value> {
    save(
        path,
        &obj(json!({"kind": "remote", "label": label, "url": url, "token": "secret-token-123456"})),
    )
    .unwrap()
    .1
}

// ---- Addresses ------------------------------------------------------------------------------

#[test]
fn link_local_metadata_and_non_gateway_addresses_are_refused() {
    for ip in [
        "169.254.169.254",
        "169.254.0.1",
        "0.0.0.0",
        "255.255.255.255",
        "224.0.0.1",
        "fe80::1",
        "fd00:ec2::254",
        "::",
        "ff02::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
    ] {
        let ip: IpAddr = ip.parse().unwrap();
        assert!(judge_address(ip, true).is_err(), "{ip} was allowed");
    }
}

#[test]
fn private_addresses_need_the_entrys_consent_and_loopback_does_not() {
    for ip in [
        "10.0.0.1",
        "172.16.0.1",
        "192.168.1.1",
        "100.64.0.1",
        "fd12::1",
    ] {
        let ip: IpAddr = ip.parse().unwrap();
        assert!(
            judge_address(ip, false).is_err(),
            "{ip} allowed without consent"
        );
        assert!(judge_address(ip, true).is_ok(), "{ip} refused with consent");
    }
    for ip in ["127.0.0.1", "::1", "93.184.215.14"] {
        assert!(
            judge_address(ip.parse().unwrap(), false).is_ok(),
            "{ip} refused"
        );
    }
}

#[tokio::test]
async fn this_gateway_itself_and_an_unconsented_private_target_are_refused() {
    let own = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9120);
    let url = url::Url::parse("http://127.0.0.1:9120").unwrap();
    assert_eq!(
        checked_addresses(&url, false, Some(own)).await,
        Err("this gateway itself")
    );
    let other = url::Url::parse("http://127.0.0.1:9121").unwrap();
    assert!(checked_addresses(&other, false, Some(own)).await.is_ok());
    let private = url::Url::parse("http://10.1.2.3").unwrap();
    assert!(checked_addresses(&private, false, None).await.is_err());
}

// ---- URLs -----------------------------------------------------------------------------------

#[test]
fn a_saved_url_is_normalised_as_upstream_and_never_carries_credentials() {
    let url = normalize_url("gw.example:8080/base/?q=1#f").unwrap();
    assert_eq!(stored_url(&url), "http://gw.example:8080/base");
    assert_eq!(
        stored_url(&normalize_url("https://gw.example:443/").unwrap()),
        "https://gw.example"
    );
    assert!(normalize_url("http://user:pass@gw.example").is_err());
    assert!(normalize_url("http://user@gw.example").is_err());
    assert!(normalize_url("ftp://gw.example").is_err());
    assert!(normalize_url("http://169.254.169.254").is_err());
    assert!(normalize_url("  ").is_err());
}

// ---- Store ----------------------------------------------------------------------------------

#[test]
fn a_missing_store_lists_the_local_entry_only() {
    let dir = tempfile::tempdir().unwrap();
    let (doc, corrupt) = read_doc(&store(&dir)).unwrap();
    assert!(doc.is_none() && !corrupt);
    let view = public_registry(None);
    assert_eq!(
        view["connections"],
        json!([{"id": "local", "kind": "local", "label": "This device", "tokenSet": false, "tokenPreview": null, "headerNames": []}])
    );
    assert_eq!(view["primary"], "local");
}

#[cfg(unix)]
#[test]
fn a_save_writes_a_user_only_file_and_the_public_view_holds_no_secret() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    let payload = obj(json!({
        "kind": "remote", "label": "Work", "url": "https://gw.example",
        "token": "secret-token-123456", "headers": {"CF-Access-Client-Secret": "header-secret-789"}
    }));
    let (doc, entry) = save(&path, &payload).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(entry["id"], "work");
    let shown = public_registry(Some(&doc)).to_string() + &public_connection(&entry).to_string();
    assert!(
        !shown.contains("secret-token") && !shown.contains("123456"),
        "{shown}"
    );
    assert!(!shown.contains("header-secret"), "{shown}");
    assert!(
        shown.contains("\"tokenPreview\":\"set\"") && shown.contains("CF-Access-Client-Secret")
    );
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("\"encoding\": \"plain\""));
}

#[cfg(unix)]
#[test]
fn a_store_others_can_read_is_closed_before_use() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    std::fs::write(&path, "{\"connections\": []}").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    read_doc(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn a_store_that_is_a_link_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("elsewhere.json");
    std::fs::write(&real, "{}").unwrap();
    std::os::unix::fs::symlink(&real, store(&dir)).unwrap();
    assert!(matches!(read_doc(&store(&dir)), Err(StoreError::Busy(_))));
}

#[test]
fn entries_this_gateway_cannot_dial_are_kept_and_cannot_be_edited_here() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    let ssh = json!({"id": "box", "kind": "ssh", "label": "Box", "host": "box.lan", "user": "me", "future": {"x": 1}});
    let oauth = json!({"id": "cloud", "kind": "cloud", "label": "Cloud", "url": "https://c.example", "authMode": "oauth"});
    std::fs::write(&path, json!({"version": 2, "revision": 4, "connections": [ssh, oauth], "importedFrom": ["/a"], "unknownKey": true}).to_string()).unwrap();
    save_remote(&path, "Work", "https://gw.example");
    let (doc, _) = read_doc(&path).unwrap();
    let doc = doc.unwrap();
    let list = connections(Some(&doc));
    assert_eq!(Value::Object(find(&list, "box").unwrap().clone()), ssh);
    assert_eq!(Value::Object(find(&list, "cloud").unwrap().clone()), oauth);
    assert_eq!(doc["importedFrom"], json!(["/a"]));
    assert_eq!(doc["unknownKey"], json!(true));
    assert_eq!(doc["revision"], json!(5));
    let edit = save(
        &path,
        &obj(json!({"id": "box", "kind": "remote", "url": "https://x.example", "token": "t"})),
    );
    assert!(matches!(edit, Err(StoreError::Refused(_))));
}

#[test]
fn only_remote_token_gateways_are_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    for payload in [
        json!({"kind": "ssh", "label": "A", "host": "a"}),
        json!({"kind": "remote", "label": "B", "url": "https://b.example", "authMode": "oauth"}),
        json!({"kind": "remote", "label": "C", "url": "https://c.example"}),
        json!({"kind": "remote", "label": "D", "url": "https://d.example", "token": "t", "headers": {"Cookie": "x"}}),
        json!({"kind": "remote", "label": "E", "url": "https://e.example", "token": "t", "headers": {"Proxy-Authorization": "x"}}),
        json!({"kind": "remote", "label": "F", "url": "https://f.example", "token": "t", "headers": {"X-Ok": "a\r\nInjected: 1"}}),
        json!({"kind": "remote", "label": "G", "url": "http://me:pw@g.example", "token": "t"}),
    ] {
        assert!(
            matches!(
                save(&path, &obj(payload.clone())),
                Err(StoreError::Refused(_))
            ),
            "{payload}"
        );
    }
}

#[test]
fn labels_and_addresses_are_unique_and_edits_keep_unsent_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    save(&path, &obj(json!({"kind": "remote", "label": "Work", "url": "https://gw.example", "token": "first", "headers": {"X-Key": "k1"}}))).unwrap();
    assert!(save(
        &path,
        &obj(
            json!({"kind": "remote", "label": "work", "url": "https://other.example", "token": "t"})
        )
    )
    .is_err());
    assert!(save(
        &path,
        &obj(
            json!({"kind": "remote", "label": "Other", "url": "https://gw.example/", "token": "t"})
        )
    )
    .is_err());
    let (_, edited) = save(&path, &obj(json!({"id": "work", "kind": "remote", "label": "Work 2", "url": "https://gw.example", "headers": {"X-Key": null}}))).unwrap();
    assert_eq!(edited["token"], envelope("first"));
    assert_eq!(edited["headers"]["X-Key"], envelope("k1"));
    assert_eq!(edited["label"], "Work 2");
    assert_eq!(edited["allowPrivateNetwork"], false);
}

#[test]
fn an_unchanged_write_keeps_the_revision_and_a_corrupt_store_is_kept_aside() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    std::fs::write(&path, "not json").unwrap();
    save_remote(&path, "Work", "https://gw.example");
    let aside = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("gateway-connections.json.corrupt-")
        });
    assert!(aside);
    last_used(&path, "work").unwrap();
    let before = read_doc(&path).unwrap().0.unwrap()["revision"].clone();
    last_used(&path, "work").unwrap();
    assert_eq!(read_doc(&path).unwrap().0.unwrap()["revision"], before);
}

#[test]
fn remove_and_last_used_name_an_entry_and_the_local_one_stays() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    save_remote(&path, "Work", "https://gw.example");
    assert!(matches!(
        last_used(&path, "nope"),
        Err(StoreError::Missing(_))
    ));
    let doc = last_used(&path, "work").unwrap();
    assert_eq!(public_registry(Some(&doc))["lastUsed"], "work");
    assert!(matches!(
        remove(&path, "local"),
        Err(StoreError::Refused(_))
    ));
    assert!(matches!(remove(&path, "nope"), Err(StoreError::Missing(_))));
    let doc = remove(&path, "work").unwrap();
    assert_eq!(public_registry(Some(&doc))["lastUsed"], "local");
    assert!(find(&connections(Some(&doc)), "work").is_none());
}

// ---- Relay paths ----------------------------------------------------------------------------

#[test]
fn the_relay_carries_only_plain_paths_on_the_gateway() {
    assert_eq!(
        relay_target("/api/relay/gw/api/status", Some("a=1"), "gw").as_deref(),
        Some("/api/status?a=1")
    );
    assert_eq!(
        relay_target("/api/relay/gw", None, "gw").as_deref(),
        Some("/")
    );
    for path in [
        "/api/relay/gw/../x",
        "/api/relay/gw/a/%2e%2E/x",
        "/api/relay/gw/a/./x",
        "/api/relay/gw/a%2fb",
        "/api/relay/gw/a%5Cb",
        "/api/relay/gw/a\\b",
        "/api/relay/gw//evil.example/x",
        "/api/relay/gw/api/relay/other/x",
        "/api/relay/gwx/api",
        "/api/other/gw/x",
    ] {
        assert_eq!(relay_target(path, None, "gw"), None, "{path}");
    }
}

#[test]
fn a_redirect_stays_within_the_gateway_or_is_refused() {
    let base = "/api/relay/gw";
    assert_eq!(
        relayed_location("/api/x?y=1", "https://gw.example", base).as_deref(),
        Some("/api/relay/gw/api/x?y=1")
    );
    assert_eq!(
        relayed_location("https://gw.example/a/../b", "https://gw.example", base).as_deref(),
        Some("/api/relay/gw/b")
    );
    assert_eq!(
        relayed_location("/../api/gateway/restart", "https://gw.example", base).as_deref(),
        Some("/api/relay/gw/api/gateway/restart")
    );
    for location in [
        "https://evil.example/",
        "//evil.example/x",
        "/a\\b",
        "/a\tb",
        "https://gw.example.evil/x",
        "relative",
    ] {
        assert_eq!(
            relayed_location(location, "https://gw.example", base),
            None,
            "{location}"
        );
    }
}

// ---- Relay end to end -----------------------------------------------------------------------

type Seen = Arc<Mutex<Vec<(String, HeaderMap)>>>;

async fn gateway() -> (SocketAddr, Seen) {
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let app = Router::new().fallback(move |request: Request| {
        let log = log.clone();
        async move {
            let path = request.uri().path().to_owned();
            log.lock()
                .unwrap()
                .push((request.uri().to_string(), request.headers().clone()));
            let mut response = match path.as_str() {
                "/unauth" => StatusCode::UNAUTHORIZED.into_response(),
                "/out" => (
                    StatusCode::FOUND,
                    [(header::LOCATION, "http://evil.example/")],
                )
                    .into_response(),
                "/in" => (StatusCode::FOUND, [(header::LOCATION, "/../api/next")]).into_response(),
                _ => "ok".into_response(),
            };
            let h = response.headers_mut();
            h.insert(header::SET_COOKIE, HeaderValue::from_static("s=1"));
            h.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
            h.insert(
                header::STRICT_TRANSPORT_SECURITY,
                HeaderValue::from_static("max-age=1"),
            );
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
            response
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, seen)
}

fn browser_request(uri: &str) -> Request {
    Request::builder()
        .uri(uri)
        .header("cookie", "webapp=1")
        .header("authorization", "Bearer browser")
        .header("origin", "http://127.0.0.1:9120")
        .header("referer", "http://127.0.0.1:9120/")
        .header("x-hermes-session-token", "browser-credential")
        .header("x-forwarded-for", "1.2.3.4")
        .header("connection", "x-secret-hop")
        .header("x-secret-hop", "1")
        .header("x-kept", "yes")
        .header("sec-fetch-site", "same-origin")
        .body(Body::empty())
        .unwrap()
}

async fn saved(dir: &tempfile::TempDir, addr: SocketAddr) -> PathBuf {
    let path = store(dir);
    save(&path, &obj(json!({"kind": "remote", "label": "Work", "url": format!("http://{addr}"), "token": "saved-token", "headers": {"X-Access": "saved-header"}}))).unwrap();
    path
}

#[tokio::test]
async fn the_relay_sends_the_saved_credential_and_nothing_of_the_browsers() {
    let (addr, seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, addr).await;
    let response = relay(
        &path,
        "work",
        None,
        browser_request("/api/relay/work/api/x?q=1"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let (uri, headers) = seen.lock().unwrap()[0].clone();
    assert_eq!(uri, "/api/x?q=1");
    assert_eq!(headers["x-hermes-session-token"], "saved-token");
    assert_eq!(headers["x-access"], "saved-header");
    assert_eq!(headers["x-kept"], "yes");
    for gone in [
        "cookie",
        "authorization",
        "origin",
        "referer",
        "x-forwarded-for",
        "x-secret-hop",
    ] {
        assert!(!headers.contains_key(gone), "{gone} crossed");
    }
    let out = response.headers();
    assert_eq!(out[header::CONTENT_SECURITY_POLICY], "sandbox");
    assert_eq!(out[header::CACHE_CONTROL], "no-store");
    assert_eq!(out[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(out[header::CONTENT_TYPE], "text/html");
    for gone in [
        header::SET_COOKIE,
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::STRICT_TRANSPORT_SECURITY,
    ] {
        assert!(!out.contains_key(&gone), "{gone} came back");
    }
}

#[tokio::test]
async fn a_redirect_out_is_refused_and_never_followed() {
    let (addr, seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, addr).await;
    let out = relay(&path, "work", None, browser_request("/api/relay/work/out")).await;
    assert_eq!(out.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(seen.lock().unwrap().len(), 1);
    let within = relay(&path, "work", None, browser_request("/api/relay/work/in")).await;
    assert_eq!(within.status(), StatusCode::FOUND);
    assert_eq!(
        within.headers()[header::LOCATION],
        "/api/relay/work/api/next"
    );
}

#[tokio::test]
async fn refusals_before_any_dial() {
    let (addr, seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, addr).await;
    let mut cross = browser_request("/api/relay/work/api/x");
    cross
        .headers_mut()
        .insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
    assert_eq!(
        relay(&path, "work", None, cross).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        relay(&path, "nope", None, browser_request("/api/relay/nope/x"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        relay(
            &path,
            "work",
            None,
            browser_request("/api/relay/work/a/../b")
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    let own = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port());
    assert_eq!(
        relay(
            &path,
            "work",
            Some(own),
            browser_request("/api/relay/work/x")
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "a refused request reached the gateway"
    );
}

#[tokio::test]
async fn a_rejected_credential_and_an_undialable_entry_ask_for_a_reconnect() {
    let (addr, _seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, addr).await;
    let response = relay(
        &path,
        "work",
        None,
        browser_request("/api/relay/work/unauth"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let doc = obj(serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap());
    let mut list = connections(Some(&doc));
    list.push(obj(
        json!({"id": "box", "kind": "ssh", "label": "Box", "host": "box"}),
    ));
    let mut doc = doc;
    doc.insert(
        "connections".into(),
        Value::Array(list.into_iter().map(Value::Object).collect()),
    );
    std::fs::write(&path, Value::Object(doc).to_string()).unwrap();
    assert_eq!(
        relay(&path, "box", None, browser_request("/api/relay/box/x"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn a_saved_header_native_desktop_wrote_is_checked_again_at_dial() {
    let (addr, seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    let entry = json!({"id": "gw", "kind": "remote", "label": "Gw", "url": format!("http://{addr}"), "authMode": "token",
        "token": envelope("t"), "headers": {"Host": envelope("evil.example")}});
    std::fs::write(&path, json!({"connections": [entry]}).to_string()).unwrap();
    assert_eq!(
        relay(&path, "gw", None, browser_request("/api/relay/gw/x"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_private_target_needs_consent() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    save(
        &path,
        &obj(
            json!({"kind": "remote", "label": "Lan", "url": "http://10.255.255.1:9", "token": "t"}),
        ),
    )
    .unwrap();
    let response = relay(&path, "lan", None, browser_request("/api/relay/lan/x")).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
