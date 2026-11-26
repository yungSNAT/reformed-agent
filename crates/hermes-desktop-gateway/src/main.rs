//! `hermes-desktop-gateway`: serve Hermes Desktop on a loopback port.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::process::ExitCode;

use clap::Parser;
use hermes_desktop_gateway::{
    new_token, serve, serve_child, serve_with_profiles, Chat, ChildGateways, MIN_TOKEN_LEN,
    TOKEN_ENV,
};

/// Preview: a backend Hermes Desktop can attach to with 'Connect to existing Hermes' and chat
/// through, with the model and sessions of the Hermes home (HERMES_HOME, else ~/.hermes).
/// Replies use no tools.
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
    /// Stop when standard input reaches end of file. A launcher passes this with a pipe as
    /// standard input, so the gateway and its token stop with the launcher even when the
    /// launcher is killed or crashes.
    #[arg(long, hide = true)]
    exit_when_stdin_closes: bool,
    /// Serve one named profile for a parent gateway, which starts this with the profile's home
    /// and a private token. The parent's connections may then speak the canonical wire with
    /// that token alone.
    #[arg(long, hide = true)]
    child: bool,
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
    // Taken before any configuration is loaded, so children start from this environment.
    let children = (!args.child)
        .then(std::env::current_exe)
        .and_then(Result::ok)
        .map(ChildGateways::new);
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
        _ if args.child => {
            eprintln!("--child needs {TOKEN_ENV}");
            return ExitCode::FAILURE;
        }
        _ => (new_token(), true),
    };
    let config = match hermes_config::load_config(None) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("cannot load the Hermes config: {err}");
            return ExitCode::FAILURE;
        }
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
        if args.exit_when_stdin_closes && err.kind() == std::io::ErrorKind::BrokenPipe {
            return ExitCode::SUCCESS;
        }
        let _ = writeln!(std::io::stderr(), "cannot write the readiness lines: {err}");
        return ExitCode::FAILURE;
    }
    let stdin_closed = stdin_closed(args.exit_when_stdin_closes);
    let chat = Chat::new(config);
    let served = async {
        match children {
            Some(children) => serve_with_profiles(listener, token, chat, children).await,
            None if args.child => serve_child(listener, token, chat).await,
            None => serve(listener, token, chat).await,
        }
    };
    tokio::select! {
        served = served => match served {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "server stopped: {err}");
                ExitCode::FAILURE
            }
        },
        () = stdin_closed => {
            let _ = writeln!(std::io::stderr(), "standard input closed; stopping");
            ExitCode::SUCCESS
        }
    }
}

/// Resolves once standard input reaches end of file or fails, when `watch` is set; never
/// otherwise. A plain thread reads it, as a blocked runtime read would hold up shutdown.
async fn stdin_closed(watch: bool) {
    if !watch {
        return std::future::pending().await;
    }
    let (closed, wait) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        let _ = closed.send(());
    });
    let _ = wait.await;
}
