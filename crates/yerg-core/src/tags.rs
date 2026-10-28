//! Reading and writing file tags with `lofty` (ID3v1/v2, Vorbis comments,
//! MP4 atoms, APE, RIFF INFO, AIFF).

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::file::TaggedFile;
use lofty::tag::{ItemKey, Tag};

use crate::model::{AudioProps, EmbeddedPicture, TagData};

#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    pub tags: TagData,
    pub props: AudioProps,
    pub picture: Option<EmbeddedPicture>,
}

/// Parse a file's tags without modifying it.  Lofty (in every parsing mode)
/// rejects ID3v2 frames that are missing their text-encoding byte, such as the
/// `WXXX` frames some rippers write, so on failure the tag is repaired in a
/// memory copy and parsed from there.
pub fn open_tagged(path: &Path) -> Result<TaggedFile> {
    let first_err = match Probe::open(path)?.read() {
        Ok(t) => return Ok(t),
        Err(e) => e,
    };
    if let Some(bytes) = crate::audio_io::read_with_repaired_id3v2(path)? {
        let probe = Probe::new(Cursor::new(bytes)).guess_file_type()?;
        if let Ok(t) = probe.read() {
            log::warn!("{}: {first_err}; read after repairing its ID3v2 tag in memory", path.display());
            return Ok(t);
        }
    }
    Err(first_err.into())
}

/// Like [`open_tagged`], for callers about to save: a damaged ID3v2 tag is
/// repaired in the file itself (same byte length, audio untouched) so lofty
/// can then rewrite it.
fn open_tagged_for_write(path: &Path) -> Result<TaggedFile> {
    let first_err = match Probe::open(path)?.read() {
        Ok(t) => return Ok(t),
        Err(e) => e,
    };
    if crate::audio_io::repair_id3v2_in_place(path)? > 0 {
        log::warn!("{}: {first_err}; repaired its ID3v2 tag before saving", path.display());
        return Ok(Probe::open(path)?.read()?);
    }
    Err(first_err.into())
}

/// Read tags, stream properties and the front cover of an audio file.
pub fn read_file(path: &Path) -> Result<FileInfo> {
    let tagged = open_tagged(path).with_context(|| format!("reading tags of {}", path.display()))?;

    let p = tagged.properties();
    let props = AudioProps {
        duration_ms: p.duration().as_millis() as i64,
        bitrate_kbps: p.audio_bitrate().map(|b| b as i32),
        sample_rate: p.sample_rate().map(|s| s as i32),
        channels: p.channels().map(|c| c as i32),
        codec: format!("{:?}", tagged.file_type()),
    };

    let mut tags = TagData::default();
    let mut picture = None;
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        tags = tag_data_from(tag);
        picture = pick_picture(tag);
    }
    // Some files carry the cover in a secondary tag (e.g. ID3v2 next to APE).
    if picture.is_none() {
        for tag in tagged.tags() {
            picture = pick_picture(tag);
            if picture.is_some() {
                break;
            }
        }
    }
    Ok(FileInfo {
        tags,
        props,
        picture,
    })
}

fn pick_picture(tag: &Tag) -> Option<EmbeddedPicture> {
    let pics = tag.pictures();
    let pic = pics
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pics.first())?;
    if pic.data().is_empty() {
        return None;
    }
    Some(EmbeddedPicture {
        mime: pic
            .mime_type()
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "image/jpeg".to_string()),
        data: pic.data().to_vec(),
    })
}

fn text(tag: &Tag, key: ItemKey) -> String {
    tag.get_string(&key).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Every non-empty value of a (possibly multi-valued) text key, in order,
/// without repeats.
fn values(tag: &Tag, key: ItemKey) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in tag.get_strings(&key) {
        let v = v.trim();
        if !v.is_empty() && !out.iter().any(|o| o == v) {
            out.push(v.to_string());
        }
    }
    out
}

/// The artist as one display string.  Files with several ARTIST values
/// (Vorbis comments, ID3v2.4) are joined with "; " rather than keeping only
/// the first, which used to drop every artist after it.
fn joined_artist(tag: &Tag) -> String {
    values(tag, ItemKey::TrackArtist).join("; ")
}

/// lofty writes each value of a multi-valued key as its own ID3v2 frame and
/// only one survives, so saving any edit used to drop all artists but one.
/// Join them with NULs first: ID3v2.4's own way of holding several values.
/// (Vorbis comments and the rest keep separate values as they are.)
fn keep_id3v2_multi_values(tag: &mut Tag, key: ItemKey) {
    if tag.tag_type() != lofty::tag::TagType::Id3v2 {
        return;
    }
    let all = values(tag, key.clone());
    if all.len() > 1 {
        tag.insert_text(key, all.join("\0"));
    }
}

/// Just the artist fields of a file, for filling in credits on tracks the
/// scanner would otherwise skip: (ARTIST values, ARTISTS values).
pub fn read_artist_fields(path: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let tagged = open_tagged(path).with_context(|| format!("reading tags of {}", path.display()))?;
    Ok(match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(tag) => (values(tag, ItemKey::TrackArtist), values(tag, ItemKey::TrackArtists)),
        None => (Vec::new(), Vec::new()),
    })
}

fn tag_data_from(tag: &Tag) -> TagData {
    let bpm = tag
        .get_string(&ItemKey::Bpm)
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|b| b.round() as i32);
    let compilation = tag
        .get_string(&ItemKey::FlagCompilation)
        .map(|s| matches!(s.trim(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let lyrics = tag
        .get_string(&ItemKey::Lyrics)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    TagData {
        title: tag.title().map(|s| s.trim().to_string()).unwrap_or_default(),
        artist: joined_artist(tag),
        artists: values(tag, ItemKey::TrackArtists),
        album_artist: text(tag, ItemKey::AlbumArtist),
        album: tag.album().map(|s| s.trim().to_string()).unwrap_or_default(),
        genre: tag.genre().map(|s| s.trim().to_string()).unwrap_or_default(),
        composer: text(tag, ItemKey::Composer),
        comment: tag.comment().map(|s| s.trim().to_string()).unwrap_or_default(),
        year: tag.year().map(|y| y as i32),
        track_no: tag.track().map(|v| v as i32),
        track_total: tag.track_total().map(|v| v as i32),
        disc_no: tag.disk().map(|v| v as i32),
        disc_total: tag.disk_total().map(|v| v as i32),
        lyrics,
        compilation,
        bpm,
    }
}

fn set_text(tag: &mut Tag, key: ItemKey, value: &str) {
    if value.trim().is_empty() {
        tag.remove_key(&key);
    } else {
        tag.insert_text(key, value.trim().to_string());
    }
}

/// Write editable metadata back into the file.  `lyrics` of `Some("")` clears them,
/// `None` leaves the file's lyrics untouched.
pub fn write_tags(path: &Path, data: &TagData) -> Result<()> {
    let mut tagged = open_tagged_for_write(path).with_context(|| format!("reading {}", path.display()))?;

    let tag_type = tagged.primary_tag_type();
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .context("no writable tag in file")?;

    macro_rules! set_or_remove {
        ($setter:ident, $remover:ident, $val:expr) => {
            match $val {
                Some(v) => tag.$setter(v),
                None => tag.$remover(),
            }
        };
    }
    set_or_remove!(set_title, remove_title, non_empty(&data.title));
    // Only rewrite the artist when it was changed, so several ARTIST values
    // in the file are not collapsed into one joined string.  A changed artist
    // also drops the file's ARTISTS credits, which would no longer match.
    if joined_artist(tag) != data.artist.trim() {
        set_or_remove!(set_artist, remove_artist, non_empty(&data.artist));
        tag.remove_key(&ItemKey::TrackArtists);
    }
    set_or_remove!(set_album, remove_album, non_empty(&data.album));
    set_or_remove!(set_genre, remove_genre, non_empty(&data.genre));
    set_or_remove!(set_comment, remove_comment, non_empty(&data.comment));
    set_or_remove!(set_year, remove_year, data.year.filter(|y| *y > 0).map(|y| y as u32));
    set_or_remove!(set_track, remove_track, data.track_no.filter(|v| *v > 0).map(|v| v as u32));
    set_or_remove!(
        set_track_total,
        remove_track_total,
        data.track_total.filter(|v| *v > 0).map(|v| v as u32)
    );
    set_or_remove!(set_disk, remove_disk, data.disc_no.filter(|v| *v > 0).map(|v| v as u32));
    set_or_remove!(
        set_disk_total,
        remove_disk_total,
        data.disc_total.filter(|v| *v > 0).map(|v| v as u32)
    );
    set_text(tag, ItemKey::AlbumArtist, &data.album_artist);
    set_text(tag, ItemKey::Composer, &data.composer);
    set_text(
        tag,
        ItemKey::Bpm,
        &data.bpm.filter(|b| *b > 0).map(|b| b.to_string()).unwrap_or_default(),
    );
    set_text(tag, ItemKey::FlagCompilation, if data.compilation { "1" } else { "" });
    if let Some(l) = &data.lyrics {
        set_text(tag, ItemKey::Lyrics, l);
    }
    keep_id3v2_multi_values(tag, ItemKey::TrackArtist);
    keep_id3v2_multi_values(tag, ItemKey::TrackArtists);

    tagged
        .save_to_path(path, WriteOptions::default())
        .with_context(|| format!("saving tags to {}", path.display()))?;
    Ok(())
}

/// Replace (or remove, with `None`) the front cover picture.
pub fn write_picture(path: &Path, picture: Option<&EmbeddedPicture>) -> Result<()> {
    use lofty::picture::{MimeType, Picture};

    let mut tagged = open_tagged_for_write(path)?;
    let tag_type = tagged.primary_tag_type();
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged.primary_tag_mut().context("no writable tag")?;
    // Drop existing front covers.
    let mut i = 0;
    while i < tag.pictures().len() {
        if tag.pictures()[i].pic_type() == PictureType::CoverFront {
            tag.remove_picture(i);
        } else {
            i += 1;
        }
    }
    if let Some(p) = picture {
        let mime = MimeType::from_str(&p.mime);
        let pic = Picture::new_unchecked(PictureType::CoverFront, Some(mime), None, p.data.clone());
        tag.push_picture(pic);
    }
    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Fill blanks the way the Teensy firmware does: title from the file name,
/// artist and album from the folder layout or an "Artist - Title" name.
pub fn fill_missing_from_path(tags: &mut TagData, path: &Path) {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let album_dir = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());
    let artist_dir = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());

    // Strip a leading track number: "01 - Title", "01. Title", "1_Title"
    let mut title = stem.as_str();
    let digits = title.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let rest = &title[digits..];
        let trimmed = rest.trim_start_matches([' ', '-', '.', '_']);
        if trimmed.len() < rest.len() && !trimmed.is_empty() {
            if tags.track_no.is_none() {
                tags.track_no = title[..digits].parse().ok();
            }
            title = trimmed;
        }
    }
    let mut file_artist: Option<&str> = None;
    if let Some((a, t)) = title.split_once(" - ") {
        if !t.is_empty() {
            file_artist = Some(a);
            title = t;
        }
    }
    if tags.title.is_empty() {
        tags.title = title.trim().to_string();
    }
    if tags.artist.is_empty() {
        tags.artist = file_artist
            .map(|s| s.trim().to_string())
            .or(artist_dir)
            .unwrap_or_else(|| "Unknown Artist".to_string());
    }
    if tags.album.is_empty() {
        tags.album = album_dir.unwrap_or_else(|| "Unknown Album".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_from_folder_layout() {
        let mut t = TagData::default();
        fill_missing_from_path(&mut t, Path::new("/music/Nujabes/Metaphorical Music/03 - Blessing It.mp3"));
        assert_eq!(t.title, "Blessing It");
        assert_eq!(t.artist, "Nujabes");
        assert_eq!(t.album, "Metaphorical Music");
        assert_eq!(t.track_no, Some(3));
    }

    #[test]
    fn fill_from_dash_name() {
        let mut t = TagData::default();
        fill_missing_from_path(&mut t, Path::new("Tycho - Awake.flac"));
        assert_eq!(t.title, "Awake");
        assert_eq!(t.artist, "Tycho");
        assert_eq!(t.album, "Unknown Album");
    }
}
