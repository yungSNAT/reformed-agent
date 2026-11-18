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

use hermes_agent::sub_agent_orchestrator::SubAgentLineage;

#[test]
fn sub_agent_lineage_keeps_its_timestamp_form() {
    let lineage = json!({
        "sub_agent_id": "s", "task": "t", "depth": 1, "max_depth": 2, "status": "completed"
    });
    assert_form::<SubAgentLineage>(lineage, &["started_at", "ended_at"], &[("ended_at", None)]);
}
