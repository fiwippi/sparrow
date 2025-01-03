use anyhow::anyhow; // TODO Remove anyhow
use cpal::traits::DeviceTrait;
use slog_scope::error;
use tokio::{
    sync::{mpsc, oneshot},
    task,
};

use super::audio;
use super::led;

type Ack<T> = oneshot::Sender<anyhow::Result<T>>; // TODO Remove anyhow

#[derive(Debug)]
pub enum Command {
    ListAudioInputs {
        tx: Ack<Vec<audio::DeviceInfo>>,
    },
    ListAudioOutputs {
        tx: Ack<Vec<audio::DeviceInfo>>,
    },
    SetAudioInput {
        device_name: String,
        tx: Ack<()>,
    },
    SetAudioOutput {
        device_name: String,
        tx: Ack<()>,
    },
    GetAudioStatus {
        tx: Ack<PlaybackStatus>,
    },
    ToggleAudioStatus {
        tx: Ack<()>,
    },
    ListGradients {
        tx: Ack<Vec<led::GradientInfo>>,
    },
    SetCurrentGradient {
        name: String,
        tx: Ack<()>,
    },
    GetGradient {
        name: String, // TODO &str
        tx: Ack<led::Gradient>,
    },
    AddGradient {
        name: String,
        gradient: led::Gradient,
        overwrite: bool,
        tx: Ack<()>,
    },
    DeleteGradient {
        name: String, // TODO &str
        tx: Ack<()>,
    },
    Shutdown {
        tx: Ack<()>,
    },
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

    pub async fn list_gradients(&self) -> anyhow::Result<Vec<led::GradientInfo>> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::ListGradients { tx }, rx);

        self.0.send(tx).await?;
        let gradients = rx.await??;
        Ok(gradients)
    }

    pub async fn set_current_gradient(&self, name: String) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::SetCurrentGradient { name, tx }, rx);

        self.0.send(tx).await?;
        rx.await??;
        Ok(())
    }

    pub async fn get_gradient(&self, name: String) -> anyhow::Result<led::Gradient> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::GetGradient { name, tx }, rx);

        self.0.send(tx).await?;
        let gradient = rx.await??;
        Ok(gradient)
    }

    pub async fn add_gradient(
        &self,
        name: String,
        gradient: led::Gradient,
        overwrite: bool,
    ) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (
            Command::AddGradient {
                name,
                gradient,
                overwrite,
                tx,
            },
            rx,
        );

        self.0.send(tx).await?;
        rx.await??;
        Ok(())
    }

    pub async fn delete_gradient(&self, name: String) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::DeleteGradient { name, tx }, rx);

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

const LATENCY_MS: f32 = 300.0; // TODO Make configurable via config

#[derive(Debug)]
pub enum PlaybackStatus {
    Playing,
    Paused,
}

// TODO Make piping on startup with the default devices configurable
pub struct Daemon {
    // Command/Error exchange
    cmd_rx: mpsc::Receiver<Command>,
    err_tx: mpsc::Sender<anyhow::Error>,

    // Audio
    input_device: String,
    output_device: String,
    input_handle: cpal::Device,
    output_handle: cpal::Device,
    play_audio: bool,

    // LEDs
    gradients: led::Gradients,
}

impl Daemon {
    pub fn new() -> anyhow::Result<(Self, Tx, mpsc::Receiver<anyhow::Error>)> {
        // To simplify configuration, we assume that the
        // user wants to use their default input devices
        // by default (as long as they exist)
        let (input_handle, output_handle) = audio::default_handles();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(32);
        let (err_tx, err_rx) = mpsc::channel::<anyhow::Error>(32);

        Ok((
            Self {
                cmd_rx,
                err_tx,
                input_device: input_handle.name()?,
                output_device: output_handle.name()?,
                input_handle,
                output_handle,
                play_audio: false,
                gradients: led::Gradients::new(),
            },
            Tx(cmd_tx),
            err_rx,
        ))
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
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
                                .and_then(|_| self.play_audio(&mut audio_pipe));

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
                                .and_then(|_| self.play_audio(&mut audio_pipe));

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
                                self.play_audio(&mut audio_pipe)
                            };

                            let _ = tx.send(resp);
                        }
                        ListGradients { tx } => {
                            let _ = tx.send(Ok(self.gradients.info()));
                        }
                        SetCurrentGradient { name, tx } => {
                            let _ = tx.send(self.gradients.set_current(&name));
                        }
                        GetGradient { name, tx } => {
                            let _ = tx.send(self.gradients.get(&name));
                        }
                        AddGradient {
                            name,
                            gradient,
                            overwrite,
                            tx,
                        } => {
                            let _ = tx.send(self.gradients.add(&name, gradient, overwrite));
                        }
                        DeleteGradient { name, tx } => {
                            let _ = tx.send(self.gradients.delete(&name));
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

    fn play_audio(&mut self, audio_pipe: &mut Option<audio::Pipe>) -> anyhow::Result<()> {
        audio::Pipe::new(
            &self.input_handle,
            &self.output_handle,
            self.err_tx.clone(),
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
            let _ = self.err_tx.send(pipe_err);

            Err(e)
        })
    }
}
