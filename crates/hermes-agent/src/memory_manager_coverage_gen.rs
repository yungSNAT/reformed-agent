use super::*;
use serde_json::json;

// ---------------------------------------------------------------------------
// Test providers
// ---------------------------------------------------------------------------

struct Bare;

impl MemoryProviderPlugin for Bare {
    fn name(&self) -> &str {
        "bare"
    }
}

struct Recorder {
    name: String,
    available: bool,
    compress: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl Recorder {
    fn new(
        name: &str,
        available: bool,
        compress: &str,
        log: &Arc<Mutex<Vec<String>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            available,
            compress: compress.to_string(),
            log: Arc::clone(log),
        })
    }

    fn push(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }
}

impl MemoryProviderPlugin for Recorder {
    fn name(&self) -> &str {
        &self.name
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        self.push(format!("init:{}:{}:{}", self.name, session_id, hermes_home));
    }

    fn shutdown(&self) {
        self.push(format!("shutdown:{}", self.name));
    }

    fn on_delegation(&self, task: &str, sub_agent_id: &str) {
        self.push(format!("delegate:{}:{}:{}", self.name, task, sub_agent_id));
    }

    fn on_session_end(&self, messages: &[Value]) {
        self.push(format!("end:{}:{}", self.name, messages.len()));
    }

    fn on_pre_compress(&self, _messages: &[Value]) -> String {
        self.compress.clone()
    }

    fn is_available(&self) -> bool {
        self.available
    }

    fn get_config_schema(&self) -> Option<Value> {
        if self.available {
            Some(json!({"type": "object"}))
        } else {
            None
        }
    }
}

struct ToolProvider {
    name: &'static str,
    tools: Vec<Value>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl ToolProvider {
    fn new(name: &'static str, tools: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            name,
            tools,
            calls: Mutex::new(Vec::new()),
        })
    }
}

impl MemoryProviderPlugin for ToolProvider {
    fn name(&self) -> &str {
        self.name
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        self.tools.clone()
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        self.calls
            .lock()
            .unwrap()
            .push((tool_name.to_string(), args.clone()));
        format!("{}:{}", self.name, tool_name)
    }
}

struct PanicOnEnd;

impl MemoryProviderPlugin for PanicOnEnd {
    fn name(&self) -> &str {
        "panicky"
    }

    fn on_session_end(&self, _messages: &[Value]) {
        panic!("session end boom");
    }
}

fn new_log() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------------------
// Default trait methods
// ---------------------------------------------------------------------------

#[test]
fn default_trait_methods_have_expected_behaviour() {
    let p = Bare;
    let out = p.handle_tool_call("some_tool", &json!({"a": 1}));
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["error"], "memory_provider_tool_not_implemented");
    assert_eq!(parsed["tool"], "some_tool");
    assert!(parsed["hint"]
        .as_str()
        .unwrap()
        .contains("handle_tool_call"));

    p.initialize("session", "/tmp/hermes-home");
    p.shutdown();
    p.on_session_end(&[json!({"role": "user"})]);
    assert_eq!(p.on_pre_compress(&[json!("x")]), "");
    p.on_memory_write("add", "memory", "content");
    assert!(p.is_available());
    assert!(p.get_config_schema().is_none());
    assert_eq!(p.save_config(&json!({"k": "v"})), Ok(()));
    p.on_delegation("task", "sub");
    assert!(p.get_tool_schemas().is_empty());
}

// ---------------------------------------------------------------------------
// Redaction / scrubber helpers
// ---------------------------------------------------------------------------

#[test]
fn redact_memory_value_keeps_non_text_scalars() {
    assert_eq!(redact_memory_value(&json!(42)), json!(42));
    assert_eq!(redact_memory_value(&json!(true)), json!(true));
    assert_eq!(redact_memory_value(&Value::Null), Value::Null);
    assert_eq!(
        redact_memory_value(&json!({"n": 1.5, "list": [null, false]})),
        json!({"n": 1.5, "list": [null, false]})
    );
}

#[test]
fn scrubber_feed_empty_returns_empty() {
    let mut s = StreamingContextScrubber::new();
    assert_eq!(s.feed(""), "");
    assert_eq!(s.flush(), "");
}

#[test]
fn scrubber_holds_open_tag_at_end_of_buffer() {
    let mut s = StreamingContextScrubber::new();
    assert_eq!(s.feed("hello <memory-context>"), "hello ");
    assert_eq!(s.flush(), "<memory-context>");
}

#[test]
fn scrubber_inline_open_tag_with_newline_not_preceded_by_newline_is_kept() {
    let mut s = StreamingContextScrubber::new();
    let out = s.feed("abc<memory-context>\nfoo");
    assert_eq!(out, "abc<memory-context>\nfoo");
    assert_eq!(s.flush(), "");
}

#[test]
fn scrubber_removes_block_after_newline() {
    let mut s = StreamingContextScrubber::new();
    let out = s.feed("abc\n<memory-context>\nsecret</memory-context>tail");
    assert_eq!(out, "abc\ntail");
    assert_eq!(s.flush(), "");
}

#[test]
fn build_memory_context_block_strips_prewrapped_tags() {
    let block = build_memory_context_block("<memory-context>remembered fact</memory-context>");
    assert!(block.starts_with("<memory-context>\n[System note"));
    assert!(block.ends_with("\nremembered fact\n</memory-context>"));
    assert_eq!(block.matches("<memory-context>").count(), 1);
    assert_eq!(block.matches("</memory-context>").count(), 1);
}

// ---------------------------------------------------------------------------
// Tool routing
// ---------------------------------------------------------------------------

#[test]
fn tool_registration_routing_and_conflicts() {
    let p1 = ToolProvider::new(
        "p1",
        vec![
            json!({"name": "recall"}),
            json!({"name": ""}),
            json!({"description": "no name"}),
            json!({"name": "store"}),
        ],
    );
    let p2 = ToolProvider::new(
        "p2",
        vec![json!({"name": "recall"}), json!({"name": "forget"})],
    );

    let mut mgr = MemoryManager::new();
    mgr.add_provider(p1.clone());
    mgr.add_provider(p2.clone());

    let names = mgr.get_all_tool_names();
    let expected: HashSet<String> = ["recall", "store", "forget"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(names, expected);
    assert!(mgr.has_tool("recall"));
    assert!(!mgr.has_tool(""));

    let schema_names: Vec<String> = mgr
        .get_all_tool_schemas()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(schema_names, vec!["recall", "", "store", "forget"]);

    // Conflicting "recall" stays routed to the first provider.
    assert_eq!(
        mgr.handle_tool_call("recall", &json!({"q": "hello"})),
        "p1:recall"
    );
    assert_eq!(mgr.handle_tool_call("forget", &json!({})), "p2:forget");
    assert_eq!(
        p1.calls.lock().unwrap().clone(),
        vec![("recall".to_string(), json!({"q": "hello"}))]
    );
    assert_eq!(p2.calls.lock().unwrap().len(), 1);

    let missing: Value =
        serde_json::from_str(&mgr.handle_tool_call("missing", &json!({}))).unwrap();
    assert_eq!(
        missing["error"],
        "No memory provider handles tool 'missing'"
    );

    assert_eq!(mgr.get_provider("p2").unwrap().name(), "p2");
    assert!(mgr.get_provider("nope").is_none());
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn initialize_delegate_and_shutdown_order() {
    let log = new_log();
    let a = Recorder::new("a", true, "", &log);
    let b = Recorder::new("b", true, "", &log);
    let mut mgr = MemoryManager::default();
    mgr.add_provider(a);
    mgr.add_provider(b);

    mgr.initialize_all("sess", "/home");
    assert_eq!(mgr.session_generation.load(Ordering::Relaxed), 1);
    mgr.on_delegation("do it", "sub-1");
    mgr.shutdown_all();

    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "init:a:sess:/home",
            "init:b:sess:/home",
            "delegate:a:do it:sub-1",
            "delegate:b:do it:sub-1",
            "shutdown:b",
            "shutdown:a",
        ]
    );
}

#[test]
fn on_pre_compress_joins_non_empty_results() {
    let log = new_log();
    let mut mgr = MemoryManager::new();
    mgr.add_provider(Recorder::new("a", true, "   ", &log));
    mgr.add_provider(Recorder::new("b", true, "summary from b", &log));
    mgr.add_provider(Recorder::new("c", true, "second note", &log));

    let out = mgr.on_pre_compress(&[json!({"content": "hi"}), json!(5)]);
    assert_eq!(out, "summary from b\n\nsecond note");

    let empty = MemoryManager::new();
    assert_eq!(empty.on_pre_compress(&[json!("x")]), "");
}

#[test]
fn nudge_disabled_with_zero_threshold() {
    let mut mgr = MemoryManager::new().with_nudge_threshold(0);
    for turn in 0..5 {
        mgr.on_turn_start(turn, "hello");
    }
    assert!(mgr.maybe_nudge().is_none());

    let mut enabled = MemoryManager::new().with_nudge_threshold(1);
    enabled.on_turn_start(1, "hello");
    assert!(enabled.maybe_nudge().is_some());
}

#[test]
fn provider_status_and_setup_interactive() {
    let empty = MemoryManager::new();
    assert!(empty.get_provider_status().is_empty());
    assert_eq!(
        empty.setup_interactive(),
        Err("No memory providers registered.".to_string())
    );

    let log = new_log();
    let mut unavailable = MemoryManager::new();
    unavailable.add_provider(Recorder::new("off", false, "", &log));
    assert_eq!(
        unavailable.setup_interactive(),
        Err("No memory providers are currently available.".to_string())
    );

    let mut mgr = MemoryManager::new();
    mgr.add_provider(Recorder::new("off", false, "", &log));
    mgr.add_provider(Recorder::new("on", true, "", &log));
    mgr.add_provider(ToolProvider::new("tools", vec![json!({"name": "t1"})]));
    let statuses = mgr.get_provider_status();
    assert_eq!(statuses.len(), 3);
    assert_eq!(statuses[0].name, "off");
    assert!(!statuses[0].available);
    assert!(!statuses[0].has_config_schema);
    assert_eq!(statuses[1].name, "on");
    assert!(statuses[1].available);
    assert!(statuses[1].has_config_schema);
    assert_eq!(statuses[2].tool_count, 1);
    assert!(mgr.setup_interactive().is_ok());
}

// ---------------------------------------------------------------------------
// Session end tasks
// ---------------------------------------------------------------------------

#[test]
fn session_end_tasks_empty_manager_returns_no_tasks() {
    let shared = Arc::new(Mutex::new(MemoryManager::new()));
    assert!(MemoryManager::session_end_tasks(&shared, &[]).is_empty());
}

#[test]
fn session_end_tasks_poisoned_lock_returns_no_tasks() {
    let shared = Arc::new(Mutex::new(MemoryManager::new()));
    shared.lock().unwrap().add_provider(Arc::new(Bare));
    let s2 = Arc::clone(&shared);
    let handle = std::thread::spawn(move || {
        let _guard = s2.lock().unwrap();
        panic!("poison the lock");
    });
    assert!(handle.join().is_err());
    assert!(shared.is_poisoned());
    assert!(MemoryManager::session_end_tasks(&shared, &[]).is_empty());
}

#[tokio::test]
async fn session_end_tasks_run_hook_for_current_generation() {
    let log = new_log();
    let shared = Arc::new(Mutex::new(MemoryManager::new()));
    shared
        .lock()
        .unwrap()
        .add_provider(Recorder::new("a", true, "", &log));
    let tasks = MemoryManager::session_end_tasks(&shared, &[]);
    assert_eq!(tasks.len(), 1);
    for task in tasks {
        task.await;
    }
    assert_eq!(*log.lock().unwrap(), vec!["end:a:0"]);
}

#[tokio::test]
async fn session_end_tasks_skip_stale_generation() {
    let log = new_log();
    let shared = Arc::new(Mutex::new(MemoryManager::new()));
    shared
        .lock()
        .unwrap()
        .add_provider(Recorder::new("a", true, "", &log));
    let tasks = MemoryManager::session_end_tasks(&shared, &[]);
    assert_eq!(tasks.len(), 1);
    shared
        .lock()
        .unwrap()
        .on_session_switch("new-session", "old-session", false);
    for task in tasks {
        task.await;
    }
    assert!(log.lock().unwrap().iter().all(|e| !e.starts_with("end:")));
}

#[tokio::test]
async fn session_end_tasks_survive_panicking_hook() {
    let shared = Arc::new(Mutex::new(MemoryManager::new()));
    shared.lock().unwrap().add_provider(Arc::new(PanicOnEnd));
    let tasks = MemoryManager::session_end_tasks(&shared, &[]);
    assert_eq!(tasks.len(), 1);
    for task in tasks {
        task.await;
    }
    // The hook panicked while the manager lock was held.
    assert!(shared.is_poisoned());
}

// ---------------------------------------------------------------------------
// Fusion / graph helpers
// ---------------------------------------------------------------------------

#[test]
fn memory_fusion_trace_path_ends_with_logs_file() {
    let path = memory_fusion_trace_path();
    assert!(path.ends_with(std::path::Path::new("logs").join("memory-fusion-trace.jsonl")));
}

fn cand(provider: &str, content: &str) -> FusedMemoryCandidate {
    FusedMemoryCandidate {
        provider: provider.to_string(),
        content: content.to_string(),
    }
}

#[test]
fn graph_enrich_early_returns() {
    let single = vec![cand("a", "alpha beta gamma")];
    assert!(graph_enrich_candidates(&single, "alpha", 2).is_empty());

    let two = vec![cand("a", "alpha beta gamma"), cand("b", "gamma delta")];
    assert!(graph_enrich_candidates(&two, "alpha", 1).is_empty());

    let no_pairs = vec![cand("a", "alpha"), cand("b", "beta")];
    assert!(graph_enrich_candidates(&no_pairs, "alpha", 2).is_empty());

    assert!(graph_enrich_candidates(&two, "omega", 2).is_empty());
}

#[test]
fn graph_enrich_builds_links_up_to_depth() {
    let cands = vec![cand("a", "alpha beta gamma"), cand("b", "gamma delta")];
    let out = graph_enrich_candidates(&cands, "alpha", 2);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].provider, "graph-depth");
    let content = &out[0].content;
    assert!(content.starts_with("Graph-depth memory hints"));
    assert!(content.contains("- alpha -> beta\n"));
    assert!(content.contains("- alpha -> gamma\n"));
    assert!(content.contains("- gamma -> delta\n"));
    // delta sits at distance 2 and is not expanded.
    assert!(!content.contains("- delta -> "));
}

#[test]
fn graph_enrich_caps_links() {
    let many = (0..31)
        .map(|i| format!("w{i:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let cands = vec![cand("a", &many), cand("b", "zzz yyy")];
    let out = graph_enrich_candidates(&cands, "w000", 3);
    assert_eq!(out.len(), 1);
    let link_lines = out[0]
        .content
        .lines()
        .filter(|l| l.starts_with("- "))
        .count();
    assert_eq!(link_lines, 12);
}

#[test]
fn extract_graph_entities_filters_dedupes_and_caps() {
    assert_eq!(
        extract_graph_entities("The alpha and alpha Beta xy"),
        vec!["alpha".to_string(), "beta".to_string()]
    );
    let many = (0..40)
        .map(|i| format!("tok{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(extract_graph_entities(&many).len(), 24);
}

#[test]
fn score_with_no_terms_uses_provider_weight_only() {
    let c = cand("x", "abc");
    let weights: HashMap<String, f64> = HashMap::new();
    let (score, confidence) = score_memory_candidate(&c, &[], &weights);
    assert!((score - (1.0 + 3.0 / 1200.0 * 0.1)).abs() < 1e-9);
    assert!((confidence - 0.65).abs() < 1e-9);
}

#[test]
fn fuse_ties_are_ordered_by_canonical_content() {
    let fused = fuse_memory_candidates(vec![cand("p", "bbb"), cand("p", "aaa")], "");
    assert_eq!(fused.len(), 2);
    assert!(fused[0].starts_with("[p score="));
    assert!(fused[0].ends_with("] aaa"));
    assert!(fused[1].ends_with("] bbb"));
}

#[test]
fn fuse_stops_at_top_k() {
    let candidates = (0..8)
        .map(|i| cand("p", &format!("distinct item number {i}")))
        .collect::<Vec<_>>();
    let fused = fuse_memory_candidates(candidates, "");
    assert_eq!(fused.len(), memory_fusion_top_k().min(8));
}
