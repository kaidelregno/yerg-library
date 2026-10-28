//! The MP3 player page: register a card, plan and run a sync, import listens.

use std::path::Path;

use egui::RichText;
use yerg_core::model::*;
use yerg_core::settings::keys;
use yerg_core::Library;
use yerg_sync::device::{DeviceScrobblePolicy, SyncPlan, SYNC_MODE_ALL, SYNC_MODE_PLAYLISTS};

use crate::app::{Confirm, ConfirmKind, Prompt, PromptKind, YergApp};

#[derive(Default)]
pub struct DeviceUi {
    pub devices: Vec<Device>,
    pub selected: Option<String>,
    pub plan: Option<SyncPlan>,
    pub plan_summary: String,
    pub log: Vec<String>,
    pub progress: (usize, usize, f32),
    pub current: String,
    pub import_report: Option<String>,
    pub device_tracks: usize,
    pub device_bytes: i64,
    pub mounted: bool,
    pub new_name: String,
}

impl DeviceUi {
    pub fn reload(&mut self, lib: &Library) {
        self.devices = lib.devices().unwrap_or_default();
        if self.selected.is_none() {
            self.selected = lib
                .setting(keys::DEVICE_LAST_ID)
                .ok()
                .flatten()
                .filter(|id| self.devices.iter().any(|d| &d.id == id))
                .or_else(|| self.devices.first().map(|d| d.id.clone()));
        }
        if let Some(d) = self.selected_device() {
            let dt = lib.device_tracks(&d.id).unwrap_or_default();
            self.device_tracks = dt.len();
            self.device_bytes = dt.iter().map(|t| t.size_bytes).sum();
            self.mounted = Path::new(&d.mount_path).is_dir();
        }
    }

    pub fn selected_device(&self) -> Option<Device> {
        let id = self.selected.as_ref()?;
        self.devices.iter().find(|d| &d.id == id).cloned()
    }

    pub fn set_plan(&mut self, plan: SyncPlan) {
        self.plan_summary = format!(
            "{} tracks selected: {} to copy ({}), {} to remove, {} already up to date. Card total about {}.",
            plan.wanted.len(),
            plan.copy.len(),
            yerg_core::fmt_bytes(plan.estimated_copy_bytes as i64),
            plan.delete.len(),
            plan.keep,
            yerg_core::fmt_bytes(plan.estimated_total_bytes as i64)
        );
        self.plan = Some(plan);
    }
}

impl YergApp {
    pub fn ui_device(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_max_width(760.0);
                    self.device_header(ui);
                    ui.add_space(8.0);
                    if self.device_ui.selected_device().is_some() {
                        self.device_sync_section(ui);
                        ui.add_space(12.0);
                        self.device_listens_section(ui);
                        ui.add_space(12.0);
                        self.device_log_section(ui);
                    }
                    ui.add_space(20.0);
                    ui.collapsing("About the card layout", |ui| {
                        ui.label("Each track is copied unchanged, in its original format, to /MUSIC/Artist/Album/NN Title.<ext>. \
                                  Tags edited in the library reach the player through the index, not the files. /YERG/INDEX.BIN holds fixed-stride tables (tracks, albums, artists, playlists, \
                                  alphabetical orders and A–Z jump tables) that the firmware loads straight into RAM; the format is documented in docs/DEVICE_FORMAT.md. \
                                  The firmware appends listens to /YERG/LISTENS.LOG, which Import reads back into your play history and scrobble queue.");
                    });
                });
            });
        });
    }

    fn device_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Player").strong());
            let devices = self.device_ui.devices.clone();
            let current = self.device_ui.selected_device();
            egui::ComboBox::from_id_salt("device_pick")
                .selected_text(current.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| "No player registered".into()))
                .show_ui(ui, |ui| {
                    for d in &devices {
                        if ui.selectable_label(self.device_ui.selected.as_deref() == Some(d.id.as_str()), &d.name).clicked() {
                            self.device_ui.selected = Some(d.id.clone());
                            self.device_ui.plan = None;
                            let _ = self.lib.set_setting(keys::DEVICE_LAST_ID, &d.id);
                            self.device_ui.reload(&self.lib);
                        }
                    }
                });
            if ui.button("Add card…").on_hover_text("Pick the SD card's drive or folder").clicked() {
                if let Some(mount) = rfd::FileDialog::new().set_title("Choose the player's SD card").pick_folder() {
                    let name = if self.device_ui.new_name.trim().is_empty() { "Yerg" } else { self.device_ui.new_name.trim() };
                    match yerg_sync::device::register_device(&self.lib, &mount, name) {
                        Ok(d) => {
                            self.device_ui.selected = Some(d.id.clone());
                            self.device_ui.plan = None;
                            self.device_ui.reload(&self.lib);
                            self.toast(format!("Registered {} at {}", d.name, d.mount_path));
                        }
                        Err(e) => self.toast(format!("Could not register card: {e:#}")),
                    }
                }
            }
            if let Some(d) = &current {
                if ui.button("Rename…").clicked() {
                    self.prompt = Some(Prompt {
                        title: "Rename Player".into(),
                        value: d.name.clone(),
                        kind: PromptKind::RenameDevice(d.id.clone()),
                    });
                }
                if ui.button("Change location…").clicked() {
                    if let Some(mount) = rfd::FileDialog::new().pick_folder() {
                        let mut dd = d.clone();
                        dd.mount_path = mount.to_string_lossy().to_string();
                        let _ = self.lib.upsert_device(&dd);
                        let _ = yerg_sync::device::register_device(&self.lib, &mount, &dd.name);
                        self.device_ui.plan = None;
                        self.device_ui.reload(&self.lib);
                    }
                }
            }
        });
        let Some(d) = self.device_ui.selected_device() else {
            ui.add_space(8.0);
            ui.label(RichText::new("Insert the player's SD card and click \"Add card…\" to register it. The app writes a small DEVICE.TXT so it recognises the card later.").weak());
            return;
        };
        ui.add_space(4.0);
        let mounted = self.device_ui.mounted;
        ui.horizontal(|ui| {
            ui.label(RichText::new(&d.mount_path).monospace());
            if mounted {
                ui.label(RichText::new("● connected").color(crate::theme::current().colors.success).small());
            } else {
                ui.label(RichText::new("○ not found (insert the card)").color(crate::theme::current().colors.warning).small());
            }
            if ui.small_button("⟳").on_hover_text("Re-check").clicked() {
                self.device_ui.reload(&self.lib);
            }
        });
        ui.label(
            RichText::new(format!(
                "{} tracks on the card ({}) · last sync {} · last listen import {}",
                self.device_ui.device_tracks,
                yerg_core::fmt_bytes(self.device_ui.device_bytes),
                d.last_sync.map(crate::util::fmt_relative).unwrap_or_else(|| "never".into()),
                d.last_import.map(crate::util::fmt_relative).unwrap_or_else(|| "never".into())
            ))
            .weak(),
        );
    }

    fn device_sync_section(&mut self, ui: &mut egui::Ui) {
        let Some(mut d) = self.device_ui.selected_device() else { return };
        ui.separator();
        ui.label(RichText::new("Sync").size(16.0).strong());
        ui.horizontal(|ui| {
            ui.label("Put on the card:");
            let mut mode = d.sync_mode.clone();
            let changed_all = ui.radio_value(&mut mode, SYNC_MODE_ALL.to_string(), "Whole library").changed();
            let changed_pl = ui.radio_value(&mut mode, SYNC_MODE_PLAYLISTS.to_string(), "Selected playlists").changed();
            if changed_all || changed_pl {
                d.sync_mode = mode;
                let _ = self.lib.upsert_device(&d);
                self.device_ui.plan = None;
                self.device_ui.reload(&self.lib);
            }
        });
        if d.sync_mode == SYNC_MODE_PLAYLISTS {
            ui.indent("pl_pick", |ui| {
                let playlists = self.playlists.clone();
                if playlists.is_empty() {
                    ui.label(RichText::new("No playlists yet. Create some, then tick them here.").weak());
                }
                for p in &playlists {
                    let mut on = p.sync_to_device;
                    if ui
                        .checkbox(&mut on, format!("{}  ({} tracks, {})", p.name, p.track_count, yerg_core::fmt_total_time(p.duration_ms)))
                        .changed()
                    {
                        let _ = self.lib.set_playlist_sync(p.id, on);
                        self.lists_dirty = true;
                        self.device_ui.plan = None;
                    }
                }
            });
        }
        let size_note = if d.sync_mode == SYNC_MODE_ALL {
            format!(
                "Files are copied as they are, in their original format: the whole library is {} on the card.",
                yerg_core::fmt_bytes(self.stats.bytes)
            )
        } else {
            "Files are copied as they are, in their original format. Preview sync shows the space the selected playlists need."
                .to_string()
        };
        ui.label(RichText::new(size_note).weak().small());
        ui.add_space(6.0);
        let mounted = self.device_ui.mounted;
        ui.horizontal(|ui| {
            let busy = self.jobs.syncing || self.jobs.planning;
            if ui.add_enabled(mounted && !busy, egui::Button::new("Preview sync")).clicked() {
                self.jobs.plan_sync(d.clone());
            }
            let can_sync = mounted && !busy && self.device_ui.plan.is_some();
            if ui.add_enabled(can_sync, egui::Button::new(RichText::new("Sync now").strong())).clicked() {
                let plan = self.device_ui.plan.as_ref().unwrap();
                if plan.delete.is_empty() {
                    self.jobs.run_sync(d.clone(), plan.clone());
                } else {
                    self.confirm = Some(Confirm {
                        title: "Sync".into(),
                        body: format!(
                            "This sync removes {} track(s) from the card and copies {}. Continue?",
                            plan.delete.len(),
                            plan.copy.len()
                        ),
                        kind: ConfirmKind::RunSync,
                    });
                }
            }
            if self.jobs.syncing && ui.button("Cancel").clicked() {
                self.jobs.sync_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if self.jobs.planning {
                ui.spinner();
                ui.label("Planning…");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!busy, egui::Button::new("Forget player")).clicked() {
                    self.confirm = Some(Confirm {
                        title: "Forget Player".into(),
                        body: "Forget this player? Files on the card are left alone.".into(),
                        kind: ConfirmKind::ForgetDevice(d.id.clone()),
                    });
                }
                if ui.add_enabled(mounted && !busy, egui::Button::new("Wipe music")).clicked() {
                    self.confirm = Some(Confirm {
                        title: "Wipe Music".into(),
                        body: "Delete every synced track and the index from the card? The listen log is kept.".into(),
                        kind: ConfirmKind::WipeDevice(d.id.clone()),
                    });
                }
            });
        });
        if let Some(plan) = &self.device_ui.plan {
            ui.add_space(4.0);
            ui.label(&self.device_ui.plan_summary);
            if !plan.copy.is_empty() || !plan.delete.is_empty() {
                egui::CollapsingHeader::new("Details").show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for (t, p) in plan.copy.iter().take(500) {
                            ui.label(RichText::new(format!("+ {}  ←  {} - {}", p, t.artist, t.display_title())).small());
                        }
                        for dt in plan.delete.iter().take(500) {
                            ui.label(RichText::new(format!("− {}", dt.device_path)).small().color(crate::theme::current().colors.error));
                        }
                    });
                });
            }
        }
        if self.jobs.syncing {
            let (i, n, frac) = self.device_ui.progress;
            let overall = if n > 0 { (i as f32 + frac) / n as f32 } else { 1.0 };
            ui.add(egui::ProgressBar::new(overall).text(format!("{} / {}  {}", (i + 1).min(n.max(1)), n, self.device_ui.current)));
        }
    }

    fn device_listens_section(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.device_ui.selected_device() else { return };
        ui.separator();
        ui.label(RichText::new("Listens from the player").size(16.0).strong());
        ui.label(
            RichText::new("The firmware logs what it plays to /YERG/LISTENS.LOG. Importing adds those plays to your history and scrobble queue, with timestamps estimated when the player's clock was not set.")
                .weak()
                .small(),
        );
        let mut policy = crate::app::device_policy(&self.lib);
        ui.horizontal(|ui| {
            ui.label("Scrobble imported listens:");
            let before = policy;
            ui.radio_value(&mut policy, DeviceScrobblePolicy::All, "All");
            ui.radio_value(&mut policy, DeviceScrobblePolicy::TimestampedOnly, "Only with real timestamps");
            ui.radio_value(&mut policy, DeviceScrobblePolicy::None, "Never");
            if policy != before {
                let _ = self.lib.set_setting(keys::SCROBBLE_DEVICE_PLAYS, policy.as_setting());
            }
        });
        let mounted = self.device_ui.mounted;
        let log_exists = Path::new(&d.mount_path).join(yerg_sync::DEVICE_DIR).join(yerg_sync::LISTENS_FILE).is_file();
        ui.horizontal(|ui| {
            if ui.add_enabled(mounted && !self.jobs.importing, egui::Button::new("Import listens")).clicked() {
                self.jobs.import_listens(d.clone(), policy);
            }
            if self.jobs.importing {
                ui.spinner();
            }
            if mounted && !log_exists {
                ui.label(RichText::new("No LISTENS.LOG on the card yet").weak().small());
            }
        });
        if let Some(r) = &self.device_ui.import_report {
            ui.label(RichText::new(r).small());
        }
    }

    fn device_log_section(&mut self, ui: &mut egui::Ui) {
        if self.device_ui.log.is_empty() {
            return;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sync log").strong());
            if ui.small_button("Clear").clicked() {
                self.device_ui.log.clear();
            }
        });
        egui::ScrollArea::vertical().max_height(200.0).stick_to_bottom(true).show(ui, |ui| {
            for line in &self.device_ui.log {
                ui.label(RichText::new(line).small().monospace());
            }
        });
    }
}
