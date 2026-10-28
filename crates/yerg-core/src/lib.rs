//! Yerg Library core: the music library database, tag reading/writing,
//! album art, lyrics, playlists and play history.
//!
//! Everything here is UI-agnostic and safe to call from background threads.
//! Each thread should open its own [`Library`] handle (SQLite in WAL mode).

pub mod art;
pub mod artists;
pub mod audio_io;
pub mod db;
pub mod ids;
pub mod lyrics;
pub mod model;
pub mod paths;
pub mod playlists;
pub mod plays;
pub mod query;
pub mod scan;
pub mod settings;
pub mod tags;

pub use db::Library;
pub use model::*;

/// Seconds since the Unix epoch, as the library stores every timestamp.
pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Format milliseconds as m:ss (or h:mm:ss past an hour).
pub fn fmt_duration(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Human friendly total time, e.g. "3 days, 4.2 hours" style used in footers.
pub fn fmt_total_time(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    let days = secs / 86_400;
    let hours = (secs % 86_400) as f64 / 3600.0;
    if days > 0 {
        format!("{days} days, {hours:.1} hours")
    } else if secs >= 3600 {
        format!("{hours:.1} hours")
    } else {
        format!("{:.1} minutes", secs as f64 / 60.0)
    }
}

pub fn fmt_bytes(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes.max(0) as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", v, UNITS[unit])
    }
}
