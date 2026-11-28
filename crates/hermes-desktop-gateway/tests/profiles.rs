//! The profiles a remote Hermes Desktop lists (`GET /api/profiles`, `profiles.list`) and the bot
//! roster it pushes (`bot_relay.roster.sync`), with the refusals first.

use std::net::SocketAddr;
use std::path::Path;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "0123456789abcdef0123456789abcdef-profiles-token";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// An install root with a default profile and named ones, some of which must not be listed.
fn seed_root(root: &Path) {
    write(
        &root.join("config.yaml"),
        "model:\n  default: root-model\n  provider: root-provider\n",
    );
    // Listed: a model given as a string, an .env, 2 skills (one with a support folder holding
    // another SKILL.md, which is not a skill) and an avatar.
    let inbox = root.join("profiles/inbox");
    write(&inbox.join("config.yaml"), "model: inbox-model\n");
    write(&inbox.join(".env"), "SOME_KEY=1\n");
    write(&inbox.join("skills/a/SKILL.md"), "---\nname: a\n---\n");
    write(
        &inbox.join("skills/group/b/SKILL.md"),
        "---\nname: b\n---\n",
    );
    write(&inbox.join("skills/a/references/SKILL.md"), "not a skill\n");
    write(&inbox.join("skills/.git/c/SKILL.md"), "not a skill\n");
    write(&inbox.join("assets/avatar.png"), "png");
    // Listed: identified by profile.yaml alone.
    write(
        &root.join("profiles/research/profile.yaml"),
        "description: '  Reads papers  '\ndescription_auto: true\ndisplay_name: Research\n\
         previous_names: [old, ' old ', '', scholar]\n\
         ui_meta:\n  hermes-bots:\n    title: Scholar\n  other: dropped\n",
    );
    // Not listed: no identity marker, a name upstream's grammar refuses, the reserved name,
    // a tombstoned profile and a plain file.
    std::fs::create_dir_all(root.join("profiles/ghost/logs")).unwrap();
    write(&root.join("profiles/Bad_Name/config.yaml"), "");
    write(&root.join("profiles/default/config.yaml"), "");
    write(&root.join("profiles/gone/config.yaml"), "");
    std::fs::create_dir_all(root.join("profiles/.deleted/gone")).unwrap();
    write(&root.join("profiles/stray"), "");
}

/// A gateway whose home is `home`.
async fn start(home: &Path) -> SocketAddr {
    let config = hermes_config::load_config(home.to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chat = hermes_desktop_gateway::Chat::new(config);
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        chat,
    ));
    addr
}

async fn get_profiles(addr: SocketAddr, headers: &[(&str, &str)]) -> reqwest::Response {
    let mut request = reqwest::Client::new().get(format!("http://{addr}/api/profiles"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.unwrap()
}

async fn connect(addr: SocketAddr) -> Socket {
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws?token={TOKEN}"))
            .await
            .unwrap();
    let ready = next_frame(&mut socket).await;
    assert_eq!(ready["params"]["type"], "gateway.ready");
    socket
}

async fn next_frame(socket: &mut Socket) -> Value {
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

async fn call(socket: &mut Socket, id: u64, method: &str, params: Value) -> Value {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Message::text(request.to_string()))
        .await
        .unwrap();
    loop {
        let frame = next_frame(socket).await;
        if frame["id"] == id {
            return frame;
        }
    }
}

fn names(rows: &Value) -> Vec<&str> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn profiles_route_refuses_a_missing_or_wrong_token_and_a_foreign_origin() {
    let root = tempfile::tempdir().unwrap();
    seed_root(root.path());
    let addr = start(root.path()).await;

    assert_eq!(get_profiles(addr, &[]).await.status(), 401);
    let wrong = [("x-hermes-session-token", "not-the-token")];
    assert_eq!(get_profiles(addr, &wrong).await.status(), 401);
    let foreign = [
        ("x-hermes-session-token", TOKEN),
        ("origin", "https://evil.example"),
    ];
    assert_eq!(get_profiles(addr, &foreign).await.status(), 403);
}

#[tokio::test]
async fn profiles_route_lists_the_default_and_each_identified_named_profile() {
    let root = tempfile::tempdir().unwrap();
    seed_root(root.path());
    let addr = start(root.path()).await;

    let reply = get_profiles(addr, &[("x-hermes-session-token", TOKEN)]).await;
    assert_eq!(reply.status(), 200);
    let body: Value = reply.json().await.unwrap();
    let rows = &body["profiles"];
    assert_eq!(names(rows), ["default", "inbox", "research"]);

    let default = &rows[0];
    assert_eq!(default["is_default"], true);
    assert_eq!(default["path"], root.path().display().to_string());
    assert_eq!(default["model"], "root-model");
    assert_eq!(default["provider"], "root-provider");
    assert_eq!(default["has_env"], false);

    let inbox = &rows[1];
    assert_eq!(inbox["is_default"], false);
    assert_eq!(
        inbox["path"],
        root.path()
            .join("profiles")
            .join("inbox")
            .display()
            .to_string()
    );
    assert_eq!(inbox["model"], "inbox-model");
    assert_eq!(inbox["provider"], Value::Null);
    assert_eq!(inbox["has_env"], true);
    assert_eq!(inbox["skill_count"], 2);
    assert_eq!(inbox["description"], "");
    assert_eq!(inbox["ui_meta"], json!({}));

    let research = &rows[2];
    assert_eq!(research["model"], Value::Null);
    assert_eq!(research["description"], "Reads papers");
    assert_eq!(research["description_auto"], true);
    assert_eq!(research["display_name"], "Research");
    assert_eq!(research["bot_title"], "Scholar");
    assert_eq!(
        research["ui_meta"],
        json!({"hermes-bots": {"title": "Scholar"}})
    );
    // Upstream's 'unknown' values for what this gateway does not track.
    for row in rows.as_array().unwrap() {
        assert_eq!(row["gateway_running"], false);
        assert_eq!(row["has_alias"], false);
        assert_eq!(row["distribution_name"], Value::Null);
    }
}

#[tokio::test]
async fn a_gateway_running_in_a_profile_lists_the_whole_install() {
    let root = tempfile::tempdir().unwrap();
    seed_root(root.path());
    let addr = start(&root.path().join("profiles").join("inbox")).await;

    let body: Value = get_profiles(addr, &[("x-hermes-session-token", TOKEN)])
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(names(&body["profiles"]), ["default", "inbox", "research"]);
    assert_eq!(
        body["profiles"][0]["path"],
        root.path().display().to_string()
    );
}

#[tokio::test]
async fn profiles_list_answers_upstreams_roster_shape() {
    let root = tempfile::tempdir().unwrap();
    seed_root(root.path());
    write(&root.path().join("install_id"), "abc123-install\n");
    let addr = start(root.path()).await;
    let mut socket = connect(addr).await;

    let reply = call(
        &mut socket,
        1,
        "profiles.list",
        json!({"include_sessions": true}),
    )
    .await;
    let result = &reply["result"];
    assert_eq!(names(&result["profiles"]), ["default", "inbox", "research"]);
    // This backend does not inject the Bot Mode teammate protocol, so the Desktop must.
    assert_eq!(result["bot_mode_protocol"], false);
    assert_eq!(result["install_id"], "abc123-install");
    let research = &result["profiles"][2];
    assert_eq!(research["previous_names"], json!(["old", "scholar"]));
    assert_eq!(research["has_avatar"], false);
    assert_eq!(result["profiles"][1]["has_avatar"], true);
    assert_eq!(research["skill_count"], 0);

    let refused = call(&mut socket, 2, "profiles.list", json!(["not", "named"])).await;
    assert_eq!(refused["error"]["code"], 4000);
}

#[tokio::test]
async fn roster_sync_keeps_valid_rows_once_and_replaces_the_last_roster() {
    let root = tempfile::tempdir().unwrap();
    seed_root(root.path());
    let addr = start(root.path()).await;
    let mut socket = connect(addr).await;
    let long = "x".repeat(200);

    let agents = json!([
        {"profile": "research", "handle": "@scholar", "connection_id": "laptop",
         "connection_label": "  Laptop  ", "title": long, "description": " a\n  b ", "online": true},
        {"profile": "default", "connection_id": "laptop", "online": "yes"},
        // The first row for a (connection, profile) pair wins.
        {"profile": "research", "handle": "other", "connection_id": "laptop"},
        // Dropped: no connection, a path as a profile, a handle starting with '-', not an object.
        {"profile": "inbox"},
        {"profile": "../etc", "connection_id": "laptop"},
        {"profile": "inbox", "handle": "-x", "connection_id": "laptop"},
        "inbox",
    ]);
    let reply = call(
        &mut socket,
        1,
        "bot_relay.roster.sync",
        json!({"agents": agents}),
    )
    .await;
    assert_eq!(reply["result"], json!({"count": 2}));

    let file = root.path().join("bot_relay").join("roster.json");
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(stored["updated_at"].as_i64().unwrap() > 1_700_000_000);
    let rows = &stored["agents"];
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(
        rows[0],
        json!({"profile": "default", "handle": "hermes", "connection_id": "laptop",
               "connection_label": "", "title": "", "description": ""})
    );
    assert_eq!(rows[1]["handle"], "scholar");
    assert_eq!(rows[1]["connection_label"], "Laptop");
    assert_eq!(rows[1]["title"].as_str().unwrap().chars().count(), 120);
    assert_eq!(rows[1]["description"], "a b");
    assert_eq!(rows[1]["online"], true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // A roster that is not a list replaces the last one with an empty one.
    let reply = call(
        &mut socket,
        2,
        "bot_relay.roster.sync",
        json!({"agents": "none"}),
    )
    .await;
    assert_eq!(reply["result"], json!({"count": 0}));
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(stored["agents"], json!([]));
}

#[cfg(unix)]
#[tokio::test]
async fn roster_sync_makes_an_existing_roster_owner_only_again() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    seed_root(root.path());
    let file = root.path().join("bot_relay").join("roster.json");
    write(&file, "{}");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let addr = start(root.path()).await;
    let mut socket = connect(addr).await;

    let agents = json!([{"profile": "inbox", "connection_id": "laptop"}]);
    let reply = call(
        &mut socket,
        1,
        "bot_relay.roster.sync",
        json!({"agents": agents}),
    )
    .await;
    assert_eq!(reply["result"], json!({"count": 1}));
    let mode = std::fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(unix)]
#[tokio::test]
async fn roster_sync_does_not_wait_on_a_pipe_in_the_rosters_place() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    seed_root(root.path());
    let dir = root.path().join("bot_relay");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("roster.json");
    let name = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o644) }, 0);
    let addr = start(root.path()).await;
    let mut socket = connect(addr).await;

    let agents = json!([{"profile": "inbox", "connection_id": "laptop"}]);
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        call(
            &mut socket,
            1,
            "bot_relay.roster.sync",
            json!({"agents": agents}),
        ),
    )
    .await
    .expect("answered within 10 s");
    assert_eq!(reply["result"], json!({"count": 1}));
    let meta = std::fs::symlink_metadata(&file).unwrap();
    assert!(!meta.file_type().is_fifo() && meta.is_file());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[tokio::test]
async fn roster_sync_reads_numbers_as_upstreams_str_does() {
    let root = tempfile::tempdir().unwrap();
    seed_root(root.path());
    let addr = start(root.path()).await;
    let mut socket = connect(addr).await;

    // Upstream keeps `str(value or "")`: a number is its digits, zero and false are empty.
    let agents = json!([
        {"profile": "inbox", "connection_id": 7, "title": 12},
        {"profile": "research", "connection_id": 0},
        {"profile": "research", "connection_id": false},
    ]);
    let reply = call(
        &mut socket,
        1,
        "bot_relay.roster.sync",
        json!({"agents": agents}),
    )
    .await;
    assert_eq!(reply["result"], json!({"count": 1}));
    let file = root.path().join("bot_relay").join("roster.json");
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(stored["agents"][0]["connection_id"], "7");
    assert_eq!(stored["agents"][0]["title"], "12");

    // `true` is `True`, a number past i64 keeps all its digits and a fraction prints as one.
    let agents = json!([{"profile": "inbox", "connection_id": true,
                         "title": 18_446_744_073_709_551_615_u64, "description": 1.5}]);
    let reply = call(
        &mut socket,
        2,
        "bot_relay.roster.sync",
        json!({"agents": agents}),
    )
    .await;
    assert_eq!(reply["result"], json!({"count": 1}));
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let row = &stored["agents"][0];
    assert_eq!(row["connection_id"], "True");
    assert_eq!(row["title"], "18446744073709551615");
    assert_eq!(row["description"], "1.5");
}

/// As upstream reads a profile's config: a model given as an object falls back from an empty
/// `default` to `model`, an empty provider is none, and a model that is neither text nor an
/// object is none; a `profile.yaml` that is not a mapping is no description.
#[tokio::test]
async fn profiles_route_reads_each_model_shape_and_meta_as_upstream_does() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("config.yaml"), "model: root-model\n");
    write(
        &root.path().join("profiles/alpha/config.yaml"),
        "model:\n  default: ''\n  model: alpha-model\n  provider: ''\n",
    );
    write(
        &root.path().join("profiles/beta/config.yaml"),
        "model: 42\n",
    );
    write(
        &root.path().join("profiles/beta/profile.yaml"),
        "- not\n- a mapping\n",
    );
    let addr = start(root.path()).await;
    let reply = get_profiles(addr, &[("x-hermes-session-token", TOKEN)]).await;
    let body: Value = reply.json().await.unwrap();
    let rows = &body["profiles"];
    assert_eq!(names(rows), ["default", "alpha", "beta"]);
    assert_eq!(rows[1]["model"], "alpha-model");
    assert_eq!(rows[1]["provider"], Value::Null);
    assert_eq!(rows[2]["model"], Value::Null);
    assert_eq!(rows[2]["provider"], Value::Null);
    assert_eq!(rows[2]["description"], "");
    assert_eq!(rows[2]["display_name"], "");
}

/// The skill count walks at most 10,000 folders, so a huge skills tree cannot stall the list.
#[tokio::test]
async fn profiles_route_stops_counting_skills_after_ten_thousand_folders() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("config.yaml"), "");
    write(&root.path().join("profiles/wide/config.yaml"), "");
    let skills = root.path().join("profiles/wide/skills");
    for n in 0..10_050 {
        write(
            &skills.join(format!("s{n}/SKILL.md")),
            "---\nname: s\n---\n",
        );
    }
    let addr = start(root.path()).await;
    let reply = get_profiles(addr, &[("x-hermes-session-token", TOKEN)]).await;
    let body: Value = reply.json().await.unwrap();
    // The skills folder itself is the first of the 10,000.
    assert_eq!(body["profiles"][1]["skill_count"], 9_999);
}

/// A folder whose name is not Unicode is not walked, as upstream's walk cannot name it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn profiles_route_skips_a_skill_folder_whose_name_is_not_unicode() {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("config.yaml"), "");
    write(&root.path().join("profiles/odd/config.yaml"), "");
    let skills = root.path().join("profiles/odd/skills");
    write(&skills.join("named/SKILL.md"), "---\nname: named\n---\n");
    let odd = skills.join(std::ffi::OsStr::from_bytes(b"\xff\xfe"));
    write(&odd.join("SKILL.md"), "---\nname: odd\n---\n");
    let addr = start(root.path()).await;
    let reply = get_profiles(addr, &[("x-hermes-session-token", TOKEN)]).await;
    let body: Value = reply.json().await.unwrap();
    assert_eq!(body["profiles"][1]["skill_count"], 1);
}
