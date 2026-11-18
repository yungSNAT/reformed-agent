//! Stored timestamps keep the text form they had under chrono.

use serde_json::json;

#[path = "../../hermes-core/tests/support/timestamp_form.rs"]
mod support;
use support::assert_form;

use hermes_intelligence::future_kernel::ContextSource;
use hermes_intelligence::usage::UsageRecord;

#[test]
fn intelligence_records_keep_their_timestamp_form() {
    assert_form::<ContextSource>(
        json!({"kind": "k", "locator": "l"}),
        &["observed_at"],
        &[("observed_at", None)],
    );
    assert_form::<UsageRecord>(
        json!({"model": "m", "prompt_tokens": 1, "completion_tokens": 2, "estimated_cost": 0.5}),
        &["timestamp"],
        &[],
    );
}
