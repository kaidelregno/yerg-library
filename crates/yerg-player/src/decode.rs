//! A rodio [`Source`] that decodes with symphonia and delivers audio in the
//! output device's own format.
//!
//! Why not rodio's decoder: rodio 0.20 wraps the stream in a reader that hides
//! its length; symphonia's MP4 reader then fails on ordinary `.m4a` files and
//! rodio answers that with a panic.  This source is opened from the file
//! itself and reports every problem as an error.
//!
//! Why convert here: rodio's own conversion uses linear interpolation (poor on
//! 48/96 kHz devices and harsh on 16 kHz Bluetooth headsets), drops the right
//! channel on mono devices and simply truncates surround.  Delivering exactly
//! the device's channel count and sample rate makes rodio's converters a
//! pass-through.  Rates are converted with a windowed-sinc resampler (rubato),
//! surround is downmixed with the usual coefficients.

use std::time::Duration;

use rodio::source::SeekError;
use rodio::Source;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

/// Give up after this many undecodable packets in a row.
const MAX_BAD_PACKETS: usize = 64;
/// Input frames per resampler block.
const RESAMPLE_CHUNK: usize = 1024;

/// The format audio leaves the decoder in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    pub channels: u16,
    pub sample_rate: u32,
}

struct Resample {
    rs: SincFixedIn<f32>,
    in_rate: u32,
    /// Per-channel input waiting for a full block.
    fifo: Vec<Vec<f32>>,
    out: Vec<Vec<f32>>,
    /// Output frames still to drop to cancel the filter delay.
    delay_left: usize,
    /// Frames fed and emitted, to trim the flushed tail to the exact length.
    fed: u64,
    emitted: u64,
}

impl Resample {
    fn new(in_rate: u32, out_rate: u32, channels: usize) -> Option<Resample> {
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let rs = SincFixedIn::<f32>::new(out_rate as f64 / in_rate as f64, 1.0, params, RESAMPLE_CHUNK, channels).ok()?;
        let delay_left = rs.output_delay();
        let out = rs.output_buffer_allocate(true);
        Some(Resample {
            rs,
            in_rate,
            fifo: vec![Vec::with_capacity(RESAMPLE_CHUNK * 2); channels],
            out,
            delay_left,
            fed: 0,
            emitted: 0,
        })
    }

    fn reset(&mut self) {
        self.rs.reset();
        for c in &mut self.fifo {
            c.clear();
        }
        self.delay_left = self.rs.output_delay();
        self.fed = 0;
        self.emitted = 0;
    }

    fn ratio(&self, out_rate: u32) -> f64 {
        out_rate as f64 / self.in_rate as f64
    }

    /// Interleave `frames` frames of `self.out` into `dst`, minus any pending delay.
    fn emit(&mut self, frames: usize, cap: Option<u64>, dst: &mut Vec<f32>) {
        let skip = self.delay_left.min(frames);
        self.delay_left -= skip;
        let mut end = frames;
        if let Some(cap) = cap {
            let room = cap.saturating_sub(self.emitted) as usize;
            end = end.min(skip + room);
        }
        for f in skip..end {
            for ch in &self.out {
                dst.push(ch[f]);
            }
        }
        self.emitted += end.saturating_sub(skip) as u64;
    }

    /// Feed interleaved frames; append whatever comes out to `dst`.
    fn push(&mut self, interleaved: &[f32], dst: &mut Vec<f32>) {
        let ch = self.fifo.len();
        for frame in interleaved.chunks_exact(ch) {
            for (c, s) in frame.iter().enumerate() {
                self.fifo[c].push(*s);
            }
        }
        self.fed += (interleaved.len() / ch) as u64;
        while self.fifo[0].len() >= self.rs.input_frames_next() {
            let Ok((used, made)) = self.rs.process_into_buffer(&self.fifo, &mut self.out, None) else {
                for c in &mut self.fifo {
                    c.clear();
                }
                return;
            };
            for c in &mut self.fifo {
                c.drain(..used);
            }
            self.emit(made, None, dst);
        }
    }

    /// End of input: push out what is buffered, trimmed to the exact length.
    fn flush(&mut self, out_rate: u32, dst: &mut Vec<f32>) {
        let expected = (self.fed as f64 * self.ratio(out_rate)).round() as u64;
        let mut guard = 0;
        while self.emitted < expected && guard < 8 {
            guard += 1;
            let input = if self.fifo[0].is_empty() { None } else { Some(&self.fifo[..]) };
            let Ok((used, made)) = self.rs.process_partial_into_buffer(input, &mut self.out, None) else { break };
            for c in &mut self.fifo {
                let n = used.min(c.len());
                c.drain(..n);
            }
            self.emit(made, Some(expected), dst);
        }
    }
}

pub struct SymphoniaSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    total: Option<Duration>,
    /// Requested output format; `None` = the file's own (fixed by its first packet).
    target: Option<OutputFormat>,
    out: Option<OutputFormat>,
    resample: Option<Resample>,
    buf: Vec<f32>,
    pos: usize,
    /// Input frames to discard after a seek (seeks land on packet boundaries).
    skip_in: usize,
    input_done: bool,
    finished: bool,
}

impl SymphoniaSource {
    /// Open a stream.  `extension` is only a hint for the prober; `target` is
    /// the device format to convert to (`None` keeps the file's format).
    pub fn new(
        source: Box<dyn MediaSource>,
        extension: Option<&str>,
        target: Option<OutputFormat>,
    ) -> Result<SymphoniaSource, SymError> {
        let mss = MediaSourceStream::new(source, Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = extension {
            hint.with_extension(ext);
        }
        let opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let probed = symphonia::default::get_probe().format(&hint, mss, &opts, &MetadataOptions::default())?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(SymError::Unsupported("no audio track"))?;
        let track_id = track.id;
        let total = track
            .codec_params
            .time_base
            .zip(track.codec_params.n_frames)
            .map(|(tb, n)| time_to_duration(tb.calc_time(n)));
        let decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
        let mut src = SymphoniaSource {
            format,
            decoder,
            track_id,
            total,
            target: target.filter(|t| t.channels > 0 && t.sample_rate > 0),
            out: None,
            resample: None,
            buf: Vec::new(),
            pos: 0,
            skip_in: 0,
            input_done: false,
            finished: false,
        };
        // Decode the first block now so the format is known, and so unplayable
        // files fail here rather than on the audio thread.
        match src.fill()? {
            true => Ok(src),
            false => Err(SymError::DecodeError("no audio could be decoded")),
        }
    }

    /// Decode one packet of our track, mapped to the output channel layout.
    /// Returns its input sample rate, or None at the end of the stream.
    fn decode_packet(&mut self, mapped: &mut Vec<f32>) -> Result<Option<u32>, SymError> {
        let mut bad = 0usize;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(e),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    if decoded.frames() == 0 {
                        continue;
                    }
                    let spec = *decoded.spec();
                    let mut sb = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                    sb.copy_interleaved_ref(decoded);
                    let in_ch = spec.channels.count().max(1);
                    let out = *self.out.get_or_insert_with(|| {
                        self.target.unwrap_or(OutputFormat {
                            channels: in_ch as u16,
                            sample_rate: spec.rate.max(1),
                        })
                    });
                    mapped.clear();
                    mix_channels(sb.samples(), spec.channels, in_ch, out.channels as usize, mapped);
                    return Ok(Some(spec.rate.max(1)));
                }
                Err(SymError::DecodeError(e)) => {
                    bad += 1;
                    log::debug!("skipping undecodable packet: {e}");
                    if bad > MAX_BAD_PACKETS {
                        return Err(SymError::DecodeError("too many undecodable packets"));
                    }
                }
                Err(SymError::ResetRequired) => self.decoder.reset(),
                Err(e) => return Err(e),
            }
        }
    }

    /// Refill `buf` with output-format samples.  Ok(false) once everything is out.
    fn fill(&mut self) -> Result<bool, SymError> {
        self.buf.clear();
        self.pos = 0;
        let mut mapped = Vec::new();
        while self.buf.is_empty() {
            if self.input_done {
                return Ok(false);
            }
            let Some(in_rate) = self.decode_packet(&mut mapped)? else {
                self.input_done = true;
                let out = self.out.unwrap_or(OutputFormat { channels: 2, sample_rate: 44_100 });
                if let Some(r) = self.resample.as_mut() {
                    r.flush(out.sample_rate, &mut self.buf);
                }
                return Ok(!self.buf.is_empty());
            };
            let out = self.out.expect("set by decode_packet");
            let ch = out.channels as usize;
            // Drop frames before a seek target.
            let mut data = &mapped[..];
            if self.skip_in > 0 {
                let frames = data.len() / ch;
                let n = self.skip_in.min(frames);
                self.skip_in -= n;
                data = &data[n * ch..];
            }
            if data.is_empty() {
                continue;
            }
            if in_rate == out.sample_rate {
                // A rate change back to the output rate mid-stream: drain the resampler first.
                if let Some(mut r) = self.resample.take() {
                    r.flush(out.sample_rate, &mut self.buf);
                }
                self.buf.extend_from_slice(data);
            } else {
                if self.resample.as_ref().is_some_and(|r| r.in_rate != in_rate) {
                    let mut r = self.resample.take().unwrap();
                    r.flush(out.sample_rate, &mut self.buf);
                }
                if self.resample.is_none() {
                    self.resample = Resample::new(in_rate, out.sample_rate, ch);
                    if self.resample.is_none() {
                        return Err(SymError::Unsupported("cannot resample to the output rate"));
                    }
                }
                self.resample.as_mut().unwrap().push(data, &mut self.buf);
            }
        }
        Ok(true)
    }

    fn out_format(&self) -> OutputFormat {
        self.out.or(self.target).unwrap_or(OutputFormat { channels: 2, sample_rate: 44_100 })
    }
}

/// Map one packet from its channel layout to `to` output channels.
///
/// * mono in: copied to every output channel (or the first two of a surround device)
/// * stereo in: as is; averaged for a mono device; front left/right of a surround device
/// * surround in: downmixed to stereo first (centre and surrounds at -3 dB,
///   LFE dropped, normalised so it cannot clip), then mapped as stereo
pub fn mix_channels(input: &[f32], layout: Channels, from: usize, to: usize, out: &mut Vec<f32>) {
    let to = to.max(1);
    if from == to && from <= 2 {
        out.extend_from_slice(input);
        return;
    }
    // Downmix coefficients (left, right) per input channel.
    let coeffs: Vec<(f32, f32)> = if from == 1 {
        vec![(1.0, 1.0)]
    } else if from == 2 {
        vec![(1.0, 0.0), (0.0, 1.0)]
    } else {
        let mut v: Vec<(f32, f32)> = layout.iter().map(downmix_coeff).collect();
        if v.len() != from {
            // Unknown layout: treat the first two as front left/right.
            v = (0..from).map(|i| match i {
                0 => (1.0, 0.0),
                1 => (0.0, 1.0),
                _ => (0.0, 0.0),
            }).collect();
        }
        let (sl, sr) = v.iter().fold((0.0f32, 0.0f32), |(a, b), (l, r)| (a + l, b + r));
        let norm = sl.max(sr).max(1.0);
        v.into_iter().map(|(l, r)| (l / norm, r / norm)).collect()
    };
    out.reserve(input.len() / from * to);
    for frame in input.chunks_exact(from) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for (s, (cl, cr)) in frame.iter().zip(&coeffs) {
            l += s * cl;
            r += s * cr;
        }
        match to {
            1 => out.push(if from == 1 { frame[0] } else { (l + r) * 0.5 }),
            2 => {
                out.push(l);
                out.push(r);
            }
            n => {
                out.push(l);
                out.push(r);
                out.extend(std::iter::repeat(0.0).take(n - 2));
            }
        }
    }
}

fn downmix_coeff(ch: Channels) -> (f32, f32) {
    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
    if ch == Channels::FRONT_LEFT {
        (1.0, 0.0)
    } else if ch == Channels::FRONT_RIGHT {
        (0.0, 1.0)
    } else if ch == Channels::FRONT_CENTRE {
        (H, H)
    } else if ch == Channels::LFE1 || ch == Channels::LFE2 {
        (0.0, 0.0)
    } else if ch == Channels::REAR_LEFT
        || ch == Channels::SIDE_LEFT
        || ch == Channels::FRONT_LEFT_CENTRE
        || ch == Channels::REAR_LEFT_CENTRE
        || ch == Channels::FRONT_LEFT_WIDE
        || ch == Channels::FRONT_LEFT_HIGH
        || ch == Channels::TOP_FRONT_LEFT
        || ch == Channels::TOP_REAR_LEFT
    {
        (H, 0.0)
    } else if ch == Channels::REAR_RIGHT
        || ch == Channels::SIDE_RIGHT
        || ch == Channels::FRONT_RIGHT_CENTRE
        || ch == Channels::REAR_RIGHT_CENTRE
        || ch == Channels::FRONT_RIGHT_WIDE
        || ch == Channels::FRONT_RIGHT_HIGH
        || ch == Channels::TOP_FRONT_RIGHT
        || ch == Channels::TOP_REAR_RIGHT
    {
        (0.0, H)
    } else {
        (0.5, 0.5) // rear/top centre and anything else
    }
}

fn time_to_duration(t: Time) -> Duration {
    Duration::from_secs(t.seconds) + Duration::from_secs_f64(t.frac.clamp(0.0, 0.999_999_999))
}

impl Iterator for SymphoniaSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() {
            if self.finished {
                return None;
            }
            match self.fill() {
                Ok(true) => {}
                Ok(false) => {
                    self.finished = true;
                    return None;
                }
                Err(e) => {
                    log::warn!("playback stopped: {e}");
                    self.finished = true;
                    return None;
                }
            }
        }
        let s = self.buf[self.pos];
        self.pos += 1;
        Some(s)
    }
}

impl Source for SymphoniaSource {
    fn current_frame_len(&self) -> Option<usize> {
        // None = the format never changes: fill() always delivers the same
        // channel count and rate.  Per-packet lengths would make rodio rebuild
        // its converters at every packet and, when resampling, split stereo
        // pairs (an audible buzz).
        None
    }

    fn channels(&self) -> u16 {
        self.out_format().channels
    }

    fn sample_rate(&self) -> u32 {
        self.out_format().sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.buf.clear();
        self.pos = 0;
        // Past the end: just finish, as if the track had played out.
        if self.total.is_some_and(|t| pos >= t) {
            self.finished = true;
            self.input_done = true;
            return Ok(());
        }
        let target = Time::new(pos.as_secs(), pos.subsec_nanos() as f64 / 1e9);
        let seeked = self
            .format
            .seek(SeekMode::Accurate, SeekTo::Time { time: target, track_id: Some(self.track_id) })
            .map_err(|e| SeekError::Other(Box::new(e)))?;
        self.decoder.reset();
        if let Some(r) = self.resample.as_mut() {
            r.reset();
        }
        self.skip_in = seeked.required_ts.saturating_sub(seeked.actual_ts) as usize;
        self.input_done = false;
        self.finished = false;
        match self.fill() {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.finished = true;
                Ok(())
            }
            Err(e) => Err(SeekError::Other(Box::new(e))),
        }
    }
}

/// A file (or part of one) as a symphonia media source with a known length.
pub struct ReadSeekLen<R> {
    inner: R,
    len: Option<u64>,
}

impl<R> ReadSeekLen<R> {
    pub fn new(inner: R, len: Option<u64>) -> ReadSeekLen<R> {
        ReadSeekLen { inner, len }
    }
}

impl<R: std::io::Read> std::io::Read for ReadSeekLen<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: std::io::Seek> std::io::Seek for ReadSeekLen<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<R: std::io::Read + std::io::Seek + Send + Sync> MediaSource for ReadSeekLen<R> {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo() -> Channels {
        Channels::FRONT_LEFT | Channels::FRONT_RIGHT
    }

    #[test]
    fn stereo_to_mono_averages_both_channels() {
        let mut out = Vec::new();
        mix_channels(&[0.2, 0.4, -0.2, -0.4], stereo(), 2, 1, &mut out);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.3).abs() < 1e-6 && (out[1] + 0.3).abs() < 1e-6);
    }

    #[test]
    fn mono_is_copied_to_both_sides() {
        let mut out = Vec::new();
        mix_channels(&[0.5, -0.5], Channels::FRONT_LEFT, 1, 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5, -0.5, -0.5]);
    }

    #[test]
    fn stereo_on_a_surround_device_goes_to_the_front() {
        let mut out = Vec::new();
        mix_channels(&[0.1, 0.2], stereo(), 2, 6, &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn surround_is_downmixed_without_clipping() {
        // 5.1: FL FR FC LFE RL RR
        let layout = Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::FRONT_CENTRE
            | Channels::LFE1
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT;
        let mut out = Vec::new();
        // Everything at full scale except LFE (which must be ignored)
        mix_channels(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0], layout, 6, 2, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out[0] <= 1.0 + 1e-6 && out[0] > 0.99, "{out:?}");
        // Only the centre: equal in both, at -3 dB relative to a front channel
        out.clear();
        mix_channels(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0], layout, 6, 2, &mut out);
        assert!((out[0] - out[1]).abs() < 1e-6);
        out.clear();
        mix_channels(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0], layout, 6, 2, &mut out);
        assert!(out[0] > 0.0 && out[1] == 0.0);
        // LFE alone is silent
        out.clear();
        mix_channels(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0], layout, 6, 2, &mut out);
        assert_eq!(out, vec![0.0, 0.0]);
    }
}
