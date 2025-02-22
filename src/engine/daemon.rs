use super::{audio, dmx, led};

use anyhow::anyhow;
use cpal::traits::DeviceTrait;
use slog_scope::{error, info};
use tokio::{
    sync::{mpsc, oneshot},
    task,
};

type Ack<T> = oneshot::Sender<anyhow::Result<T>>;

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
        device_name: Option<String>,
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
        name: String,
        tx: Ack<led::Gradient>,
    },
    AddGradient {
        name: String,
        gradient: led::Gradient,
        overwrite: bool,
        tx: Ack<()>,
    },
    DeleteGradient {
        name: String,
        tx: Ack<()>,
    },
    ListDMXOutputs {
        tx: Ack<Vec<dmx::DeviceInfo>>,
    },
    SetDMXOutput {
        device_name: Option<String>,
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
        let inputs = rx.await??;
        Ok(inputs)
    }

    pub async fn list_audio_outputs(&self) -> anyhow::Result<Vec<audio::DeviceInfo>> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::ListAudioOutputs { tx }, rx);

        self.0.send(tx).await?;
        let outputs = rx.await??;
        Ok(outputs)
    }

    pub async fn set_audio_input(&self, device_name: String) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::SetAudioInput { device_name, tx }, rx);

        self.0.send(tx).await?;
        rx.await??;
        Ok(())
    }

    pub async fn set_audio_output(&self, device_name: Option<String>) -> anyhow::Result<()> {
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

    pub async fn list_dmx_outputs(&self) -> anyhow::Result<Vec<dmx::DeviceInfo>> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::ListDMXOutputs { tx }, rx);

        self.0.send(tx).await?;
        let outputs = rx.await??;
        Ok(outputs)
    }

    pub async fn set_dmx_output(&self, device_name: Option<String>) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::SetDMXOutput { device_name, tx }, rx);

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

#[derive(Debug)]
pub enum PlaybackStatus {
    Playing,
    Paused,
}

pub struct Daemon {
    // Command exchange
    cmd_rx: mpsc::Receiver<Command>,

    // Audio
    input_device: String,
    input_handle: cpal::Device,
    output_device: Option<String>,
    output_handle: Option<cpal::Device>,
    play_audio: bool,

    // LEDs
    gradients: led::Gradients,
    dmx_port: Option<String>, // The name of the port, e.g. /dev/tty0
}

impl Daemon {
    pub fn new() -> anyhow::Result<(Self, Tx)> {
        // To simplify configuration, we assume that the
        // user wants to use their default input devices
        // by default (as long as they exist)
        let (input_handle, output_handle) = audio::default_handles();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(32);

        Ok((
            Self {
                cmd_rx,
                input_device: input_handle.name()?,
                output_device: Some(output_handle.name()?),
                input_handle,
                output_handle: Some(output_handle),
                play_audio: false,
                gradients: led::Gradients::default(),
                dmx_port: None,
            },
            Tx(cmd_tx),
        ))
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        // Running the daemon involves three tasks:
        //   1. Listen for commands from the HTTP API
        //   2. Manage the input-to-output audio streaming
        //   3. Manage the connection to a DMX device

        let local_set = task::LocalSet::new();
        local_set
            .run_until(local_set.spawn_local(async move {
                // The pipe is !Send, so we make sure to only
                // instantiate it within the LocalSet, which
                // supports running !Send futures
                let mut audio_pipe: Option<audio::Pipe> = None;
                let mut dmx_agent: Option<dmx::SerialAgent> = None;

                while let Some(cmd) = self.cmd_rx.recv().await {
                    use Command::*;

                    match cmd {
                        ListAudioInputs { tx } => {
                            let name = Some(self.input_device.as_str());
                            let _ = tx.send(audio::input_device_info(name));
                        }
                        ListAudioOutputs { tx } => {
                            let name = self.output_device.as_deref();
                            let device_info = audio::output_device_info(name);

                            // In some cases, an audio device may be switched or
                            // replaced, like the internal speaker audio becoming
                            // external headphones audio. We can detect this if
                            // we have a defined output device, but no stream
                            // is marked as selected
                            let device_selected = device_info
                                .as_ref()
                                .is_ok_and(|i| i.iter().map(|d| d.selected).any(|s| s == true));
                            if name.is_some() && !device_selected {
                                info!("Tearing down unselected device"; "name" => name);
                                self.output_device = None;
                                self.output_handle = None;
                                self.cleanup_audio(&mut audio_pipe, &mut dmx_agent).await;
                            }

                            let _ = tx.send(device_info);
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
                                .and_then(|_| {
                                    if self.play_audio {
                                        self.play_audio(&mut audio_pipe, &dmx_agent)
                                    } else {
                                        Ok(())
                                    }
                                });

                            let _ = tx.send(resp);
                        }
                        SetAudioOutput { device_name, tx } => {
                            let resp = if let Some(name) = device_name {
                                audio::output_device_info(None)
                                    .and_then(|devices| {
                                        devices
                                            .into_iter()
                                            .find(|d| d.name == name)
                                            .ok_or(anyhow!("device not found: {}", name))
                                    })
                                    .and_then(|d| {
                                        self.output_device = Some(d.name);
                                        self.output_handle = Some(d.handle);
                                        Ok(())
                                    })
                            } else {
                                self.output_device = None;
                                self.output_handle = None;
                                Ok(())
                            }
                            .and_then(|_| {
                                if self.play_audio {
                                    self.play_audio(&mut audio_pipe, &dmx_agent)
                                } else {
                                    Ok(())
                                }
                            });

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
                                self.play_audio = false;
                                if let Some(pipe) = audio_pipe.take() {
                                    drop(pipe);
                                }
                                // When pausing, we don't actually want to
                                // destroy the connection to the DMX device
                                // so we don't stop the agent like we do in
                                // .cleanup_audio()
                                if let Some(agent) = &dmx_agent {
                                    let _ = agent.set_colour(led::BLACK);
                                }
                                Ok(())
                            } else {
                                self.play_audio = true;
                                self.play_audio(&mut audio_pipe, &dmx_agent)
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
                        ListDMXOutputs { tx } => {
                            let _ = tx.send(dmx::available_ports(self.dmx_port.as_deref()));
                        }
                        SetDMXOutput { device_name, tx } => {
                            self.cleanup_audio(&mut audio_pipe, &mut dmx_agent).await;

                            self.dmx_port = device_name;
                            if let Some(port) = &self.dmx_port {
                                match dmx::SerialAgent::open(port) {
                                    Ok(agent) => dmx_agent = Some(agent),
                                    Err(e) => {
                                        let _ = tx.send(Err(e));
                                        continue;
                                    }
                                }
                            };
                            let resp = if self.play_audio {
                                self.play_audio(&mut audio_pipe, &dmx_agent)
                            } else {
                                Ok(())
                            };

                            let _ = tx.send(resp);
                        }
                        Shutdown { tx } => {
                            self.cmd_rx.close();
                            self.cleanup_audio(&mut audio_pipe, &mut dmx_agent).await;
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
        dmx_agent: &Option<dmx::SerialAgent>,
    ) -> anyhow::Result<()> {
        let gradient = self
            .gradients
            .get_current()
            .unwrap_or_else(|| led::Gradient::new());
        let output_handle = if let Some(handle) = &self.output_handle {
            Some(handle)
        } else {
            None
        };

        audio::Pipe::new(
            &self.input_handle,
            output_handle,
            300.0,
            dmx_agent.clone(),
            gradient,
        )
        .and_then(|p| {
            if let Some(pipe) = audio_pipe.take() {
                drop(pipe);
            };
            *audio_pipe = Some(p);
            audio_pipe.as_ref().unwrap().play()
        })
        .or_else(|e| {
            error!("Failed to start pipe"; "error" => format!("{:?}", e));

            // If we failed to play audio, let the user know that the
            // playback failed. At this point we forcefully pause the
            // stream and wait for user input to continue playback
            if let Some(pipe) = audio_pipe.take() {
                drop(pipe);
            }
            self.play_audio = false;

            Err(e)
        })
    }

    async fn cleanup_audio(
        &mut self,
        audio_pipe: &mut Option<audio::Pipe>,
        dmx_agent: &mut Option<dmx::SerialAgent>,
    ) {
        self.play_audio = false;
        if let Some(pipe) = audio_pipe.take() {
            drop(pipe);
        }
        if let Some(agent) = dmx_agent.take() {
            let _ = agent.set_colour(led::BLACK);
            agent.stop().await;
        }
    }
}
