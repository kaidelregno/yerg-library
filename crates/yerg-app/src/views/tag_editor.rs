//! Tag editor window (single track, or shared fields across a selection).

use std::collections::BTreeSet;

use egui::{RichText, Sense, Vec2};
use yerg_core::model::*;
use yerg_core::Library;

use crate::app::YergApp;

pub struct TagEditor {
    pub ids: Vec<TrackId>,
    pub title_line: String,
    pub data: TagData,
    pub lyrics: String,
    pub multi: bool,
    pub apply: BTreeSet<&'static str>,
    pub art_hash: Option<String>,
    pub new_art: Option<(String, Vec<u8>, String)>,
    pub remove_art: bool,
    pub write_files: bool,
    pub year_s: String,
    pub track_s: String,
    pub track_total_s: String,
    pub disc_s: String,
    pub disc_total_s: String,
    pub bpm_s: String,
    pub path: String,
}

fn common<T: PartialEq + Clone + Default>(tracks: &[Track], f: impl Fn(&Track) -> T) -> T {
    let first = f(&tracks[0]);
    if tracks.iter().all(|t| f(t) == first) {
        first
    } else {
        T::default()
    }
}

impl TagEditor {
    pub fn new(lib: &Library, tracks: Vec<Track>, write_files: bool) -> TagEditor {
        let multi = tracks.len() > 1;
        let lyrics = if multi {
            String::new()
        } else {
            lib.lyrics(tracks[0].id).ok().flatten().unwrap_or_default()
        };
        let data = if multi {
            TagData {
                artist: common(&tracks, |t| t.artist.clone()),
                album_artist: common(&tracks, |t| t.album_artist.clone()),
                album: common(&tracks, |t| t.album.clone()),
                genre: common(&tracks, |t| t.genre.clone()),
                composer: common(&tracks, |t| t.composer.clone()),
                comment: common(&tracks, |t| t.comment.clone()),
                year: common(&tracks, |t| t.year),
                disc_no: common(&tracks, |t| t.disc_no),
                disc_total: common(&tracks, |t| t.disc_total),
                track_total: common(&tracks, |t| t.track_total),
                compilation: common(&tracks, |t| t.compilation),
                ..Default::default()
            }
        } else {
            tracks[0].to_tag_data(None)
        };
        let s = |v: Option<i32>| v.filter(|x| *x > 0).map(|x| x.to_string()).unwrap_or_default();
        TagEditor {
            title_line: if multi {
                format!("{} tracks", tracks.len())
            } else {
                format!("{} — {}", tracks[0].artist, tracks[0].display_title())
            },
            year_s: s(data.year),
            track_s: s(data.track_no),
            track_total_s: s(data.track_total),
            disc_s: s(data.disc_no),
            disc_total_s: s(data.disc_total),
            bpm_s: s(data.bpm),
            art_hash: if multi { common(&tracks, |t| t.art_hash.clone()) } else { tracks[0].art_hash.clone() },
            path: if multi { String::new() } else { tracks[0].path.clone() },
            ids: tracks.iter().map(|t| t.id).collect(),
            data,
            lyrics,
            multi,
            apply: BTreeSet::new(),
            new_art: None,
            remove_art: false,
            write_files,
        }
    }

    fn parse_numbers(&mut self) {
        let p = |s: &str| s.trim().parse::<i32>().ok().filter(|v| *v > 0);
        self.data.year = p(&self.year_s);
        self.data.track_no = p(&self.track_s);
        self.data.track_total = p(&self.track_total_s);
        self.data.disc_no = p(&self.disc_s);
        self.data.disc_total = p(&self.disc_total_s);
        self.data.bpm = p(&self.bpm_s);
    }
}

/// Outcome of drawing the editor for one frame.
pub enum EditorResult {
    Open,
    Cancel,
    Save,
}

impl YergApp {
    pub fn ui_tag_editor_contents(&mut self, ui: &mut egui::Ui) -> EditorResult {
        let Some(ed) = self.tag_editor.as_mut() else { return EditorResult::Cancel };
        let mut result = EditorResult::Open;
        ui.label(RichText::new(&ed.title_line).strong());
        if !ed.path.is_empty() {
            ui.label(RichText::new(&ed.path).weak().small());
        }
        ui.add_space(6.0);

        let multi = ed.multi;
        ui.horizontal_top(|ui| {
            // Artwork column
            ui.vertical(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(160.0), Sense::hover());
                let hash = if ed.remove_art { None } else { ed.art_hash.clone() };
                // paint_art needs &mut self.art; ed is a disjoint field, fine.
                let rounding = egui::CornerRadius::same(4);
                match self.art.get(hash.as_deref(), false) {
                    Some(tex) => {
                        egui::Image::new(&tex).corner_radius(rounding).paint_at(ui, rect);
                    }
                    None => {
                        ui.painter().rect_filled(rect, rounding, crate::theme::current().colors.raised);
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "♪", egui::FontId::proportional(48.0), crate::theme::current().colors.icon_dim);
                    }
                }
                if let Some((_, _, name)) = &ed.new_art {
                    ui.label(RichText::new(format!("New: {name}")).small());
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Choose…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Images", &["jpg", "jpeg", "png", "webp"]).pick_file() {
                            if let Ok(bytes) = std::fs::read(&p) {
                                let mime = yerg_core::art::mime_for_path(&p).to_string();
                                let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                                ed.new_art = Some((mime, bytes, name));
                                ed.remove_art = false;
                            }
                        }
                    }
                    if (ed.art_hash.is_some() || ed.new_art.is_some()) && ui.small_button("Remove").clicked() {
                        ed.new_art = None;
                        ed.remove_art = true;
                    }
                });
            });

            ui.add_space(12.0);
            ui.vertical(|ui| {
                egui::Grid::new("tag_grid").num_columns(if multi { 3 } else { 2 }).spacing([8.0, 6.0]).show(ui, |ui| {
                    let mut field = |ui: &mut egui::Ui, key: &'static str, label: &str, value: &mut String, width: f32| {
                        ui.label(label);
                        let r = ui.add(egui::TextEdit::singleline(value).desired_width(width));
                        if multi {
                            let mut on = ed.apply.contains(key);
                            if r.changed() {
                                on = true;
                            }
                            if ui.checkbox(&mut on, "apply").changed() || r.changed() {
                                if on {
                                    ed.apply.insert(key);
                                } else {
                                    ed.apply.remove(key);
                                }
                            }
                        }
                        ui.end_row();
                    };
                    if !multi {
                        field(ui, "title", "Title", &mut ed.data.title, 360.0);
                    }
                    field(ui, "artist", "Artist", &mut ed.data.artist, 360.0);
                    field(ui, "album_artist", "Album artist", &mut ed.data.album_artist, 360.0);
                    field(ui, "album", "Album", &mut ed.data.album, 360.0);
                    field(ui, "genre", "Genre", &mut ed.data.genre, 200.0);
                    field(ui, "composer", "Composer", &mut ed.data.composer, 360.0);
                    field(ui, "year", "Year", &mut ed.year_s, 70.0);
                    if !multi {
                        field(ui, "track_no", "Track #", &mut ed.track_s, 50.0);
                    }
                    field(ui, "track_total", "Tracks on disc", &mut ed.track_total_s, 50.0);
                    field(ui, "disc_no", "Disc #", &mut ed.disc_s, 50.0);
                    field(ui, "disc_total", "Discs", &mut ed.disc_total_s, 50.0);
                    if !multi {
                        field(ui, "bpm", "BPM", &mut ed.bpm_s, 60.0);
                    }
                    field(ui, "comment", "Comment", &mut ed.data.comment, 360.0);
                    ui.label("Compilation");
                    let r = ui.checkbox(&mut ed.data.compilation, "part of a various-artists album");
                    if multi {
                        let mut on = ed.apply.contains("compilation") || r.changed();
                        if ui.checkbox(&mut on, "apply").changed() || r.changed() {
                            if on {
                                ed.apply.insert("compilation");
                            } else {
                                ed.apply.remove("compilation");
                            }
                        }
                    }
                    ui.end_row();
                });
                if !multi {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Lyrics").strong());
                    ui.label(RichText::new("Plain text or LRC with [mm:ss.xx] timestamps for synced display.").weak().small());
                    egui::ScrollArea::vertical().max_height(180.0).id_salt("lyrics_edit").show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut ed.lyrics).desired_width(f32::INFINITY).desired_rows(8).code_editor());
                    });
                    ui.horizontal(|ui| {
                        if ui.small_button("Load .lrc/.txt…").clicked() {
                            if let Some(p) = rfd::FileDialog::new().add_filter("Lyrics", &["lrc", "txt"]).pick_file() {
                                if let Ok(b) = std::fs::read(&p) {
                                    ed.lyrics = String::from_utf8_lossy(&b).to_string();
                                }
                            }
                        }
                        if ui.small_button("Save as .lrc next to file").clicked() {
                            match yerg_core::lyrics::write_sidecar(std::path::Path::new(&ed.path), &ed.lyrics) {
                                Ok(p) => log::info!("wrote {}", p.display()),
                                Err(e) => log::warn!("{e}"),
                            }
                        }
                    });
                }
            });
        });

        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.checkbox(&mut ed.write_files, "Write to file(s)");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new("Save").strong()).clicked() {
                    result = EditorResult::Save;
                }
                if ui.button("Cancel").clicked() {
                    result = EditorResult::Cancel;
                }
            });
        });
        result
    }

    pub fn commit_tag_editor(&mut self) {
        let Some(mut ed) = self.tag_editor.take() else { return };
        ed.parse_numbers();
        let fields: Option<Vec<&'static str>> = if ed.multi { Some(ed.apply.iter().copied().collect()) } else { None };
        let mut data = ed.data.clone();
        if !ed.multi {
            data.lyrics = Some(ed.lyrics.clone());
        }
        let any_fields = !ed.multi || fields.as_ref().map(|f| !f.is_empty()).unwrap_or(false);
        if any_fields {
            self.jobs.save_tags(ed.ids.clone(), data, fields, ed.write_files);
        }
        if let Some((mime, bytes, _)) = ed.new_art.take() {
            self.art.forget(&yerg_core::art::hash_bytes(&bytes));
            self.jobs.set_art(ed.ids.clone(), Some((mime, bytes)), ed.write_files);
        } else if ed.remove_art {
            self.jobs.set_art(ed.ids.clone(), None, ed.write_files);
        }
    }
}
