//! The right-hand Now Playing panel: big art, metadata, rating and lyrics.

use egui::{Context, RichText, Sense, Vec2};

use crate::app::{Action, YergApp};
use crate::views::track_table::rating_widget;

impl YergApp {
    pub fn ui_now_playing_panel(&mut self, ctx: &Context) {
        egui::SidePanel::right("now_playing")
            .resizable(true)
            .default_width(300.0)
            .width_range(240.0..=560.0)
            .show(ctx, |ui| {
                let mut actions = Vec::new();
                let Some(now) = &self.now else {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(ui.available_width().min(260.0)), Sense::hover());
                        self.paint_art(ui, rect, None, true);
                        ui.add_space(12.0);
                        ui.label(RichText::new("Nothing playing").weak());
                    });
                    return;
                };
                let track = now.track.clone();
                let artists = now.artists.clone();
                let lyrics = now.lyrics.clone();
                let has_raw = !now.lyrics_raw.trim().is_empty();
                let line_idx = self.lyrics_line;

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(10.0);
                    let w = ui.available_width() - 8.0;
                    ui.vertical_centered(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(w.min(480.0)), Sense::hover());
                        self.paint_art(ui, rect, track.art_hash.as_deref(), true);
                    });
                    ui.add_space(10.0);
                    ui.add(egui::Label::new(RichText::new(track.display_title()).size(18.0).strong()).wrap());
                    // Each artist of a collaboration is its own link.
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (i, a) in artists.iter().enumerate() {
                            if i > 0 {
                                ui.label(RichText::new(", ").size(14.0).weak());
                            }
                            let link = ui
                                .add(
                                    egui::Label::new(RichText::new(a).size(14.0).color(crate::theme::current().colors.link))
                                        .sense(Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if link.clicked() {
                                actions.push(Action::OpenArtist(a.clone()));
                            }
                        }
                    });
                    let album_text = match track.year {
                        Some(y) if y > 0 => format!("{} · {y}", track.album),
                        _ => track.album.clone(),
                    };
                    let album = ui.add(egui::Label::new(RichText::new(album_text).weak()).sense(Sense::click()));
                    if album.clicked() {
                        actions.push(Action::OpenAlbum(yerg_core::model::AlbumKey {
                            album: track.album.clone(),
                            album_artist: track.effective_album_artist().to_string(),
                        }));
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if let Some(r) = rating_widget(ui, track.rating, false) {
                            actions.push(Action::Rate(vec![track.id], r));
                        }
                        ui.add_space(8.0);
                        let mut facts = Vec::new();
                        if track.play_count > 0 {
                            facts.push(format!("{} plays", track.play_count));
                        }
                        if !track.genre.is_empty() {
                            facts.push(track.genre.clone());
                        }
                        if let Some(b) = track.bitrate_kbps {
                            facts.push(format!("{} kbps {}", b, track.codec.to_uppercase()));
                        }
                        ui.label(RichText::new(facts.join(" · ")).weak().small());
                    });
                    ui.add_space(10.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Lyrics").strong());
                        if let Some(l) = &lyrics {
                            if l.synced {
                                ui.label(RichText::new("synced").weak().small());
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(if has_raw { "Edit" } else { "Add" }).clicked() {
                                actions.push(Action::EditTags(vec![track.id]));
                            }
                        });
                    });
                    ui.add_space(4.0);
                    match &lyrics {
                        Some(l) if !l.is_empty() => {
                            for (i, line) in l.lines.iter().enumerate() {
                                let current = Some(i) == line_idx;
                                let past = l.synced && line_idx.map(|c| i < c).unwrap_or(false);
                                let mut rt = RichText::new(if line.text.is_empty() { " " } else { &line.text }).size(if current { 16.0 } else { 14.0 });
                                if current {
                                    rt = rt.strong().color(crate::theme::current().colors.strong);
                                } else if past {
                                    rt = rt.color(crate::theme::current().colors.weak);
                                } else if l.synced {
                                    rt = rt.color(crate::theme::current().colors.text);
                                }
                                let resp = ui.add(egui::Label::new(rt).wrap().sense(if l.synced { Sense::click() } else { Sense::hover() }));
                                if current && self.lyrics_scroll_pending {
                                    resp.scroll_to_me(Some(egui::Align::Center));
                                }
                                if resp.clicked() {
                                    if let Some(t) = line.time_ms {
                                        actions.push(Action::Seek((t - l.offset_ms()) as f64 / 1000.0));
                                    }
                                }
                            }
                            self.lyrics_scroll_pending = false;
                        }
                        _ => {
                            ui.label(RichText::new("No lyrics for this track.\nAdd them with Edit, or drop a .lrc file next to the audio file.").weak());
                        }
                    }
                    ui.add_space(20.0);
                });
                self.apply_actions(actions);
            });
    }
}
