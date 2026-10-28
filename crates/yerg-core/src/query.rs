//! Library views: filtered, sorted track lists and artist/album/genre groupings.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::artists::ArtistIndex;
use crate::settings::keys;
use crate::db::{track_from_row, Library, TRACK_COLS};
use crate::model::*;
use crate::playlists::escape_like;

/// SQL for the artist an album is filed under.
pub const EFFECTIVE_ALBUM_ARTIST: &str =
    "CASE WHEN t.album_artist != '' THEN t.album_artist WHEN t.compilation != 0 THEN 'Various Artists' ELSE t.artist END";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SortKey {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Year,
    TrackNo,
    Duration,
    Genre,
    Composer,
    PlayCount,
    LastPlayed,
    AddedAt,
    Rating,
    Bitrate,
    Path,
}

impl SortKey {
    pub const ALL: [SortKey; 15] = [
        SortKey::Title,
        SortKey::Artist,
        SortKey::AlbumArtist,
        SortKey::Album,
        SortKey::Year,
        SortKey::TrackNo,
        SortKey::Duration,
        SortKey::Genre,
        SortKey::Composer,
        SortKey::PlayCount,
        SortKey::LastPlayed,
        SortKey::AddedAt,
        SortKey::Rating,
        SortKey::Bitrate,
        SortKey::Path,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Title => "Title",
            SortKey::Artist => "Artist",
            SortKey::AlbumArtist => "Album Artist",
            SortKey::Album => "Album",
            SortKey::Year => "Year",
            SortKey::TrackNo => "Track #",
            SortKey::Duration => "Time",
            SortKey::Genre => "Genre",
            SortKey::Composer => "Composer",
            SortKey::PlayCount => "Plays",
            SortKey::LastPlayed => "Last Played",
            SortKey::AddedAt => "Date Added",
            SortKey::Rating => "Rating",
            SortKey::Bitrate => "Bit Rate",
            SortKey::Path => "Location",
        }
    }

    /// Whether the natural first click should sort descending (newest / most first).
    pub fn default_descending(self) -> bool {
        matches!(
            self,
            SortKey::PlayCount | SortKey::LastPlayed | SortKey::AddedAt | SortKey::Rating | SortKey::Year
        )
    }

    fn order_terms(self, desc: bool) -> String {
        let d = if desc { "DESC" } else { "ASC" };
        let album_run = "aa COLLATE NOCASE, t.album COLLATE NOCASE, t.disc_no, t.track_no, t.title COLLATE NOCASE";
        match self {
            SortKey::Title => format!("t.title COLLATE NOCASE {d}, t.artist COLLATE NOCASE"),
            SortKey::Artist => format!("t.artist COLLATE NOCASE {d}, t.album COLLATE NOCASE, t.disc_no, t.track_no"),
            SortKey::AlbumArtist => format!("aa COLLATE NOCASE {d}, t.album COLLATE NOCASE, t.disc_no, t.track_no"),
            SortKey::Album => format!("t.album COLLATE NOCASE {d}, aa COLLATE NOCASE, t.disc_no, t.track_no"),
            SortKey::Year => format!("t.year {d}, {album_run}"),
            SortKey::TrackNo => format!("t.disc_no {d}, t.track_no {d}, {album_run}"),
            SortKey::Duration => format!("t.duration_ms {d}, t.title COLLATE NOCASE"),
            SortKey::Genre => format!("t.genre COLLATE NOCASE {d}, {album_run}"),
            SortKey::Composer => format!("t.composer COLLATE NOCASE {d}, {album_run}"),
            SortKey::PlayCount => format!("t.play_count {d}, t.last_played DESC, {album_run}"),
            SortKey::LastPlayed => format!("t.last_played {d}, {album_run}"),
            SortKey::AddedAt => format!("t.added_at {d}, {album_run}"),
            SortKey::Rating => format!("t.rating {d}, {album_run}"),
            SortKey::Bitrate => format!("t.bitrate_kbps {d}, {album_run}"),
            SortKey::Path => format!("t.path COLLATE NOCASE {d}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            key: SortKey::AlbumArtist,
            descending: false,
        }
    }
}

impl Sort {
    pub fn new(key: SortKey) -> Sort {
        Sort {
            key,
            descending: key.default_descending(),
        }
    }
    /// Clicking a header: same key flips direction, a new key starts fresh.
    pub fn toggle(&mut self, key: SortKey) {
        if self.key == key {
            self.descending = !self.descending;
        } else {
            *self = Sort::new(key);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackFilter {
    /// Free text matched against title / artist / album / album artist / genre.
    pub search: String,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<AlbumKey>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub min_rating: i32,
    pub only_with_lyrics: bool,
    pub include_missing: bool,
    /// Only tracks played at least once.
    pub only_played: bool,
    /// Only tracks added after this timestamp.
    pub added_since: Option<i64>,
}

impl TrackFilter {
    /// `artist_ids`: the tracks by `self.artist`, collaborations included (see
    /// [`ArtistIndex::track_has_artist`]).  `None` matches the tag exactly.
    fn where_clause(&self, params: &mut Vec<Value>, artist_ids: Option<&[TrackId]>) -> String {
        let mut conds: Vec<String> = Vec::new();
        if !self.include_missing {
            conds.push("t.missing = 0".into());
        }
        if let Some(a) = &self.artist {
            match artist_ids {
                // Integer ids written inline: no parameter limit for prolific artists.
                Some(ids) if !ids.is_empty() => {
                    let list: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
                    conds.push(format!("t.id IN ({})", list.join(",")));
                }
                Some(_) => conds.push("0".into()),
                None => {
                    params.push(Value::Text(a.clone()));
                    conds.push(format!("t.artist = ?{}", params.len()));
                }
            }
        }
        if let Some(a) = &self.album_artist {
            params.push(Value::Text(a.clone()));
            conds.push(format!("aa = ?{}", params.len()));
        }
        if let Some(k) = &self.album {
            params.push(Value::Text(k.album.clone()));
            let p1 = params.len();
            params.push(Value::Text(k.album_artist.clone()));
            let p2 = params.len();
            conds.push(format!("t.album = ?{p1} AND aa = ?{p2}"));
        }
        if let Some(g) = &self.genre {
            params.push(Value::Text(g.clone()));
            conds.push(format!("t.genre = ?{}", params.len()));
        }
        if let Some(c) = &self.composer {
            params.push(Value::Text(c.clone()));
            conds.push(format!("t.composer = ?{}", params.len()));
        }
        if self.min_rating > 0 {
            params.push(Value::Integer(self.min_rating as i64));
            conds.push(format!("t.rating >= ?{}", params.len()));
        }
        if self.only_with_lyrics {
            conds.push("t.lyrics IS NOT NULL AND t.lyrics != ''".into());
        }
        if self.only_played {
            conds.push("t.play_count > 0".into());
        }
        if let Some(s) = self.added_since {
            params.push(Value::Integer(s));
            conds.push(format!("t.added_at >= ?{}", params.len()));
        }
        for token in self.search.split_whitespace() {
            params.push(Value::Text(format!("%{}%", escape_like(token))));
            let n = params.len();
            conds.push(format!(
                "(t.title LIKE ?{n} ESCAPE '\\' OR t.artist LIKE ?{n} ESCAPE '\\' OR t.album LIKE ?{n} ESCAPE '\\' \
                 OR t.album_artist LIKE ?{n} ESCAPE '\\' OR t.genre LIKE ?{n} ESCAPE '\\' OR t.composer LIKE ?{n} ESCAPE '\\')"
            ));
        }
        if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlbumSort {
    Name,
    Artist,
    Year,
    AddedAt,
    PlayCount,
    LastPlayed,
}

impl AlbumSort {
    pub const ALL: [AlbumSort; 6] = [
        AlbumSort::Artist,
        AlbumSort::Name,
        AlbumSort::Year,
        AlbumSort::AddedAt,
        AlbumSort::PlayCount,
        AlbumSort::LastPlayed,
    ];
    pub fn label(self) -> &'static str {
        match self {
            AlbumSort::Name => "Title",
            AlbumSort::Artist => "Artist",
            AlbumSort::Year => "Year",
            AlbumSort::AddedAt => "Date Added",
            AlbumSort::PlayCount => "Plays",
            AlbumSort::LastPlayed => "Last Played",
        }
    }
    fn order(self) -> &'static str {
        match self {
            AlbumSort::Name => "album COLLATE NOCASE, aa COLLATE NOCASE",
            AlbumSort::Artist => "aa COLLATE NOCASE, year, album COLLATE NOCASE",
            AlbumSort::Year => "year DESC, aa COLLATE NOCASE, album COLLATE NOCASE",
            AlbumSort::AddedAt => "added_at DESC, aa COLLATE NOCASE",
            AlbumSort::PlayCount => "play_count DESC, aa COLLATE NOCASE",
            AlbumSort::LastPlayed => "last_played DESC, aa COLLATE NOCASE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtistMode {
    /// Group by the track artist field.
    Artist,
    /// Group by album artist (compilations file under "Various Artists").
    AlbumArtist,
}

impl Library {
    /// The main track list.
    pub fn query_tracks(&self, filter: &TrackFilter, sort: Sort) -> Result<Vec<Track>> {
        let mut params = Vec::new();
        let ids = self.artist_ids_for(filter)?;
        let where_sql = filter.where_clause(&mut params, ids.as_deref());
        let sql = format!(
            "SELECT * FROM (SELECT {TRACK_COLS}, {EFFECTIVE_ALBUM_ARTIST} AS aa FROM tracks t) t {where_sql} ORDER BY {}",
            sort.key.order_terms(sort.descending)
        );
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(params), track_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn albums(&self, filter: &TrackFilter, sort: AlbumSort) -> Result<Vec<AlbumSummary>> {
        let mut params = Vec::new();
        let ids = self.artist_ids_for(filter)?;
        let where_sql = filter.where_clause(&mut params, ids.as_deref());
        let sql = format!(
            "SELECT album, aa, MIN(year), COUNT(*), SUM(duration_ms), MAX(art_hash), SUM(play_count), \
             MIN(added_at), MAX(last_played) \
             FROM (SELECT t.*, {EFFECTIVE_ALBUM_ARTIST} AS aa FROM tracks t) t {where_sql} \
             GROUP BY aa, album ORDER BY {}",
            sort.order()
        );
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(params), |r| {
            Ok(AlbumSummary {
                album: r.get(0)?,
                album_artist: r.get(1)?,
                year: r.get(2)?,
                track_count: r.get(3)?,
                duration_ms: r.get(4)?,
                art_hash: r.get(5)?,
                play_count: r.get(6)?,
                added_at: r.get(7)?,
                last_played: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Names the user never wants split (Settings › Library), one per line.
    pub fn artist_split_exceptions(&self) -> Vec<String> {
        self.setting_string(keys::ARTIST_SPLIT_EXCEPTIONS)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Every artist tag split into artists (collaborations into their
    /// members), plus the (id, artist tag, credits) rows it was built from.
    fn artist_index_and_rows(&self) -> Result<(ArtistIndex, Vec<(TrackId, String, String)>)> {
        let rows = self.track_artist_fields()?;
        let mut st = self.conn.prepare_cached("SELECT DISTINCT album_artist FROM tracks WHERE album_artist != ''")?;
        let album_artists: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let exceptions = self.artist_split_exceptions();
        let index = ArtistIndex::build(
            rows.iter().map(|(_, a, c)| (a.as_str(), c.as_str())),
            album_artists.iter().map(String::as_str),
            exceptions.iter().map(String::as_str),
        );
        Ok((index, rows))
    }

    pub fn artist_index(&self) -> Result<ArtistIndex> {
        Ok(self.artist_index_and_rows()?.0)
    }

    /// The artists of one track, e.g. for "Go to Artist".
    pub fn track_artists(&self, id: TrackId) -> Result<Vec<String>> {
        let (index, rows) = self.artist_index_and_rows()?;
        Ok(rows
            .iter()
            .find(|(tid, _, _)| *tid == id)
            .map(|(_, a, c)| index.artists_of_track(a, c))
            .unwrap_or_default())
    }

    /// Ids of the tracks by `artist`, collaborations included.
    fn artist_track_ids(&self, artist: &str) -> Result<Vec<TrackId>> {
        let (index, rows) = self.artist_index_and_rows()?;
        let mut cache: HashMap<(&str, &str), bool> = HashMap::new();
        Ok(rows
            .iter()
            .filter(|(_, a, c)| *cache.entry((a.as_str(), c.as_str())).or_insert_with(|| index.track_has_artist(a, c, artist)))
            .map(|(id, _, _)| *id)
            .collect())
    }

    fn artist_ids_for(&self, filter: &TrackFilter) -> Result<Option<Vec<TrackId>>> {
        match &filter.artist {
            Some(a) => Ok(Some(self.artist_track_ids(a)?)),
            None => Ok(None),
        }
    }

    /// Artists, one entry per artist: a collaboration counts towards each of
    /// its members rather than being an artist of its own.
    fn split_artists(&self, search: &str) -> Result<Vec<ArtistSummary>> {
        let filter = TrackFilter {
            search: search.to_string(),
            ..Default::default()
        };
        let mut params = Vec::new();
        let where_sql = filter.where_clause(&mut params, None);
        let sql = format!("SELECT artist, artists, album, duration_ms, play_count FROM tracks t {where_sql}");
        let index = self.artist_index()?;
        #[derive(Default)]
        struct Acc {
            spellings: HashMap<String, usize>,
            albums: HashSet<String>,
            tracks: i64,
            duration_ms: i64,
            plays: i64,
        }
        let mut by_artist: HashMap<String, Acc> = HashMap::new();
        let mut split_cache: HashMap<(String, String), Vec<String>> = HashMap::new();
        let mut st = self.conn.prepare_cached(&sql)?;
        let mut rows = st.query(rusqlite::params_from_iter(params))?;
        while let Some(r) = rows.next()? {
            let tag: String = r.get(0)?;
            let credits: String = r.get(1)?;
            let album: String = r.get(2)?;
            let duration: i64 = r.get(3)?;
            let plays: i64 = r.get(4)?;
            let artists = split_cache.entry((tag.clone(), credits.clone())).or_insert_with(|| {
                let a = index.artists_of_track(&tag, &credits);
                if a.is_empty() {
                    vec![String::new()]
                } else {
                    a
                }
            });
            for a in artists.iter() {
                let acc = by_artist.entry(a.to_lowercase()).or_default();
                *acc.spellings.entry(a.clone()).or_default() += 1;
                acc.albums.insert(album.clone());
                acc.tracks += 1;
                acc.duration_ms += duration;
                acc.plays += plays;
            }
        }
        let mut out: Vec<ArtistSummary> = by_artist
            .into_values()
            .map(|acc| {
                // Show the spelling used most ("Charli XCX" over "Charli xcx").
                let name = acc
                    .spellings
                    .iter()
                    .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                    .map(|(s, _)| s.clone())
                    .unwrap_or_default();
                ArtistSummary {
                    name,
                    album_count: acc.albums.len() as i64,
                    track_count: acc.tracks,
                    duration_ms: acc.duration_ms,
                    play_count: acc.plays,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    pub fn artists(&self, mode: ArtistMode, search: &str) -> Result<Vec<ArtistSummary>> {
        let group = match mode {
            ArtistMode::Artist => return self.split_artists(search),
            ArtistMode::AlbumArtist => "aa",
        };
        let filter = TrackFilter {
            search: search.to_string(),
            ..Default::default()
        };
        let mut params = Vec::new();
        let where_sql = filter.where_clause(&mut params, None);
        let sql = format!(
            "SELECT {group} AS name, COUNT(DISTINCT album), COUNT(*), SUM(duration_ms), SUM(play_count) \
             FROM (SELECT t.*, {EFFECTIVE_ALBUM_ARTIST} AS aa FROM tracks t) t {where_sql} \
             GROUP BY name ORDER BY name COLLATE NOCASE"
        );
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(params), |r| {
            Ok(ArtistSummary {
                name: r.get(0)?,
                album_count: r.get(1)?,
                track_count: r.get(2)?,
                duration_ms: r.get(3)?,
                play_count: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn genres(&self) -> Result<Vec<GenreSummary>> {
        let mut st = self.conn.prepare_cached(
            "SELECT genre, COUNT(*), SUM(duration_ms) FROM tracks WHERE missing = 0 \
             GROUP BY genre ORDER BY genre COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| {
            Ok(GenreSummary {
                name: r.get(0)?,
                track_count: r.get(1)?,
                duration_ms: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Tracks of one album in disc/track order.
    pub fn album_tracks(&self, key: &AlbumKey) -> Result<Vec<Track>> {
        self.query_tracks(
            &TrackFilter {
                album: Some(key.clone()),
                ..Default::default()
            },
            Sort {
                key: SortKey::TrackNo,
                descending: false,
            },
        )
    }

    /// Distinct values for autocomplete in the tag editor.
    pub fn distinct_values(&self, column: &str) -> Result<Vec<String>> {
        let col = match column {
            "artist" | "album_artist" | "album" | "genre" | "composer" => column,
            _ => anyhow::bail!("unsupported column {column}"),
        };
        let mut st = self.conn.prepare(&format!(
            "SELECT DISTINCT {col} FROM tracks WHERE {col} != '' ORDER BY {col} COLLATE NOCASE"
        ))?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(lib: &Library, path: &str, title: &str, artist: &str, album: &str, aa: &str, no: i32) {
        lib.upsert_scanned(
            path,
            &TagData {
                title: title.into(),
                artist: artist.into(),
                album: album.into(),
                album_artist: aa.into(),
                track_no: Some(no),
                ..Default::default()
            },
            &AudioProps {
                duration_ms: 1000,
                ..Default::default()
            },
            1,
            1,
            None,
            None,
        )
        .unwrap();
    }

    #[test]
    fn views() {
        let lib = Library::open_in_memory().unwrap();
        add(&lib, "/1", "Bee", "Zed", "Comp", "Various Artists", 2);
        add(&lib, "/2", "Ant", "Amy", "Comp", "Various Artists", 1);
        add(&lib, "/3", "Cat", "Amy", "Solo", "", 1);

        let all = lib.query_tracks(&TrackFilter::default(), Sort::new(SortKey::Title)).unwrap();
        assert_eq!(all.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Ant", "Bee", "Cat"]);

        let albums = lib.albums(&TrackFilter::default(), AlbumSort::Artist).unwrap();
        assert_eq!(albums.len(), 2);
        assert_eq!(albums[0].album_artist, "Amy");
        assert_eq!(albums[1].track_count, 2);

        let comp = lib.album_tracks(&albums[1].key()).unwrap();
        assert_eq!(comp[0].title, "Ant");

        let artists = lib.artists(ArtistMode::Artist, "").unwrap();
        assert_eq!(artists.len(), 2);
        let aas = lib.artists(ArtistMode::AlbumArtist, "").unwrap();
        assert_eq!(aas.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Amy", "Various Artists"]);

        let found = lib
            .query_tracks(
                &TrackFilter {
                    search: "am".into(),
                    ..Default::default()
                },
                Sort::default(),
            )
            .unwrap();
        assert_eq!(found.len(), 2);
    }
}
