//! Artists / Album Artists / Albums / Genres views.

use egui::{CornerRadius, RichText, Sense, Vec2};
use yerg_core::model::*;
use yerg_core::query::AlbumSort;

use crate::app::{Action, View, YergApp};

const TILE: f32 = 150.0;
const TILE_GAP: f32 = 14.0;

impl YergApp {
    pub fn ui_artists(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        egui::SidePanel::left("artist_list")
            .resizable(true)
            .default_width(220.0)
            .width_range(150.0..=400.0)
            .show_inside(ui, |ui| {
                let artists = std::mem::take(&mut self.artists);
                let total = artists.len();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 22.0, total + 1, |ui, range| {
                    for i in range {
                        if i == 0 {
                            let all_sel = self.sel_artist.is_none();
                            if ui.selectable_label(all_sel, RichText::new(format!("All ({total})")).strong()).clicked() {
                                self.sel_artist = None;
                                self.sel_album = None;
                                self.tracks_dirty = true;
                            }
                            continue;
                        }
                        let a = &artists[i - 1];
                        let sel = self.sel_artist.as_deref() == Some(a.name.as_str());
                        let name = if a.name.is_empty() { "Unknown Artist" } else { &a.name };
                        let resp = ui
                            .selectable_label(sel, name)
                            .on_hover_text(format!("{} albums, {} tracks, {} plays", a.album_count, a.track_count, a.play_count));
                        if resp.clicked() {
                            self.sel_artist = Some(a.name.clone());
                            self.sel_album = None;
                            self.tracks_dirty = true;
                            self.table.selection.clear();
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Play all").clicked() {
                                self.sel_artist = Some(a.name.clone());
                                self.sel_album = None;
                                self.refresh_tracks();
                                let tracks = self.tracks.clone();
                                self.play_list(&tracks, 0);
                                ui.close();
                            }
                        });
                    }
                });
                self.artists = artists;
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            // Album strip for the selected artist
            if let Some(artist) = self.sel_artist.clone() {
                let by_album_artist = matches!(self.view, View::AlbumArtists);
                let albums: Vec<AlbumSummary> = if by_album_artist {
                    self.albums.iter().filter(|a| a.album_artist == artist).cloned().collect()
                } else {
                    // Albums the artist appears on
                    let mut keys = std::collections::HashSet::new();
                    for t in &self.tracks {
                        keys.insert((t.album.clone(), t.effective_album_artist().to_string()));
                    }
                    self.albums
                        .iter()
                        .filter(|a| keys.contains(&(a.album.clone(), a.album_artist.clone())))
                        .cloned()
                        .collect()
                };
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new(if artist.is_empty() { "Unknown Artist" } else { &artist }).size(20.0).strong());
                    let albums_text = if albums.len() == 1 { "1 album".to_string() } else { format!("{} albums", albums.len()) };
                    ui.label(crate::views::summary_text(format!("{albums_text} · {}", crate::views::track_summary(&self.tracks))));
                    if ui.small_button("▶ Play all").clicked() {
                        actions.push(Action::PlayIndex(0));
                    }
                });
                if albums.len() > 1 || self.sel_album.is_some() {
                    egui::ScrollArea::horizontal().id_salt("album_strip").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            let all_sel = self.sel_album.is_none();
                            if ui.selectable_label(all_sel, "All").clicked() {
                                self.sel_album = None;
                                self.tracks_dirty = true;
                            }
                            for a in &albums {
                                let sel = self.sel_album.as_ref() == Some(&a.key());
                                if self.album_tile_small(ui, a, sel) {
                                    self.sel_album = if sel { None } else { Some(a.key()) };
                                    self.tracks_dirty = true;
                                }
                            }
                        });
                    });
                }
                ui.add_space(4.0);
            }
            // With an artist selected the header above carries the summary.
            let summary = self.sel_artist.is_none();
            self.ui_track_list(ui, None, summary);
        });
        self.apply_actions(actions);
    }

    fn album_tile_small(&mut self, ui: &mut egui::Ui, a: &AlbumSummary, selected: bool) -> bool {
        let size = Vec2::new(96.0, 96.0 + 34.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        if ui.is_rect_visible(rect) {
            let art_rect = egui::Rect::from_min_size(rect.min, Vec2::splat(96.0));
            if selected {
                ui.painter().rect_filled(rect.expand(3.0), CornerRadius::same(6), ui.visuals().selection.bg_fill.gamma_multiply(0.35));
            }
            self.paint_art(ui, art_rect, a.art_hash.as_deref(), false);
            let text_rect = egui::Rect::from_min_max(rect.min + Vec2::new(0.0, 100.0), rect.max);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::top_down(egui::Align::Min)));
            child.add(egui::Label::new(RichText::new(&a.album).small()).truncate());
            child.add(egui::Label::new(RichText::new(a.year.map(|y| y.to_string()).unwrap_or_default()).weak().small()).truncate());
        }
        resp.on_hover_text(format!("{}\n{} tracks", a.album, a.track_count)).clicked()
    }

    /// Draw art (or a placeholder) into a rect.
    pub fn paint_art(&mut self, ui: &mut egui::Ui, rect: egui::Rect, hash: Option<&str>, full: bool) {
        let rounding = CornerRadius::same(4);
        match self.art.get(hash, full) {
            Some(tex) => {
                let img = egui::Image::new(&tex).corner_radius(rounding);
                // Fit inside, keep aspect
                let tex_size = tex.size_vec2();
                let scale = (rect.width() / tex_size.x).min(rect.height() / tex_size.y);
                let size = tex_size * scale;
                let r = egui::Rect::from_center_size(rect.center(), size);
                img.paint_at(ui, r);
            }
            None => {
                ui.painter().rect_filled(rect, rounding, crate::theme::current().colors.raised);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "♪",
                    egui::FontId::proportional(rect.height() * 0.4),
                    crate::theme::current().colors.icon_dim,
                );
            }
        }
    }

    pub fn ui_albums(&mut self, ui: &mut egui::Ui) {
        if let Some(key) = self.sel_album.clone() {
            self.ui_album_detail(ui, key);
            return;
        }
        let mut actions = Vec::new();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} albums", self.albums.len())).weak());
            ui.label("Sort by");
            let mut sort = self.album_sort;
            egui::ComboBox::from_id_salt("album_sort")
                .selected_text(sort.label())
                .show_ui(ui, |ui| {
                    for s in AlbumSort::ALL {
                        ui.selectable_value(&mut sort, s, s.label());
                    }
                });
            if sort != self.album_sort {
                self.album_sort = sort;
                self.lists_dirty = true;
            }
        });
        ui.add_space(4.0);
        let albums = std::mem::take(&mut self.albums);
        let avail_w = ui.available_width() - 16.0;
        let cols = ((avail_w + TILE_GAP) / (TILE + TILE_GAP)).floor().max(1.0) as usize;
        let rows = albums.len().div_ceil(cols);
        let row_h = TILE + 44.0 + TILE_GAP;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, rows, |ui, range| {
            for r in range {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    for c in 0..cols {
                        let i = r * cols + c;
                        let Some(a) = albums.get(i) else { break };
                        if let Some(act) = self.album_tile(ui, a) {
                            actions.push(act);
                        }
                        ui.add_space(TILE_GAP - ui.spacing().item_spacing.x);
                    }
                });
            }
        });
        self.albums = albums;
        self.apply_actions(actions);
    }

    fn album_tile(&mut self, ui: &mut egui::Ui, a: &AlbumSummary) -> Option<Action> {
        let size = Vec2::new(TILE, TILE + 44.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let mut action = None;
        if ui.is_rect_visible(rect) {
            let art_rect = egui::Rect::from_min_size(rect.min, Vec2::splat(TILE));
            if resp.hovered() {
                ui.painter().rect_filled(rect.expand(4.0), CornerRadius::same(6), crate::theme::current().colors.hover);
            }
            self.paint_art(ui, art_rect, a.art_hash.as_deref(), false);
            let text_rect = egui::Rect::from_min_max(rect.min + Vec2::new(0.0, TILE + 4.0), rect.max);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::top_down(egui::Align::Min)));
            child.add(egui::Label::new(RichText::new(if a.album.is_empty() { "Unknown Album" } else { &a.album }).strong()).truncate());
            let sub = match a.year {
                Some(y) if y > 0 => format!("{} · {y}", a.album_artist),
                _ => a.album_artist.clone(),
            };
            child.add(egui::Label::new(RichText::new(sub).weak().small()).truncate());
        }
        if resp.clicked() {
            action = Some(Action::OpenAlbum(a.key()));
        }
        resp.context_menu(|ui| {
            if ui.button("Play").clicked() {
                if let Ok(tracks) = self.lib.album_tracks(&a.key()) {
                    self.play_list(&tracks, 0);
                }
                ui.close();
            }
            if ui.button("Play Next").clicked() {
                if let Ok(tracks) = self.lib.album_tracks(&a.key()) {
                    action = Some(Action::PlayNext(tracks.iter().map(|t| t.id).collect()));
                }
                ui.close();
            }
            if ui.button("Add to Up Next").clicked() {
                if let Ok(tracks) = self.lib.album_tracks(&a.key()) {
                    action = Some(Action::AddToQueue(tracks.iter().map(|t| t.id).collect()));
                }
                ui.close();
            }
            if ui.button("Go to Artist").clicked() {
                action = Some(Action::OpenAlbumArtist(a.album_artist.clone()));
                ui.close();
            }
        });
        action
    }

    fn ui_album_detail(&mut self, ui: &mut egui::Ui, key: AlbumKey) {
        let summary = self.albums.iter().find(|a| a.key() == key).cloned();
        let mut actions = Vec::new();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("‹ Albums").clicked() {
                self.sel_album = None;
                self.tracks_dirty = true;
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(140.0), Sense::hover());
            let hash = summary.as_ref().and_then(|s| s.art_hash.clone());
            self.paint_art(ui, rect, hash.as_deref(), false);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(if key.album.is_empty() { "Unknown Album" } else { &key.album }).size(22.0).strong());
                let artist_resp = ui.add(egui::Label::new(RichText::new(&key.album_artist).size(15.0)).sense(Sense::click()));
                if artist_resp.clicked() {
                    actions.push(Action::OpenAlbumArtist(key.album_artist.clone()));
                }
                if let Some(s) = &summary {
                    let mut bits = Vec::new();
                    if let Some(y) = s.year.filter(|y| *y > 0) {
                        bits.push(y.to_string());
                    }
                    bits.push(crate::views::track_summary(&self.tracks));
                    if s.play_count > 0 {
                        bits.push(format!("{} plays", s.play_count));
                    }
                    ui.label(crate::views::summary_text(bits.join(" · ")));
                    let genres: Vec<String> = {
                        let mut g: Vec<String> = self.tracks.iter().map(|t| t.genre.clone()).filter(|g| !g.is_empty()).collect();
                        g.sort();
                        g.dedup();
                        g
                    };
                    if !genres.is_empty() {
                        ui.label(RichText::new(genres.join(", ")).weak().small());
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("▶ Play").clicked() {
                        actions.push(Action::PlayIndex(0));
                    }
                    if ui.button("Play Next").clicked() {
                        actions.push(Action::PlayNext(self.tracks.iter().map(|t| t.id).collect()));
                    }
                    if ui.button("Add to Up Next").clicked() {
                        actions.push(Action::AddToQueue(self.tracks.iter().map(|t| t.id).collect()));
                    }
                });
            });
        });
        ui.add_space(6.0);
        self.apply_actions(actions);
        // The album header above carries the summary.
        self.ui_track_list(ui, None, false);
    }

    pub fn ui_genres(&mut self, ui: &mut egui::Ui) {
        egui::SidePanel::left("genre_list")
            .resizable(true)
            .default_width(200.0)
            .show_inside(ui, |ui| {
                let genres = std::mem::take(&mut self.genres);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if ui.selectable_label(self.sel_genre.is_none(), RichText::new("All").strong()).clicked() {
                        self.sel_genre = None;
                        self.tracks_dirty = true;
                    }
                    for g in &genres {
                        let sel = self.sel_genre.as_deref() == Some(g.name.as_str());
                        let name = if g.name.is_empty() { "No Genre" } else { &g.name };
                        if ui
                            .selectable_label(sel, format!("{name}  ({})", g.track_count))
                            .on_hover_text(yerg_core::fmt_total_time(g.duration_ms))
                            .clicked()
                        {
                            self.sel_genre = Some(g.name.clone());
                            self.tracks_dirty = true;
                            self.table.selection.clear();
                        }
                    }
                });
                self.genres = genres;
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.ui_track_list(ui, None, true);
        });
    }
}
