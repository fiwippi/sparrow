use std::{
    borrow::Cow,
    fmt,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::engine::{self, audio};

use askama::Template;
use axum::{
    body::{Body, Bytes},
    extract::{Query, Request, State},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use http::{header, status::StatusCode, HeaderValue};
use serde::{de, Deserialize, Deserializer};
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
                .route("/", get(home))
                .nest("/assets", asset_routes())
                .nest("/api/v1", api_routes(engine_errors_rx))
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

// -- Middleware / Helpers

async fn shutdown_signal(engine_tx: engine::Tx) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
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

/// Serde deserialization decorator to map empty Strings to None,
/// we use this in some cases to parse query parameters from htmx
/// requests
fn empty_string_as_none<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: fmt::Display,
{
    let opt = Option::<String>::deserialize(de)?;
    match opt.as_deref() {
        None | Some("") => Ok(None),
        Some(s) => FromStr::from_str(s).map_err(de::Error::custom).map(Some),
    }
}

// -- Templates

struct HtmlTemplate<T>(T);

/// This trait implementation allows us to convert the askama
/// HTML templates into valid HTML which our axum router can
/// then serve
impl<T> IntoResponse for HtmlTemplate<T>
where
    T: Template,
{
    fn into_response(self) -> Response {
        match self.0.render() {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                error!("Failed to render template"; "error" => format!("{e}"));
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

// -- Routes

async fn home() -> impl IntoResponse {
    #[derive(Template)]
    #[template(path = "home.html")]
    struct HomeTemplate;

    HtmlTemplate(HomeTemplate {})
}

fn asset_routes() -> Router<engine::Tx> {
    // TODO Create a favicon.ico route

    const HTMX_JS_FILE: &[u8] = include_bytes!("../assets/htmx.min.js");

    async fn get_htmx() -> impl IntoResponse {
        match Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(Bytes::from_static(HTMX_JS_FILE)))
        {
            Ok(resp) => resp,
            Err(e) => {
                error!("Failed to serve htmx asset"; "error" => format!("{e}"));
                (StatusCode::INTERNAL_SERVER_ERROR, "").into_response()
            }
        }
    }

    const SSE_JS_FILE: &[u8] = include_bytes!("../assets/sse.js");

    async fn get_sse() -> impl IntoResponse {
        match Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(Bytes::from_static(SSE_JS_FILE)))
        {
            Ok(resp) => resp,
            Err(e) => {
                error!("Failed to serve sse asset"; "error" => format!("{e}"));
                (StatusCode::INTERNAL_SERVER_ERROR, "").into_response()
            }
        }
    }

    Router::new()
        .route("/htmx.min.js", get(get_htmx))
        .route("/sse.js", get(get_sse))
}

fn api_routes(mut engine_errors_rx: mpsc::Receiver<anyhow::Error>) -> Router<engine::Tx> {
    /// When we GET the list of input/output devices,
    /// we use this query parameter to request to
    /// change the currently used device by supplying
    /// a device name
    #[derive(Deserialize)]
    struct ChangeDeviceRequest {
        #[serde(default, deserialize_with = "empty_string_as_none")]
        device: Option<String>,
    }

    #[derive(Template)]
    #[template(path = "audio-inputs.html")]
    struct AudioInputsTemplate<'a> {
        devices: Vec<audio::DeviceInfo>,
        err_message: Option<&'a str>,
    }

    async fn get_audio_inputs(
        State(engine_tx): State<engine::Tx>,
        change_request: Query<ChangeDeviceRequest>,
    ) -> impl IntoResponse {
        let set_input_res = if let Some(device) = change_request.0.device {
            engine_tx.set_audio_input(device).await
        } else {
            Ok(())
        };
        let list_inputs_res = engine_tx.list_audio_inputs().await;

        let template = match (set_input_res, list_inputs_res) {
            (Ok(_), Ok(devices)) => AudioInputsTemplate {
                devices,
                err_message: None,
            },
            (Err(e), Ok(devices)) => {
                error!("Failed to switch audio input"; "error" => format!("{e}"));
                AudioInputsTemplate {
                    devices,
                    err_message: Some("Failed to switch input device."),
                }
            }
            (Ok(_), Err(e)) => {
                error!("Failed to list audio inputs"; "error" => format!("{e}"));
                AudioInputsTemplate {
                    devices: vec![],
                    err_message: Some("Switched device, but failed to reload the available input devices. Please refresh the page."),
                }
            }
            (Err(left), Err(right)) => {
                error!("Failed to switch audio input"; "error" => format!("{left}"));
                error!("Failed to list audio inputs"; "error" => format!("{right}"));
                AudioInputsTemplate {
                    devices: vec![],
                    err_message: Some("Failed to switch device and reload the available input devices. Please refresh the page."),
                }
            }
        };
        HtmlTemplate(template).into_response()
    }

    #[derive(Template)]
    #[template(path = "audio-outputs.html")]
    struct AudioOutputsTemplate<'a> {
        devices: Vec<audio::DeviceInfo>,
        err_message: Option<&'a str>,
    }

    async fn get_audio_outputs(
        State(engine_tx): State<engine::Tx>,
        change_request: Query<ChangeDeviceRequest>,
    ) -> impl IntoResponse {
        let set_output_res = if let Some(device) = change_request.0.device {
            engine_tx.set_audio_output(device).await
        } else {
            Ok(())
        };
        let list_outputs_res = engine_tx.list_audio_outputs().await;

        let template = match (set_output_res, list_outputs_res) {
            (Ok(_), Ok(devices)) => AudioOutputsTemplate {
                devices,
                err_message: None,
            },
            (Err(e), Ok(devices)) => {
                error!("Failed to switch audio output"; "error" => format!("{e}"));
                AudioOutputsTemplate {
                    devices,
                    err_message: Some("Failed to switch input device."),
                }
            }
            (Ok(_), Err(e)) => {
                error!("Failed to list audio outputs"; "error" => format!("{e}"));
                AudioOutputsTemplate {
                    devices: vec![],
                    err_message: Some("Switched device, but failed to reload the available input devices. Please refresh the page."),
                }
            }
            (Err(left), Err(right)) => {
                error!("Failed to switch audio output"; "error" => format!("{left}"));
                error!("Failed to list audio outputs"; "error" => format!("{right}"));
                AudioOutputsTemplate {
                    devices: vec![],
                    err_message: Some("Failed to switch device and reload the available input devices. Please refresh the page."),
                }
            }
        };
        HtmlTemplate(template).into_response()
    }

    #[derive(Template)]
    #[template(path = "audio-status.html")]
    struct AudioStatusTemplate<'a> {
        status: Cow<'a, str>,
        err_message: Option<&'a str>,
    }

    async fn get_audio_status(State(engine_tx): State<engine::Tx>) -> impl IntoResponse {
        let template = match engine_tx.get_audio_status().await {
            Ok(status) => AudioStatusTemplate {
                status: format!("{status:?}").into(),
                err_message: None,
            },
            Err(e) => {
                error!("Failed to get audio status"; "error" => format!("{e}"));
                AudioStatusTemplate {
                    status: "N/A".into(),
                    err_message: Some("Failed to get audio status."),
                }
            }
        };
        HtmlTemplate(template).into_response()
    }

    #[derive(Template)]
    #[template(path = "audio-status-toggle.html")]
    struct AudioStatusToggleTemplate<'a> {
        err_message: Option<&'a str>,
    }

    async fn toggle_audio_status(State(engine_tx): State<engine::Tx>) -> impl IntoResponse {
        let resp = match engine_tx.toggle_audio_status().await {
            Ok(_) => {
                let template = AudioStatusToggleTemplate { err_message: None };
                let mut resp: Response<Body> = HtmlTemplate(template).into_response();
                resp.headers_mut().insert(
                    "HX-Trigger-After-Settle",
                    HeaderValue::from_static("audioStatusToggled"),
                );
                resp
            }
            Err(e) => {
                error!("Failed to toggle audio status"; "error" => format!("{e}"));
                HtmlTemplate(AudioStatusToggleTemplate {
                    err_message: Some("Failed to toggle audio playback."),
                })
                .into_response()
            }
        };
        resp
    }

    let event_log = Arc::new(Mutex::new(Vec::new()));
    let closure_event_log = Arc::clone(&event_log);
    tokio::spawn(async move {
        let log_retention = Duration::from_secs(5 * 60); // TODO Make configurable
        while let Some(error) = engine_errors_rx.recv().await {
            {
                let now = Instant::now();
                let mut log = closure_event_log.lock().unwrap();
                log.push((format!("{error:?}"), now));
                log.retain(|&(_, timestamp)| now.duration_since(timestamp) <= log_retention);
            }
            // Ensure the mutex is dropped post-event...
        }
    });

    #[derive(Template)]
    #[template(path = "logs.html")]
    struct LogsTemplate<'a> {
        logs: Vec<(&'a str, Instant)>,
    }

    let closure_event_log = Arc::clone(&event_log);
    let log_retention = Duration::from_secs(5 * 60); // TODO Make configurable
    let get_logs = move |State(_): State<engine::Tx>| async move {
        let now = Instant::now();
        let mut log = closure_event_log.lock().unwrap();
        log.retain(|&(_, timestamp)| now.duration_since(timestamp) <= log_retention);
        let logs = log.iter().map(|(s, t)| (s.as_ref(), *t)).collect();
        HtmlTemplate(LogsTemplate { logs }).into_response()
    };

    Router::new()
        .route("/audio/devices/input", get(get_audio_inputs))
        .route("/audio/devices/output", get(get_audio_outputs))
        .route("/audio/status", get(get_audio_status))
        .route("/audio/status/toggle", get(toggle_audio_status))
        .route("/logs", get(get_logs))
}
