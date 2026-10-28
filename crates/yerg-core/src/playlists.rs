//! User playlists, plus M3U import and export.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

use crate::db::{track_from_row, Library, TRACK_COLS};
use crate::model::*;
use crate::now_unix;

const PLAYLIST_SQL: &str = "SELECT p.id, p.name, p.created_at, p.sync_to_device, \
    (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id), \
    (SELECT COALESCE(SUM(t.duration_ms),0) FROM playlist_items i JOIN tracks t ON t.id = i.track_id WHERE i.playlist_id = p.id) \
    FROM playlists p";

fn playlist_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Playlist> {
    Ok(Playlist {
        id: r.get(0)?,
        name: r.get(1)?,
        created_at: r.get(2)?,
        sync_to_device: r.get::<_, i64>(3)? != 0,
        track_count: r.get(4)?,
        duration_ms: r.get(5)?,
    })
}

impl Library {
    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut st = self
            .conn
            .prepare(&format!("{PLAYLIST_SQL} ORDER BY p.sort_order, p.name COLLATE NOCASE"))?;
        let rows = st.query_map([], playlist_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn playlist(&self, id: PlaylistId) -> Result<Option<Playlist>> {
        Ok(self
            .conn
            .query_row(&format!("{PLAYLIST_SQL} WHERE p.id = ?1"), [id], playlist_from_row)
            .optional()?)
    }

    pub fn create_playlist(&self, name: &str) -> Result<PlaylistId> {
        let order: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(sort_order),0)+1 FROM playlists", [], |r| r.get(0))?;
        self.conn.execute(
            "INSERT INTO playlists(name, created_at, sort_order) VALUES (?1, ?2, ?3)",
            params![name.trim(), now_unix(), order],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_playlist(&self, id: PlaylistId, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE playlists SET name = ?1 WHERE id = ?2",
            params![name.trim(), id],
        )?;
        Ok(())
    }

    pub fn delete_playlist(&self, id: PlaylistId) -> Result<()> {
        self.conn.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn set_playlist_sync(&self, id: PlaylistId, sync: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE playlists SET sync_to_device = ?1 WHERE id = ?2",
            params![sync as i64, id],
        )?;
        Ok(())
    }

    pub fn playlist_track_ids(&self, id: PlaylistId) -> Result<Vec<TrackId>> {
        let mut st = self.conn.prepare(
            "SELECT track_id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position",
        )?;
        let rows = st.query_map([id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn playlist_tracks(&self, id: PlaylistId) -> Result<Vec<Track>> {
        let sql = format!(
            "SELECT {TRACK_COLS} FROM playlist_items i JOIN tracks t ON t.id = i.track_id \
             WHERE i.playlist_id = ?1 ORDER BY i.position"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([id], track_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Append tracks to the end of a playlist.
    pub fn add_to_playlist(&self, id: PlaylistId, tracks: &[TrackId]) -> Result<()> {
        self.transaction(|lib| {
            let mut pos: i64 = lib.conn.query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_items WHERE playlist_id = ?1",
                [id],
                |r| r.get(0),
            )?;
            let mut st = lib.conn.prepare_cached(
                "INSERT INTO playlist_items(playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
            )?;
            for t in tracks {
                st.execute(params![id, pos, t])?;
                pos += 1;
            }
            Ok(())
        })
    }

    /// Replace the whole contents of a playlist.
    pub fn set_playlist_items(&self, id: PlaylistId, tracks: &[TrackId]) -> Result<()> {
        self.transaction(|lib| {
            lib.conn
                .execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
            let mut st = lib.conn.prepare_cached(
                "INSERT INTO playlist_items(playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
            )?;
            for (pos, t) in tracks.iter().enumerate() {
                st.execute(params![id, pos as i64, t])?;
            }
            Ok(())
        })
    }

    /// Remove the entries at the given positions (0-based).
    pub fn remove_from_playlist(&self, id: PlaylistId, positions: &[usize]) -> Result<()> {
        let mut items = self.playlist_track_ids(id)?;
        let mut sorted = positions.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        for p in sorted.into_iter().rev() {
            if p < items.len() {
                items.remove(p);
            }
        }
        self.set_playlist_items(id, &items)
    }

    pub fn move_in_playlist(&self, id: PlaylistId, from: usize, to: usize) -> Result<()> {
        let mut items = self.playlist_track_ids(id)?;
        if from >= items.len() || to >= items.len() || from == to {
            return Ok(());
        }
        let t = items.remove(from);
        items.insert(to, t);
        self.set_playlist_items(id, &items)
    }

    /// Playlists that contain a track.
    pub fn playlists_containing(&self, track: TrackId) -> Result<Vec<PlaylistId>> {
        let mut st = self.conn.prepare(
            "SELECT DISTINCT playlist_id FROM playlist_items WHERE track_id = ?1",
        )?;
        let rows = st.query_map([track], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Write an extended M3U (UTF-8) file.
    pub fn export_m3u(&self, id: PlaylistId, out: &Path) -> Result<usize> {
        let tracks = self.playlist_tracks(id)?;
        let mut s = String::from("#EXTM3U\n");
        for t in &tracks {
            s.push_str(&format!(
                "#EXTINF:{},{} - {}\n{}\n",
                t.duration_ms / 1000,
                t.artist,
                t.display_title(),
                t.path
            ));
        }
        std::fs::write(out, s).with_context(|| format!("writing {}", out.display()))?;
        Ok(tracks.len())
    }

    /// Import an M3U/M3U8 file as a new playlist.  Returns (playlist id, matched, unmatched).
    pub fn import_m3u(&self, file: &Path, name: Option<&str>) -> Result<(PlaylistId, usize, Vec<String>)> {
        let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        let base = file.parent().unwrap_or(Path::new("."));
        let mut ids = Vec::new();
        let mut unmatched = Vec::new();
        for line in text.lines() {
            let line = line.trim().trim_start_matches('\u{feff}');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let candidate = Path::new(line);
            let abs = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                base.join(candidate)
            };
            let abs_str = crate::db::normalize_path(&abs);
            let found = self.track_by_path(&abs_str)?.or_else(|| {
                // Fall back to matching the file name anywhere in the library.
                let fname = candidate.file_name()?.to_str()?.to_string();
                let sql = format!(
                    "SELECT {TRACK_COLS} FROM tracks t WHERE t.path LIKE ?1 ESCAPE '\\' LIMIT 1"
                );
                let pat = format!("%{}{}", std::path::MAIN_SEPARATOR, escape_like(&fname));
                self.conn.query_row(&sql, [pat], track_from_row).optional().ok().flatten()
            });
            match found {
                Some(t) => ids.push(t.id),
                None => unmatched.push(line.to_string()),
            }
        }
        let pname = name
            .map(|s| s.to_string())
            .or_else(|| file.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "Imported".to_string());
        let id = self.create_playlist(&pname)?;
        self.set_playlist_items(id, &ids)?;
        Ok((id, ids.len(), unmatched))
    }
}

pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(lib: &Library, path: &str) -> TrackId {
        lib.upsert_scanned(
            path,
            &TagData {
                title: path.into(),
                ..Default::default()
            },
            &AudioProps::default(),
            1,
            1,
            None,
            None,
        )
        .unwrap()
        .0
    }

    #[test]
    fn playlist_crud() {
        let lib = Library::open_in_memory().unwrap();
        let a = add(&lib, "/a.mp3");
        let b = add(&lib, "/b.mp3");
        let c = add(&lib, "/c.mp3");
        let p = lib.create_playlist("Mix").unwrap();
        lib.add_to_playlist(p, &[a, b, c]).unwrap();
        assert_eq!(lib.playlist_track_ids(p).unwrap(), vec![a, b, c]);
        lib.move_in_playlist(p, 2, 0).unwrap();
        assert_eq!(lib.playlist_track_ids(p).unwrap(), vec![c, a, b]);
        lib.remove_from_playlist(p, &[1]).unwrap();
        assert_eq!(lib.playlist_track_ids(p).unwrap(), vec![c, b]);
        assert_eq!(lib.playlists().unwrap()[0].track_count, 2);
        lib.delete_track(b).unwrap();
        assert_eq!(lib.playlist_track_ids(p).unwrap(), vec![c]);
        lib.delete_playlist(p).unwrap();
        assert!(lib.playlists().unwrap().is_empty());
    }
}
