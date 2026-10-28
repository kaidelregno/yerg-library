//! Bottom transport bar: song info, controls, volume and a centred seek bar.
//!
//! ```text
//!  ▣ Title             ⤮ ↻          ⏮   ▶/❚❚   ⏭       🔊 ━━●── ☰
//!  ▣ Artist — Album  1:40 ━━━━━━━━━━━━●────────────────────── 3:10
//! ```
//!
//! Cover, title and "artist — album" on the left (click the cover for Now
//! Playing).  The seek bar is centred and half the window wide, so its ends
//! are easy to reach, with the elapsed time and the length at its ends.  The
//! controls sit in a row just above it and move with its ends as the window
//! resizes: shuffle and repeat above the left end, previous / play-pause /
//! next in the middle, and volume (speaker to mute, slider, scroll wheel)
//! with the Up Next button beside it above the right end.  Icons are drawn as
//! vector shapes so they look the same on every platform and font.

use std::f32::consts::{FRAC_PI_2, PI};

use egui::{pos2, vec2, Align, Align2, Color32, Context, CursorIcon, FontId, Layout, Painter, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, UiBuilder};

use crate::app::{Action, RightPanel, YergApp};
use crate::queue::Repeat;
use crate::util::fmt_secs;

/// The theme's colours (see `theme.rs`).
fn colors() -> crate::theme::Colors {
    crate::theme::current().colors.clone()
}

const BAR_HEIGHT: f32 = 76.0;
/// Centre of the control row, from the top of the panel's content area.
const CONTROLS_Y: f32 = 26.0;
/// Seek bar, from the bottom of the content area.
const SEEK_FROM_BOTTOM: f32 = 12.0;
/// Cover size on the left.
const ART: f32 = 48.0;

#[derive(Clone, Copy)]
enum Icon {
    Shuffle,
    Repeat,
    RepeatOne,
    Prev,
    Play,
    Pause,
    Next,
    Queue,
    Volume(f32),
}

impl YergApp {
    pub fn ui_transport(&mut self, ctx: &Context) {
        egui::TopBottomPanel::bottom("transport").exact_height(BAR_HEIGHT).show(ctx, |ui| {
            let st = self.player.state();
            let mut actions = Vec::new();
            let r = ui.max_rect();
            let ctrl_y = r.top() + CONTROLS_Y;
            // The seek bar: centred and half the window wide, so its ends stay
            // away from the screen edges.  Shuffle and repeat sit above its
            // left end; volume and Up Next sit beside its right end, after the
            // length, so a narrow window shortens the bar to leave them room.
            let cx = r.center().x;
            // Up Next is a square about two thirds of the bar height.  Right of the seek
            // bar: length label (74), speaker and slider (143), Up Next, margin.
            let queue_side = (r.height() * 2.0 / 3.0).round();
            let right_group_w = 74.0 + 143.0 + queue_side + 6.0;
            let half = (r.width() * 0.25).max(150.0).min(r.width() / 2.0 - right_group_w).max(120.0);
            let bar_left = cx - half;
            let bar_right = cx + half;

            // ---- Left: cover, title, artist — album.
            let info_w = (r.width() * 0.24).clamp(150.0, 300.0).min(bar_left - r.left() - 16.0);
            // The seek bar is only under the centre, so the cover and text use
            // the bar's full height.
            let info_y = r.center().y;
            let art_rect = Rect::from_center_size(pos2(r.left() + 2.0 + ART / 2.0, info_y), vec2(ART, ART));
            let (hash, title, artists, album) = match &self.now {
                Some(n) => {
                    let album = (!n.track.album.is_empty()).then(|| yerg_core::model::AlbumKey {
                        album: n.track.album.clone(),
                        album_artist: n.track.effective_album_artist().to_string(),
                    });
                    let artists: Vec<String> = n.artists.iter().filter(|a| !a.is_empty()).cloned().collect();
                    (n.track.art_hash.clone(), n.track.display_title().to_string(), artists, album)
                }
                None => (None, String::new(), Vec::new(), None),
            };
            self.paint_art(ui, art_rect, hash.as_deref(), false);
            let art_resp = ui.interact(art_rect, ui.id().with("transport_art"), Sense::click());
            if self.now.is_some() {
                let art_resp = art_resp.on_hover_text("Show Now Playing").on_hover_cursor(CursorIcon::PointingHand);
                if art_resp.clicked() {
                    self.toggle_right_panel(RightPanel::NowPlaying);
                }
            }
            let text_rect = Rect::from_min_max(
                pos2(art_rect.right() + 12.0, info_y - 18.0),
                pos2(r.left() + info_w, info_y + 18.0),
            );
            let mut text_ui = ui.new_child(UiBuilder::new().max_rect(text_rect).layout(Layout::top_down(Align::Min)));
            text_ui.spacing_mut().item_spacing.y = 2.0;
            if self.now.is_some() {
                text_ui.add(egui::Label::new(RichText::new(&title).strong().size(14.0)).truncate().selectable(false));
                // "Artist, Artist — Album", each name a link (clipped to the space).
                text_ui.horizontal(|ui| {
                    ui.set_clip_rect(text_rect.intersect(ui.clip_rect()));
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (i, a) in artists.iter().enumerate() {
                        if i > 0 {
                            ui.label(RichText::new(", ").size(12.0).color(colors().weak));
                        }
                        if link(ui, a).on_hover_text(format!("Go to {a}")).clicked() {
                            actions.push(Action::OpenArtist(a.clone()));
                        }
                    }
                    if let Some(k) = &album {
                        if !artists.is_empty() {
                            ui.label(RichText::new(" — ").size(12.0).color(colors().weak));
                        }
                        if link(ui, &k.album).on_hover_text("Go to album").clicked() {
                            actions.push(Action::OpenAlbum(k.clone()));
                        }
                    }
                });
            } else {
                text_ui.add_space(10.0);
                text_ui.label(RichText::new("Not playing").weak());
            }

            // ---- Shuffle and repeat, just above the seek bar's left end (a
            // little further left in a narrow window, to keep clear of Previous,
            // which sits at cx - 54 and is 24 wide).
            let prev_left = cx - 54.0 - 12.0;
            let shuffle_x = (bar_left + 12.0).min(prev_left - 20.0 - 52.0);
            let shuffle_rect = Rect::from_center_size(pos2(shuffle_x, ctrl_y), vec2(24.0, 24.0));
            let shuffle = self.queue.shuffle;
            if icon_button(ui, "shuffle", shuffle_rect, Icon::Shuffle, shuffle, if shuffle { "Shuffle: on" } else { "Shuffle: off" }).clicked() {
                self.queue.set_shuffle(!shuffle);
            }
            let repeat_rect = shuffle_rect.translate(vec2(40.0, 0.0));
            let (repeat_icon, repeat_on) = match self.queue.repeat {
                Repeat::Off => (Icon::Repeat, false),
                Repeat::All => (Icon::Repeat, true),
                Repeat::One => (Icon::RepeatOne, true),
            };
            if icon_button(ui, "repeat", repeat_rect, repeat_icon, repeat_on, &format!("Repeat: {}", self.queue.repeat.as_str()))
                .clicked()
            {
                self.queue.repeat = self.queue.repeat.next();
            }

            // ---- Centre: previous, play/pause, next.
            let playing = st.loaded && !st.paused;
            let play_rect = Rect::from_center_size(pos2(cx, ctrl_y), vec2(30.0, 30.0));
            if icon_button(ui, "play", play_rect, if playing { Icon::Pause } else { Icon::Play }, false, "Play/Pause (Space)").clicked() {
                self.toggle_play();
            }
            let prev_rect = Rect::from_center_size(pos2(cx - 54.0, ctrl_y), vec2(24.0, 24.0));
            if icon_button(ui, "prev", prev_rect, Icon::Prev, false, "Previous (Ctrl+←)").clicked() {
                self.prev_track();
            }
            let next_rect = Rect::from_center_size(pos2(cx + 54.0, ctrl_y), vec2(24.0, 24.0));
            if icon_button(ui, "next", next_rect, Icon::Next, false, "Next (Ctrl+→)").clicked() {
                self.next_track(true);
            }

            // ---- Volume and Up Next, beside the seek bar's right end (after
            // the length label), centred on the bar: [speaker][slider] [Up Next].
            // Up Next is nearly the bar's full height.
            let group_y = info_y;
            let group_left = bar_right + 74.0;
            let queue_rect = Rect::from_min_size(pos2(group_left + 143.0, group_y - queue_side / 2.0), vec2(queue_side, queue_side));
            let queue_open = self.right_panel == RightPanel::Queue;
            if icon_button(ui, "queue", queue_rect, Icon::Queue, queue_open, if queue_open { "Hide Up Next" } else { "Up Next" })
                .clicked()
            {
                self.toggle_right_panel(RightPanel::Queue);
            }
            let vol = st.volume;
            let slider = Rect::from_min_max(pos2(group_left + 35.0, group_y - 8.0), pos2(group_left + 125.0, group_y + 8.0));
            // Far enough left that a muted knob (at the slider's start) clears the icon.
            let speaker_rect = Rect::from_center_size(pos2(group_left + 11.0, group_y), vec2(22.0, 22.0));
            let mute_tip = if vol > 0.001 { format!("Mute ({:.0}%)", vol * 100.0) } else { "Unmute".to_string() };
            let speaker = icon_button(ui, "volume", speaker_rect, Icon::Volume(vol), false, &mute_tip);
            if speaker.clicked() {
                if vol > 0.001 {
                    self.volume_before_mute = vol;
                    actions.push(Action::SetVolume(0.0));
                } else {
                    actions.push(Action::SetVolume(self.volume_before_mute.max(0.05)));
                }
            }
            let vol_resp = ui.interact(slider, ui.id().with("volume_slider"), Sense::click_and_drag());
            if vol_resp.is_pointer_button_down_on() || vol_resp.dragged() {
                if let Some(p) = vol_resp.interact_pointer_pos() {
                    let v = ((p.x - slider.left()) / slider.width()).clamp(0.0, 1.0);
                    actions.push(Action::SetVolume(v));
                }
            }
            if vol_resp.hovered() || speaker.hovered() {
                let dy = ui.input(|i| i.raw_scroll_delta.y);
                if dy != 0.0 {
                    actions.push(Action::SetVolume((vol + dy / 50.0 * 0.05).clamp(0.0, 1.0)));
                }
            }
            let vol_resp = vol_resp.on_hover_text(format!("Volume {:.0}%", vol * 100.0));
            let shown_vol = actions
                .iter()
                .rev()
                .find_map(|a| if let Action::SetVolume(v) = a { Some(*v) } else { None })
                .unwrap_or(vol);
            paint_slider(ui.painter(), slider, shown_vol, vol_resp.hovered() || vol_resp.dragged(), 2.0);

            if let Some(err) = &st.output_error {
                let warn_rect = speaker_rect.translate(vec2(-26.0, 0.0));
                let resp = ui.interact(warn_rect, ui.id().with("output_warning"), Sense::hover()).on_hover_text(err);
                ui.painter().text(resp.rect.center(), Align2::CENTER_CENTER, "⚠", FontId::proportional(15.0), colors().warning);
            }

            // ---- Seek bar, times at its ends.
            let duration_s = self
                .now
                .as_ref()
                .map(|n| n.track.duration_ms as f64 / 1000.0)
                .filter(|d| *d > 0.0)
                .or_else(|| st.duration.map(|d| d.as_secs_f64()))
                .unwrap_or(0.0);
            let seekable = st.loaded && duration_s > 0.0;
            let seek_y = r.bottom() - SEEK_FROM_BOTTOM;
            let seek_rect = Rect::from_min_max(pos2(bar_left, seek_y - 7.0), pos2(bar_right, seek_y + 7.0));
            let seek = ui.interact(seek_rect, ui.id().with("seek"), if seekable { Sense::click_and_drag() } else { Sense::hover() });
            let fraction_at = |x: f32| ((x - bar_left) / (bar_right - bar_left)).clamp(0.0, 1.0) as f64;
            if seekable {
                if seek.hovered() || seek.dragged() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if seek.is_pointer_button_down_on() || seek.dragged() {
                    if let Some(p) = seek.interact_pointer_pos() {
                        self.seek_preview = Some(fraction_at(p.x) * duration_s);
                    }
                }
                if seek.clicked() || seek.drag_stopped() {
                    if let Some(v) = self.seek_preview.take() {
                        actions.push(Action::Seek(v));
                    }
                }
            } else {
                self.seek_preview = None;
            }
            let pos_s = self.seek_preview.unwrap_or_else(|| st.position_now().as_secs_f64()).min(duration_s.max(0.0));
            // Redraw about as often as the knob moves a pixel: up to 60 times a
            // second on a very short track, every 200 ms (as before) on a song.
            if seekable && st.is_playing() {
                let secs_per_px = duration_s / f64::from((bar_right - bar_left).max(1.0));
                if secs_per_px < 0.03 {
                    // Faster than ~30 px a second: redraw every screen refresh.
                    // (A timed redraw this short is stretched by the system
                    // timer, which made the knob jump.)
                    ui.ctx().request_repaint();
                } else {
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(secs_per_px.min(0.2)));
                }
            }
            let seek_rect_line = Rect::from_min_max(pos2(bar_left, seek_y - 1.0), pos2(bar_right, seek_y + 1.0));
            if seekable {
                let active = seek.hovered() || seek.dragged() || self.seek_preview.is_some();
                paint_slider(ui.painter(), seek_rect_line, (pos_s / duration_s) as f32, active, 3.0);
            } else {
                ui.painter().line_segment([pos2(bar_left, seek_y), pos2(bar_right, seek_y)], Stroke::new(3.0_f32, colors().track));
            }
            if st.loaded || self.now.is_some() {
                let font = FontId::monospace(crate::theme::current().mono_size - 1.5);
                ui.painter().text(pos2(bar_left - 10.0, seek_y), Align2::RIGHT_CENTER, fmt_secs(pos_s), font.clone(), colors().text);
                ui.painter().text(pos2(bar_right + 10.0, seek_y), Align2::LEFT_CENTER, fmt_secs(duration_s), font, colors().text);
            }
            self.apply_actions(actions);
        });
    }
}

/// Small text that is a link: underlined, brighter and a pointing hand while
/// hovered.
fn link(ui: &mut egui::Ui, text: &str) -> Response {
    let c = colors();
    let resp = ui.add(
        egui::Label::new(RichText::new(text).size(12.0).color(c.weak))
            .sense(Sense::click())
            .selectable(false),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        let y = resp.rect.bottom() - 1.0;
        ui.painter().hline(resp.rect.x_range(), y, Stroke::new(1.0_f32, c.text));
    }
    resp
}

/// A horizontal bar filled to `fraction` with a round knob (seek and volume).
fn paint_slider(p: &Painter, rect: Rect, fraction: f32, active: bool, thickness: f32) {
    let c = colors();
    let y = rect.center().y;
    let x = rect.left() + rect.width() * fraction.clamp(0.0, 1.0);
    p.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(thickness, c.track));
    p.line_segment([pos2(rect.left(), y), pos2(x, y)], Stroke::new(thickness, if active { c.accent } else { c.icon }));
    p.circle_filled(pos2(x, y), if active { 6.0 } else { 5.0 }, c.knob);
}

impl Icon {
    /// The name a replacement icon file or glyph uses (see `theme::ICON_NAMES`).
    fn name(self) -> &'static str {
        match self {
            Icon::Shuffle => "shuffle",
            Icon::Repeat => "repeat",
            Icon::RepeatOne => "repeat_one",
            Icon::Prev => "previous",
            Icon::Play => "play",
            Icon::Pause => "pause",
            Icon::Next => "next",
            Icon::Queue => "queue",
            Icon::Volume(l) if l <= 0.001 => "volume_muted",
            Icon::Volume(_) => "volume",
        }
    }
}

/// A clickable icon.  `active` draws it in the accent colour (shuffle on,
/// repeat on, Up Next open).  The theme can resize it and replace it with an
/// image or a font glyph.
fn icon_button(ui: &mut egui::Ui, id: &str, rect: Rect, icon: Icon, active: bool, tip: &str) -> Response {
    let c = colors();
    let rect = Rect::from_center_size(rect.center(), rect.size() * crate::theme::current().icon_scale);
    let resp = ui.interact(rect, ui.id().with(("transport", id)), Sense::click()).on_hover_text(tip);
    let color = if resp.is_pointer_button_down_on() {
        c.weak
    } else if active {
        c.accent
    } else if resp.hovered() {
        c.strong
    } else {
        c.icon
    };
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if !crate::theme::draw_custom_icon(ui, icon.name(), rect, color) {
        paint_icon(ui.painter(), rect, icon, color);
    }
    resp
}

/// Point in the icon's unit square (0..1 on both axes, y down).
fn at(rect: Rect, x: f32, y: f32) -> Pos2 {
    pos2(rect.left() + x * rect.width(), rect.top() + y * rect.height())
}

fn fill(p: &Painter, rect: Rect, pts: &[(f32, f32)], c: Color32) {
    p.add(Shape::convex_polygon(pts.iter().map(|(x, y)| at(rect, *x, *y)).collect(), c, Stroke::NONE));
}

fn line(p: &Painter, rect: Rect, pts: &[(f32, f32)], width: f32, c: Color32) {
    p.add(Shape::line(pts.iter().map(|(x, y)| at(rect, *x, *y)).collect(), Stroke::new(width, c)));
}

/// Points along a circular arc in unit coordinates (angles in radians, y down).
fn arc_points(cx: f32, cy: f32, radius: f32, from: f32, to: f32, steps: usize) -> Vec<(f32, f32)> {
    (0..=steps)
        .map(|i| {
            let a = from + (to - from) * i as f32 / steps as f32;
            (cx + radius * a.cos(), cy + radius * a.sin())
        })
        .collect()
}

fn paint_icon(p: &Painter, rect: Rect, icon: Icon, c: Color32) {
    // Line weight grows with the icon, up to a limit (Up Next is large).
    let w = (rect.width() / 12.0).clamp(1.6, 4.0);
    match icon {
        Icon::Play => fill(p, rect, &[(0.24, 0.1), (0.9, 0.5), (0.24, 0.9)], c),
        Icon::Pause => {
            fill(p, rect, &[(0.2, 0.1), (0.42, 0.1), (0.42, 0.9), (0.2, 0.9)], c);
            fill(p, rect, &[(0.58, 0.1), (0.8, 0.1), (0.8, 0.9), (0.58, 0.9)], c);
        }
        Icon::Prev => {
            fill(p, rect, &[(0.1, 0.16), (0.2, 0.16), (0.2, 0.84), (0.1, 0.84)], c);
            fill(p, rect, &[(0.2, 0.5), (0.55, 0.16), (0.55, 0.84)], c);
            fill(p, rect, &[(0.55, 0.5), (0.9, 0.16), (0.9, 0.84)], c);
        }
        Icon::Next => {
            fill(p, rect, &[(0.8, 0.16), (0.9, 0.16), (0.9, 0.84), (0.8, 0.84)], c);
            fill(p, rect, &[(0.8, 0.5), (0.45, 0.16), (0.45, 0.84)], c);
            fill(p, rect, &[(0.45, 0.5), (0.1, 0.16), (0.1, 0.84)], c);
        }
        Icon::Shuffle => {
            line(p, rect, &[(0.08, 0.28), (0.32, 0.28), (0.62, 0.72), (0.8, 0.72)], w, c);
            line(p, rect, &[(0.08, 0.72), (0.32, 0.72), (0.62, 0.28), (0.8, 0.28)], w, c);
            fill(p, rect, &[(0.76, 0.14), (0.96, 0.28), (0.76, 0.42)], c);
            fill(p, rect, &[(0.76, 0.58), (0.96, 0.72), (0.76, 0.86)], c);
        }
        Icon::Repeat | Icon::RepeatOne => {
            // The usual repeat glyph: two rounded half-loops, the top one
            // running right into an arrowhead, the bottom one running left.
            let mut top = vec![(0.14, 0.62)];
            top.extend(arc_points(0.30, 0.46, 0.16, PI, PI + FRAC_PI_2, 6));
            top.push((0.70, 0.30));
            line(p, rect, &top, w, c);
            fill(p, rect, &[(0.66, 0.16), (0.88, 0.30), (0.66, 0.44)], c);
            let mut bottom = vec![(0.86, 0.38)];
            bottom.extend(arc_points(0.70, 0.54, 0.16, 0.0, FRAC_PI_2, 6));
            bottom.push((0.30, 0.70));
            line(p, rect, &bottom, w, c);
            fill(p, rect, &[(0.34, 0.56), (0.12, 0.70), (0.34, 0.84)], c);
            if matches!(icon, Icon::RepeatOne) {
                p.text(at(rect, 0.5, 0.5), Align2::CENTER_CENTER, "1", FontId::proportional(rect.height() * 0.34), c);
            }
        }
        Icon::Queue => {
            fill(p, rect, &[(0.08, 0.12), (0.92, 0.12), (0.92, 0.27), (0.08, 0.27)], c);
            for y in [0.43, 0.59, 0.75, 0.91] {
                line(p, rect, &[(0.08, y), (0.92, y)], w, c);
            }
        }
        Icon::Volume(level) => {
            fill(p, rect, &[(0.08, 0.36), (0.3, 0.36), (0.3, 0.64), (0.08, 0.64)], c);
            fill(p, rect, &[(0.3, 0.36), (0.55, 0.14), (0.55, 0.86), (0.3, 0.64)], c);
            if level <= 0.001 {
                line(p, rect, &[(0.68, 0.36), (0.92, 0.64)], w, c);
                line(p, rect, &[(0.68, 0.64), (0.92, 0.36)], w, c);
            } else {
                line(p, rect, &arc_points(0.55, 0.5, 0.17, -0.85, 0.85, 10), w, c);
                line(p, rect, &arc_points(0.55, 0.5, 0.33, -0.85, 0.85, 10), w, if level > 0.5 { c } else { colors().icon_dim });
            }
        }
    }
}
