//! Library scanning: walk the configured folders, read tags in parallel and
//! bring the database up to date.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Result;
use crossbeam_channel::Sender;
use rayon::prelude::*;

use crate::art;
use crate::db::{normalize_path, Library};
use crate::lyrics;
use crate::model::TagData;
use crate::paths::is_audio_file;
use crate::tags::{self, FileInfo};

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Started { total: usize },
    Progress { done: usize, total: usize, path: String },
    Finished(ScanSummary),
}

#[derive(Debug, Clone, Default)]
pub struct ScanSummary {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub missing: usize,
    /// Unchanged tracks whose artist credits were read for the first time.
    pub credits_filled: usize,
    pub errors: Vec<String>,
    pub elapsed: Duration,
}

impl std::fmt::Display for ScanSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} added, {} updated, {} unchanged, {} missing, {} errors",
            self.added,
            self.updated,
            self.unchanged,
            self.missing,
            self.errors.len(),
        )?;
        if self.credits_filled > 0 {
            write!(f, ", artist credits read for {}", self.credits_filled)?;
        }
        write!(f, " in {:.1}s", self.elapsed.as_secs_f64())
    }
}

struct Found {
    path: PathBuf,
    mtime: i64,
    size: i64,
}

enum ArtSource {
    Embedded(crate::model::EmbeddedPicture),
    Folder(PathBuf),
    None,
}

struct ReadResult {
    found: Found,
    info: Result<FileInfo>,
    art: ArtSource,
    sidecar_lyrics: Option<String>,
}

fn walk(root: &Path, out: &mut Vec<Found>) {
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true; // the library folder itself, whatever its name
            }
            let name = e.file_name().to_string_lossy();
            !(name.starts_with('.') || name == "System Volume Information" || name == "$RECYCLE.BIN")
        })
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() || !is_audio_file(entry.path()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(Found {
            path: entry.into_path(),
            mtime,
            size: meta.len() as i64,
        });
    }
}

fn read_one(found: Found) -> ReadResult {
    let info = tags::read_file(&found.path);
    let art = match &info {
        Ok(i) if i.picture.is_some() => ArtSource::Embedded(i.picture.clone().unwrap()),
        _ => match found.path.parent().and_then(art::find_folder_art) {
            Some(p) => ArtSource::Folder(p),
            None => ArtSource::None,
        },
    };
    let sidecar_lyrics = lyrics::read_sidecar(&found.path);
    ReadResult {
        found,
        info,
        art,
        sidecar_lyrics,
    }
}

/// Scan every library folder.  With `full`, every file is re-read even if its
/// size and modification time have not changed.
pub fn scan_library(lib: &Library, events: Option<&Sender<ScanEvent>>, full: bool) -> Result<ScanSummary> {
    let start = Instant::now();
    let folders = lib.folders()?;
    let mut summary = ScanSummary::default();

    let mut files = Vec::new();
    for f in &folders {
        walk(Path::new(&f.path), &mut files);
    }
    // Dedupe (a folder nested inside another)
    let mut seen_paths = HashSet::new();
    files.retain(|f| seen_paths.insert(normalize_path(&f.path)));

    let known = lib.track_fingerprints()?;
    // Never parsed successfully: retry even if unchanged (the reader may have improved).
    let unreadable = lib.unreadable_track_ids()?;
    let mut to_read = Vec::new();
    for f in files {
        let key = normalize_path(&f.path);
        match known.get(&key) {
            Some((mtime, size, id))
                if !full && *mtime == f.mtime && *size == f.size && !unreadable.contains(id) =>
            {
                summary.unchanged += 1;
            }
            _ => to_read.push(f),
        }
    }
    let total = to_read.len();
    send(events, ScanEvent::Started { total });

    // Files that exist on disk right now (for the missing check)
    let on_disk: HashSet<String> = seen_paths;

    let mut folder_art_cache: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut done = 0usize;
    const CHUNK: usize = 64;
    let mut iter = to_read.into_iter().peekable();
    while iter.peek().is_some() {
        let chunk: Vec<Found> = iter.by_ref().take(CHUNK).collect();
        let results: Vec<ReadResult> = chunk.into_par_iter().map(read_one).collect();

        lib.transaction(|lib| {
            for r in results {
                done += 1;
                let path_str = normalize_path(&r.found.path);
                let known_id = known.get(&path_str).map(|k| k.2);
                // Only placeholder tags so far: keep anything the user typed in since.
                let previous = match known_id {
                    Some(id) if unreadable.contains(&id) => lib.track(id)?,
                    _ => None,
                };
                match r.info {
                    Ok(mut info) => {
                        tags::fill_missing_from_path(&mut info.tags, &r.found.path);
                        if let Some(prev) = &previous {
                            info.tags = merge_over_placeholders(prev, &info.tags, &r.found.path);
                        }
                        let art_hash = match r.art {
                            ArtSource::Embedded(p) => lib.store_art(&p.mime, &p.data).ok(),
                            ArtSource::Folder(p) => folder_art_cache
                                .entry(p.clone())
                                .or_insert_with(|| {
                                    std::fs::read(&p)
                                        .ok()
                                        .and_then(|bytes| lib.store_art(art::mime_for_path(&p), &bytes).ok())
                                })
                                .clone(),
                            ArtSource::None => None,
                        };
                        // Lyrics: a sidecar .lrc wins over embedded plain text when it is synced.
                        let mut lyrics_text = pick_lyrics(&info.tags, r.sidecar_lyrics.as_deref());
                        let mut art_hash = art_hash;
                        if let Some(prev) = &previous {
                            // Artwork or lyrics set in the app while the file was unreadable win.
                            if prev.art_hash.is_some() {
                                art_hash = prev.art_hash.clone();
                            }
                            if prev.has_lyrics {
                                lyrics_text = None;
                            }
                        }
                        match lib.upsert_scanned(
                            &path_str,
                            &info.tags,
                            &info.props,
                            r.found.size,
                            r.found.mtime,
                            art_hash.as_deref(),
                            lyrics_text.as_deref(),
                        ) {
                            Ok((_, true)) => summary.added += 1,
                            Ok((_, false)) => summary.updated += 1,
                            Err(e) => summary.errors.push(format!("{path_str}: {e}")),
                        }
                    }
                    Err(e) => {
                        summary.errors.push(format!("{path_str}: {e:#}"));
                        match known_id {
                            // Already in the library: never replace its tags with guesses.
                            Some(id) => lib.set_file_stat(id, r.found.mtime, r.found.size)?,
                            // New and unreadable: still record it, with path-derived tags.
                            None => {
                                let mut t = TagData::default();
                                tags::fill_missing_from_path(&mut t, &r.found.path);
                                let props = crate::model::AudioProps::default();
                                if let Ok((_, true)) = lib.upsert_scanned(
                                    &path_str, &t, &props, r.found.size, r.found.mtime, None, None,
                                ) {
                                    summary.added += 1;
                                }
                            }
                        }
                    }
                }
                send(
                    events,
                    ScanEvent::Progress {
                        done,
                        total,
                        path: path_str,
                    },
                );
            }
            Ok(())
        })?;
    }

    summary.credits_filled = fill_artist_credits(lib, events)?;

    // Anything we knew about under a library folder that is no longer on disk
    let roots: Vec<String> = folders.iter().map(|f| f.path.clone()).collect();
    lib.transaction(|lib| {
        for (path, (_, _, id)) in &known {
            let under_root = roots.iter().any(|r| path.starts_with(r.as_str()));
            if under_root && !on_disk.contains(path) {
                lib.set_missing(*id, true)?;
                summary.missing += 1;
            }
        }
        Ok(())
    })?;

    summary.elapsed = start.elapsed();
    send(events, ScanEvent::Finished(summary.clone()));
    Ok(summary)
}

/// One-time catch-up for tracks scanned before artist credits were stored:
/// read just their ARTIST and ARTISTS fields.  The scanner skips unchanged
/// files, and a full re-read would replace tags edited only in the library,
/// so nothing else is touched.  Returns how many tracks were filled in.
fn fill_artist_credits(lib: &Library, events: Option<&Sender<ScanEvent>>) -> Result<usize> {
    let todo = lib.tracks_without_artist_credits()?;
    if todo.is_empty() {
        return Ok(0);
    }
    let total = todo.len();
    send(events, ScanEvent::Started { total });
    let mut done = 0usize;
    let mut filled = 0usize;
    for chunk in todo.chunks(64) {
        let read: Vec<_> = chunk
            .par_iter()
            .map(|(id, path, artist)| (*id, path.clone(), artist.clone(), tags::read_artist_fields(Path::new(path))))
            .collect();
        lib.transaction(|lib| {
            for (id, path, stored, result) in read {
                done += 1;
                match result {
                    Ok((values, credits)) => {
                        // Several ARTIST values used to be cut to the first one.  Store
                        // them all, unless the artist has been edited since.
                        let fuller = (values.len() > 1 && values.first() == Some(&stored)).then(|| values.join("; "));
                        lib.set_artist_credits(id, fuller.as_deref(), &credits)?;
                        filled += 1;
                    }
                    // Unreadable now: mark it done so it is not retried on every scan.
                    Err(_) => lib.set_artist_credits(id, None, &[])?,
                }
                send(events, ScanEvent::Progress { done, total, path });
            }
            Ok(())
        })?;
    }
    Ok(filled)
}

/// A track that was first added without readable tags holds placeholders
/// guessed from its path, possibly edited by the user since.  Now that the
/// file's real tags are available, take them for every field that still holds
/// its placeholder (or is blank) and keep every field the user changed.
fn merge_over_placeholders(prev: &crate::model::Track, file: &TagData, path: &Path) -> TagData {
    let mut guess = TagData::default();
    tags::fill_missing_from_path(&mut guess, path);
    let mut out = prev.to_tag_data(None);
    fn take_str(cur: &mut String, placeholder: &str, file: &str) {
        if cur.is_empty() || cur == placeholder {
            *cur = file.to_string();
        }
    }
    fn take_opt(cur: &mut Option<i32>, placeholder: Option<i32>, file: Option<i32>) {
        if cur.is_none() || *cur == placeholder {
            *cur = file;
        }
    }
    take_str(&mut out.title, &guess.title, &file.title);
    take_str(&mut out.artist, &guess.artist, &file.artist);
    take_str(&mut out.album, &guess.album, &file.album);
    take_str(&mut out.album_artist, "", &file.album_artist);
    take_str(&mut out.genre, "", &file.genre);
    take_str(&mut out.composer, "", &file.composer);
    take_str(&mut out.comment, "", &file.comment);
    take_opt(&mut out.track_no, guess.track_no, file.track_no);
    take_opt(&mut out.year, None, file.year);
    take_opt(&mut out.track_total, None, file.track_total);
    take_opt(&mut out.disc_no, None, file.disc_no);
    take_opt(&mut out.disc_total, None, file.disc_total);
    take_opt(&mut out.bpm, None, file.bpm);
    out.compilation = out.compilation || file.compilation;
    out.lyrics = file.lyrics.clone();
    out.artists = file.artists.clone();
    out
}

fn pick_lyrics(tags: &TagData, sidecar: Option<&str>) -> Option<String> {
    match (tags.lyrics.as_deref(), sidecar) {
        (Some(embedded), Some(side)) => {
            if lyrics::parse(side).synced && !lyrics::parse(embedded).synced {
                Some(side.to_string())
            } else {
                Some(embedded.to_string())
            }
        }
        (Some(e), None) => Some(e.to_string()),
        (None, Some(s)) => Some(s.to_string()),
        (None, None) => None,
    }
}

/// Re-read a single file (after a tag edit, for instance).
pub fn rescan_file(lib: &Library, path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let r = read_one(Found {
        path: path.to_path_buf(),
        mtime,
        size: meta.len() as i64,
    });
    let mut info = r.info?;
    tags::fill_missing_from_path(&mut info.tags, path);
    let art_hash = match r.art {
        ArtSource::Embedded(p) => Some(lib.store_art(&p.mime, &p.data)?),
        ArtSource::Folder(p) => std::fs::read(&p)
            .ok()
            .and_then(|b| lib.store_art(art::mime_for_path(&p), &b).ok()),
        ArtSource::None => None,
    };
    let lyr = pick_lyrics(&info.tags, r.sidecar_lyrics.as_deref());
    lib.upsert_scanned(
        &normalize_path(path),
        &info.tags,
        &info.props,
        r.found.size,
        r.found.mtime,
        art_hash.as_deref(),
        lyr.as_deref(),
    )?;
    Ok(())
}

fn send(events: Option<&Sender<ScanEvent>>, ev: ScanEvent) {
    if let Some(tx) = events {
        let _ = tx.send(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_io::test_support;
    use crate::model::AudioProps;

    #[test]
    fn rescans_previously_unreadable_files_keeping_user_edits() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Some Band").join("[2003] Some Album");
        std::fs::create_dir_all(&album).unwrap();
        let file = album.join("01 Placeholder.mp3");
        std::fs::write(&file, test_support::mp3_with_malformed_wxxx()).unwrap();
        let lib = Library::open_in_memory().unwrap();
        lib.add_folder(dir.path()).unwrap();

        // What an older build stored: path-guessed tags and empty audio properties...
        let meta = std::fs::metadata(&file).unwrap();
        let mtime = meta.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let mut guess = TagData::default();
        tags::fill_missing_from_path(&mut guess, &file);
        let path = normalize_path(&file);
        let (id, _) = lib
            .upsert_scanned(&path, &guess, &AudioProps::default(), meta.len() as i64, mtime, None, None)
            .unwrap();
        // ...then the user corrected the artist in the app.
        let mut edited = guess.clone();
        edited.artist = "Edited Artist".into();
        lib.update_track_tags(id, &edited).unwrap();

        // An incremental scan picks it up even though the file did not change.
        let summary = scan_library(&lib, None, false).unwrap();
        assert_eq!(summary.updated, 1, "{summary}");
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        let t = lib.track(id).unwrap().unwrap();
        assert_eq!(t.title, "Tone", "real title from the file replaces the placeholder");
        assert_eq!(t.artist, "Edited Artist", "the user's edit is kept");
        assert!(!t.codec.is_empty());
        assert!(t.duration_ms > 1500, "duration {}", t.duration_ms);
        assert!(lib.unreadable_track_ids().unwrap().is_empty());

        // The file itself was not modified by scanning.
        assert_eq!(std::fs::read(&file).unwrap(), test_support::mp3_with_malformed_wxxx());
    }

    #[test]
    fn unreadable_file_does_not_clobber_existing_tags() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Band").join("Album").join("01 Song.mp3");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"definitely not audio").unwrap();
        let lib = Library::open_in_memory().unwrap();
        lib.add_folder(dir.path()).unwrap();
        scan_library(&lib, None, false).unwrap();
        let t = lib.track_by_path(&normalize_path(&file)).unwrap().unwrap();
        let mut edited = t.to_tag_data(None);
        edited.title = "My Title".into();
        lib.update_track_tags(t.id, &edited).unwrap();
        // Rescanned (still unreadable): the edit stays.
        scan_library(&lib, None, true).unwrap();
        assert_eq!(lib.track(t.id).unwrap().unwrap().title, "My Title");
    }

    /// An MP3 tagged with several ARTIST values and an ARTISTS list.
    fn write_mp3_with_artists(path: &Path, artists: &[&str], credits: &[&str]) {
        use lofty::prelude::*;
        use lofty::tag::{ItemValue, Tag, TagItem, TagType};
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, test_support::plain_mp3()).unwrap();
        let mut tagged = lofty::read_from_path(path).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Song".into());
        // ID3v2.4 keeps several values in one frame, separated by NULs.
        tag.push(TagItem::new(lofty::tag::ItemKey::TrackArtist, ItemValue::Text(artists.join("\0"))));
        if !credits.is_empty() {
            tag.push(TagItem::new(lofty::tag::ItemKey::TrackArtists, ItemValue::Text(credits.join("\0"))));
        }
        tagged.insert_tag(tag);
        tagged.save_to_path(path, lofty::config::WriteOptions::default()).unwrap();
    }

    #[test]
    fn reads_every_artist_value_and_the_artists_credits() {
        use crate::query::{ArtistMode, Sort, TrackFilter};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("Song.mp3");
        write_mp3_with_artists(&file, &["HEALTH", "Nine Inch Nails"], &["HEALTH", "Nine Inch Nails", "Someone"]);
        let lib = Library::open_in_memory().unwrap();
        lib.add_folder(dir.path()).unwrap();
        scan_library(&lib, None, false).unwrap();
        let t = lib.track_by_path(&normalize_path(&file)).unwrap().unwrap();
        // Both ARTIST values are kept (only the first used to be).
        assert_eq!(t.artist, "HEALTH; Nine Inch Nails");
        assert_eq!(lib.track_artists(t.id).unwrap(), ["HEALTH", "Nine Inch Nails", "Someone"]);
        let names: Vec<String> = lib.artists(ArtistMode::Artist, "").unwrap().into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["HEALTH", "Nine Inch Nails", "Someone"]);
        let by = |name: &str| {
            let f = TrackFilter {
                artist: Some(name.into()),
                ..Default::default()
            };
            lib.query_tracks(&f, Sort::default()).unwrap().len()
        };
        assert_eq!((by("Someone"), by("nine inch nails"), by("Nobody")), (1, 1, 0));
    }

    #[test]
    fn fills_in_credits_for_tracks_scanned_before_they_were_stored() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("x").join("a.mp3");
        let b = dir.path().join("x").join("b.mp3");
        write_mp3_with_artists(&a, &["HEALTH", "Nine Inch Nails"], &["HEALTH", "Nine Inch Nails"]);
        write_mp3_with_artists(&b, &["HEALTH", "Godflesh"], &[]);
        let lib = Library::open_in_memory().unwrap();
        lib.add_folder(dir.path()).unwrap();
        scan_library(&lib, None, false).unwrap();
        // How a library scanned by the previous version looks: first artist
        // only, no credits, not read yet.  And b's artist was edited since.
        lib.conn
            .execute("UPDATE tracks SET artist = 'HEALTH', artists = '', artists_read = 0", [])
            .unwrap();
        lib.conn
            .execute("UPDATE tracks SET artist = 'My Edit' WHERE path LIKE '%b.mp3'", [])
            .unwrap();
        let before = (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());

        let s = scan_library(&lib, None, false).unwrap();
        assert_eq!((s.unchanged, s.updated, s.credits_filled), (2, 0, 2));
        let ta = lib.track_by_path(&normalize_path(&a)).unwrap().unwrap();
        let tb = lib.track_by_path(&normalize_path(&b)).unwrap().unwrap();
        assert_eq!(ta.artist, "HEALTH; Nine Inch Nails");
        assert_eq!(lib.track_artists(ta.id).unwrap(), ["HEALTH", "Nine Inch Nails"]);
        assert_eq!(tb.artist, "My Edit", "an edited artist is kept");
        // Done once: the next scan has nothing to fill in, and no file was touched.
        assert_eq!(scan_library(&lib, None, false).unwrap().credits_filled, 0);
        assert_eq!((std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap()), before);
    }

    #[test]
    fn saving_tags_keeps_artist_values_unless_the_artist_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Song.mp3");
        write_mp3_with_artists(&file, &["HEALTH", "Nine Inch Nails"], &["HEALTH", "Nine Inch Nails"]);
        let mut data = tags::read_file(&file).unwrap().tags;
        assert_eq!(data.artist, "HEALTH; Nine Inch Nails");

        // Another field edited: the separate artist values and credits stay.
        data.title = "Renamed".into();
        tags::write_tags(&file, &data).unwrap();
        let (values, credits) = tags::read_artist_fields(&file).unwrap();
        assert_eq!(values, ["HEALTH", "Nine Inch Nails"]);
        assert_eq!(credits, ["HEALTH", "Nine Inch Nails"]);

        // The artist edited: written as typed, and the stale credits go.
        data.artist = "HEALTH & Someone".into();
        tags::write_tags(&file, &data).unwrap();
        let (values, credits) = tags::read_artist_fields(&file).unwrap();
        assert_eq!(values, ["HEALTH & Someone"]);
        assert!(credits.is_empty());
    }
}
