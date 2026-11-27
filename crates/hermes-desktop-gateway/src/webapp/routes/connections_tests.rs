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
fn a_store_holding_an_entry_that_is_not_an_object_is_kept_aside_before_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    save_remote(&path, "Work", "https://gw.example");
    let mut doc = read_doc(&path).unwrap().0.unwrap();
    let list = doc.get_mut("connections").unwrap().as_array_mut().unwrap();
    list.push(json!("an entry native Desktop wrote"));
    let bytes = serde_json::to_vec(&doc).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    last_used(&path, "work").unwrap();
    let kept: Vec<Vec<u8>> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("gateway-connections.json.corrupt-")
        })
        .map(|e| std::fs::read(e.path()).unwrap())
        .collect();
    assert_eq!(kept, vec![bytes], "the dropped entry was not kept");
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
            // The body's length, once it has all arrived.
            if path == "/echo" {
                let body = axum::body::to_bytes(request.into_body(), usize::MAX).await;
                return body.map_or(StatusCode::BAD_REQUEST.into_response(), |b| {
                    b.len().to_string().into_response()
                });
            }
            let mut response = match path.as_str() {
                "/unauth" => StatusCode::UNAUTHORIZED.into_response(),
                "/out" => (
                    StatusCode::FOUND,
                    [(header::LOCATION, "http://evil.example/")],
                )
                    .into_response(),
                "/in" => (StatusCode::FOUND, [(header::LOCATION, "/../api/next")]).into_response(),
                // Headers and a first chunk, then nothing, ever.
                "/stall" => Body::from_stream(
                    futures::stream::once(async {
                        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"first"))
                    })
                    .chain(futures::stream::pending()),
                )
                .into_response(),
                "/big" => vec![b'x'; MAX_RESPONSE + 1].into_response(),
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

// ---- Review round 1 -------------------------------------------------------------------------

#[test]
fn an_edit_that_moves_a_gateway_must_send_its_token_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    save(&path, &obj(json!({"kind": "remote", "label": "Work", "url": "https://gw.example", "token": "first", "headers": {"X-Key": "k1"}, "allowPrivateNetwork": true}))).unwrap();
    for moved in [
        json!({"id": "work", "url": "https://collector.example"}),
        json!({"id": "work", "url": "https://collector.example", "token": "  ", "headers": {"X-Key": null}}),
        json!({"id": "work", "url": "https://gw.example:8443"}),
        json!({"id": "work", "url": "http://gw.example"}),
        json!({"id": "work", "url": "https://gw.example/other"}),
    ] {
        assert!(
            matches!(
                save(&path, &obj(moved.clone())),
                Err(StoreError::Refused(_))
            ),
            "{moved}"
        );
    }
    let (_, moved) = save(&path, &obj(json!({"id": "work", "url": "https://collector.example", "token": "second", "headers": {"X-Key": null}}))).unwrap();
    assert_eq!(moved["token"], envelope("second"));
    assert!(
        moved.get("headers").is_none(),
        "a saved header moved: {moved:?}"
    );
    assert_eq!(moved["allowPrivateNetwork"], false);
    // The same address in another spelling is not a move.
    let (_, same) = save(
        &path,
        &obj(json!({"id": "work", "url": "https://collector.example:443/", "label": "Work 2"})),
    )
    .unwrap();
    assert_eq!(same["token"], envelope("second"));
}

#[tokio::test]
async fn a_token_saved_for_one_gateway_never_reaches_another() {
    let (first, first_seen) = gateway().await;
    let (second, second_seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, first).await;
    let move_without_token = obj(json!({"id": "work", "url": format!("http://{second}")}));
    assert!(save(&path, &move_without_token).is_err());
    let ok = relay(&path, "work", None, browser_request("/api/relay/work/x")).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert!(second_seen.lock().unwrap().is_empty());
    save(
        &path,
        &obj(json!({"id": "work", "url": format!("http://{second}"), "token": "new-token"})),
    )
    .unwrap();
    let moved = relay(&path, "work", None, browser_request("/api/relay/work/x")).await;
    assert_eq!(moved.status(), StatusCode::OK);
    assert_eq!(first_seen.lock().unwrap().len(), 1);
    let (_, headers) = second_seen.lock().unwrap()[0].clone();
    assert_eq!(headers["x-hermes-session-token"], "new-token");
    assert!(
        !headers.contains_key("x-access"),
        "the first gateway's header moved"
    );
    assert!(
        !format!("{headers:?}").contains("saved-"),
        "a secret saved for the first gateway reached the second"
    );
}

#[test]
fn the_public_view_never_shows_a_name_or_password_in_a_url() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    // As native Desktop writes it: userinfo kept in the URL, the token sealed by the keychain.
    let sealed = json!({"encoding": "safeStorage", "value": "djEwAAAA"});
    let doc = json!({"version": 2, "connections": [
        {"id": "gw", "kind": "remote", "label": "Gw", "url": "https://user:pw-secret@gw.example:8443/base", "authMode": "token", "token": sealed},
        {"id": "nameonly", "kind": "remote", "label": "N", "url": "https://only-name@n.example", "authMode": "token", "token": sealed},
        {"id": "broken", "kind": "remote", "label": "B", "url": "https://user:pw-secret@[bad", "authMode": "token", "token": sealed}
    ]});
    std::fs::write(&path, doc.to_string()).unwrap();
    let (doc, _) = read_doc(&path).unwrap();
    let view = public_registry(doc.as_ref());
    let shown = view.to_string();
    assert!(
        !shown.contains("pw-secret") && !shown.contains("only-name") && !shown.contains('@'),
        "{shown}"
    );
    let list = view["connections"].as_array().unwrap();
    let url_of = |id: &str| {
        list.iter()
            .find(|c| c["id"] == id)
            .unwrap()
            .get("url")
            .cloned()
    };
    assert_eq!(url_of("gw"), Some(json!("https://gw.example:8443/base")));
    assert_eq!(url_of("nameonly"), Some(json!("https://n.example")));
    assert_eq!(url_of("broken"), None);
}

#[test]
fn a_percent_encoded_plain_character_is_refused_in_a_relayed_path() {
    for path in [
        "/api/relay/gw/%61pi/relay/other/x",
        "/api/relay/gw/%2E%2E/x",
        "/api/relay/gw/a%7Eb",
        "/api/relay/gw/a%5fb",
    ] {
        assert_eq!(relay_target(path, None, "gw"), None, "{path}");
    }
    assert_eq!(
        relay_target("/api/relay/gw/a%20b%3F", None, "gw").as_deref(),
        Some("/a%20b%3F")
    );
}

#[tokio::test]
async fn only_upstreams_methods_are_relayed() {
    let (addr, seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, addr).await;
    for method in ["TRACE", "CONNECT", "PROPFIND"] {
        let mut request = browser_request("/api/relay/work/x");
        *request.method_mut() = Method::from_bytes(method.as_bytes()).unwrap();
        assert_eq!(
            relay(&path, "work", None, request).await.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
    }
    assert!(seen.lock().unwrap().is_empty());
    for method in [
        Method::GET,
        Method::HEAD,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ] {
        let mut request = browser_request("/api/relay/work/x");
        *request.method_mut() = method.clone();
        assert_eq!(
            relay(&path, "work", None, request).await.status(),
            StatusCode::OK,
            "{method}"
        );
    }
}

#[tokio::test]
async fn only_fetch_and_xhr_reach_the_relay() {
    let (addr, seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, addr).await;
    for dest in ["script", "document", "iframe", "worker", "style"] {
        let mut request = browser_request("/api/relay/work/x.js");
        request
            .headers_mut()
            .insert("sec-fetch-dest", HeaderValue::from_static(dest));
        assert_eq!(
            relay(&path, "work", None, request).await.status(),
            StatusCode::FORBIDDEN,
            "{dest}"
        );
    }
    assert!(seen.lock().unwrap().is_empty());
    let mut fetch = browser_request("/api/relay/work/x");
    fetch
        .headers_mut()
        .insert("sec-fetch-dest", HeaderValue::from_static("empty"));
    assert_eq!(
        relay(&path, "work", None, fetch).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_stalled_or_oversized_body_ends_the_relay() {
    let (addr, _seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, addr).await;
    for (route, what) in [
        ("/api/relay/work/stall", "stalled"),
        ("/api/relay/work/big", "oversized"),
    ] {
        let response = relay(&path, "work", None, browser_request(route)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = tokio::time::timeout(
            BODY_IDLE_TIMEOUT * 5,
            axum::body::to_bytes(response.into_body(), usize::MAX),
        )
        .await;
        assert!(
            matches!(body, Ok(Err(_))),
            "a {what} body was not ended by the relay"
        );
    }
}

#[test]
fn deprecated_wrapped_and_platform_addresses_are_refused() {
    for ip in [
        "fec0::1",
        "::7f00:1",
        "::a9fe:a9fe",
        "64:ff9b::a9fe:a9fe",
        "64:ff9b::a00:1",
        "168.63.129.16",
    ] {
        let ip: IpAddr = ip.parse().unwrap();
        assert!(judge_address(ip, false).is_err(), "{ip} was allowed");
    }
    assert!(judge_address("64:ff9b::5db8:d70e".parse().unwrap(), false).is_ok());
}

#[test]
fn a_header_value_with_a_control_byte_is_refused_at_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    for value in ["a\u{1}b", "a\u{7f}b", "a\u{1b}[0m"] {
        assert!(
            matches!(
                save(
                    &path,
                    &obj(
                        json!({"kind": "remote", "label": "W", "url": "https://gw.example", "token": "t", "headers": {"X-Key": value}})
                    )
                ),
                Err(StoreError::Refused(_))
            ),
            "{value:?}"
        );
    }
    save(&path, &obj(json!({"kind": "remote", "label": "W", "url": "https://gw.example", "token": "t", "headers": {"X-Key": "a\tb"}}))).unwrap();
}

// ---- Review round 2 -------------------------------------------------------------------------

#[test]
fn a_url_without_a_web_scheme_is_never_shown() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    let sealed = json!({"encoding": "safeStorage", "value": "djEwAAAA"});
    let doc = json!({"version": 2, "connections": [
        {"id": "bare", "kind": "remote", "label": "Bare", "url": "user:pw-secret@gw.example", "authMode": "token", "token": sealed},
        {"id": "file", "kind": "remote", "label": "File", "url": "file:///home/u/pw-secret", "authMode": "token", "token": sealed}
    ]});
    std::fs::write(&path, doc.to_string()).unwrap();
    let (doc, _) = read_doc(&path).unwrap();
    let view = public_registry(doc.as_ref());
    assert!(!view.to_string().contains("pw-secret"), "{view}");
    for entry in view["connections"].as_array().unwrap() {
        if entry["id"] != "local" {
            assert!(entry.get("url").is_none(), "{entry}");
        }
    }
}

#[test]
fn a_move_carries_only_what_the_edit_sends() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    // An entry native Desktop wrote, with fields this gateway does not manage.
    let entry = json!({"id": "gw", "kind": "remote", "label": "Gw", "url": "https://gw.example", "authMode": "token",
        "token": envelope("first"), "deviceSecret": "native-only", "remoteProfile": "work"});
    std::fs::write(&path, json!({"connections": [entry]}).to_string()).unwrap();
    // An edit at the same address keeps them.
    let (_, kept) = save(&path, &obj(json!({"id": "gw", "label": "Gw 2"}))).unwrap();
    assert_eq!(kept["deviceSecret"], "native-only");
    assert_eq!(kept["remoteProfile"], "work");
    // A move starts afresh: id, kind, label, URL, auth mode, the new token and consent only.
    let (_, moved) = save(
        &path,
        &obj(json!({"id": "gw", "url": "https://collector.example", "token": "second"})),
    )
    .unwrap();
    let mut keys: Vec<&str> = moved.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "allowPrivateNetwork",
            "authMode",
            "id",
            "kind",
            "label",
            "token",
            "url"
        ]
    );
    assert_eq!(moved["label"], "Gw 2");
}

// ---- Failures and edge shapes ----------------------------------------------------------------

fn busy(text: &str) -> StoreError {
    StoreError::Busy(text.into())
}

/// A store document written as native Desktop might, owner-only.
fn written(dir: &tempfile::TempDir, doc: Value) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = store(dir);
    std::fs::write(&path, doc.to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

#[test]
fn store_errors_answer_400_404_and_503() {
    for (err, status) in [
        (StoreError::Refused("r".into()), StatusCode::BAD_REQUEST),
        (StoreError::Missing("m".into()), StatusCode::NOT_FOUND),
        (busy("b"), StatusCode::SERVICE_UNAVAILABLE),
    ] {
        assert_eq!(ApiError::from(err).0, status);
    }
}

#[test]
fn a_store_that_cannot_be_read_inspected_or_placed_answers_busy() {
    let dir = tempfile::tempdir().unwrap();
    let unreadable = busy("gateway connections store is unreadable");
    let folder = store(&dir);
    std::fs::create_dir(&folder).unwrap();
    assert_eq!(read_doc(&folder), Err(unreadable.clone()));
    assert_eq!(
        guard_permissions(&dir.path().join("gone")),
        Err(unreadable.clone())
    );
    let file = dir.path().join("file");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(last_used(&file.join(STORE_NAME), "local"), Err(unreadable));
}

#[test]
fn a_write_that_cannot_land_answers_busy_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    // A folder in the store's place, not empty, so the rename onto it fails.
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("inside"), "x").unwrap();
    let failed = busy("gateway connections store could not be written");
    assert_eq!(write_doc(&path, &Map::new(), false), Err(failed.clone()));
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(left.is_empty(), "a temporary file was left");
    let missing = dir.path().join("missing").join(STORE_NAME);
    assert_eq!(write_doc(&missing, &Map::new(), true), Err(failed));
}

#[test]
fn a_change_whose_lock_was_taken_over_meanwhile_is_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    let lock = dir.path().join(format!("{STORE_NAME}.lock"));
    let result = mutate(&path, |_, list| {
        // Another writer breaks this lock whilst the change is made.
        std::fs::rename(&lock, dir.path().join("moved")).unwrap();
        list.push(obj(json!({"id": "x", "kind": "ssh"})));
        Ok(())
    });
    assert_eq!(
        result.map(|_| ()),
        Err(busy("gateway connections store lock was taken over"))
    );
    assert!(!path.exists(), "the store was written without the lock");
}

#[test]
fn the_registry_names_a_saved_primary_launch_mode_and_quarantined_entries() {
    let doc = obj(json!({
        "primary": "work",
        "launchMode": "last-used",
        "lastUsed": "gone",
        "connections": [{"id": "work", "kind": "remote", "label": "Work",
                         "url": "https://gw.example", "token": "bare-secret"}],
        "quarantined": [{"reason": "bad", "entry": {"label": "Old"}}, {"entry": 5}, 7],
    }));
    let registry = public_registry(Some(&doc));
    assert_eq!(registry["primary"], "work");
    assert_eq!(registry["launchMode"], "last-used");
    assert_eq!(
        registry["lastUsed"], "work",
        "an unknown last-used id names the primary"
    );
    assert_eq!(
        registry["quarantined"],
        json!([{"reason": "bad", "label": "Old"}, {"reason": "unknown", "label": ""}])
    );
    // A token stored as a bare string counts as set, and is never shown.
    assert_eq!(registry["connections"][1]["tokenSet"], true);
    assert!(!registry.to_string().contains("bare-secret"));
}

#[test]
fn urls_that_do_not_parse_are_refused_and_ip_literals_are_judged() {
    assert_eq!(
        normalize_url("http://[::1").map(|u| u.to_string()),
        Err(refused("Remote gateway URL is not valid: Invalid URL"))
    );
    assert_eq!(
        normalize_url("http://[::1]:9").map(|u| u.to_string()),
        Ok("http://[::1]:9/".into())
    );
    assert!(normalize_url("http://[fe80::1]").is_err());
}

#[test]
fn ids_come_from_the_label_and_never_take_a_used_or_the_local_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    assert_eq!(
        save_remote(&path, "Work Box!", "https://a.example")["id"],
        "work-box"
    );
    assert_eq!(
        save_remote(&path, "!!!", "https://b.example")["id"],
        "connection"
    );
    assert_eq!(
        save_remote(&path, "Local", "https://c.example")["id"],
        "local-2"
    );
    let unnamed = save(
        &path,
        &obj(json!({"url": "https://d.example", "token": "secret-token-123456"})),
    )
    .unwrap()
    .1;
    assert_eq!(
        unnamed["label"], "d.example",
        "an unnamed gateway takes its host"
    );
}

#[test]
fn bad_names_headers_and_consent_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    let attempt = |extra: Value| {
        let mut payload = obj(
            json!({"label": "Gw", "url": "https://gw.example", "token": "secret-token-123456"}),
        );
        payload.extend(obj(extra));
        save(&path, &payload).map(|_| ())
    };
    assert_eq!(
        attempt(json!({"label": "x".repeat(LABEL_MAX + 1)})),
        Err(refused(format!(
            "A gateway name is at most {LABEL_MAX} characters."
        )))
    );
    assert_eq!(
        attempt(json!({"headers": {"X-Count": 5}})),
        Err(refused("Header \"X-Count\" must be a string or null."))
    );
    assert_eq!(
        attempt(json!({"headers": "X-A: 1"})),
        Err(refused("Gateway headers must be an object."))
    );
    assert_eq!(
        attempt(json!({"allowPrivateNetwork": "yes"})),
        Err(refused("allowPrivateNetwork must be true or false."))
    );
    // A blank header name is skipped, not refused.
    assert_eq!(attempt(json!({"headers": {" ": "v"}})), Ok(()));
    let doc = read_doc(&path).unwrap().0;
    let entry = find(&connections(doc.as_ref()), "gw").cloned().unwrap();
    assert!(!entry.contains_key("headers"));
}

#[test]
fn the_relay_refuses_a_second_hop_in_any_case_and_a_control_character_in_the_query() {
    assert_eq!(relay_target("/api/relay/w/API/Relay/x", None, "w"), None);
    assert_eq!(relay_target("/api/relay/w/api/relay", None, "w"), None);
    assert_eq!(relay_target("/api/relay/w/x", Some("a=\u{1}"), "w"), None);
    assert_eq!(
        relay_target("/api/relay/w/x", Some("a=1"), "w"),
        Some("/x?a=1".into())
    );
}

#[test]
fn a_redirect_to_the_gateway_root_or_a_trailing_dot_segment_stays_a_folder() {
    let (gw, base) = ("https://gw.example", "/api/relay/w");
    assert_eq!(relayed_location(gw, gw, base), Some("/api/relay/w/".into()));
    assert_eq!(
        relayed_location("/a/b/..", gw, base),
        Some("/api/relay/w/a/".into())
    );
    assert_eq!(
        relayed_location("/a/.", gw, base),
        Some("/api/relay/w/a/".into())
    );
}

#[tokio::test]
async fn names_are_resolved_and_judged_and_empty_or_zoned_answers_are_refused() {
    let url = |u: &str| url::Url::parse(u).unwrap();
    let local = checked_addresses(&url("http://localhost:9"), false, None)
        .await
        .unwrap();
    assert!(local.iter().all(|a| a.ip().is_loopback()), "{local:?}");
    assert!(checked_addresses(&url("http://[::1]:9"), false, None)
        .await
        .is_ok());
    assert_eq!(
        checked_addresses(&url("http://gw.invalid"), false, None).await,
        Err("the name does not resolve")
    );
    assert_eq!(
        judged(vec![], false, None),
        Err("the name does not resolve")
    );
    let zoned = std::net::SocketAddrV6::new("2001:db8::1".parse().unwrap(), 80, 0, 3);
    assert_eq!(
        judged(vec![SocketAddr::V6(zoned)], true, None),
        Err("a zoned IPv6 address")
    );
}

#[test]
fn a_saved_entry_without_an_address_or_credential_is_not_dialled() {
    let dir = tempfile::tempdir().unwrap();
    let path = written(
        &dir,
        json!({"connections": [
            {"id": "nourl", "kind": "remote", "label": "NoUrl", "url": "not a url::", "token": "t"},
            {"id": "notoken", "kind": "remote", "label": "NoToken", "url": "https://a.example"},
            {"id": "blank", "kind": "remote", "url": "https://b.example", "token": "bad\ntoken",
             "headers": {"X-Empty": ""}},
        ]}),
    );
    let why = |id: &str| match dial_of(&path, id) {
        Err(DialError::NotRelayable(why)) => why,
        _ => panic!("{id} was dialled"),
    };
    assert_eq!(
        why("nourl"),
        "NoUrl has no valid address. Edit it in Settings → Gateways."
    );
    assert_eq!(
        why("notoken"),
        "NoToken has no saved credential. Edit it in Settings → Gateways."
    );
    let Ok(blank) = dial_of(&path, "blank") else {
        panic!("blank was not dialled");
    };
    assert!(blank.headers.is_empty(), "an empty header value was sent");
    assert_eq!(blank.label, "blank", "an unnamed entry is named by its id");
    let sent = forwarded_headers(&HeaderMap::new(), &blank);
    assert!(
        !sent.contains_key(SESSION_TOKEN_HEADER),
        "a token that is no header value was sent"
    );
}

#[tokio::test]
async fn the_relay_carries_a_body_and_refuses_one_past_the_cap() {
    let (addr, seen) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = saved(&dir, addr).await;
    let post = |body: Vec<u8>| {
        Request::builder()
            .method(Method::POST)
            .uri("/api/relay/work/echo")
            .header("sec-fetch-site", "same-origin")
            .body(Body::from(body))
            .unwrap()
    };
    let response = relay(&path, "work", None, post(b"hello".to_vec())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"5");
    let response = relay(&path, "work", None, post(vec![b'x'; MAX_REQUEST + 1])).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    // A GET that names an empty body is carried without one.
    let get = Request::builder()
        .uri("/api/relay/work/plain")
        .header(header::CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        relay(&path, "work", None, get).await.status(),
        StatusCode::OK
    );
    assert_eq!(seen.lock().unwrap().last().unwrap().0, "/plain");
}

#[tokio::test]
async fn the_relay_dials_a_saved_name_and_answers_an_unreachable_gateway_or_store() {
    let (addr, _) = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let path = store(&dir);
    let named = format!("http://localhost:{}", addr.port());
    save_remote(&path, "Named", &named);
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gone = closed.local_addr().unwrap();
    drop(closed);
    save_remote(&path, "Gone", &format!("http://{gone}"));
    let get = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
    let response = relay(&path, "named", None, get("/api/relay/named/x")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = relay(&path, "gone", None, get("/api/relay/gone/x")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"], "relay_unreachable");
    assert_eq!(body["detail"], "Gone is not reachable from this gateway.");
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir(store(&folder)).unwrap();
    let response = relay(&store(&folder), "x", None, get("/api/relay/x/y")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
