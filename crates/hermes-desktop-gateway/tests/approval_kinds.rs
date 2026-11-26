//! `approval` request params and answers for the `access` (folder) and `network` (site) kinds.

use hermes_desktop_gateway::approval::{
    ApprovalAccess, ApprovalAnswer, ApprovalChoice, ApprovalKind, ApprovalRequest,
    ApprovalRespondParams,
};
use serde_json::{json, Value};

fn request(fields: Value) -> Result<ApprovalRequest, serde_json::Error> {
    let mut params = json!({"session_id": "s1", "request_id": "r1"});
    params
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(params)
}

#[test]
fn a_command_approval_needs_no_kind_and_sends_none() {
    let approval =
        request(json!({"command": "rm -rf build", "description": "dangerous command"})).unwrap();
    assert_eq!(approval.kind, None);

    let params = approval.params().unwrap();
    assert_eq!(params["command"], "rm -rf build");
    assert!(params.get("kind").is_none(), "{params}");
    assert!(params.get("path").is_none(), "{params}");

    let bare = request(json!({})).unwrap();
    assert_eq!((bare.command.as_str(), bare.description.as_str()), ("", ""));
}

#[test]
fn folder_and_site_cards_carry_their_fields() {
    let folder = request(json!({
        "kind": "access", "path": "C:\\docs", "access": "read_write",
        "risk": "The agent can change and delete files in this folder."
    }))
    .unwrap();
    assert_eq!(folder.kind, Some(ApprovalKind::Access));
    assert_eq!(folder.path.as_deref(), Some("C:\\docs"));
    assert_eq!(folder.access, Some(ApprovalAccess::ReadWrite));
    let params = folder.params().unwrap();
    assert_eq!(params["kind"], "access");
    assert_eq!(params["access"], "read_write");
    assert_eq!(
        params["risk"],
        "The agent can change and delete files in this folder."
    );

    let site = request(json!({
        "kind": "network", "host": "example.com", "label": "Web search (example.com)",
        "choices": ["once", "always", "deny"]
    }))
    .unwrap();
    assert_eq!(site.kind, Some(ApprovalKind::Network));
    assert_eq!(
        site.choices,
        [
            ApprovalChoice::Once,
            ApprovalChoice::Always,
            ApprovalChoice::Deny
        ]
    );
    let params = site.params().unwrap();
    assert_eq!(params["kind"], "network");
    assert_eq!(params["host"], "example.com");
    assert_eq!(params["label"], "Web search (example.com)");
    assert_eq!(params["choices"], json!(["once", "always", "deny"]));
}

#[test]
fn values_outside_the_enums_are_refused() {
    assert!(request(json!({"kind": "printer"})).is_err());
    assert!(request(json!({"kind": "access", "path": "/srv", "access": "rw"})).is_err());
    assert!(request(json!({"choices": ["forever"]})).is_err());
}

#[test]
fn a_folder_or_site_request_that_names_none_is_never_sent() {
    for fields in [
        json!({"kind": "access", "access": "read"}),
        json!({"kind": "access", "path": "  ", "access": "read"}),
        json!({"kind": "network"}),
        json!({"kind": "network", "host": "", "label": "Somewhere"}),
    ] {
        let approval = request(fields.clone()).unwrap();
        assert!(approval.params().is_err(), "{fields} was accepted");
    }
}

#[test]
fn extra_keys_cannot_stand_in_for_the_checked_fields() {
    for (key, value) in [("kind", json!("access")), ("kind", json!("printer"))] {
        let mut approval = request(json!({"command": "ls"})).unwrap();
        approval.extra.insert(key.into(), value.clone());
        assert!(approval.params().is_err(), "{key}={value} was accepted");
    }
}

#[test]
fn tool_context_the_card_may_render_rides_along() {
    let approval =
        request(json!({"command": "ls", "pattern_key": "ls", "pattern_keys": ["ls"]})).unwrap();
    let params = approval.params().unwrap();
    assert_eq!(params["pattern_key"], "ls");
    assert_eq!(params["pattern_keys"], json!(["ls"]));
}

#[test]
fn a_deny_reason_rides_the_answer_and_approval_respond() {
    let answer: ApprovalAnswer =
        serde_json::from_value(json!({"choice": "deny", "reason": "Not this site"})).unwrap();
    assert_eq!(answer.choice, ApprovalChoice::Deny);
    assert_eq!(answer.reason.as_deref(), Some("Not this site"));
    let once: ApprovalAnswer = serde_json::from_value(json!({"choice": "once"})).unwrap();
    assert_eq!(once.reason, None);
    assert!(
        serde_json::from_value::<ApprovalAnswer>(json!({"choice": "deny", "why": "x"})).is_err()
    );

    let respond: ApprovalRespondParams = serde_json::from_value(
        json!({"session_id": "s1", "request_id": "r1", "choice": "deny", "reason": "Not this site"}),
    )
    .unwrap();
    assert_eq!(respond.choice, Some(ApprovalChoice::Deny));
    assert_eq!(respond.reason.as_deref(), Some("Not this site"));
    let err = serde_json::from_value::<ApprovalRespondParams>(
        json!({"session_id": "s1", "choice": "deny", "why": "x"}),
    )
    .unwrap_err();
    assert!(err.to_string().contains("why"), "{err}");
}
