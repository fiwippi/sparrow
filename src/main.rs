mod engine;
mod ui;

use slog::Drain;
use slog_scope::error;
use tokio;

#[tokio::main]
async fn main() {
    let decorator = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    let logger = slog::Logger::root(drain, slog::o!());
    let _guard = slog_scope::set_global_logger(logger); // We must hold the guard due to how slog_scope works

    let (daemon, daemon_cmd_tx, daemon_errors_rx) = match engine::Daemon::new() {
        Ok((daemon, cmd_tx, err_rx)) => (daemon, cmd_tx, err_rx),
        Err(e) => {
            error!("Failed to create daemon"; "error" => format!("{e}"));
            return;
        }
    };
    let server = ui::Server::new(daemon_cmd_tx, daemon_errors_rx);

    if let Err(e) = tokio::try_join!(server.run(), daemon.run()) {
        error!("Failed to run server/daemon"; "error" => format!("{e}"));
        return;
    };
}
