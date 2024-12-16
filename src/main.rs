mod audio;
mod ui;

use slog::{o, Drain}; // TODO Use the async handler for slog_scope
use slog_scope::error;
use tokio;

#[tokio::main]
async fn main() {
    let decorator = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let logger = slog::Logger::root(drain, o!());
    let _guard = slog_scope::set_global_logger(logger); // We must hold the guard due to how slog_scope works

    let daemon = audio::Daemon::new();
    let (daemon_handle, daemon_tx) = daemon.run();
    let server = ui::Server::new(daemon_tx.clone());
    if let Err(e) = server.run().await {
        error!("Failed to run server"; "error" => format!("{e}"));
    }

    // TODO Graceful shutdown for HTTP server and daemon
    if let Err(e) = daemon_tx.shutdown().await {
        error!("Failed to shutdown daemon"; "error" => format!("{e}"));
    }
    if let Err(e) = daemon_handle.await {
        error!("Failed to join on daemon"; "error" => format!("{e}"));
    }
}
