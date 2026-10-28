//! `INDEX.BIN`: the on-card library index.
//!
//! The firmware has 16 MB of RAM and no filesystem-walking budget at boot, so
//! the index is a handful of fixed-stride tables it can `fread` directly into
//! arrays, plus a string pool it references by byte offset.  Every integer is
//! little-endian.  Strings are ASCII (transliterated) and NUL-terminated;
//! offset 0 is always the empty string.
//!
//! ```text
//! Header (32 bytes)
//!   0  "YRGX"            magic
//!   4  u16 version       = 1
//!   6  u16 section_count
//!   8  u32 flags         bit0: strings are pure ASCII
//!  12  u32 generated_at  unix seconds
//!  16  u32 library_id
//!  20  u32 total_size    whole file, bytes
//!  24  u32 crc32         of everything after the directory
//!  28  u32 reserved
//! Directory: section_count x 16 bytes
//!   0  char[4] tag   4 u32 offset   8 u32 count   12 u16 stride   14 u16 flags
//! Sections are 4-byte aligned.
//! ```
//!
//! Sections (see the `TRACK_STRIDE` etc. constants for record layouts):
//!
//! | tag    | record                                   | sorted by                         |
//! |--------|------------------------------------------|-----------------------------------|
//! | `STRS` | bytes                                    | -                                 |
//! | `TRKS` | track, 40 B                              | album artist, album, disc, track  |
//! | `ALBS` | album, 16 B (range into TRKS)            | album artist, year, name          |
//! | `ARTS` | album artist, 16 B (ranges into ALBS/TRKS)| name                             |
//! | `PLST` | playlist, 16 B (range into PLEN)         | library order                     |
//! | `PLEN` | u16 track index                          | playlist order                    |
//! | `TTLO` | u16 track index                          | title                             |
//! | `ALBO` | u16 album index                          | album name                        |
//! | `JMPT` | 27 x u16 index into TTLO ("#", A..Z)     |                                   |
//! | `JMPA` | 27 x u16 index into ARTS                 |                                   |
//! | `JMPB` | 27 x u16 index into ALBO                 |                                   |

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::crc32;
use crate::names::ascii_fold;

pub const MAGIC: &[u8; 4] = b"YRGX";
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 32;
pub const DIR_ENTRY_SIZE: usize = 16;
pub const TRACK_STRIDE: usize = 40;
pub const ALBUM_STRIDE: usize = 16;
pub const ARTIST_STRIDE: usize = 16;
pub const PLAYLIST_STRIDE: usize = 16;
pub const FLAG_ASCII: u32 = 1;
pub const TRACK_FLAG_COMPILATION: u16 = 1;
pub const TRACK_FLAG_HAS_LYRICS: u16 = 2;
/// u16 indices everywhere: the firmware's lists are capped here.
pub const MAX_TRACKS: usize = 65_535;

/// One track as it goes into the index.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexTrack {
    /// Stable 32-bit id, referenced by the listen log.
    pub id: u32,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub genre: String,
    /// Absolute path on the card, forward slashes, e.g. `/MUSIC/A/B/01 C.flac`.
    pub path: String,
    pub length_ms: u32,
    pub track_no: u16,
    pub disc_no: u8,
    pub rating: u8,
    pub year: u16,
    pub compilation: bool,
    pub has_lyrics: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexPlaylist {
    pub name: String,
    /// Track ids (as in [`IndexTrack::id`]); unknown ids are dropped.
    pub track_ids: Vec<u32>,
}

/// Decoded view of an index (what the firmware would reconstruct).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Index {
    pub generated_at: u32,
    pub library_id: u32,
    pub tracks: Vec<IndexTrack>,
    pub albums: Vec<IndexAlbum>,
    pub artists: Vec<IndexArtist>,
    pub playlists: Vec<IndexPlaylistRange>,
    pub playlist_entries: Vec<u16>,
    pub title_order: Vec<u16>,
    pub album_order: Vec<u16>,
    pub jump_titles: Vec<u16>,
    pub jump_artists: Vec<u16>,
    pub jump_albums: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexAlbum {
    pub name: String,
    pub artist: String,
    pub first_track: u16,
    pub track_count: u16,
    pub year: u16,
    pub artist_idx: u16,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexArtist {
    pub name: String,
    pub first_album: u16,
    pub album_count: u16,
    pub first_track: u16,
    pub track_count: u16,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexPlaylistRange {
    pub name: String,
    pub first_entry: u32,
    pub entry_count: u32,
    pub total_ms: u32,
}

struct StringPool {
    bytes: Vec<u8>,
    seen: HashMap<String, u32>,
}

impl StringPool {
    fn new() -> StringPool {
        let mut p = StringPool {
            bytes: vec![0],
            seen: HashMap::new(),
        };
        p.seen.insert(String::new(), 0);
        p
    }
    fn intern(&mut self, s: &str) -> u32 {
        let folded = ascii_fold(s);
        if let Some(off) = self.seen.get(&folded) {
            return *off;
        }
        let off = self.bytes.len() as u32;
        self.bytes.extend_from_slice(folded.as_bytes());
        self.bytes.push(0);
        self.seen.insert(folded, off);
        off
    }
}

fn sort_key(s: &str) -> String {
    let mut k = ascii_fold(s).to_ascii_lowercase();
    for prefix in ["the ", "a ", "an "] {
        if k.starts_with(prefix) && k.len() > prefix.len() {
            k = k[prefix.len()..].to_string();
            break;
        }
    }
    k
}

fn letter_bucket(s: &str) -> usize {
    match sort_key(s).chars().next() {
        Some(c) if c.is_ascii_alphabetic() => (c.to_ascii_uppercase() as u8 - b'A') as usize + 1,
        _ => 0,
    }
}

/// Jump table: for each of 27 buckets, the first position in `order` whose key
/// falls in that bucket (or the next populated bucket's start).
fn jump_table(keys: &[String]) -> Vec<u16> {
    let mut starts = vec![u16::MAX; 27];
    for (pos, k) in keys.iter().enumerate() {
        let b = letter_bucket(k);
        if starts[b] == u16::MAX {
            starts[b] = pos as u16;
        }
    }
    let mut next = keys.len() as u16;
    for b in (0..27).rev() {
        if starts[b] == u16::MAX {
            starts[b] = next;
        } else {
            next = starts[b];
        }
    }
    starts
}

struct Section {
    tag: [u8; 4],
    count: u32,
    stride: u16,
    data: Vec<u8>,
}

fn push_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn push_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// Build the index bytes.  Tracks may come in any order; they are sorted here.
pub fn build(
    tracks: &[IndexTrack],
    playlists: &[IndexPlaylist],
    library_id: u32,
    generated_at: u32,
) -> Result<Vec<u8>> {
    if tracks.len() > MAX_TRACKS {
        bail!("too many tracks for the device index ({} > {MAX_TRACKS})", tracks.len());
    }
    let mut pool = StringPool::new();

    // ---- order tracks: album artist, album, disc, track, title
    let mut order: Vec<usize> = (0..tracks.len()).collect();
    let artist_of = |t: &IndexTrack| -> String {
        if !t.album_artist.is_empty() {
            t.album_artist.clone()
        } else if t.compilation {
            "Various Artists".to_string()
        } else {
            t.artist.clone()
        }
    };
    order.sort_by_cached_key(|&i| {
        let t = &tracks[i];
        (
            sort_key(&artist_of(t)),
            artist_of(t).to_ascii_lowercase(),
            sort_key(&t.album),
            t.album.to_ascii_lowercase(),
            t.disc_no,
            t.track_no,
            sort_key(&t.title),
            t.id,
        )
    });
    let sorted: Vec<&IndexTrack> = order.iter().map(|&i| &tracks[i]).collect();

    // ---- albums and artists as contiguous ranges
    let mut albums: Vec<IndexAlbum> = Vec::new();
    let mut artists: Vec<IndexArtist> = Vec::new();
    let mut track_album_idx = vec![0u16; sorted.len()];
    let mut track_artist_idx = vec![0u16; sorted.len()];
    for (pos, t) in sorted.iter().enumerate() {
        let a_name = artist_of(t);
        let new_artist = artists
            .last()
            .map(|a| !a.name.eq_ignore_ascii_case(&a_name))
            .unwrap_or(true);
        if new_artist {
            artists.push(IndexArtist {
                name: a_name.clone(),
                first_album: albums.len() as u16,
                album_count: 0,
                first_track: pos as u16,
                track_count: 0,
            });
        }
        let new_album = new_artist
            || albums
                .last()
                .map(|al| !al.name.eq_ignore_ascii_case(&t.album))
                .unwrap_or(true);
        if new_album {
            albums.push(IndexAlbum {
                name: t.album.clone(),
                artist: a_name.clone(),
                first_track: pos as u16,
                track_count: 0,
                year: t.year,
                artist_idx: (artists.len() - 1) as u16,
            });
            artists.last_mut().unwrap().album_count += 1;
        }
        let al = albums.last_mut().unwrap();
        al.track_count += 1;
        if al.year == 0 && t.year != 0 {
            al.year = t.year;
        }
        artists.last_mut().unwrap().track_count += 1;
        track_album_idx[pos] = (albums.len() - 1) as u16;
        track_artist_idx[pos] = (artists.len() - 1) as u16;
    }

    // ---- id -> position, playlists
    let mut id_pos: HashMap<u32, u16> = HashMap::new();
    for (pos, t) in sorted.iter().enumerate() {
        if id_pos.insert(t.id, pos as u16).is_some() {
            bail!("duplicate device id {:08x} in index", t.id);
        }
    }
    let mut pl_records: Vec<IndexPlaylistRange> = Vec::new();
    let mut pl_entries: Vec<u16> = Vec::new();
    for p in playlists {
        let first = pl_entries.len() as u32;
        let mut total_ms: u64 = 0;
        for id in &p.track_ids {
            if let Some(pos) = id_pos.get(id) {
                pl_entries.push(*pos);
                total_ms += sorted[*pos as usize].length_ms as u64;
            }
        }
        pl_records.push(IndexPlaylistRange {
            name: p.name.clone(),
            first_entry: first,
            entry_count: pl_entries.len() as u32 - first,
            total_ms: total_ms.min(u32::MAX as u64) as u32,
        });
    }

    // ---- alternative orders and jump tables
    let mut title_order: Vec<u16> = (0..sorted.len() as u16).collect();
    title_order.sort_by_cached_key(|&i| (sort_key(&sorted[i as usize].title), i));
    let title_keys: Vec<String> = title_order.iter().map(|&i| sorted[i as usize].title.clone()).collect();
    let mut album_order: Vec<u16> = (0..albums.len() as u16).collect();
    album_order.sort_by_cached_key(|&i| (sort_key(&albums[i as usize].name), i));
    let album_keys: Vec<String> = album_order.iter().map(|&i| albums[i as usize].name.clone()).collect();
    let artist_keys: Vec<String> = artists.iter().map(|a| a.name.clone()).collect();

    // ---- encode records
    let mut trk = Vec::with_capacity(sorted.len() * TRACK_STRIDE);
    for (pos, t) in sorted.iter().enumerate() {
        push_u32(&mut trk, t.id);
        push_u32(&mut trk, pool.intern(&t.title));
        push_u32(&mut trk, pool.intern(&t.artist));
        push_u32(&mut trk, pool.intern(&t.album));
        push_u32(&mut trk, pool.intern(&t.path));
        push_u32(&mut trk, t.length_ms);
        push_u16(&mut trk, track_album_idx[pos]);
        push_u16(&mut trk, track_artist_idx[pos]);
        push_u16(&mut trk, t.track_no);
        trk.push(t.disc_no);
        trk.push(t.rating);
        push_u16(&mut trk, t.year);
        let mut flags = 0u16;
        if t.compilation {
            flags |= TRACK_FLAG_COMPILATION;
        }
        if t.has_lyrics {
            flags |= TRACK_FLAG_HAS_LYRICS;
        }
        push_u16(&mut trk, flags);
        push_u32(&mut trk, pool.intern(&t.genre));
        debug_assert_eq!(trk.len(), (pos + 1) * TRACK_STRIDE);
    }
    let mut alb = Vec::with_capacity(albums.len() * ALBUM_STRIDE);
    for a in &albums {
        push_u32(&mut alb, pool.intern(&a.name));
        push_u32(&mut alb, pool.intern(&a.artist));
        push_u16(&mut alb, a.first_track);
        push_u16(&mut alb, a.track_count);
        push_u16(&mut alb, a.year);
        push_u16(&mut alb, a.artist_idx);
    }
    let mut art = Vec::with_capacity(artists.len() * ARTIST_STRIDE);
    for a in &artists {
        push_u32(&mut art, pool.intern(&a.name));
        push_u16(&mut art, a.first_album);
        push_u16(&mut art, a.album_count);
        push_u16(&mut art, a.first_track);
        push_u16(&mut art, a.track_count);
        push_u32(&mut art, 0);
    }
    let mut pls = Vec::with_capacity(pl_records.len() * PLAYLIST_STRIDE);
    for p in &pl_records {
        push_u32(&mut pls, pool.intern(&p.name));
        push_u32(&mut pls, p.first_entry);
        push_u32(&mut pls, p.entry_count);
        push_u32(&mut pls, p.total_ms);
    }
    let u16s = |v: &[u16]| -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 2);
        for x in v {
            push_u16(&mut b, *x);
        }
        b
    };

    let sections = vec![
        Section { tag: *b"TRKS", count: sorted.len() as u32, stride: TRACK_STRIDE as u16, data: trk },
        Section { tag: *b"ALBS", count: albums.len() as u32, stride: ALBUM_STRIDE as u16, data: alb },
        Section { tag: *b"ARTS", count: artists.len() as u32, stride: ARTIST_STRIDE as u16, data: art },
        Section { tag: *b"PLST", count: pl_records.len() as u32, stride: PLAYLIST_STRIDE as u16, data: pls },
        Section { tag: *b"PLEN", count: pl_entries.len() as u32, stride: 2, data: u16s(&pl_entries) },
        Section { tag: *b"TTLO", count: title_order.len() as u32, stride: 2, data: u16s(&title_order) },
        Section { tag: *b"ALBO", count: album_order.len() as u32, stride: 2, data: u16s(&album_order) },
        Section { tag: *b"JMPT", count: 27, stride: 2, data: u16s(&jump_table(&title_keys)) },
        Section { tag: *b"JMPA", count: 27, stride: 2, data: u16s(&jump_table(&artist_keys)) },
        Section { tag: *b"JMPB", count: 27, stride: 2, data: u16s(&jump_table(&album_keys)) },
        Section { tag: *b"STRS", count: pool.bytes.len() as u32, stride: 1, data: pool.bytes },
    ];

    // ---- lay out
    let dir_end = HEADER_SIZE + sections.len() * DIR_ENTRY_SIZE;
    let mut body = Vec::new();
    let mut dir = Vec::with_capacity(sections.len() * DIR_ENTRY_SIZE);
    for s in &sections {
        while (dir_end + body.len()) % 4 != 0 {
            body.push(0);
        }
        let offset = (dir_end + body.len()) as u32;
        dir.extend_from_slice(&s.tag);
        push_u32(&mut dir, offset);
        push_u32(&mut dir, s.count);
        push_u16(&mut dir, s.stride);
        push_u16(&mut dir, 0);
        body.extend_from_slice(&s.data);
    }
    while (dir_end + body.len()) % 4 != 0 {
        body.push(0);
    }
    let total = dir_end + body.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(MAGIC);
    push_u16(&mut out, VERSION);
    push_u16(&mut out, sections.len() as u16);
    push_u32(&mut out, FLAG_ASCII);
    push_u32(&mut out, generated_at);
    push_u32(&mut out, library_id);
    push_u32(&mut out, total as u32);
    push_u32(&mut out, crc32(&body));
    push_u32(&mut out, 0);
    out.extend_from_slice(&dir);
    out.extend_from_slice(&body);
    debug_assert_eq!(out.len(), total);
    Ok(out)
}

// ------------------------------------------------------------------ reading

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn u16(&self, off: usize) -> Result<u16> {
        let b = self.bytes.get(off..off + 2).context("index truncated")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&self, off: usize) -> Result<u32> {
        let b = self.bytes.get(off..off + 4).context("index truncated")?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn str(&self, pool: (usize, usize), off: u32) -> Result<String> {
        let start = pool.0 + off as usize;
        let end = pool.0 + pool.1;
        if start > end {
            bail!("string offset {off} outside pool");
        }
        let slice = &self.bytes[start..end];
        let len = slice.iter().position(|b| *b == 0).unwrap_or(slice.len());
        Ok(String::from_utf8_lossy(&slice[..len]).to_string())
    }
}

/// Parse an index back into structured form (used for tests and `yerg index dump`).
pub fn read(bytes: &[u8]) -> Result<Index> {
    let r = Reader { bytes };
    if bytes.len() < HEADER_SIZE || &bytes[0..4] != MAGIC {
        bail!("not a YERG index (bad magic)");
    }
    let version = r.u16(4)?;
    if version != VERSION {
        bail!("unsupported index version {version}");
    }
    let nsec = r.u16(6)? as usize;
    let generated_at = r.u32(12)?;
    let library_id = r.u32(16)?;
    let total = r.u32(20)? as usize;
    if total != bytes.len() {
        bail!("index size mismatch: header says {total}, file is {}", bytes.len());
    }
    let dir_end = HEADER_SIZE + nsec * DIR_ENTRY_SIZE;
    if crc32(&bytes[dir_end..]) != r.u32(24)? {
        bail!("index checksum mismatch");
    }
    let mut sections: HashMap<[u8; 4], (usize, usize, usize)> = HashMap::new();
    for i in 0..nsec {
        let e = HEADER_SIZE + i * DIR_ENTRY_SIZE;
        let tag: [u8; 4] = bytes[e..e + 4].try_into().unwrap();
        let off = r.u32(e + 4)? as usize;
        let count = r.u32(e + 8)? as usize;
        let stride = r.u16(e + 12)? as usize;
        if off + count * stride > bytes.len() {
            bail!("section {} runs past end of file", String::from_utf8_lossy(&tag));
        }
        sections.insert(tag, (off, count, stride));
    }
    let sec = |tag: &[u8; 4]| -> Result<(usize, usize, usize)> {
        sections
            .get(tag)
            .copied()
            .with_context(|| format!("missing section {}", String::from_utf8_lossy(tag)))
    };
    let (pool_off, pool_len, _) = sec(b"STRS")?;
    let pool = (pool_off, pool_len);

    let mut idx = Index {
        generated_at,
        library_id,
        ..Default::default()
    };
    let (off, n, stride) = sec(b"TRKS")?;
    for i in 0..n {
        let b = off + i * stride;
        let flags = r.u16(b + 34)?;
        idx.tracks.push(IndexTrack {
            id: r.u32(b)?,
            title: r.str(pool, r.u32(b + 4)?)?,
            artist: r.str(pool, r.u32(b + 8)?)?,
            album: r.str(pool, r.u32(b + 12)?)?,
            path: r.str(pool, r.u32(b + 16)?)?,
            length_ms: r.u32(b + 20)?,
            track_no: r.u16(b + 28)?,
            disc_no: bytes[b + 30],
            rating: bytes[b + 31],
            year: r.u16(b + 32)?,
            compilation: flags & TRACK_FLAG_COMPILATION != 0,
            has_lyrics: flags & TRACK_FLAG_HAS_LYRICS != 0,
            genre: r.str(pool, r.u32(b + 36)?)?,
            album_artist: String::new(), // filled from ALBS below
        });
    }
    let (off, n, stride) = sec(b"ALBS")?;
    for i in 0..n {
        let b = off + i * stride;
        idx.albums.push(IndexAlbum {
            name: r.str(pool, r.u32(b)?)?,
            artist: r.str(pool, r.u32(b + 4)?)?,
            first_track: r.u16(b + 8)?,
            track_count: r.u16(b + 10)?,
            year: r.u16(b + 12)?,
            artist_idx: r.u16(b + 14)?,
        });
    }
    let (off, n, stride) = sec(b"ARTS")?;
    for i in 0..n {
        let b = off + i * stride;
        idx.artists.push(IndexArtist {
            name: r.str(pool, r.u32(b)?)?,
            first_album: r.u16(b + 4)?,
            album_count: r.u16(b + 6)?,
            first_track: r.u16(b + 8)?,
            track_count: r.u16(b + 10)?,
        });
    }
    let (off, n, stride) = sec(b"PLST")?;
    for i in 0..n {
        let b = off + i * stride;
        idx.playlists.push(IndexPlaylistRange {
            name: r.str(pool, r.u32(b)?)?,
            first_entry: r.u32(b + 4)?,
            entry_count: r.u32(b + 8)?,
            total_ms: r.u32(b + 12)?,
        });
    }
    let read_u16s = |tag: &[u8; 4]| -> Result<Vec<u16>> {
        let (off, n, stride) = sec(tag)?;
        (0..n).map(|i| r.u16(off + i * stride)).collect()
    };
    idx.playlist_entries = read_u16s(b"PLEN")?;
    idx.title_order = read_u16s(b"TTLO")?;
    idx.album_order = read_u16s(b"ALBO")?;
    idx.jump_titles = read_u16s(b"JMPT")?;
    idx.jump_artists = read_u16s(b"JMPA")?;
    idx.jump_albums = read_u16s(b"JMPB")?;
    for a in &idx.albums {
        for t in a.first_track..a.first_track + a.track_count {
            if let Some(tr) = idx.tracks.get_mut(t as usize) {
                tr.album_artist = a.artist.clone();
            }
        }
    }
    Ok(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u32, title: &str, artist: &str, album: &str, no: u16) -> IndexTrack {
        IndexTrack {
            id,
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            path: format!("/MUSIC/{artist}/{album}/{no:02} {title}.flac"),
            length_ms: 180_000,
            track_no: no,
            year: 2004,
            ..Default::default()
        }
    }

    #[test]
    fn roundtrip() {
        let tracks = vec![
            t(3, "Feather", "Nujabes", "Modal Soul", 1),
            t(1, "Luv(sic)", "Nujabes", "Hydeout", 2),
            t(2, "Awake", "Tycho", "Awake", 1),
            t(4, "Battle", "Nujabes", "Hydeout", 1),
            t(5, "Zed", "The Beatles", "Abbey Road", 1),
        ];
        let playlists = vec![IndexPlaylist {
            name: "Mix".into(),
            track_ids: vec![2, 3, 99],
        }];
        let bytes = build(&tracks, &playlists, 0xDEADBEEF, 1_700_000_000).unwrap();
        let idx = read(&bytes).unwrap();
        assert_eq!(idx.library_id, 0xDEADBEEF);
        assert_eq!(idx.tracks.len(), 5);
        // Sorted: The Beatles ("Beatles") / Nujabes / Tycho
        let artists: Vec<&str> = idx.artists.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(artists, ["The Beatles", "Nujabes", "Tycho"]);
        let n = &idx.artists[1];
        assert_eq!(n.album_count, 2);
        assert_eq!(n.track_count, 3);
        // Hydeout before Modal Soul; within Hydeout, track 1 then 2
        let hyde = &idx.albums[n.first_album as usize];
        assert_eq!(hyde.name, "Hydeout");
        assert_eq!(idx.tracks[hyde.first_track as usize].title, "Battle");
        assert_eq!(idx.tracks[hyde.first_track as usize + 1].title, "Luv(sic)");
        assert_eq!(idx.tracks[hyde.first_track as usize].album_artist, "Nujabes");
        // Playlist resolved 2 of 3 ids
        assert_eq!(idx.playlists[0].entry_count, 2);
        let first = idx.playlist_entries[idx.playlists[0].first_entry as usize];
        assert_eq!(idx.tracks[first as usize].id, 2);
        // Title order: Awake, Battle, Feather, Luv(sic), Zed
        let titles: Vec<&str> = idx.title_order.iter().map(|&i| idx.tracks[i as usize].title.as_str()).collect();
        assert_eq!(titles, ["Awake", "Battle", "Feather", "Luv(sic)", "Zed"]);
        // Jump table: 'A' bucket is index 1 -> position 0; 'Z' -> position 4; '#' -> 0
        assert_eq!(idx.jump_titles[1], 0);
        assert_eq!(idx.jump_titles[26], 4);
        assert_eq!(idx.jump_titles[2], 1); // B
        assert_eq!(idx.jump_titles[3], 2); // C has none -> next populated (Feather at 2)
        assert_eq!(bytes.len() % 4, 0);
    }

    #[test]
    fn rejects_corruption() {
        let bytes = build(&[t(1, "a", "b", "c", 1)], &[], 1, 1).unwrap();
        let mut bad = bytes.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        assert!(read(&bad).is_err());
        assert!(read(&bytes[..bytes.len() - 4]).is_err());
    }
}
