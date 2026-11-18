//! Stored timestamps keep the text form they had under chrono.

use serde_json::{json, Value};

#[path = "../../hermes-core/tests/support/timestamp_form.rs"]
mod support;
use support::assert_form;

use hermes_eval::result::RunRecord;

#[test]
fn run_records_keep_their_timestamp_form() {
    let record = json!({
        "run_id": "r",
        "benchmark": {"id": "b", "name": "B", "source": "s", "version": "1"},
        "seed": 1,
        "concurrency": 1,
        "model": "m",
        "tasks": [],
        "metrics": {
            "total": 0, "passed": 0, "failed": 0, "timeout": 0, "error": 0, "skipped": 0,
            "pass_at_1": 0.0, "total_duration": {"secs": 0, "nanos": 0},
            "total_tokens_input": 0, "total_tokens_output": 0, "total_cost_usd": 0.0
        }
    });
    assert_form::<RunRecord>(
        record,
        &["started_at", "finished_at"],
        &[("finished_at", Some(Value::Null))],
    );
}
