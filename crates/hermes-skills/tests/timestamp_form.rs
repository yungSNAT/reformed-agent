//! Stored timestamps keep the text form they had under chrono.

use serde_json::json;

#[path = "../../hermes-core/tests/support/timestamp_form.rs"]
mod support;
use support::assert_form;

use hermes_skills::{SkillChange, SkillVersion};

#[test]
fn skill_versions_keep_their_timestamp_form() {
    assert_form::<SkillVersion>(
        json!({"version": "1.0.0"}),
        &["created_at", "updated_at"],
        &[],
    );
    assert_form::<SkillChange>(
        json!({"skill_name": "s", "from_version": "1", "to_version": "2", "description": null}),
        &["timestamp"],
        &[],
    );
}
