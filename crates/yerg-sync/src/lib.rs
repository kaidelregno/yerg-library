//! Syncing the library to the Yerg MP3 player.
//!
//! The player is a Teensy 4.1 with 16 MB of RAM that reads music from an SD
//! card.  Sync:
//!
//! * copies each track unchanged, in its original format, to
//!   `/MUSIC/Artist/Album/NN Title.<ext>` (see [`device::copy_to_card`]);
//! * writes `/YERG/INDEX.BIN`, a set of fixed-stride tables the firmware can
//!   read straight into RAM (see [`index`]);
//! * reads back `/YERG/LISTENS.LOG`, the firmware's listen log, into the play
//!   history so those listens count and can be scrobbled (see [`listens`]).
//!
//! The whole on-card layout is documented in `docs/DEVICE_FORMAT.md`.

pub mod device;
pub mod index;
pub mod listens;
pub mod names;

/// Folder on the card that holds the index and logs.
pub const DEVICE_DIR: &str = "YERG";
/// Folder on the card that holds the music.
pub const MUSIC_DIR: &str = "MUSIC";
pub const INDEX_FILE: &str = "INDEX.BIN";
pub const LISTENS_FILE: &str = "LISTENS.LOG";
pub const DEVICE_FILE: &str = "DEVICE.TXT";
pub const PLAYLIST_DIR: &str = "PLAYLISTS";

/// IEEE CRC-32 (same polynomial as zip/png), used to checksum the index.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    let mut crc = 0xFFFF_FFFFu32;
    for b in data {
        crc = table[((crc ^ *b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc32_vector() {
        assert_eq!(super::crc32(b"123456789"), 0xCBF4_3926);
    }
}
