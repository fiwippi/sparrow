use std::fmt;

use anyhow::anyhow; // TODO Remove use of this
use cpal::traits::{DeviceTrait, HostTrait};
use tokio::sync::{mpsc, oneshot};

// -- Commands

type Ack<T> = oneshot::Sender<anyhow::Result<T>>; // TODO Replace use of anyhow::Result with concrete errors

#[derive(Debug)]
pub enum Command {
    ListAudioInputs { tx: Ack<Vec<DeviceInfo>> },
    ListAudioOutputs { tx: Ack<Vec<DeviceInfo>> },
    SetAudioInput { device_name: String, tx: Ack<()> },
    SetAudioOutput { device_name: String, tx: Ack<()> },
    Shutdown { tx: Ack<()> },
}

#[derive(Clone)]
pub struct Tx(pub(crate) mpsc::Sender<Command>);

// TODO Add timeouts if daemon isn't replying
impl Tx {
    pub async fn list_audio_inputs(&self) -> anyhow::Result<Vec<DeviceInfo>> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::ListAudioInputs { tx }, rx);

        self.0.send(tx).await?;
        let names = rx.await??;
        Ok(names)
    }

    pub async fn list_audio_outputs(&self) -> anyhow::Result<Vec<DeviceInfo>> {
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

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let (tx, rx) = (Command::Shutdown { tx }, rx);

        self.0.send(tx).await?;
        rx.await?
    }
}

// -- Daemon

pub type Handle = tokio::task::JoinHandle<()>;

pub struct Daemon {
    input_device: Option<String>,
    output_device: Option<String>,
}

impl Daemon {
    pub fn new() -> Self {
        // To simplify configuration, we assume that the
        // user wants to use their default input devices
        // by default (as long as they exist)
        let host = cpal::default_host();
        Self {
            input_device: host.default_input_device().and_then(|d| d.name().ok()),
            output_device: host.default_output_device().and_then(|d| d.name().ok()),
        }
    }

    pub fn run(mut self) -> (Handle, Tx) {
        // Running the daemon involves two tasks:
        //   1. Listen for commands from the HTTP API
        //   2. Manage the input-to-output audio streaming
        //   3. Piping the calculated colour output via DMX

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);

        let daemon_handle = tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                use Command::*;

                match cmd {
                    ListAudioInputs { tx } => {
                        let name = self.input_device.as_deref();
                        let _ = tx.send(input_device_info(name));
                    }
                    ListAudioOutputs { tx } => {
                        let name = self.output_device.as_deref();
                        let _ = tx.send(output_device_info(name));
                    }
                    SetAudioInput { device_name, tx } => {
                        let resp = input_device_info(None)
                            .and_then(|devices| {
                                devices
                                    .into_iter()
                                    .find(|d| d.name == device_name)
                                    .ok_or(anyhow!("device not found: {}", device_name))
                            })
                            .and_then(|d| {
                                self.input_device = Some(d.name);
                                Ok(())
                            });
                        let _ = tx.send(resp);
                    }
                    SetAudioOutput { device_name, tx } => {
                        let resp = output_device_info(None)
                            .and_then(|devices| {
                                devices
                                    .into_iter()
                                    .find(|d| d.name == device_name)
                                    .ok_or(anyhow!("device not found: {}", device_name))
                            })
                            .and_then(|d| {
                                self.output_device = Some(d.name);
                                Ok(())
                            });
                        let _ = tx.send(resp);
                    }
                    Shutdown { tx } => {
                        cmd_rx.close();
                        let _ = tx.send(Ok(()));
                        return;
                    }
                }
            }
        });

        (daemon_handle, Tx(cmd_tx))
    }
}

// -- CPAL

#[derive(Debug)]
pub struct DeviceInfo {
    pub name: String,
    pub selected: bool,
}

impl fmt::Display for DeviceInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

fn input_device_info(current_input_device: Option<&str>) -> anyhow::Result<Vec<DeviceInfo>> {
    let mut info: Vec<DeviceInfo> = vec![];

    let available_hosts = cpal::available_hosts();
    for host_id in available_hosts {
        let host = cpal::host_from_id(host_id)?;

        for handle in host.input_devices()? {
            let device_name = handle.name()?;
            info.push(DeviceInfo {
                selected: current_input_device.is_some_and(|n| n == device_name),
                name: device_name,
            })
        }
    }

    Ok(info)
}

fn output_device_info(current_output_device: Option<&str>) -> anyhow::Result<Vec<DeviceInfo>> {
    let mut info: Vec<DeviceInfo> = vec![];

    let available_hosts = cpal::available_hosts();
    for host_id in available_hosts {
        let host = cpal::host_from_id(host_id)?;

        for handle in host.output_devices()? {
            let device_name = handle.name()?;
            info.push(DeviceInfo {
                selected: current_output_device.is_some_and(|n| n == device_name),
                name: device_name,
            })
        }
    }

    Ok(info)
}
