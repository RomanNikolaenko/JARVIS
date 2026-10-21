//! Audio devices, microphone capture and playback (cpal: WASAPI on
//! Windows, ALSA on Linux).
//!
//! cpal streams are not `Send` on every platform, so each stream lives on
//! its own thread and is stopped by dropping the handle.

use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tokio::sync::mpsc;

use crate::error::{Result, VoiceError};
use crate::resample::{Resampler, downmix, resample};
use crate::types::AudioDevice;
use crate::vad::SAMPLE_RATE;

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

fn device_error(err: impl std::fmt::Display) -> VoiceError {
    VoiceError::Device(err.to_string())
}

fn device_id(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.to_string())
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| device.to_string())
}

fn list(input: bool) -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default = if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
    .and_then(|d| device_id(&d));
    let devices: Vec<cpal::Device> = if input {
        host.input_devices().map_err(device_error)?.collect()
    } else {
        host.output_devices().map_err(device_error)?.collect()
    };
    let mut out: Vec<AudioDevice> = devices
        .iter()
        .filter_map(|device| {
            let id = device_id(device)?;
            Some(AudioDevice {
                is_default: default.as_deref() == Some(id.as_str()),
                name: device_name(device),
                id,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.cmp(&b.name))
    });
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

pub fn input_devices() -> Result<Vec<AudioDevice>> {
    list(true)
}

pub fn output_devices() -> Result<Vec<AudioDevice>> {
    list(false)
}

/// The chosen device, or the default one if it is not connected.
fn pick(input: bool, id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(id) = id {
        let found = id
            .parse::<cpal::DeviceId>()
            .ok()
            .and_then(|parsed| host.device_by_id(&parsed));
        if let Some(device) = found {
            return Ok(device);
        }
        tracing::warn!("audio device {id} is not available; using the default device");
    }
    let device = if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    };
    device.ok_or_else(|| {
        VoiceError::Device(if input {
            "no microphone found".to_owned()
        } else {
            "no speakers or headphones found".to_owned()
        })
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaptureEvent {
    /// 16 kHz mono samples.
    Audio(Vec<f32>),
    Failed(String),
}

/// An open microphone. Dropping it closes the microphone.
#[derive(Debug)]
pub struct Capture {
    stop: Option<std_mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    device_name: String,
}

impl Capture {
    /// Opens the microphone; audio arrives on `events` as 16 kHz mono.
    pub fn start(device: Option<&str>, events: mpsc::Sender<CaptureEvent>) -> Result<Self> {
        let device = device.map(str::to_owned);
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<String>>();
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("jarvis-microphone".into())
            .spawn(move || {
                let opened = open_input(device.as_deref(), events);
                match opened {
                    Ok((stream, name)) => {
                        let _ = ready_tx.send(Ok(name));
                        // Until the handle is dropped (the sender closes).
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                    }
                }
            })
            .map_err(|err| VoiceError::io("starting the microphone thread", err))?;
        let mut capture = Self {
            stop: Some(stop_tx),
            thread: Some(thread),
            device_name: String::new(),
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(name)) => {
                capture.device_name = name;
                Ok(capture)
            }
            Ok(Err(err)) => Err(err),
            Err(_) => Err(VoiceError::Device(
                "the microphone did not open in time".into(),
            )),
        }
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn open_input(
    device: Option<&str>,
    events: mpsc::Sender<CaptureEvent>,
) -> Result<(cpal::Stream, String)> {
    let device = pick(true, device)?;
    let name = device_name(&device);
    let config = device.default_input_config().map_err(device_error)?;
    let channels = usize::from(config.channels()).max(1);
    let rate = config.sample_rate();
    let stream_config = config.config();
    let stream = match config.sample_format() {
        SampleFormat::F32 => build_input::<f32>(&device, stream_config, channels, rate, events),
        SampleFormat::I16 => build_input::<i16>(&device, stream_config, channels, rate, events),
        SampleFormat::U16 => build_input::<u16>(&device, stream_config, channels, rate, events),
        SampleFormat::I32 => build_input::<i32>(&device, stream_config, channels, rate, events),
        SampleFormat::U8 => build_input::<u8>(&device, stream_config, channels, rate, events),
        SampleFormat::F64 => build_input::<f64>(&device, stream_config, channels, rate, events),
        other => Err(VoiceError::Device(format!(
            "unsupported microphone sample format {other:?}"
        ))),
    }?;
    stream.play().map_err(device_error)?;
    Ok((stream, name))
}

fn build_input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    rate: u32,
    events: mpsc::Sender<CaptureEvent>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut resampler = Resampler::new(rate, SAMPLE_RATE);
    let errors = events.clone();
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                let samples: Vec<f32> = data.iter().map(|s| s.to_sample::<f32>()).collect();
                let mono = downmix(&samples, channels);
                let out = resampler.process(&mono);
                // Never block the audio thread; a full queue drops audio.
                if !out.is_empty() {
                    let _ = events.try_send(CaptureEvent::Audio(out));
                }
            },
            move |err| {
                let _ = errors.try_send(CaptureEvent::Failed(err.to_string()));
            },
            None,
        )
        .map_err(device_error)
}

#[derive(Debug, Default)]
struct Queue {
    samples: VecDeque<f32>,
    failed: Option<String>,
}

/// An open output device that plays queued audio. Dropping it closes the
/// device.
#[derive(Debug)]
pub struct Player {
    queue: Arc<Mutex<Queue>>,
    rate: u32,
    stop: Option<std_mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn open(device: Option<&str>) -> Result<Self> {
        let device = device.map(str::to_owned);
        let queue = Arc::new(Mutex::new(Queue::default()));
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<u32>>();
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let shared = Arc::clone(&queue);
        let thread = std::thread::Builder::new()
            .name("jarvis-speaker".into())
            .spawn(move || match open_output(device.as_deref(), shared) {
                Ok((stream, rate)) => {
                    let _ = ready_tx.send(Ok(rate));
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                }
            })
            .map_err(|err| VoiceError::io("starting the playback thread", err))?;
        let mut player = Self {
            queue,
            rate: 0,
            stop: Some(stop_tx),
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(rate)) => {
                player.rate = rate;
                Ok(player)
            }
            Ok(Err(err)) => Err(err),
            Err(_) => Err(VoiceError::Device(
                "the speakers did not open in time".into(),
            )),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues mono audio at `rate` for playback.
    pub fn enqueue(&self, samples: &[f32], rate: u32) {
        let converted = resample(samples, rate, self.rate);
        self.lock().samples.extend(converted);
    }

    /// Drops everything not yet played.
    pub fn clear(&self) {
        self.lock().samples.clear();
    }

    /// Seconds of audio still queued.
    pub fn pending_secs(&self) -> f32 {
        self.lock().samples.len() as f32 / self.rate.max(1) as f32
    }

    pub fn failure(&self) -> Option<String> {
        self.lock().failed.clone()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn open_output(device: Option<&str>, queue: Arc<Mutex<Queue>>) -> Result<(cpal::Stream, u32)> {
    let device = pick(false, device)?;
    let config = device.default_output_config().map_err(device_error)?;
    let channels = usize::from(config.channels()).max(1);
    let rate = config.sample_rate();
    let stream_config = config.config();
    let stream = match config.sample_format() {
        SampleFormat::F32 => build_output::<f32>(&device, stream_config, channels, queue),
        SampleFormat::I16 => build_output::<i16>(&device, stream_config, channels, queue),
        SampleFormat::U16 => build_output::<u16>(&device, stream_config, channels, queue),
        SampleFormat::I32 => build_output::<i32>(&device, stream_config, channels, queue),
        SampleFormat::U8 => build_output::<u8>(&device, stream_config, channels, queue),
        SampleFormat::F64 => build_output::<f64>(&device, stream_config, channels, queue),
        other => Err(VoiceError::Device(format!(
            "unsupported speaker sample format {other:?}"
        ))),
    }?;
    stream.play().map_err(device_error)?;
    Ok((stream, rate))
}

fn build_output<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    queue: Arc<Mutex<Queue>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let errors = Arc::clone(&queue);
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                let mut queue = queue.lock().unwrap_or_else(PoisonError::into_inner);
                for frame in data.chunks_mut(channels) {
                    let value = T::from_sample(queue.samples.pop_front().unwrap_or(0.0));
                    frame.fill(value);
                }
            },
            move |err| {
                errors.lock().unwrap_or_else(PoisonError::into_inner).failed =
                    Some(err.to_string());
            },
            None,
        )
        .map_err(device_error)
}
