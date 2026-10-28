//! The background scrobbler: drains pending plays from the library whenever
//! a service is configured and reachable, and relays "now playing" updates.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use yerg_core::model::{ScrobbleService, ScrobbleState};
use yerg_core::settings::keys;
use yerg_core::Library;

use crate::lastfm::LastFm;
use crate::listenbrainz::ListenBrainz;
use crate::{Outcome, ScrobbleItem, Service};

/// Scrobbler configuration as stored in library settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScrobbleConfig {
    pub enabled: bool,
    pub now_playing: bool,
    pub lastfm_enabled: bool,
    pub lastfm_api_key: String,
    pub lastfm_api_secret: String,
    pub lastfm_session_key: String,
    pub lastfm_username: String,
    pub listenbrainz_enabled: bool,
    pub listenbrainz_token: String,
    pub listenbrainz_username: String,
    /// Libre.fm needs no API account: just a session from browser approval.
    pub librefm_enabled: bool,
    pub librefm_session_key: String,
    pub librefm_username: String,
}

impl ScrobbleConfig {
    pub fn load(lib: &Library) -> ScrobbleConfig {
        ScrobbleConfig {
            enabled: lib.setting_bool(keys::SCROBBLE_ENABLED, true),
            now_playing: lib.setting_bool(keys::SCROBBLE_NOW_PLAYING, true),
            lastfm_enabled: lib.setting_bool(keys::LASTFM_ENABLED, false),
            lastfm_api_key: lib.setting_string(keys::LASTFM_API_KEY),
            lastfm_api_secret: lib.setting_string(keys::LASTFM_API_SECRET),
            lastfm_session_key: lib.setting_string(keys::LASTFM_SESSION_KEY),
            lastfm_username: lib.setting_string(keys::LASTFM_USERNAME),
            listenbrainz_enabled: lib.setting_bool(keys::LISTENBRAINZ_ENABLED, false),
            listenbrainz_token: lib.setting_string(keys::LISTENBRAINZ_TOKEN),
            listenbrainz_username: lib.setting_string(keys::LISTENBRAINZ_USERNAME),
            librefm_enabled: lib.setting_bool(keys::LIBREFM_ENABLED, false),
            librefm_session_key: lib.setting_string(keys::LIBREFM_SESSION_KEY),
            librefm_username: lib.setting_string(keys::LIBREFM_USERNAME),
        }
    }

    pub fn save(&self, lib: &Library) -> Result<()> {
        lib.set_setting_bool(keys::SCROBBLE_ENABLED, self.enabled)?;
        lib.set_setting_bool(keys::SCROBBLE_NOW_PLAYING, self.now_playing)?;
        lib.set_setting_bool(keys::LASTFM_ENABLED, self.lastfm_enabled)?;
        lib.set_setting(keys::LASTFM_API_KEY, self.lastfm_api_key.trim())?;
        lib.set_setting(keys::LASTFM_API_SECRET, self.lastfm_api_secret.trim())?;
        lib.set_setting(keys::LASTFM_SESSION_KEY, self.lastfm_session_key.trim())?;
        lib.set_setting(keys::LASTFM_USERNAME, self.lastfm_username.trim())?;
        lib.set_setting_bool(keys::LISTENBRAINZ_ENABLED, self.listenbrainz_enabled)?;
        lib.set_setting(keys::LISTENBRAINZ_TOKEN, self.listenbrainz_token.trim())?;
        lib.set_setting(keys::LISTENBRAINZ_USERNAME, self.listenbrainz_username.trim())?;
        lib.set_setting_bool(keys::LIBREFM_ENABLED, self.librefm_enabled)?;
        lib.set_setting(keys::LIBREFM_SESSION_KEY, self.librefm_session_key.trim())?;
        lib.set_setting(keys::LIBREFM_USERNAME, self.librefm_username.trim())?;
        Ok(())
    }

    pub fn lastfm_ready(&self) -> bool {
        self.lastfm_enabled
            && !self.lastfm_api_key.is_empty()
            && !self.lastfm_api_secret.is_empty()
            && !self.lastfm_session_key.is_empty()
    }

    pub fn listenbrainz_ready(&self) -> bool {
        self.listenbrainz_enabled && !self.listenbrainz_token.is_empty()
    }

    pub fn librefm_ready(&self) -> bool {
        self.librefm_enabled && !self.librefm_session_key.is_empty()
    }

    /// Whether `which` is set up to take listens (scrobbling as a whole may
    /// still be switched off).  Listens for services that are not are kept but
    /// not shown as waiting.
    pub fn ready(&self, which: ScrobbleService) -> bool {
        match which {
            ScrobbleService::LastFm => self.lastfm_ready(),
            ScrobbleService::ListenBrainz => self.listenbrainz_ready(),
            ScrobbleService::LibreFm => self.librefm_ready(),
        }
    }

    /// The services that are set up, in display order.
    pub fn configured(&self) -> Vec<ScrobbleService> {
        ScrobbleService::ALL.into_iter().filter(|s| self.ready(*s)).collect()
    }

    /// Any service that will take listens right now.
    pub fn any_ready(&self) -> bool {
        self.enabled && !self.configured().is_empty()
    }

    pub fn service(&self, which: ScrobbleService) -> Option<Box<dyn Service>> {
        match which {
            ScrobbleService::LibreFm if self.librefm_ready() => {
                Some(Box::new(LastFm::librefm(self.librefm_session_key.clone())))
            }
            ScrobbleService::LastFm if self.lastfm_ready() => Some(Box::new(LastFm {
                network: crate::lastfm::Network::LastFm,
                api_key: self.lastfm_api_key.clone(),
                api_secret: self.lastfm_api_secret.clone(),
                session_key: self.lastfm_session_key.clone(),
            })),
            ScrobbleService::ListenBrainz if self.listenbrainz_ready() => {
                Some(Box::new(ListenBrainz::new(self.listenbrainz_token.clone())))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FlushReport {
    pub submitted: usize,
    pub ignored: usize,
    pub errors: Vec<String>,
}

/// Submit everything pending for every configured service.  Safe to call from
/// anywhere; the worker thread calls it periodically.
pub fn flush_pending(lib: &Library, cfg: &ScrobbleConfig) -> FlushReport {
    let mut report = FlushReport::default();
    if !cfg.enabled {
        return report;
    }
    for which in ScrobbleService::ALL {
        let Some(service) = cfg.service(which) else { continue };
        loop {
            let pending = match lib.pending_scrobbles(which, service.batch_size()) {
                Ok(p) => p,
                Err(e) => {
                    report.errors.push(format!("{}: {e}", service.name()));
                    break;
                }
            };
            if pending.is_empty() {
                break;
            }
            let mut items: Vec<ScrobbleItem> = Vec::new();
            for p in &pending {
                let item = ScrobbleItem::from(p);
                if item.is_submittable() {
                    items.push(item);
                } else {
                    let _ = lib.set_scrobble_state(
                        p.play.id,
                        which,
                        ScrobbleState::Skipped,
                        Some("missing artist or title"),
                    );
                }
            }
            if items.is_empty() {
                continue;
            }
            match service.scrobble(&items) {
                Ok(outcomes) => {
                    for (item, outcome) in items.iter().zip(outcomes) {
                        let (state, note) = match outcome {
                            Outcome::Accepted => {
                                report.submitted += 1;
                                (ScrobbleState::Done, None)
                            }
                            Outcome::Ignored(reason) => {
                                report.ignored += 1;
                                (ScrobbleState::Ignored, Some(reason))
                            }
                        };
                        if let Err(e) = lib.set_scrobble_state(item.play_id, which, state, note.as_deref()) {
                            report.errors.push(format!("db: {e}"));
                        }
                    }
                }
                Err(e) => {
                    if !e.is_retryable() {
                        // Bad credentials: leave listens pending, surface the error.
                        report.errors.push(format!("{}: {e}", service.name()));
                    } else {
                        report.errors.push(format!("{}: {e} (will retry)", service.name()));
                    }
                    break;
                }
            }
        }
    }
    report
}

#[derive(Debug, Clone)]
pub enum WorkerMsg {
    /// Try to flush now (a play was just recorded, the network came back...).
    Flush,
    NowPlaying(ScrobbleItem),
    Quit,
}

#[derive(Debug, Clone, Default)]
pub struct WorkerStatus {
    pub last_flush: Option<i64>,
    pub last_report: Option<FlushReport>,
    pub last_error: Option<String>,
    pub busy: bool,
}

pub struct ScrobbleWorker {
    tx: Sender<WorkerMsg>,
    status: Arc<Mutex<WorkerStatus>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Poison-tolerant wrapper around `std::sync::Mutex`.
pub struct Mutex<T>(std::sync::Mutex<T>);
impl<T> Mutex<T> {
    pub fn new(v: T) -> Self {
        Mutex(std::sync::Mutex::new(v))
    }
    pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl ScrobbleWorker {
    /// Start the worker on its own database connection.
    pub fn start(db_path: PathBuf, interval: Duration) -> ScrobbleWorker {
        let (tx, rx) = unbounded();
        let status = Arc::new(Mutex::new(WorkerStatus::default()));
        let st = status.clone();
        let thread = std::thread::Builder::new()
            .name("yerg-scrobbler".into())
            .spawn(move || worker_main(db_path, rx, st, interval))
            .expect("spawn scrobble worker");
        ScrobbleWorker {
            tx,
            status,
            thread: Some(thread),
        }
    }

    pub fn flush(&self) {
        let _ = self.tx.send(WorkerMsg::Flush);
    }

    pub fn now_playing(&self, item: ScrobbleItem) {
        let _ = self.tx.send(WorkerMsg::NowPlaying(item));
    }

    pub fn status(&self) -> WorkerStatus {
        self.status.lock().clone()
    }
}

impl Drop for ScrobbleWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(WorkerMsg::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn worker_main(
    db_path: PathBuf,
    rx: Receiver<WorkerMsg>,
    status: Arc<Mutex<WorkerStatus>>,
    interval: Duration,
) {
    let lib = match Library::open(&db_path) {
        Ok(l) => l,
        Err(e) => {
            status.lock().last_error = Some(format!("scrobbler cannot open library: {e}"));
            return;
        }
    };
    // First flush shortly after start-up so offline listens go out quickly.
    let mut next_wait = Duration::from_secs(5);
    loop {
        match rx.recv_timeout(next_wait) {
            Ok(WorkerMsg::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(WorkerMsg::NowPlaying(item)) => {
                let cfg = ScrobbleConfig::load(&lib);
                if cfg.enabled && cfg.now_playing && item.is_submittable() {
                    for which in ScrobbleService::ALL {
                        if let Some(s) = cfg.service(which) {
                            if let Err(e) = s.now_playing(&item) {
                                log::info!("now playing to {}: {e}", s.name());
                            }
                        }
                    }
                }
                next_wait = Duration::from_secs(1);
                continue;
            }
            Ok(WorkerMsg::Flush) | Err(RecvTimeoutError::Timeout) => {}
        }
        status.lock().busy = true;
        let cfg = ScrobbleConfig::load(&lib);
        let report = flush_pending(&lib, &cfg);
        {
            let mut s = status.lock();
            s.busy = false;
            s.last_flush = Some(yerg_core::now_unix());
            s.last_error = report.errors.first().cloned();
            s.last_report = Some(report);
        }
        next_wait = interval;
    }
}
