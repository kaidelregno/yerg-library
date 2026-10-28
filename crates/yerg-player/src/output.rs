//! Audio output devices: choosing one, opening it in a known format, and
//! noticing when it goes away.
//!
//! The cpal stream is built here rather than by `rodio::OutputStream`, which
//! silently swaps in another format when the requested one fails and only
//! prints stream errors to stderr.  Here the active format is always known (so
//! the decoder can deliver exactly that), errors raise a flag, and a counter
//! bumped by every audio callback shows whether the device is still pulling.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::cpal::{self, FromSample, SampleFormat, SizedSample, SupportedStreamConfig};
use crate::decode::OutputFormat;
use crate::playback::Playback;

pub struct Output {
    _stream: cpal::Stream,
    /// What the device plays; shared with the audio callback.
    pub playback: Arc<Mutex<Playback>>,
    pub device_name: String,
    pub format: OutputFormat,
    pub sample_format: SampleFormat,
    /// Set by the stream's error callback.
    pub failed: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<String>>>,
    /// Frames rendered so far; stops moving if the device stops pulling.
    pub frames: Arc<AtomicU64>,
}

impl Output {
    /// Human-readable format, e.g. "48 kHz, 2 ch, f32".
    pub fn describe(&self) -> String {
        describe_format(self.format, self.sample_format)
    }

}

pub fn describe_format(f: OutputFormat, sf: SampleFormat) -> String {
    let rate = if f.sample_rate % 1000 == 0 {
        format!("{} kHz", f.sample_rate / 1000)
    } else {
        format!("{:.1} kHz", f.sample_rate as f64 / 1000.0)
    };
    let ch = match f.channels {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        n => format!("{n} ch"),
    };
    format!("{rate}, {ch}, {}", format!("{sf:?}").to_ascii_lowercase())
}

/// Name of the system's current default output device.
pub fn default_device_name() -> Option<String> {
    cpal::default_host().default_output_device().and_then(|d| d.name().ok())
}

/// Names of all output devices, in the order the system lists them.
pub fn device_names() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(devices) = cpal::default_host().output_devices() {
        for d in devices {
            if let Ok(n) = d.name() {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// Whether a device with this name is currently present.
pub fn device_exists(name: &str) -> bool {
    find_device(name).is_some()
}

fn find_device(name: &str) -> Option<cpal::Device> {
    cpal::default_host()
        .output_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

/// Open the preferred device if it is present, else the system default, else
/// any device that works.  Returns the output and whether it is the preferred one.
pub fn open(preferred: Option<&str>) -> Result<(Output, bool), String> {
    let host = cpal::default_host();
    let mut candidates: Vec<(cpal::Device, bool)> = Vec::new();
    if let Some(name) = preferred {
        if let Some(d) = find_device(name) {
            candidates.push((d, true));
        }
    }
    if let Some(d) = host.default_output_device() {
        candidates.push((d, false));
    }
    let mut errors = Vec::new();
    for (dev, is_pref) in candidates {
        match open_device(&dev) {
            Ok(o) => return Ok((o, is_pref)),
            Err(e) => errors.push(e),
        }
    }
    // Last resort: anything that will open.
    if let Ok(devices) = host.output_devices() {
        for dev in devices {
            if let Ok(o) = open_device(&dev) {
                return Ok((o, false));
            }
        }
    }
    if errors.is_empty() {
        Err("no audio output device found".to_string())
    } else {
        Err(errors.join("; "))
    }
}

/// Configurations to try, best first: the device's own default (what the OS
/// mixer runs at, so no extra resampling happens there), then common rates in
/// stereo with float or 16-bit samples, then anything else it supports.
fn candidate_configs(dev: &cpal::Device) -> Vec<SupportedStreamConfig> {
    let mut out = Vec::new();
    if let Ok(c) = dev.default_output_config() {
        out.push(c);
    }
    if let Ok(ranges) = dev.supported_output_configs() {
        let mut extra = Vec::new();
        for r in ranges {
            for rate in [48_000u32, 44_100, 96_000] {
                if r.min_sample_rate().0 <= rate && rate <= r.max_sample_rate().0 {
                    extra.push(r.with_sample_rate(cpal::SampleRate(rate)));
                }
            }
            extra.push(r.with_max_sample_rate());
        }
        let rank = |c: &SupportedStreamConfig| {
            let ch = if c.channels() == 2 { 0 } else if c.channels() == 1 { 2 } else { 1 };
            let fmt = match c.sample_format() {
                SampleFormat::F32 => 0,
                SampleFormat::I16 => 1,
                SampleFormat::I32 => 2,
                _ => 3,
            };
            (ch, fmt)
        };
        extra.sort_by_key(rank);
        out.extend(extra);
    }
    out
}

fn open_device(dev: &cpal::Device) -> Result<Output, String> {
    let name = dev.name().unwrap_or_else(|_| "unknown device".to_string());
    let configs = candidate_configs(dev);
    if configs.is_empty() {
        return Err(format!("{name}: no usable output configuration"));
    }
    let mut last_err = String::new();
    for cfg in configs {
        match build_stream(dev, &cfg) {
            Ok((stream, playback, failed, error, frames)) => {
                if let Err(e) = stream.play() {
                    last_err = e.to_string();
                    continue;
                }
                return Ok(Output {
                    _stream: stream,
                    playback,
                    device_name: name,
                    format: OutputFormat {
                        channels: cfg.channels(),
                        sample_rate: cfg.sample_rate().0,
                    },
                    sample_format: cfg.sample_format(),
                    failed,
                    error,
                    frames,
                });
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("{name}: {last_err}"))
}

type Built = (
    cpal::Stream,
    Arc<Mutex<Playback>>,
    Arc<AtomicBool>,
    Arc<Mutex<Option<String>>>,
    Arc<AtomicU64>,
);

fn build_stream(dev: &cpal::Device, cfg: &SupportedStreamConfig) -> Result<Built, String> {
    match cfg.sample_format() {
        SampleFormat::F32 => build_typed::<f32>(dev, cfg),
        SampleFormat::F64 => build_typed::<f64>(dev, cfg),
        SampleFormat::I8 => build_typed::<i8>(dev, cfg),
        SampleFormat::I16 => build_typed::<i16>(dev, cfg),
        SampleFormat::I32 => build_typed::<i32>(dev, cfg),
        SampleFormat::I64 => build_typed::<i64>(dev, cfg),
        SampleFormat::U8 => build_typed::<u8>(dev, cfg),
        SampleFormat::U16 => build_typed::<u16>(dev, cfg),
        SampleFormat::U32 => build_typed::<u32>(dev, cfg),
        SampleFormat::U64 => build_typed::<u64>(dev, cfg),
        other => Err(format!("unsupported sample format {other:?}")),
    }
}

fn build_typed<T>(dev: &cpal::Device, cfg: &SupportedStreamConfig) -> Result<Built, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = cfg.channels();
    let playback = Arc::new(Mutex::new(Playback::new(channels)));
    let pb = playback.clone();
    let mut scratch: Vec<f32> = Vec::new();
    let failed = Arc::new(AtomicBool::new(false));
    let error = Arc::new(Mutex::new(None));
    let frames = Arc::new(AtomicU64::new(0));
    let (f2, e2, fr2) = (failed.clone(), error.clone(), frames.clone());
    let stream = dev
        .build_output_stream::<T, _, _>(
            &cfg.config(),
            move |data: &mut [T], _| {
                if scratch.len() < data.len() {
                    scratch.resize(data.len(), 0.0);
                }
                let buf = &mut scratch[..data.len()];
                pb.lock().render(buf);
                for (d, s) in data.iter_mut().zip(buf.iter()) {
                    *d = T::from_sample(s.clamp(-1.0, 1.0));
                }
                fr2.fetch_add((data.len() / channels.max(1) as usize) as u64, Ordering::Relaxed);
            },
            move |err| {
                log::warn!("audio output error: {err}");
                *e2.lock() = Some(err.to_string());
                f2.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    Ok((stream, playback, failed, error, frames))
}
