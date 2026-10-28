//! The track list: sortable columns, multi-select, context menu.

use std::collections::BTreeSet;

use egui::{RichText, Sense};
use egui_extras::{Column as TCol, TableBuilder};
use serde::{Deserialize, Serialize};
use yerg_core::model::*;
use yerg_core::query::SortKey;

use crate::app::{Action, View, YergApp};
use crate::util::{fmt_relative, stars};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Col {
    Playing,
    TrackNo,
    Title,
    Duration,
    Artist,
    Album,
    AlbumArtist,
    Genre,
    Year,
    PlayCount,
    Rating,
    LastPlayed,
    AddedAt,
    Composer,
    Bitrate,
}

impl Col {
    pub const ALL: [Col; 15] = [
        Col::Playing,
        Col::TrackNo,
        Col::Title,
        Col::Duration,
        Col::Artist,
        Col::Album,
        Col::AlbumArtist,
        Col::Genre,
        Col::Year,
        Col::PlayCount,
        Col::Rating,
        Col::LastPlayed,
        Col::AddedAt,
        Col::Composer,
        Col::Bitrate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Col::Playing => "",
            Col::TrackNo => "#",
            Col::Title => "Title",
            Col::Duration => "Time",
            Col::Artist => "Artist",
            Col::Album => "Album",
            Col::AlbumArtist => "Album Artist",
            Col::Genre => "Genre",
            Col::Year => "Year",
            Col::PlayCount => "Plays",
            Col::Rating => "Rating",
            Col::LastPlayed => "Last Played",
            Col::AddedAt => "Date Added",
            Col::Composer => "Composer",
            Col::Bitrate => "Bit Rate",
        }
    }

    pub fn sort_key(self) -> Option<SortKey> {
        Some(match self {
            Col::Playing => return None,
            Col::TrackNo => SortKey::TrackNo,
            Col::Title => SortKey::Title,
            Col::Duration => SortKey::Duration,
            Col::Artist => SortKey::Artist,
            Col::Album => SortKey::Album,
            Col::AlbumArtist => SortKey::AlbumArtist,
            Col::Genre => SortKey::Genre,
            Col::Year => SortKey::Year,
            Col::PlayCount => SortKey::PlayCount,
            Col::Rating => SortKey::Rating,
            Col::LastPlayed => SortKey::LastPlayed,
            Col::AddedAt => SortKey::AddedAt,
            Col::Composer => SortKey::Composer,
            Col::Bitrate => SortKey::Bitrate,
        })
    }

    fn column(self) -> TCol {
        match self {
            Col::Playing => TCol::exact(18.0),
            Col::TrackNo => TCol::exact(34.0),
            Col::Title => TCol::initial(210.0).at_least(120.0).clip(true).resizable(true),
            Col::Duration => TCol::exact(52.0),
            Col::Artist => TCol::initial(140.0).at_least(80.0).clip(true).resizable(true),
            Col::Album => TCol::initial(150.0).at_least(80.0).clip(true).resizable(true),
            Col::AlbumArtist => TCol::initial(160.0).at_least(80.0).clip(true).resizable(true),
            Col::Genre => TCol::initial(110.0).at_least(60.0).clip(true).resizable(true),
            Col::Year => TCol::exact(48.0),
            Col::PlayCount => TCol::exact(48.0),
            Col::Rating => TCol::exact(76.0),
            Col::LastPlayed => TCol::initial(100.0).at_least(70.0).clip(true),
            Col::AddedAt => TCol::initial(100.0).at_least(70.0).clip(true),
            Col::Composer => TCol::initial(140.0).at_least(60.0).clip(true).resizable(true),
            Col::Bitrate => TCol::exact(60.0),
        }
    }

    fn text(self, t: &Track) -> String {
        match self {
            Col::Playing => String::new(),
            Col::TrackNo => t.track_no.filter(|n| *n > 0).map(|n| n.to_string()).unwrap_or_default(),
            Col::Title => t.display_title().to_string(),
            Col::Duration => yerg_core::fmt_duration(t.duration_ms),
            Col::Artist => t.artist.clone(),
            Col::Album => t.album.clone(),
            Col::AlbumArtist => t.effective_album_artist().to_string(),
            Col::Genre => t.genre.clone(),
            Col::Year => t.year.filter(|y| *y > 0).map(|y| y.to_string()).unwrap_or_default(),
            Col::PlayCount => {
                if t.play_count > 0 {
                    t.play_count.to_string()
                } else {
                    String::new()
                }
            }
            Col::Rating => stars(t.rating),
            Col::LastPlayed => t.last_played.map(fmt_relative).unwrap_or_default(),
            Col::AddedAt => fmt_relative(t.added_at),
            Col::Composer => t.composer.clone(),
            Col::Bitrate => t.bitrate_kbps.map(|b| format!("{b} kbps")).unwrap_or_default(),
        }
    }

    fn right_aligned(self) -> bool {
        matches!(self, Col::TrackNo | Col::Duration | Col::Year | Col::PlayCount | Col::Bitrate)
    }
}

#[derive(Debug, Clone)]
pub struct TableState {
    pub columns: Vec<Col>,
    pub selection: BTreeSet<usize>,
    pub anchor: Option<usize>,
    pub scroll_to: Option<usize>,
}

impl Default for TableState {
    fn default() -> Self {
        TableState {
            columns: vec![
                Col::Playing,
                Col::Title,
                Col::Duration,
                Col::Artist,
                Col::Album,
                Col::PlayCount,
                Col::Rating,
            ],
            selection: BTreeSet::new(),
            anchor: None,
            scroll_to: None,
        }
    }
}

impl YergApp {
    /// A track table, with its summary line ("N songs · time · size") at the
    /// top.  `header` draws a view's own title above the table; views whose
    /// header already carries the summary pass `summary: false`, so it is
    /// never shown twice.
    pub fn ui_track_list(&mut self, ui: &mut egui::Ui, header: Option<&dyn Fn(&mut egui::Ui, &mut Vec<Action>)>, summary: bool) {
        let mut actions = Vec::new();
        if let Some(h) = header {
            h(ui, &mut actions);
        }
        if summary {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(crate::views::summary_text(crate::views::track_summary(&self.tracks)));
            });
            ui.add_space(4.0);
        }
        self.track_table(ui, &mut actions);
        self.apply_actions(actions);
    }

    pub fn ui_playlist_view(&mut self, ui: &mut egui::Ui, id: PlaylistId) {
        let playlist = self.playlists.iter().find(|p| p.id == id).cloned();
        let sorted = self.sort.is_some();
        let summary = crate::views::track_summary(&self.tracks);
        let header = move |ui: &mut egui::Ui, actions: &mut Vec<Action>| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if let Some(p) = &playlist {
                    ui.label(RichText::new(&p.name).size(20.0).strong());
                    ui.label(crate::views::summary_text(&summary));
                    if crate::app::SHOW_DEVICE && p.sync_to_device {
                        ui.label(crate::views::summary_text("· synced to player"));
                    }
                }
                if sorted && ui.small_button("Playlist order").on_hover_text("Clear column sort").clicked() {
                    actions.push(Action::ResetSort);
                }
            });
            ui.add_space(4.0);
        };
        self.ui_track_list(ui, Some(&header), false);
    }

    /// Just the table; the caller lays out around it.
    pub fn track_table(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tracks = std::mem::take(&mut self.tracks);
        let playing_id = self.now.as_ref().map(|n| n.track.id);
        let is_playing = self.player.state().is_playing();
        let sort = self.effective_sort();
        let columns = self.table.columns.clone();
        let in_playlist = matches!(self.view, View::Playlist(_));
        let playlists: Vec<(PlaylistId, String)> = self.playlists.iter().map(|p| (p.id, p.name.clone())).collect();
        let row_h = 24.0;

        let mut sel = std::mem::take(&mut self.table.selection);
        let mut anchor = self.table.anchor;
        let scroll_to = self.table.scroll_to.take();
        let modifiers = ui.input(|i| i.modifiers);
        let table_height = ui.available_height();

        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .min_scrolled_height(0.0)
            .auto_shrink([false, false]);
        for c in &columns {
            builder = builder.column(c.column());
        }
        builder = builder.column(TCol::remainder());
        if let Some(i) = scroll_to {
            builder = builder.scroll_to_row(i, Some(egui::Align::Center));
        }
        if let Some(a) = &self.autoscroll {
            let max = (tracks.len() as f32 * row_h - table_height + 24.0).max(1.0);
            builder = builder.vertical_scroll_offset((a.start.elapsed().as_secs_f32() * a.speed) % max);
        }

        builder
            .header(24.0, |mut header| {
                for c in &columns {
                    header.col(|ui| {
                        let is_sorted = sort.map(|s| Some(s.key) == c.sort_key()).unwrap_or(false);
                        let arrow = if is_sorted {
                            if sort.unwrap().descending {
                                " ▼"
                            } else {
                                " ▲"
                            }
                        } else {
                            ""
                        };
                        let text = format!("{}{}", c.label(), arrow);
                        let resp = ui.add(
                            egui::Label::new(RichText::new(text).strong().color(if is_sorted {
                                ui.visuals().strong_text_color()
                            } else {
                                ui.visuals().text_color()
                            }))
                            .sense(Sense::click())
                            .truncate(),
                        );
                        if resp.clicked() {
                            if let Some(k) = c.sort_key() {
                                actions.push(Action::Sort(k));
                            }
                        }
                        resp.context_menu(|ui| column_menu(ui, &columns, actions));
                    });
                }
                header.col(|ui| {
                    let resp = ui.add(egui::Label::new("").sense(Sense::click()));
                    resp.context_menu(|ui| column_menu(ui, &columns, actions));
                });
            })
            .body(|body| {
                body.rows(row_h, tracks.len(), |mut row| {
                    let i = row.index();
                    let t = &tracks[i];
                    let selected = sel.contains(&i);
                    row.set_selected(selected);
                    let is_now = playing_id == Some(t.id);
                    for c in &columns {
                        row.col(|ui| {
                            match c {
                                Col::Playing => {
                                    if is_now {
                                        let accent = crate::theme::current().colors.accent;
                                        let marker = match (is_playing, crate::theme::glyph("now_playing")) {
                                            (true, Some(g)) => RichText::new(g).family(egui::FontFamily::Name(crate::theme::ICON_FAMILY.into())),
                                            (true, None) => RichText::new("▶"),
                                            (false, _) => RichText::new("❚❚"),
                                        };
                                        ui.label(marker.color(accent));
                                    }
                                }
                                Col::Rating => {
                                    if let Some(r) = rating_widget(ui, t.rating, selected) {
                                        actions.push(Action::Rate(vec![t.id], r));
                                    }
                                }
                                _ => {
                                    let text = c.text(t);
                                    let mut rt = RichText::new(text);
                                    if is_now {
                                        rt = rt.color(crate::theme::current().colors.playing);
                                    }
                                    if t.missing {
                                        rt = rt.weak().strikethrough();
                                    }
                                    if c.right_aligned() {
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            ui.add(egui::Label::new(rt).truncate());
                                        });
                                    } else {
                                        ui.add(egui::Label::new(rt).truncate());
                                    }
                                }
                            }
                        });
                    }
                    row.col(|_ui| {});

                    let resp = row.response();
                    if resp.double_clicked() {
                        actions.push(Action::PlayIndex(i));
                    } else if resp.clicked() {
                        if modifiers.command {
                            if !sel.remove(&i) {
                                sel.insert(i);
                            }
                            anchor = Some(i);
                        } else if modifiers.shift {
                            let a = anchor.unwrap_or(i);
                            let (lo, hi) = if a <= i { (a, i) } else { (i, a) };
                            sel.extend(lo..=hi);
                        } else {
                            sel.clear();
                            sel.insert(i);
                            anchor = Some(i);
                        }
                    } else if resp.secondary_clicked() && !sel.contains(&i) {
                        sel.clear();
                        sel.insert(i);
                        anchor = Some(i);
                    }
                    resp.context_menu(|ui| {
                        let ids: Vec<TrackId> = if sel.contains(&i) {
                            sel.iter().filter_map(|j| tracks.get(*j).map(|t| t.id)).collect()
                        } else {
                            vec![t.id]
                        };
                        let positions: Vec<usize> = if sel.contains(&i) { sel.iter().copied().collect() } else { vec![i] };
                        let artists = menu_artists(&mut self.menu_artists, &self.lib, t);
                        track_context_menu(ui, t, &artists, ids, positions, in_playlist, i, &playlists, actions);
                    });
                });
            });

        self.table.selection = sel;
        self.table.anchor = anchor;
        self.tracks = tracks;
    }
}

/// The artists of the track whose menu is open, looked up once per track
/// rather than on every frame the menu is shown.
fn menu_artists(cache: &mut Option<(TrackId, Vec<String>)>, lib: &yerg_core::Library, t: &Track) -> Vec<String> {
    if cache.as_ref().map(|(id, _)| *id) != Some(t.id) {
        let found = lib.track_artists(t.id).unwrap_or_default();
        *cache = Some((t.id, if found.is_empty() { vec![t.artist.clone()] } else { found }));
    }
    cache.as_ref().map(|(_, a)| a.clone()).unwrap_or_default()
}

fn column_menu(ui: &mut egui::Ui, columns: &[Col], actions: &mut Vec<Action>) {
    ui.label(RichText::new("Columns").weak().small());
    for c in Col::ALL.iter().skip(1) {
        let mut on = columns.contains(c);
        if ui.checkbox(&mut on, c.label()).changed() {
            actions.push(Action::ToggleColumn(*c));
        }
    }
}

/// Five clickable stars.  Returns the new rating when clicked (clicking the
/// current rating clears it).
pub fn rating_widget(ui: &mut egui::Ui, rating: i32, selected: bool) -> Option<i32> {
    let mut out = None;
    let c = crate::theme::current().colors.clone();
    let color = if selected { c.selection_text } else { c.stars };
    let faint = if selected { c.selection_text.gamma_multiply(0.4) } else { c.stars_empty };
    // The theme can swap the star characters for icon-font glyphs.
    let icon_family = egui::FontFamily::Name(crate::theme::ICON_FAMILY.into());
    let star = |filled: bool| match crate::theme::glyph(if filled { "star" } else { "star_empty" }) {
        Some(g) => RichText::new(g).family(icon_family.clone()),
        None => RichText::new(if filled { "★" } else { "☆" }),
    };
    ui.spacing_mut().item_spacing.x = 0.0;
    for n in 1..=5 {
        let filled = n <= rating;
        let resp = ui.add(egui::Label::new(star(filled).color(if filled { color } else { faint })).sense(Sense::click()));
        if resp.clicked() {
            out = Some(if n == rating { 0 } else { n });
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn track_context_menu(
    ui: &mut egui::Ui,
    t: &Track,
    artists: &[String],
    ids: Vec<TrackId>,
    positions: Vec<usize>,
    in_playlist: bool,
    index: usize,
    playlists: &[(PlaylistId, String)],
    actions: &mut Vec<Action>,
) {
    ui.set_min_width(200.0);
    if ui.button("Play").clicked() {
        actions.push(Action::PlayIndex(index));
        ui.close();
    }
    if ui.button("Play Next").clicked() {
        actions.push(Action::PlayNext(ids.clone()));
        ui.close();
    }
    if ui.button("Add to Up Next").clicked() {
        actions.push(Action::AddToQueue(ids.clone()));
        ui.close();
    }
    ui.separator();
    ui.menu_button("Add to Playlist", |ui| {
        if ui.button("New Playlist…").clicked() {
            actions.push(Action::NewPlaylistWith(ids.clone()));
            ui.close();
        }
        if !playlists.is_empty() {
            ui.separator();
        }
        for (pid, name) in playlists {
            if ui.button(name).clicked() {
                actions.push(Action::AddToPlaylist(*pid, ids.clone()));
                ui.close();
            }
        }
    });
    if in_playlist && ui.button("Remove from Playlist").clicked() {
        actions.push(Action::RemoveFromPlaylist(positions.clone()));
        ui.close();
    }
    ui.menu_button("Rating", |ui| {
        for r in (0..=5).rev() {
            let label = if r == 0 { "None".to_string() } else { stars(r) };
            if ui.button(label).clicked() {
                actions.push(Action::Rate(ids.clone(), r));
                ui.close();
            }
        }
    });
    ui.separator();
    if ui.button(if ids.len() > 1 { "Edit Tags (multiple)…" } else { "Edit Tags…" }).clicked() {
        actions.push(Action::EditTags(ids.clone()));
        ui.close();
    }
    match artists {
        [one] => {
            if ui.button("Go to Artist").clicked() {
                actions.push(Action::OpenArtist(one.clone()));
                ui.close();
            }
        }
        many => {
            ui.menu_button("Go to Artist", |ui| {
                for a in many {
                    if ui.button(a).clicked() {
                        actions.push(Action::OpenArtist(a.clone()));
                        ui.close();
                    }
                }
            });
        }
    }
    if ui.button("Go to Album").clicked() {
        actions.push(Action::OpenAlbum(AlbumKey {
            album: t.album.clone(),
            album_artist: t.effective_album_artist().to_string(),
        }));
        ui.close();
    }
    if ui.button("Show in Folder").clicked() {
        actions.push(Action::ShowInFolder(t.id));
        ui.close();
    }
    ui.separator();
    if ui.button(RichText::new("Remove from Library").color(crate::theme::current().colors.error)).clicked() {
        actions.push(Action::RemoveFromLibrary(ids));
        ui.close();
    }
}
