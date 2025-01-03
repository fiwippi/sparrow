use std::time::Instant;

use super::routes;
use crate::engine;

use axum::{
    extract::Request,
    middleware::{self, Next},
    response::IntoResponse,
    routing::get,
    Router,
};
use slog_scope::{error, info};
use tokio::{signal, sync::mpsc};

pub struct Server {
    app: Router,
    engine_tx: engine::Tx,
}

impl Server {
    pub fn new(engine_tx: engine::Tx, engine_errors_rx: mpsc::Receiver<anyhow::Error>) -> Self {
        Self {
            app: Router::new()
                .route("/", get(routes::home))
                .nest("/assets", routes::assets())
                .nest("/api/v1", routes::api(engine_errors_rx))
                .with_state(engine_tx.clone())
                .layer(middleware::from_fn(log_requests)),
            engine_tx,
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:4181").await?;
        info!("Listening on {:?}...", listener.local_addr().unwrap());
        axum::serve(listener, self.app)
            .with_graceful_shutdown(shutdown_signal(self.engine_tx.clone()))
            .await?;

        Ok(())
    }
}

async fn shutdown_signal(engine_tx: engine::Tx) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install terminate signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    if let Err(e) = engine_tx.shutdown().await {
        error!("Failed to shutdown daemon"; "error" => format!("{e}"))
    } else {
        info!("Daemon shutdown")
    }
}

async fn log_requests(request: Request, next: Next) -> impl IntoResponse {
    let uri = request.uri().to_string();
    let method = request.method().to_string();

    let start = Instant::now();
    let resp = next.run(request).await;
    let elapsed = start.elapsed();
    let status = resp.status().to_string();

    info!("Request"; "status" => status, "method" => method, "uri" => uri, "elapsed" => format!("{elapsed:?}"));

    resp
}
