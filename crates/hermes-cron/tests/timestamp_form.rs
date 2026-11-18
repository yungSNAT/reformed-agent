//! Stored timestamps keep the text form they had under chrono.

use serde_json::json;

#[path = "../../hermes-core/tests/support/timestamp_form.rs"]
mod support;
use support::assert_form;

use hermes_cron::suggestions::SuggestionRecord;
use hermes_cron::CronJob;

#[test]
fn cron_records_keep_their_timestamp_form() {
    let job = serde_json::to_value(CronJob::new("0 9 * * *", "p")).unwrap();
    assert_form::<CronJob>(
        job,
        &["created_at", "last_run", "next_run"],
        &[("last_run", None), ("next_run", None)],
    );
    let suggestion = json!({
        "id": "i", "title": "t", "description": "d", "source": "catalog",
        "job_spec": {"schedule": "0 9 * * *", "prompt": "p", "name": "n"},
        "dedup_key": "k", "status": "pending"
    });
    assert_form::<SuggestionRecord>(
        suggestion,
        &["created_at", "resolved_at"],
        &[("resolved_at", None)],
    );
}
