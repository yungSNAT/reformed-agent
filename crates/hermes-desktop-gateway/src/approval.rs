//! Wire types for the `approval` server request and the `approval.respond` method.
//!
//! `kind` selects the card Hermes Desktop shows; absent means `command`, a dangerous command
//! (the command is redacted before it reaches these types). A sandbox asks for a folder with
//! `access` (`path`, and `access` read or read_write) and for a site with `network` (`host`,
//! and an optional display `label`). `risk` is one short line shown under the folder or site.
//! On those 2 kinds `once` allows until the sandbox is switched off and `always` saves the
//! rule; a `deny` answer may carry the user's `reason`, which command approvals ignore.

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub session_id: String,
    pub request_id: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    /// The request's JSON params. A folder request without a folder, or a site request without
    /// a site, is refused: the desktop would show a card the user cannot read.
    pub fn params(&self) -> Result<Value, &'static str> {
        let blank = |field: &Option<String>| field.as_deref().is_none_or(|s| s.trim().is_empty());
        match self.kind {
            Some(ApprovalKind::Access) if blank(&self.path) => {
                return Err("an access request needs a path")
            }
            Some(ApprovalKind::Network) if blank(&self.host) => {
                return Err("a network request needs a host")
            }
            _ => {}
        }
        serde_json::to_value(self).map_err(|_| "approval request does not serialise")
    }
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
