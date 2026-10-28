pub mod appearance;
pub mod browse;
pub mod device;
pub mod history;
pub mod now_playing;
pub mod queue_panel;
pub mod settings;
pub mod sidebar;
pub mod tag_editor;
pub mod track_table;
pub mod transport;
pub mod windows;

use egui::{Context, RichText, Sense, Stroke, StrokeKind};

use crate::app::{RightPanel, View, YergApp};

/// A "sidebar" icon: a window outline with the left (or right) strip filled
/// while that side is shown.
fn panel_toggle(ui: &mut egui::Ui, left: bool, shown: bool) -> egui::Response {
    let c = crate::theme::current().colors.clone();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), Sense::click());
    let color = if resp.hovered() { c.strong } else { c.icon };
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, c.hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    // A replacement icon or glyph from the theme, else the built-in drawing.
    let name = if left { "sidebar_left" } else { "sidebar_right" };
    if crate::theme::draw_custom_icon(ui, name, rect.shrink(4.0), color) {
        return resp;
    }
    let frame = egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 14.0));
    ui.painter().rect_stroke(frame, 2.0, Stroke::new(1.5_f32, color), StrokeKind::Inside);
    let strip = if left {
        egui::Rect::from_min_max(frame.min, egui::pos2(frame.left() + 6.0, frame.bottom()))
    } else {
        egui::Rect::from_min_max(egui::pos2(frame.right() - 6.0, frame.top()), frame.max)
    };
    if shown {
        ui.painter().rect_filled(strip, 2.0, color);
    } else {
        let x = if left { strip.right() } else { strip.left() };
        ui.painter().vline(x, strip.y_range(), Stroke::new(1.5_f32, color));
    }
    resp
}

impl YergApp {
    pub fn ui_top_bar(&mut self, ctx: &Context) {
        egui::TopBottomPanel::top("top").exact_height(40.0).show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(2.0);
                let tip = if self.show_sidebar { "Hide sidebar" } else { "Show sidebar" };
                if panel_toggle(ui, true, self.show_sidebar).on_hover_text(tip).clicked() {
                    self.show_sidebar = !self.show_sidebar;
                }
                ui.add_space(6.0);
                let title = match &self.view {
                    View::Playlist(id) => self
                        .playlists
                        .iter()
                        .find(|p| p.id == *id)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| "Playlist".into()),
                    v => v.title().to_string(),
                };
                ui.label(RichText::new(title).size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(2.0);
                    let right_shown = self.right_panel != RightPanel::Hidden;
                    let tip = if right_shown { "Hide the right panel" } else { "Show the right panel" };
                    if panel_toggle(ui, false, right_shown).on_hover_text(tip).clicked() {
                        self.toggle_right_side();
                    }
                    ui.add_space(4.0);
                    let now_playing_shown = self.right_panel == RightPanel::NowPlaying;
                    if ui
                        .selectable_label(now_playing_shown, "♪ Now Playing")
                        .on_hover_text(if now_playing_shown { "Hide the now playing panel" } else { "Show the now playing panel" })
                        .clicked()
                    {
                        self.toggle_right_panel(RightPanel::NowPlaying);
                    }
                    if ui.button("⚙").on_hover_text("Settings").clicked() {
                        self.settings_open = true;
                    }
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search  (Ctrl+F)")
                            .desired_width(240.0),
                    );
                    if self.focus_search {
                        resp.request_focus();
                        self.focus_search = false;
                    }
                    if resp.changed() {
                        self.tracks_dirty = true;
                        self.lists_dirty = true;
                    }
                    if !self.search.is_empty() && ui.small_button("✕").clicked() {
                        self.search.clear();
                        self.tracks_dirty = true;
                        self.lists_dirty = true;
                    }
                    if let Some((done, total, _)) = &self.scan_progress {
                        ui.add(
                            egui::ProgressBar::new(if *total > 0 { *done as f32 / *total as f32 } else { 0.0 })
                                .desired_width(140.0)
                                .text(format!("Scanning {done}/{total}")),
                        );
                    }
                });
            });
        });
    }

    pub fn ui_central(&mut self, ctx: &Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin::same(0)))
            .show(ctx, |ui| match self.view.clone() {
                View::Songs | View::RecentlyAdded | View::RecentlyPlayed | View::MostPlayed => {
                    self.ui_track_list(ui, None, true);
                }
                View::Playlist(id) => self.ui_playlist_view(ui, id),
                View::Artists | View::AlbumArtists => self.ui_artists(ui),
                View::Albums => self.ui_albums(ui),
                View::Genres => self.ui_genres(ui),
                View::History => self.ui_history(ui),
                View::Device => self.ui_device(ui),
            });
    }

    pub fn ui_toasts(&mut self, ctx: &Context) {
        self.toasts.retain(|(_, t)| t.elapsed().as_secs_f32() < 5.0);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -84.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for (msg, _) in self.toasts.iter().rev().take(4) {
                    egui::Frame::popup(ui.style())
                        .fill(crate::theme::current().colors.raised)
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.label(msg);
                        });
                }
            });
    }

}

/// "1,234 songs · 3 days, 4 hours · 45.2 GB" for a list of tracks.  Shown
/// once per view, at the top (in the view's header where it has one).
pub fn track_summary(tracks: &[yerg_core::model::Track]) -> String {
    let total_ms: i64 = tracks.iter().map(|t| t.duration_ms).sum();
    let bytes: i64 = tracks.iter().map(|t| t.file_size).sum();
    let n = tracks.len();
    format!(
        "{} {} · {} · {}",
        thousands(n),
        if n == 1 { "song" } else { "songs" },
        yerg_core::fmt_total_time(total_ms),
        yerg_core::fmt_bytes(bytes)
    )
}

/// The style for summary lines: a little larger and brighter than captions.
pub fn summary_text(text: impl Into<String>) -> RichText {
    RichText::new(text).size(crate::theme::current().body_size + 0.5).color(crate::theme::current().colors.text)
}

/// 6973 → "6,973".
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn thousands_separators() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(6973), "6,973");
        assert_eq!(super::thousands(1_234_567), "1,234,567");
    }
}
