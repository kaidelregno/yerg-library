//! Audio playback engine.
//!
//! A dedicated thread owns the audio output and takes commands over a
//! channel.  The UI reads a shared [`PlayerState`] snapshot and receives
//! [`PlayerEvent`]s when a track ends or playback fails.
//!
//! Devices: the engine plays to a chosen device, or follows the system
//! default.  It notices when the default changes, when the device reports an
//! error or stops pulling audio (unplugged USB DAC, Bluetooth headphones
//! switched off), and reconnects, carrying on from the same position.  Audio
//! is decoded straight into the device's own sample rate and channel count.

use std::fs::File;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use rodio::Source;
use yerg_core::audio_io::SkipPrefix;

pub mod decode;
pub mod output;
pub mod playback;
use decode::{OutputFormat, ReadSeekLen, SymphoniaSource};
use output::Output;

#[derive(Debug, Clone)]
pub enum Command {
    /// Start playing a file from the beginning.
    Load { path: PathBuf, paused: bool },
    Play,
    Pause,
    Toggle,
    Stop,
    Seek(Duration),
    SetVolume(f32),
    /// Play to this device (by name), or follow the system default with `None`.
    SetOutputDevice(Option<String>),
    Quit,
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    /// The current file played to its end.
    TrackEnded { path: PathBuf },
    /// Loading or playing failed; the engine is idle afterwards.
    Error { path: Option<PathBuf>, message: String },
    /// Audio output came up, or moved to another device.
    OutputReady { device: String },
    /// The output device went away; playback resumes when one is available.
    OutputLost { device: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct PlayerState {
    pub path: Option<PathBuf>,
    /// True while a file is loaded (playing or paused).
    pub loaded: bool,
    pub paused: bool,
    pub position: Duration,
    /// When `position` was read from the audio clock.  The engine refreshes
    /// it about every 50 ms; [`PlayerState::position_now`] fills the gap.
    pub position_at: Option<Instant>,
    /// Duration as reported by the decoder, when it knows it.
    pub duration: Option<Duration>,
    pub volume: f32,
    /// Wall-clock milliseconds actually spent playing the current file.
    pub played_ms: u64,
    /// Why there is no sound right now, if there is a device problem.
    pub output_error: Option<String>,
    /// Device currently playing, and its format ("48 kHz, stereo, f32").
    pub output_device: Option<String>,
    pub output_format: Option<String>,
    /// The device the user picked (`None` = follow the system default).
    pub preferred_device: Option<String>,
}

impl Default for PlayerState {
    fn default() -> Self {
        PlayerState {
            path: None,
            loaded: false,
            paused: false,
            position: Duration::ZERO,
            position_at: None,
            duration: None,
            volume: 0.8,
            played_ms: 0,
            output_error: None,
            output_device: None,
            output_format: None,
            preferred_device: None,
        }
    }
}

/// How far [`PlayerState::position_now`] runs ahead of the last reading.  A
/// little over two engine ticks, so a stalled device does not run away.
const MAX_EXTRAPOLATION: Duration = Duration::from_millis(120);

impl PlayerState {
    pub fn is_playing(&self) -> bool {
        self.loaded && !self.paused
    }

    /// The playing position at this moment, for smooth progress display: the
    /// last reading plus the time since, while playing (never past the end).
    pub fn position_now(&self) -> Duration {
        let mut pos = self.position;
        if self.is_playing() {
            if let Some(at) = self.position_at {
                pos += at.elapsed().min(MAX_EXTRAPOLATION);
            }
        }
        match self.duration {
            Some(d) => pos.min(d),
            None => pos,
        }
    }
}

pub struct Player {
    tx: Sender<Command>,
    events: Receiver<PlayerEvent>,
    state: Arc<Mutex<PlayerState>>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Start the engine.  `device` is the preferred output (`None` = system default).
    pub fn new() -> Player {
        Player::with_device(None)
    }

    pub fn with_device(device: Option<String>) -> Player {
        let (tx, rx) = unbounded::<Command>();
        let (etx, erx) = unbounded::<PlayerEvent>();
        let state = Arc::new(Mutex::new(PlayerState {
            preferred_device: device.clone(),
            ..Default::default()
        }));
        let st = state.clone();
        let thread = std::thread::Builder::new()
            .name("yerg-player".into())
            .spawn(move || Engine::new(etx, st, device).run(rx))
            .expect("spawn player thread");
        Player {
            tx,
            events: erx,
            state,
            thread: Some(thread),
        }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    pub fn load(&self, path: impl Into<PathBuf>) {
        self.send(Command::Load {
            path: path.into(),
            paused: false,
        });
    }
    pub fn play(&self) {
        self.send(Command::Play)
    }
    pub fn pause(&self) {
        self.send(Command::Pause)
    }
    pub fn toggle(&self) {
        self.send(Command::Toggle)
    }
    pub fn stop(&self) {
        self.send(Command::Stop)
    }
    pub fn seek(&self, pos: Duration) {
        self.send(Command::Seek(pos))
    }
    pub fn set_volume(&self, v: f32) {
        self.send(Command::SetVolume(v.clamp(0.0, 1.0)))
    }
    pub fn set_output_device(&self, device: Option<String>) {
        self.send(Command::SetOutputDevice(device))
    }

    /// Output devices present right now.
    pub fn output_devices() -> Vec<String> {
        output::device_names()
    }

    /// The system default output device.
    pub fn default_output_device() -> Option<String> {
        output::default_device_name()
    }

    pub fn state(&self) -> PlayerState {
        self.state.lock().clone()
    }

    /// Drain pending events without blocking.
    pub fn poll_events(&self) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            out.push(e);
        }
        out
    }

    pub fn event_receiver(&self) -> Receiver<PlayerEvent> {
        self.events.clone()
    }
}

impl Default for Player {
    fn default() -> Self {
        Player::new()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How often the device situation is re-checked.
const DEVICE_CHECK: Duration = Duration::from_millis(1000);
/// How often to retry when no device could be opened.
const DEVICE_RETRY: Duration = Duration::from_secs(2);
/// A device that has not pulled any audio for this long is considered dead.
const STALL_LIMIT: Duration = Duration::from_millis(2500);

struct Current {
    path: PathBuf,
    /// Whether the open output's playback holds this track's source.
    active: bool,
    /// Position the active source started from (seeks and device moves restart it).
    base: Duration,
    duration: Option<Duration>,
    paused: bool,
    /// Listening time from earlier sources of this track (before seeks / device moves).
    played_before: Duration,
    ended_reported: bool,
}

struct Engine {
    events: Sender<PlayerEvent>,
    state: Arc<Mutex<PlayerState>>,
    output: Option<Output>,
    /// Device the user asked for (`None` = system default).
    preferred: Option<String>,
    /// Whether the open output is the preferred device.
    on_preferred: bool,
    current: Option<Current>,
    volume: f32,
    last_check: Instant,
    last_open_try: Option<Instant>,
    /// Heartbeat: frames rendered at the last check and when that number last moved.
    last_frames: u64,
    frames_moved_at: Instant,
}

fn frames_to_duration(frames: u64, rate: u32) -> Duration {
    Duration::from_secs_f64(frames as f64 / rate.max(1) as f64)
}

impl Engine {
    fn new(events: Sender<PlayerEvent>, state: Arc<Mutex<PlayerState>>, preferred: Option<String>) -> Engine {
        let volume = state.lock().volume;
        Engine {
            events,
            state,
            output: None,
            preferred,
            on_preferred: false,
            current: None,
            volume,
            last_check: Instant::now(),
            last_open_try: None,
            last_frames: 0,
            frames_moved_at: Instant::now(),
        }
    }

    fn run(mut self, rx: Receiver<Command>) {
        self.try_open_output();
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Command::Quit) => break,
                Ok(cmd) => {
                    // A panic in a decoder must not take the whole player down with it.
                    let result = catch_unwind(AssertUnwindSafe(|| self.handle(cmd)));
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => log::warn!("player: {e:#}"),
                        Err(panic) => {
                            let msg = panic_message(&panic);
                            log::error!("player command panicked: {msg}");
                            self.clear_source();
                            let path = self.current.take().map(|c| c.path);
                            {
                                let mut s = self.state.lock();
                                s.loaded = false;
                                s.paused = false;
                            }
                            let _ = self.events.send(PlayerEvent::Error {
                                path,
                                message: format!("internal error: {msg}"),
                            });
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.last_check.elapsed() >= DEVICE_CHECK {
                self.last_check = Instant::now();
                if let Err(p) = catch_unwind(AssertUnwindSafe(|| self.check_device())) {
                    log::error!("device check panicked: {}", panic_message(&p));
                }
            }
            self.tick();
        }
        self.clear_source();
    }

    // ------------------------------------------------------------ devices

    fn publish_output(&self) {
        let mut s = self.state.lock();
        match &self.output {
            Some(o) => {
                s.output_device = Some(o.device_name.clone());
                s.output_format = Some(o.describe());
                s.output_error = if self.preferred.is_some() && !self.on_preferred {
                    Some(format!(
                        "{} is not available; playing on {} instead",
                        self.preferred.as_deref().unwrap_or_default(),
                        o.device_name
                    ))
                } else {
                    None
                };
            }
            None => {
                s.output_device = None;
                s.output_format = None;
            }
        }
        s.preferred_device = self.preferred.clone();
    }

    fn try_open_output(&mut self) -> bool {
        self.last_open_try = Some(Instant::now());
        match output::open(self.preferred.as_deref()) {
            Ok((o, is_pref)) => {
                log::info!("audio output: {} ({})", o.device_name, o.describe());
                o.playback.lock().set_volume(self.volume);
                let name = o.device_name.clone();
                self.last_frames = o.frames.load(Ordering::Relaxed);
                self.frames_moved_at = Instant::now();
                self.output = Some(o);
                self.on_preferred = is_pref;
                self.publish_output();
                let _ = self.events.send(PlayerEvent::OutputReady { device: name });
                // Whatever was playing carries on from where it was.
                self.restart_current(None);
                true
            }
            Err(e) => {
                self.output = None;
                self.publish_output();
                self.state.lock().output_error = Some(format!("no audio output: {e}"));
                false
            }
        }
    }

    /// Remember where the current track is, then detach it from the output.
    fn detach_current(&mut self) {
        let (pos, listened) = self.progress();
        if let Some(c) = self.current.as_mut() {
            c.base = pos;
            c.played_before = listened;
            c.active = false;
        }
        self.clear_source();
    }

    /// Take the source out of the output (dropping it outside the audio lock).
    fn clear_source(&mut self) {
        if let Some(o) = &self.output {
            let old = o.playback.lock().set_source(None, false);
            drop(old);
        }
    }

    /// Drop the current output (keeping the track and its position).
    fn release_output(&mut self, reason: &str) {
        self.detach_current();
        if let Some(o) = self.output.take() {
            log::warn!("audio output {}: {reason}", o.device_name);
            let _ = self.events.send(PlayerEvent::OutputLost {
                device: o.device_name.clone(),
                reason: reason.to_string(),
            });
        }
        self.publish_output();
        self.state.lock().output_error = Some(format!("audio output lost ({reason}); reconnecting"));
    }

    fn switch_output(&mut self, reason: &str) {
        log::info!("switching audio output: {reason}");
        self.detach_current();
        self.output = None;
        self.try_open_output();
    }

    fn check_device(&mut self) {
        let Some(o) = self.output.as_ref() else {
            if self.last_open_try.map(|t| t.elapsed() >= DEVICE_RETRY).unwrap_or(true) {
                self.try_open_output();
            }
            return;
        };
        // 1. The stream reported an error (device unplugged, driver reset...).
        if o.failed.load(Ordering::Relaxed) {
            let why = o.error.lock().clone().unwrap_or_else(|| "stream error".into());
            self.release_output(&why);
            self.try_open_output();
            return;
        }
        // 2. The device stopped pulling audio without saying so.
        let frames = o.frames.load(Ordering::Relaxed);
        if frames != self.last_frames {
            self.last_frames = frames;
            self.frames_moved_at = Instant::now();
        } else if self.frames_moved_at.elapsed() > STALL_LIMIT {
            self.release_output("stopped responding");
            self.try_open_output();
            return;
        }
        // 3. The device we should be on is a different one now.
        let current_name = o.device_name.clone();
        match &self.preferred {
            None => {
                if let Some(def) = output::default_device_name() {
                    if def != current_name {
                        self.switch_output(&format!("system default changed to {def}"));
                    }
                }
            }
            Some(pref) => {
                if !self.on_preferred && output::device_exists(pref) {
                    let pref = pref.clone();
                    self.switch_output(&format!("{pref} is available again"));
                }
            }
        }
    }

    // ------------------------------------------------------------ tracks

    fn target_format(&self) -> Option<OutputFormat> {
        self.output.as_ref().map(|o| o.format)
    }

    /// (position, listening time) of the current track right now.
    fn progress(&self) -> (Duration, Duration) {
        let Some(c) = self.current.as_ref() else { return (Duration::ZERO, Duration::ZERO) };
        match (&self.output, c.active) {
            (Some(o), true) => {
                let pb = o.playback.lock();
                let rate = o.format.sample_rate;
                (
                    c.base + frames_to_duration(pb.frames(), rate),
                    c.played_before + frames_to_duration(pb.listened(), rate),
                )
            }
            _ => (c.base, c.played_before),
        }
    }

    /// (Re)open the current track on the output at `at` (default: where it is now).
    fn restart_current(&mut self, at: Option<Duration>) {
        if self.current.is_none() {
            return;
        }
        let (now_pos, listened) = self.progress();
        let pos = at.unwrap_or(now_pos);
        let (path, paused) = {
            let c = self.current.as_mut().unwrap();
            c.base = pos;
            c.played_before = listened;
            c.active = false;
            (c.path.clone(), c.paused)
        };
        let Some(fmt) = self.target_format() else {
            self.clear_source();
            return;
        };
        // Open and seek outside the audio lock; only the swap happens under it.
        match open_source_for(&path, Some(fmt)) {
            Ok((mut src, duration)) => {
                if pos > Duration::ZERO {
                    if let Err(e) = src.try_seek(pos) {
                        log::warn!("seek to {pos:?} failed: {e}");
                    }
                }
                let old = {
                    let o = self.output.as_ref().unwrap();
                    let mut pb = o.playback.lock();
                    pb.set_volume(self.volume);
                    pb.set_source(Some(src), paused)
                };
                drop(old);
                if let Some(c) = self.current.as_mut() {
                    c.active = true;
                    if duration.is_some() {
                        c.duration = duration;
                    }
                }
                {
                    let mut s = self.state.lock();
                    s.position = pos;
                    s.position_at = Some(Instant::now());
                }
            }
            Err(e) => {
                self.clear_source();
                let path = self.current.take().map(|c| c.path);
                {
                    let mut s = self.state.lock();
                    s.loaded = false;
                    s.paused = false;
                }
                let _ = self.events.send(PlayerEvent::Error {
                    path,
                    message: format!("{e:#}"),
                });
            }
        }
    }

    fn handle(&mut self, cmd: Command) -> Result<()> {
        match cmd {
            Command::Load { path, paused } => {
                self.clear_source();
                self.current = None;
                // Probe first so a broken file is reported even with no device present.
                match open_source_for(&path, self.target_format()) {
                    Ok((probe, duration)) => {
                        drop(probe);
                        self.current = Some(Current {
                            path: path.clone(),
                            active: false,
                            base: Duration::ZERO,
                            duration,
                            paused,
                            played_before: Duration::ZERO,
                            ended_reported: false,
                        });
                        {
                            let mut s = self.state.lock();
                            s.path = Some(path);
                            s.loaded = true;
                            s.paused = paused;
                            s.position = Duration::ZERO;
                            s.position_at = Some(Instant::now());
                            s.duration = duration;
                            s.played_ms = 0;
                        }
                        if self.output.is_some() {
                            self.restart_current(Some(Duration::ZERO));
                        } else if self.last_open_try.map(|t| t.elapsed() >= DEVICE_RETRY).unwrap_or(true) {
                            self.try_open_output(); // starts the track if a device appears
                        }
                    }
                    Err(e) => {
                        {
                            let mut s = self.state.lock();
                            s.loaded = false;
                            s.path = Some(path.clone());
                        }
                        let _ = self.events.send(PlayerEvent::Error {
                            path: Some(path),
                            message: format!("{e:#}"),
                        });
                    }
                }
            }
            Command::Play => self.set_paused(false),
            Command::Pause => self.set_paused(true),
            Command::Toggle => {
                if let Some(p) = self.current.as_ref().map(|c| c.paused) {
                    self.set_paused(!p);
                }
            }
            Command::Stop => {
                self.clear_source();
                self.current = None;
                let mut s = self.state.lock();
                s.loaded = false;
                s.paused = false;
                s.position = Duration::ZERO;
                s.position_at = Some(Instant::now());
            }
            Command::Seek(pos) => {
                if self.current.is_some() {
                    let pos = match self.current.as_ref().and_then(|c| c.duration) {
                        Some(d) => pos.min(d),
                        None => pos,
                    };
                    self.restart_current(Some(pos));
                }
            }
            Command::SetVolume(v) => {
                self.volume = v;
                self.state.lock().volume = v;
                if let Some(o) = &self.output {
                    o.playback.lock().set_volume(v);
                }
            }
            Command::SetOutputDevice(dev) => {
                let dev = dev.filter(|d| !d.trim().is_empty());
                if dev != self.preferred || self.output.is_none() {
                    self.preferred = dev;
                    self.switch_output("output device changed in settings");
                }
            }
            Command::Quit => {}
        }
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) {
        let Some(c) = self.current.as_mut() else { return };
        c.paused = paused;
        if c.active {
            if let Some(o) = &self.output {
                o.playback.lock().set_paused(paused);
            }
        }
        self.state.lock().paused = paused;
    }

    /// Update position / listening time, detect end of track.
    fn tick(&mut self) {
        if self.current.is_none() {
            return;
        }
        let (pos, listened) = self.progress();
        let finished = match (&self.output, self.current.as_ref().map(|c| c.active)) {
            (Some(o), Some(true)) => o.playback.lock().finished(),
            _ => false,
        };
        let cur = self.current.as_mut().unwrap();
        {
            let mut s = self.state.lock();
            s.position = pos;
            s.position_at = Some(Instant::now());
            s.paused = cur.paused;
            s.played_ms = listened.as_millis() as u64;
        }
        if finished && !cur.ended_reported {
            cur.ended_reported = true;
            let path = cur.path.clone();
            {
                let mut s = self.state.lock();
                s.loaded = false;
                s.paused = false;
            }
            let _ = self.events.send(PlayerEvent::TrackEnded { path });
            self.current = None;
        }
    }
}

use playback::BoxedSource;

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// Open a decoder, turning a panic inside symphonia into an ordinary error.
fn try_open(
    source: Box<dyn symphonia::core::io::MediaSource>,
    ext: Option<&str>,
    target: Option<OutputFormat>,
) -> Result<SymphoniaSource> {
    match catch_unwind(AssertUnwindSafe(|| SymphoniaSource::new(source, ext, target))) {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(anyhow::Error::new(e)),
        Err(p) => Err(anyhow::anyhow!("decoder crashed: {}", panic_message(&p))),
    }
}

/// Open a file for playback in its own format (see [`open_source_for`]).
pub fn open_source(path: &Path) -> Result<(BoxedSource, Option<Duration>)> {
    open_source_for(path, None)
}

/// Open a file for playback, converted to `target` (a device format).  If
/// decoding fails and the file starts with an ID3v2 tag, try again with the
/// tag hidden: a malformed frame there makes symphonia give up on files whose
/// audio is perfectly fine.
pub fn open_source_for(path: &Path, target: Option<OutputFormat>) -> Result<(BoxedSource, Option<Duration>)> {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    // The length matters: symphonia's MP4 reader cannot open .m4a files without it.
    let len = file.metadata().ok().map(|m| m.len());
    let first_err = match try_open(Box::new(ReadSeekLen::new(file, len)), ext.as_deref(), target) {
        Ok(src) => {
            let duration = src.total_duration();
            return Ok((Box::new(src), duration));
        }
        Err(e) => e,
    };
    if let Some(skip) = SkipPrefix::open_without_id3v2(path)? {
        let skipped = skip.skipped();
        let len = skip.len();
        if let Ok(src) = try_open(Box::new(ReadSeekLen::new(skip, Some(len))), ext.as_deref(), target) {
            log::warn!(
                "{}: {first_err:#}; playing with its {skipped}-byte ID3v2 tag skipped",
                path.display()
            );
            let duration = src.total_duration();
            return Ok((Box::new(src), duration));
        }
    }
    Err(first_err).with_context(|| format!("decoding {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yerg_core::audio_io::test_support;

    #[test]
    fn position_now_runs_on_between_readings() {
        let read = |ago_ms: u64, paused: bool| PlayerState {
            loaded: true,
            paused,
            position: Duration::from_millis(1000),
            position_at: Some(Instant::now() - Duration::from_millis(ago_ms)),
            duration: Some(Duration::from_millis(1100)),
            ..Default::default()
        };
        // Playing: the time since the reading is added...
        let p = read(40, false).position_now().as_millis();
        assert!((1040..1060).contains(&p), "{p}");
        // ...but never more than a couple of engine ticks, nor past the end.
        assert_eq!(read(500, false).position_now(), Duration::from_millis(1100));
        let mut long = read(500, false);
        long.duration = None;
        assert_eq!(long.position_now(), Duration::from_millis(1000) + MAX_EXTRAPOLATION);
        // Paused or stopped: exactly the reading.
        assert_eq!(read(40, true).position_now(), Duration::from_millis(1000));
        let mut stopped = read(40, false);
        stopped.loaded = false;
        assert_eq!(stopped.position_now(), Duration::from_millis(1000));
    }

    #[test]
    fn plays_files_with_a_malformed_id3_frame() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.mp3");
        std::fs::write(&bad, test_support::mp3_with_malformed_wxxx()).unwrap();
        // The plain decoder refuses it...
        let direct = SymphoniaSource::new(Box::new(File::open(&bad).unwrap()), Some("mp3"), None);
        assert!(direct.is_err(), "symphonia unexpectedly accepted the malformed tag");
        // ...the fallback plays it, with the right length.
        let (source, duration) = open_source(&bad).unwrap();
        let samples = source.count();
        assert!(samples > 44_100, "only {samples} samples decoded");
        if let Some(d) = duration {
            assert!((d.as_secs_f64() - 2.0).abs() < 0.2, "duration {d:?}");
        }
    }

    #[test]
    fn saving_tags_keeps_the_file_playable() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.mp3");
        std::fs::write(&bad, test_support::mp3_with_malformed_wxxx()).unwrap();
        let mut tags = yerg_core::tags::read_file(&bad).unwrap().tags;
        assert_eq!(tags.title, "Tone");
        tags.title = "Edited".into();
        tags.artist = "Children of Bodom".into();
        yerg_core::tags::write_tags(&bad, &tags).unwrap();
        assert_eq!(yerg_core::tags::read_file(&bad).unwrap().tags.title, "Edited");
        let (source, _) = open_source(&bad).unwrap();
        assert!(source.count() > 44_100);
    }

    /// `.m4a` (AAC in MP4) with embedded cover art, like iTunes/streaming rips.
    /// rodio's own decoder panicked on these.
    #[test]
    fn plays_and_seeks_m4a() {
        let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/tone_cover.m4a"));
        let (mut src, duration) = open_source(path).unwrap();
        let d = duration.expect("m4a duration");
        assert!((d.as_secs_f64() - 3.0).abs() < 0.1, "duration {d:?}");
        assert_eq!(src.channels(), 2);
        assert_eq!(src.sample_rate(), 44_100);
        // Seek to 2 s: about 1 s of stereo audio should be left.
        src.try_seek(Duration::from_secs(2)).unwrap();
        let rest = src.count();
        let expect = 44_100 * 2;
        assert!((rest as i64 - expect as i64).abs() < 4_096, "{rest} samples after seeking");
        // Seeking past the end finishes the track instead of failing.
        let (mut src, _) = open_source(path).unwrap();
        src.try_seek(Duration::from_secs(10)).unwrap();
        assert_eq!(src.count(), 0);
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata")).join(name)
    }

    /// Decode a file for a device format and render it exactly as the audio
    /// callback does (in device-sized blocks), returning samples per channel.
    fn render_for_device(path: &std::path::Path, fmt: OutputFormat, seek: Option<Duration>) -> Vec<Vec<f32>> {
        let (mut src, _) = open_source_for(path, Some(fmt)).unwrap();
        assert_eq!(src.channels(), fmt.channels);
        assert_eq!(src.sample_rate(), fmt.sample_rate);
        if let Some(p) = seek {
            src.try_seek(p).unwrap();
        }
        let mut pb = playback::Playback::new(fmt.channels);
        pb.set_volume(1.0);
        pb.set_source(Some(src), false);
        let ch = fmt.channels as usize;
        let mut chans = vec![Vec::new(); ch];
        // Odd-sized blocks, like WASAPI/CoreAudio callbacks, to catch any framing slip.
        let mut block = vec![0.0f32; 441 * ch];
        while !pb.finished() {
            pb.render(&mut block);
            for (i, s) in block.iter().enumerate() {
                chans[i % ch].push(*s);
            }
            assert!(chans[0].len() < fmt.sample_rate as usize * 10, "never finished");
        }
        let last = chans.iter().filter_map(|c| c.iter().rposition(|s| s.abs() > 1e-4)).max().unwrap_or(0);
        for c in &mut chans {
            c.truncate(last + 1);
        }
        chans
    }

    fn max_jump(c: &[f32], margin: usize) -> f32 {
        if c.len() <= margin * 2 {
            return 0.0;
        }
        c[margin..c.len() - margin].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
    }

    fn crossings(c: &[f32]) -> usize {
        c.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn converts_to_common_device_formats() {
        let path = fixture("stereo_330L_550R.m4a");
        let cases = [
            OutputFormat { channels: 2, sample_rate: 48_000 }, // typical Windows/macOS mix format
            OutputFormat { channels: 2, sample_rate: 44_100 }, // no conversion at all
            OutputFormat { channels: 6, sample_rate: 96_000 }, // surround receiver / USB DAC
            OutputFormat { channels: 1, sample_rate: 16_000 }, // Bluetooth headset (hands-free profile)
            OutputFormat { channels: 2, sample_rate: 192_000 },
        ];
        for fmt in cases {
            let ch = render_for_device(&path, fmt, None);
            let secs = ch[0].len() as f64 / fmt.sample_rate as f64;
            assert!((secs - 3.0).abs() < 0.08, "{fmt:?}: {secs:.3} s of audio");
            // Pitch survives: count zero crossings over the middle second only
            // (the encoder's near-silent edges add spurious crossings).
            let mid = |c: &Vec<f32>| -> Vec<f32> {
                let r = fmt.sample_rate as usize;
                c[r..(2 * r).min(c.len())].to_vec()
            };
            let rate_hz = crossings(&mid(&ch[0])) as f64;
            if fmt.channels == 1 {
                // mono device: (330 + 550)/2 mix, dominated by neither; just check continuity
            } else {
                assert!((rate_hz - 330.0).abs() < 3.0, "{fmt:?}: left at {rate_hz:.1} Hz");
                let right = crossings(&mid(&ch[1])) as f64;
                assert!((right - 550.0).abs() < 3.0, "{fmt:?}: right at {right:.1} Hz");
            }
            // Smooth: a 550 Hz tone at 1/8 scale moves at most ~0.0098 per sample at 44.1 kHz,
            // less at higher rates; at 16 kHz up to ~0.027.
            let limit = if fmt.sample_rate < 30_000 { 0.06 } else { 0.03 };
            let margin = fmt.sample_rate as usize / 10;
            for (i, c) in ch.iter().enumerate().take(2) {
                let j = max_jump(c, margin);
                assert!(j < limit, "{fmt:?} channel {i}: jump {j}");
            }
            // Extra surround channels stay silent
            for c in ch.iter().skip(2) {
                assert!(c.iter().all(|s| *s == 0.0), "{fmt:?}: surround channel not silent");
            }
        }
    }

    /// A 5.1 FLAC (front left 330 Hz, front right 550 Hz, the rest silent) on a
    /// 44.1 kHz stereo device: the fronts stay on their sides and nothing clips.
    /// (LFE exclusion and centre/surround weights are covered in `decode::tests`.)
    #[test]
    fn surround_file_downmixes_for_a_stereo_device() {
        let fmt = OutputFormat { channels: 2, sample_rate: 44_100 };
        let ch = render_for_device(&fixture("surround_5_1.flac"), fmt, None);
        let secs = ch[0].len() as f64 / 44_100.0;
        assert!((secs - 1.0).abs() < 0.05, "{secs:.3} s");
        let mid = |c: &Vec<f32>| c[11_025..33_075].to_vec(); // middle half second
        let l = crossings(&mid(&ch[0])) as f64 * 2.0;
        let r = crossings(&mid(&ch[1])) as f64 * 2.0;
        assert!((l - 330.0).abs() < 6.0, "left {l} Hz");
        assert!((r - 550.0).abs() < 6.0, "right {r} Hz");
        let peak = ch.iter().flatten().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak <= 1.0, "clipped: {peak}");
        assert!(max_jump(&ch[0], 2_000) < 0.03);
    }

    #[test]
    fn seeking_is_accurate_after_resampling() {
        let fmt = OutputFormat { channels: 2, sample_rate: 48_000 };
        let ch = render_for_device(&fixture("stereo_330L_550R.mp3"), fmt, Some(Duration::from_secs(2)));
        let secs = ch[0].len() as f64 / 48_000.0;
        assert!((secs - 1.0).abs() < 0.08, "{secs:.3} s left after seeking to 2 s");
        assert!(max_jump(&ch[0], 4_800) < 0.03);
    }

    #[test]
    fn untagged_and_unreadable_files() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok.mp3");
        std::fs::write(&ok, test_support::plain_mp3()).unwrap();
        assert!(open_source(&ok).is_ok());
        let junk = dir.path().join("junk.mp3");
        std::fs::write(&junk, b"not audio at all").unwrap();
        assert!(open_source(&junk).is_err());
    }
}
