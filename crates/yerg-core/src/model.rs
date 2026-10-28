//! Plain data types shared by every crate.

use serde::{Deserialize, Serialize};

pub type TrackId = i64;
pub type PlaylistId = i64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub uuid: String,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub genre: String,
    pub composer: String,
    pub comment: String,
    pub year: Option<i32>,
    pub track_no: Option<i32>,
    pub track_total: Option<i32>,
    pub disc_no: Option<i32>,
    pub disc_total: Option<i32>,
    pub duration_ms: i64,
    pub bitrate_kbps: Option<i32>,
    pub sample_rate: Option<i32>,
    pub channels: Option<i32>,
    pub codec: String,
    pub file_size: i64,
    pub mtime: i64,
    pub added_at: i64,
    pub rating: i32,
    pub play_count: i64,
    pub skip_count: i64,
    pub last_played: Option<i64>,
    pub art_hash: Option<String>,
    pub has_lyrics: bool,
    pub compilation: bool,
    pub bpm: Option<i32>,
    pub missing: bool,
}

impl Track {
    /// The artist used for grouping in Album Artist views.
    pub fn effective_album_artist(&self) -> &str {
        if !self.album_artist.is_empty() {
            &self.album_artist
        } else if self.compilation {
            "Various Artists"
        } else {
            &self.artist
        }
    }

    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            std::path::Path::new(&self.path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
        } else {
            &self.title
        }
    }

    pub fn to_tag_data(&self, lyrics: Option<String>) -> TagData {
        TagData {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album_artist: self.album_artist.clone(),
            album: self.album.clone(),
            genre: self.genre.clone(),
            composer: self.composer.clone(),
            comment: self.comment.clone(),
            year: self.year,
            track_no: self.track_no,
            track_total: self.track_total,
            disc_no: self.disc_no,
            disc_total: self.disc_total,
            lyrics,
            compilation: self.compilation,
            bpm: self.bpm,
            artists: Vec::new(),
        }
    }
}

/// Values read from (or written to) the tags in an audio file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TagData {
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub genre: String,
    pub composer: String,
    pub comment: String,
    pub year: Option<i32>,
    pub track_no: Option<i32>,
    pub track_total: Option<i32>,
    pub disc_no: Option<i32>,
    pub disc_total: Option<i32>,
    pub lyrics: Option<String>,
    pub compilation: bool,
    pub bpm: Option<i32>,
    /// The file's ARTISTS tag: each credited artist as its own value (written
    /// by MusicBrainz Picard and others).  Read only; `artist` stays the
    /// display text.  Empty when the file has none.
    #[serde(default)]
    pub artists: Vec<String>,
}

/// Audio stream facts that come with the tags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioProps {
    pub duration_ms: i64,
    pub bitrate_kbps: Option<i32>,
    pub sample_rate: Option<i32>,
    pub channels: Option<i32>,
    pub codec: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedPicture {
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AlbumKey {
    pub album: String,
    pub album_artist: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumSummary {
    pub album: String,
    pub album_artist: String,
    pub year: Option<i32>,
    pub track_count: i64,
    pub duration_ms: i64,
    pub art_hash: Option<String>,
    pub play_count: i64,
    pub added_at: i64,
    pub last_played: Option<i64>,
}

impl AlbumSummary {
    pub fn key(&self) -> AlbumKey {
        AlbumKey {
            album: self.album.clone(),
            album_artist: self.album_artist.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistSummary {
    pub name: String,
    pub album_count: i64,
    pub track_count: i64,
    pub duration_ms: i64,
    pub play_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenreSummary {
    pub name: String,
    pub track_count: i64,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: PlaylistId,
    pub name: String,
    pub created_at: i64,
    pub track_count: i64,
    pub duration_ms: i64,
    /// Sync this playlist to the MP3 player when the device sync mode is "playlists".
    pub sync_to_device: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaySource {
    /// Played in this app.
    Local,
    /// Imported from the MP3 player's listen log.
    Device,
    /// Imported from somewhere else (a scrobble export, another player).
    Imported,
}

impl PlaySource {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaySource::Local => "local",
            PlaySource::Device => "device",
            PlaySource::Imported => "imported",
        }
    }
    pub fn parse(s: &str) -> PlaySource {
        match s {
            "device" => PlaySource::Device,
            "imported" => PlaySource::Imported,
            _ => PlaySource::Local,
        }
    }
}

/// Scrobble submission state per service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrobbleState {
    Pending,
    Done,
    /// The service accepted the request but refused this listen (too old, bad metadata...).
    Ignored,
    /// Never submitted on purpose (too short, scrobbling was off when it was played...).
    Skipped,
}

impl ScrobbleState {
    pub fn from_i32(v: i32) -> ScrobbleState {
        match v {
            1 => ScrobbleState::Done,
            2 => ScrobbleState::Ignored,
            3 => ScrobbleState::Skipped,
            _ => ScrobbleState::Pending,
        }
    }
    pub fn as_i32(self) -> i32 {
        match self {
            ScrobbleState::Pending => 0,
            ScrobbleState::Done => 1,
            ScrobbleState::Ignored => 2,
            ScrobbleState::Skipped => 3,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ScrobbleState::Pending => "pending",
            ScrobbleState::Done => "scrobbled",
            ScrobbleState::Ignored => "ignored",
            ScrobbleState::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScrobbleService {
    LastFm,
    ListenBrainz,
    /// Libre.fm: the Last.fm protocol on libre.fm's own server.
    LibreFm,
}

impl ScrobbleService {
    pub fn column(self) -> &'static str {
        match self {
            ScrobbleService::LastFm => "lastfm",
            ScrobbleService::ListenBrainz => "listenbrainz",
            ScrobbleService::LibreFm => "librefm",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ScrobbleService::LastFm => "Last.fm",
            ScrobbleService::ListenBrainz => "ListenBrainz",
            ScrobbleService::LibreFm => "Libre.fm",
        }
    }
    pub const ALL: [ScrobbleService; 3] = [ScrobbleService::LastFm, ScrobbleService::ListenBrainz, ScrobbleService::LibreFm];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Play {
    pub id: i64,
    pub track_id: TrackId,
    pub started_at: i64,
    pub played_ms: i64,
    pub source: PlaySource,
    pub device_id: Option<String>,
    pub ts_estimated: bool,
    pub lastfm: ScrobbleState,
    pub listenbrainz: ScrobbleState,
    pub lastfm_note: Option<String>,
    pub listenbrainz_note: Option<String>,
    pub librefm: ScrobbleState,
    pub librefm_note: Option<String>,
}

impl Play {
    pub fn state(&self, service: ScrobbleService) -> ScrobbleState {
        match service {
            ScrobbleService::LastFm => self.lastfm,
            ScrobbleService::ListenBrainz => self.listenbrainz,
            ScrobbleService::LibreFm => self.librefm,
        }
    }

    pub fn note(&self, service: ScrobbleService) -> Option<&str> {
        match service {
            ScrobbleService::LastFm => self.lastfm_note.as_deref(),
            ScrobbleService::ListenBrainz => self.listenbrainz_note.as_deref(),
            ScrobbleService::LibreFm => self.librefm_note.as_deref(),
        }
    }
}

/// A play joined with the track metadata needed to scrobble or display it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayWithTrack {
    pub play: Play,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track_no: Option<i32>,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub mount_path: String,
    pub last_sync: Option<i64>,
    pub last_import: Option<i64>,
    /// Byte offset into the device's LISTENS.LOG that has already been imported.
    pub listens_offset: i64,
    /// "all" or "playlists"
    pub sync_mode: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceTrack {
    pub device_id: String,
    pub track_id: TrackId,
    pub device_path: String,
    /// Fingerprint of the source file when it was copied to the card.
    pub source_sig: String,
    pub synced_at: i64,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryFolder {
    pub id: i64,
    pub path: String,
    pub added_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryStats {
    pub tracks: i64,
    pub albums: i64,
    pub artists: i64,
    pub duration_ms: i64,
    pub bytes: i64,
    pub plays: i64,
}
