use std::{
    borrow::Cow,
    fmt,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::engine::{self, audio, dmx, led};

use anyhow::anyhow;
use askama::Template;
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, patch, post},
    Form, Router,
};
use futures::TryFutureExt;
use http::{header, HeaderMap, HeaderValue, StatusCode};
use serde::{de, Deserialize, Deserializer};
use slog_scope::error;
use tokio::sync::mpsc;

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

pub async fn home() -> impl IntoResponse {
    #[derive(Template)]
    #[template(path = "home.html")]
    struct HomeTemplate;

    HtmlTemplate(HomeTemplate {})
}

pub fn assets() -> Router<engine::Tx> {
    // TODO Create a favicon.ico route

    const HTMX_JS_FILE: &[u8] = include_bytes!("../../assets/htmx.min.js");

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

    Router::new().route("/htmx.min.js", get(get_htmx))
}

/// Serde deserialization decorator to map empty Strings to None,
/// we use this in some cases to parse query parameters from htmx
/// requests. It ends up being simpler than fiddling around with
/// Javascript to not send query parameters for certain requests
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

pub fn api(mut engine_errors_rx: mpsc::Receiver<anyhow::Error>) -> Router<engine::Tx> {
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

    #[derive(Template)]
    #[template(path = "dmx-outputs.html")]
    struct DMXOutputsTemplate<'a> {
        devices: Vec<dmx::DeviceInfo>,
        err_message: Option<&'a str>,
    }

    async fn get_dmx_outputs(
        State(engine_tx): State<engine::Tx>,
        change_request: Query<ChangeDeviceRequest>,
    ) -> impl IntoResponse {
        let set_output_res = if let Some(device) = change_request.0.device {
            if device == "unselected" {
                // This is a reserved device name which lets us
                // know we should not be connected to any DMX
                // device
                engine_tx.set_dmx_output(None).await
            } else {
                engine_tx.set_dmx_output(Some(device)).await
            }
        } else {
            Ok(())
        };
        let list_outputs_res = engine_tx.list_dmx_outputs().await;

        let template = match (set_output_res, list_outputs_res) {
            (Ok(_), Ok(devices)) => DMXOutputsTemplate {
                devices,
                err_message: None,
            },
            (Err(e), Ok(devices)) => {
                error!("Failed to switch DMX output"; "error" => format!("{e}"));
                DMXOutputsTemplate {
                    devices,
                    err_message: Some("Failed to switch device."),
                }
            }
            (Ok(_), Err(e)) => {
                error!("Failed to list DMX outputs"; "error" => format!("{e}"));
                DMXOutputsTemplate {
                    devices: vec![],
                    err_message: Some("Switched device, but failed to reload the available devices. Please refresh the page."),
                }
            }
            (Err(left), Err(right)) => {
                error!("Failed to switch DMX output"; "error" => format!("{left}"));
                error!("Failed to list DMX outputs"; "error" => format!("{right}"));
                DMXOutputsTemplate {
                    devices: vec![],
                    err_message: Some("Failed to switch device and reload the available devices. Please refresh the page."),
                }
            }
        };
        HtmlTemplate(template).into_response()
    }

    #[derive(Deserialize)]
    struct ChangeGradientRequest {
        #[serde(default, deserialize_with = "empty_string_as_none")]
        gradient: Option<String>,
    }

    #[derive(Template)]
    #[template(path = "gradients.html")]
    struct GradientsTemplate<'a> {
        gradients: Vec<led::GradientInfo>,
        // We pre-process the selected gradient in the
        // Rust code because it isn't working in the
        // askama template for some reason
        selected_gradient: Option<led::GradientInfo>,
        err_message: Option<&'a str>,
    }

    async fn get_led_gradients(
        State(engine_tx): State<engine::Tx>,
        change_request: Query<ChangeGradientRequest>,
    ) -> impl IntoResponse {
        let set_gradient_res = if let Some(gradient) = change_request.0.gradient {
            engine_tx.set_current_gradient(gradient).await
        } else {
            Ok(())
        };
        let list_gradients_res = engine_tx.list_gradients().await;

        let template = match (set_gradient_res, list_gradients_res) {
            (Ok(_), Ok(gradients)) => {
                let selected_gradient = gradients.iter().find(|g| g.selected).map(|g| g.clone());
                GradientsTemplate {
                    gradients,
                    selected_gradient,
                    err_message: None,
                }
            }
            (Err(e), Ok(gradients)) => {
                let selected_gradient = gradients.iter().find(|g| g.selected).map(|g| g.clone());
                error!("Failed to switch gradient"; "error" => format!("{e}"));
                GradientsTemplate {
                    gradients,
                    selected_gradient,
                    err_message: Some("Failed to switch gradient."),
                }
            }
            (Ok(_), Err(e)) => {
                error!("Failed to list gradients"; "error" => format!("{e}"));
                GradientsTemplate {
                    gradients: vec![],
                    selected_gradient: None,
                    err_message: Some("Switched gradient, but failed to reload the available gradients. Please refresh the page."),
                }
            }
            (Err(left), Err(right)) => {
                error!("Failed to switch gradient"; "error" => format!("{left}"));
                error!("Failed to list gradients"; "error" => format!("{right}"));
                GradientsTemplate {
                    gradients: vec![],
                    selected_gradient: None,
                    err_message: Some("Failed to switch gradient and reload the available gradients. Please refresh the page."),
                }
            }
        };
        HtmlTemplate(template).into_response()
    }

    #[derive(Template)]
    #[template(path = "gradients-edited.html")]
    struct GradientsEditedTemplate<'a> {
        err_message: Option<&'a str>,
    }

    // Most of the gradient functions either succeed and result
    // in a refresh, or fail with some error. We write a helper
    // function to encapsulate this shared functionality
    fn gradients_edited_resp(result: anyhow::Result<()>, err_message: &str) -> Response<Body> {
        match result {
            Ok(_) => {
                let template = GradientsEditedTemplate { err_message: None };
                let mut resp: Response<Body> = HtmlTemplate(template).into_response();
                resp.headers_mut().insert(
                    "HX-Trigger-After-Settle",
                    HeaderValue::from_static("gradientsEdited"),
                );
                resp
            }
            Err(e) => {
                error!("Failed gradient editing operation"; "error" => format!("{e}"));
                HtmlTemplate(GradientsEditedTemplate {
                    err_message: Some(err_message),
                })
                .into_response()
            }
        }
    }

    // Some gradient functions all parse input in the same
    // way, so we define a helper for this as well
    fn parse_hx_prompt(headers: HeaderMap) -> Option<String> {
        match headers
            .get("HX-Prompt")
            .map(|h| h.to_str().unwrap_or_default())
        {
            Some("") | None => None,
            Some(n) => Some(n.to_string()),
        }
    }

    // And we also need to parse valid input data for
    // the gradients in some cases
    async fn parse_colour_index(
        engine_tx: &engine::Tx,
        gradient: String,
        index: usize,
    ) -> anyhow::Result<led::Gradient> {
        engine_tx
            .get_gradient(gradient)
            .and_then(|gradient| async {
                let len = gradient.colours.len();
                if index >= len || len == 0 {
                    Err(anyhow!("invalid colour index"))
                } else {
                    Ok(gradient)
                }
            })
            .await
    }

    async fn add_led_gradient(
        State(engine_tx): State<engine::Tx>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        let name = match parse_hx_prompt(headers) {
            None => {
                return HtmlTemplate(GradientsEditedTemplate {
                    err_message: Some("Gradient must have a name."),
                })
                .into_response();
            }
            Some(name) => name,
        };

        let operation = engine_tx
            .add_gradient(name, led::Gradient::new(), false)
            .await;
        gradients_edited_resp(operation, "Failed to add gradient.")
    }

    async fn delete_led_gradient(
        State(engine_tx): State<engine::Tx>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        let name = match parse_hx_prompt(headers) {
            None => {
                return HtmlTemplate(GradientsEditedTemplate {
                    err_message: Some("Gradient must have a name."),
                })
                .into_response();
            }
            Some(name) => name,
        };

        let operation = engine_tx.delete_gradient(name).await;
        gradients_edited_resp(operation, "Failed to delete gradient.")
    }

    async fn add_colour(
        State(engine_tx): State<engine::Tx>,
        Path(name): Path<String>,
    ) -> impl IntoResponse {
        let operation = engine_tx
            .get_gradient(name.clone())
            .and_then(|mut gradient| async {
                gradient.add_colour(led::BLACK);
                engine_tx.add_gradient(name, gradient, true).await
            })
            .await;
        gradients_edited_resp(operation, "Failed to add colour.")
    }

    async fn delete_colour(
        State(engine_tx): State<engine::Tx>,
        Path((name, index)): Path<(String, usize)>,
    ) -> impl IntoResponse {
        let operation = parse_colour_index(&engine_tx, name.clone(), index)
            .and_then(|mut gradient| async {
                gradient.delete_colour(index);
                engine_tx.add_gradient(name, gradient, true).await
            })
            .await;
        gradients_edited_resp(operation, "Failed to delete colour.")
    }

    #[derive(Deserialize)]
    struct ColourValue {
        value: String,
    }

    async fn edit_colour_value(
        State(engine_tx): State<engine::Tx>,
        Path((name, index)): Path<(String, usize)>,
        Form(cv): Form<ColourValue>,
    ) -> impl IntoResponse {
        let tx = &engine_tx;
        let operation = parse_colour_index(tx, name.clone(), index)
            .and_then(|gradient| async {
                led::Colour::from_str(&cv.value).map(|colour| (gradient, colour))
            })
            .and_then(|(mut gradient, colour)| async move {
                gradient.change_colour(index, colour);
                tx.add_gradient(name, gradient, true).await
            })
            .await;
        gradients_edited_resp(operation, "Failed to edit colour.")
    }

    #[derive(Deserialize)]
    struct ColourPosition {
        position: f32,
    }

    async fn edit_colour_position(
        State(engine_tx): State<engine::Tx>,
        Path((name, index)): Path<(String, usize)>,
        Form(cp): Form<ColourPosition>,
    ) -> impl IntoResponse {
        let operation = parse_colour_index(&engine_tx, name.clone(), index)
            .and_then(|mut gradient| async {
                if cp.position < 0.0 || cp.position > 1.0 {
                    Err(anyhow!("invalid colour position"))
                } else {
                    gradient.change_position(index, cp.position);
                    engine_tx.add_gradient(name, gradient, true).await
                }
            })
            .await;
        gradients_edited_resp(operation, "Failed to edit colour position.")
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
        .route("/led/dmx", get(get_dmx_outputs))
        .route("/led/gradients", get(get_led_gradients))
        .route("/led/gradients", post(add_led_gradient))
        .route("/led/gradients", delete(delete_led_gradient))
        .route("/led/gradients/:name/colours", post(add_colour))
        .route("/led/gradients/:name/colours/:index", delete(delete_colour))
        .route(
            "/led/gradients/:name/colours/:index/value",
            patch(edit_colour_value),
        )
        .route(
            "/led/gradients/:name/colours/:index/position",
            patch(edit_colour_position),
        )
        .route("/logs", get(get_logs))
}
