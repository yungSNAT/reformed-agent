//! How a browser gets the credential without it crossing a URL, argv or output (upstream
//! hermes_cli/web_routers/webapp.py): one-use tickets, the 2 credential-free pages that spend
//! them, `POST /webapp/window-session` and the browser's socket tickets.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::credential::random_token;

/// How long a launch link works (upstream `LAUNCH_TICKET_TTL_SECONDS`).
pub const LAUNCH_TTL: Duration = Duration::from_secs(60);
/// How long a child window's ticket works (upstream `WINDOW_TICKET_TTL_SECONDS`).
pub const WINDOW_TTL: Duration = Duration::from_secs(30);
/// How long a socket ticket works (upstream dashboard_auth/ws_tickets.py `TTL_SECONDS`).
pub const SOCKET_TTL: Duration = Duration::from_secs(30);
const MAX_HANDOFF: usize = 128;
const MAX_SOCKET: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Crossed a browser launcher's argv: spendable only by this user's connection (Linux).
    Launch,
    /// Printed for the user to open by hand where no connection's owner can be checked.
    Printed,
    /// A child window's, minted by an authorised tab.
    Window,
    /// One dial of `/api/ws`.
    Socket,
}

struct Entry {
    expires: Instant,
    generation: u64,
    kind: Kind,
}

/// The browser's tickets, apart from the native ones, so neither kind passes as the other.
#[derive(Default)]
pub struct Tickets {
    issued: Mutex<HashMap<String, Entry>>,
}

impl Tickets {
    /// A new ticket of `kind` for credential `generation`; drops expired and older entries first.
    pub fn mint(&self, kind: Kind, generation: u64) -> Result<String, &'static str> {
        let ttl = match kind {
            Kind::Launch | Kind::Printed => LAUNCH_TTL,
            Kind::Window => WINDOW_TTL,
            Kind::Socket => SOCKET_TTL,
        };
        let now = Instant::now();
        let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        issued.retain(|_, e| e.expires > now && e.generation >= generation);
        let socket = kind == Kind::Socket;
        let held = issued
            .values()
            .filter(|e| (e.kind == Kind::Socket) == socket)
            .count();
        if held >= if socket { MAX_SOCKET } else { MAX_HANDOFF } {
            return Err("Too many pending Webapp tickets; try again shortly");
        }
        let ticket = random_token();
        issued.insert(
            ticket.clone(),
            Entry {
                expires: now + ttl,
                generation,
                kind,
            },
        );
        Ok(ticket)
    }

    /// Spends a handoff ticket (launch, printed or window), giving its generation. A launch ticket
    /// whose redeemer `launch_peer_ok` refuses is left unspent for the real browser.
    pub fn take_handoff(&self, ticket: &str, launch_peer_ok: impl FnOnce() -> bool) -> Option<u64> {
        let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        let entry = issued.get(ticket)?;
        match entry.kind {
            Kind::Socket => return None,
            Kind::Launch if !launch_peer_ok() => return None,
            _ => {}
        }
        let entry = issued.remove(ticket)?;
        (entry.expires > Instant::now()).then_some(entry.generation)
    }

    /// Spends a socket ticket, giving its generation.
    pub fn take_socket(&self, ticket: &str) -> Option<u64> {
        let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        if issued.get(ticket)?.kind != Kind::Socket {
            return None;
        }
        let entry = issued.remove(ticket)?;
        (entry.expires > Instant::now()).then_some(entry.generation)
    }

    /// Drops every ticket of a generation before `generation`.
    pub fn retire_before(&self, generation: u64) {
        let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        issued.retain(|_, e| e.generation >= generation);
    }
}

/// Whether a launch link opened by a browser launcher can be checked here: Linux, not WSL, where
/// the redeeming connection's owner is read from `/proc/net/tcp*`. Elsewhere the link is printed.
pub fn launch_links_checked() -> bool {
    cfg!(target_os = "linux") && !is_wsl()
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|r| {
            let r = r.to_ascii_lowercase();
            r.contains("microsoft") || r.contains("wsl")
        })
        .unwrap_or(false)
}

/// The owner uid of the client end of a loopback TCP connection, from `/proc/net/tcp` and
/// `/proc/net/tcp6`: the row whose local address is `client` and remote address `server`.
pub fn loopback_peer_uid(client: SocketAddr, server: SocketAddr) -> Option<u32> {
    let wanted = (normal(client), normal(server));
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        if let Some(uid) = uid_in_table(&text, wanted) {
            return Some(uid);
        }
    }
    None
}

fn normal(addr: SocketAddr) -> (IpAddr, u16) {
    let ip = match addr.ip() {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    (ip, addr.port())
}

/// One `/proc/net/tcp*` table's uid for the row joining `wanted`'s 2 ends.
pub fn uid_in_table(text: &str, wanted: ((IpAddr, u16), (IpAddr, u16))) -> Option<u32> {
    text.lines().skip(1).find_map(|row| {
        let fields: Vec<&str> = row.split_whitespace().collect();
        let local = row_address(fields.get(1)?)?;
        let remote = row_address(fields.get(2)?)?;
        ((local, remote) == wanted)
            .then(|| fields.get(7)?.parse().ok())
            .flatten()
    })
}

/// A `/proc/net/tcp*` address field: each 32-bit word of the address in host byte order, in hex.
fn row_address(field: &str) -> Option<(IpAddr, u16)> {
    let (host, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let mut bytes = Vec::with_capacity(16);
    for word in host.as_bytes().chunks(8) {
        let word = u32::from_str_radix(std::str::from_utf8(word).ok()?, 16).ok()?;
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    let ip = match bytes.len() {
        4 => IpAddr::from(<[u8; 4]>::try_from(bytes).ok()?),
        16 => IpAddr::from(<[u8; 16]>::try_from(bytes).ok()?),
        _ => return None,
    };
    Some(normal(SocketAddr::new(ip, port)))
}

/// A credential-free, uncached page whose only script is `script` (upstream `_handoff_page`).
pub fn handoff_page(title: &str, status: &str, script: &str) -> (String, String) {
    let nonce = random_token();
    let script = script.replace("__BASE_PATH__", "\"\"");
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\">\
         <title>{title}</title><p id=\"status\">{status}</p><script nonce=\"{nonce}\">{script}</script>"
    );
    let policy = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; connect-src 'self'; frame-ancestors 'none'; \
         base-uri 'none'; form-action 'none'"
    );
    (html, policy)
}

/// The launch page's script (upstream `_LAUNCH_SCRIPT`, unchanged): spends the fragment's ticket,
/// keeps the credential in this tab's sessionStorage and enters the Webapp at a clean address.
pub const LAUNCH_SCRIPT: &str = r#"
const basePath = __BASE_PATH__;
const params = new URLSearchParams(location.hash.slice(1));
history.replaceState(null, '', location.pathname);
const ticket = params.get('ticket') || '';
const show = message => { document.getElementById('status').textContent = message; };
const fail = () => show('This launch link was already used or has expired. Run hermes webapp again for a new one.');
if (!/^[A-Za-z0-9_-]{43}$/.test(ticket)) {
  fail();
} else {
  fetch(`${basePath}/webapp/window-session`, {
    method: 'POST', credentials: 'same-origin', headers: {'X-Hermes-Window-Ticket': ticket}
  }).then(response => response.ok ? response.json() : Promise.reject()).then(({token}) => {
    if (typeof token !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(token)) throw new Error();
    try {
      sessionStorage.setItem(`hermes.webapp.session.v1:${JSON.stringify([location.origin, basePath])}`, token);
    } catch {
      show('This browser does not let the Webapp keep its sign-in in this tab. Allow site data for this '
        + 'address, then run hermes webapp again.');
      return;
    }
    const target = new URL(`${basePath}/`, location.origin);
    target.search = params.get('query') || '';
    location.replace(target.href);
  }).catch(fail);
}
"#;

/// The child window's script (upstream `_WINDOW_SCRIPT`, unchanged).
pub const WINDOW_SCRIPT: &str = r#"
const basePath = __BASE_PATH__;
const params = new URLSearchParams(location.hash.slice(1));
history.replaceState(null, '', location.pathname);
const id = params.get('channel') || '';
const status = document.getElementById('status');
if (!/^[a-f0-9-]{36}$/.test(id)) {
  status.textContent = 'Open this window from an authorized Hermes Webapp tab.';
} else {
  const channel = new BroadcastChannel(`hermes.webapp.window:${id}`);
  let receiving = false;
  let finished = false;
  const controller = new AbortController();
  const fail = message => {
    if (finished) return;
    finished = true;
    controller.abort();
    status.textContent = message;
    channel.postMessage({type: 'error'});
    channel.close();
    clearTimeout(timeout);
  };
  const timeout = setTimeout(() => fail('This window link expired. Open a new window from the original tab.'), 30000);
  channel.onmessage = async event => {
    if (receiving) return;
    if (event.data?.error) { fail(event.data.error); return; }
    const ticket = event.data?.ticket;
    if (typeof ticket !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(ticket)) return;
    receiving = true;
    try {
      const response = await fetch(`${basePath}/webapp/window-session`, {
        method: 'POST', credentials: 'same-origin',
        headers: {'X-Hermes-Window-Ticket': ticket}, signal: controller.signal
      });
      if (!response.ok) throw new Error('Window authorization expired. Open a new window from the original tab.');
      const {token} = await response.json();
      if (finished) return;
      if (typeof token !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(token)) throw new Error('Invalid window authorization.');
      sessionStorage.setItem(`hermes.webapp.session.v1:${JSON.stringify([location.origin, basePath])}`, token);
      const target = new URL(`${basePath}/`, location.origin);
      target.search = params.get('query') || '';
      target.hash = params.get('route') || '/';
      channel.postMessage({type: 'done'});
      finished = true;
      clearTimeout(timeout);
      channel.close();
      location.replace(target.href);
    } catch (error) { fail(error.message || 'Could not authorize this window. Open it again from the original tab.'); }
  };
  channel.postMessage({type: 'ready'});
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_spent_once_and_only_as_its_kind() {
        let tickets = Tickets::default();
        let window = tickets.mint(Kind::Window, 1).unwrap();
        assert_eq!(
            tickets.take_socket(&window),
            None,
            "a window ticket opens no socket"
        );
        assert_eq!(tickets.take_handoff(&window, || true), Some(1));
        assert_eq!(tickets.take_handoff(&window, || true), None);
        let socket = tickets.mint(Kind::Socket, 1).unwrap();
        assert_eq!(tickets.take_handoff(&socket, || true), None);
        assert_eq!(tickets.take_socket(&socket), Some(1));
        assert_eq!(tickets.take_socket(&socket), None);
    }

    #[test]
    fn a_refused_launch_redeemer_leaves_the_ticket_for_the_real_browser() {
        let tickets = Tickets::default();
        let launch = tickets.mint(Kind::Launch, 1).unwrap();
        assert_eq!(tickets.take_handoff(&launch, || false), None);
        assert_eq!(tickets.take_handoff(&launch, || true), Some(1));
        // A printed ticket needs no peer check.
        let printed = tickets.mint(Kind::Printed, 1).unwrap();
        assert_eq!(tickets.take_handoff(&printed, || false), Some(1));
    }

    #[test]
    fn retiring_a_generation_voids_its_tickets_and_pending_ones_are_capped() {
        let tickets = Tickets::default();
        let old = tickets.mint(Kind::Window, 1).unwrap();
        tickets.retire_before(2);
        assert_eq!(tickets.take_handoff(&old, || true), None);
        for _ in 0..MAX_HANDOFF {
            tickets.mint(Kind::Window, 2).unwrap();
        }
        assert!(tickets.mint(Kind::Launch, 2).is_err());
        assert!(
            tickets.mint(Kind::Socket, 2).is_ok(),
            "socket tickets have their own cap"
        );
    }

    #[test]
    fn the_proc_table_row_joining_both_ends_gives_the_uid() {
        // 127.0.0.1:40000 -> 127.0.0.1:9120 owned by uid 1000, and the reverse row by 0.
        let local = format!("{:08X}", u32::from_ne_bytes([127, 0, 0, 1]));
        let table = format!(
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid\n\
             0: {local}:9C40 {local}:23A0 01 00000000:00000000 00:00000000 00000000  1000 0 1\n\
             1: {local}:23A0 {local}:9C40 01 00000000:00000000 00:00000000 00000000     0 0 2\n"
        );
        let lo = IpAddr::from([127, 0, 0, 1]);
        assert_eq!(uid_in_table(&table, ((lo, 40000), (lo, 9120))), Some(1000));
        assert_eq!(uid_in_table(&table, ((lo, 9120), (lo, 40000))), Some(0));
        assert_eq!(uid_in_table(&table, ((lo, 1), (lo, 9120))), None);
    }

    #[test]
    fn ipv4_mapped_addresses_match_their_ipv4_rows() {
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:5".parse().unwrap();
        assert_eq!(normal(mapped).0, IpAddr::from([127, 0, 0, 1]));
    }

    #[test]
    fn handoff_pages_carry_no_credential_and_a_nonce_policy() {
        let (html, policy) = handoff_page("Opening Hermes", "Opening", LAUNCH_SCRIPT);
        assert!(html.contains("const basePath = \"\";"));
        let nonce = policy
            .split("'nonce-")
            .nth(1)
            .and_then(|r| r.split('\'').next())
            .unwrap();
        assert!(html.contains(&format!("nonce=\"{nonce}\"")));
        assert!(policy.contains("frame-ancestors 'none'"));
    }
}
