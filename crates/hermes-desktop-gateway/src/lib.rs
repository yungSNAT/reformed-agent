//! Preview backend that Hermes Desktop attaches to by URL and token (stub).

/// Serves the desktop gateway on `listener` until the task is dropped.
pub async fn serve(listener: tokio::net::TcpListener, token: String) -> std::io::Result<()> {
    let _ = (listener, token);
    Ok(())
}
