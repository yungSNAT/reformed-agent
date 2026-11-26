//! `write_file` refuses, before touching the disk, to overwrite an existing
//! file the session never read in full or that changed on disk since that
//! read; `patch` only warns. Every call goes through the tool registry, so
//! what the model was actually shown decides whether a read counts.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use hermes_core::ToolHandler;
use hermes_environments::local::LocalBackend;
use hermes_tools::backends::file::LocalPatchBackend;
use hermes_tools::registry::ToolRegistry;
use hermes_tools::tools::file::{PatchHandler, ReadFileHandler, WriteFileHandler};
use serde_json::{json, Value};

/// The parameter through which the agent loop names its session.
const SESSION: &str = "_hermes_session";

fn registry() -> ToolRegistry {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HERMES_HOME", home.path());
        home
    });
    let registry = ToolRegistry::new();
    let backend: Arc<dyn hermes_core::TerminalBackend> = Arc::new(LocalBackend::new(60, 1 << 20));
    let handlers: [Arc<dyn ToolHandler>; 3] = [
        Arc::new(ReadFileHandler::new(backend.clone())),
        Arc::new(WriteFileHandler::new(backend)),
        Arc::new(PatchHandler::new(Arc::new(LocalPatchBackend::new()))),
    ];
    for handler in handlers {
        let schema = handler.schema();
        registry.register(
            schema.name.clone(),
            "file",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "",
            "",
            None,
        );
    }
    registry
}

async fn call(registry: &ToolRegistry, tool: &str, session: &str, mut args: Value) -> Value {
    args[SESSION] = json!(session);
    let out = registry.dispatch_async(tool, args).await;
    serde_json::from_str(&out).unwrap_or_else(|_| json!({ "result": out }))
}

/// A `read_file` made by the agent loop's call `call_id`; returns the
/// registry's result, which the loop then settles with what the model sees.
async fn agent_read(registry: &ToolRegistry, session: &str, call_id: &str, path: &Path) -> String {
    let mut args = json!({ "path": path });
    args[SESSION] = json!(session);
    args["_hermes_call"] = json!(call_id);
    registry.dispatch_async("read_file", args).await
}

async fn read(registry: &ToolRegistry, session: &str, path: &Path) -> Value {
    call(registry, "read_file", session, json!({ "path": path })).await
}

async fn read_page(
    registry: &ToolRegistry,
    session: &str,
    path: &Path,
    offset: u64,
    limit: u64,
) -> Value {
    let args = json!({ "path": path, "offset": offset, "limit": limit });
    call(registry, "read_file", session, args).await
}

async fn write(registry: &ToolRegistry, session: &str, path: &Path, content: &str) -> Value {
    let args = json!({ "path": path, "content": content });
    call(registry, "write_file", session, args).await
}

async fn patch(registry: &ToolRegistry, session: &str, path: &Path, old: &str, new: &str) -> Value {
    let args = json!({ "path": path, "old_string": old, "new_string": new });
    call(registry, "patch", session, args).await
}

fn ok(result: &Value) {
    assert!(result.get("error").is_none(), "{result}");
}

fn refused(result: &Value) {
    let error = result["error"]
        .as_str()
        .unwrap_or_else(|| panic!("not refused: {result}"));
    assert!(error.contains("Refusing to overwrite"), "{error}");
    assert!(error.contains("The file was NOT modified"), "{error}");
}

fn text(result: &Value) -> String {
    result["result"]
        .as_str()
        .map_or_else(|| result.to_string(), str::to_string)
}

/// Rewrites `path` so its modified time also moves.
fn modify_externally(path: &Path, content: &str) {
    let before = std::fs::metadata(path).unwrap().modified().unwrap();
    std::fs::write(path, content).unwrap();
    if std::fs::metadata(path).unwrap().modified().unwrap() == before {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(before + std::time::Duration::from_secs(1))
            .unwrap();
    }
}

fn fixture(name: &str, content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, content).unwrap();
    (dir, path)
}

#[tokio::test]
async fn no_refusal_when_file_unchanged() {
    let tools = registry();
    let (_dir, path) = fixture("stale_test.txt", "original content\n");
    ok(&read(&tools, "t1", &path).await);
    ok(&write(&tools, "t1", &path, "new content").await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
}

#[tokio::test]
async fn write_file_refuses_before_mutation_when_modified_externally() {
    let tools = registry();
    let (_dir, path) = fixture("stale_test.txt", "original content\n");
    ok(&read(&tools, "t1", &path).await);
    modify_externally(&path, "someone else changed this\n");

    refused(&write(&tools, "t1", &path, "new content\n").await);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "someone else changed this\n"
    );

    ok(&read(&tools, "t1", &path).await);
    ok(&write(&tools, "t1", &path, "merged\n").await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "merged\n");
}

#[tokio::test]
async fn write_file_requires_full_unredacted_read_of_existing_file() {
    let tools = registry();
    let (dir, path) = fixture("stale_test.txt", "original content\n");

    // Never read.
    refused(&write(&tools, "t2", &path, "x\n").await);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "original content\n"
    );

    // Only patched.
    ok(&patch(&tools, "t2", &path, "original", "patched").await);
    refused(&write(&tools, "t2", &path, "x\n").await);

    // Read one line of three.
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    ok(&read_page(&tools, "t2", &path, 1, 1).await);
    refused(&write(&tools, "t2", &path, "x\n").await);

    // Read with a secret redacted from what the model saw.
    let secret = format!("ghp_{}", "A".repeat(40));
    std::fs::write(&path, format!("token={secret}\n")).unwrap();
    let shown = read(&tools, "t2", &path).await;
    assert!(!shown.to_string().contains(&secret), "{shown}");
    refused(&write(&tools, "t2", &path, "token=changed\n").await);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("token={secret}\n")
    );

    // A net-new file needs no read, and the session's own write is a baseline.
    let new_path = dir.path().join("brand_new.txt");
    ok(&write(&tools, "t2", &new_path, "one\n").await);
    ok(&write(&tools, "t2", &new_path, "two\n").await);
    assert_eq!(std::fs::read_to_string(&new_path).unwrap(), "two\n");
}

#[tokio::test]
async fn paged_read_of_large_file_is_a_full_baseline() {
    let tools = registry();
    // Over the registry's 50,000-character result cap, so a whole read is cut.
    let lines = |word: &str| -> String {
        (1..=2500)
            .map(|i| format!("{word} {i:>5} padding padding padding\n"))
            .collect()
    };
    let (_dir, path) = fixture("large.txt", &lines("line"));
    let whole = read(&tools, "t3", &path).await;
    assert!(text(&whole).contains("truncated"), "{whole}");
    refused(&write(&tools, "t3", &path, "x\n").await);

    for offset in [1, 1001, 2001] {
        ok(&read_page(&tools, "t3", &path, offset, 1000).await);
    }
    ok(&write(&tools, "t3", &path, "merged\n").await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "merged\n");

    // An edit between pages voids the earlier ones.
    std::fs::write(&path, lines("line")).unwrap();
    ok(&read_page(&tools, "t3", &path, 1, 1000).await);
    ok(&read_page(&tools, "t3", &path, 1001, 1000).await);
    modify_externally(&path, &lines("other"));
    ok(&read_page(&tools, "t3", &path, 2001, 1000).await);
    let result = write(&tools, "t3", &path, "x\n").await;
    refused(&result);
    assert!(
        !result["error"].as_str().unwrap().contains("Warning:"),
        "{result}"
    );
}

#[tokio::test]
async fn relative_path_is_tracked_where_the_backend_resolves_it() {
    let tools = registry();
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("worktree");
    std::fs::create_dir(&live).unwrap();
    let live_file = live.join("shared.txt");
    std::fs::write(&live_file, "live copy\n").unwrap();

    // The only test that sets TERMINAL_CWD; the others use absolute paths.
    std::env::set_var("TERMINAL_CWD", &live);
    ok(&read(&tools, "live_task", Path::new("shared.txt")).await);
    modify_externally(&live_file, "live copy modified elsewhere\n");
    let result = write(&tools, "live_task", Path::new("shared.txt"), "replacement").await;
    std::env::remove_var("TERMINAL_CWD");
    refused(&result);
    assert_eq!(
        std::fs::read_to_string(&live_file).unwrap(),
        "live copy modified elsewhere\n"
    );
}

#[tokio::test]
async fn patch_warns_on_stale_file() {
    let tools = registry();
    let (_dir, path) = fixture("patch_test.txt", "original line\n");
    ok(&read(&tools, "p1", &path).await);
    modify_externally(&path, "original line\nexternally added\n");

    let result = patch(&tools, "p1", &path, "original", "patched").await;
    ok(&result);
    assert!(
        text(&result).contains("modified since you last read"),
        "{result}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "patched line\nexternally added\n"
    );
}

#[tokio::test]
async fn patch_no_warning_when_fresh() {
    let tools = registry();
    let (_dir, path) = fixture("patch_test.txt", "original line\n");
    ok(&read(&tools, "p2", &path).await);

    let result = patch(&tools, "p2", &path, "original", "patched").await;
    ok(&result);
    assert!(
        !text(&result).contains("modified since you last read"),
        "{result}"
    );
}

#[tokio::test]
async fn another_sessions_read_does_not_bless_a_write() {
    let tools = registry();
    let (_dir, path) = fixture("shared.txt", "original\n");
    ok(&read(&tools, "reader", &path).await);

    refused(&write(&tools, "other", &path, "clobber\n").await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");

    ok(&write(&tools, "reader", &path, "informed\n").await);
    // The reader's write moved the file on from what the other session last saw.
    ok(&read(&tools, "other", &path).await);
    ok(&write(&tools, "reader", &path, "again\n").await);
    refused(&write(&tools, "other", &path, "stale\n").await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "again\n");
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_and_its_target_share_one_record() {
    let tools = registry();
    let (dir, target) = fixture("target.txt", "original\n");
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    ok(&read(&tools, "l1", &link).await);
    ok(&write(&tools, "l1", &target, "through the target\n").await);
    modify_externally(&target, "changed elsewhere\n");
    refused(&write(&tools, "l1", &link, "stale\n").await);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "changed elsewhere\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_fresh_write_through_a_link_is_left_to_the_backend() {
    let tools = registry();
    let (dir, target) = fixture("target.txt", "original\n");
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    ok(&read(&tools, "l2", &link).await);
    // The guard passes; the local backend will not write through a leaf link.
    let result = write(&tools, "l2", &link, "through the link\n").await;
    let error = result["error"]
        .as_str()
        .unwrap_or_else(|| panic!("written: {result}"));
    assert!(!error.contains("Refusing to overwrite"), "{error}");
    assert!(error.contains("symlink"), "{error}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original\n");
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[tokio::test]
async fn a_read_the_agent_cuts_does_not_bless_a_write() {
    let tools = registry();
    let (_dir, path) = fixture("cut.txt", "first line\nsecond line\n");
    let returned = agent_read(&tools, "b1", "b1-cut", &path).await;
    let cut = format!("{}\n[... truncated ...]", &returned[..10]);
    hermes_core::file_edit::settle_view("b1-cut", &cut);
    refused(&write(&tools, "b1", &path, "replacement\n").await);

    let returned = agent_read(&tools, "b1", "b1-whole", &path).await;
    // Not yet settled: the model has not been given it.
    refused(&write(&tools, "b1", &path, "replacement\n").await);
    hermes_core::file_edit::settle_view("b1-whole", &returned);
    ok(&write(&tools, "b1", &path, "replacement\n").await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement\n");
}

#[tokio::test]
async fn concurrent_reads_are_settled_each_by_its_own_call() {
    let tools = registry();
    let long: String = (0..3000)
        .map(|n| format!("line {n:05} padding\n"))
        .collect();
    let (_dir, path) = fixture("concurrent.txt", &long);
    // A whole read cut by the registry's cap beside an intact one-line page.
    let (page, whole) = tokio::join!(
        read_page(&tools, "b2", &path, 1, 1),
        read(&tools, "b2", &path)
    );
    ok(&page);
    ok(&whole);
    refused(&write(&tools, "b2", &path, "replacement\n").await);

    let (_dir, path) = fixture("pages.txt", "first\nsecond\n");
    let (first, second) = tokio::join!(
        read_page(&tools, "b2", &path, 1, 1),
        read_page(&tools, "b2", &path, 2, 1)
    );
    ok(&first);
    ok(&second);
    ok(&write(&tools, "b2", &path, "replacement\n").await);
}

#[tokio::test]
async fn a_file_that_is_not_utf8_is_refused_with_its_reason() {
    let tools = registry();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary.dat");
    std::fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
    read(&tools, "s2", &path).await;

    let result = write(&tools, "s2", &path, "text\n").await;
    refused(&result);
    assert!(text(&result).contains("not UTF-8"), "{result}");
    assert_eq!(std::fs::read(&path).unwrap(), [0xff, 0xfe, 0x00, 0x80]);
}
