//! Play history: every listen, where it came from, and whether it has been scrobbled.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Row};

use crate::db::Library;
use crate::model::*;

const PLAY_COLS: &str = "p.id, p.track_id, p.started_at, p.played_ms, p.source, p.device_id, \
    p.ts_estimated, p.lastfm, p.listenbrainz, p.lastfm_note, p.listenbrainz_note, p.librefm, p.librefm_note";

fn play_from_row(r: &Row<'_>) -> rusqlite::Result<Play> {
    Ok(Play {
        id: r.get(0)?,
        track_id: r.get(1)?,
        started_at: r.get(2)?,
        played_ms: r.get(3)?,
        source: PlaySource::parse(&r.get::<_, String>(4)?),
        device_id: r.get(5)?,
        ts_estimated: r.get::<_, i64>(6)? != 0,
        lastfm: ScrobbleState::from_i32(r.get(7)?),
        listenbrainz: ScrobbleState::from_i32(r.get(8)?),
        lastfm_note: r.get(9)?,
        listenbrainz_note: r.get(10)?,
        librefm: ScrobbleState::from_i32(r.get(11)?),
        librefm_note: r.get(12)?,
    })
}

fn play_with_track_from_row(r: &Row<'_>) -> rusqlite::Result<PlayWithTrack> {
    Ok(PlayWithTrack {
        play: play_from_row(r)?,
        title: r.get(13)?,
        artist: r.get(14)?,
        album: r.get(15)?,
        album_artist: r.get(16)?,
        track_no: r.get(17)?,
        duration_ms: r.get(18)?,
    })
}

const PLAY_JOIN: &str = "SELECT p.id, p.track_id, p.started_at, p.played_ms, p.source, p.device_id, \
    p.ts_estimated, p.lastfm, p.listenbrainz, p.lastfm_note, p.listenbrainz_note, p.librefm, p.librefm_note, \
    t.title, t.artist, t.album, t.album_artist, t.track_no, t.duration_ms \
    FROM plays p JOIN tracks t ON t.id = p.track_id";

/// Everything needed to record one listen.
#[derive(Debug, Clone)]
pub struct NewPlay {
    pub track_id: TrackId,
    pub started_at: i64,
    pub played_ms: i64,
    pub source: PlaySource,
    pub device_id: Option<String>,
    pub ts_estimated: bool,
    /// False marks both services Skipped immediately (e.g. scrobbling disabled).
    pub scrobble: bool,
}

/// Last.fm's rule: a listen counts once half the track or four minutes has played,
/// and only for tracks longer than 30 seconds.
pub fn counts_as_play(duration_ms: i64, played_ms: i64) -> bool {
    if duration_ms < 30_000 {
        return false;
    }
    played_ms >= (duration_ms / 2).min(240_000)
}

impl Library {
    /// Record a listen.  Returns `None` if the same listen was already recorded.
    pub fn record_play(&self, p: &NewPlay) -> Result<Option<i64>> {
        let state = if p.scrobble {
            ScrobbleState::Pending
        } else {
            ScrobbleState::Skipped
        }
        .as_i32();
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO plays(track_id, started_at, played_ms, source, device_id, ts_estimated, lastfm, listenbrainz, librefm) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?7)",
            params![
                p.track_id,
                p.started_at,
                p.played_ms,
                p.source.as_str(),
                p.device_id,
                p.ts_estimated as i64,
                state
            ],
        )?;
        if inserted == 0 {
            return Ok(None);
        }
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "UPDATE tracks SET play_count = play_count + 1, \
             last_played = MAX(COALESCE(last_played, 0), ?1) WHERE id = ?2",
            params![p.started_at, p.track_id],
        )?;
        Ok(Some(id))
    }

    pub fn record_skip(&self, track_id: TrackId) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET skip_count = skip_count + 1 WHERE id = ?1",
            [track_id],
        )?;
        Ok(())
    }

    pub fn delete_play(&self, id: i64) -> Result<()> {
        let track: Option<TrackId> = self
            .conn
            .query_row("SELECT track_id FROM plays WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        if let Some(t) = track {
            self.conn.execute("DELETE FROM plays WHERE id = ?1", [id])?;
            self.recount_track(t)?;
        }
        Ok(())
    }

    /// Recompute play_count / last_played for a track from the plays table.
    pub fn recount_track(&self, track_id: TrackId) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET play_count = (SELECT COUNT(*) FROM plays WHERE track_id = ?1), \
             last_played = (SELECT MAX(started_at) FROM plays WHERE track_id = ?1) WHERE id = ?1",
            [track_id],
        )?;
        Ok(())
    }

    pub fn recount_all(&self) -> Result<()> {
        self.conn.execute_batch(
            "UPDATE tracks SET play_count = (SELECT COUNT(*) FROM plays WHERE track_id = tracks.id), \
             last_played = (SELECT MAX(started_at) FROM plays WHERE track_id = tracks.id)",
        )?;
        Ok(())
    }

    /// Listens still waiting to go to a service, oldest first.
    pub fn pending_scrobbles(&self, service: ScrobbleService, limit: usize) -> Result<Vec<PlayWithTrack>> {
        let sql = format!(
            "{PLAY_JOIN} WHERE p.{} = 0 ORDER BY p.started_at ASC LIMIT ?1",
            service.column()
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([limit as i64], play_with_track_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn pending_count(&self, service: ScrobbleService) -> Result<i64> {
        Ok(self.conn.query_row(
            &format!("SELECT COUNT(*) FROM plays WHERE {} = 0", service.column()),
            [],
            |r| r.get(0),
        )?)
    }

    pub fn set_scrobble_state(
        &self,
        play_id: i64,
        service: ScrobbleService,
        state: ScrobbleState,
        note: Option<&str>,
    ) -> Result<()> {
        let col = service.column();
        self.conn.execute(
            &format!("UPDATE plays SET {col} = ?1, {col}_note = ?2 WHERE id = ?3"),
            params![state.as_i32(), note, play_id],
        )?;
        Ok(())
    }

    /// Mark every pending listen for a service as skipped (used when the user
    /// decides not to backfill history).
    pub fn skip_all_pending(&self, service: ScrobbleService) -> Result<usize> {
        let col = service.column();
        Ok(self.conn.execute(
            &format!("UPDATE plays SET {col} = 3, {col}_note = 'skipped by user' WHERE {col} = 0"),
            [],
        )?)
    }

    /// Put ignored/skipped listens back in the queue.
    pub fn requeue(&self, service: ScrobbleService, include_ignored: bool) -> Result<usize> {
        let col = service.column();
        let states = if include_ignored { "(2,3)" } else { "(3)" };
        Ok(self.conn.execute(
            &format!("UPDATE plays SET {col} = 0, {col}_note = NULL WHERE {col} IN {states}"),
            [],
        )?)
    }

    pub fn recent_plays(&self, limit: usize) -> Result<Vec<PlayWithTrack>> {
        let sql = format!("{PLAY_JOIN} ORDER BY p.started_at DESC, p.id DESC LIMIT ?1");
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([limit as i64], play_with_track_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn plays_for_track(&self, track_id: TrackId) -> Result<Vec<Play>> {
        let sql = format!("SELECT {PLAY_COLS} FROM plays p WHERE p.track_id = ?1 ORDER BY p.started_at DESC");
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([track_id], play_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Plays since a timestamp (for stats panels).
    pub fn play_count_since(&self, since: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM plays WHERE started_at >= ?1",
            [since],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_rules() {
        assert!(!counts_as_play(20_000, 20_000));
        assert!(counts_as_play(200_000, 100_000));
        assert!(!counts_as_play(200_000, 99_000));
        assert!(counts_as_play(1_000_000, 240_000));
    }

    #[test]
    fn record_and_dedupe() {
        let lib = Library::open_in_memory().unwrap();
        let (tid, _) = lib
            .upsert_scanned("/x.mp3", &TagData::default(), &AudioProps::default(), 1, 1, None, None)
            .unwrap();
        let np = NewPlay {
            track_id: tid,
            started_at: 1_700_000_000,
            played_ms: 100_000,
            source: PlaySource::Local,
            device_id: None,
            ts_estimated: false,
            scrobble: true,
        };
        assert!(lib.record_play(&np).unwrap().is_some());
        assert!(lib.record_play(&np).unwrap().is_none());
        let t = lib.track(tid).unwrap().unwrap();
        assert_eq!(t.play_count, 1);
        assert_eq!(t.last_played, Some(1_700_000_000));
        assert_eq!(lib.pending_count(ScrobbleService::LastFm).unwrap(), 1);
        let pending = lib.pending_scrobbles(ScrobbleService::LastFm, 10).unwrap();
        lib.set_scrobble_state(pending[0].play.id, ScrobbleService::LastFm, ScrobbleState::Done, None)
            .unwrap();
        assert_eq!(lib.pending_count(ScrobbleService::LastFm).unwrap(), 0);
        assert_eq!(lib.pending_count(ScrobbleService::ListenBrainz).unwrap(), 1);
        assert_eq!(lib.pending_count(ScrobbleService::LibreFm).unwrap(), 1);
        let p = &lib.pending_scrobbles(ScrobbleService::LibreFm, 10).unwrap()[0];
        assert_eq!(p.play.state(ScrobbleService::LibreFm), ScrobbleState::Pending);
        lib.set_scrobble_state(p.play.id, ScrobbleService::LibreFm, ScrobbleState::Ignored, Some("too old"))
            .unwrap();
        let after = &lib.recent_plays(1).unwrap()[0];
        assert_eq!(after.play.note(ScrobbleService::LibreFm), Some("too old"));
        assert_eq!(after.play.state(ScrobbleService::ListenBrainz), ScrobbleState::Pending, "other services untouched");
        lib.delete_play(pending[0].play.id).unwrap();
        assert_eq!(lib.track(tid).unwrap().unwrap().play_count, 0);
    }

    #[test]
    fn listens_from_before_librefm_start_skipped_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        {
            let lib = Library::open(&path).unwrap();
            let (tid, _) = lib
                .upsert_scanned("/x.mp3", &TagData::default(), &AudioProps::default(), 1, 1, None, None)
                .unwrap();
            lib.record_play(&NewPlay {
                track_id: tid,
                started_at: 1_700_000_000,
                played_ms: 100_000,
                source: PlaySource::Local,
                device_id: None,
                ts_estimated: false,
                scrobble: true,
            })
            .unwrap();
            // Turn it back into a version-2 library, as the previous release left it.
            lib.conn
                .execute_batch(
                    "ALTER TABLE plays DROP COLUMN librefm_note; ALTER TABLE plays DROP COLUMN librefm; PRAGMA user_version = 2;",
                )
                .unwrap();
        }
        let lib = Library::open(&path).unwrap();
        let old = &lib.recent_plays(1).unwrap()[0];
        assert_eq!(old.play.state(ScrobbleService::LibreFm), ScrobbleState::Skipped);
        assert_eq!(old.play.state(ScrobbleService::LastFm), ScrobbleState::Pending, "existing queues kept");
        assert_eq!(lib.pending_count(ScrobbleService::LibreFm).unwrap(), 0);
        // `requeue` brings old listens back if the user wants them sent.
        assert_eq!(lib.requeue(ScrobbleService::LibreFm, false).unwrap(), 1);
    }
}
