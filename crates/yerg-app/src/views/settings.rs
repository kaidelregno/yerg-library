//! Settings window: library folders, scrobbling accounts, about.

use egui::RichText;
use yerg_core::model::*;
use yerg_core::settings::keys;
use yerg_core::Library;
use yerg_scrobble::lastfm::{Network, LIBREFM_API_KEY, LIBREFM_API_SECRET};
use yerg_scrobble::worker::ScrobbleConfig;

use crate::app::YergApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Library,
    Playback,
    Scrobbling,
    Appearance,
    About,
}

pub struct SettingsUi {
    pub tab: Tab,
    pub cfg: ScrobbleConfig,
    pub lastfm_token: Option<String>,
    pub lastfm_status: String,
    pub lbz_status: String,
    /// Libre.fm connect flow (same steps as Last.fm).
    pub librefm_token: Option<String>,
    pub librefm_status: String,
    pub folders: Vec<LibraryFolder>,
    pub first_run: bool,
    pub show_secret: bool,
    /// Output devices, listed when the Playback tab is opened (enumeration is slow-ish).
    pub devices: Option<Vec<String>>,
    pub default_device: Option<String>,
    /// Artist names never to split, one per line, as typed and as last saved.
    pub split_exceptions: String,
    pub split_exceptions_saved: String,
}

impl SettingsUi {
    pub fn new(lib: &Library, cfg: &ScrobbleConfig) -> SettingsUi {
        SettingsUi {
            tab: Tab::Library,
            cfg: cfg.clone(),
            lastfm_token: None,
            lastfm_status: if cfg.lastfm_ready() {
                format!("Connected as {}", cfg.lastfm_username)
            } else {
                String::new()
            },
            lbz_status: if cfg.listenbrainz_ready() {
                format!("Connected as {}", cfg.listenbrainz_username)
            } else {
                String::new()
            },
            librefm_token: None,
            librefm_status: if cfg.librefm_ready() {
                format!("Connected as {}", cfg.librefm_username)
            } else {
                String::new()
            },
            folders: lib.folders().unwrap_or_default(),
            first_run: false,
            show_secret: false,
            devices: None,
            default_device: None,
            split_exceptions: lib.setting_string(keys::ARTIST_SPLIT_EXCEPTIONS),
            split_exceptions_saved: lib.setting_string(keys::ARTIST_SPLIT_EXCEPTIONS),
        }
    }
}

impl YergApp {
    pub fn ui_settings_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.settings.tab, Tab::Library, "Library");
            ui.selectable_value(&mut self.settings.tab, Tab::Playback, "Playback");
            ui.selectable_value(&mut self.settings.tab, Tab::Scrobbling, "Scrobbling");
            ui.selectable_value(&mut self.settings.tab, Tab::Appearance, "Appearance");
            ui.selectable_value(&mut self.settings.tab, Tab::About, "About");
        });
        ui.separator();
        match self.settings.tab {
            Tab::Library => self.settings_library(ui),
            Tab::Playback => self.settings_playback(ui),
            Tab::Scrobbling => self.settings_scrobbling(ui),
            Tab::Appearance => self.settings_appearance(ui),
            Tab::About => settings_about(ui),
        }
    }

    fn settings_library(&mut self, ui: &mut egui::Ui) {
        if self.settings.first_run {
            ui.label(RichText::new("Welcome! Add the folder(s) that hold your music to build the library.").strong());
            ui.add_space(6.0);
        }
        ui.label(RichText::new("Music folders").strong());
        let folders = self.settings.folders.clone();
        let mut remove: Option<i64> = None;
        for f in &folders {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&f.path).monospace());
                if ui.small_button("Remove").on_hover_text("Remove the folder and its tracks from the library").clicked() {
                    remove = Some(f.id);
                }
            });
        }
        if folders.is_empty() {
            ui.label(RichText::new("No folders yet.").weak());
        }
        ui.horizontal(|ui| {
            if ui.button("Add folder…").clicked() {
                let start = yerg_core::paths::default_music_dir();
                let mut dlg = rfd::FileDialog::new().set_title("Choose a music folder");
                if let Some(s) = start {
                    dlg = dlg.set_directory(s);
                }
                if let Some(p) = dlg.pick_folder() {
                    match self.lib.add_folder(&p) {
                        Ok(_) => {
                            self.settings.folders = self.lib.folders().unwrap_or_default();
                            self.settings.first_run = false;
                            self.jobs.scan(false);
                        }
                        Err(e) => self.toast(format!("Could not add folder: {e:#}")),
                    }
                }
            }
            let busy = self.jobs.scanning;
            if ui.add_enabled(!busy, egui::Button::new("Scan for changes")).clicked() {
                self.jobs.scan(false);
            }
            if ui.add_enabled(!busy, egui::Button::new("Full rescan")).on_hover_text("Re-read tags of every file").clicked() {
                self.jobs.scan(true);
            }
            if ui.add_enabled(!busy, egui::Button::new("Remove missing tracks")).on_hover_text("Forget tracks whose files are gone").clicked() {
                self.jobs.purge_missing();
            }
        });
        if let Some(id) = remove {
            match self.lib.remove_folder(id) {
                Ok(n) => {
                    self.toast(format!("Removed folder and {n} tracks"));
                    self.settings.folders = self.lib.folders().unwrap_or_default();
                    self.mark_library_changed();
                }
                Err(e) => self.toast(format!("Could not remove folder: {e:#}")),
            }
        }
        if let Some(s) = &self.last_scan {
            ui.label(RichText::new(format!("Last scan: {s}")).weak().small());
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Tag editing").strong());
        let mut w = self.write_tags_to_files;
        if ui
            .checkbox(&mut w, "Write edited tags, lyrics and artwork into the audio files")
            .on_hover_text("Off: edits are kept only in the library database")
            .changed()
        {
            self.write_tags_to_files = w;
            let _ = self.lib.set_setting_bool(keys::WRITE_TAGS_TO_FILES, w);
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Artists").strong());
        ui.label(
            RichText::new(
                "Collaborations (\"A & B\", \"A, B\", \"A feat. B\") are listed under each of their artists. \
                 Tracks with an ARTISTS tag (MusicBrainz Picard writes one) are listed under everyone it names. \
                 A name with a comma or \"&\" stays whole unless part of it is an artist of its own, and common ones \
                 like Tyler, the Creator or Earth, Wind & Fire are never split. Add any other name to keep whole, one per line:",
            )
            .weak()
            .small(),
        );
        let resp = ui.add(
            egui::TextEdit::multiline(&mut self.settings.split_exceptions)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("e.g. Hey, ily!"),
        );
        // Save once editing stops, not on every keystroke (it regroups the library).
        if !resp.has_focus() && self.settings.split_exceptions != self.settings.split_exceptions_saved {
            let text = self.settings.split_exceptions.clone();
            let _ = self.lib.set_setting(keys::ARTIST_SPLIT_EXCEPTIONS, &text);
            self.settings.split_exceptions_saved = text;
            self.menu_artists = None;
            self.lists_dirty = true;
            self.tracks_dirty = true;
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Storage").strong());
        ui.label(RichText::new(format!("Database: {}", self.db_path.display())).weak().small());
        ui.label(
            RichText::new(format!(
                "{} tracks, {} albums, {} artists, {} plays",
                self.stats.tracks, self.stats.albums, self.stats.artists, self.stats.plays
            ))
            .weak()
            .small(),
        );
    }

    fn settings_playback(&mut self, ui: &mut egui::Ui) {
        if self.settings.devices.is_none() {
            self.settings.devices = Some(yerg_player::Player::output_devices());
            self.settings.default_device = yerg_player::Player::default_output_device();
        }
        let st = self.player.state();
        let devices = self.settings.devices.clone().unwrap_or_default();
        let default_label = match &self.settings.default_device {
            Some(d) => format!("System default ({d})"),
            None => "System default".to_string(),
        };

        ui.label(RichText::new("Output device").strong());
        let mut choice = st.preferred_device.clone();
        let before = choice.clone();
        egui::ComboBox::from_id_salt("output_device")
            .width(380.0)
            .selected_text(choice.clone().unwrap_or_else(|| default_label.clone()))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut choice, None, &default_label);
                for d in &devices {
                    ui.selectable_value(&mut choice, Some(d.clone()), d);
                }
                // A saved device that is unplugged right now stays selectable.
                if let Some(p) = &before {
                    if !devices.contains(p) {
                        ui.selectable_value(&mut choice, Some(p.clone()), format!("{p} (not connected)"));
                    }
                }
            });
        if choice != before {
            let _ = self.lib.set_setting(keys::OUTPUT_DEVICE, choice.as_deref().unwrap_or(""));
            self.player.set_output_device(choice);
        }
        ui.horizontal(|ui| {
            if ui.small_button("Refresh list").clicked() {
                self.settings.devices = None;
            }
            ui.label(
                RichText::new("With System default, playback follows the device chosen in your system sound settings.")
                    .weak()
                    .small(),
            );
        });

        ui.add_space(8.0);
        egui::Grid::new("output_status").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            ui.label("Playing on");
            ui.label(st.output_device.clone().unwrap_or_else(|| "nothing (no device)".into()));
            ui.end_row();
            ui.label("Format");
            ui.label(st.output_format.clone().unwrap_or_else(|| "-".into()));
            ui.end_row();
        });
        if let Some(e) = &st.output_error {
            ui.label(RichText::new(e).color(crate::theme::current().colors.warning).small());
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Audio is converted to the device's own sample rate and channel count with a high-quality resampler. \
                 Surround files are downmixed for stereo devices, and stereo is averaged for mono devices such as headsets in call mode. \
                 If the device disappears (unplugged, Bluetooth switched off) or the system default changes, playback carries on from the same spot on the next available device.",
            )
            .weak()
            .small(),
        );
    }

    fn settings_scrobbling(&mut self, ui: &mut egui::Ui) {
        let mut cfg = self.settings.cfg.clone();
        let mut changed = false;
        let mut disconnect_lastfm = false;
        changed |= ui.checkbox(&mut cfg.enabled, "Scrobble what I play").changed();
        changed |= ui
            .checkbox(&mut cfg.now_playing, "Send \"now playing\" updates")
            .changed();
        ui.label(
            RichText::new("Listens are stored locally first and submitted when a service is reachable, so offline and MP3-player listens are scrobbled later with their original times.")
                .weak()
                .small(),
        );

        ui.add_space(10.0);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Last.fm").strong());
                changed |= ui.checkbox(&mut cfg.lastfm_enabled, "enabled").changed();
            });
            ui.label(RichText::new("Last.fm needs your own API account: create one at last.fm/api/account/create and paste the key and shared secret.").weak().small());
            egui::Grid::new("lastfm_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("API key");
                changed |= ui.add(egui::TextEdit::singleline(&mut cfg.lastfm_api_key).desired_width(320.0)).changed();
                ui.end_row();
                ui.label("Shared secret");
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut cfg.lastfm_api_secret).password(!self.settings.show_secret).desired_width(280.0))
                        .changed();
                    ui.checkbox(&mut self.settings.show_secret, "show");
                });
                ui.end_row();
            });
            ui.horizontal(|ui| {
                let can = !cfg.lastfm_api_key.trim().is_empty() && !cfg.lastfm_api_secret.trim().is_empty();
                if self.settings.lastfm_token.is_none() {
                    if ui.add_enabled(can, egui::Button::new(if cfg.lastfm_ready() { "Reconnect" } else { "Connect" })).clicked() {
                        self.settings.lastfm_status = "Opening Last.fm in your browser…".into();
                        self.jobs.lastfm_token(Network::LastFm, cfg.lastfm_api_key.trim().to_string(), cfg.lastfm_api_secret.trim().to_string());
                    }
                } else {
                    ui.spinner();
                    if ui
                        .button("I've approved it")
                        .on_hover_text("Check with Last.fm right now instead of waiting for the next automatic check")
                        .clicked()
                    {
                        let token = self.settings.lastfm_token.clone().unwrap();
                        self.jobs.lastfm_session(Network::LastFm, cfg.lastfm_api_key.trim().to_string(), cfg.lastfm_api_secret.trim().to_string(), token, true);
                    }
                    if ui.button("Cancel").clicked() {
                        self.jobs.cancel_lastfm_wait();
                        self.settings.lastfm_token = None;
                        self.settings.lastfm_status.clear();
                    }
                }
                if cfg.lastfm_ready() && ui.button("Disconnect").clicked() {
                    cfg.lastfm_session_key.clear();
                    cfg.lastfm_username.clear();
                    cfg.lastfm_enabled = false;
                    self.settings.lastfm_status.clear();
                    disconnect_lastfm = true;
                    changed = true;
                }
            });
            if self.settings.lastfm_status.is_empty()
                && self.settings.lastfm_token.is_none()
                && !cfg.lastfm_ready()
                && !cfg.lastfm_api_key.trim().is_empty()
                && !cfg.lastfm_api_secret.trim().is_empty()
            {
                ui.label(
                    RichText::new("Not connected yet: click Connect and approve Yerg Library on the page that opens. Nothing is scrobbled until then.")
                        .color(crate::theme::current().colors.warning)
                        .small(),
                );
            }
            if !self.settings.lastfm_status.is_empty() {
                let ok = self.settings.lastfm_status.starts_with("Connected");
                ui.label(RichText::new(&self.settings.lastfm_status).color(if ok { crate::theme::current().colors.success } else { crate::theme::current().colors.warning }).small());
            }
            self.pending_row(ui, ScrobbleService::LastFm);
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ListenBrainz").strong());
                changed |= ui.checkbox(&mut cfg.listenbrainz_enabled, "enabled").changed();
            });
            ui.label(RichText::new("Paste the user token from listenbrainz.org/settings.").weak().small());
            ui.horizontal(|ui| {
                ui.label("User token");
                changed |= ui
                    .add(egui::TextEdit::singleline(&mut cfg.listenbrainz_token).password(!self.settings.show_secret).desired_width(320.0))
                    .changed();
                if ui.add_enabled(!cfg.listenbrainz_token.trim().is_empty(), egui::Button::new("Validate")).clicked() {
                    self.settings.lbz_status = "Checking…".into();
                    self.jobs.lbz_validate(cfg.listenbrainz_token.trim().to_string());
                }
            });
            if !self.settings.lbz_status.is_empty() {
                let ok = self.settings.lbz_status.starts_with("Connected");
                ui.label(RichText::new(&self.settings.lbz_status).color(if ok { crate::theme::current().colors.success } else { crate::theme::current().colors.warning }).small());
            }
            self.pending_row(ui, ScrobbleService::ListenBrainz);
        });

        ui.add_space(8.0);
        let mut disconnect_librefm = false;
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Libre.fm").strong());
                changed |= ui.checkbox(&mut cfg.librefm_enabled, "enabled").changed();
            });
            ui.label(
                RichText::new("No API account needed: click Connect and approve Yerg Library on the libre.fm page that opens.")
                    .weak()
                    .small(),
            );
            ui.horizontal(|ui| {
                if self.settings.librefm_token.is_none() {
                    if ui.button(if cfg.librefm_ready() { "Reconnect" } else { "Connect" }).clicked() {
                        self.settings.librefm_status = "Opening Libre.fm in your browser…".into();
                        self.jobs.lastfm_token(Network::LibreFm, LIBREFM_API_KEY.into(), LIBREFM_API_SECRET.into());
                    }
                } else {
                    ui.spinner();
                    if ui
                        .button("I've approved it")
                        .on_hover_text("Check with Libre.fm right now instead of waiting for the next automatic check")
                        .clicked()
                    {
                        let token = self.settings.librefm_token.clone().unwrap();
                        self.jobs.lastfm_session(Network::LibreFm, LIBREFM_API_KEY.into(), LIBREFM_API_SECRET.into(), token, true);
                    }
                    if ui.button("Cancel").clicked() {
                        self.jobs.cancel_lastfm_wait();
                        self.settings.librefm_token = None;
                        self.settings.librefm_status.clear();
                    }
                }
                if cfg.librefm_ready() && ui.button("Disconnect").clicked() {
                    cfg.librefm_session_key.clear();
                    cfg.librefm_username.clear();
                    cfg.librefm_enabled = false;
                    self.settings.librefm_status.clear();
                    disconnect_librefm = true;
                    changed = true;
                }
            });
            if !self.settings.librefm_status.is_empty() {
                let ok = self.settings.librefm_status.starts_with("Connected");
                ui.label(RichText::new(&self.settings.librefm_status).color(if ok { crate::theme::current().colors.success } else { crate::theme::current().colors.warning }).small());
            }
            self.pending_row(ui, ScrobbleService::LibreFm);
        });

        if changed {
            // Accounts are connected by background jobs that write straight to the
            // database; never overwrite what they stored with this window's copy.
            let stored = ScrobbleConfig::load(&self.lib);
            if !disconnect_lastfm {
                cfg.lastfm_session_key = stored.lastfm_session_key;
                cfg.lastfm_username = stored.lastfm_username;
            }
            if !disconnect_librefm {
                cfg.librefm_session_key = stored.librefm_session_key;
                cfg.librefm_username = stored.librefm_username;
            }
            if cfg.listenbrainz_token.trim() == stored.listenbrainz_token.trim() {
                cfg.listenbrainz_username = stored.listenbrainz_username;
            }
            if let Err(e) = cfg.save(&self.lib) {
                self.toast(format!("Could not save settings: {e:#}"));
            }
            self.settings.cfg = cfg.clone();
            self.scrobble_cfg = cfg;
            self.scrobbler.flush();
        }

        ui.add_space(8.0);
        let st = self.scrobbler.status();
        let mut line = String::new();
        if let Some(t) = st.last_flush {
            line.push_str(&format!("Last attempt {}", crate::util::fmt_relative(t)));
        }
        if let Some(e) = &st.last_error {
            line.push_str(&format!(" · {e}"));
        }
        if !line.is_empty() {
            ui.label(RichText::new(line).weak().small());
        }
    }

    /// "N listens waiting" with Submit / Skip / Requeue, only for a service
    /// that is set up: an unconfigured one has nothing waiting to go.
    fn pending_row(&mut self, ui: &mut egui::Ui, service: ScrobbleService) {
        if !self.settings.cfg.ready(service) {
            return;
        }
        let pending = self.lib.pending_count(service).unwrap_or(0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{pending} listens waiting")).small());
            if pending > 0 {
                if ui.small_button("Submit now").clicked() {
                    self.jobs.flush_scrobbles();
                }
                if ui
                    .small_button("Skip them")
                    .on_hover_text("Do not send the waiting listens (they still count locally)")
                    .clicked()
                {
                    let _ = self.lib.skip_all_pending(service);
                }
            }
            if ui.small_button("Requeue skipped").on_hover_text("Put skipped and ignored listens back in the queue").clicked() {
                let n = self.lib.requeue(service, true).unwrap_or(0);
                self.toast(format!("Requeued {n} listens for {}", service.label()));
            }
        });
    }
}

fn settings_about(ui: &mut egui::Ui) {
    ui.label(RichText::new("Yerg Library").size(20.0).strong());
    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
    ui.add_space(6.0);
    ui.label("A lightweight library manager and player, and the sync companion for the Yerg MP3 player.");
    ui.add_space(6.0);
    ui.label(RichText::new("Shortcuts").strong());
    ui.label("Space: play / pause · Ctrl+→ / Ctrl+←: next / previous · Ctrl+↑ / Ctrl+↓: volume · Ctrl+F: search · Enter: play selection · Delete: remove from playlist");
    ui.add_space(6.0);
    ui.label(RichText::new("Crash log").strong());
    let path = crate::crashlog::log_path();
    if crate::crashlog::has_log() {
        ui.label(RichText::new(format!("Errors that stopped the app are recorded in {}.", path.display())).small());
        ui.horizontal(|ui| {
            if ui.button("Open crash log").clicked() {
                let _ = open::that(&path);
            }
            if ui.button("Open folder").clicked() {
                if let Some(dir) = path.parent() {
                    let _ = open::that(dir);
                }
            }
        });
    } else {
        ui.label(RichText::new("No errors recorded. If the app ever stops unexpectedly, the details are saved here and shown on the next start.").weak().small());
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Built with Rust, egui, cpal, symphonia, lofty and SQLite.").weak().small());
}
