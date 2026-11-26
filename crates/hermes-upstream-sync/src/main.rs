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
    /// Logging level: DEBUG, INFO, WARNING or ERROR (others mean INFO).
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
    };
    let now = jiff::Zoned::now();
    eprintln!(
        "[{},{:03}] {name} upstream-webhook-sync: {message}",
        now.strftime("%Y-%m-%d %H:%M:%S"),
        now.subsec_nanosecond() / 1_000_000
    );
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = match cli.log_level.to_ascii_uppercase().as_str() {
        "DEBUG" => Level::Debug,
        "WARNING" | "WARN" => Level::Warning,
        "ERROR" | "CRITICAL" => Level::Error,
        _ => Level::Info,
    };
    LOG_LEVEL.store(level as u8, Ordering::Relaxed);
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
        secret: std::env::var("GITHUB_WEBHOOK_SECRET").unwrap_or_default(),
        allow_unsigned: args.allow_unsigned,
        expected_repo: args.expected_repo,
        expected_ref: args.expected_ref,
        backend_name: "sqlite".to_string(),
    };
    listen::check_secret(&config).map_err(|e| (2, e))?;
    let queue = queue::SqliteQueue::open(&args.sqlite_path).map_err(|e| (1, e))?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| (1, format!("could not start the runtime: {e}")))?;
    runtime
        .block_on(listen::serve(config, Arc::new(queue)))
        .map_err(|e| (1, e))
}
