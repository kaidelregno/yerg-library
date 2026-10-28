//! Playing tracks: queue handling, play counting and scrobble hooks.

use std::time::Duration;

use yerg_core::lyrics::Lyrics;
use yerg_core::model::*;
use yerg_core::plays::{counts_as_play, NewPlay};
use yerg_player::PlayerEvent;
use yerg_scrobble::ScrobbleItem;

use crate::app::YergApp;

pub struct NowPlaying {
    pub track: Track,
    /// The track's artists, a collaboration split into its members.
    pub artists: Vec<String>,
    pub started_at: i64,
    pub lyrics: Option<Lyrics>,
    pub lyrics_raw: String,
    pub recorded: bool,
    pub load_failed: bool,
}

impl YergApp {
    /// Replace the queue with `tracks` and start at `start`.
    pub fn play_list(&mut self, tracks: &[Track], start: usize) {
        let ids: Vec<TrackId> = tracks.iter().map(|t| t.id).collect();
        if let Some(first) = self.queue.set(ids, start) {
            self.start_track(first);
        }
    }

    pub fn play_ids(&mut self, ids: Vec<TrackId>, start: usize) {
        if let Some(first) = self.queue.set(ids, start) {
            self.start_track(first);
        }
    }

    pub fn start_track(&mut self, id: TrackId) {
        self.finalize_current(true);
        let Ok(Some(track)) = self.lib.track(id) else {
            self.toast("Track not found in library");
            return;
        };
        let lyrics_raw = self.lib.lyrics(id).ok().flatten().unwrap_or_default();
        let lyrics = if lyrics_raw.trim().is_empty() {
            None
        } else {
            Some(yerg_core::lyrics::parse(&lyrics_raw))
        };
        self.player.load(&track.path);
        if self.scrobble_cfg.enabled && self.scrobble_cfg.now_playing {
            let item = ScrobbleItem {
                play_id: 0,
                artist: track.artist.clone(),
                title: track.display_title().to_string(),
                album: track.album.clone(),
                album_artist: track.album_artist.clone(),
                track_no: track.track_no,
                duration_secs: Some((track.duration_ms / 1000) as u32),
                timestamp: yerg_core::now_unix(),
            };
            self.scrobbler.now_playing(item);
        }
        let artists = self.lib.track_artists(track.id).unwrap_or_default();
        self.now = Some(NowPlaying {
            artists: if artists.is_empty() { vec![track.artist.clone()] } else { artists },
            track,
            started_at: yerg_core::now_unix(),
            lyrics,
            lyrics_raw,
            recorded: false,
            load_failed: false,
        });
        self.lyrics_line = None;
        self.lyrics_scroll_pending = true;
    }

    /// Record the current track as played (if it played long enough) or skipped.
    pub fn finalize_current(&mut self, skipped: bool) {
        let Some(now) = self.now.as_mut() else { return };
        if now.recorded || now.load_failed {
            return;
        }
        now.recorded = true;
        let state = self.player.state();
        let played_ms = state.played_ms as i64;
        let duration_ms = if now.track.duration_ms > 0 {
            now.track.duration_ms
        } else {
            state.duration.map(|d| d.as_millis() as i64).unwrap_or(0)
        };
        if counts_as_play(duration_ms, played_ms) {
            let np = NewPlay {
                track_id: now.track.id,
                started_at: now.started_at,
                played_ms,
                source: PlaySource::Local,
                device_id: None,
                ts_estimated: false,
                scrobble: self.scrobble_cfg.enabled,
            };
            match self.lib.record_play(&np) {
                Ok(Some(_)) => {
                    self.scrobbler.flush();
                    self.lists_dirty = true;
                    self.history_dirty = true;
                    self.counts_dirty = true;
                }
                Ok(None) => {}
                Err(e) => log::warn!("recording play: {e:#}"),
            }
        } else if skipped && played_ms > 2000 {
            let _ = self.lib.record_skip(now.track.id);
        }
    }

    pub fn next_track(&mut self, manual: bool) {
        match self.queue.next(manual) {
            Some(id) => self.start_track(id),
            None => {
                self.finalize_current(manual);
                self.player.stop();
                self.now = None;
            }
        }
    }

    pub fn prev_track(&mut self) {
        // Restart the current track if more than 3 s in, like every player.
        if self.player.state().position > Duration::from_secs(3) {
            self.player.seek(Duration::ZERO);
            return;
        }
        if let Some(id) = self.queue.prev() {
            self.start_track(id);
        }
    }

    pub fn toggle_play(&mut self) {
        let st = self.player.state();
        if st.loaded {
            self.player.toggle();
        } else if let Some(id) = self.queue.current() {
            self.start_track(id);
        } else if !self.tracks.is_empty() {
            let tracks = self.tracks.clone();
            self.play_list(&tracks, 0);
        }
    }

    pub fn handle_player_events(&mut self) {
        for ev in self.player.poll_events() {
            match ev {
                PlayerEvent::TrackEnded { .. } => {
                    self.finalize_current(false);
                    self.next_track(false);
                }
                PlayerEvent::Error { message, .. } => {
                    if let Some(n) = self.now.as_mut() {
                        n.load_failed = true;
                    }
                    self.toast(format!("Playback error: {message}"));
                    // Skip to the next track so a bad file does not stall the queue.
                    self.consecutive_errors += 1;
                    if self.consecutive_errors < 5 {
                        self.next_track(true);
                    } else {
                        self.consecutive_errors = 0;
                        self.player.stop();
                        self.now = None;
                    }
                    continue;
                }
                PlayerEvent::OutputReady { device } => {
                    if self.output_was_lost {
                        self.output_was_lost = false;
                        self.toast(format!("Audio is playing on {device}"));
                    }
                    continue;
                }
                PlayerEvent::OutputLost { device, reason } => {
                    self.output_was_lost = true;
                    self.toast(format!("Lost audio device {device} ({reason}); reconnecting"));
                    continue;
                }
            }
            self.consecutive_errors = 0;
        }
    }

    /// Track the synced-lyrics cursor; returns true when the highlighted line changed.
    pub fn update_lyrics_cursor(&mut self) -> bool {
        let Some(now) = &self.now else { return false };
        let Some(l) = &now.lyrics else { return false };
        if !l.synced {
            return false;
        }
        let pos = self.player.state().position_now().as_millis() as i64;
        let idx = l.current_line(pos);
        if idx != self.lyrics_line {
            self.lyrics_line = idx;
            self.lyrics_scroll_pending = true;
            true
        } else {
            false
        }
    }
}
