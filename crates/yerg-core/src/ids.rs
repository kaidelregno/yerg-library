//! Stable public identifiers.
//!
//! Database row ids can change if the library is rebuilt, so every track also
//! carries a UUID.  The MP3 player only has room for a 32-bit id per track, so
//! the device-facing id is an FNV-1a hash of that UUID.  Collisions are checked
//! at sync time and resolved by salting (see `yerg-sync`).

pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Device id for a track uuid.  Never zero (zero means "no track" on the device).
pub fn device_id(uuid: &str) -> u32 {
    let h = fnv1a32(uuid.as_bytes());
    if h == 0 {
        1
    } else {
        h
    }
}

pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_known_vectors() {
        assert_eq!(fnv1a32(b""), 0x811c_9dc5);
        assert_eq!(fnv1a32(b"a"), 0xe40c_292c);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
