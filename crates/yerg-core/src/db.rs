//! SQLite storage.  One [`Library`] per thread; all threads share the file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::*;
use crate::{ids, now_unix};

const SCHEMA_VERSION: i32 = 3;

pub struct Library {
    pub conn: Connection,
    pub path: Option<PathBuf>,
}

/// Column list used by every query that returns a [`Track`].
pub const TRACK_COLS: &str = "t.id, t.uuid, t.path, t.title, t.artist, t.album_artist, t.album, t.genre, \
    t.composer, t.comment, t.year, t.track_no, t.track_total, t.disc_no, t.disc_total, t.duration_ms, \
    t.bitrate_kbps, t.sample_rate, t.channels, t.codec, t.file_size, t.mtime, t.added_at, t.rating, \
    t.play_count, t.skip_count, t.last_played, t.art_hash, (t.lyrics IS NOT NULL AND t.lyrics != ''), \
    t.compilation, t.bpm, t.missing";

pub fn track_from_row(r: &Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
        id: r.get(0)?,
        uuid: r.get(1)?,
        path: r.get(2)?,
        title: r.get(3)?,
        artist: r.get(4)?,
        album_artist: r.get(5)?,
        album: r.get(6)?,
        genre: r.get(7)?,
        composer: r.get(8)?,
        comment: r.get(9)?,
        year: r.get(10)?,
        track_no: r.get(11)?,
        track_total: r.get(12)?,
        disc_no: r.get(13)?,
        disc_total: r.get(14)?,
        duration_ms: r.get(15)?,
        bitrate_kbps: r.get(16)?,
        sample_rate: r.get(17)?,
        channels: r.get(18)?,
        codec: r.get(19)?,
        file_size: r.get(20)?,
        mtime: r.get(21)?,
        added_at: r.get(22)?,
        rating: r.get(23)?,
        play_count: r.get(24)?,
        skip_count: r.get(25)?,
        last_played: r.get(26)?,
        art_hash: r.get(27)?,
        has_lyrics: r.get::<_, i64>(28)? != 0,
        compilation: r.get::<_, i64>(29)? != 0,
        bpm: r.get(30)?,
        missing: r.get::<_, i64>(31)? != 0,
    })
}

impl Library {
    /// Open (creating if needed) the library database at `path`.
    pub fn open(path: &Path) -> Result<Library> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        let mut lib = Library {
            conn,
            path: Some(path.to_path_buf()),
        };
        lib.init()?;
        Ok(lib)
    }

    /// Open the default per-user library.
    pub fn open_default() -> Result<Library> {
        Library::open(&crate::paths::database_path())
    }

    /// A throwaway in-memory library (tests, dry runs).
    pub fn open_in_memory() -> Result<Library> {
        let conn = Connection::open_in_memory()?;
        let mut lib = Library { conn, path: None };
        lib.init()?;
        Ok(lib)
    }

    /// Another handle on the same database, for use on another thread.
    pub fn reopen(&self) -> Result<Library> {
        match &self.path {
            Some(p) => Library::open(p),
            None => anyhow::bail!("cannot reopen an in-memory library"),
        }
    }

    fn init(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;",
        )?;
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            self.conn.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            self.conn.execute_batch(SCHEMA_V2)?;
        }
        if version < 3 {
            self.conn.execute_batch(SCHEMA_V3)?;
        }
        if version != SCHEMA_VERSION {
            self.conn
                .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ folders

    pub fn folders(&self) -> Result<Vec<LibraryFolder>> {
        let mut st = self
            .conn
            .prepare("SELECT id, path, added_at FROM folders ORDER BY path")?;
        let rows = st.query_map([], |r| {
            Ok(LibraryFolder {
                id: r.get(0)?,
                path: r.get(1)?,
                added_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_folder(&self, path: &Path) -> Result<i64> {
        let p = normalize_path(path);
        self.conn.execute(
            "INSERT OR IGNORE INTO folders(path, added_at) VALUES (?1, ?2)",
            params![p, now_unix()],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM folders WHERE path = ?1", [p], |r| r.get(0))?)
    }

    /// Remove a folder and every track under it.
    pub fn remove_folder(&self, id: i64) -> Result<usize> {
        let path: Option<String> = self
            .conn
            .query_row("SELECT path FROM folders WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        let Some(path) = path else { return Ok(0) };
        let prefix = format!("{}{}", path.trim_end_matches(['/', '\\']), std::path::MAIN_SEPARATOR);
        let removed = self.conn.execute(
            "DELETE FROM tracks WHERE substr(path, 1, ?1) = ?2",
            params![prefix.chars().count() as i64, prefix],
        )?;
        self.conn.execute("DELETE FROM folders WHERE id = ?1", [id])?;
        Ok(removed)
    }

    // ------------------------------------------------------------ tracks

    pub fn track(&self, id: TrackId) -> Result<Option<Track>> {
        let sql = format!("SELECT {TRACK_COLS} FROM tracks t WHERE t.id = ?1");
        Ok(self
            .conn
            .query_row(&sql, [id], track_from_row)
            .optional()?)
    }

    pub fn track_by_path(&self, path: &str) -> Result<Option<Track>> {
        let sql = format!("SELECT {TRACK_COLS} FROM tracks t WHERE t.path = ?1");
        Ok(self
            .conn
            .query_row(&sql, [path], track_from_row)
            .optional()?)
    }

    pub fn track_by_uuid(&self, uuid: &str) -> Result<Option<Track>> {
        let sql = format!("SELECT {TRACK_COLS} FROM tracks t WHERE t.uuid = ?1");
        Ok(self
            .conn
            .query_row(&sql, [uuid], track_from_row)
            .optional()?)
    }

    pub fn tracks_by_ids(&self, ids: &[TrackId]) -> Result<Vec<Track>> {
        // Preserve the order of `ids`.
        let mut out = Vec::with_capacity(ids.len());
        let sql = format!("SELECT {TRACK_COLS} FROM tracks t WHERE t.id = ?1");
        let mut st = self.conn.prepare_cached(&sql)?;
        for id in ids {
            if let Some(t) = st.query_row([id], track_from_row).optional()? {
                out.push(t);
            }
        }
        Ok(out)
    }

    pub fn all_tracks(&self) -> Result<Vec<Track>> {
        let sql = format!(
            "SELECT {TRACK_COLS} FROM tracks t WHERE t.missing = 0 \
             ORDER BY t.album_artist COLLATE NOCASE, t.album COLLATE NOCASE, t.disc_no, t.track_no, t.title COLLATE NOCASE"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], track_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// (path, mtime, file_size) for every known track; used by the scanner to skip unchanged files.
    pub fn track_fingerprints(&self) -> Result<std::collections::HashMap<String, (i64, i64, i64)>> {
        let mut st = self
            .conn
            .prepare("SELECT path, mtime, file_size, id FROM tracks")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Tracks whose files could never be parsed (scanned with path-derived tags
    /// only).  The scanner retries these even when the file has not changed.
    pub fn unreadable_track_ids(&self) -> Result<std::collections::HashSet<TrackId>> {
        let mut st = self.conn.prepare("SELECT id FROM tracks WHERE codec = ''")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Tracks scanned before artist credits were stored: (id, path, artist).
    pub fn tracks_without_artist_credits(&self) -> Result<Vec<(TrackId, String, String)>> {
        let mut st = self
            .conn
            .prepare("SELECT id, path, artist FROM tracks WHERE artists_read = 0 AND missing = 0 AND codec != ''")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record a track's artist credits (and, when given, a fuller artist
    /// string) without touching any other tag.
    pub fn set_artist_credits(&self, id: TrackId, artist: Option<&str>, credits: &[String]) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET artist = COALESCE(?1, artist), artists = ?2, artists_read = 1 WHERE id = ?3",
            params![artist, credits.join("\n"), id],
        )?;
        Ok(())
    }

    /// (artist tag, credits) for every track, credits one name per line.
    pub fn track_artist_fields(&self) -> Result<Vec<(TrackId, String, String)>> {
        let mut st = self.conn.prepare_cached("SELECT id, artist, artists FROM tracks")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Insert or refresh a scanned file.  Returns (id, newly_added).
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_scanned(
        &self,
        path: &str,
        tags: &TagData,
        props: &AudioProps,
        file_size: i64,
        mtime: i64,
        art_hash: Option<&str>,
        lyrics: Option<&str>,
    ) -> Result<(TrackId, bool)> {
        let existing: Option<TrackId> = self
            .conn
            .query_row("SELECT id FROM tracks WHERE path = ?1", [path], |r| r.get(0))
            .optional()?;
        let credits = tags.artists.join("\n");
        if let Some(id) = existing {
            self.conn.execute(
                "UPDATE tracks SET title=?1, artist=?2, album_artist=?3, album=?4, genre=?5, composer=?6, \
                 comment=?7, year=?8, track_no=?9, track_total=?10, disc_no=?11, disc_total=?12, \
                 duration_ms=?13, bitrate_kbps=?14, sample_rate=?15, channels=?16, codec=?17, \
                 file_size=?18, mtime=?19, art_hash=?20, lyrics=COALESCE(?21, lyrics), compilation=?22, \
                 bpm=?23, missing=0, artists=?24, artists_read=1 WHERE id=?25",
                params![
                    tags.title, tags.artist, tags.album_artist, tags.album, tags.genre, tags.composer,
                    tags.comment, tags.year, tags.track_no, tags.track_total, tags.disc_no,
                    tags.disc_total, props.duration_ms, props.bitrate_kbps, props.sample_rate,
                    props.channels, props.codec, file_size, mtime, art_hash, lyrics,
                    tags.compilation as i64, tags.bpm, credits, id
                ],
            )?;
            Ok((id, false))
        } else {
            self.conn.execute(
                "INSERT INTO tracks(uuid, path, title, artist, album_artist, album, genre, composer, \
                 comment, year, track_no, track_total, disc_no, disc_total, duration_ms, bitrate_kbps, \
                 sample_rate, channels, codec, file_size, mtime, added_at, art_hash, lyrics, compilation, bpm, \
                 artists, artists_read) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,1)",
                params![
                    ids::new_uuid(), path, tags.title, tags.artist, tags.album_artist, tags.album,
                    tags.genre, tags.composer, tags.comment, tags.year, tags.track_no, tags.track_total,
                    tags.disc_no, tags.disc_total, props.duration_ms, props.bitrate_kbps,
                    props.sample_rate, props.channels, props.codec, file_size, mtime, now_unix(),
                    art_hash, lyrics, tags.compilation as i64, tags.bpm, credits
                ],
            )?;
            Ok((self.conn.last_insert_rowid(), true))
        }
    }

    /// Update the editable metadata of a track in the database only.
    /// Editing the artist clears the stored ARTISTS credits, which would no
    /// longer match it (SET expressions see the old row, so `artist = ?2`
    /// compares against the previous artist).
    pub fn update_track_tags(&self, id: TrackId, tags: &TagData) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET artists = CASE WHEN artist = ?2 THEN artists ELSE '' END, \
             title=?1, artist=?2, album_artist=?3, album=?4, genre=?5, composer=?6, \
             comment=?7, year=?8, track_no=?9, track_total=?10, disc_no=?11, disc_total=?12, \
             compilation=?13, bpm=?14, lyrics=COALESCE(?15, lyrics) WHERE id=?16",
            params![
                tags.title, tags.artist, tags.album_artist, tags.album, tags.genre, tags.composer,
                tags.comment, tags.year, tags.track_no, tags.track_total, tags.disc_no, tags.disc_total,
                tags.compilation as i64, tags.bpm, tags.lyrics, id
            ],
        )?;
        Ok(())
    }

    pub fn set_rating(&self, id: TrackId, rating: i32) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET rating = ?1 WHERE id = ?2",
            params![rating.clamp(0, 5), id],
        )?;
        Ok(())
    }

    pub fn set_file_stat(&self, id: TrackId, mtime: i64, size: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET mtime = ?1, file_size = ?2 WHERE id = ?3",
            params![mtime, size, id],
        )?;
        Ok(())
    }

    pub fn lyrics(&self, id: TrackId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT lyrics FROM tracks WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten()
            .filter(|s| !s.is_empty()))
    }

    pub fn set_lyrics(&self, id: TrackId, lyrics: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET lyrics = ?1 WHERE id = ?2",
            params![lyrics, id],
        )?;
        Ok(())
    }

    pub fn set_missing(&self, id: TrackId, missing: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET missing = ?1 WHERE id = ?2",
            params![missing as i64, id],
        )?;
        Ok(())
    }

    /// Delete tracks that are flagged missing.  Their plays go with them.
    pub fn purge_missing(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM tracks WHERE missing = 1", [])?)
    }

    pub fn delete_track(&self, id: TrackId) -> Result<()> {
        self.conn.execute("DELETE FROM tracks WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn stats(&self) -> Result<LibraryStats> {
        let (tracks, duration_ms, bytes): (i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(duration_ms),0), COALESCE(SUM(file_size),0) FROM tracks WHERE missing = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let albums: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM (SELECT DISTINCT album, CASE WHEN album_artist != '' THEN album_artist WHEN compilation != 0 THEN 'Various Artists' ELSE artist END FROM tracks WHERE missing = 0)",
            [],
            |r| r.get(0),
        )?;
        let artists: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT CASE WHEN album_artist != '' THEN album_artist ELSE artist END) FROM tracks WHERE missing = 0",
            [],
            |r| r.get(0),
        )?;
        let plays: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM plays", [], |r| r.get(0))?;
        Ok(LibraryStats {
            tracks,
            albums,
            artists,
            duration_ms,
            bytes,
            plays,
        })
    }

    // ------------------------------------------------------------ devices

    pub fn devices(&self) -> Result<Vec<Device>> {
        let mut st = self.conn.prepare(
            "SELECT id, name, mount_path, last_sync, last_import, listens_offset, sync_mode, created_at \
             FROM devices ORDER BY name",
        )?;
        let rows = st.query_map([], device_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn device(&self, id: &str) -> Result<Option<Device>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, mount_path, last_sync, last_import, listens_offset, sync_mode, created_at \
                 FROM devices WHERE id = ?1",
                [id],
                device_from_row,
            )
            .optional()?)
    }

    pub fn upsert_device(&self, d: &Device) -> Result<()> {
        self.conn.execute(
            "INSERT INTO devices(id, name, mount_path, last_sync, last_import, listens_offset, sync_mode, created_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, mount_path=excluded.mount_path, \
             last_sync=excluded.last_sync, last_import=excluded.last_import, \
             listens_offset=excluded.listens_offset, sync_mode=excluded.sync_mode",
            params![
                d.id, d.name, d.mount_path, d.last_sync, d.last_import, d.listens_offset,
                d.sync_mode, d.created_at
            ],
        )?;
        Ok(())
    }

    pub fn delete_device(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM devices WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn device_tracks(&self, device_id: &str) -> Result<Vec<DeviceTrack>> {
        let mut st = self.conn.prepare(
            "SELECT device_id, track_id, device_path, source_sig, synced_at, size_bytes \
             FROM device_tracks WHERE device_id = ?1",
        )?;
        let rows = st.query_map([device_id], |r| {
            Ok(DeviceTrack {
                device_id: r.get(0)?,
                track_id: r.get(1)?,
                device_path: r.get(2)?,
                source_sig: r.get(3)?,
                synced_at: r.get(4)?,
                size_bytes: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn upsert_device_track(&self, dt: &DeviceTrack) -> Result<()> {
        self.conn.execute(
            "INSERT INTO device_tracks(device_id, track_id, device_path, source_sig, synced_at, size_bytes) \
             VALUES (?1,?2,?3,?4,?5,?6) \
             ON CONFLICT(device_id, track_id) DO UPDATE SET device_path=excluded.device_path, \
             source_sig=excluded.source_sig, synced_at=excluded.synced_at, size_bytes=excluded.size_bytes",
            params![
                dt.device_id, dt.track_id, dt.device_path, dt.source_sig, dt.synced_at, dt.size_bytes
            ],
        )?;
        Ok(())
    }

    pub fn delete_device_track(&self, device_id: &str, track_id: TrackId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM device_tracks WHERE device_id = ?1 AND track_id = ?2",
            params![device_id, track_id],
        )?;
        Ok(())
    }

    /// Run `f` inside a transaction.
    pub fn transaction<T>(&self, f: impl FnOnce(&Library) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}

fn device_from_row(r: &Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        id: r.get(0)?,
        name: r.get(1)?,
        mount_path: r.get(2)?,
        last_sync: r.get(3)?,
        last_import: r.get(4)?,
        listens_offset: r.get(5)?,
        sync_mode: r.get(6)?,
        created_at: r.get(7)?,
    })
}

/// Canonical string form of a path for storage.
pub fn normalize_path(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    // Strip Windows verbatim prefix if canonicalize added one.
    s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or(s)
}

/// Version 2: artist credits from the ARTISTS tag (one name per line), and
/// whether they have been read for this track yet.  Tracks scanned before
/// this have `artists_read = 0`; the next scan reads just their artist
/// fields (see `scan::fill_artist_credits`).
const SCHEMA_V2: &str = "
ALTER TABLE tracks ADD COLUMN artists TEXT NOT NULL DEFAULT '';
ALTER TABLE tracks ADD COLUMN artists_read INTEGER NOT NULL DEFAULT 0;
";

/// Version 3: Libre.fm as a third scrobble service.  Listens recorded before
/// it existed start as skipped (3), so connecting it does not upload the whole
/// history; `yerg scrobble requeue librefm` queues them if wanted.  New listens
/// set the column explicitly.
const SCHEMA_V3: &str = "
ALTER TABLE plays ADD COLUMN librefm INTEGER NOT NULL DEFAULT 3;
ALTER TABLE plays ADD COLUMN librefm_note TEXT;
";

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS folders(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    added_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS tracks(
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    path TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL DEFAULT '',
    artist TEXT NOT NULL DEFAULT '',
    album_artist TEXT NOT NULL DEFAULT '',
    album TEXT NOT NULL DEFAULT '',
    genre TEXT NOT NULL DEFAULT '',
    composer TEXT NOT NULL DEFAULT '',
    comment TEXT NOT NULL DEFAULT '',
    year INTEGER,
    track_no INTEGER,
    track_total INTEGER,
    disc_no INTEGER,
    disc_total INTEGER,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    bitrate_kbps INTEGER,
    sample_rate INTEGER,
    channels INTEGER,
    codec TEXT NOT NULL DEFAULT '',
    file_size INTEGER NOT NULL DEFAULT 0,
    mtime INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER NOT NULL,
    rating INTEGER NOT NULL DEFAULT 0,
    play_count INTEGER NOT NULL DEFAULT 0,
    skip_count INTEGER NOT NULL DEFAULT 0,
    last_played INTEGER,
    art_hash TEXT,
    lyrics TEXT,
    compilation INTEGER NOT NULL DEFAULT 0,
    bpm INTEGER,
    missing INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album_artist, album);
CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
CREATE INDEX IF NOT EXISTS idx_tracks_genre ON tracks(genre);
CREATE TABLE IF NOT EXISTS art(
    hash TEXT PRIMARY KEY,
    mime TEXT NOT NULL,
    data BLOB NOT NULL,
    thumb BLOB,
    width INTEGER,
    height INTEGER
);
CREATE TABLE IF NOT EXISTS playlists(
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    sync_to_device INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS playlist_items(
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    PRIMARY KEY(playlist_id, position)
);
CREATE INDEX IF NOT EXISTS idx_playlist_items_track ON playlist_items(track_id);
CREATE TABLE IF NOT EXISTS plays(
    id INTEGER PRIMARY KEY,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL,
    played_ms INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL DEFAULT 'local',
    device_id TEXT,
    ts_estimated INTEGER NOT NULL DEFAULT 0,
    lastfm INTEGER NOT NULL DEFAULT 0,
    listenbrainz INTEGER NOT NULL DEFAULT 0,
    lastfm_note TEXT,
    listenbrainz_note TEXT,
    UNIQUE(track_id, started_at, source)
);
CREATE INDEX IF NOT EXISTS idx_plays_started ON plays(started_at);
CREATE INDEX IF NOT EXISTS idx_plays_pending ON plays(lastfm, listenbrainz);
CREATE TABLE IF NOT EXISTS devices(
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    mount_path TEXT NOT NULL,
    last_sync INTEGER,
    last_import INTEGER,
    listens_offset INTEGER NOT NULL DEFAULT 0,
    sync_mode TEXT NOT NULL DEFAULT 'all',
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS device_tracks(
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    device_path TEXT NOT NULL,
    source_sig TEXT NOT NULL,
    synced_at INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(device_id, track_id)
);
CREATE TABLE IF NOT EXISTS settings(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tags(title: &str) -> TagData {
        TagData {
            title: title.into(),
            artist: "Artist".into(),
            album: "Album".into(),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_and_fetch() {
        let lib = Library::open_in_memory().unwrap();
        let props = AudioProps {
            duration_ms: 1000,
            ..Default::default()
        };
        let (id, new) = lib
            .upsert_scanned("/a/b.mp3", &sample_tags("B"), &props, 10, 20, None, None)
            .unwrap();
        assert!(new);
        let (id2, new2) = lib
            .upsert_scanned("/a/b.mp3", &sample_tags("B2"), &props, 11, 21, None, None)
            .unwrap();
        assert_eq!(id, id2);
        assert!(!new2);
        let t = lib.track(id).unwrap().unwrap();
        assert_eq!(t.title, "B2");
        assert_eq!(t.file_size, 11);
        assert_eq!(lib.stats().unwrap().tracks, 1);
    }

    #[test]
    fn remove_folder_removes_tracks_under_it() {
        let lib = Library::open_in_memory().unwrap();
        let props = AudioProps::default();
        let sep = std::path::MAIN_SEPARATOR;
        let root = format!("{sep}music");
        let fid = lib.add_folder(Path::new(&root)).unwrap();
        lib.upsert_scanned(&format!("{root}{sep}x.mp3"), &sample_tags("x"), &props, 1, 1, None, None)
            .unwrap();
        lib.upsert_scanned(&format!("{sep}other{sep}y.mp3"), &sample_tags("y"), &props, 1, 1, None, None)
            .unwrap();
        assert_eq!(lib.remove_folder(fid).unwrap(), 1);
        assert_eq!(lib.stats().unwrap().tracks, 1);
    }
}
