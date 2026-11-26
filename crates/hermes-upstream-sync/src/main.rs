//! Webhook-driven upstream sync: a GitHub webhook receiver that queues
//! upstream push events. Port of `scripts/upstream_webhook_sync.py`; the
//! worker follows in a later change.

mod event;
mod listen;
mod queue;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "hermes-upstream-sync", about = "Webhook-driven upstream sync")]
struct Cli {
    /// Logging level: DEBUG, INFO, WARNING, ERROR or CRITICAL (others mean
    /// INFO).
    #[arg(long, default_value = "INFO")]
    log_level: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the GitHub webhook receiver.
    Listen(ListenArgs),
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Backend {
    Sqlite,
    Sqs,
    Kafka,
}

#[derive(clap::Args)]
struct ListenArgs {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8099)]
    port: u16,
    #[arg(long, default_value = "/github/upstream-sync")]
    path: String,
    /// Accept deliveries without a signature when GITHUB_WEBHOOK_SECRET is
    /// unset; only on a loopback --host.
    #[arg(long)]
    allow_unsigned: bool,
    #[arg(long, default_value = "NousResearch/hermes-agent")]
    expected_repo: String,
    #[arg(long, default_value = "refs/heads/main")]
    expected_ref: String,
    #[arg(long, value_enum, default_value = "sqlite")]
    backend: Backend,
    #[arg(long, default_value = ".sync-queue/upstream-events.db")]
    sqlite_path: PathBuf,
    /// Default: UPSTREAM_SYNC_SQS_QUEUE_URL.
    #[arg(long)]
    sqs_queue_url: Option<String>,
    #[arg(long = "sqs-region")]
    _sqs_region: Option<String>,
    /// Default: UPSTREAM_SYNC_KAFKA_BOOTSTRAP, else 127.0.0.1:9092.
    #[arg(long)]
    kafka_bootstrap: Option<String>,
    /// Default: UPSTREAM_SYNC_KAFKA_TOPIC, else hermes-upstream-sync.
    #[arg(long)]
    kafka_topic: Option<String>,
}

#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub(crate) enum Level {
    Debug = 10,
    Info = 20,
    Warning = 30,
    Error = 40,
    /// A threshold only, as in Python's logging: hides ERROR lines too.
    Critical = 50,
}

static LOG_LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

/// One stderr line in the script's logging format.
pub(crate) fn log(level: Level, message: &str) {
    if (level as u8) < LOG_LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let name = match level {
        Level::Debug => "DEBUG",
        Level::Info => "INFO",
        Level::Warning => "WARNING",
        Level::Error => "ERROR",
        Level::Critical => "CRITICAL",
    };
    let now = jiff::Zoned::now();
    eprintln!(
        "[{},{:03}] {name} upstream-webhook-sync: {message}",
        now.strftime("%Y-%m-%d %H:%M:%S"),
        now.subsec_nanosecond() / 1_000_000
    );
}

fn parse_level(name: &str) -> Level {
    match name.to_ascii_uppercase().as_str() {
        "DEBUG" => Level::Debug,
        "WARNING" | "WARN" => Level::Warning,
        "ERROR" => Level::Error,
        "CRITICAL" => Level::Critical,
        _ => Level::Info,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    LOG_LEVEL.store(parse_level(&cli.log_level) as u8, Ordering::Relaxed);
    let result = match cli.command {
        Command::Listen(args) => listen(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("hermes-upstream-sync: {message}");
            ExitCode::from(code)
        }
    }
}

fn listen(args: ListenArgs) -> Result<(), (u8, String)> {
    // argparse took these defaults from the environment.
    let flag_or_env = |flag: Option<String>, var: &str, default: &str| {
        flag.unwrap_or_else(|| std::env::var(var).unwrap_or_else(|_| default.to_string()))
    };
    let sqs_queue_url = flag_or_env(args.sqs_queue_url, "UPSTREAM_SYNC_SQS_QUEUE_URL", "");
    let kafka_bootstrap = flag_or_env(
        args.kafka_bootstrap,
        "UPSTREAM_SYNC_KAFKA_BOOTSTRAP",
        "127.0.0.1:9092",
    );
    let kafka_topic = flag_or_env(
        args.kafka_topic,
        "UPSTREAM_SYNC_KAFKA_TOPIC",
        "hermes-upstream-sync",
    );
    match args.backend {
        Backend::Sqlite => {}
        Backend::Sqs if sqs_queue_url.is_empty() => {
            return Err((2, "--sqs-queue-url is required for SQS backend".into()))
        }
        Backend::Kafka if kafka_bootstrap.is_empty() || kafka_topic.is_empty() => {
            return Err((
                2,
                "--kafka-bootstrap and --kafka-topic are required for Kafka backend".into(),
            ))
        }
        Backend::Sqs | Backend::Kafka => {
            return Err((
                2,
                "the sqs and kafka backends are not ported to Rust yet; use --backend sqlite"
                    .into(),
            ))
        }
    }
    let config = listen::ListenConfig {
        host: args.host,
        port: args.port,
        path: args.path,
        secret: read_secret(std::env::var_os("GITHUB_WEBHOOK_SECRET")).map_err(|e| (2, e))?,
        allow_unsigned: args.allow_unsigned,
        expected_repo: args.expected_repo,
        expected_ref: args.expected_ref,
        backend_name: "sqlite".to_string(),
    };
    let addrs = listen::resolve(&config.host, config.port).map_err(|e| (1, e))?;
    listen::check_secret(&config, &addrs).map_err(|e| (2, e))?;
    let queue = queue::SqliteQueue::open(&args.sqlite_path).map_err(|e| (1, e))?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| (1, format!("could not start the runtime: {e}")))?;
    runtime
        .block_on(listen::serve(&addrs, config, Arc::new(queue)))
        .map_err(|e| (1, e))
}

/// Unset or empty means no secret; set but not Unicode is refused rather
/// than read as unset.
fn read_secret(value: Option<std::ffi::OsString>) -> Result<String, String> {
    value.map_or(Ok(String::new()), |v| {
        v.into_string().map_err(|_| {
            "GITHUB_WEBHOOK_SECRET is set but is not valid Unicode: refusing to start".to_string()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_is_above_error_as_in_python() {
        assert!(parse_level("critical") > Level::Error);
        assert!(parse_level("error") == Level::Error);
        assert!(parse_level("verbose") == Level::Info);
    }

    #[test]
    fn a_secret_is_read_only_when_it_is_unicode() {
        assert_eq!(read_secret(None), Ok(String::new()));
        assert_eq!(read_secret(Some("s".into())), Ok("s".to_string()));
    }
}
