//! Application state and the per-frame update loop.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use egui::Context;
use serde::{Deserialize, Serialize};
use yerg_core::model::*;
use yerg_core::query::{AlbumSort, ArtistMode, Sort, SortKey, TrackFilter};
use yerg_core::scan::ScanEvent;
use yerg_core::settings::keys;
use yerg_core::Library;
use yerg_player::Player;
use yerg_scrobble::lastfm::{Network, LIBREFM_API_KEY, LIBREFM_API_SECRET};
use yerg_scrobble::worker::{ScrobbleConfig, ScrobbleWorker};
use yerg_sync::device::{DeviceScrobblePolicy, SyncEvent};

use crate::artcache::ArtCache;
use crate::jobs::{JobEvent, Jobs};
use crate::playback::NowPlaying;
use crate::queue::{Queue, Repeat};
use crate::views::device::DeviceUi;
use crate::views::queue_panel::QueuePanelCache;
use crate::views::settings::SettingsUi;
use crate::views::tag_editor::TagEditor;
use crate::views::track_table::{Col, TableState};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum View {
    Songs,
    Artists,
    AlbumArtists,
    Albums,
    Genres,
    RecentlyAdded,
    RecentlyPlayed,
    MostPlayed,
    Playlist(PlaylistId),
    History,
    Device,
}

impl View {
    pub fn title(&self) -> &'static str {
        match self {
            View::Songs => "Songs",
            View::Artists => "Artists",
            View::AlbumArtists => "Album Artists",
            View::Albums => "Albums",
            View::Genres => "Genres",
            View::RecentlyAdded => "Recently Added",
            View::RecentlyPlayed => "Recently Played",
            View::MostPlayed => "Most Played",
            View::Playlist(_) => "Playlist",
            View::History => "History",
            View::Device => "Yerg MP3 Player",
        }
    }
}

/// Things the views ask the app to do; applied after the UI closures return.
pub enum Action {
    PlayIndex(usize),
    PlayNext(Vec<TrackId>),
    AddToQueue(Vec<TrackId>),
    AddToPlaylist(PlaylistId, Vec<TrackId>),
    NewPlaylistWith(Vec<TrackId>),
    RemoveFromPlaylist(Vec<usize>),
    Rate(Vec<TrackId>, i32),
    EditTags(Vec<TrackId>),
    ShowInFolder(TrackId),
    RemoveFromLibrary(Vec<TrackId>),
    Sort(SortKey),
    ResetSort,
    OpenArtist(String),
    OpenAlbumArtist(String),
    OpenAlbum(AlbumKey),
    ToggleColumn(Col),
    RemoveQueue(usize),
    /// Play an entry of the Up Next list now (index into `Queue::upcoming`).
    JumpQueue(usize),
    /// Drag and drop in Up Next: move entry `from` before entry `to`.
    MoveQueue(usize, usize),
    ClearQueue,
    Seek(f64),
    SetVolume(f32),
}

pub struct Prompt {
    pub title: String,
    pub value: String,
    pub kind: PromptKind,
}

pub enum PromptKind {
    NewPlaylist { add: Vec<TrackId> },
    RenamePlaylist(PlaylistId),
    RenameDevice(String),
}

pub struct Confirm {
    pub title: String,
    pub body: String,
    pub kind: ConfirmKind,
}

pub enum ConfirmKind {
    DeletePlaylist(PlaylistId),
    RemoveTracks(Vec<TrackId>),
    WipeDevice(String),
    ForgetDevice(String),
    RunSync,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UiState {
    view: View,
    show_now_playing: bool,
    #[serde(default)]
    show_queue: bool,
    /// The right-panel toggle brings back Up Next rather than Now Playing.
    #[serde(default)]
    right_last_queue: bool,
    #[serde(default = "default_true")]
    show_sidebar: bool,
    columns: Vec<Col>,
    sort: Option<Sort>,
    album_sort: AlbumSort,
}

fn default_true() -> bool {
    true
}

/// The panel on the right of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightPanel {
    Hidden,
    NowPlaying,
    Queue,
}

pub struct YergApp {
    pub lib: Library,
    pub db_path: PathBuf,
    pub player: Player,
    pub scrobbler: ScrobbleWorker,
    pub scrobble_cfg: ScrobbleConfig,
    pub jobs: Jobs,
    pub art: ArtCache,
    pub queue: Queue,
    pub now: Option<NowPlaying>,
    pub lyrics_line: Option<usize>,
    pub consecutive_errors: u32,

    pub view: View,
    pub search: String,
    pub sort: Option<Sort>,
    pub album_sort: AlbumSort,
    pub tracks: Vec<Track>,
    pub tracks_dirty: bool,
    pub lists_dirty: bool,
    pub counts_dirty: bool,
    pub history_dirty: bool,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
    pub genres: Vec<GenreSummary>,
    pub playlists: Vec<Playlist>,
    pub stats: LibraryStats,
    pub history: Vec<PlayWithTrack>,
    pub sel_artist: Option<String>,
    pub sel_album: Option<AlbumKey>,
    pub sel_genre: Option<String>,
    pub table: TableState,

    /// Which panel is open on the right, if any.
    pub right_panel: RightPanel,
    /// What the right-panel toggle brings back when it is hidden (never `Hidden`).
    pub right_panel_last: RightPanel,
    /// The left sidebar (library, playlists).
    pub show_sidebar: bool,
    /// Artists of the track whose right-click menu is open (see `track_table`).
    pub menu_artists: Option<(TrackId, Vec<String>)>,
    /// Volume to go back to when the speaker icon unmutes.
    pub volume_before_mute: f32,
    /// Problems in the appearance dotfiles (Settings › Appearance lists them).
    pub theme_errors: Vec<String>,
    pub appearance: crate::views::appearance::AppearanceUi,
    pub queue_panel: QueuePanelCache,
    pub settings_open: bool,
    pub settings: SettingsUi,
    pub tag_editor: Option<TagEditor>,
    pub prompt: Option<Prompt>,
    pub confirm: Option<Confirm>,
    pub toasts: Vec<(String, Instant)>,
    pub scan_progress: Option<(usize, usize, String)>,
    pub last_scan: Option<String>,
    pub device_ui: DeviceUi,
    pub write_tags_to_files: bool,
    pub focus_search: bool,
    pub seek_preview: Option<f64>,
    pub lyrics_scroll_pending: bool,
    pub output_was_lost: bool,
    pub frame_count: u64,
    pub autoscroll: Option<AutoScroll>,
}

/// The MP3 player page (sync, listen import) and the per-playlist
/// "Sync to MP3 player" option. Hidden until the player is ready; the code and
/// the `yerg device` commands still work. `YERG_DEBUG_VIEW=device` opens it.
pub const SHOW_DEVICE: bool = false;

/// Debug aid: `YERG_DEBUG_AUTOSCROLL=<px per second>` scrolls the track table
/// continuously and logs the frame rate every 5 seconds (for performance
/// measurements; the Slint prototype has the same hook).
pub struct AutoScroll {
    pub start: Instant,
    pub speed: f32,
    frames: u64,
    reported_at: Instant,
    reported_frames: u64,
}

impl AutoScroll {
    fn from_env() -> Option<AutoScroll> {
        let speed = std::env::var("YERG_DEBUG_AUTOSCROLL").ok()?.parse::<f32>().ok()?;
        let now = Instant::now();
        Some(AutoScroll {
            start: now,
            speed,
            frames: 0,
            reported_at: now,
            reported_frames: 0,
        })
    }

    fn tick(&mut self, ctx: &Context) {
        self.frames += 1;
        let since = self.reported_at.elapsed();
        if since >= Duration::from_secs(5) {
            let fps = (self.frames - self.reported_frames) as f64 / since.as_secs_f64();
            log::info!("autoscroll: {fps:.1} fps");
            self.reported_at = Instant::now();
            self.reported_frames = self.frames;
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

impl YergApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<YergApp> {
        let db_path = yerg_core::paths::database_path();
        let lib = Library::open(&db_path)?;
        let ctx = cc.egui_ctx.clone();
        configure_style(&ctx);
        // Replacement icons are PNG/SVG files loaded through egui's loaders.
        egui_extras::install_image_loaders(&ctx);

        let volume = lib.setting_f64(keys::VOLUME, 0.8) as f32;
        let shuffle = lib.setting_bool(keys::SHUFFLE, false);
        let repeat = Repeat::parse(&lib.setting_string(keys::REPEAT));
        let saved_device = Some(lib.setting_string(keys::OUTPUT_DEVICE)).filter(|d| !d.trim().is_empty());
        let player = Player::with_device(saved_device);
        player.set_volume(volume);

        let ui_state: Option<UiState> = lib
            .setting(keys::UI_STATE)
            .ok()
            .flatten()
            // The Up Next page became the queue panel; a layout saved on it opens Songs.
            .map(|s| s.replace(r#""view":"Queue""#, r#""view":"Songs""#))
            .and_then(|s| serde_json::from_str(&s).ok());

        let scrobble_cfg = ScrobbleConfig::load(&lib);
        let scrobbler = ScrobbleWorker::start(db_path.clone(), Duration::from_secs(120));
        let settings = SettingsUi::new(&lib, &scrobble_cfg);
        let write_tags_to_files = lib.setting_bool(keys::WRITE_TAGS_TO_FILES, true);
        let mut table = TableState::default();
        if let Some(s) = &ui_state {
            if !s.columns.is_empty() {
                table.columns = s.columns.clone();
            }
        }

        let mut app = YergApp {
            jobs: Jobs::new(db_path.clone(), ctx.clone()),
            art: ArtCache::new(db_path.clone(), ctx.clone()),
            lib,
            db_path,
            player,
            scrobbler,
            scrobble_cfg,
            queue: Queue::new(shuffle, repeat),
            now: None,
            lyrics_line: None,
            consecutive_errors: 0,
            view: ui_state.as_ref().map(|s| s.view.clone()).unwrap_or(View::Songs),
            search: String::new(),
            sort: ui_state.as_ref().and_then(|s| s.sort),
            album_sort: ui_state.as_ref().map(|s| s.album_sort).unwrap_or(AlbumSort::Artist),
            tracks: Vec::new(),
            tracks_dirty: true,
            lists_dirty: true,
            counts_dirty: false,
            history_dirty: true,
            albums: Vec::new(),
            artists: Vec::new(),
            genres: Vec::new(),
            playlists: Vec::new(),
            stats: LibraryStats::default(),
            history: Vec::new(),
            sel_artist: None,
            sel_album: None,
            sel_genre: None,
            table,
            right_panel: match &ui_state {
                Some(s) if s.show_queue => RightPanel::Queue,
                Some(s) if !s.show_now_playing => RightPanel::Hidden,
                _ => RightPanel::NowPlaying,
            },
            right_panel_last: match &ui_state {
                Some(s) if s.show_queue || s.right_last_queue => RightPanel::Queue,
                _ => RightPanel::NowPlaying,
            },
            show_sidebar: ui_state.as_ref().map(|s| s.show_sidebar).unwrap_or(true),
            menu_artists: None,
            volume_before_mute: 0.8,
            theme_errors: Vec::new(),
            appearance: Default::default(),
            queue_panel: QueuePanelCache::default(),
            settings_open: false,
            settings,
            tag_editor: None,
            prompt: None,
            confirm: None,
            toasts: Vec::new(),
            scan_progress: None,
            last_scan: None,
            device_ui: DeviceUi::default(),
            write_tags_to_files,
            focus_search: false,
            seek_preview: None,
            lyrics_scroll_pending: false,
            output_was_lost: false,
            frame_count: 0,
            autoscroll: AutoScroll::from_env(),
        };
        app.device_ui.reload(&app.lib);
        if !SHOW_DEVICE && matches!(app.view, View::Device) {
            app.view = View::Songs;
        }
        if let Ok(v) = std::env::var("YERG_DEBUG_VIEW") {
            app.view = match v.as_str() {
                "artists" => View::Artists,
                "album-artists" => View::AlbumArtists,
                "albums" => View::Albums,
                "genres" => View::Genres,
                "history" => View::History,
                "device" => View::Device,
                // "artist:NAME" opens the Artists view with NAME selected.
                s if s.starts_with("artist:") => {
                    app.sel_artist = Some(s["artist:".len()..].to_string());
                    View::Artists
                }
                _ => View::Songs,
            };
        }
        // First run: offer the Music folder.
        if app.lib.folders().map(|f| f.is_empty()).unwrap_or(true) {
            app.settings_open = true;
            app.settings.first_run = true;
        } else {
            app.jobs.scan(false);
        }
        if let Some(notice) = crate::crashlog::take_startup_notice() {
            app.toast(notice);
        }
        // Appearance from the dotfiles, re-read whenever they are saved.
        app.reload_theme(&ctx, true);
        // Debug aid: YERG_DEBUG_SETTINGS=appearance opens Settings on that tab.
        if std::env::var("YERG_DEBUG_SETTINGS").as_deref() == Ok("appearance") {
            app.settings_open = true;
            app.settings.tab = crate::views::settings::Tab::Appearance;
        }
        crate::theme::start_watcher(ctx.clone());
        Ok(app)
    }

    /// The Settings fields for a Last.fm-protocol connection in progress:
    /// (request token, status line).
    fn connect_state(&mut self, network: Network) -> (&mut Option<String>, &mut String) {
        match network {
            Network::LastFm => (&mut self.settings.lastfm_token, &mut self.settings.lastfm_status),
            Network::LibreFm => (&mut self.settings.librefm_token, &mut self.settings.librefm_status),
        }
    }

    /// Open `panel` on the right.
    pub fn show_right_panel(&mut self, panel: RightPanel) {
        self.right_panel = panel;
        if panel != RightPanel::Hidden {
            self.right_panel_last = panel;
        }
    }

    /// The Now Playing / Up Next buttons: open that panel, or hide it if it is
    /// already open.
    pub fn toggle_right_panel(&mut self, panel: RightPanel) {
        if self.right_panel == panel {
            self.right_panel = RightPanel::Hidden;
        } else {
            self.show_right_panel(panel);
        }
    }

    /// The right-panel toggle in the top bar: hide it, or bring back the last one.
    pub fn toggle_right_side(&mut self) {
        if self.right_panel == RightPanel::Hidden {
            self.right_panel = self.right_panel_last;
        } else {
            self.right_panel = RightPanel::Hidden;
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        log::info!("{msg}");
        self.toasts.push((msg, Instant::now()));
    }

    pub fn save_ui_state(&self) {
        // Debug runs force a view or scroll position; don't let them overwrite
        // the layout the user left.
        if ["YERG_DEBUG_VIEW", "YERG_DEBUG_AUTOSCROLL", "YERG_DEBUG_AUTOPLAY", "YERG_DEBUG_PANEL", "YERG_DEBUG_PANIC", "YERG_DEBUG_SETTINGS", "YERG_SCREENSHOT"]
            .iter()
            .any(|v| std::env::var_os(v).is_some())
        {
            return;
        }
        let st = UiState {
            view: self.view.clone(),
            show_now_playing: self.right_panel == RightPanel::NowPlaying,
            show_queue: self.right_panel == RightPanel::Queue,
            right_last_queue: self.right_panel_last == RightPanel::Queue,
            show_sidebar: self.show_sidebar,
            columns: self.table.columns.clone(),
            sort: self.sort,
            album_sort: self.album_sort,
        };
        if let Ok(s) = serde_json::to_string(&st) {
            let _ = self.lib.set_setting(keys::UI_STATE, &s);
        }
        let _ = self.lib.set_setting(keys::VOLUME, &self.player.state().volume.to_string());
        let _ = self.lib.set_setting_bool(keys::SHUFFLE, self.queue.shuffle);
        let _ = self.lib.set_setting(keys::REPEAT, self.queue.repeat.as_str());
    }

    pub fn set_view(&mut self, view: View) {
        if self.view != view {
            self.view = view;
            self.table.selection.clear();
            self.table.anchor = None;
            self.tracks_dirty = true;
        }
    }

    pub fn mark_library_changed(&mut self) {
        self.tracks_dirty = true;
        self.lists_dirty = true;
        self.history_dirty = true;
    }

    /// The filter behind the current view.
    fn current_filter(&self) -> TrackFilter {
        let mut f = TrackFilter {
            search: self.search.clone(),
            ..Default::default()
        };
        match &self.view {
            View::Artists => {
                f.artist = self.sel_artist.clone();
                f.album = self.sel_album.clone();
            }
            View::AlbumArtists => {
                f.album_artist = self.sel_artist.clone();
                f.album = self.sel_album.clone();
            }
            View::Albums => f.album = self.sel_album.clone(),
            View::Genres => f.genre = self.sel_genre.clone(),
            View::RecentlyPlayed | View::MostPlayed => f.only_played = true,
            _ => {}
        }
        f
    }

    /// Sort used when the user has not clicked a column.
    pub fn natural_sort(&self) -> Option<Sort> {
        match &self.view {
            View::Songs => Some(Sort::default()),
            View::Artists => Some(Sort::new(SortKey::Album)),
            View::AlbumArtists | View::Albums | View::Genres => Some(Sort::new(SortKey::AlbumArtist)),
            View::RecentlyAdded => Some(Sort::new(SortKey::AddedAt)),
            View::RecentlyPlayed => Some(Sort::new(SortKey::LastPlayed)),
            View::MostPlayed => Some(Sort::new(SortKey::PlayCount)),
            View::Playlist(_) | View::History | View::Device => None,
        }
    }

    pub fn effective_sort(&self) -> Option<Sort> {
        self.sort.or_else(|| self.natural_sort())
    }

    pub fn refresh_tracks(&mut self) {
        self.tracks_dirty = false;
        let result = match &self.view {
            View::Playlist(id) => {
                let id = *id;
                self.lib.playlist_tracks(id).map(|mut ts| {
                    if !self.search.trim().is_empty() {
                        let needle = self.search.to_lowercase();
                        ts.retain(|t| {
                            needle.split_whitespace().all(|w| {
                                t.title.to_lowercase().contains(w)
                                    || t.artist.to_lowercase().contains(w)
                                    || t.album.to_lowercase().contains(w)
                                    || t.album_artist.to_lowercase().contains(w)
                            })
                        });
                    }
                    if let Some(s) = self.sort {
                        sort_tracks(&mut ts, s);
                    }
                    ts
                })
            }
            View::History | View::Device => Ok(Vec::new()),
            _ => {
                let filter = self.current_filter();
                let sort = self.effective_sort().unwrap_or_default();
                self.lib.query_tracks(&filter, sort)
            }
        };
        match result {
            Ok(t) => self.tracks = t,
            Err(e) => self.toast(format!("Query failed: {e:#}")),
        }
        self.table
            .selection
            .retain(|i| *i < self.tracks.len());
    }

    pub fn refresh_lists(&mut self) {
        self.lists_dirty = false;
        let search_filter = TrackFilter {
            search: self.search.clone(),
            ..Default::default()
        };
        if let Ok(a) = self.lib.albums(&search_filter, self.album_sort) {
            self.albums = a;
        }
        let mode = if matches!(self.view, View::Artists) {
            ArtistMode::Artist
        } else {
            ArtistMode::AlbumArtist
        };
        if let Ok(a) = self.lib.artists(mode, &self.search) {
            self.artists = a;
        }
        if let Ok(g) = self.lib.genres() {
            self.genres = g;
        }
        if let Ok(p) = self.lib.playlists() {
            self.playlists = p;
        }
        if let Ok(s) = self.lib.stats() {
            self.stats = s;
        }
        self.device_ui.reload(&self.lib);
    }

    fn handle_job_events(&mut self) {
        for ev in self.jobs.poll() {
            match ev {
                JobEvent::Scan(ScanEvent::Started { total }) => {
                    self.scan_progress = Some((0, total, String::new()));
                }
                JobEvent::Scan(ScanEvent::Progress { done, total, path }) => {
                    self.scan_progress = Some((done, total, path));
                    if done % 200 == 0 {
                        self.mark_library_changed();
                    }
                }
                JobEvent::Scan(ScanEvent::Finished(summary)) => {
                    self.scan_progress = None;
                    self.last_scan = Some(summary.to_string());
                    if summary.added + summary.updated + summary.missing > 0 {
                        self.toast(format!("Library scan: {summary}"));
                    }
                    self.mark_library_changed();
                }
                JobEvent::LibraryChanged => self.mark_library_changed(),
                JobEvent::SyncPlanned(r) => match r {
                    Ok(plan) => self.device_ui.set_plan(plan),
                    Err(e) => self.toast(format!("Sync planning failed: {e}")),
                },
                JobEvent::Sync(ev) => self.handle_sync_event(ev),
                JobEvent::SyncDone(r) => {
                    match r {
                        Ok(rep) => {
                            let msg = if rep.cancelled {
                                format!("Sync cancelled after {} tracks", rep.copied)
                            } else {
                                format!(
                                    "Sync complete: {} copied, {} removed, {} failed",
                                    rep.copied,
                                    rep.deleted,
                                    rep.failed.len()
                                )
                            };
                            self.device_ui.log.push(msg.clone());
                            self.toast(msg);
                        }
                        Err(e) => {
                            self.device_ui.log.push(format!("Sync failed: {e}"));
                            self.toast(format!("Sync failed: {e}"));
                        }
                    }
                    self.device_ui.plan = None;
                    self.device_ui.reload(&self.lib);
                }
                JobEvent::Imported(r) => match r {
                    Ok(rep) => {
                        let msg = format!(
                            "Imported {} listens from the player ({} with estimated times, {} too short, {} unknown, {} already known)",
                            rep.counted, rep.estimated, rep.too_short, rep.unknown_track, rep.duplicates
                        );
                        self.device_ui.import_report = Some(msg.clone());
                        self.toast(msg);
                        self.scrobbler.flush();
                        self.device_ui.reload(&self.lib);
                    }
                    Err(e) => self.toast(format!("Import failed: {e}")),
                },
                JobEvent::LastFmToken(network, r) => match r {
                    Ok((token, url)) => {
                        let name = network.name();
                        let (key, secret) = match network {
                            Network::LastFm => {
                                let cfg = &self.settings.cfg;
                                (cfg.lastfm_api_key.trim().to_string(), cfg.lastfm_api_secret.trim().to_string())
                            }
                            Network::LibreFm => (LIBREFM_API_KEY.to_string(), LIBREFM_API_SECRET.to_string()),
                        };
                        let opened = open::that(&url).is_ok();
                        let (slot, status) = self.connect_state(network);
                        *slot = Some(token.clone());
                        *status = if opened {
                            format!("Waiting for you to approve Yerg Library on {name} in your browser…")
                        } else {
                            format!("Open {url} in a browser and approve Yerg Library; this finishes by itself")
                        };
                        // Finish automatically as soon as the user approves.
                        self.jobs.lastfm_session(network, key, secret, token, true);
                    }
                    Err(e) => *self.connect_state(network).1 = format!("Could not start authorisation: {e}"),
                },
                JobEvent::LastFmSession(network, r) => match r {
                    Ok((_, user)) => {
                        let (slot, status) = self.connect_state(network);
                        *slot = None;
                        *status = format!("Connected as {user}");
                        self.toast(format!("Connected to {} as {user}", network.name()));
                        self.scrobble_cfg = ScrobbleConfig::load(&self.lib);
                        self.settings.cfg = self.scrobble_cfg.clone();
                        self.scrobbler.flush();
                    }
                    Err(e) => {
                        let (slot, status) = self.connect_state(network);
                        *slot = None;
                        *status = format!("Not connected: {e}");
                    }
                },
                JobEvent::LbzValidated(r) => match r {
                    Ok(user) => {
                        self.settings.lbz_status = format!("Connected as {user}");
                        self.scrobble_cfg = ScrobbleConfig::load(&self.lib);
                        self.settings.cfg = self.scrobble_cfg.clone();
                        self.scrobbler.flush();
                    }
                    Err(e) => self.settings.lbz_status = format!("Token rejected: {e}"),
                },
                JobEvent::Flushed(r) => {
                    let msg = if r.errors.is_empty() {
                        format!("Scrobbled {} listens ({} ignored)", r.submitted, r.ignored)
                    } else {
                        format!("Scrobbled {}; {}", r.submitted, r.errors.join("; "))
                    };
                    self.toast(msg);
                    self.history_dirty = true;
                }
                JobEvent::TagsSaved(r) => {
                    match r {
                        Ok(ids) => {
                            self.toast(format!("Saved tags for {} track(s)", ids.len()));
                            self.menu_artists = None;
                            // Refresh now-playing metadata if it was edited.
                            if let Some(now) = &self.now {
                                if ids.contains(&now.track.id) {
                                    if let Ok(Some(t)) = self.lib.track(now.track.id) {
                                        let raw = self.lib.lyrics(t.id).ok().flatten().unwrap_or_default();
                                        let artists = self.lib.track_artists(t.id).unwrap_or_default();
                                        if let Some(n) = self.now.as_mut() {
                                            n.artists = if artists.is_empty() { vec![t.artist.clone()] } else { artists };
                                            n.track = t;
                                            n.lyrics = if raw.trim().is_empty() {
                                                None
                                            } else {
                                                Some(yerg_core::lyrics::parse(&raw))
                                            };
                                            n.lyrics_raw = raw;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => self.toast(format!("Some tags could not be written: {e}")),
                    }
                    self.mark_library_changed();
                }
                JobEvent::PlaylistImported(r) => match r {
                    Ok((id, n, missing)) => {
                        self.toast(format!("Imported playlist: {n} tracks matched, {} not found", missing.len()));
                        self.lists_dirty = true;
                        self.set_view(View::Playlist(id));
                    }
                    Err(e) => self.toast(format!("Playlist import failed: {e}")),
                },
                JobEvent::Toast(m) => self.toast(m),
                JobEvent::Error(e) => self.toast(e),
            }
        }
    }

    fn handle_sync_event(&mut self, ev: SyncEvent) {
        let d = &mut self.device_ui;
        match ev {
            SyncEvent::Started { to_copy, to_delete } => {
                d.log.clear();
                d.log.push(format!("Starting: {to_copy} to copy, {to_delete} to remove"));
                d.progress = (0, to_copy, 0.0);
            }
            SyncEvent::Copying { index, total, title } => {
                d.progress = (index, total, 0.0);
                d.current = title;
            }
            SyncEvent::Progress { index, fraction } => {
                d.progress = (index, d.progress.1, fraction);
            }
            SyncEvent::Copied { title, bytes } => {
                d.log.push(format!("Copied {title} ({})", yerg_core::fmt_bytes(bytes as i64)));
                d.progress.0 += 1;
            }
            SyncEvent::Deleted { path } => d.log.push(format!("Removed {path}")),
            SyncEvent::Log(s) => d.log.push(s),
            SyncEvent::IndexWritten { tracks, albums, artists, playlists, bytes } => d.log.push(format!(
                "Index written: {tracks} tracks, {albums} albums, {artists} artists, {playlists} playlists ({})",
                yerg_core::fmt_bytes(bytes as i64)
            )),
            SyncEvent::Finished(_) => {}
        }
        if d.log.len() > 500 {
            d.log.drain(..d.log.len() - 500);
        }
    }

    pub fn apply_actions(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::PlayIndex(i) => {
                    let tracks = self.tracks.clone();
                    self.play_list(&tracks, i);
                }
                Action::PlayNext(ids) => {
                    let n = ids.len();
                    self.queue.play_next(&ids);
                    if self.now.is_none() {
                        self.next_track(true);
                    }
                    self.toast(format!("{n} track(s) will play next"));
                }
                Action::AddToQueue(ids) => {
                    let n = ids.len();
                    self.queue.add_to_queue(&ids);
                    if self.now.is_none() {
                        self.next_track(true);
                    }
                    self.toast(format!("Added {n} track(s) to Up Next"));
                }
                Action::AddToPlaylist(pid, ids) => {
                    match self.lib.add_to_playlist(pid, &ids) {
                        Ok(()) => self.toast(format!("Added {} track(s) to playlist", ids.len())),
                        Err(e) => self.toast(format!("Could not add to playlist: {e:#}")),
                    }
                    self.lists_dirty = true;
                    if self.view == View::Playlist(pid) {
                        self.tracks_dirty = true;
                    }
                }
                Action::NewPlaylistWith(ids) => {
                    self.prompt = Some(Prompt {
                        title: "New Playlist".into(),
                        value: "New Playlist".into(),
                        kind: PromptKind::NewPlaylist { add: ids },
                    });
                }
                Action::RemoveFromPlaylist(positions) => {
                    if let View::Playlist(pid) = self.view {
                        if self.sort.is_some() {
                            self.toast("Reset the sort to playlist order before removing tracks");
                        } else if let Err(e) = self.lib.remove_from_playlist(pid, &positions) {
                            self.toast(format!("Could not remove: {e:#}"));
                        }
                        self.table.selection.clear();
                        self.tracks_dirty = true;
                        self.lists_dirty = true;
                    }
                }
                Action::Rate(ids, rating) => {
                    for id in &ids {
                        let _ = self.lib.set_rating(*id, rating);
                        if let Some(n) = self.now.as_mut() {
                            if n.track.id == *id {
                                n.track.rating = rating;
                            }
                        }
                    }
                    for t in self.tracks.iter_mut() {
                        if ids.contains(&t.id) {
                            t.rating = rating;
                        }
                    }
                }
                Action::EditTags(ids) => {
                    let tracks = self.lib.tracks_by_ids(&ids).unwrap_or_default();
                    if !tracks.is_empty() {
                        self.tag_editor = Some(TagEditor::new(&self.lib, tracks, self.write_tags_to_files));
                    }
                }
                Action::ShowInFolder(id) => {
                    if let Ok(Some(t)) = self.lib.track(id) {
                        show_in_folder(std::path::Path::new(&t.path));
                    }
                }
                Action::RemoveFromLibrary(ids) => {
                    self.confirm = Some(Confirm {
                        title: "Remove from Library".into(),
                        body: format!(
                            "Remove {} track(s) from the library? The files stay on disk, but play history for them is lost.",
                            ids.len()
                        ),
                        kind: ConfirmKind::RemoveTracks(ids),
                    });
                }
                Action::Sort(key) => {
                    let mut s = self.effective_sort().unwrap_or_default();
                    if self.sort.is_none() && !matches!(self.view, View::Playlist(_)) {
                        s = Sort::new(key);
                        if self.natural_sort().map(|n| n.key) == Some(key) {
                            s.descending = !self.natural_sort().unwrap().descending;
                        }
                    } else if self.sort.is_none() {
                        s = Sort::new(key);
                    } else {
                        s.toggle(key);
                    }
                    self.sort = Some(s);
                    self.tracks_dirty = true;
                }
                Action::ResetSort => {
                    self.sort = None;
                    self.tracks_dirty = true;
                }
                Action::OpenArtist(name) => {
                    self.set_view(View::Artists);
                    self.sel_artist = Some(name);
                    self.sel_album = None;
                    self.lists_dirty = true;
                    self.tracks_dirty = true;
                }
                Action::OpenAlbumArtist(name) => {
                    self.set_view(View::AlbumArtists);
                    self.sel_artist = Some(name);
                    self.sel_album = None;
                    self.lists_dirty = true;
                    self.tracks_dirty = true;
                }
                Action::OpenAlbum(key) => {
                    self.set_view(View::Albums);
                    self.sel_album = Some(key);
                    self.tracks_dirty = true;
                }
                Action::ToggleColumn(c) => {
                    if let Some(p) = self.table.columns.iter().position(|x| *x == c) {
                        if self.table.columns.len() > 2 {
                            self.table.columns.remove(p);
                        }
                    } else {
                        self.table.columns.push(c);
                        let order = Col::ALL;
                        self.table
                            .columns
                            .sort_by_key(|c| order.iter().position(|o| o == c).unwrap_or(99));
                    }
                }
                Action::RemoveQueue(i) => self.queue.remove_upcoming(i),
                Action::MoveQueue(from, to) => self.queue.move_upcoming(from, to),
                Action::JumpQueue(i) => {
                    if let Some(id) = self.queue.jump_upcoming(i) {
                        self.start_track(id);
                    }
                }
                Action::ClearQueue => self.queue.clear_upcoming(),
                Action::Seek(secs) => self.player.seek(Duration::from_secs_f64(secs.max(0.0))),
                Action::SetVolume(v) => self.player.set_volume(v),
            }
        }
    }

    pub fn resolve_prompt(&mut self, accepted: bool) {
        let Some(p) = self.prompt.take() else { return };
        if !accepted || p.value.trim().is_empty() {
            return;
        }
        match p.kind {
            PromptKind::NewPlaylist { add } => match self.lib.create_playlist(&p.value) {
                Ok(id) => {
                    if !add.is_empty() {
                        let _ = self.lib.add_to_playlist(id, &add);
                    }
                    self.lists_dirty = true;
                    if add.is_empty() {
                        self.set_view(View::Playlist(id));
                    }
                }
                Err(e) => self.toast(format!("Could not create playlist: {e:#}")),
            },
            PromptKind::RenamePlaylist(id) => {
                let _ = self.lib.rename_playlist(id, &p.value);
                self.lists_dirty = true;
            }
            PromptKind::RenameDevice(id) => {
                if let Ok(Some(mut d)) = self.lib.device(&id) {
                    d.name = p.value.trim().to_string();
                    let _ = self.lib.upsert_device(&d);
                    self.device_ui.reload(&self.lib);
                }
            }
        }
    }

    pub fn resolve_confirm(&mut self, accepted: bool) {
        let Some(c) = self.confirm.take() else { return };
        if !accepted {
            return;
        }
        match c.kind {
            ConfirmKind::DeletePlaylist(id) => {
                let _ = self.lib.delete_playlist(id);
                if self.view == View::Playlist(id) {
                    self.set_view(View::Songs);
                }
                self.lists_dirty = true;
            }
            ConfirmKind::RemoveTracks(ids) => {
                self.table.selection.clear();
                self.jobs.remove_tracks(ids);
            }
            ConfirmKind::WipeDevice(id) => {
                if let Ok(Some(d)) = self.lib.device(&id) {
                    match yerg_sync::device::wipe_device_music(&self.lib, &d) {
                        Ok(n) => self.toast(format!("Removed {n} files from the player")),
                        Err(e) => self.toast(format!("Wipe failed: {e:#}")),
                    }
                    self.device_ui.plan = None;
                    self.device_ui.reload(&self.lib);
                }
            }
            ConfirmKind::ForgetDevice(id) => {
                let _ = self.lib.delete_device(&id);
                self.device_ui.selected = None;
                self.device_ui.plan = None;
                self.device_ui.reload(&self.lib);
            }
            ConfirmKind::RunSync => {
                if let (Some(dev), Some(plan)) = (self.device_ui.selected_device(), self.device_ui.plan.clone()) {
                    self.jobs.run_sync(dev, plan);
                }
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (space, right, left, find, up, down, enter, del) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.modifiers.command && i.key_pressed(egui::Key::ArrowRight),
                i.modifiers.command && i.key_pressed(egui::Key::ArrowLeft),
                i.modifiers.command && i.key_pressed(egui::Key::F),
                i.modifiers.command && i.key_pressed(egui::Key::ArrowUp),
                i.modifiers.command && i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Delete),
            )
        });
        if space {
            self.toggle_play();
        }
        if right {
            self.next_track(true);
        }
        if left {
            self.prev_track();
        }
        if find {
            self.focus_search = true;
        }
        if up || down {
            let v = (self.player.state().volume + if up { 0.05 } else { -0.05 }).clamp(0.0, 1.0);
            self.player.set_volume(v);
        }
        if enter {
            if let Some(i) = self.table.selection.iter().next().copied() {
                self.apply_actions(vec![Action::PlayIndex(i)]);
            }
        }
        if del {
            if let View::Playlist(_) = self.view {
                let pos: Vec<usize> = self.table.selection.iter().copied().collect();
                if !pos.is_empty() {
                    self.apply_actions(vec![Action::RemoveFromPlaylist(pos)]);
                }
            }
        }
    }

}

impl eframe::App for YergApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.frame_count += 1;
        if crate::theme::take_change() {
            self.reload_theme(ctx, true);
        }
        // Debug aid: YERG_DEBUG_PANIC=1 panics shortly after start, to test the crash log.
        if self.frame_count == 30 && std::env::var_os("YERG_DEBUG_PANIC").is_some() {
            panic!("deliberate test panic (YERG_DEBUG_PANIC)");
        }
        if self.frame_count == 2 {
            if let Ok(q) = std::env::var("YERG_DEBUG_AUTOPLAY") {
                // Debug runs (screenshots) stay silent.
                self.player.set_volume(0.0);
                self.refresh_tracks();
                let all = self.lib.all_tracks().unwrap_or_default();
                if let Some(i) = all.iter().position(|t| t.title.eq_ignore_ascii_case(&q)) {
                    self.play_list(&all, i);
                    self.show_right_panel(RightPanel::NowPlaying);
                }
            }
            match std::env::var("YERG_DEBUG_PANEL").as_deref() {
                Ok("queue") => self.show_right_panel(RightPanel::Queue),
                Ok("now-playing") => self.show_right_panel(RightPanel::NowPlaying),
                Ok("none") => {
                    self.right_panel = RightPanel::Hidden;
                    self.show_sidebar = false;
                }
                _ => {}
            }
        }
        self.debug_screenshot(ctx);
        if let Some(a) = &mut self.autoscroll {
            a.tick(ctx);
        }
        self.handle_job_events();
        self.handle_player_events();
        self.art.poll(ctx);
        if self.lists_dirty {
            self.refresh_lists();
        }
        if self.tracks_dirty {
            self.refresh_tracks();
        }
        if self.counts_dirty {
            // Cheap refresh of play counts for the visible list without resetting selection.
            self.counts_dirty = false;
            let ids: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
            if ids.len() < 5000 {
                if let Ok(fresh) = self.lib.tracks_by_ids(&ids) {
                    if fresh.len() == self.tracks.len() {
                        self.tracks = fresh;
                    }
                }
            }
        }
        if self.history_dirty && matches!(self.view, View::History) {
            self.history_dirty = false;
            self.history = self.lib.recent_plays(500).unwrap_or_default();
        }
        self.handle_shortcuts(ctx);
        if self.update_lyrics_cursor() {
            ctx.request_repaint();
        }

        self.ui_top_bar(ctx);
        self.ui_transport(ctx);
        if self.show_sidebar {
            self.ui_sidebar(ctx);
        }
        match self.right_panel {
            RightPanel::Queue => self.ui_queue_panel(ctx),
            RightPanel::NowPlaying => self.ui_now_playing_panel(ctx),
            RightPanel::Hidden => {}
        }
        self.ui_central(ctx);
        self.ui_windows(ctx);
        self.ui_toasts(ctx);

        let st = self.player.state();
        if st.loaded && !st.paused {
            ctx.request_repaint_after(Duration::from_millis(200));
        } else if self.jobs.scanning || self.jobs.syncing || self.jobs.planning || !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

impl Drop for YergApp {
    fn drop(&mut self) {
        self.finalize_current(true);
        self.save_ui_state();
    }
}

fn sort_tracks(tracks: &mut [Track], s: Sort) {
    use std::cmp::Ordering;
    let cmp = |a: &Track, b: &Track| -> Ordering {
        let o = match s.key {
            SortKey::Title => a.display_title().to_lowercase().cmp(&b.display_title().to_lowercase()),
            SortKey::Artist => a.artist.to_lowercase().cmp(&b.artist.to_lowercase()),
            SortKey::AlbumArtist => a
                .effective_album_artist()
                .to_lowercase()
                .cmp(&b.effective_album_artist().to_lowercase()),
            SortKey::Album => a.album.to_lowercase().cmp(&b.album.to_lowercase()),
            SortKey::Year => a.year.cmp(&b.year),
            SortKey::TrackNo => (a.disc_no, a.track_no).cmp(&(b.disc_no, b.track_no)),
            SortKey::Duration => a.duration_ms.cmp(&b.duration_ms),
            SortKey::Genre => a.genre.to_lowercase().cmp(&b.genre.to_lowercase()),
            SortKey::Composer => a.composer.to_lowercase().cmp(&b.composer.to_lowercase()),
            SortKey::PlayCount => a.play_count.cmp(&b.play_count),
            SortKey::LastPlayed => a.last_played.cmp(&b.last_played),
            SortKey::AddedAt => a.added_at.cmp(&b.added_at),
            SortKey::Rating => a.rating.cmp(&b.rating),
            SortKey::Bitrate => a.bitrate_kbps.cmp(&b.bitrate_kbps),
            SortKey::Path => a.path.cmp(&b.path),
        };
        if s.descending {
            o.reverse()
        } else {
            o
        }
    };
    tracks.sort_by(cmp);
}

pub fn show_in_folder(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(dir) = path.parent() {
            let _ = open::that(dir);
        }
    }
}

/// Spacing and behaviour that do not depend on the theme.  Colours, fonts
/// and sizes come from the dotfiles (see `theme.rs`).
fn configure_style(ctx: &Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.interaction.selectable_labels = false;
        // Scroll bars: egui's resting width is 2 px and the handle is hidden
        // until the pointer is over the list.  Make it a little wider and
        // always faintly visible; it still widens under the pointer.
        style.spacing.scroll.floating_width = 5.0;
        style.spacing.scroll.dormant_handle_opacity = 0.35;
        style.spacing.scroll.active_handle_opacity = 0.7;
    });
}

impl YergApp {
    /// Read the dotfiles again and apply them (on start and whenever they change).
    pub fn reload_theme(&mut self, ctx: &Context, notify: bool) {
        let loaded = crate::theme::load();
        crate::theme::apply(ctx, loaded.theme);
        crate::theme::watch(loaded.watch);
        if notify {
            match loaded.errors.first() {
                Some(e) if loaded.errors.len() == 1 => self.toast(format!("Theme: {e}")),
                Some(e) => self.toast(format!("Theme: {e} (and {} more, see Settings › Appearance)", loaded.errors.len() - 1)),
                None => {}
            }
        }
        self.theme_errors = loaded.errors;
    }
}

/// Helper used by the device view for its policy setting.
pub fn device_policy(lib: &Library) -> DeviceScrobblePolicy {
    DeviceScrobblePolicy::from_setting(&lib.setting_string(keys::SCROBBLE_DEVICE_PLAYS))
}

impl YergApp {
    /// Debug aid: with `YERG_SCREENSHOT=<file.png>` set, render for a moment,
    /// save a frame capture to that file and exit.
    fn debug_screenshot(&mut self, ctx: &Context) {
        let Ok(path) = std::env::var("YERG_SCREENSHOT") else { return };
        if self.frame_count == 150 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                Some(buf) => {
                    if let Err(e) = buf.save(&path) {
                        log::error!("screenshot: {e}");
                    } else {
                        log::info!("screenshot saved to {path}");
                    }
                }
                None => log::error!("screenshot: bad buffer"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.frame_count > 600 {
            log::error!("screenshot never arrived");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}
