//! Stored timestamps keep the text form they had under chrono.

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

/// Reads each timestamp field from another offset with a short fraction and
/// checks the record writes it back in chrono's form; then drops each optional
/// field and checks how the record writes its absence.
fn assert_form<T: Serialize + DeserializeOwned>(
    mut record: Value,
    fields: &[&str],
    absent: &[(&str, Option<Value>)],
) {
    for field in fields {
        record[*field] = json!("2026-03-08T19:30:15.25+10:00");
    }
    let written =
        serde_json::to_value(serde_json::from_value::<T>(record.clone()).unwrap()).unwrap();
    for field in fields {
        assert_eq!(
            written[*field],
            json!("2026-03-08T09:30:15.250Z"),
            "{field}"
        );
    }
    for (field, expected) in absent {
        let mut without = record.clone();
        without.as_object_mut().unwrap().remove(*field);
        let written = serde_json::to_value(serde_json::from_value::<T>(without).unwrap()).unwrap();
        assert_eq!(written.get(*field).cloned(), *expected, "{field}");
    }
}

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
