//! Where the library keeps its data on each platform.

use std::path::PathBuf;

/// Application data directory (database, art cache, settings).
/// Override with the `YERG_DATA_DIR` environment variable.
pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("YERG_DATA_DIR") {
        return PathBuf::from(p);
    }
    directories::ProjectDirs::from("dev", "yerg", "YergLibrary")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".yerg-library"))
}

pub fn database_path() -> PathBuf {
    data_dir().join("library.db")
}

/// Default music folder for first-run suggestions.
pub fn default_music_dir() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|u| u.audio_dir().map(|p| p.to_path_buf()))
}

/// File extensions the scanner treats as audio.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "mp4", "aac", "wav", "aif", "aiff", "wv", "ape",
    "mpc",
];

pub fn is_audio_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}
