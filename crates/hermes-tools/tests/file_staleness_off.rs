//! `HERMES_DISABLE_FILE_STATE_GUARD=1` turns off `write_file`'s refusal to
//! replace a file the session has not read. Its own test binary, as it sets
//! a variable every test in the process would see.

use std::sync::Arc;

use hermes_core::ToolHandler;
use hermes_environments::local::LocalBackend;
use hermes_tools::registry::ToolRegistry;
use hermes_tools::tools::file::WriteFileHandler;
use serde_json::json;

#[tokio::test]
async fn the_switch_lets_an_unread_file_be_replaced() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HERMES_HOME", home.path());
    let registry = ToolRegistry::new();
    let handler: Arc<dyn ToolHandler> = Arc::new(WriteFileHandler::new(Arc::new(
        LocalBackend::new(60, 1 << 20),
    )));
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unread.txt");
    std::fs::write(&path, "original\n").unwrap();
    let args = json!({ "path": path, "content": "replaced\n", "_hermes_session": "off" });

    let refused = registry.dispatch_async("write_file", args.clone()).await;
    assert!(refused.contains("Refusing to overwrite"), "{refused}");

    std::env::set_var("HERMES_DISABLE_FILE_STATE_GUARD", "1");
    let written = registry.dispatch_async("write_file", args).await;
    assert!(!written.contains("error"), "{written}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replaced\n");
}
