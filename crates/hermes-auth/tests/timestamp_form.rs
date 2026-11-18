//! Stored timestamps keep the text form they had under chrono.

use serde_json::{json, Value};

#[path = "../../hermes-core/tests/support/timestamp_form.rs"]
mod support;
use support::assert_form;

use hermes_auth::OAuthCredential;

#[test]
fn oauth_credentials_keep_their_timestamp_form() {
    let credential = json!({
        "provider": "p", "access_token": "a", "refresh_token": null,
        "token_type": "Bearer", "scope": null
    });
    assert_form::<OAuthCredential>(
        credential,
        &["expires_at"],
        &[("expires_at", Some(Value::Null))],
    );
}
