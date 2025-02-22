mod engine;
mod ui;

use std::{str::FromStr, time::Duration};

use clap::{Parser, ValueEnum};
use slog::Drain;
use slog_scope::error;
use tokio;

#[derive(Debug, Clone, ValueEnum)]
enum BufferSize {
    XS = 1024,
    S = 2048,
    M = 4096,
    L = 8192,
    XL = 16384,
  }

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(
        long, 
        default_value_t = String::from("INFO"),
        value_parser = clap::builder::PossibleValuesParser::new(
            ["CRITICAL", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"]
        ),
    )]
    log_level: String,

    /// The size of the buffer used to calculate the final LED colour,
    /// smaller values are quicker to process at the cost of reduced
    /// accuracy
    #[arg(
        short = 'b',
        long, 
        value_enum,
        default_value_t = BufferSize::XL,
        verbatim_doc_comment
    )]
    buffer_size: BufferSize,

    /// How long a colour is interpolated before a new measurement is
    /// taken, smaller values lead to rougher colour changes
    #[arg(
        short = 'p',
        long, 
        default_value_t = 250,
        verbatim_doc_comment
    )]
    interpolation_period: u64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let decorator = slog_term::PlainSyncDecorator::new(std::io::stderr());
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    let drain = slog::LevelFilter::new(
        drain,
        slog::Level::from_str(args.log_level.as_str()).unwrap(),
    )
    .fuse();
    let logger = slog::Logger::root(drain, slog::o!());

    // Normally the global logger is reset once the guard is dropped,
    // this causes further attempts to log to panic. We cancel this
    // because there are some cases where closures continue to try to
    // log even after the guard is dropped
    let guard = slog_scope::set_global_logger(logger);
    guard.cancel_reset();

    let config = engine::Config { 
        buffer_size: args.buffer_size as usize,
        min_period: Duration::from_millis(args.interpolation_period),
     };
    let (daemon, daemon_cmd_tx) = match engine::Daemon::new(config) {
        Ok((daemon, cmd_tx)) => (daemon, cmd_tx),
        Err(e) => {
            error!("Failed to create daemon"; "error" => format!("{e}"));
            return;
        }
    };
    let server = ui::Server::new(daemon_cmd_tx);

    if let Err(e) = tokio::try_join!(server.run(), daemon.run()) {
        error!("Failed to run server/daemon"; "error" => format!("{e}"));
        return;
    };
}
