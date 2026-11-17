//! `hermes-desktop-gateway`: serve the Hermes Desktop handshake on a loopback port.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::process::ExitCode;

use clap::Parser;
use hermes_desktop_gateway::{new_token, serve, MIN_TOKEN_LEN, TOKEN_ENV};

/// Preview: a backend Hermes Desktop can attach to with 'Connect to existing Hermes'.
/// The token comes from HERMES_DASHBOARD_SESSION_TOKEN; without it one is made and printed.
/// A supplied token must be random: only its length (at least 32 characters) is checked and
/// connection attempts are not rate limited, so prefer the generated one.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Loopback address to bind (127.0.0.1, ::1 or localhost).
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Port to bind; 0 picks a free one.
    #[arg(long, default_value_t = 9119)]
    port: u16,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let ip = match args.host.as_str() {
        "localhost" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        other => match other.parse::<IpAddr>() {
            Ok(ip) if ip.is_loopback() => ip,
            _ => {
                eprintln!(
                    "refusing --host {}: only a loopback address (127.0.0.1, ::1 or localhost) is allowed",
                    args.host
                );
                return ExitCode::FAILURE;
            }
        },
    };
    let (token, generated) = match std::env::var(TOKEN_ENV) {
        Ok(token) if !token.is_empty() => {
            if token.chars().count() < MIN_TOKEN_LEN {
                eprintln!("{TOKEN_ENV} must be at least {MIN_TOKEN_LEN} characters");
                return ExitCode::FAILURE;
            }
            (token, false)
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            eprintln!("{TOKEN_ENV} is set but not valid Unicode");
            return ExitCode::FAILURE;
        }
        _ => (new_token(), true),
    };
    let listener = match tokio::net::TcpListener::bind((ip, args.port)).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("cannot bind {ip} port {}: {err}", args.port);
            return ExitCode::FAILURE;
        }
    };
    let addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("cannot read the bound address: {err}");
            return ExitCode::FAILURE;
        }
    };
    if generated {
        eprintln!("Connect Hermes Desktop with this URL and token:");
        eprintln!("  url: http://{addr}");
        eprintln!("  token: {token}");
    }
    // Hermes Desktop's launcher reads the port from these lines.
    let mut stdout = std::io::stdout().lock();
    let announced = writeln!(stdout, "HERMES_BACKEND_READY port={}", addr.port())
        .and_then(|()| writeln!(stdout, "HERMES_DASHBOARD_READY port={}", addr.port()))
        .and_then(|()| stdout.flush());
    drop(stdout);
    if let Err(err) = announced {
        eprintln!("cannot write the readiness lines: {err}");
        return ExitCode::FAILURE;
    }
    match serve(listener, token).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("server stopped: {err}");
            ExitCode::FAILURE
        }
    }
}
