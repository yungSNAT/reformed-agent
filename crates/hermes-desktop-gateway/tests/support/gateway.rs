//! A desktop gateway for the tests, served on a free local port.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

/// Serves a gateway that takes `token`, configured from the `config.yaml` in `home`, with its
/// socket write timeout when given.
pub async fn serve(home: &Path, token: &str, write_timeout: Option<Duration>) -> SocketAddr {
    let config = hermes_config::load_config(home.to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut chat = hermes_desktop_gateway::Chat::new(config);
    if let Some(timeout) = write_timeout {
        chat = chat.with_write_timeout(timeout);
    }
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        token.to_string(),
        chat,
    ));
    addr
}
