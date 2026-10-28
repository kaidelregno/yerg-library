//! Settings › Appearance: a front-end for the appearance dotfiles.
//!
//! The files are the source of truth (see `theme.rs`); this tab shows where
//! they are, switches themes, reports mistakes and lists what can be used.

use egui::RichText;

use crate::app::YergApp;
use crate::theme;

#[derive(Default)]
pub struct AppearanceUi {
    pub font_filter: String,
    /// Installed font families, read the first time the list is opened.
    pub fonts: Option<Vec<String>>,
}

impl YergApp {
    pub fn settings_appearance(&mut self, ui: &mut egui::Ui) {
        let t = theme::current();
        let c = t.colors.clone();
        ui.label(
            RichText::new(
                "Colours, fonts, sizes and icons come from plain-text files, like dotfiles. \
                 Edit them in any editor: changes apply as soon as you save.",
            )
            .weak(),
        );
        ui.add_space(6.0);

        let dir = theme::config_dir();
        ui.horizontal(|ui| {
            ui.label("Config folder");
            ui.label(RichText::new(dir.display().to_string()).monospace());
        });
        ui.horizontal(|ui| {
            if ui.button("Open folder").clicked() {
                let _ = open::that(&dir);
            }
            if ui.button("Edit yerg.toml").clicked() {
                let _ = open::that(theme::config_path());
            }
            let theme_file = theme::themes_dir().join(format!("{}.toml", self.selected_theme_name()));
            if ui
                .add_enabled(theme_file.is_file(), egui::Button::new("Edit theme file"))
                .on_disabled_hover_text("The pywal theme comes from ~/.cache/wal/colors.json")
                .clicked()
            {
                let _ = open::that(&theme_file);
            }
            if ui.button("Reload").on_hover_text("Read the files again now").clicked() {
                let ctx = ui.ctx().clone();
                self.reload_theme(&ctx, true);
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Theme").strong());
            let current = self.selected_theme_name();
            let mut pick: Option<String> = None;
            egui::ComboBox::from_id_salt("theme_pick").selected_text(&current).show_ui(ui, |ui| {
                for name in theme::available_themes() {
                    if ui.selectable_label(name == current, &name).clicked() {
                        pick = Some(name);
                    }
                }
            });
            ui.label(RichText::new(format!("({})", t.name)).weak());
            if let Some(name) = pick {
                match theme::select_theme(&name) {
                    Ok(()) => {
                        let ctx = ui.ctx().clone();
                        self.reload_theme(&ctx, true);
                    }
                    Err(e) => self.toast(e),
                }
            }
        });
        ui.label(
            RichText::new("Sets `theme = …` in yerg.toml, keeping the rest of the file. Add your own themes as .toml files in themes/.")
                .weak()
                .small(),
        );

        ui.add_space(8.0);
        ui.label(RichText::new("In use").strong());
        let font = |f: &Option<theme::LoadedFont>| f.as_ref().map(|f| f.label.clone()).unwrap_or_else(|| "built-in".into());
        egui::Grid::new("appearance_now").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            ui.label("Font");
            ui.label(format!(
                "{} · body {} · small {} · heading {} pt",
                font(&t.ui_font),
                t.body_size,
                t.small_size,
                t.heading_size
            ));
            ui.end_row();
            ui.label("Monospace");
            ui.label(format!("{} · {} pt", font(&t.mono_font), t.mono_size));
            ui.end_row();
            ui.label("Zoom");
            ui.label(format!("{:.0}%", t.scale * 100.0));
            ui.end_row();
            ui.label("Icons");
            ui.label(format!(
                "size {:.0}% · {} replaced by files · {} by glyphs{}",
                t.icon_scale * 100.0,
                t.icon_files.len(),
                t.glyphs.len(),
                t.icon_font.as_ref().map(|f| format!(" ({})", f.label)).unwrap_or_default()
            ));
            ui.end_row();
        });

        ui.add_space(8.0);
        if self.theme_errors.is_empty() {
            ui.label(RichText::new("✔ No problems in your files.").color(c.success));
        } else {
            ui.label(RichText::new(format!("Problems in your files ({}): the rest still applies.", self.theme_errors.len())).color(c.warning));
            for e in &self.theme_errors {
                ui.label(RichText::new(format!("• {e}")).color(c.error));
            }
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new("Installed fonts").id_salt("fonts_list").show(ui, |ui| {
            let fonts = self.appearance.fonts.get_or_insert_with(theme::system_font_families).clone();
            ui.horizontal(|ui| {
                ui.label("Filter");
                ui.text_edit_singleline(&mut self.appearance.font_filter);
            });
            ui.label(RichText::new("Click a name to copy its line for the [font] section.").weak().small());
            let filter = self.appearance.font_filter.to_lowercase();
            egui::ScrollArea::vertical().max_height(180.0).id_salt("fonts_scroll").show(ui, |ui| {
                for f in fonts.iter().filter(|f| filter.is_empty() || f.to_lowercase().contains(&filter)) {
                    if ui.selectable_label(false, f).clicked() {
                        ui.ctx().copy_text(format!("family = \"{f}\""));
                        self.toast(format!("Copied: family = \"{f}\""));
                    }
                }
            });
        });
        egui::CollapsingHeader::new("Icon names").id_salt("icon_names").show(ui, |ui| {
            ui.label(
                RichText::new("Put <name>.svg or <name>.png in the icons folder, or set [icons.glyphs] <name> = \"…\" with an icon font.")
                    .weak()
                    .small(),
            );
            egui::Grid::new("icon_names_grid").num_columns(3).spacing([12.0, 2.0]).show(ui, |ui| {
                for (name, what) in theme::ICON_NAMES {
                    ui.label(RichText::new(*name).monospace());
                    ui.label(*what);
                    let how = if t.glyphs.contains_key(*name) {
                        "glyph"
                    } else if t.icon_files.contains_key(*name) {
                        "file"
                    } else {
                        "built-in"
                    };
                    ui.label(RichText::new(how).weak());
                    ui.end_row();
                }
            });
        });
    }

    /// The `theme = …` value in yerg.toml (or "default").
    fn selected_theme_name(&self) -> String {
        std::fs::read_to_string(theme::config_path())
            .ok()
            .and_then(|s| s.parse::<toml_edit::DocumentMut>().ok())
            .and_then(|d| d.get("theme").and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_else(|| "default".into())
    }
}
