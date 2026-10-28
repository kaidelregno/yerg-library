//! ASCII, FAT-safe names for the card.  The player's font is ASCII only and
//! its path buffer is 128 bytes, so everything is transliterated and clipped.

use std::collections::HashSet;

use yerg_core::model::Track;

/// Longest path the firmware accepts, including the leading slash and the NUL.
pub const MAX_PATH: usize = 127;

/// Transliterate to ASCII and drop control characters.
pub fn ascii_fold(s: &str) -> String {
    let folded = deunicode::deunicode_with_tofu(s, "?");
    let mut out = String::with_capacity(folded.len());
    for c in folded.chars() {
        if c.is_ascii() && !c.is_ascii_control() {
            out.push(c);
        } else if !c.is_ascii() {
            out.push('?');
        } else {
            out.push(' ');
        }
    }
    collapse_spaces(&out)
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for c in s.chars() {
        if c == ' ' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// One path component safe for FAT32 and readable on the device.
pub fn fat_component(s: &str, max_len: usize) -> String {
    let folded = ascii_fold(s);
    let mut out = String::new();
    for c in folded.chars() {
        let ok = c.is_ascii_alphanumeric() || " -_.,'&!()[]+=@#".contains(c);
        out.push(if ok { c } else { '_' });
    }
    let mut out = collapse_spaces(&out);
    // FAT dislikes trailing dots/spaces and leading dots hide files.
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    let out = out.trim_start_matches('.').to_string();
    let mut clipped: String = out.chars().take(max_len).collect();
    while clipped.ends_with(['.', ' ']) {
        clipped.pop();
    }
    if clipped.is_empty() {
        "_".to_string()
    } else {
        clipped
    }
}

/// The source file's extension, lower-cased, for the copy on the card
/// (`mp3`, `flac`, `m4a`, …).  The firmware picks its decoder from it.
pub fn audio_extension(source_path: &str) -> String {
    std::path::Path::new(source_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".to_string())
}

/// `/MUSIC/Artist/Album/NN Title.<ext>` for a track, keeping the source
/// file's extension, made unique within `used` (case-insensitively, as FAT
/// is).  Returns the path with forward slashes.
pub fn device_path(track: &Track, device_id: u32, used: &mut HashSet<String>) -> String {
    let ext = format!(".{}", audio_extension(&track.path));
    let artist = fat_component(track.effective_album_artist(), 32);
    let album = fat_component(if track.album.is_empty() { "Unknown Album" } else { &track.album }, 32);
    let title_len = MAX_PATH
        .saturating_sub("/MUSIC/".len() + artist.len() + 1 + album.len() + 1 + 7 + ext.len())
        .clamp(8, 48);
    let title = fat_component(track.display_title(), title_len);
    let mut prefix = String::new();
    if let Some(d) = track.disc_no.filter(|d| *d > 1) {
        prefix.push_str(&format!("{d}-"));
    }
    if let Some(n) = track.track_no.filter(|n| *n > 0) {
        prefix.push_str(&format!("{n:02} "));
    }
    let base = format!("/MUSIC/{artist}/{album}/{prefix}{title}");
    let mut candidate = format!("{base}{ext}");
    if used.insert(candidate.to_ascii_lowercase()) && candidate.len() <= MAX_PATH {
        return candidate;
    }
    // Collision or too long: salt with the device id.
    let salt = format!("~{:04x}", device_id & 0xFFFF);
    let mut base_chars: Vec<char> = base.chars().collect();
    loop {
        let trial = format!("{}{}{ext}", base_chars.iter().collect::<String>(), salt);
        if trial.len() <= MAX_PATH {
            candidate = trial;
            break;
        }
        base_chars.pop();
    }
    if used.insert(candidate.to_ascii_lowercase()) {
        return candidate;
    }
    // Still colliding (same title twice with the same salt): number it.
    for n in 2..10_000 {
        let trial = format!("{}{}-{n}{ext}", base_chars.iter().collect::<String>(), salt);
        if used.insert(trial.to_ascii_lowercase()) {
            return trial;
        }
    }
    format!("/MUSIC/{:08x}{ext}", device_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, album: &str, no: Option<i32>) -> Track {
        Track {
            id: 1,
            uuid: "u".into(),
            path: "/x.mp3".into(),
            title: title.into(),
            artist: artist.into(),
            album_artist: String::new(),
            album: album.into(),
            genre: String::new(),
            composer: String::new(),
            comment: String::new(),
            year: None,
            track_no: no,
            track_total: None,
            disc_no: None,
            disc_total: None,
            duration_ms: 0,
            bitrate_kbps: None,
            sample_rate: None,
            channels: None,
            codec: String::new(),
            file_size: 0,
            mtime: 0,
            added_at: 0,
            rating: 0,
            play_count: 0,
            skip_count: 0,
            last_played: None,
            art_hash: None,
            has_lyrics: false,
            compilation: false,
            bpm: None,
            missing: false,
        }
    }

    #[test]
    fn folds_and_sanitises() {
        assert_eq!(ascii_fold("Björk – Jóga"), "Bjork - Joga");
        assert_eq!(fat_component("AC/DC: Back in Black...", 40), "AC_DC_ Back in Black");
        assert_eq!(fat_component("", 10), "_");
    }

    #[test]
    fn unique_paths() {
        let mut used = HashSet::new();
        let t = track("Song", "Artist", "Album", Some(1));
        let a = device_path(&t, 0xABCD, &mut used);
        assert_eq!(a, "/MUSIC/Artist/Album/01 Song.mp3");
        let b = device_path(&t, 0xABCD, &mut used);
        assert_ne!(a, b);
        assert!(b.ends_with("~abcd.mp3"));
        let c = device_path(&t, 0xABCD, &mut used);
        assert!(c.ends_with("~abcd-2.mp3"));
        assert!(a.len() <= MAX_PATH && b.len() <= MAX_PATH);
    }

    #[test]
    fn keeps_the_source_format() {
        let mut used = HashSet::new();
        let mut t = track("Song", "Artist", "Album", Some(1));
        t.path = r"E:\Music\Artist\Album\01 Song.FLAC".into();
        assert_eq!(device_path(&t, 1, &mut used), "/MUSIC/Artist/Album/01 Song.flac");
        // Same title in another format is a different file, not a collision.
        t.path = "/music/01 Song.m4a".into();
        assert_eq!(device_path(&t, 1, &mut used), "/MUSIC/Artist/Album/01 Song.m4a");
        assert_eq!(audio_extension("/a/b/no_extension"), "bin");
        assert_eq!(audio_extension("/a/b.c/song.opus"), "opus");
    }

    #[test]
    fn long_names_fit() {
        let mut used = HashSet::new();
        let long = "x".repeat(300);
        let mut t = track(&long, &long, &long, Some(12));
        t.path = "/x.flac".into();
        let p = device_path(&t, 1, &mut used);
        assert!(p.len() <= MAX_PATH, "{} > {}", p.len(), MAX_PATH);
        assert!(p.ends_with(".flac"));
    }
}
