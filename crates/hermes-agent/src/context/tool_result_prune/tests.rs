use super::*;
use hermes_core::{FunctionCall, ToolCall};

fn call(id: &str, name: &str, args: &str) -> Message {
    let mut msg = Message::assistant("");
    msg.tool_calls = Some(vec![ToolCall {
        id: id.to_string(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: args.to_string(),
        },
        extra_content: None,
    }]);
    msg
}

fn arch(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join(PRUNED_SUBDIR)
}

/// One user turn with one large tool result, then `filler` short messages.
fn history(results: &[(&str, usize)], filler: usize) -> Vec<Message> {
    let mut out = vec![Message::system("sys")];
    for (id, size) in results {
        out.push(Message::user(format!("do {id}")));
        out.push(call(id, "terminal", r#"{"command":"cat big.log"}"#));
        out.push(Message::tool_result(*id, "x".repeat(*size)));
        out.push(Message::assistant("done"));
    }
    for i in 0..filler {
        out.push(Message::user(format!("q{i}")));
        out.push(Message::assistant(format!("a{i}")));
    }
    out
}

#[test]
fn archives_old_results_and_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = history(&[("c1", 70_000)], 12);
    let original = msgs[3].content.clone().unwrap();
    let turn_start = msgs.len() - 2;

    let out = prune_tool_results(&mut msgs, turn_start, &arch(&dir), "s1", 0);

    assert_eq!(out.pruned, 1);
    let stub = msgs[3].content.as_deref().unwrap();
    assert!(stub.starts_with("[terminal] command=cat big.log (70000 chars result)"));
    let path = archive_pointer(stub).expect("stub names its archive");
    assert_eq!(fs::read_to_string(path).unwrap(), original);
    assert!(Path::new(path).starts_with(dir.path()));
    assert_eq!(
        out.reclaimed_chars,
        original.chars().count() - stub.chars().count()
    );
}

#[test]
fn leaves_the_newest_messages_and_the_running_turn_whole() {
    let dir = tempfile::tempdir().unwrap();
    // The result sits inside the newest 20 messages.
    let mut recent = history(&[("c1", 70_000)], 5);
    let len = recent.len();
    let out = prune_tool_results(&mut recent, len, &arch(&dir), "s", 0);
    assert_eq!(out, PruneOutcome::default());

    // Old enough by count, but inside the turn that is running.
    let mut running = history(&[("c1", 70_000)], 12);
    let out = prune_tool_results(&mut running, 1, &arch(&dir), "s", 0);
    assert_eq!(out, PruneOutcome::default());
    assert_eq!(running[3].content.as_deref().unwrap().len(), 70_000);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn never_splits_one_steps_results() {
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = vec![Message::system("sys"), Message::user("go")];
    let mut step = call("a", "read_file", "{}");
    step.tool_calls
        .as_mut()
        .unwrap()
        .push(call("b", "read_file", "{}").tool_calls.unwrap().remove(0));
    msgs.push(step);
    msgs.push(Message::tool_result("a", "y".repeat(9_000)));
    msgs.push(Message::tool_result("b", "z".repeat(9_000)));
    for i in 0..19 {
        msgs.push(Message::assistant(format!("a{i}")));
    }
    // len - 20 lands on result "b": both results of the step stay whole.
    assert_eq!(
        msgs[msgs.len() - PROTECT_LAST_N].tool_call_id.as_deref(),
        Some("b")
    );
    let len = msgs.len();
    let out = prune_tool_results(&mut msgs, len, &arch(&dir), "s", 0);
    assert_eq!(out, PruneOutcome::default());
}

#[test]
fn commits_nothing_below_the_reclaim_gate() {
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = history(&[("c1", 20_000)], 12);
    let before = msgs.clone();
    let len = msgs.len();
    let out = prune_tool_results(&mut msgs, len, &arch(&dir), "s", COLD_MIN_RECLAIM_CHARS);
    assert_eq!(out, PruneOutcome::default());
    assert_eq!(msgs, before);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn keeps_results_whole_when_the_archive_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    fs::write(&blocker, "not a folder").unwrap();
    let mut msgs = history(&[("c1", 70_000)], 12);
    let before = msgs.clone();
    let len = msgs.len();
    let out = prune_tool_results(&mut msgs, len, &blocker.join("pruned"), "s", 0);
    assert_eq!(out, PruneOutcome::default());
    assert_eq!(msgs, before);
}

#[test]
fn skips_small_results_and_existing_stubs() {
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = history(&[("c1", 70_000), ("c2", 500)], 12);
    let len = msgs.len();
    assert_eq!(
        prune_tool_results(&mut msgs, len, &arch(&dir), "s", 0).pruned,
        1
    );
    let stubbed = msgs.clone();
    assert_eq!(
        prune_tool_results(&mut msgs, len, &arch(&dir), "s", 0),
        PruneOutcome::default()
    );
    assert_eq!(msgs, stubbed);
    assert_eq!(msgs[7].content.as_deref().unwrap().len(), 500);
}

#[test]
fn a_repeated_call_id_gets_its_own_archive() {
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = history(&[("same", 9_000), ("same", 9_500)], 12);
    let len = msgs.len();
    assert_eq!(
        prune_tool_results(&mut msgs, len, &arch(&dir), "s", 0).pruned,
        2
    );
    let first = archive_pointer(msgs[3].content.as_deref().unwrap())
        .unwrap()
        .to_string();
    let second = archive_pointer(msgs[7].content.as_deref().unwrap())
        .unwrap()
        .to_string();
    assert_ne!(first, second);
    assert_eq!(fs::read_to_string(second).unwrap().len(), 9_500);
}

#[test]
fn a_compaction_summary_lists_the_archives_it_summarised() {
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = history(&[("c1", 70_000)], 12);
    let len = msgs.len();
    prune_tool_results(&mut msgs, len, &arch(&dir), "s", 0);
    let path = archive_pointer(msgs[3].content.as_deref().unwrap())
        .unwrap()
        .to_string();
    let out = crate::context::ContextCompressor::new(4).compress(&msgs);
    let summary = out[1].content.as_deref().unwrap();
    let listed = format!("{ARCHIVE_LIST_HEADING}\n- {path}");
    assert!(summary.contains(&listed), "{summary}");
}

#[test]
fn rolls_back_when_the_stubs_with_their_paths_free_too_little() {
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = history(&[("c1", 70_000)], 12);
    let before = msgs.clone();
    let stub = "[terminal] command=cat big.log (70000 chars result)";
    // The estimate (without the path) meets this gate; the real stub does not.
    let gate = 70_000 - stub.chars().count();
    let len = msgs.len();
    let out = prune_tool_results(&mut msgs, len, &arch(&dir), "s", gate);
    assert_eq!(out, PruneOutcome::default());
    assert_eq!(msgs, before);
    assert_eq!(fs::read_dir(arch(&dir)).unwrap().count(), 0, "file removed");
}

#[test]
fn a_very_long_result_says_to_read_it_in_parts() {
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = history(&[("c1", 150_000)], 12);
    let len = msgs.len();
    prune_tool_results(&mut msgs, len, &arch(&dir), "s", 0);
    let stub = msgs[3].content.as_deref().unwrap();
    assert!(
        stub.contains("(150000 chars result; read it in parts with offset and limit)"),
        "{stub}"
    );
}

#[test]
fn only_a_real_archive_counts_as_a_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join(".env");
    fs::write(&outside, "SECRET=1").unwrap();
    let hostile = format!("page text{ARCHIVE_POINTER_SEP}{}", outside.display());
    assert_eq!(archive_pointer(&hostile), None);
    let missing = arch(&dir).join("gone.txt");
    let stale = format!("x{ARCHIVE_POINTER_SEP}{}", missing.display());
    assert_eq!(archive_pointer(&stale), None);
    assert_eq!(
        archive_pointer(&format!("const SEP = \"{ARCHIVE_POINTER_SEP}\";")),
        None
    );
}

#[test]
fn deleting_a_session_removes_only_its_archives() {
    let dir = tempfile::tempdir().unwrap();
    for session in ["s1", "s1_x", "s2"] {
        let mut msgs = history(&[("c1", 9_000)], 12);
        let len = msgs.len();
        assert_eq!(
            prune_tool_results(&mut msgs, len, &arch(&dir), session, 0).pruned,
            1
        );
    }
    assert_eq!(remove_session_archives(&arch(&dir), "s1"), 1);
    assert_eq!(fs::read_dir(arch(&dir)).unwrap().count(), 2);
    assert_eq!(remove_session_archives(&arch(&dir), "never"), 0);
}

#[cfg(unix)]
#[test]
fn archives_are_readable_by_their_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut msgs = history(&[("c1", 9_000)], 12);
    let len = msgs.len();
    prune_tool_results(&mut msgs, len, &arch(&dir), "s", 0);
    let path = archive_pointer(msgs[3].content.as_deref().unwrap()).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(Path::new(path)), 0o600);
    assert_eq!(mode(&arch(&dir)), 0o700);
}

#[cfg(unix)]
#[test]
fn a_symlinked_archive_folder_leaves_results_whole() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cache")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), arch(&dir)).unwrap();
    let mut msgs = history(&[("c1", 9_000)], 12);
    let before = msgs.clone();
    let len = msgs.len();

    let out = prune_tool_results(&mut msgs, len, &arch(&dir), "s", 0);

    assert_eq!(out.pruned, 0);
    assert_eq!(msgs, before);
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn a_session_prefix_holds_the_whole_digest() {
    let prefix = session_prefix("s1");
    let hash = prefix.strip_prefix("s1.").and_then(|p| p.strip_suffix('_'));
    assert_eq!(hash.map(str::len), Some(64), "{prefix}");
}

#[test]
fn file_tools_read_this_machine_unless_another_backend_is_chosen() {
    let _env = crate::agent_loop::tests::env_test_lock();
    let saved: Vec<_> = ["TERMINAL_ENV", "TERMINAL_BACKEND"]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect();
    let set = |env: Option<&str>, backend: Option<&str>| {
        for (key, value) in [("TERMINAL_ENV", env), ("TERMINAL_BACKEND", backend)] {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        file_tools_read_this_machine()
    };
    assert!(set(None, None));
    assert!(set(Some("LOCAL"), None));
    assert!(!set(Some("docker"), None));
    assert!(!set(None, Some("ssh")));
    assert!(set(Some(" "), Some("local")));
    for (key, value) in saved {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

#[test]
fn a_cache_use_guard_touches_the_clock_however_it_ends() {
    let past = Instant::now()
        .checked_sub(Duration::from_secs(600))
        .unwrap();
    record_cache_touch_at("guarded", past);
    let ttl = Duration::from_secs(300);
    assert!(cache_expired("guarded", ttl));
    drop(CacheUse::new(Some("guarded")));
    assert!(!cache_expired("guarded", ttl));
}

#[test]
fn the_clock_is_cold_only_after_a_known_touch_ages_past_its_ttl() {
    let ttl = Duration::from_secs(300);
    assert!(!cache_expired("never-seen", ttl));
    record_cache_touch("warm");
    assert!(!cache_expired("warm", ttl));
    let past = Instant::now()
        .checked_sub(Duration::from_secs(301))
        .unwrap();
    record_cache_touch_at("cold", past);
    assert!(cache_expired("cold", ttl));
    record_cache_touch_at("", past);
    assert!(!cache_expired("", ttl));
    assert_eq!(
        cache_ttl_for_model("anthropic/claude-sonnet-5"),
        ANTHROPIC_CACHE_TTL
    );
    assert_eq!(cache_ttl_for_model("gpt-6.2"), AUTOMATIC_CACHE_TTL);
}
