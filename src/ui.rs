use std::{fmt, str::FromStr, time::Instant};

use crate::audio;

use askama::Template;
use axum::{
    body::{Body, Bytes},
    extract::{Query, Request, State},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use http::{header, status::StatusCode};
use serde::{de, Deserialize, Deserializer};
use slog_scope::{error, info};

pub struct Server {
    app: Router,
}

// TODO Return concrete errors from API
//      https://github.com/tokio-rs/axum/blob/main/examples/error-handling/src/main.rs
impl Server {
    pub fn new(daemon_tx: audio::Tx) -> Self {
        Self {
            app: Router::new()
                .route("/", get(home))
                .nest("/assets", asset_routes())
                .nest("/api/v1", api_routes())
                .with_state(daemon_tx)
                .layer(middleware::from_fn(log_requests)),
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:4181").await?;
        info!("Listening on {:?}...", listener.local_addr().unwrap());
        axum::serve(listener, self.app).await?;

        Ok(())
    }
}

// -- Middleware / Helpers

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
            Ok(html) => (StatusCode::OK, Html(html)).into_response(),
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

const HTMX_JS_FILE: &[u8] = include_bytes!("../assets/htmx.min.js");

fn asset_routes() -> Router<audio::Tx> {
    // TODO Create a favicon.ico route

    async fn get_htmx() -> impl IntoResponse {
        match Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(Bytes::from_static(HTMX_JS_FILE)))
        {
            Ok(resp) => resp,
            Err(e) => {
                error!("Failed to generate response for htmx asset"; "error" => format!("{e}"));
                (StatusCode::INTERNAL_SERVER_ERROR, "").into_response()
            }
        }
    }

    Router::new().route("/htmx.min.js", get(get_htmx))
}

fn api_routes() -> Router<audio::Tx> {
    #[derive(Template)]
    #[template(path = "device-info.html")]
    struct DeviceNamesTemplate {
        devices: Vec<audio::DeviceInfo>,
    }

    /// When we GET the list of input/output devices,
    /// we use this query parameter to request to
    /// change the currently used device by supplying
    /// a device name
    #[derive(Deserialize)]
    struct ChangeDeviceRequest {
        #[serde(default, deserialize_with = "empty_string_as_none")]
        device: Option<String>,
    }

    async fn get_audio_inputs(
        State(daemon_tx): State<audio::Tx>,
        change_request: Query<ChangeDeviceRequest>,
    ) -> impl IntoResponse {
        if let Some(device) = change_request.0.device {
            if let Err(e) = daemon_tx.set_audio_input(device).await {
                error!("Failed to set audio input"; "error" => format!("{e}"));
                return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
            };
        }

        match daemon_tx.list_audio_inputs().await {
            Ok(devices) => HtmlTemplate(DeviceNamesTemplate { devices }).into_response(),
            Err(e) => {
                error!("Failed to list audio inputs"; "error" => format!("{e}"));
                (StatusCode::INTERNAL_SERVER_ERROR, "").into_response()
            }
        }
    }

    async fn get_audio_outputs(
        State(daemon_tx): State<audio::Tx>,
        change_request: Query<ChangeDeviceRequest>,
    ) -> impl IntoResponse {
        if let Some(device) = change_request.0.device {
            if let Err(e) = daemon_tx.set_audio_output(device).await {
                error!("Failed to set audio output"; "error" => format!("{e}"));
                return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
            };
        }

        match daemon_tx.list_audio_outputs().await {
            Ok(devices) => HtmlTemplate(DeviceNamesTemplate { devices }).into_response(),
            Err(e) => {
                error!("Failed to list audio outputs"; "error" => format!("{e}"));
                (StatusCode::INTERNAL_SERVER_ERROR, "").into_response()
            }
        }
    }

    Router::new()
        .route("/devices/input", get(get_audio_inputs))
        .route("/devices/output", get(get_audio_outputs))
}
