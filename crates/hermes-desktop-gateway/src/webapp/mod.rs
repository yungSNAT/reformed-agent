//! The browser-hosted Desktop (Webapp) on the local runtime's own listener (upstream
//! gateway/webapp_listener.py and hermes_cli/web_routers/webapp.py at v0.23.2).
//!
//! `webapp.enabled` in the home's `config.yaml` makes the runtime bind its one listener to
//! `webapp.host`:`webapp.port` (default 127.0.0.1:9120) instead of a free loopback port; native
//! clients keep using it. The runtime then serves the bundle `HERMES_WEBAPP_DIST` names (this build
//! ships none and builds none), the 2 handoff pages, `POST /webapp/window-session` and the
//! browser's ticket routes, and answers `webapp-status`, `webapp-launch` and `webapp-sign-out` on
//! its control socket.
//!
//! The browser's credential (`credential`) is separate from the native one-use tickets: a request
//! carrying it is admitted on its own branch, with no `Origin` or exactly this listener's own,
//! and a browser socket needs a browser socket ticket and that `Origin`. Binds upstream gates
//! behind its sign-in (a wildcard host, another host, `webapp.public_url`) stay off here.
//!
//! shortcut: no sign-in gate, Web Access, uploads, voice, host terminal or relay; one profile a
//! runtime; Unix only (local mode is). Upgrade path: upstream's modules above.

pub mod api;
pub mod routes;

#[cfg(unix)]
pub mod assets;
#[cfg(unix)]
pub mod credential;
#[cfg(unix)]
pub mod handoff;
#[cfg(unix)]
pub mod settings;

#[cfg(unix)]
mod serve;
#[cfg(unix)]
pub use serve::*;
