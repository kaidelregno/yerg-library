//! Floating windows and modal prompts.

use egui::{Context, RichText};

use crate::app::YergApp;
use crate::views::tag_editor::EditorResult;

impl YergApp {
    pub fn ui_windows(&mut self, ctx: &Context) {
        // Settings
        if self.settings_open {
            let mut open = true;
            egui::Window::new("Settings")
                .open(&mut open)
                .default_width(620.0)
                .default_height(520.0)
                .collapsible(false)
                .vscroll(true)
                .show(ctx, |ui| self.ui_settings_contents(ui));
            if !open {
                self.settings_open = false;
                self.settings.first_run = false;
            }
        }

        // Tag editor
        if self.tag_editor.is_some() {
            let mut open = true;
            let mut result = EditorResult::Open;
            egui::Window::new("Edit Tags")
                .open(&mut open)
                .default_width(680.0)
                .collapsible(false)
                .vscroll(true)
                .show(ctx, |ui| {
                    result = self.ui_tag_editor_contents(ui);
                });
            match result {
                EditorResult::Open => {
                    if !open {
                        self.tag_editor = None;
                    }
                }
                EditorResult::Cancel => self.tag_editor = None,
                EditorResult::Save => self.commit_tag_editor(),
            }
        }

        // Text prompt
        if let Some(p) = self.prompt.as_mut() {
            let mut decision: Option<bool> = None;
            egui::Window::new(p.title.clone())
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -60.0))
                .show(ctx, |ui| {
                    let resp = ui.add(egui::TextEdit::singleline(&mut p.value).desired_width(300.0));
                    resp.request_focus();
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || enter {
                            decision = Some(true);
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            decision = Some(false);
                        }
                    });
                });
            if let Some(d) = decision {
                self.resolve_prompt(d);
            }
        }

        // Confirmation
        if let Some(c) = &self.confirm {
            let mut decision: Option<bool> = None;
            let title = c.title.clone();
            let body = c.body.clone();
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -60.0))
                .show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(body);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Continue").strong()).clicked() {
                            decision = Some(true);
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            decision = Some(false);
                        }
                    });
                });
            if let Some(d) = decision {
                self.resolve_confirm(d);
            }
        }
    }
}
