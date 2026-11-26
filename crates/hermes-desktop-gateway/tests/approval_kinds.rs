//! `approval` request params and answers for the `access` (folder) and `network` (site) kinds.

use hermes_desktop_gateway::approval::{
    ApprovalAccess, ApprovalAnswer, ApprovalChoice, ApprovalKind, ApprovalRequest,
    ApprovalRespondParams, CanonicalApprovalRespondParams,
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

    let folder = request(json!({"kind": "access", "path": "/srv", "access": "read"})).unwrap();
    assert_eq!(folder.access, Some(ApprovalAccess::Read));
    assert!(folder.params().is_ok());
    let site =
        request(json!({"kind": "network", "host": "example.com", "label": "Example"})).unwrap();
    assert_eq!(site.label.as_deref(), Some("Example"));
    assert!(site.params().is_ok());
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
        json!({"kind": "access", "host": "example.com"}),
        json!({"kind": "network", "path": "/srv"}),
        json!({"kind": "access", "path": "\u{1f}\u{1c} \t"}),
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

    // Nor replace a field that is set: the folder card would go out as a command card.
    for (fields, key, value) in [
        (
            json!({"kind": "access", "access": "read"}),
            "kind",
            json!("command"),
        ),
        (json!({"kind": "network"}), "kind", json!("command")),
        (
            json!({"kind": "access", "path": "/srv"}),
            "path",
            json!("/"),
        ),
        (
            json!({"kind": "network", "host": "a.example"}),
            "host",
            json!("b.example"),
        ),
    ] {
        let mut approval = request(fields.clone()).unwrap();
        approval.extra.insert(key.into(), value.clone());
        assert!(
            approval.params().is_err(),
            "{fields} with {key}={value} was accepted"
        );
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

#[test]
fn the_offered_choices_are_always_sent_and_never_broader_than_the_request_allows() {
    let offered = |fields: Value| request(fields).unwrap().params().unwrap()["choices"].clone();
    assert_eq!(
        offered(json!({"command": "ls"})),
        json!(["once", "session", "always", "deny"])
    );
    assert_eq!(
        offered(json!({"command": "ls", "choices": []})),
        json!(["once", "session", "always", "deny"])
    );
    for restricted in [
        json!({"command": "rm -rf /", "allow_session": false}),
        json!({"command": "rm -rf /", "allow_session": false, "choices": []}),
        json!({"command": "rm -rf /", "smart_denied": true}),
        json!({"command": "rm -rf /", "smart_denied": true, "allow_permanent": true}),
    ] {
        assert_eq!(
            offered(restricted.clone()),
            json!(["once", "deny"]),
            "{restricted}"
        );
    }
    assert_eq!(
        offered(json!({"command": "ls", "allow_permanent": false})),
        json!(["once", "session", "deny"])
    );
    // Choices the sender names go out as named.
    assert_eq!(
        offered(json!({"command": "ls", "choices": ["once", "deny"], "allow_permanent": true})),
        json!(["once", "deny"])
    );
    // A named choice the request bars is refused, not sent.
    for broader in [
        json!({"command": "rm -rf /", "allow_session": false,
               "choices": ["once", "session", "always", "deny"]}),
        json!({"command": "rm -rf /", "smart_denied": true, "choices": ["session"]}),
        json!({"command": "ls", "allow_permanent": false, "choices": ["always", "deny"]}),
    ] {
        assert!(
            request(broader.clone()).unwrap().params().is_err(),
            "{broader}"
        );
    }
}

#[test]
fn an_absent_respond_choice_denies() {
    let respond: ApprovalRespondParams =
        serde_json::from_value(json!({"session_id": "s1", "request_id": "r1"})).unwrap();
    assert_eq!(respond.choice_or_deny(), ApprovalChoice::Deny);
    let respond: ApprovalRespondParams =
        serde_json::from_value(json!({"session_id": "s1", "choice": "once"})).unwrap();
    assert_eq!(respond.choice_or_deny(), ApprovalChoice::Once);
}

#[test]
fn a_canonical_answer_names_its_turn_generation_and_prompt() {
    let frame = json!({
        "session_id": "s1", "execution_generation": 3, "prompt_id": "p1", "choice": "once"
    });
    let answer: CanonicalApprovalRespondParams = serde_json::from_value(frame.clone()).unwrap();
    assert_eq!(
        (
            answer.execution_generation,
            answer.prompt_id.as_str(),
            answer.choice
        ),
        (3, "p1", ApprovalChoice::Once)
    );
    let routed: CanonicalApprovalRespondParams =
        serde_json::from_value(json!({"profile": "work", "session_id": "s1",
            "execution_generation": 3, "prompt_id": "p1", "choice": "deny"}))
        .unwrap();
    assert_eq!(routed.profile.as_deref(), Some("work"));

    for missing in ["execution_generation", "prompt_id", "choice", "session_id"] {
        let mut partial = frame.clone();
        partial.as_object_mut().unwrap().remove(missing);
        assert!(
            serde_json::from_value::<CanonicalApprovalRespondParams>(partial).is_err(),
            "accepted without {missing}"
        );
    }
    for (key, value) in [
        ("request_id", json!("r1")),
        ("all", json!(true)),
        ("reason", json!("x")),
    ] {
        let mut wider = frame.clone();
        wider.as_object_mut().unwrap().insert(key.into(), value);
        assert!(
            serde_json::from_value::<CanonicalApprovalRespondParams>(wider).is_err(),
            "accepted {key}"
        );
    }
    let mut negative = frame.clone();
    negative["execution_generation"] = json!(-1);
    assert!(serde_json::from_value::<CanonicalApprovalRespondParams>(negative).is_err());
}
