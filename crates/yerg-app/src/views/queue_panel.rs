//! The right-hand Up Next panel, opened by the button at the right end of the
//! transport bar.  Shows the current track and what plays after it.

use egui::{vec2, Align, Align2, Context, FontId, Layout, RichText, Sense, UiBuilder};
use yerg_core::model::{Track, TrackId};

use crate::app::{Action, YergApp};
use crate::util::fmt_secs;

/// How many upcoming tracks the panel lists.  Playing the whole library
/// queues thousands; looking them all up on every track change is wasted work.
pub const QUEUE_PANEL_LIMIT: usize = 200;


/// Drag payload: the dragged entry's position in `Queue::upcoming`.
#[derive(Clone, Copy)]
struct QueueDrag(usize);

/// Scroll the list when a drag is held near its top or bottom edge.
fn autoscroll_while_dragging(ui: &mut egui::Ui) {
    let Some(p) = ui.ctx().pointer_hover_pos() else { return };
    let clip = ui.clip_rect();
    if !clip.x_range().contains(p.x) {
        return;
    }
    let edge = 36.0;
    let delta = if p.y < clip.top() + edge {
        (clip.top() + edge - p.y).min(edge) / 3.0
    } else if p.y > clip.bottom() - edge {
        -(p.y - (clip.bottom() - edge)).min(edge) / 3.0
    } else {
        return;
    };
    ui.scroll_with_delta(vec2(0.0, delta));
    ui.ctx().request_repaint();
}

/// The dragged track's title following the pointer.
fn drag_ghost(ui: &egui::Ui, t: &Track) {
    let Some(p) = ui.ctx().pointer_interact_pos() else { return };
    egui::Area::new(egui::Id::new("queue_drag_ghost"))
        .order(egui::Order::Tooltip)
        .fixed_pos(p + vec2(14.0, 6.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(RichText::new(t.display_title()).strong());
            });
        });
}

/// The upcoming tracks as last looked up, keyed by their ids so the panel only
/// queries the library when the queue actually changes.
#[derive(Default)]
pub struct QueuePanelCache {
    ids: Vec<TrackId>,
    /// (position in the queue's upcoming list, track).  Tracks no longer in
    /// the library are skipped but positions stay right for jump and remove.
    rows: Vec<(usize, Track)>,
}

impl YergApp {
    pub fn ui_queue_panel(&mut self, ctx: &Context) {
        let upcoming = self.queue.upcoming();
        let total = upcoming.len();
        let shown: Vec<TrackId> = upcoming.iter().take(QUEUE_PANEL_LIMIT).copied().collect();
        if shown != self.queue_panel.ids {
            let rows = shown
                .iter()
                .enumerate()
                .filter_map(|(i, id)| self.lib.track(*id).ok().flatten().map(|t| (i, t)))
                .collect();
            self.queue_panel = QueuePanelCache { ids: shown, rows };
        }

        egui::SidePanel::right("queue_panel")
            .resizable(true)
            .default_width(300.0)
            .width_range(240.0..=560.0)
            .show(ctx, |ui| {
                let mut actions = Vec::new();
                let rows = std::mem::take(&mut self.queue_panel.rows);

                // The current track is in the transport bar right below, so
                // the panel starts with what comes next.
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("UP NEXT").weak().small().strong());
                    if total > 0 {
                        let ms: i64 = rows.iter().map(|(_, t)| t.duration_ms).sum();
                        let time = yerg_core::fmt_total_time(ms);
                        let more = if total > rows.len() { "+" } else { "" };
                        ui.label(RichText::new(format!("{total} tracks · {time}{more}")).weak().small());
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if total > 0 && ui.small_button("Clear").clicked() {
                            actions.push(Action::ClearQueue);
                        }
                    });
                });
                ui.separator();

                if total == 0 {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Nothing queued. Right-click a track and choose Play Next or Add to Up Next.").weak(),
                    );
                } else {
                    let row_h = 44.0;
                    let extra = total.saturating_sub(QUEUE_PANEL_LIMIT);
                    let dragging = egui::DragAndDrop::payload::<QueueDrag>(ui.ctx()).map(|p| p.0);
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
                        ui,
                        row_h,
                        rows.len() + usize::from(extra > 0),
                        |ui, range| {
                            if dragging.is_some() {
                                autoscroll_while_dragging(ui);
                            }
                            for k in range {
                                let Some((i, t)) = rows.get(k) else {
                                    ui.label(RichText::new(format!("and {extra} more")).weak().small());
                                    continue;
                                };
                                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
                                let being_dragged = dragging == Some(*i);
                                if being_dragged {
                                    ui.painter().rect_filled(rect, 4.0, crate::theme::current().colors.surface);
                                } else if resp.hovered() && dragging.is_none() {
                                    ui.painter().rect_filled(rect, 4.0, crate::theme::current().colors.hover);
                                }
                                ui.scope(|ui| {
                                    if being_dragged {
                                        ui.set_opacity(0.4);
                                    }
                                    self.track_row(ui, rect, t, false);
                                });

                                if resp.drag_started() {
                                    resp.dnd_set_drag_payload(QueueDrag(*i));
                                }
                                if resp.dragged() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                    drag_ghost(ui, t);
                                }
                                // Drop target: top half inserts before this entry, bottom half after it.
                                if let Some(from) = resp.dnd_hover_payload::<QueueDrag>().map(|p| p.0) {
                                    let after = ui.ctx().pointer_hover_pos().map(|p| p.y > rect.center().y).unwrap_or(false);
                                    let y = if after { rect.bottom() } else { rect.top() };
                                    if from != *i {
                                        ui.painter().hline(rect.x_range(), y, egui::Stroke::new(2.0_f32, crate::theme::current().colors.accent));
                                    }
                                    if resp.dnd_release_payload::<QueueDrag>().is_some() {
                                        actions.push(Action::MoveQueue(from, if after { *i + 1 } else { *i }));
                                    }
                                }

                                if resp.double_clicked() {
                                    actions.push(Action::JumpQueue(*i));
                                }
                                let resp = if dragging.is_none() { resp.on_hover_text("Double-click to play, drag to reorder") } else { resp };
                                resp.context_menu(|ui| {
                                    if ui.button("Play Now").clicked() {
                                        actions.push(Action::JumpQueue(*i));
                                        ui.close();
                                    }
                                    if ui.button("Remove from Up Next").clicked() {
                                        actions.push(Action::RemoveQueue(*i));
                                        ui.close();
                                    }
                                });
                            }
                        },
                    );
                }

                self.queue_panel.rows = rows;
                self.apply_actions(actions);
            });
    }

    /// One track: small cover, title and artist, length on the right.
    fn track_row(&mut self, ui: &mut egui::Ui, rect: egui::Rect, t: &Track, large: bool) {
        let art = if large { 44.0 } else { 36.0 };
        let art_rect = egui::Rect::from_min_size(rect.left_top() + vec2(2.0, (rect.height() - art) / 2.0), vec2(art, art));
        self.paint_art(ui, art_rect, t.art_hash.as_deref(), false);
        let time_w = 40.0;
        ui.painter().text(
            egui::pos2(rect.right() - 4.0, rect.center().y),
            Align2::RIGHT_CENTER,
            fmt_secs(t.duration_ms as f64 / 1000.0),
            FontId::monospace(11.0),
            crate::theme::current().colors.weak,
        );
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(art_rect.right() + 10.0, rect.top()),
            egui::pos2(rect.right() - time_w - 4.0, rect.bottom()),
        );
        let mut text_ui = ui.new_child(UiBuilder::new().max_rect(text_rect).layout(Layout::top_down(Align::Min)));
        text_ui.add_space((rect.height() - if large { 36.0 } else { 32.0 }) / 2.0);
        text_ui.spacing_mut().item_spacing.y = 2.0;
        let title = RichText::new(t.display_title()).size(if large { 14.0 } else { 13.0 });
        text_ui.add(egui::Label::new(if large { title.strong() } else { title }).truncate().selectable(false));
        text_ui.add(egui::Label::new(RichText::new(&t.artist).weak().small()).truncate().selectable(false));
    }
}
