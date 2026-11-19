//! One-use tickets, the only credential of local mode's native path (upstream
//! gateway/control_socket.py `session-ticket`: 30 seconds, one use, bound to a purpose).
// Only local mode, which is Unix only, mints.
#![cfg_attr(not(unix), allow(dead_code))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a minted ticket stays usable; the `expires_in_seconds` the control socket reports.
pub(crate) const TICKET_TTL: Duration = Duration::from_secs(30);

/// Most unexpired tickets held at once. Only a peer that passed the control socket's checks
/// can mint, so the cap bounds memory, not access.
const MAX_OUTSTANDING: usize = 1024;

/// What a ticket opens: the WebSocket (`interactive`) or one REST request (`native-http`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Purpose {
    Interactive,
    NativeHttp,
}

#[derive(Default)]
pub(crate) struct Tickets {
    issued: Mutex<HashMap<String, (Purpose, Instant)>>,
    /// Set when the owner stops: nothing mints or redeems after, on any connection.
    closed: AtomicBool,
}

impl Tickets {
    /// Spends every outstanding ticket and refuses all later mints.
    pub(crate) fn close(&self) {
        let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        self.closed.store(true, Ordering::SeqCst);
        issued.clear();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn mint(&self, purpose: Purpose) -> Result<String, &'static str> {
        let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_closed() {
            return Err("gateway is stopping");
        }
        issued.retain(|_, (_, at)| at.elapsed() < TICKET_TTL);
        if issued.len() >= MAX_OUTSTANDING {
            return Err("too many outstanding tickets");
        }
        let ticket = crate::new_token();
        issued.insert(ticket.clone(), (purpose, Instant::now()));
        Ok(ticket)
    }

    /// True when `ticket` was minted for `purpose` and has not expired. A presented ticket is
    /// spent whether or not it then opens anything.
    pub(crate) fn consume(&self, ticket: &str, purpose: Purpose) -> bool {
        let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        matches!(issued.remove(ticket), Some((p, at)) if p == purpose && at.elapsed() < TICKET_TTL)
    }
}
