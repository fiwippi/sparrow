mod engine;
mod ui;

use std::str::FromStr;

use clap::Parser;
use slog::Drain;
use slog_scope::error;
use tokio;

#[derive(Parser, Debug)]
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

    let (daemon, daemon_cmd_tx) = match engine::Daemon::new() {
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
