//! History view: the play log with scrobble state.  (Up Next lives in the
//! queue panel, `queue_panel.rs`.)

use egui::{RichText, Sense};
use egui_extras::{Column, TableBuilder};
use yerg_core::model::*;

use crate::app::YergApp;
use crate::util::fmt_date;

impl YergApp {
    pub fn ui_history(&mut self, ui: &mut egui::Ui) {
        let week_ago = yerg_core::now_unix() - 7 * 86_400;
        let plays_week = self.lib.play_count_since(week_ago).unwrap_or(0);
        // Only services that are set up get a "waiting" count and a column;
        // listens are still kept for the others in case they are connected later.
        let services = self.scrobble_cfg.configured();
        let pending: Vec<(ScrobbleService, i64)> =
            services.iter().map(|s| (*s, self.lib.pending_count(*s).unwrap_or(0))).collect();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(crate::views::summary_text(format!("{} plays recorded · {} this week", self.stats.plays, plays_week)));
            if self.scrobble_cfg.any_ready() {
                let waiting: Vec<String> = pending.iter().map(|(s, n)| format!("{} {n}", s.label())).collect();
                ui.label(crate::views::summary_text(format!("· waiting: {}", waiting.join(", "))));
                if pending.iter().any(|(_, n)| *n > 0) && ui.small_button("Submit now").clicked() {
                    self.jobs.flush_scrobbles();
                }
            } else {
                ui.label(RichText::new("· no scrobble service connected (Settings › Scrobbling)").weak());
            }
            let st = self.scrobbler.status();
            if let Some(e) = &st.last_error {
                ui.label(RichText::new("⚠").color(crate::theme::current().colors.warning)).on_hover_text(e);
            }
            if ui.small_button("Refresh").clicked() {
                self.history_dirty = true;
            }
        });
        ui.add_space(4.0);
        let history = std::mem::take(&mut self.history);
        let mut delete: Option<i64> = None;
        let mut play: Option<TrackId> = None;
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(Column::exact(130.0))
            .column(Column::initial(240.0).at_least(100.0).clip(true).resizable(true))
            .column(Column::initial(170.0).at_least(80.0).clip(true).resizable(true))
            .column(Column::initial(170.0).at_least(80.0).clip(true).resizable(true))
            .column(Column::exact(70.0));
        for _ in &services {
            table = table.column(Column::exact(100.0));
        }
        table
            .column(Column::remainder())
            .header(24.0, |mut h| {
                let labels = ["Played", "Title", "Artist", "Album", "Source"]
                    .into_iter()
                    .chain(services.iter().map(|s| s.label()))
                    .chain([""]);
                for label in labels {
                    h.col(|ui| {
                        ui.label(RichText::new(label).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, history.len(), |mut row| {
                    let p = &history[row.index()];
                    row.col(|ui| {
                        let mut t = RichText::new(fmt_date(p.play.started_at)).small();
                        if p.play.ts_estimated {
                            t = t.italics();
                        }
                        ui.label(t).on_hover_text(if p.play.ts_estimated { "Estimated time (player clock was unset)" } else { "" });
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&p.title).truncate());
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&p.artist).truncate());
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&p.album).truncate());
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(match p.play.source {
                            PlaySource::Local => "app",
                            PlaySource::Device => "player",
                            PlaySource::Imported => "import",
                        }).small());
                    });
                    for s in &services {
                        row.col(|ui| state_label(ui, p.play.state(*s), p.play.note(*s)));
                    }
                    row.col(|_| {});
                    let resp = row.response();
                    if resp.double_clicked() {
                        play = Some(p.play.track_id);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Play").clicked() {
                            play = Some(p.play.track_id);
                            ui.close();
                        }
                        if ui.button("Delete this play").clicked() {
                            delete = Some(p.play.id);
                            ui.close();
                        }
                    });
                });
            });
        self.history = history;
        if let Some(id) = delete {
            let _ = self.lib.delete_play(id);
            self.history_dirty = true;
            self.lists_dirty = true;
            self.counts_dirty = true;
        }
        if let Some(t) = play {
            self.play_ids(vec![t], 0);
        }
    }
}

fn state_label(ui: &mut egui::Ui, s: ScrobbleState, note: Option<&str>) {
    let (text, color) = match s {
        ScrobbleState::Pending => ("waiting", crate::theme::current().colors.warning),
        ScrobbleState::Done => ("sent", crate::theme::current().colors.success),
        ScrobbleState::Ignored => ("ignored", crate::theme::current().colors.error),
        ScrobbleState::Skipped => ("skipped", crate::theme::current().colors.weak),
    };
    let r = ui.label(RichText::new(text).small().color(color));
    if let Some(n) = note {
        r.on_hover_text(n);
    }
}
