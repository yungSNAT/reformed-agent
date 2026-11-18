//! Stored timestamps keep the text form they had under chrono.

use serde_json::Value;

#[path = "../../hermes-core/tests/support/timestamp_form.rs"]
mod support;
use support::assert_form;

use hermes_config::session::{SessionResetPolicy, SessionType};
use hermes_gateway::session::Session;
use hermes_gateway::stream::StreamHandle;

#[test]
fn gateway_records_keep_their_timestamp_form() {
    let session = Session::new("p", "c", "u", SessionType::Dm, SessionResetPolicy::None);
    assert_form::<Session>(
        serde_json::to_value(&session).unwrap(),
        &["created_at", "last_active_at"],
        &[],
    );
    let stream = serde_json::to_value(StreamHandle::new("p", "c")).unwrap();
    assert_form::<StreamHandle>(
        stream,
        &["created_at", "last_edit_at"],
        &[("last_edit_at", Some(Value::Null))],
    );
}
