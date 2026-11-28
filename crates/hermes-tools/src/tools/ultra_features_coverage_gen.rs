use super::*;

use crate::test_env::with_env;

fn parse(s: &str) -> Value {
    serde_json::from_str(s).expect("valid json")
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn write_bad_utf8(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, [0xff_u8, 0xfe, 0xfd, 0x80]).unwrap();
}

fn anchor_err(content: &str, old: &str, new: &str, all: bool) -> ToolError {
    match replace_anchor(content, old, new, all) {
        Ok(_) => panic!("expected replace_anchor to fail"),
        Err(e) => e,
    }
}

fn ufcg_git(dir: &Path, args: &[&str]) -> std::process::Output {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    output
}

#[test]
fn benchmark_ledger_lists_all_items() {
    let v = parse(&benchmark_ledger());
    assert_eq!(v["version"], ULTRA_FEATURE_VERSION);
    assert_eq!(v["rust_only_core"].as_bool(), Some(true));
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 15);
    assert_eq!(items[0]["id"], 1);
    assert_eq!(items[11]["status"], "implemented_js_shell_no_python");
}

#[tokio::test]
async fn handler_execute_dispatches_by_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let state = Arc::new(UltraFeatureState::new(tmp.path().join("state")));
    let ledger = UltraFeatureHandler::new(state.clone(), UltraFeatureKind::BenchmarkLedger)
        .execute(json!({}))
        .await
        .unwrap();
    assert_eq!(ledger, benchmark_ledger());
    let min = UltraFeatureHandler::new(state.clone(), UltraFeatureKind::MinimizeOutput)
        .execute(json!({"output":"error: boom\nok"}))
        .await
        .unwrap();
    assert_eq!(parse(&min)["errors"][0], "error: boom");
    let guard = UltraFeatureHandler::new(state, UltraFeatureKind::StreamRuleGuard)
        .execute(json!({"action":"list"}))
        .await
        .unwrap();
    assert_eq!(parse(&guard)["rules"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn hash_edit_line_range_dry_run_then_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.txt");
    fs::write(&file, "a\nb\nc\n").unwrap();
    let out =
        hash_edit(json!({"path":file,"new_string":"B","start_line":2,"end_line":2,"dry_run":true}))
            .await
            .unwrap();
    let v = parse(&out);
    assert_eq!(v["applied"].as_bool(), Some(false));
    assert_eq!(v["dry_run"].as_bool(), Some(true));
    assert_eq!(v["match_strategy"], "line_range");
    assert_eq!(v["bytes_delta"], 0);
    assert_eq!(fs::read_to_string(&file).unwrap(), "a\nb\nc\n");

    let applied = parse(
        &hash_edit(json!({"path":file,"new_string":"B","start_line":2}))
            .await
            .unwrap(),
    );
    assert_eq!(applied["applied"].as_bool(), Some(true));
    assert_eq!(fs::read_to_string(&file).unwrap(), "a\nB\nc\n");

    let err = hash_edit(json!({"path":file,"new_string":"x","start_line":9}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "a\nB\nc\n");
}

#[test]
fn replace_anchor_strategies_and_errors() {
    let err = anchor_err("abc", "", "x", false);
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let err = anchor_err("x x", "x", "y", false);
    assert!(err.to_string().contains("matched 2 times"), "{err}");

    let all = replace_anchor("x x", "x", "y", true).ok().unwrap();
    assert_eq!(all.content, "y y");
    assert_eq!(all.replacements, 2);
    assert_eq!(all.strategy, "exact");

    let trimmed = replace_anchor("  foo\n  bar\nbaz\n", "foo\nbar", "X", false)
        .ok()
        .unwrap();
    assert_eq!(trimmed.strategy, "line_trimmed");
    assert_eq!(trimmed.content, "Xbaz\n");
    assert_eq!(trimmed.replacements, 1);

    let ws = replace_anchor("a  b\nc\n", "a b", "Z", false).ok().unwrap();
    assert_eq!(ws.strategy, "whitespace_normalized");
    assert_eq!(ws.content, "Zc\n");

    let err = anchor_err("abc", "xyz", "q", false);
    assert!(matches!(err, ToolError::ExecutionFailed(_)), "{err}");
    assert!(find_recovered_anchor("abc", "xyz").is_none());
}

#[test]
fn replace_line_range_handles_valid_and_invalid_ranges() {
    let ok = replace_line_range("a\nb\nc\n", &json!({"start_line":2,"end_line":2}), "B")
        .ok()
        .unwrap();
    assert_eq!(ok.content, "a\nB\nc\n");
    assert_eq!(ok.strategy, "line_range");
    match replace_line_range("a\nb\n", &json!({"start_line":2,"end_line":1}), "x") {
        Ok(_) => panic!("expected invalid range"),
        Err(e) => assert!(e.to_string().contains("invalid line range"), "{e}"),
    }
}

#[tokio::test]
async fn search_resource_scans_directories_and_truncates() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));
    let dir = tmp.path().join("docs");
    write(&dir.join("a.md"), "needle one\nneedle two\n");
    write(&dir.join("b.txt"), "nothing\n");
    write_bad_utf8(&dir.join("bad.txt"));

    let full = parse(
        &search_resource(json!({"uri":dir,"pattern":"needle"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(full["matches"].as_array().unwrap().len(), 2);
    assert_eq!(full["truncated"].as_bool(), Some(false));

    let trunc = parse(
        &search_resource(
            json!({"uri":format!("file://{}", dir.display()),"pattern":"needle","limit":1}),
            &state,
        )
        .await
        .unwrap(),
    );
    assert_eq!(trunc["matches"].as_array().unwrap().len(), 1);
    assert_eq!(trunc["truncated"].as_bool(), Some(true));

    let single = parse(
        &search_resource(
            json!({"uri":dir.join("a.md"),"pattern":"needle","limit":1}),
            &state,
        )
        .await
        .unwrap(),
    );
    assert_eq!(single["truncated"].as_bool(), Some(true));
    assert_eq!(single["matches"][0]["line"], 1);

    let err = search_resource(json!({"uri":dir,"pattern":"("}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
}

#[tokio::test]
async fn read_resource_plain_path_and_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));
    let file = tmp.path().join("plain.txt");
    write(&file, "one\ntwo\n");
    let read = read_resource(json!({"uri":file.to_str().unwrap()}), &state)
        .await
        .unwrap();
    assert_eq!(read, "one\ntwo\n");
    let err = read_resource(
        json!({"uri":tmp.path().join("missing.txt").to_str().unwrap()}),
        &state,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("read path resource"), "{err}");
    assert!(uri_to_local_path("https://example.invalid/x", &state).is_none());
    assert!(uri_to_local_path("relative/thing", &state).is_some());
}

#[tokio::test]
async fn agent_and_special_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));
    subagent_workspace(json!({"action":"create","id":"w1","goal":"g"}), &state)
        .await
        .unwrap();
    subagent_workspace(
        json!({"action":"write","id":"w1","path":"notes/out.md","content":"hello agent"}),
        &state,
    )
    .await
    .unwrap();

    let read = read_resource(json!({"uri":"agent://w1/notes/out.md"}), &state)
        .await
        .unwrap();
    assert_eq!(read, "hello agent");
    let capped = read_resource(
        json!({"uri":"agent://w1/notes/out.md","max_chars":3}),
        &state,
    )
    .await
    .unwrap();
    assert_eq!(capped, "hel...");

    let found = parse(
        &search_resource(json!({"uri":"agent://w1","pattern":"hello agent"}), &state)
            .await
            .unwrap(),
    );
    let matches = found["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert!(matches[0]["path"].as_str().unwrap().ends_with("out.md"));

    let err = read_resource(json!({"uri":"pr://only/two"}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
    let err = read_resource(json!({"uri":"issue://a/b/c/d"}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
    assert_eq!(
        parse_github_spec("owner/repo/7").unwrap(),
        ("owner".to_string(), "repo".to_string(), "7".to_string())
    );

    let err = read_resource(json!({"uri":"conflict://999999"}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)), "{err}");
}

#[test]
fn home_resources_read_skills_sessions_and_memory() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    write(
        &home.join(".codex/skills/ufcg-direct-skill/SKILL.md"),
        "direct skill body",
    );
    write(
        &home.join(".agents/skills/group/ufcg-nested-skill/SKILL.md"),
        "nested skill body",
    );
    write(
        &home.join(".codex/sessions/2026/07/rollout-ufcg-session-42.jsonl"),
        "{\"session\":42}",
    );
    write(
        &home.join(".codex/memories/MEMORY.md"),
        "first line\nRust_only policy\nother\n",
    );
    let state = UltraFeatureState::new(tmp.path().join("state"));

    let (direct, nested, missing, session, no_session, memory, no_match) =
        with_env(&[("HOME", Some(home.as_path()))], || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let direct = rt.block_on(read_resource(
                json!({"uri":"skill://ufcg-direct-skill"}),
                &state,
            ));
            let nested = rt.block_on(read_resource(
                json!({"uri":"skill://ufcg-nested-skill"}),
                &state,
            ));
            let missing = read_skill_resource("ufcg-missing-skill-zz");
            let session = rt.block_on(read_resource(
                json!({"uri":"session://ufcg-session-42"}),
                &state,
            ));
            let no_session = read_session_resource("ufcg-absent-session-zz");
            let memory = rt.block_on(read_resource(json!({"uri":"memory://rust_only"}), &state));
            let no_match = read_memory_resource("zzzqqq");
            (
                direct, nested, missing, session, no_session, memory, no_match,
            )
        });

    assert_eq!(direct.unwrap(), "direct skill body");
    assert_eq!(nested.unwrap(), "nested skill body");
    assert!(matches!(missing, Err(ToolError::NotFound(_))));
    assert_eq!(session.unwrap(), "{\"session\":42}");
    assert!(matches!(no_session, Err(ToolError::NotFound(_))));
    assert_eq!(memory.unwrap(), "2:Rust_only policy\n");
    assert_eq!(no_match.unwrap(), "No MEMORY.md lines matched.");
    assert!(find_named_skill(&tmp.path().join("absent"), "x").is_none());
}

#[test]
fn github_repo_from_ssh_remote_and_rejects_other_hosts() {
    let ssh = tempfile::tempdir().unwrap();
    ufcg_git(ssh.path(), &["init", "-q"]);
    ufcg_git(
        ssh.path(),
        &["remote", "add", "origin", "git@github.com:croft/manor.git"],
    );
    assert_eq!(
        github_repo_from_remote(ssh.path()).unwrap(),
        ("croft".to_string(), "manor".to_string())
    );

    let other = tempfile::tempdir().unwrap();
    ufcg_git(other.path(), &["init", "-q"]);
    ufcg_git(
        other.path(),
        &["remote", "add", "origin", "https://gitlab.com/a/b.git"],
    );
    let err = github_repo_from_remote(other.path()).unwrap_err();
    assert!(
        err.to_string().contains("cannot infer GitHub repo"),
        "{err}"
    );
}

#[tokio::test]
async fn conflict_actions_cover_base_theirs_manual_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("work");
    let file = dir.join("c.txt");
    write(
        &file,
        "top\n<<<<<<< ours\nleft\n||||||| base\norig\n=======\nright\n>>>>>>> theirs\nend\n<<<<<<< dangling\nnever closed\n",
    );
    write_bad_utf8(&dir.join("bad.txt"));
    let original = fs::read_to_string(&file).unwrap();

    let listed = parse(
        &resolve_conflict(json!({"action":"list","path":dir}))
            .await
            .unwrap(),
    );
    let conflicts = listed["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["base"], "orig");

    let read = parse(
        &resolve_conflict(json!({"action":"read","path":dir,"index":0}))
            .await
            .unwrap(),
    );
    assert_eq!(read["theirs"], "right");
    let err = resolve_conflict(json!({"action":"read","path":dir,"index":5}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)), "{err}");

    let err = resolve_conflict(json!({"action":"resolve","path":dir,"choice":"ours"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
    let err = resolve_conflict(json!({"action":"resolve","path":dir,"index":0,"choice":"bogus"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown conflict choice"), "{err}");

    let dry = parse(
        &resolve_conflict(
            json!({"action":"resolve","path":dir,"index":0,"choice":"base","dry_run":true}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(dry["resolved"].as_bool(), Some(false));
    assert_ne!(dry["old_hash"], dry["new_hash"]);
    assert_eq!(fs::read_to_string(&file).unwrap(), original);

    let err = resolve_conflict(json!({"action":"resolve","path":dir,"index":0,"choice":"manual"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let theirs = parse(
        &resolve_conflict(json!({"action":"resolve","path":dir,"index":0,"choice":"theirs"}))
            .await
            .unwrap(),
    );
    assert_eq!(theirs["resolved"].as_bool(), Some(true));
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "top\nright\nend\n<<<<<<< dangling\nnever closed\n"
    );

    let err = resolve_conflict(json!({"action":"explode","path":dir}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let other = tempfile::tempdir().unwrap();
    let plain = other.path().join("p.txt");
    write(&plain, "<<<<<<< a\nx\n=======\ny\n>>>>>>> b\n");
    let err = resolve_conflict(json!({"action":"resolve","path":plain,"index":0,"choice":"base"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no base marker"), "{err}");
    resolve_conflict(
        json!({"action":"resolve","path":plain,"index":0,"choice":"manual","content":"merged"}),
    )
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(&plain).unwrap(), "merged");
}

#[tokio::test]
async fn lsp_workspace_symbols_diagnostics_actions_and_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("src");
    write(
        &root.join("a.rs"),
        "fn alpha() {}\nfn beta() { alpha(); }\nstruct Gamma;\n// TODO later\n",
    );
    write(
        &root.join("b.py"),
        "def delta():\n    pass\nclass Epsilon:\n    pass\n",
    );
    write(
        &root.join("c.ts"),
        "export interface Zeta {}\nexport type Eta = number;\nexport const theta = 1;\nexport class Iota {}\nexport function kappa() {}\n",
    );
    write(&root.join("e.js"), "function nu() {}\n");
    write(
        &root.join("d.go"),
        "func lambda() {\n}\ntype Mu struct {\n}\n",
    );
    write(&root.join("broken.rs"), "fn broken( {\n]\n");
    write_bad_utf8(&root.join("bad.rs"));
    write(&root.join("notes.md"), "fn not_symbol() {}\n");

    let limited = parse(
        &lsp_inspect(json!({"action":"workspace_symbols","path":root,"limit":2}))
            .await
            .unwrap(),
    );
    assert_eq!(limited["symbols"].as_array().unwrap().len(), 2);
    assert_eq!(limited["truncated"].as_bool(), Some(true));

    let all = parse(
        &lsp_inspect(json!({"action":"workspace_symbols","path":root,"limit":500}))
            .await
            .unwrap(),
    );
    assert_eq!(all["truncated"].as_bool(), Some(false));
    let names: BTreeSet<String> = all["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["symbol"]["name"].as_str().unwrap().to_string())
        .collect();
    for expected in [
        "alpha", "beta", "Gamma", "delta", "Epsilon", "Zeta", "Eta", "theta", "Iota", "kappa",
        "nu", "lambda", "Mu", "broken",
    ] {
        assert!(names.contains(expected), "missing {expected}: {names:?}");
    }
    assert!(!names.contains("not_symbol"));

    let diags = parse(
        &lsp_inspect(json!({"action":"diagnostics","path":root.join("broken.rs")}))
            .await
            .unwrap(),
    );
    let messages: Vec<String> = diags["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["message"].as_str().unwrap().to_string())
        .collect();
    assert!(messages
        .iter()
        .any(|m| m == "unmatched closing delimiter ]"));
    assert!(messages.iter().any(|m| m == "unclosed delimiter ("));
    assert!(!diagnostics_for_path(&root).is_empty());

    let dir_actions = parse(
        &lsp_inspect(json!({"action":"code_action_preview","path":root}))
            .await
            .unwrap(),
    );
    let kinds: Vec<&str> = dir_actions["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"quickfix.diagnostics"));
    assert!(kinds.contains(&"refactor.todo"));
    let file_actions = code_action_preview(&root.join("a.rs"));
    assert_eq!(file_actions.len(), 1);
    assert_eq!(file_actions[0]["kind"], "refactor.todo");

    let err = lsp_inspect(json!({"action":"hover","path":root}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let bad_ref = parse(
        &lsp_inspect(json!({"action":"references","path":root,"symbol":"a-b"}))
            .await
            .unwrap(),
    );
    assert!(bad_ref["references"].as_array().unwrap().is_empty());
    assert_eq!(find_references(&root.join("a.rs"), "Gamma", 10).len(), 1);
    assert_eq!(find_references(&root, "alpha", 1).len(), 1);

    let err = lsp_inspect(
        json!({"action":"rename_apply","path":root,"symbol":"beta","replacement":"1bad"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let renamed = parse(
        &lsp_inspect(
            json!({"action":"rename_apply","path":root,"symbol":"beta","replacement":"betaRenamed"}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(renamed["applied"].as_bool(), Some(true));
    assert_eq!(renamed["files"].as_array().unwrap().len(), 1);
    assert!(fs::read_to_string(root.join("a.rs"))
        .unwrap()
        .contains("fn betaRenamed()"));
}

#[tokio::test]
async fn debug_probe_plans_packets_and_errors() {
    let packet = parse(
        &debug_probe(json!({"action":"initialize_packet"}))
            .await
            .unwrap(),
    );
    let raw = packet["packet"].as_str().unwrap().to_string();
    let (header, body) = raw.split_once("\r\n\r\n").unwrap();
    assert_eq!(header, format!("Content-Length: {}", body.len()));
    assert_eq!(parse(body)["command"], "initialize");

    let launch = parse(
        &debug_probe(json!({"action":"launch_plan","program":"/bin/app"}))
            .await
            .unwrap(),
    );
    assert_eq!(launch["adapter"], "lldb");
    assert_eq!(launch["program"], "/bin/app");
    assert_eq!(launch["sequence"].as_array().unwrap().len(), 8);

    let bp = parse(
        &debug_probe(json!({"action":"breakpoint_plan","program":"p"}))
            .await
            .unwrap(),
    );
    assert_eq!(bp["request"], "setBreakpoints");

    let err = debug_probe(json!({"action":"step"})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
    let err = debug_probe(json!({"action":"connect_initialize"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("requires port"), "{err}");

    let adapters = parse(&debug_probe(json!({})).await.unwrap());
    let list = adapters["adapters"].as_array().unwrap();
    assert_eq!(list.len(), 5);
    assert!(list.iter().all(|a| a["available"].is_boolean()));
}

#[tokio::test]
async fn debug_connect_initialize_reads_adapter_reply() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = sock.read(&mut buf).await.unwrap();
        sock.write_all(b"Content-Length: 2\r\n\r\n{}")
            .await
            .unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    let out = parse(
        &debug_probe(json!({"action":"connect_initialize","host":"127.0.0.1","port":port}))
            .await
            .unwrap(),
    );
    assert_eq!(out["connected"].as_bool(), Some(true));
    assert!(out["response"].as_str().unwrap().contains("Content-Length"));
    let received = server.await.unwrap();
    assert!(received.starts_with("Content-Length:"), "{received}");
}

async fn create_txn(state: &UltraFeatureState, kind: &str, payload: Value) -> String {
    let created = transaction_preview(
        json!({"action":"create","kind":kind,"payload":payload}),
        state,
    )
    .await
    .unwrap();
    parse(&created)["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn transactions_list_reject_file_write_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));
    let target = tmp.path().join("out.txt");

    let write_id = create_txn(
        &state,
        "file_write",
        json!({"path":target,"content":"written\n"}),
    )
    .await;
    let preview = parse(
        &transaction_preview(json!({"action":"create","kind":"notes"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(preview["payload"], json!({}));
    assert_eq!(preview["status"], "pending");
    let preview_id = preview["id"].as_str().unwrap().to_string();

    fs::write(state.transactions_dir().join("broken.json"), "not json").unwrap();
    fs::write(state.transactions_dir().join("readme.txt"), "ignored").unwrap();
    let listed = parse(
        &transaction_preview(json!({"action":"list"}), &state)
            .await
            .unwrap(),
    );
    let txns = listed["transactions"].as_array().unwrap();
    assert_eq!(txns.len(), 2);
    assert!(txns[0]["id"].as_str() < txns[1]["id"].as_str());

    let accepted = parse(
        &transaction_preview(json!({"action":"accept","id":write_id}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(fs::read_to_string(&target).unwrap(), "written\n");

    let err = transaction_preview(json!({"action":"accept","id":preview_id}), &state)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("preview-only"), "{err}");

    let rejected = parse(
        &transaction_preview(json!({"action":"reject","id":preview_id}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(rejected["status"], "rejected");
    let read = parse(
        &transaction_preview(json!({"action":"read","id":preview_id}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(read["status"], "rejected");

    for (kind, payload, expected) in [
        ("file_write", json!({}), "payload.path"),
        ("file_write", json!({"path":target}), "payload.content"),
        ("content_replace", json!({}), "payload.path"),
        (
            "content_replace",
            json!({"path":target}),
            "payload.old_string",
        ),
    ] {
        let id = create_txn(&state, kind, payload).await;
        let err = transaction_preview(json!({"action":"accept","id":id}), &state)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)), "{kind}: {err}");
        assert!(err.to_string().contains(expected), "{kind}: {err}");
    }
    assert_eq!(fs::read_to_string(&target).unwrap(), "written\n");

    let err = transaction_preview(json!({"action":"purge"}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
}

#[tokio::test]
async fn ast_search_filters_limits_and_replaces() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("code");
    let lib = root.join("lib.rs");
    write(
        &lib,
        "pub fn handle_one() {}\npub fn handle_two() {}\npub struct HandleThree;\nfn caller() { handle_one(); handle_two(); }\n",
    );
    write_bad_utf8(&root.join("bad.rs"));

    let filtered = parse(
        &ast_search(json!({"path":root,"pattern":"(?i)handle","symbol_kind":"function"}))
            .await
            .unwrap(),
    );
    let matches = filtered["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2);
    assert!(matches.iter().all(|m| m["symbol"]["kind"] == "function"));

    let limited = parse(
        &ast_search(json!({"path":root,"pattern":"(?i)handle","limit":1}))
            .await
            .unwrap(),
    );
    assert_eq!(limited["matches"].as_array().unwrap().len(), 1);
    assert_eq!(limited["truncated"].as_bool(), Some(true));

    let err = ast_search(json!({"action":"frobnicate","path":root,"pattern":"x"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown ast action"), "{err}");
    let err = ast_search(
        json!({"action":"replace","path":root,"pattern":"handle","replacement":"not-ident"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let original = fs::read_to_string(&lib).unwrap();
    let preview = parse(
        &ast_search(
            json!({"action":"replace","path":root,"pattern":"^handle_one$","replacement":"handle_first"}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(preview["applied"].as_bool(), Some(false));
    let touched = preview["touched"].as_array().unwrap();
    assert_eq!(touched.len(), 1);
    assert_eq!(touched[0]["replacements"], 2);
    assert_eq!(fs::read_to_string(&lib).unwrap(), original);

    let applied = parse(
        &ast_search(json!({"action":"replace","path":root,"pattern":"^handle_one$","replacement":"handle_first","dry_run":false}))
            .await
            .unwrap(),
    );
    assert_eq!(applied["applied"].as_bool(), Some(true));
    let now = fs::read_to_string(&lib).unwrap();
    assert!(now.contains("fn handle_first()"));
    assert!(!now.contains("handle_one"));
}

#[tokio::test]
async fn stream_rules_add_list_remove_evaluate_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));

    let err = stream_rule_guard(json!({"action":"add","pattern":"("}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let added = parse(
        &stream_rule_guard(
            json!({"action":"add","id":"r1","pattern":"stop","effect":"abort","message":"halt"}),
            &state,
        )
        .await
        .unwrap(),
    );
    assert_eq!(added["rules"].as_array().unwrap().len(), 4);
    assert!(state.rules_path().is_file());

    let listed = parse(
        &stream_rule_guard(json!({"action":"list"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(listed["rules"].as_array().unwrap().len(), 4);

    let eval = parse(
        &stream_rule_guard(
            json!({"action":"evaluate","text":"please stop now"}),
            &state,
        )
        .await
        .unwrap(),
    );
    assert_eq!(eval["verdict"], "abort");

    let advice = parse(
        &advisor_watch(json!({"transcript":"we must stop here"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(advice["blocked"].as_bool(), Some(true));

    let removed = parse(
        &stream_rule_guard(json!({"action":"remove","id":"r1"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(removed["rules"].as_array().unwrap().len(), 3);

    let auto = parse(
        &stream_rule_guard(json!({"action":"add","pattern":"zzz"}), &state)
            .await
            .unwrap(),
    );
    let rules = auto["rules"].as_array().unwrap();
    let last = rules.last().unwrap();
    assert!(last["id"].as_str().unwrap().starts_with("rule-"));
    assert_eq!(last["effect"], "warn");
    assert_eq!(last["message"], "stream guard matched");

    let err = stream_rule_guard(json!({"action":"explode"}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let default_eval = parse(&stream_rule_guard(json!({}), &state).await.unwrap());
    assert_eq!(default_eval["verdict"], "allow");

    fs::write(state.rules_path(), "garbage").unwrap();
    let err = stream_rule_guard(json!({"action":"list"}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::ExecutionFailed(_)), "{err}");
}

fn rule(id: &str, pattern: &str, effect: &str) -> StreamRule {
    StreamRule {
        id: id.into(),
        pattern: pattern.into(),
        effect: effect.into(),
        message: "m".into(),
    }
}

#[test]
fn evaluate_stream_rules_verdict_precedence() {
    let v = evaluate_stream_rules(
        "x",
        &[
            rule("bad", "(", "abort"),
            rule("a", "x", "retry"),
            rule("b", "x", "inject"),
            rule("c", "x", "warn"),
        ],
    );
    assert_eq!(v["verdict"], "retry");
    assert_eq!(v["matches"].as_array().unwrap().len(), 3);

    let v = evaluate_stream_rules("x", &[rule("a", "x", "inject"), rule("b", "x", "warn")]);
    assert_eq!(v["verdict"], "inject");

    let v = evaluate_stream_rules("x", &[rule("a", "x", "abort"), rule("b", "x", "retry")]);
    assert_eq!(v["verdict"], "abort");

    let v = evaluate_stream_rules("y", &[rule("a", "x", "abort")]);
    assert_eq!(v["verdict"], "allow");
    assert!(v["matches"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn advisor_reports_risks_and_drift() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));
    let out = parse(
        &advisor_watch(
            json!({
                "transcript":"Implemented the placeholder; tests passed but one failed. cargo test ran.",
                "objective":"deploy rocket"
            }),
            &state,
        )
        .await
        .unwrap(),
    );
    let kinds: Vec<&str> = out["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert!(!kinds.contains(&"verification_required"));
    assert!(kinds.contains(&"incomplete_implementation"));
    assert!(kinds.contains(&"contradictory_evidence"));
    assert!(kinds.contains(&"objective_drift"));
    assert_eq!(out["blocked"].as_bool(), Some(false));

    let err = advisor_watch(json!({}), &state).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
}

#[tokio::test]
async fn subagent_list_read_write_remove_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));
    subagent_workspace(json!({"action":"create","id":"a1","goal":"one"}), &state)
        .await
        .unwrap();
    let auto = parse(&subagent_workspace(json!({}), &state).await.unwrap());
    let auto_id = auto["manifest"]["id"].as_str().unwrap().to_string();
    assert!(auto_id.starts_with("agent-"), "{auto_id}");
    write(&state.agents_dir().join("junk/manifest.json"), "nope");
    fs::create_dir_all(state.agents_dir().join("empty")).unwrap();

    let listed = parse(
        &subagent_workspace(json!({"action":"list"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(listed["agents"].as_array().unwrap().len(), 2);

    let manifest = parse(
        &subagent_workspace(json!({"action":"read","id":"a1"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(manifest["goal"], "one");
    assert_eq!(manifest["git_worktree"].as_bool(), Some(false));

    let err = subagent_workspace(
        json!({"action":"write","id":"a1","path":"../../escape.txt","content":"x"}),
        &state,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("escaped workspace"), "{err}");

    let removed = parse(
        &subagent_workspace(json!({"action":"remove","id":"a1"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(removed["removed"].as_bool(), Some(true));
    assert!(!state.agents_dir().join("a1").exists());

    let err = subagent_workspace(json!({"action":"remove","id":"a1"}), &state)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("read subagent manifest"), "{err}");

    let err = subagent_workspace(json!({"action":"fork"}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");
}

#[tokio::test]
async fn subagent_git_workspace_tracks_its_branch_and_removes_only_its_checkout() {
    const CHILD: &str = "HERMES_SUBAGENT_GIT_WORKSPACE_TEST_ROOT";
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let fixture = tempfile::tempdir().unwrap();
        let config = fixture.path().join("git-config");
        fs::write(&config, "").unwrap();
        let module = module_path!().split_once("::").unwrap().1;
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .env_clear()
            .env(CHILD, fixture.path())
            .env("HOME", fixture.path())
            .env("USERPROFILE", fixture.path())
            .env("GIT_CONFIG_GLOBAL", config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(fixture.path())
            .args([
                "--exact",
                &format!("{module}::subagent_git_workspace_tracks_its_branch_and_removes_only_its_checkout"),
            ])
            .kill_on_drop(true);
        for key in ["PATH", "SystemRoot", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        let output = tokio::time::timeout(Duration::from_secs(15), child.output())
            .await
            .expect("isolated Git fixture timed out")
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "isolated Git fixture failed: {output:?}"
        );
        return;
    };

    ufcg_git(&root, &["init", "--template=", "-q"]);
    write(&root.join("notes.txt"), "Field notes\n");
    ufcg_git(&root, &["add", "notes.txt"]);
    ufcg_git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "Initial notes",
        ],
    );
    let state = UltraFeatureState::new(root.join("state"));
    let branch = "ryan/fixture-notes";
    let created = parse(
        &subagent_workspace(
            json!({ "action": "create", "id": "notes", "git_worktree": true, "branch": branch }),
            &state,
        )
        .await
        .unwrap(),
    );
    let checkout = state.agents_dir().join("notes");
    assert_eq!(created["manifest"]["branch"], branch);
    assert_eq!(created["manifest"]["git_worktree"], true);
    assert_eq!(
        fs::read_to_string(checkout.join("notes.txt"))
            .unwrap()
            .trim(),
        "Field notes"
    );
    let branch_output = ufcg_git(&checkout, &["branch", "--show-current"]);
    assert_eq!(
        String::from_utf8_lossy(&branch_output.stdout).trim(),
        branch
    );

    let duplicate = subagent_workspace(
        json!({ "action": "create", "id": "duplicate", "git_worktree": true, "branch": branch }),
        &state,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(duplicate, ToolError::ExecutionFailed(detail) if detail.contains("git worktree add exited"))
    );
    assert!(!state.agents_dir().join("duplicate").exists());

    let removed = parse(
        &subagent_workspace(json!({ "action": "remove", "id": "notes" }), &state)
            .await
            .unwrap(),
    );
    assert_eq!(removed["removed"], true);
    assert!(!checkout.exists());
    assert!(root.join("notes.txt").is_file());
    let worktrees = ufcg_git(&root, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        String::from_utf8_lossy(&worktrees.stdout)
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "only the original checkout should remain registered"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn eval_kernel_shell_session_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));

    let err = eval_kernel(json!({"language":"python","code":"1"}), &state)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Python is unsupported"), "{err}");
    let err = eval_kernel(json!({"language":"sh","session":"s1"}), &state)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err}");

    let first = parse(
        &eval_kernel(
            json!({"action":"run","session":"s1","language":"sh","code":"echo hi"}),
            &state,
        )
        .await
        .unwrap(),
    );
    let output = parse(first["output"].as_str().unwrap());
    assert_eq!(output["stdout"], "hi\n");
    assert_eq!(output["exit_code"], 0);

    let second = parse(
        &eval_kernel(
            json!({"session":"s1","language":"sh","code":"echo second","timeout":5}),
            &state,
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        parse(second["output"].as_str().unwrap())["stdout"],
        "hi\nsecond\n"
    );

    let read = parse(
        &eval_kernel(json!({"action":"read","session":"s1"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(read["language"], "sh");
    assert_eq!(read["snippets"].as_array().unwrap().len(), 2);

    let err = eval_kernel(
        json!({"session":"s1","language":"javascript","code":"1"}),
        &state,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("kernel language is sh"), "{err}");

    let reset = parse(
        &eval_kernel(json!({"action":"reset","session":"s1"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(reset["reset"].as_bool(), Some(true));
    let fresh = parse(
        &eval_kernel(json!({"action":"read","session":"s1"}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(fresh["language"], "javascript");
    assert!(fresh["snippets"].as_array().unwrap().is_empty());

    let err = eval_kernel(
        json!({"session":"r1","language":"ruby","code":"puts 1"}),
        &state,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("unsupported eval language"),
        "{err}"
    );
    assert!(read_kernel(&state.kernels_dir().join("r1.json")).is_none());

    let err = eval_kernel(json!({"action":"compile"}), &state)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown eval action"), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn eval_kernel_javascript_runs_or_reports_missing_node() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));
    let result = eval_kernel(
        json!({"session":"js1","language":"javascript","code":"console.log('js-ok')"}),
        &state,
    )
    .await;
    if command_available("node").await {
        let out = parse(&result.unwrap());
        let inner = parse(out["output"].as_str().unwrap());
        assert!(inner["stdout"].as_str().unwrap().contains("js-ok"));
        assert!(state.kernels_dir().join("js1.js").is_file());
    } else {
        let err = result.unwrap_err();
        assert!(err.to_string().contains("node is required"), "{err}");
    }
}

#[tokio::test]
async fn run_command_with_timeout_reports_spawn_failure() {
    let err = run_command_with_timeout("ufcg-definitely-missing-binary-zz", &[], 5)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::ExecutionFailed(_)), "{err}");
    assert!(err.to_string().contains("failed"), "{err}");
}

#[test]
fn minimize_output_tool_extracts_sections() {
    let out = parse(&minimize_output_tool(json!({
        "tool":"git",
        "output":"M src/main.rs\nREADME\nerror: x\nwarning: y\nwarn: z\ntest a ... FAILED\nfailure here\n",
        "max_lines":2,
        "max_chars":100000
    })));
    assert_eq!(out["tool"], "git");
    assert_eq!(out["changed_files"], json!(["M src/main.rs"]));
    assert_eq!(out["errors"], json!(["error: x"]));
    assert_eq!(out["warnings"], json!(["warning: y", "warn: z"]));
    assert_eq!(
        out["failures"],
        json!(["test a ... FAILED", "failure here"])
    );
    assert_eq!(out["tail"], json!(["test a ... FAILED", "failure here"]));
    let minimized = out["minimized"].as_str().unwrap();
    assert!(minimized.contains("## changed_files\n- M src/main.rs\n"));
    assert!(minimized.contains("## failures\n"));

    let generic = minimize_output("generic", "modified: docs/x.md\nplain line\n", 10, 10);
    assert_eq!(
        generic.changed_files,
        vec!["modified: docs/x.md".to_string()]
    );
    assert!(generic.minimized.ends_with("..."));
    assert_eq!(generic.minimized.chars().count(), 13);

    let many: String = (0..50).map(|i| format!("error {i}\n")).collect();
    let capped = minimize_output("cargo", &many, 5, 100_000);
    assert_eq!(capped.errors.len(), 40);
    assert_eq!(capped.tail.len(), 5);

    assert!(looks_like_path_line("lib.toml"));
    assert!(!looks_like_path_line("nothing here"));
}

#[tokio::test]
async fn first_run_inherit_scans_and_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let state = UltraFeatureState::new(tmp.path().join("state"));
    let root = tmp.path().join("ws");
    write(&root.join("AGENTS.md"), "agent rules");
    write(&root.join(".cursor/rules/a.md"), "cursor rule");
    write(&root.join(".vscode/settings.json"), "{}");

    let scan = parse(
        &first_run_inherit(json!({"path":root,"max_chars_per_file":1}), &state)
            .await
            .unwrap(),
    );
    let surfaces: Vec<&str> = scan["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["surface"].as_str().unwrap())
        .collect();
    assert_eq!(surfaces, vec!["codex-agents", "cursor-rules", "vscode"]);
    assert_eq!(scan["findings"][0]["excerpt"], "agent rules");

    let imported = parse(
        &first_run_inherit(json!({"action":"import","path":root}), &state)
            .await
            .unwrap(),
    );
    assert_eq!(
        imported["imported_path"].as_str().unwrap(),
        state.import_path().to_str().unwrap()
    );
    let doc = fs::read_to_string(state.import_path()).unwrap();
    assert!(doc.starts_with("# Imported Agent Context"));
    assert!(doc.contains("cursor rule"));

    let err = first_run_inherit(json!({"action":"export","path":root}), &state)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown inherit action"), "{err}");
}

#[test]
fn file_collection_limits_and_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    write(&root.join("a/x.md"), "x");
    write(&root.join("b/y.md"), "y");
    write(&root.join("target/z.md"), "z");
    write(&root.join("node_modules/w.md"), "w");

    let limited = collect_text_files(&root, 1);
    assert_eq!(limited.len(), 1);
    let all = collect_text_files(&root, 100);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|p| !p.to_string_lossy().contains("target")));
    assert!(should_skip_dir(Path::new("/x/__pycache__")));
    assert!(!should_skip_dir(Path::new("/x/src")));

    let mut found = Vec::new();
    collect_matching_files(&root, ".md", &mut found, 1);
    assert_eq!(found.len(), 1);
    let mut none = Vec::new();
    collect_matching_files(&tmp.path().join("missing"), "x", &mut none, 5);
    assert!(none.is_empty());
}

#[test]
fn small_helpers_behave() {
    assert_eq!(fold_path(PathBuf::from("a/./b/../c")), PathBuf::from("a/c"));
    assert_eq!(fold_path(PathBuf::from("./..")), PathBuf::from("."));
    assert_eq!(
        clean_join(Path::new("/base/dir"), "../other"),
        PathBuf::from("/base/other")
    );
    assert!(!looks_like_identifier(""));
    assert!(looks_like_identifier("_a1"));
    assert!(!looks_like_identifier("1a"));
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(normalize_ws("  a \n b  "), "a b");
    assert_eq!(paginate_lines("a\nb", None, None), "a\nb");
    assert_eq!(paginate_lines("a\nb\nc", Some(2), None), "b\nc");
    assert_eq!(split_once_or_all("abc", '/'), ("abc", ""));
    assert_eq!(safe_id("a/b c"), "a-b-c");
    assert_eq!(language_for_path(Path::new("x.tsx")), "typescript");
    assert_eq!(language_for_path(Path::new("x.unknown")), "text");
    let err = to_tool_err("boom");
    assert!(matches!(err, ToolError::ExecutionFailed(ref m) if m == "boom"));
    assert_eq!(truncate_chars("abcdef", 3), "abc...");
}
