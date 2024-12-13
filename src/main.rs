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

mod ui {
    use std::time::Instant;

    use crate::audio;

    use askama::Template;
    use axum::{
        body::{Body, Bytes},
        extract::{Request, State},
        middleware::{self, Next},
        response::{Html, IntoResponse, Response},
        routing::get,
        Router,
    };
    use http::{header, status::StatusCode};
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

        // After the server is run we don't want it to be
        // run again, so we ensure it's moved due to being
        // passed as mut via the self parameter
        #[allow(unused_mut)]
        pub async fn run(mut self) -> anyhow::Result<()> {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:4181").await?;
            info!("Listening on {:?}...", listener.local_addr().unwrap());
            axum::serve(listener, self.app).await?;

            Ok(())
        }
    }

    // -- Middleware

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

    // -- Templates

    struct HtmlTemplate<T>(T);

    // This trait implementation allows us to convert the Askama
    // HTML templates into valid HTML which our Axum router can
    // serve
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
        #[template(path = "device-names.html")]
        struct DeviceNamesTemplate {
            names: Vec<String>,
        }

        async fn get_input_devices(State(daemon_tx): State<audio::Tx>) -> impl IntoResponse {
            match daemon_tx.enumerate_input_devices().await {
                Ok(names) => HtmlTemplate(DeviceNamesTemplate { names }).into_response(),
                Err(e) => {
                    error!("Failed to enumerate input devices"; "error" => format!("{e}"));
                    (StatusCode::INTERNAL_SERVER_ERROR, "").into_response()
                }
            }
        }

        async fn get_output_devices(State(daemon_tx): State<audio::Tx>) -> impl IntoResponse {
            match daemon_tx.enumerate_output_devices().await {
                Ok(names) => HtmlTemplate(DeviceNamesTemplate { names }).into_response(),
                Err(e) => {
                    error!("Failed to enumerate output devices"; "error" => format!("{e}"));
                    (StatusCode::INTERNAL_SERVER_ERROR, "").into_response()
                }
            }
        }

        Router::new()
            .route("/devices/input", get(get_input_devices))
            .route("/devices/output", get(get_output_devices))
    }
}

mod audio {
    use cpal::traits::{DeviceTrait, HostTrait};
    use tokio::sync::{mpsc, oneshot};

    // -- Commands

    type Ack<T> = oneshot::Sender<anyhow::Result<T>>; // TODO Replace use of anyhow::Result with concrete errors

    #[derive(Debug)]
    pub enum Command {
        EnumerateInputDevices { tx: Ack<Vec<String>> },
        EnumerateOutputDevices { tx: Ack<Vec<String>> },
        Shutdown { tx: Ack<()> },
    }

    // TODO Remove use of Tx?
    #[derive(Clone)]
    pub struct Tx(pub(crate) mpsc::Sender<Command>);

    impl Tx {
        // TODO Add timeouts if daemon isn't replying
        pub async fn enumerate_input_devices(&self) -> anyhow::Result<Vec<String>> {
            let (tx, rx) = oneshot::channel();
            let (enum_input, enum_input_rx) = (Command::EnumerateInputDevices { tx }, rx);

            self.0.send(enum_input).await?;
            let names = enum_input_rx.await??;
            Ok(names)
        }

        pub async fn enumerate_output_devices(&self) -> anyhow::Result<Vec<String>> {
            let (tx, rx) = oneshot::channel();
            let (enum_output, enum_output_rx) = (Command::EnumerateOutputDevices { tx }, rx);

            self.0.send(enum_output).await?;
            let names = enum_output_rx.await??;
            Ok(names)
        }

        pub async fn shutdown(&self) -> anyhow::Result<()> {
            let (tx, rx) = oneshot::channel();
            let (shutdown_tx, shutdown_rx) = (Command::Shutdown { tx }, rx);

            self.0.send(shutdown_tx).await?;
            shutdown_rx.await?
        }
    }

    // -- Daemon

    pub type Handle = tokio::task::JoinHandle<()>;

    pub struct Daemon {}

    impl Daemon {
        pub fn new() -> Self {
            Self {}
        }

        // After the daemon is run we don't want it to be
        // accessible so we ensure it's moved due to being
        // passed as mut via the self parameter
        #[allow(unused_mut)]
        pub fn run(mut self) -> (Handle, Tx) {
            let (tx, mut rx) = mpsc::channel::<Command>(32);

            let daemon_handle = tokio::spawn(async move {
                while let Some(cmd) = rx.recv().await {
                    use Command::*;

                    match cmd {
                        EnumerateInputDevices { tx } => {
                            let resp = match enumerate_devices() {
                                Ok((input_devices, _)) => Ok(input_devices
                                    .into_iter()
                                    .map(|d| d.name)
                                    .collect::<Vec<String>>()),
                                Err(e) => Err(e),
                            };

                            let _ = tx.send(resp);
                        }
                        EnumerateOutputDevices { tx } => {
                            let resp = match enumerate_devices() {
                                Ok((_, output_devices)) => Ok(output_devices
                                    .into_iter()
                                    .map(|d| d.name)
                                    .collect::<Vec<String>>()),
                                Err(e) => Err(e),
                            };

                            let _ = tx.send(resp);
                        }
                        Shutdown { tx } => {
                            rx.close();
                            let _ = tx.send(Ok(()));
                            return;
                        }
                    }
                }
            });

            (daemon_handle, Tx(tx))
        }
    }

    // -- CPAL

    // TODO Switch to using marker types for marking if
    //      a device is input or output, so that we can
    //      specify `Device<Input>` and `Device<Output>`
    struct Device {
        handle: cpal::Device,
        name: String,
    }

    fn enumerate_devices() -> anyhow::Result<(Vec<Device>, Vec<Device>)> {
        let mut input_devices: Vec<Device> = vec![];
        let mut output_devices: Vec<Device> = vec![];

        let available_hosts = cpal::available_hosts();
        for host_id in available_hosts {
            let host = cpal::host_from_id(host_id)?;

            for device in host.input_devices()? {
                let name = device.name()?;
                input_devices.push(Device {
                    handle: device,
                    name,
                })
            }
            for device in host.output_devices()? {
                let name = device.name()?;
                output_devices.push(Device {
                    handle: device,
                    name,
                })
            }
        }

        Ok((input_devices, output_devices))
    }
}
