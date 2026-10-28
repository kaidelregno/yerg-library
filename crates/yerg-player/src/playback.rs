//! What the audio callback plays: the current source, volume, pause.
//!
//! This replaces rodio's `Sink` → queue → mixer chain, whose fixed 512-sample
//! chunks and per-chunk converter rebuilds split multichannel frames (6- and
//! 8-channel devices heard the music shifted onto the rear speakers) and,
//! when resampling, split stereo pairs.  The source here is already in the
//! device's exact format, so rendering is a straight copy with a gain.

use rodio::Source;

pub type BoxedSource = Box<dyn Source<Item = f32> + Send>;

/// Per-frame smoothing of gain changes (about 5 ms at 48 kHz) so volume
/// changes, pauses and resumes do not click.
const RAMP: f32 = 1.0 / 256.0;
/// Below this the output counts as silent (pause has fully faded out).
const SILENT: f32 = 1e-4;

pub struct Playback {
    channels: usize,
    source: Option<BoxedSource>,
    paused: bool,
    volume: f32,
    gain: f32,
    /// Frames of the current source delivered since it was set.
    frames: u64,
    /// Frames delivered while not paused (for "how long was this listened to").
    listened: u64,
    /// The source ran out.
    finished: bool,
}

impl Playback {
    pub fn new(channels: u16) -> Playback {
        Playback {
            channels: channels.max(1) as usize,
            source: None,
            paused: false,
            volume: 1.0,
            gain: 0.0,
            frames: 0,
            listened: 0,
            finished: false,
        }
    }

    /// Start a new source (already in the device format).  Returns the old one
    /// so the caller can drop it outside any lock.
    pub fn set_source(&mut self, source: Option<BoxedSource>, paused: bool) -> Option<BoxedSource> {
        self.paused = paused;
        self.frames = 0;
        self.listened = 0;
        self.finished = false;
        self.gain = 0.0; // fade in
        std::mem::replace(&mut self.source, source)
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn listened(&self) -> u64 {
        self.listened
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn has_source(&self) -> bool {
        self.source.is_some()
    }

    /// Fill an interleaved buffer for the device.
    pub fn render(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        for frame in out.chunks_exact_mut(ch) {
            let target = if self.paused || self.source.is_none() { 0.0 } else { self.volume };
            self.gain += (target - self.gain) * RAMP;
            if (target - self.gain).abs() < 1e-6 {
                self.gain = target;
            }
            let pull = self.source.is_some() && !(self.paused && self.gain < SILENT);
            if !pull {
                frame.fill(0.0);
                continue;
            }
            let src = self.source.as_mut().unwrap();
            let mut ended = false;
            for s in frame.iter_mut() {
                match src.next() {
                    Some(v) => *s = v * self.gain,
                    None => {
                        *s = 0.0;
                        ended = true;
                    }
                }
            }
            if ended {
                self.source = None;
                self.finished = true;
            } else {
                self.frames += 1;
                if !self.paused {
                    self.listened += 1;
                }
            }
        }
        // A partial trailing frame (should not happen) is silenced.
        let rem = out.len() % ch;
        if rem != 0 {
            let n = out.len();
            out[n - rem..].fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::source::SineWave;

    fn sine(secs: f32) -> BoxedSource {
        // rodio's SineWave is mono 48 kHz, endless; take a fixed amount
        Box::new(SineWave::new(440.0).take_duration(std::time::Duration::from_secs_f32(secs)).amplify(0.5))
    }

    #[test]
    fn plays_to_the_end_and_counts_frames() {
        let mut p = Playback::new(1);
        p.set_volume(1.0);
        p.set_source(Some(sine(0.5)), false);
        let mut buf = vec![0.0f32; 48_000];
        p.render(&mut buf);
        assert!(p.finished());
        assert!((p.frames() as i64 - 24_000).abs() < 2, "{}", p.frames());
        assert_eq!(p.frames(), p.listened());
        // Silence after the end
        assert!(buf[30_000..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn pause_fades_out_and_stops_pulling() {
        let mut p = Playback::new(1);
        p.set_source(Some(sine(10.0)), false);
        let mut buf = vec![0.0f32; 4_800];
        p.render(&mut buf);
        let before = p.frames();
        p.set_paused(true);
        p.render(&mut buf);
        // Faded smoothly: no sample-to-sample jump bigger than the tone itself makes
        let jump = buf.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(jump < 0.03, "click on pause: {jump}");
        // Once silent, the source is no longer consumed
        let paused_at = p.frames();
        assert!(paused_at > before && paused_at < before + 4_800);
        p.render(&mut buf);
        assert_eq!(p.frames(), paused_at);
        assert!(buf.iter().all(|s| s.abs() < 1e-3));
        p.set_paused(false);
        p.render(&mut buf);
        assert!(p.frames() > paused_at);
    }

    #[test]
    fn volume_scales_output() {
        let mut p = Playback::new(1);
        p.set_volume(0.25);
        p.set_source(Some(sine(1.0)), false);
        let mut buf = vec![0.0f32; 9_600];
        p.render(&mut buf);
        let peak = buf[4_800..].iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!((peak - 0.125).abs() < 0.01, "peak {peak}");
    }
}
