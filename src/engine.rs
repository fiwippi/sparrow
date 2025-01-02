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

    // LEDs
    gradients: led::Gradients,
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
            gradients: led::Gradients::new(),
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

pub mod led {
    use std::collections::HashMap;

    use anyhow::anyhow;
    use palette::{oklch::Oklch, Mix};

    #[derive(Debug, Clone)]
    pub struct Gradient {
        // Colours are sorted by position, which
        // is clamped between 0.0 and 1.0
        pub colours: Vec<(Oklch, f32)>,
    }

    impl Gradient {
        pub fn new() -> Self {
            Self { colours: vec![] }
        }

        pub fn add_colour(&mut self, colour: Oklch) {
            self.colours.push((colour, 1.0))
        }

        pub fn delete_colour(&mut self, index: usize) -> anyhow::Result<()> {
            if index >= self.colours.len() {
                Err(anyhow!("index out of range"))
            } else {
                self.colours.remove(index);
                Ok(())
            }
        }

        pub fn edit_colour_position(&mut self, index: usize, position: f32) -> anyhow::Result<()> {
            if index >= self.colours.len() {
                Err(anyhow!("index out of range"))
            } else if position < 0.0 || position > 1.0 {
                Err(anyhow!("position out of range"))
            } else {
                self.colours[index].1 = position;
                self.colours.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
                Ok(())
            }
        }

        pub fn edit_colour_value(&mut self, index: usize, value: Oklch) -> anyhow::Result<()> {
            if index >= self.colours.len() {
                Err(anyhow!("index out of range"))
            } else {
                self.colours[index].0 = value;
                Ok(())
            }
        }

        pub fn interpolate(&self, position: f32) -> Oklch {
            if self.colours.len() == 0 {
                return Oklch::new(0.0, 0.0, 0.0);
            } else if self.colours.len() == 1 {
                return self.colours[0].0;
            }

            for i in 0..self.colours.len() - 1 {
                let left = self.colours[i];
                let right = self.colours[i + 1];
                if left.1 <= position && position <= right.1 {
                    let factor = (position - left.1) / (right.1 - left.1);
                    return left.0.mix(right.0, factor);
                }
            }

            // If we haven't reached the position yet, our colour
            // is either before the first point or after the last
            // one
            if position < self.colours[0].1 {
                self.colours[0].0
            } else {
                self.colours[self.colours.len() - 1].0
            }
        }

        pub fn bar(&self, points: usize) -> Vec<Oklch> {
            let mut colours = Vec::<Oklch>::new();

            let interval = 1.0 / points as f32;
            let mut position = 0.0;
            while position < 1.0 {
                colours.push(self.interpolate(position));
                position += interval;
            }
            colours.push(self.interpolate(1.0));

            colours
        }
    }

    #[derive(Debug, Clone)]
    pub struct GradientInfo {
        pub name: String,
        pub selected: bool,
        pub data: Gradient,
    }

    pub struct Gradients {
        current: Option<String>,
        gradients: HashMap<String, Gradient>,
    }

    impl Gradients {
        pub fn new() -> Self {
            Self {
                current: None,
                gradients: HashMap::new(),
            }
        }

        pub fn info(&self) -> Vec<GradientInfo> {
            let mut info: Vec<GradientInfo> = vec![];

            for (name, gradient) in self.gradients.iter() {
                let name = name.clone();
                info.push(GradientInfo {
                    selected: self.current.as_ref().is_some_and(|n| *n == name),
                    data: gradient.clone(),
                    name,
                })
            }

            info.sort_by(|a, b| a.name.cmp(&b.name));
            info
        }

        pub fn set_current(&mut self, name: &str) -> anyhow::Result<()> {
            if !self.gradients.contains_key(name) {
                return Err(anyhow!("gradient does not exist"));
            }
            self.current = Some(name.to_string());

            Ok(())
        }

        pub fn get(&self, name: &str) -> anyhow::Result<Gradient> {
            if let Some(gradient) = self.gradients.get(name) {
                Ok(gradient.clone())
            } else {
                Err(anyhow!("gradient does not exist"))
            }
        }

        pub fn add(
            &mut self,
            name: &str,
            gradient: Gradient,
            overwrite: bool,
        ) -> anyhow::Result<()> {
            if !overwrite && self.gradients.contains_key(name) {
                return Err(anyhow!("gradient already exists"));
            }

            self.gradients.insert(name.to_string(), gradient);
            self.current = Some(name.to_string());

            Ok(())
        }

        pub fn delete(&mut self, name: &str) -> anyhow::Result<()> {
            self.gradients.remove(name);
            if self.current.as_ref().is_some_and(|n| n == name) {
                self.current = None;
            }

            Ok(())
        }
    }
}

pub mod audio {
    use std::{fmt, sync::Mutex};

    use cpal::{
        self,
        traits::{DeviceTrait, HostTrait, StreamTrait},
    };
    use num_complex::Complex;
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

            let input_buffer = Mutex::new(Vec::<f32>::new());
            let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples_written = producer.push_slice(data);
                if samples_written > 0 {
                    let mut buf = input_buffer.lock().unwrap();
                    buf.extend_from_slice(&data[..samples_written]);
                    if buf.len() < 8096 {
                        // TODO Make configurable
                        return;
                    }

                    // Audio is interleaved, LRLRLR, so to mix
                    // down the samples into mono we do L+R/2
                    let mut mono: Vec<Complex<f32>> = buf
                        .chunks_exact(2)
                        .map(|chunk| (chunk[0] + chunk[1]) / 2.0)
                        .map(|re| Complex::new(re, 0.0))
                        .collect();

                    fourier::create_fft_f32(mono.len()).fft_in_place(&mut mono);

                    // We only consider the first half of
                    // the samples because the FFT is mirrored
                    let magnitudes: Vec<f32> =
                        mono[..mono.len() / 2].iter().map(|c| c.norm()).collect();

                    let index = {
                        let mut max = 0.0;
                        let mut index = 0;

                        for (i, &m) in magnitudes.iter().enumerate() {
                            if m > max {
                                max = m;
                                index = i;
                            }
                        }

                        index as f32
                    };
                    let freq_bin = config.sample_rate.0 as f32 / mono.len() as f32;
                    let freq = index * freq_bin;

                    buf.clear();
                }
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
