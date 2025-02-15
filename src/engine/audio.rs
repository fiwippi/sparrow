use std::{
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{dmx, led};

use cpal::{
    self,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use num_complex::Complex;
use ringbuf::{
    traits::{Consumer, Producer, Split},
    CachingProd, HeapRb,
};
use slog_scope::error;

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
    output_stream: Option<cpal::Stream>,
}

impl Pipe {
    pub fn new(
        input_handle: &cpal::Device,
        output_handle: Option<&cpal::Device>,
        latency_ms: f32,
        dmx_agent: Option<dmx::SerialAgent>,
        gradient: led::Gradient,
    ) -> anyhow::Result<Self> {
        // We use the same configurations between the input
        // and output stream to simplify the logic
        let supported_config: cpal::SupportedStreamConfig = input_handle.default_input_config()?;
        let mut config: cpal::StreamConfig = supported_config.config();
        if let cpal::SupportedBufferSize::Range { min, max: _ } = supported_config.buffer_size() {
            // If the buffer isn't small, we must wait a long time for the OS
            // to buffer the audio data to the size we want instead of doing
            // it ourselves, which means we can't interpolate LED lights in
            // the meantime
            config.buffer_size = cpal::BufferSize::Fixed(u32::max(*min, 256));
        }

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

        // If we don't want to perform loopback then an output
        // stream won't be defined
        let output_stream = if let Some(output_handle) = output_handle {
            Some(output_handle.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    for sample in data {
                        *sample = consumer.try_pop().unwrap_or_else(|| 0.0);
                    }
                },
                move |err: cpal::StreamError| {
                    error!("Output stream error"; "error" => format!("{:?}", err));
                },
                None,
            )?)
        } else {
            None
        };

        Ok(Self {
            input_stream: input_handle.build_input_stream(
                &config,
                input_callback(
                    producer,
                    config.sample_rate,
                    dmx_agent,
                    gradient,
                    output_handle.is_some(),
                ),
                move |err: cpal::StreamError| {
                    error!("Input stream error"; "error" => format!("{:?}", err));
                },
                None,
            )?,
            output_stream,
        })
    }

    pub fn play(&self) -> anyhow::Result<()> {
        self.input_stream.play()?;
        if let Some(output_stream) = &self.output_stream {
            output_stream.play()?;
        }

        Ok(())
    }
}

fn input_callback(
    mut producer: CachingProd<Arc<HeapRb<f32>>>,
    sample_rate: cpal::SampleRate,
    dmx_agent: Option<dmx::SerialAgent>,
    gradient: led::Gradient,
    loopback: bool,
) -> impl FnMut(&[f32], &cpal::InputCallbackInfo) {
    // These constants are used to define how the calculated
    // FFT frequency is converted into a colour which looks
    // nice, they are a bit magic, let's not change them
    const TOTAL_HUES: f32 = 320.0;
    const MAX_FREQ: f32 = 2500.0;
    const MAX_USEFUL_FREQ: f32 = 1200.0;
    const USEFUL_FREQ_HUE: f32 = 310.0;
    const MIN_PERIOD: Duration = Duration::from_millis(250);

    struct Data {
        // We perform FFT calculations from this buffer
        buffer: Vec<f32>,
        // The max frequency of last buffer
        freq: f32,
        // The max frequency of last - 1 buffer
        old_freq: f32,
        // Time taken between FFT calculations
        period: Duration,
        // Instant the FFT was last calculated
        last_calculated: Instant,
    }
    let mut state = Data {
        buffer: Vec::new(),
        freq: 0.0,
        old_freq: 0.0,
        period: Duration::new(0, 0),
        last_calculated: Instant::now(),
    };

    move |data: &[f32], _: &cpal::InputCallbackInfo| {
        let samples_written = if loopback {
            producer.push_slice(data)
        } else {
            // If we're not performing loopback then there's no
            // point writing to the ringbuffer, we still do want
            // to process the data however
            data.len()
        };
        if samples_written > 0 {
            state.buffer.extend_from_slice(&data[..samples_written]);
            // The lower the MIN_PERIOD the smaller the buffer size used
            // to calculate the FFT, this will result in a more inaccurate
            // calculation (larger error boundary)
            if Instant::now().duration_since(state.last_calculated) > MIN_PERIOD {
                state.period = Instant::now().duration_since(state.last_calculated);
                state.last_calculated = Instant::now();

                // Audio is interleaved, LRLRLR, so to mix
                // down the samples into mono we do L+R/2
                let mut mono: Vec<Complex<f32>> = state
                    .buffer
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
                let freq_bin = sample_rate.0 as f32 / mono.len() as f32;

                state.old_freq = state.freq;
                state.freq = f32::min(index * freq_bin, MAX_FREQ);

                state.buffer.clear();
            }
        }

        // To make the colour change smoother, we dampen the frequency
        let delta = Instant::now().duration_since(state.last_calculated);
        let ratio = delta.as_nanos() as f32 / state.period.as_nanos() as f32;
        let interpolated_freq = state.old_freq + ratio.sqrt() * (state.freq - state.old_freq);

        // This is an artifact of when the visualisation system used to
        // be HSV-only, but we're keeping it because it creates a nice
        // smooth colour.
        let hue = if interpolated_freq > MAX_USEFUL_FREQ {
            USEFUL_FREQ_HUE + (TOTAL_HUES - USEFUL_FREQ_HUE) * (interpolated_freq / MAX_FREQ)
        } else {
            interpolated_freq / MAX_USEFUL_FREQ * USEFUL_FREQ_HUE
        };
        let colour = gradient.interpolate(hue / TOTAL_HUES);

        // Boom! Send the colour
        if let Some(agent) = &dmx_agent {
            // This would only fail if the channel is closed,
            // this happens on Daemon shutdown so we can just
            // ignore this
            _ = agent.blocking_set_colour(colour)
        }
    }
}
