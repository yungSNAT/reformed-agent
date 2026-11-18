//! Stored timestamps keep the text form they had under chrono.

use serde_json::json;

#[path = "../../hermes-core/tests/support/timestamp_form.rs"]
mod support;
use support::assert_form;

use hermes_agent::sub_agent_orchestrator::SubAgentLineage;

#[test]
fn sub_agent_lineage_keeps_its_timestamp_form() {
    let lineage = json!({
        "sub_agent_id": "s", "task": "t", "depth": 1, "max_depth": 2, "status": "completed"
    });
    assert_form::<SubAgentLineage>(lineage, &["started_at", "ended_at"], &[("ended_at", None)]);
}
