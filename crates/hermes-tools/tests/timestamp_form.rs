//! Stored timestamps keep the text form they had under chrono.

use serde_json::json;

#[path = "../../hermes-core/tests/support/timestamp_form.rs"]
mod support;
use support::assert_form;

use hermes_tools::tools::ultra_autonomy::{AutonomyBoard, CardComment, ToolLoopEvent};

#[test]
fn autonomy_records_keep_their_timestamp_form() {
    assert_form::<ToolLoopEvent>(json!({"tool": "t"}), &["timestamp"], &[("timestamp", None)]);
    assert_form::<AutonomyBoard>(
        json!({"id": "b", "title": "t"}),
        &["created_at", "updated_at"],
        &[],
    );
    assert_form::<CardComment>(json!({"author": "a", "body": "b"}), &["created_at"], &[]);
}
