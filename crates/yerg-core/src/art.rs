//! Album art: content-addressed blobs in the database plus lazily built thumbnails.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

use crate::db::Library;
use crate::ids::fnv1a64;

/// Hash used as the art key.  Length is mixed in to make accidental collisions rarer.
pub fn hash_bytes(data: &[u8]) -> String {
    format!("{:016x}{:08x}", fnv1a64(data), data.len() as u32)
}

const FOLDER_ART_NAMES: &[&str] = &[
    "cover", "folder", "front", "album", "albumart", "albumartsmall", "art", "artwork",
];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];

/// Look for a cover image sitting next to the audio files.
pub fn find_folder_art(dir: &Path) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    for name in FOLDER_ART_NAMES {
        if let Some(p) = entries.iter().find(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        }) {
            return Some(p.clone());
        }
    }
    // Any image will do if it is the only one.
    if entries.len() == 1 {
        return Some(entries[0].clone());
    }
    None
}

pub fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        _ => "image/jpeg",
    }
}

/// Decode any supported image into RGBA8.  Returns (width, height, pixels).
pub fn decode_rgba(data: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(data).context("decoding image")?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((w, h, rgba.into_raw()))
}

/// Shrink an image so its longest side is `size` pixels and encode as JPEG.
pub fn make_thumbnail(data: &[u8], size: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory(data).context("decoding image")?;
    let thumb = img.thumbnail(size, size).to_rgb8();
    let mut out = Cursor::new(Vec::new());
    thumb
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .context("encoding thumbnail")?;
    Ok(out.into_inner())
}

impl Library {
    /// Store an image if it is not already there and return its hash.
    pub fn store_art(&self, mime: &str, data: &[u8]) -> Result<String> {
        let hash = hash_bytes(data);
        self.conn.execute(
            "INSERT OR IGNORE INTO art(hash, mime, data) VALUES (?1, ?2, ?3)",
            params![hash, mime, data],
        )?;
        Ok(hash)
    }

    pub fn has_art(&self, hash: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM art WHERE hash = ?1", [hash], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Full-size art bytes and mime type.
    pub fn art(&self, hash: &str) -> Result<Option<(String, Vec<u8>)>> {
        Ok(self
            .conn
            .query_row("SELECT mime, data FROM art WHERE hash = ?1", [hash], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?)
    }

    /// A JPEG thumbnail (longest side 256px), built on first use.
    pub fn art_thumb(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let row: Option<(Option<Vec<u8>>, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT thumb, data FROM art WHERE hash = ?1",
                [hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((thumb, data)) = row else { return Ok(None) };
        if let Some(t) = thumb {
            return Ok(Some(t));
        }
        let t = make_thumbnail(&data, 256)?;
        self.conn.execute(
            "UPDATE art SET thumb = ?1 WHERE hash = ?2",
            params![t, hash],
        )?;
        Ok(Some(t))
    }

    /// Point a track at a new (or no) image.
    pub fn set_track_art(&self, track_id: i64, hash: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET art_hash = ?1 WHERE id = ?2",
            params![hash, track_id],
        )?;
        Ok(())
    }

    /// Drop art rows that no track references any more.  Returns rows removed.
    pub fn prune_art(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM art WHERE hash NOT IN (SELECT DISTINCT art_hash FROM tracks WHERE art_hash IS NOT NULL)",
            [],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_roundtrip() {
        // 64x48 solid colour PNG
        let img = image::RgbaImage::from_pixel(64, 48, image::Rgba([200, 30, 30, 255]));
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let data = png.into_inner();
        let thumb = make_thumbnail(&data, 16).unwrap();
        let (w, h, _) = decode_rgba(&thumb).unwrap();
        assert_eq!((w, h), (16, 12));

        let lib = Library::open_in_memory().unwrap();
        let hash = lib.store_art("image/png", &data).unwrap();
        assert!(lib.has_art(&hash).unwrap());
        assert!(lib.art_thumb(&hash).unwrap().is_some());
        // Second call returns the cached thumb
        assert!(lib.art_thumb(&hash).unwrap().is_some());
    }
}
