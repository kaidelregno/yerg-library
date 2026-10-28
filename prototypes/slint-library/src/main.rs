//! Slint prototype of the Songs view, loading the real library.
//!
//! Read-only: ratings change on screen but are never written to the database,
//! and double-click does not play.  The audio engine is started (it opens the
//! output device at startup, as in the real app) but nothing is loaded.
//!
//! Debug hook, shared with the egui app: `YERG_DEBUG_AUTOSCROLL=<px per second>`
//! scrolls the table continuously and logs the frame rate every 5 seconds.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, SharedString, Timer, TimerMode};
use yerg_core::query::{Sort, SortKey, TrackFilter};
use yerg_core::settings::keys;
use yerg_core::{Library, Track};

slint::include_modules!();

/// The table model.  Rows are formatted on demand, so only visible rows cost
/// anything, like the egui table.
struct Tracks {
    rows: RefCell<Vec<Track>>,
    selection: RefCell<BTreeSet<usize>>,
    anchor: Cell<Option<usize>>,
    notify: ModelNotify,
}

impl Model for Tracks {
    type Data = TrackRow;

    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    fn row_data(&self, i: usize) -> Option<TrackRow> {
        let rows = self.rows.borrow();
        let t = rows.get(i)?;
        Some(TrackRow {
            title: t.display_title().into(),
            time: yerg_core::fmt_duration(t.duration_ms).into(),
            artist: t.artist.as_str().into(),
            album: t.album.as_str().into(),
            plays: if t.play_count > 0 {
                t.play_count.to_string().into()
            } else {
                SharedString::new()
            },
            rating: t.rating,
            missing: t.missing,
            selected: self.selection.borrow().contains(&i),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

impl Tracks {
    fn replace(&self, rows: Vec<Track>) {
        *self.rows.borrow_mut() = rows;
        self.selection.borrow_mut().clear();
        self.anchor.set(None);
        self.notify.reset();
    }

    fn press(&self, i: usize, ctrl: bool, shift: bool) {
        let before: BTreeSet<usize> = self.selection.borrow().clone();
        {
            let mut sel = self.selection.borrow_mut();
            if ctrl {
                if !sel.remove(&i) {
                    sel.insert(i);
                }
                self.anchor.set(Some(i));
            } else if shift {
                let a = self.anchor.get().unwrap_or(i);
                let (lo, hi) = if a <= i { (a, i) } else { (i, a) };
                sel.extend(lo..=hi);
            } else {
                sel.clear();
                sel.insert(i);
                self.anchor.set(Some(i));
            }
        }
        let after = self.selection.borrow().clone();
        for j in before.symmetric_difference(&after) {
            self.notify.row_changed(*j);
        }
    }
}

const SORT_COLUMNS: [(i32, SortKey); 6] = [
    (1, SortKey::Title),
    (2, SortKey::Duration),
    (3, SortKey::Artist),
    (4, SortKey::Album),
    (5, SortKey::PlayCount),
    (6, SortKey::Rating),
];

fn sort_col(sort: Sort) -> i32 {
    SORT_COLUMNS.iter().find(|(_, k)| *k == sort.key).map(|(c, _)| *c).unwrap_or(-1)
}

fn footer(rows: &[Track]) -> String {
    let ms: i64 = rows.iter().map(|t| t.duration_ms).sum();
    let bytes: i64 = rows.iter().map(|t| t.file_size).sum();
    format!(
        "{} songs, {}, {:.1} GB",
        rows.len(),
        yerg_core::fmt_total_time(ms),
        bytes as f64 / 1e9
    )
}

fn stats(rows: &[Track]) -> String {
    let albums: HashSet<(&str, &str)> = rows.iter().map(|t| (t.album.as_str(), t.effective_album_artist())).collect();
    let artists: HashSet<&str> = rows.iter().map(|t| t.artist.as_str()).collect();
    format!("{} songs · {} albums · {} artists", rows.len(), albums.len(), artists.len())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lib = Rc::new(Library::open(&yerg_core::paths::database_path())?);
    let device = lib.setting(keys::OUTPUT_DEVICE).ok().flatten().filter(|s| !s.is_empty());
    let _player = yerg_player::Player::with_device(device);

    let ui = MainWindow::new()?;
    let filter = Rc::new(RefCell::new(TrackFilter::default()));
    let sort = Rc::new(Cell::new(Sort::default()));

    let started = Instant::now();
    let first = lib.query_tracks(&filter.borrow(), sort.get())?;
    eprintln!("loaded {} tracks in {:?}", first.len(), started.elapsed());
    ui.set_footer(footer(&first).into());
    ui.set_stats(stats(&first).into());
    let model = Rc::new(Tracks {
        rows: RefCell::new(first),
        selection: RefCell::new(BTreeSet::new()),
        anchor: Cell::new(None),
        notify: ModelNotify::default(),
    });
    ui.set_tracks(ModelRc::from(model.clone()));
    ui.set_sort_col(sort_col(sort.get()));
    ui.set_sort_desc(sort.get().descending);

    let reload = {
        let (lib, model, filter, sort, weak) = (lib.clone(), model.clone(), filter.clone(), sort.clone(), ui.as_weak());
        Rc::new(move || {
            let Some(ui) = weak.upgrade() else { return };
            match lib.query_tracks(&filter.borrow(), sort.get()) {
                Ok(rows) => {
                    ui.set_footer(footer(&rows).into());
                    model.replace(rows);
                    ui.set_scroll_y(0.0);
                }
                Err(e) => eprintln!("query failed: {e:#}"),
            }
            ui.set_sort_col(sort_col(sort.get()));
            ui.set_sort_desc(sort.get().descending);
        })
    };

    ui.on_sort({
        let (sort, reload) = (sort.clone(), reload.clone());
        move |col| {
            if let Some((_, key)) = SORT_COLUMNS.iter().find(|(c, _)| *c == col) {
                let mut s = sort.get();
                s.toggle(*key);
                sort.set(s);
                reload();
            }
        }
    });
    ui.on_search({
        let (filter, reload) = (filter.clone(), reload.clone());
        move |text| {
            filter.borrow_mut().search = text.to_string();
            reload();
        }
    });
    ui.on_row_pressed({
        let model = model.clone();
        move |i, ctrl, shift| model.press(i as usize, ctrl, shift)
    });
    ui.on_row_activated(|_| {});
    ui.on_rate({
        let model = model.clone();
        move |i, r| {
            // On screen only: the prototype never writes to your library.
            if let Some(t) = model.rows.borrow_mut().get_mut(i as usize) {
                t.rating = r;
            }
            model.notify.row_changed(i as usize);
        }
    });
    ui.on_toggle_now_playing({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_show_now_playing(!ui.get_show_now_playing());
            }
        }
    });

    // Frame counting, for the comparison.
    let frames = Rc::new(Cell::new(0u64));
    let notifier = ui.window().set_rendering_notifier({
        let frames = frames.clone();
        move |state, _| {
            if matches!(state, slint::RenderingState::AfterRendering) {
                frames.set(frames.get() + 1);
            }
        }
    });
    let counting_frames = notifier.is_ok();
    if let Err(e) = notifier {
        eprintln!("rendering notifier unavailable ({e:?}); counting scroll updates instead");
    }

    let scroll_timer = Timer::default();
    let stats_timer = Timer::default();
    if let Some(speed) = std::env::var("YERG_DEBUG_AUTOSCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
        let updates = Rc::new(Cell::new(0u64));
        let t0 = Instant::now();
        scroll_timer.start(TimerMode::Repeated, Duration::from_millis(16), {
            let (weak, updates) = (ui.as_weak(), updates.clone());
            move || {
                let Some(ui) = weak.upgrade() else { return };
                let max = (ui.get_list_viewport_height() - ui.get_list_visible_height()).max(1.0);
                let y = (t0.elapsed().as_secs_f32() * speed) % max;
                ui.set_scroll_y(-y);
                updates.set(updates.get() + 1);
            }
        });
        let (mut last_frames, mut last_updates) = (0u64, 0u64);
        stats_timer.start(TimerMode::Repeated, Duration::from_secs(5), {
            let frames = frames.clone();
            move || {
                let (f, u) = (frames.get(), updates.get());
                if counting_frames {
                    eprintln!("autoscroll: {:.1} fps", (f - last_frames) as f64 / 5.0);
                } else {
                    eprintln!("autoscroll: {:.1} updates/s", (u - last_updates) as f64 / 5.0);
                }
                last_frames = f;
                last_updates = u;
            }
        });
    }

    ui.run()?;
    Ok(())
}
