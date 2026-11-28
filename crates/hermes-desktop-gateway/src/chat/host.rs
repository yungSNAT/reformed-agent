//! Methods that act on the host: `shell.exec`, `cli.exec` and `complete.path` (upstream
//! tui_gateway/methods_tools.py and methods_complete.py).
//!
//! Each is an access boundary, so each refuses before reading its parameters unless the
//! connection holds the host-input capability ([`Conn::host_input`]): only a local-mode ticket
//! connection does, as a token on the loopback listener cannot tell this machine's client from
//! one tunnelled in. No refusal echoes request text, and nothing typed, run or printed is
//! logged. Children lead their own session and process group with a cleared environment, and a
//! supervisor task, not the handler, owns each one: on exit, timeout or a dropped request (the
//! connection closed) it kills the group (on Linux, the session too), reaps the child and stops
//! reading output soon after, whatever a descendant still holds. Windows has no local mode yet
//! and these supervisors kill process groups only, so there all 3 refuse. `shell.exec`'s pattern
//! checks are a guard, not a sandbox: shell quoting can hide a command from them, and the
//! capability, not the patterns, is the boundary.

use serde_json::{Map, Value};

use super::{Conn, Handler, Origin, Request, RpcError, INVALID_PARAMS};

pub(super) const METHODS: &[(&str, Handler)] = &[
    ("shell.exec", |r| Box::pin(shell_exec(r))),
    ("cli.exec", |r| Box::pin(cli_exec(r))),
    ("complete.path", |r| Box::pin(complete_path(r))),
];

/// A connection without the host-input capability, or a host that cannot serve the method.
const NOT_AVAILABLE: i64 = 4030;
const NOT_AVAILABLE_TEXT: &str = "not available to this client";
const NOT_ON_THIS_HOST: &str = "not available on this host yet";
/// Whether this host serves the host methods: every handler passes it to [`admit`].
const HOST_SERVES: bool = cfg!(unix);

/// The capability check, first in every handler here, then whether this host serves these
/// methods at all (`host_serves`, [`HOST_SERVES`] outside tests).
fn admit(conn: &Conn, host_serves: bool) -> Result<(), RpcError> {
    if !(conn.host_input && conn.origin == Origin::Local) {
        Err((NOT_AVAILABLE, NOT_AVAILABLE_TEXT.into()))
    } else if !host_serves {
        Err((NOT_AVAILABLE, NOT_ON_THIS_HOST.into()))
    } else {
        Ok(())
    }
}

/// An optional string parameter of at most `max` bytes with no NUL: absent or null is empty.
#[cfg_attr(not(unix), allow(dead_code, reason = "read by the Unix bodies only"))]
fn text<'a>(params: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, RpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(text)) if text.len() <= max && !text.contains('\0') => Ok(text),
        Some(Value::String(_)) => Err((
            INVALID_PARAMS,
            format!("{key} is too long or holds a NUL byte"),
        )),
        Some(_) => Err((INVALID_PARAMS, format!("{key} must be a string"))),
    }
}

async fn shell_exec(req: Request) -> Result<Value, RpcError> {
    admit(&req.conn, HOST_SERVES)?;
    #[cfg(unix)]
    {
        unix::shell_exec(req).await
    }
    #[cfg(not(unix))]
    {
        Err((NOT_AVAILABLE, NOT_ON_THIS_HOST.into()))
    }
}

async fn cli_exec(req: Request) -> Result<Value, RpcError> {
    admit(&req.conn, HOST_SERVES)?;
    #[cfg(unix)]
    {
        unix::cli_exec(req).await
    }
    #[cfg(not(unix))]
    {
        Err((NOT_AVAILABLE, NOT_ON_THIS_HOST.into()))
    }
}

async fn complete_path(req: Request) -> Result<Value, RpcError> {
    admit(&req.conn, HOST_SERVES)?;
    #[cfg(unix)]
    {
        unix::complete_path(req).await
    }
    #[cfg(not(unix))]
    {
        Err((NOT_AVAILABLE, NOT_ON_THIS_HOST.into()))
    }
}

#[cfg(unix)]
mod unix;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_without_the_capability_is_refused_whatever_its_origin() {
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut conn = Conn::new(1, outbox, false);
        for origin in [Origin::Remote, Origin::Webapp, Origin::Local] {
            conn.origin = origin;
            assert_eq!(admit(&conn, true).unwrap_err().0, NOT_AVAILABLE);
        }
        conn.host_input = true;
        for origin in [Origin::Remote, Origin::Webapp] {
            conn.origin = origin;
            assert_eq!(admit(&conn, true).unwrap_err().0, NOT_AVAILABLE);
        }
        conn.origin = Origin::Local;
        assert!(admit(&conn, true).is_ok());
    }

    /// Every handler passes this flag, so serving off Unix fails here on that host.
    #[test]
    fn only_a_unix_host_serves_the_host_methods() {
        assert_eq!(HOST_SERVES, cfg!(unix));
    }

    /// Run on every platform, so weakening the Windows refusal fails here, not only on Windows.
    #[test]
    fn a_host_that_cannot_serve_refuses_even_the_capability() {
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut conn = Conn::new(1, outbox, false);
        conn.host_input = true;
        conn.origin = Origin::Local;
        assert_eq!(
            admit(&conn, false).unwrap_err(),
            (NOT_AVAILABLE, NOT_ON_THIS_HOST.to_string())
        );
    }
}
