use anyhow::anyhow; // TODO Remove use of anyhow macro
use cpal::traits::DeviceTrait;
use slog_scope::{debug, error};
use tokio::{
    sync::{mpsc, oneshot},
    task,
};

// -- Commands

type Ack<T> = oneshot::Sender<anyhow::Result<T>>; // TODO Replace use of anyhow::Result with concrete errors

#[derive(Debug)]
pub enum Command {
    ListAudioInputs { tx: Ack<Vec<audio::DeviceInfo>> },
    ListAudioOutputs { tx: Ack<Vec<audio::DeviceInfo>> },
    SetAudioInput { device_name: String, tx: Ack<()> },
    SetAudioOutput { device_name: String, tx: Ack<()> },
    GetAudioStatus { tx: Ack<PlaybackStatus> },
    ToggleAudioStatus { tx: Ack<()> },
    Shutdown { tx: Ack<()> },
}

#[derive(Clone)]
pub struct Tx(pub(crate) mpsc::Sender<Command>);

impl Tx {
    pub async fn list_audio_inputs(&self) -> anyhow::Result<Vec<audio::DeviceInfo>> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::ListAudioInputs { tx }, rx);

        self.0.send(tx).await?;
        let names = rx.await??;
        Ok(names)
    }

    pub async fn list_audio_outputs(&self) -> anyhow::Result<Vec<audio::DeviceInfo>> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::ListAudioOutputs { tx }, rx);

        self.0.send(tx).await?;
        let names = rx.await??;
        Ok(names)
    }

    pub async fn set_audio_input(&self, device_name: String) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::SetAudioInput { device_name, tx }, rx);

        self.0.send(tx).await?;
        rx.await??;
        Ok(())
    }

    pub async fn set_audio_output(&self, device_name: String) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::SetAudioOutput { device_name, tx }, rx);

        self.0.send(tx).await?;
        rx.await??;
        Ok(())
    }

    pub async fn get_audio_status(&self) -> anyhow::Result<PlaybackStatus> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::GetAudioStatus { tx }, rx);

        self.0.send(tx).await?;
        let status = rx.await??;
        Ok(status)
    }

    pub async fn toggle_audio_status(&self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::ToggleAudioStatus { tx }, rx);

        self.0.send(tx).await?;
        rx.await??;
        Ok(())
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::Shutdown { tx }, rx);

        self.0.send(tx).await?;
        rx.await?
    }
}

// -- Daemon

// TODO Make latency configurable via the config
const LATENCY_MS: f32 = 300.0;

#[derive(Debug)]
pub enum PlaybackStatus {
    Playing,
    Paused,
}

// TODO Make piping on startup a configurable option
pub struct Daemon {
    // Commands
    pub tx: Tx,
    cmd_rx: mpsc::Receiver<Command>,

    // Audio
    input_device: String,
    output_device: String,
    input_handle: cpal::Device,
    output_handle: cpal::Device,
    play_audio: bool,
}

impl Daemon {
    pub fn new() -> anyhow::Result<Self> {
        // To simplify configuration, we assume that the
        // user wants to use their default input devices
        // by default (as long as they exist)
        let (input_handle, output_handle) = audio::default_handles();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(32);

        Ok(Self {
            tx: Tx(cmd_tx),
            cmd_rx,
            input_device: input_handle.name()?,
            output_device: output_handle.name()?,
            input_handle,
            output_handle,
            play_audio: false,
        })
    }

    pub async fn run(mut self, errors_tx: mpsc::Sender<anyhow::Error>) -> anyhow::Result<()> {
        // Running the daemon involves two tasks:
        //   1. Listen for commands from the HTTP API
        //   2. Manage the input-to-output audio streaming
        //   3. Piping the calculated colour output via DMX

        let local_set = task::LocalSet::new();
        local_set
            .run_until(local_set.spawn_local(async move {
                // The pipe is !Send, so we make sure to only
                // instantiate it within the LocalSet, which
                // supports running !Send futures
                let mut audio_pipe: Option<audio::Pipe> = None;

                while let Some(cmd) = self.cmd_rx.recv().await {
                    use Command::*;

                    debug!("Command"; "name" => format!("{cmd:?}"));

                    match cmd {
                        ListAudioInputs { tx } => {
                            let name = Some(self.input_device.as_str());
                            let _ = tx.send(audio::input_device_info(name));
                        }
                        ListAudioOutputs { tx } => {
                            let name = Some(self.output_device.as_str());
                            let _ = tx.send(audio::output_device_info(name));
                        }
                        SetAudioInput { device_name, tx } => {
                            let resp = audio::input_device_info(None)
                                .and_then(|devices| {
                                    devices
                                        .into_iter()
                                        .find(|d| d.name == device_name)
                                        .ok_or(anyhow!("device not found: {}", device_name))
                                })
                                .and_then(|d| {
                                    self.input_device = d.name;
                                    self.input_handle = d.handle;
                                    Ok(())
                                })
                                .and_then(|_| self.play_audio(&mut audio_pipe, errors_tx.clone()));

                            let _ = tx.send(resp);
                        }
                        SetAudioOutput { device_name, tx } => {
                            let resp = audio::output_device_info(None)
                                .and_then(|devices| {
                                    devices
                                        .into_iter()
                                        .find(|d| d.name == device_name)
                                        .ok_or(anyhow!("device not found: {}", device_name))
                                })
                                .and_then(|d| {
                                    self.output_device = d.name;
                                    self.output_handle = d.handle;
                                    Ok(())
                                })
                                .and_then(|_| self.play_audio(&mut audio_pipe, errors_tx.clone()));

                            let _ = tx.send(resp);
                        }
                        GetAudioStatus { tx } => {
                            let status = if self.play_audio {
                                PlaybackStatus::Playing
                            } else {
                                PlaybackStatus::Paused
                            };

                            let _ = tx.send(Ok(status));
                        }
                        ToggleAudioStatus { tx } => {
                            let resp = if self.play_audio {
                                if let Some(pipe) = audio_pipe.take() {
                                    drop(pipe);
                                }

                                self.play_audio = false;
                                Ok(())
                            } else {
                                self.play_audio = true;
                                self.play_audio(&mut audio_pipe, errors_tx.clone())
                            };

                            let _ = tx.send(resp);
                        }
                        Shutdown { tx } => {
                            self.cmd_rx.close();
                            let _ = tx.send(Ok(()));
                            return;
                        }
                    }
                }
            }))
            .await?;

        Ok(())
    }

    fn play_audio(
        &mut self,
        audio_pipe: &mut Option<audio::Pipe>,
        errors_tx: mpsc::Sender<anyhow::Error>,
    ) -> anyhow::Result<()> {
        audio::Pipe::new(
            &self.input_handle,
            &self.output_handle,
            errors_tx.clone(),
            LATENCY_MS,
        )
        .and_then(|p| {
            if let Some(pipe) = audio_pipe.take() {
                drop(pipe);
            }
            *audio_pipe = Some(p);
            if self.play_audio {
                audio_pipe.as_ref().unwrap().play()
            } else {
                Ok(())
            }
        })
        .or_else(|e| {
            error!("Failed to start pipe"; "error" => format!("{:?}", e));
            if let Some(pipe) = audio_pipe.take() {
                drop(pipe);
            }

            // If we failed to play audio, let the user know
            // that the playback failed. At this point we
            // forcefully pause the stream and wait for user
            // input to continue playback
            self.play_audio = false;
            let pipe_err = anyhow!(format!("{e:?}")); // TODO Remove, needed until concrete errors
            let _ = errors_tx.send(pipe_err);

            Err(e)
        })
    }
}

pub mod audio {
    use std::fmt;

    use cpal::{
        self,
        traits::{DeviceTrait, HostTrait, StreamTrait},
    };
    use ringbuf::{
        traits::{Consumer, Producer, Split},
        HeapRb,
    };
    use slog_scope::error;
    use tokio::sync::mpsc;

    pub fn default_handles() -> (cpal::Device, cpal::Device) {
        let host = cpal::default_host();

        // Sparrow doesn't handle cases where the default
        // devices are unavailable (that's why we're ok to
        // call unwrap)
        let input = host.default_input_device().unwrap();
        let output = host.default_output_device().unwrap();

        (input, output)
    }

    pub struct DeviceInfo {
        pub(super) handle: cpal::Device,
        pub name: String,
        pub selected: bool,
    }

    impl fmt::Display for DeviceInfo {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "{}", self.name)
        }
    }

    impl fmt::Debug for DeviceInfo {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.debug_struct("DeviceInfo")
                .field("name", &self.name)
                .field("selected", &self.selected)
                .finish()
        }
    }

    pub fn input_device_info(
        current_input_device: Option<&str>,
    ) -> anyhow::Result<Vec<DeviceInfo>> {
        let mut info: Vec<DeviceInfo> = vec![];

        let available_hosts = cpal::available_hosts();
        for host_id in available_hosts {
            let host = cpal::host_from_id(host_id)?;

            for handle in host.input_devices()? {
                let device_name = handle.name()?;
                info.push(DeviceInfo {
                    selected: current_input_device.is_some_and(|n| n == device_name),
                    name: device_name,
                    handle,
                })
            }
        }

        Ok(info)
    }

    pub fn output_device_info(
        current_output_device: Option<&str>,
    ) -> anyhow::Result<Vec<DeviceInfo>> {
        let mut info: Vec<DeviceInfo> = vec![];

        let available_hosts = cpal::available_hosts();
        for host_id in available_hosts {
            let host = cpal::host_from_id(host_id)?;

            for handle in host.output_devices()? {
                let device_name = handle.name()?;
                info.push(DeviceInfo {
                    selected: current_output_device.is_some_and(|n| n == device_name),
                    name: device_name,
                    handle,
                })
            }
        }

        Ok(info)
    }

    pub struct Pipe {
        input_stream: cpal::Stream,
        output_stream: cpal::Stream,
    }

    impl Pipe {
        pub fn new(
            input_handle: &cpal::Device,
            output_handle: &cpal::Device,
            errors: mpsc::Sender<anyhow::Error>,
            latency_ms: f32,
        ) -> anyhow::Result<Self> {
            // We use the same configurations between the input
            // and output stream to simplify the logic
            let config: cpal::StreamConfig = input_handle.default_input_config()?.into();

            // Create a delay in case the input and output
            // devices aren't synced
            let latency_frames = (latency_ms / 1_000.0) * config.sample_rate.0 as f32;
            let latency_samples = latency_frames as usize * config.channels as usize;

            let ring = HeapRb::<f32>::new(latency_samples * 2);
            let (mut producer, mut consumer) = ring.split();

            // We simulate the delay by placing null samples
            // inside the buffer
            for _ in 0..latency_samples {
                // The buffer is twice the size of the latency we will
                // fill in, so this function shouldn't fail
                producer.try_push(0.0).unwrap();
            }

            let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
                producer.push_slice(data);
            };
            let output_data_fn = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for sample in data {
                    *sample = consumer.try_pop().unwrap_or_else(|| 0.0);
                }
            };

            let input_err_events = errors.clone();
            let input_err_fn = move |err: cpal::StreamError| {
                if let Err(e) = input_err_events.blocking_send(err.into()) {
                    error!("Failed to send event"; "event" => format!("{:?}", e))
                }
            };
            let output_err_events = errors.clone();
            let output_err_fn = move |err: cpal::StreamError| {
                if let Err(e) = output_err_events.blocking_send(err.into()) {
                    error!("Failed to send event"; "event" => format!("{:?}", e))
                }
            };

            Ok(Self {
                input_stream: input_handle.build_input_stream(
                    &config,
                    input_data_fn,
                    input_err_fn,
                    None,
                )?,
                output_stream: output_handle.build_output_stream(
                    &config,
                    output_data_fn,
                    output_err_fn,
                    None,
                )?,
            })
        }

        pub fn play(&self) -> anyhow::Result<()> {
            self.input_stream.play()?;
            self.output_stream.play()?;

            Ok(())
        }
    }
}
