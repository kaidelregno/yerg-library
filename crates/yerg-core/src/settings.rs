//! Key/value settings stored in the database.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::db::Library;

/// Well-known setting keys.
pub mod keys {
    pub const LASTFM_API_KEY: &str = "lastfm.api_key";
    pub const LASTFM_API_SECRET: &str = "lastfm.api_secret";
    pub const LASTFM_SESSION_KEY: &str = "lastfm.session_key";
    pub const LASTFM_USERNAME: &str = "lastfm.username";
    pub const LASTFM_ENABLED: &str = "lastfm.enabled";
    pub const LISTENBRAINZ_TOKEN: &str = "listenbrainz.token";
    pub const LISTENBRAINZ_USERNAME: &str = "listenbrainz.username";
    pub const LISTENBRAINZ_ENABLED: &str = "listenbrainz.enabled";
    pub const LIBREFM_SESSION_KEY: &str = "librefm.session_key";
    pub const LIBREFM_USERNAME: &str = "librefm.username";
    pub const LIBREFM_ENABLED: &str = "librefm.enabled";
    pub const SCROBBLE_ENABLED: &str = "scrobble.enabled";
    pub const SCROBBLE_NOW_PLAYING: &str = "scrobble.now_playing";
    pub const SCROBBLE_DEVICE_PLAYS: &str = "scrobble.device_plays";
    pub const VOLUME: &str = "player.volume";
    /// Output device name; empty or absent = follow the system default.
    pub const OUTPUT_DEVICE: &str = "player.output_device";
    /// Artist names never split into several artists, one per line.
    pub const ARTIST_SPLIT_EXCEPTIONS: &str = "library.artist_split_exceptions";
    pub const SHUFFLE: &str = "player.shuffle";
    pub const REPEAT: &str = "player.repeat";
    pub const WRITE_TAGS_TO_FILES: &str = "library.write_tags_to_files";
    pub const DEVICE_LAST_ID: &str = "device.last_id";
    pub const UI_STATE: &str = "ui.state";
}

impl Library {
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn remove_setting(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }

    pub fn setting_bool(&self, key: &str, default: bool) -> bool {
        match self.setting(key) {
            Ok(Some(v)) => matches!(v.as_str(), "1" | "true" | "yes" | "on"),
            _ => default,
        }
    }

    pub fn set_setting_bool(&self, key: &str, value: bool) -> Result<()> {
        self.set_setting(key, if value { "1" } else { "0" })
    }

    pub fn setting_i64(&self, key: &str, default: i64) -> i64 {
        match self.setting(key) {
            Ok(Some(v)) => v.parse().unwrap_or(default),
            _ => default,
        }
    }

    pub fn setting_f64(&self, key: &str, default: f64) -> f64 {
        match self.setting(key) {
            Ok(Some(v)) => v.parse().unwrap_or(default),
            _ => default,
        }
    }

    pub fn setting_string(&self, key: &str) -> String {
        self.setting(key).ok().flatten().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let lib = Library::open_in_memory().unwrap();
        assert!(lib.setting("x").unwrap().is_none());
        lib.set_setting("x", "1").unwrap();
        assert!(lib.setting_bool("x", false));
        lib.set_setting("x", "42").unwrap();
        assert_eq!(lib.setting_i64("x", 0), 42);
        lib.remove_setting("x").unwrap();
        assert_eq!(lib.setting_i64("x", 7), 7);
    }
}
