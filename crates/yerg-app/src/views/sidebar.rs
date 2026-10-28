use egui::{Context, RichText};

use crate::app::{Confirm, ConfirmKind, Prompt, PromptKind, View, YergApp, SHOW_DEVICE};

impl YergApp {
    pub fn ui_sidebar(&mut self, ctx: &Context) {
        egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(210.0)
            .width_range(160.0..=320.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(6.0);
                    section(ui, "LIBRARY");
                    let mut pick: Option<View> = None;
                    for v in [
                        View::Songs,
                        View::Artists,
                        View::AlbumArtists,
                        View::Albums,
                        View::Genres,
                        View::RecentlyAdded,
                        View::RecentlyPlayed,
                        View::MostPlayed,
                    ] {
                        if ui.selectable_label(self.view == v, v.title()).clicked() {
                            pick = Some(v);
                        }
                    }
                    ui.add_space(8.0);
                    section(ui, "PLAYING");
                    if ui.selectable_label(self.view == View::History, "History").clicked() {
                        pick = Some(View::History);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        section(ui, "PLAYLISTS");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("+").on_hover_text("New playlist").clicked() {
                                self.prompt = Some(Prompt {
                                    title: "New Playlist".into(),
                                    value: "New Playlist".into(),
                                    kind: PromptKind::NewPlaylist { add: vec![] },
                                });
                            }
                            if ui.small_button("Import").on_hover_text("Import an M3U playlist").clicked() {
                                if let Some(p) = rfd::FileDialog::new()
                                    .add_filter("Playlists", &["m3u", "m3u8"])
                                    .pick_file()
                                {
                                    self.jobs.import_playlist(p);
                                }
                            }
                        });
                    });
                    let playlists = self.playlists.clone();
                    for p in &playlists {
                        let selected = self.view == View::Playlist(p.id);
                        let synced = SHOW_DEVICE && p.sync_to_device;
                        let label = if synced {
                            format!("{}  ·", p.name)
                        } else {
                            p.name.clone()
                        };
                        let resp = ui.selectable_label(selected, label).on_hover_text(format!(
                            "{} tracks, {}{}",
                            p.track_count,
                            yerg_core::fmt_total_time(p.duration_ms),
                            if synced { "\nSynced to the MP3 player" } else { "" }
                        ));
                        if resp.clicked() {
                            pick = Some(View::Playlist(p.id));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Play").clicked() {
                                if let Ok(tracks) = self.lib.playlist_tracks(p.id) {
                                    self.play_list(&tracks, 0);
                                }
                                ui.close();
                            }
                            if ui.button("Rename…").clicked() {
                                self.prompt = Some(Prompt {
                                    title: "Rename Playlist".into(),
                                    value: p.name.clone(),
                                    kind: PromptKind::RenamePlaylist(p.id),
                                });
                                ui.close();
                            }
                            let mut sync = p.sync_to_device;
                            if SHOW_DEVICE && ui.checkbox(&mut sync, "Sync to MP3 player").changed() {
                                let _ = self.lib.set_playlist_sync(p.id, sync);
                                self.lists_dirty = true;
                                ui.close();
                            }
                            if ui.button("Export M3U…").clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_file_name(format!("{}.m3u8", p.name))
                                    .add_filter("M3U playlist", &["m3u8", "m3u"])
                                    .save_file()
                                {
                                    self.jobs.export_playlist(p.id, path);
                                }
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Delete").clicked() {
                                self.confirm = Some(Confirm {
                                    title: "Delete Playlist".into(),
                                    body: format!("Delete the playlist \"{}\"?", p.name),
                                    kind: ConfirmKind::DeletePlaylist(p.id),
                                });
                                ui.close();
                            }
                        });
                    }
                    if playlists.is_empty() {
                        ui.label(RichText::new("No playlists yet").weak().small());
                    }
                    if SHOW_DEVICE {
                        ui.add_space(8.0);
                        section(ui, "DEVICE");
                        let dev_label = match self.device_ui.devices.first() {
                            Some(d) if self.device_ui.devices.len() == 1 => d.name.clone(),
                            _ => "Yerg MP3 Player".to_string(),
                        };
                        if ui.selectable_label(self.view == View::Device, dev_label).clicked() {
                            pick = Some(View::Device);
                        }
                    }
                    // Library totals and the last scan result live in Settings ›
                    // Library (and each scan ends with a notice), not here too.
                    if let Some(v) = pick {
                        if matches!(v, View::Artists | View::AlbumArtists) && !matches!(self.view, View::Artists | View::AlbumArtists) {
                            self.sel_artist = None;
                            self.sel_album = None;
                        }
                        if matches!(v, View::Artists | View::AlbumArtists) {
                            self.lists_dirty = true;
                        }
                        self.set_view(v);
                    }
                });
            });
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).weak().small().strong());
}
