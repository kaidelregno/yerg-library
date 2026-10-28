//! Background work.  Every job opens its own library connection and reports
//! back over a channel that the UI drains each frame.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use yerg_core::model::*;
use yerg_core::scan::ScanEvent;
use yerg_core::Library;
use yerg_scrobble::lastfm::Network;
use yerg_scrobble::worker::{flush_pending, FlushReport, ScrobbleConfig};
use yerg_sync::device::{self, DeviceScrobblePolicy, ImportReport, SyncEvent, SyncPlan, SyncReport};

pub enum JobEvent {
    Scan(ScanEvent),
    SyncPlanned(Result<SyncPlan, String>),
    Sync(SyncEvent),
    SyncDone(Result<SyncReport, String>),
    Imported(Result<ImportReport, String>),
    /// Desktop auth for Last.fm or Libre.fm: (token, approval URL).
    LastFmToken(Network, Result<(String, String), String>),
    /// (session key, username) once approved.
    LastFmSession(Network, Result<(String, String), String>),
    LbzValidated(Result<String, String>),
    Flushed(FlushReport),
    TagsSaved(Result<Vec<TrackId>, String>),
    PlaylistImported(Result<(PlaylistId, usize, Vec<String>), String>),
    LibraryChanged,
    Toast(String),
    Error(String),
}

pub struct Jobs {
    db: PathBuf,
    tx: Sender<JobEvent>,
    rx: Receiver<JobEvent>,
    ctx: egui::Context,
    pub scanning: bool,
    pub syncing: bool,
    pub planning: bool,
    pub importing: bool,
    pub sync_cancel: Arc<AtomicBool>,
    lastfm_wait_cancel: Arc<AtomicBool>,
}

impl Jobs {
    pub fn new(db: PathBuf, ctx: egui::Context) -> Jobs {
        let (tx, rx) = unbounded();
        Jobs {
            db,
            tx,
            rx,
            ctx,
            scanning: false,
            syncing: false,
            planning: false,
            importing: false,
            sync_cancel: Arc::new(AtomicBool::new(false)),
            lastfm_wait_cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn poll(&mut self) -> Vec<JobEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.rx.try_recv() {
            match &e {
                JobEvent::Scan(ScanEvent::Finished(_)) => self.scanning = false,
                JobEvent::SyncPlanned(_) => self.planning = false,
                JobEvent::SyncDone(_) => self.syncing = false,
                JobEvent::Imported(_) => self.importing = false,
                _ => {}
            }
            out.push(e);
        }
        out
    }

    fn spawn<F>(&self, name: &'static str, f: F)
    where
        F: FnOnce(&Library, &Sender<JobEvent>) -> Result<()> + Send + 'static,
    {
        let tx = self.tx.clone();
        let db = self.db.clone();
        let ctx = self.ctx.clone();
        let _ = std::thread::Builder::new().name(format!("yerg-{name}")).spawn(move || {
            match Library::open(&db) {
                Ok(lib) => {
                    if let Err(e) = f(&lib, &tx) {
                        let _ = tx.send(JobEvent::Error(format!("{name}: {e:#}")));
                    }
                }
                Err(e) => {
                    let _ = tx.send(JobEvent::Error(format!("{name}: cannot open library: {e:#}")));
                }
            }
            ctx.request_repaint();
        });
    }

    pub fn scan(&mut self, full: bool) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let ctx = self.ctx.clone();
        self.spawn("scan", move |lib, tx| {
            let (stx, srx) = unbounded::<ScanEvent>();
            let tx2 = tx.clone();
            let relay = std::thread::spawn(move || {
                let mut n = 0usize;
                for ev in srx {
                    n += 1;
                    let finished = matches!(ev, ScanEvent::Finished(_));
                    let _ = tx2.send(JobEvent::Scan(ev));
                    if n % 10 == 0 || finished {
                        ctx.request_repaint();
                    }
                }
            });
            let res = yerg_core::scan::scan_library(lib, Some(&stx), full);
            drop(stx);
            let _ = relay.join();
            match res {
                Ok(_) => {
                    let _ = tx.send(JobEvent::LibraryChanged);
                    Ok(())
                }
                Err(e) => {
                    // Make sure the UI leaves the "scanning" state.
                    let _ = tx.send(JobEvent::Scan(ScanEvent::Finished(Default::default())));
                    Err(e)
                }
            }
        });
    }

    pub fn plan_sync(&mut self, device: Device) {
        self.planning = true;
        self.spawn("plan", move |lib, tx| {
            let r = device::plan_sync(lib, &device).map_err(|e| format!("{e:#}"));
            let _ = tx.send(JobEvent::SyncPlanned(r));
            Ok(())
        });
    }

    pub fn run_sync(&mut self, device: Device, plan: SyncPlan) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        self.sync_cancel.store(false, Ordering::Relaxed);
        let cancel = self.sync_cancel.clone();
        let ctx = self.ctx.clone();
        self.spawn("sync", move |lib, tx| {
            let (stx, srx) = unbounded::<SyncEvent>();
            let tx2 = tx.clone();
            let relay = std::thread::spawn(move || {
                for ev in srx {
                    let _ = tx2.send(JobEvent::Sync(ev));
                    ctx.request_repaint();
                }
            });
            let r = device::run_sync(lib, &device, &plan, Some(&stx), &cancel).map_err(|e| format!("{e:#}"));
            drop(stx);
            let _ = relay.join();
            let _ = tx.send(JobEvent::SyncDone(r));
            Ok(())
        });
    }

    pub fn import_listens(&mut self, device: Device, policy: DeviceScrobblePolicy) {
        self.importing = true;
        self.spawn("import", move |lib, tx| {
            let r = device::import_listens(lib, &device, policy).map_err(|e| format!("{e:#}"));
            let ok = r.is_ok();
            let _ = tx.send(JobEvent::Imported(r));
            if ok {
                let _ = tx.send(JobEvent::LibraryChanged);
            }
            Ok(())
        });
    }

    /// Step 1 of connecting Last.fm or Libre.fm (same protocol).  For Libre.fm
    /// the key and secret are the app's placeholders.
    pub fn lastfm_token(&self, network: Network, api_key: String, api_secret: String) {
        self.spawn("lastfm-token", move |_lib, tx| {
            let r = yerg_scrobble::lastfm::get_token(network, &api_key, &api_secret)
                .map(|t| {
                    let url = yerg_scrobble::lastfm::auth_url(network, &api_key, &t);
                    (t, url)
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(JobEvent::LastFmToken(network, r));
            Ok(())
        });
    }

    /// Exchange the request token for a session key.  With `wait`, keep asking
    /// every few seconds until the user approves the app in the browser (up to
    /// ten minutes, or until [`cancel_lastfm_wait`](Self::cancel_lastfm_wait)).
    pub fn lastfm_session(&mut self, network: Network, api_key: String, api_secret: String, token: String, wait: bool) {
        // A new attempt supersedes any earlier wait.
        self.lastfm_wait_cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.lastfm_wait_cancel = cancel.clone();
        let ctx = self.ctx.clone();
        self.spawn("lastfm-session", move |lib, tx| {
            let started = std::time::Instant::now();
            let name = network.name();
            let r = loop {
                match yerg_scrobble::lastfm::get_session(network, &api_key, &api_secret, &token) {
                    Ok(v) => break Ok(v),
                    Err(yerg_scrobble::ScrobbleError::AwaitingApproval) if wait => {}
                    Err(yerg_scrobble::ScrobbleError::AwaitingApproval) => {
                        break Err(format!("{name} says the app has not been approved yet. Approve it in the browser and try again."))
                    }
                    Err(e) => break Err(e.to_string()),
                }
                if started.elapsed() > std::time::Duration::from_secs(600) {
                    break Err(format!("Timed out waiting for approval on {name}. Click Connect to try again."));
                }
                // Sleep in short steps so Cancel takes effect quickly.
                for _ in 0..15 {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            };
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            if let Ok((sk, user)) = &r {
                let mut cfg = ScrobbleConfig::load(lib);
                match network {
                    Network::LastFm => {
                        cfg.lastfm_api_key = api_key;
                        cfg.lastfm_api_secret = api_secret;
                        cfg.lastfm_session_key = sk.clone();
                        cfg.lastfm_username = user.clone();
                        cfg.lastfm_enabled = true;
                    }
                    Network::LibreFm => {
                        cfg.librefm_session_key = sk.clone();
                        cfg.librefm_username = user.clone();
                        cfg.librefm_enabled = true;
                    }
                }
                cfg.save(lib)?;
            }
            let _ = tx.send(JobEvent::LastFmSession(network, r));
            ctx.request_repaint();
            Ok(())
        });
    }

    pub fn cancel_lastfm_wait(&self) {
        self.lastfm_wait_cancel.store(true, Ordering::Relaxed);
    }

    pub fn lbz_validate(&self, token: String) {
        self.spawn("lbz-validate", move |lib, tx| {
            let lb = yerg_scrobble::listenbrainz::ListenBrainz::new(token.clone());
            let r = lb.validate_token().map_err(|e| e.to_string());
            if let Ok(user) = &r {
                let mut cfg = ScrobbleConfig::load(lib);
                cfg.listenbrainz_token = token;
                cfg.listenbrainz_username = user.clone();
                cfg.listenbrainz_enabled = true;
                cfg.save(lib)?;
            }
            let _ = tx.send(JobEvent::LbzValidated(r));
            Ok(())
        });
    }

    pub fn flush_scrobbles(&self) {
        self.spawn("flush", move |lib, tx| {
            let cfg = ScrobbleConfig::load(lib);
            let r = flush_pending(lib, &cfg);
            let _ = tx.send(JobEvent::Flushed(r));
            Ok(())
        });
    }

    /// Save edited tags to the database and (optionally) the files.
    /// `fields` limits which fields apply when several tracks are edited.
    pub fn save_tags(&self, ids: Vec<TrackId>, data: TagData, fields: Option<Vec<&'static str>>, write_files: bool) {
        self.spawn("save-tags", move |lib, tx| {
            let mut errors = Vec::new();
            for id in &ids {
                let Some(track) = lib.track(*id)? else { continue };
                let merged = match &fields {
                    None => data.clone(),
                    Some(f) => {
                        let mut t = track.to_tag_data(None);
                        for field in f {
                            match *field {
                                "artist" => t.artist = data.artist.clone(),
                                "album_artist" => t.album_artist = data.album_artist.clone(),
                                "album" => t.album = data.album.clone(),
                                "genre" => t.genre = data.genre.clone(),
                                "composer" => t.composer = data.composer.clone(),
                                "year" => t.year = data.year,
                                "disc_no" => t.disc_no = data.disc_no,
                                "disc_total" => t.disc_total = data.disc_total,
                                "track_total" => t.track_total = data.track_total,
                                "compilation" => t.compilation = data.compilation,
                                "comment" => t.comment = data.comment.clone(),
                                _ => {}
                            }
                        }
                        t
                    }
                };
                if write_files {
                    let path = std::path::Path::new(&track.path);
                    match yerg_core::tags::write_tags(path, &merged) {
                        Ok(()) => {
                            if let Ok(meta) = std::fs::metadata(path) {
                                let mtime = meta
                                    .modified()
                                    .ok()
                                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(track.mtime);
                                let _ = lib.set_file_stat(*id, mtime, meta.len() as i64);
                            }
                        }
                        Err(e) => errors.push(format!("{}: {e:#}", track.path)),
                    }
                }
                lib.update_track_tags(*id, &merged)?;
                if fields.is_none() {
                    if let Some(l) = &merged.lyrics {
                        lib.set_lyrics(*id, if l.trim().is_empty() { None } else { Some(l) })?;
                    }
                }
            }
            if errors.is_empty() {
                let _ = tx.send(JobEvent::TagsSaved(Ok(ids)));
            } else {
                let _ = tx.send(JobEvent::TagsSaved(Err(errors.join("\n"))));
            }
            let _ = tx.send(JobEvent::LibraryChanged);
            Ok(())
        });
    }

    pub fn set_art(&self, ids: Vec<TrackId>, picture: Option<(String, Vec<u8>)>, write_files: bool) {
        self.spawn("set-art", move |lib, tx| {
            let hash = match &picture {
                Some((mime, data)) => Some(lib.store_art(mime, data)?),
                None => None,
            };
            let mut errors = Vec::new();
            for id in &ids {
                let Some(track) = lib.track(*id)? else { continue };
                lib.set_track_art(*id, hash.as_deref())?;
                if write_files {
                    let pic = picture.as_ref().map(|(mime, data)| EmbeddedPicture {
                        mime: mime.clone(),
                        data: data.clone(),
                    });
                    if let Err(e) = yerg_core::tags::write_picture(std::path::Path::new(&track.path), pic.as_ref()) {
                        errors.push(format!("{}: {e:#}", track.path));
                    }
                }
            }
            let _ = tx.send(if errors.is_empty() {
                JobEvent::Toast("Artwork updated".into())
            } else {
                JobEvent::Error(errors.join("\n"))
            });
            let _ = tx.send(JobEvent::LibraryChanged);
            Ok(())
        });
    }

    pub fn import_playlist(&self, path: PathBuf) {
        self.spawn("import-playlist", move |lib, tx| {
            let r = lib.import_m3u(&path, None).map_err(|e| format!("{e:#}"));
            let _ = tx.send(JobEvent::PlaylistImported(r));
            Ok(())
        });
    }

    pub fn export_playlist(&self, id: PlaylistId, path: PathBuf) {
        self.spawn("export-playlist", move |lib, tx| {
            let n = lib.export_m3u(id, &path)?;
            let _ = tx.send(JobEvent::Toast(format!("Exported {n} tracks to {}", path.display())));
            Ok(())
        });
    }

    pub fn remove_tracks(&self, ids: Vec<TrackId>) {
        self.spawn("remove-tracks", move |lib, tx| {
            for id in &ids {
                lib.delete_track(*id)?;
            }
            let _ = tx.send(JobEvent::Toast(format!("Removed {} tracks from the library", ids.len())));
            let _ = tx.send(JobEvent::LibraryChanged);
            Ok(())
        });
    }

    pub fn purge_missing(&self) {
        self.spawn("purge-missing", move |lib, tx| {
            let n = lib.purge_missing()?;
            let _ = tx.send(JobEvent::Toast(format!("Removed {n} missing tracks")));
            let _ = tx.send(JobEvent::LibraryChanged);
            Ok(())
        });
    }
}
