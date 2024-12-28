mod engine;
mod ui;

use slog::{o, Drain};
use slog_scope::error;
use tokio::{self, sync::mpsc};

#[tokio::main]
async fn main() {
    let decorator = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    let logger = slog::Logger::root(drain, o!());
    let _guard = slog_scope::set_global_logger(logger); // We must hold the guard due to how slog_scope works

    let (daemon_events_tx, daemon_events_rx) = mpsc::channel::<anyhow::Error>(32);
    let daemon = match engine::Daemon::new() {
        Ok(daemon) => daemon,
        Err(e) => {
            error!("Failed to create daemon"; "error" => format!("{e}"));
            return;
        }
    };
    let server = ui::Server::new(daemon.tx.clone(), daemon_events_rx);

    if let Err(e) = tokio::try_join!(server.run(), daemon.run(daemon_events_tx)) {
        error!("Failed to run server/daemon"; "error" => format!("{e}"));
        return;
    };
}
