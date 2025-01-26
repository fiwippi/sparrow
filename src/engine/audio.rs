use std::{fmt, sync::Mutex};

use super::{dmx, led};

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

/// Returns (input device, output device)
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

pub fn input_device_info(current_input_device: Option<&str>) -> anyhow::Result<Vec<DeviceInfo>> {
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

pub fn output_device_info(current_output_device: Option<&str>) -> anyhow::Result<Vec<DeviceInfo>> {
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
        latency_ms: f32,
        dmx_agent: Option<dmx::SerialAgent>,
        gradient: led::Gradient,
        errors: mpsc::Sender<anyhow::Error>,
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
                let mut freq = index * freq_bin;

                // TODO Add FFT dampening
                // TODO Make MAX_FREQ configurable
                const MAX_FREQ: f32 = 2000.0;
                if freq > MAX_FREQ {
                    freq = MAX_FREQ
                }

                if let Some(agent) = &dmx_agent {
                    let colour = gradient.interpolate(freq / MAX_FREQ);
                    if let Err(e) = agent.blocking_set_colour(colour) {
                        error!("Failed to set LED colour"; "error" => format!("{:?}", e))
                    }
                }

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
