//! Wire types for the `approval` server request and the `approval.respond` method.
//!
//! `kind` selects the card Hermes Desktop shows; absent means `command`, a dangerous command.
//! A sandbox asks for a folder with `access` (`path`, and `access` read or read_write) and for
//! a site with `network` (`host`, and an optional display `label`). `risk` is one short line
//! shown under the folder or site. On those 2 kinds `once` allows until the sandbox is switched
//! off and `always` saves the rule; a `deny` answer may carry the user's `reason`, which command
//! approvals ignore.
//!
//! The sender redacts `command`, `description`, `path`, `host`, `label` and `risk` before they
//! reach these types: each is shown to the user.
//!
//! Binding an answer to its request: [`ApprovalAnswer`] carries no id, so it answers only the
//! server request whose JSON-RPC id it replies to, in that request's session.
//! [`ApprovalRespondParams`] with neither `request_id` nor `all` resolves the session's oldest
//! pending approval, so a folder or site card should be answered only through a `request_id`
//! that names it. [`CanonicalApprovalRespondParams`] names the turn generation and the prompt;
//! the handler refuses a generation other than the session's current one and a prompt that is
//! not pending.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The card an approval request asks the desktop to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Command,
    Access,
    Network,
}

/// How much of a folder an `access` request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAccess {
    Read,
    ReadWrite,
}

/// An answer the card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChoice {
    Once,
    Session,
    Always,
    Deny,
}

/// Params of the `approval` server request. Keys beyond these are tool context the card may
/// render, so they are kept in `extra`.
///
/// Only [`ApprovalRequest::params`] output may go on the wire: serialising the struct directly
/// skips its checks and leaves an empty `choices`, which the desktop reads as every choice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub session_id: String,
    pub request_id: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub description: String,
    /// Empty means the choices `allow_session`, `allow_permanent` and `smart_denied` allow.
    #[serde(default)]
    pub choices: Vec<ApprovalChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_permanent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart_denied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ApprovalKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<ApprovalAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ApprovalRequest {
    /// The request's JSON params, always with the offered `choices`. A folder request without
    /// a folder, or a site request without a site, is refused: the desktop would show a card
    /// the user cannot read. So is a key in `extra` that names a field, which would replace
    /// that field on the wire.
    pub fn params(&self) -> Result<Value, &'static str> {
        let mut params =
            serde_json::to_value(self).map_err(|_| "approval request does not serialise")?;
        let sent =
            Self::deserialize(&params).map_err(|_| "approval request has an invalid field")?;
        if sent != *self {
            return Err("an extra key names an approval field");
        }
        match self.kind {
            Some(ApprovalKind::Access) if blank(self.path.as_deref()) => {
                return Err("an access request needs a path")
            }
            Some(ApprovalKind::Network) if blank(self.host.as_deref()) => {
                return Err("a network request needs a host")
            }
            _ => {}
        }
        if self.choices.is_empty() {
            params["choices"] = serde_json::to_value(self.allowed_choices())
                .map_err(|_| "approval request does not serialise")?;
        }
        Ok(params)
    }

    /// `once`; then `session` unless the request is smart-denied or bars the session, and with
    /// it `always` unless the request bars a permanent rule; then `deny`.
    fn allowed_choices(&self) -> Vec<ApprovalChoice> {
        let mut choices = vec![ApprovalChoice::Once];
        if self.smart_denied != Some(true) && self.allow_session != Some(false) {
            choices.push(ApprovalChoice::Session);
            if self.allow_permanent != Some(false) {
                choices.push(ApprovalChoice::Always);
            }
        }
        choices.push(ApprovalChoice::Deny);
        choices
    }
}

/// Blank as a Python `str.strip()` sees it, which also strips U+001C to U+001F.
fn blank(field: Option<&str>) -> bool {
    field.is_none_or(|text| {
        text.chars()
            .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
    })
}

/// The desktop's answer to an `approval` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalAnswer {
    pub choice: ApprovalChoice,
    #[serde(default)]
    pub all: Option<bool>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Params of the `approval.respond` method: `all` resolves every pending approval,
/// `request_id` one, neither the oldest; an absent `choice` means `deny`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRespondParams {
    pub session_id: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub choice: Option<ApprovalChoice>,
    #[serde(default)]
    pub all: Option<bool>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl ApprovalRespondParams {
    /// The choice, with an absent one read as `deny`.
    pub fn choice_or_deny(&self) -> ApprovalChoice {
        self.choice.unwrap_or(ApprovalChoice::Deny)
    }
}

/// Params of `approval.respond` on the canonical wire: the answer names the turn generation and
/// the prompt it answers, and every field but `profile` is required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalApprovalRespondParams {
    pub session_id: String,
    #[serde(default)]
    pub profile: Option<String>,
    pub execution_generation: u64,
    pub prompt_id: String,
    pub choice: ApprovalChoice,
}
