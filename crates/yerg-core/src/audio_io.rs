//! Opening audio files for decoding when their tags are damaged.
//!
//! Symphonia reads a leading ID3v2 tag before it looks at the audio, and any
//! malformed frame in that tag (for example a `WXXX` frame written without its
//! text-encoding byte, as some rippers do) aborts the whole probe even though
//! the MPEG audio after it is fine.  [`SkipPrefix`] hides the tag so the
//! decoder sees the audio stream directly.  Lofty is more forgiving, so tags
//! are still read from these files as usual.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// Length in bytes of the ID3v2 tag(s) at the start of a stream, or 0.
/// Consecutive tags (rare, but some taggers prepend a second one) are all counted.
pub fn id3v2_prefix_len<R: Read + Seek>(r: &mut R) -> io::Result<u64> {
    let mut total = 0u64;
    loop {
        r.seek(SeekFrom::Start(total))?;
        let mut h = [0u8; 10];
        let mut got = 0;
        while got < h.len() {
            match r.read(&mut h[got..])? {
                0 => break,
                n => got += n,
            }
        }
        // "ID3", version 2..=4, synchsafe size bytes (top bit clear)
        if got < 10 || &h[0..3] != b"ID3" || !(2..=4).contains(&h[3]) || h[6..10].iter().any(|b| b & 0x80 != 0) {
            break;
        }
        let size = ((h[6] as u64) << 21) | ((h[7] as u64) << 14) | ((h[8] as u64) << 7) | h[9] as u64;
        let footer = if h[3] == 4 && h[5] & 0x10 != 0 { 10 } else { 0 };
        total += 10 + size + footer;
    }
    r.seek(SeekFrom::Start(0))?;
    Ok(total)
}

/// A reader that exposes everything after the first `start` bytes of `inner`
/// as if it were the whole stream.
pub struct SkipPrefix<R> {
    inner: R,
    start: u64,
    len: u64,
}

impl<R: Read + Seek> SkipPrefix<R> {
    pub fn new(mut inner: R, start: u64) -> io::Result<SkipPrefix<R>> {
        let end = inner.seek(SeekFrom::End(0))?;
        let start = start.min(end);
        inner.seek(SeekFrom::Start(start))?;
        Ok(SkipPrefix {
            inner,
            start,
            len: end - start,
        })
    }

    /// Bytes visible through this reader.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes hidden at the front.
    pub fn skipped(&self) -> u64 {
        self.start
    }
}

impl SkipPrefix<File> {
    /// Open `path` with any leading ID3v2 tag hidden.  Returns `None` when the
    /// file has no ID3v2 tag, so callers can tell a retry would not help.
    pub fn open_without_id3v2(path: &Path) -> io::Result<Option<SkipPrefix<File>>> {
        let mut f = File::open(path)?;
        let n = id3v2_prefix_len(&mut f)?;
        if n == 0 {
            return Ok(None);
        }
        SkipPrefix::new(f, n).map(Some)
    }
}

impl<R: Read> Read for SkipPrefix<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for SkipPrefix<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(n) => self.start.checked_add(n),
            SeekFrom::End(n) => (self.start + self.len).checked_add_signed(n),
            SeekFrom::Current(n) => {
                let cur = self.inner.stream_position()?;
                cur.checked_add_signed(n)
            }
        };
        match target {
            Some(t) if t >= self.start => Ok(self.inner.seek(SeekFrom::Start(t))? - self.start),
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start of stream")),
        }
    }
}

/// Fix ID3v2.3/2.4 text frames that lofty and symphonia refuse to read, in place.
///
/// `tag` is one whole ID3v2 tag (10-byte header plus body).  Text frames
/// (`T***` other than `TXXX`) missing their encoding byte get a Latin-1 one, and
/// `WXXX` gets one plus an empty description, so the original text is kept.
/// UTF-16 strings in `T***`, `TXXX`, `WXXX`, `COMM` and `USLT` frames lose stray
/// leading NULs and gain a byte-order mark if they have none.
/// The tag keeps its exact length: the extra bytes come out of the padding,
/// and if there is not enough padding the broken frames are dropped instead.
/// Returns how many frames were repaired or dropped (0 = left untouched).
/// Unsynchronised tags and compressed/encrypted frames are never modified.
pub fn repair_id3v2_frames(tag: &mut [u8]) -> usize {
    if tag.len() < 10 || &tag[0..3] != b"ID3" {
        return 0;
    }
    let version = tag[3];
    let flags = tag[5];
    if !(version == 3 || version == 4) || flags & 0x80 != 0 || tag[6..10].iter().any(|b| b & 0x80 != 0) {
        return 0;
    }
    let size = synchsafe(&tag[6..10]) as usize;
    if tag.len() < 10 + size {
        return 0;
    }
    let body = &tag[10..10 + size];
    let mut pos = 0usize;
    if flags & 0x40 != 0 && body.len() >= 4 {
        let ext = if version == 3 {
            u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize + 4
        } else {
            synchsafe(&body[0..4]) as usize
        };
        if ext > body.len() {
            return 0;
        }
        pos = ext;
    }
    let head = body[..pos].to_vec();

    struct Frame {
        raw: Vec<u8>,
        /// None: fine as is.  Some(None): drop.  Some(Some(bytes)): replacement frame.
        fixed: Option<Option<Vec<u8>>>,
    }
    let mut frames = Vec::new();
    while pos + 10 <= body.len() {
        let id = &body[pos..pos + 4];
        if !id.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
            break; // padding (zeros) or garbage
        }
        let fsize = if version == 3 {
            u32::from_be_bytes([body[pos + 4], body[pos + 5], body[pos + 6], body[pos + 7]]) as usize
        } else {
            synchsafe(&body[pos + 4..pos + 8]) as usize
        };
        if pos + 10 + fsize > body.len() {
            break;
        }
        let raw = body[pos..pos + 10 + fsize].to_vec();
        let fl = body[pos + 9];
        let opaque = if version == 3 { fl & 0xC0 != 0 } else { fl & 0x0F != 0 };
        let data = &raw[10..];
        let fix = if opaque { Fix::Keep } else { sanitize_frame(id, data) };
        let fixed = match fix {
            Fix::Keep => None,
            Fix::Drop => Some(None),
            Fix::Replace(d) => Some(Some(d)),
        };
        let fixed = fixed.map(|d| d.map(|d| {
            let mut f = raw[..10].to_vec();
            let n = d.len() as u32;
            let sz = if version == 3 {
                n.to_be_bytes()
            } else {
                [(n >> 21) as u8 & 0x7f, (n >> 14) as u8 & 0x7f, (n >> 7) as u8 & 0x7f, n as u8 & 0x7f]
            };
            f[4..8].copy_from_slice(&sz);
            f.extend(d);
            f
        }));
        frames.push(Frame { raw, fixed });
        pos += 10 + fsize;
    }
    let broken = frames.iter().filter(|f| f.fixed.is_some()).count();
    if broken == 0 {
        return 0;
    }
    // Whatever follows the frames (normally zero padding) is kept unless we need its room.
    let tail = &body[pos..];
    let tail_used = tail.iter().rposition(|b| *b != 0).map(|i| i + 1).unwrap_or(0);
    let build = |repair: bool| -> Vec<u8> {
        let mut out = head.clone();
        for f in &frames {
            match (&f.fixed, repair) {
                (Some(Some(fixed)), true) => out.extend_from_slice(fixed),
                (Some(_), _) => {} // drop it
                (None, _) => out.extend_from_slice(&f.raw),
            }
        }
        out.extend_from_slice(&tail[..tail_used]);
        out
    };
    let mut new_body = build(true);
    if new_body.len() > size {
        new_body = build(false);
    }
    if new_body.len() > size {
        return 0;
    }
    new_body.resize(size, 0);
    tag[10..10 + size].copy_from_slice(&new_body);
    broken
}

enum Fix {
    Keep,
    Replace(Vec<u8>),
    Drop,
}

/// Check the strings in a text-bearing frame and patch the faults lofty (and
/// symphonia) refuse to read.  Only `T***`, `TXXX`, `WXXX`, `COMM` and `USLT`
/// are inspected; everything else is kept as is.
fn sanitize_frame(id: &[u8], data: &[u8]) -> Fix {
    enum Kind {
        Text,          // T***: encoding, text (maybe several NUL-separated values)
        DescText,      // TXXX: encoding, description, text
        DescUrl,       // WXXX: encoding, description, Latin-1 URL
        LangDescText,  // COMM / USLT: encoding, language, description, text
    }
    let kind = match id {
        b"TXXX" => Kind::DescText,
        b"WXXX" => Kind::DescUrl,
        b"COMM" | b"USLT" => Kind::LangDescText,
        _ if id[0] == b'T' => Kind::Text,
        _ => return Fix::Keep,
    };
    if data.is_empty() {
        return Fix::Keep;
    }
    let mut out = Vec::with_capacity(data.len() + 4);
    if data[0] > 3 {
        // No encoding byte: the content starts straight away.
        match kind {
            Kind::Text => {
                out.push(0);
                out.extend_from_slice(data);
            }
            Kind::DescUrl => {
                out.extend_from_slice(&[0, 0]); // Latin-1, empty description
                out.extend_from_slice(data);
            }
            _ => return Fix::Drop,
        }
        return Fix::Replace(out);
    }
    let enc = data[0];
    let rest = &data[1..];
    out.push(enc);
    match kind {
        Kind::Text => out.extend(fix_text_values(enc, rest)),
        Kind::DescText | Kind::DescUrl | Kind::LangDescText => {
            let mut rest = rest;
            if let Kind::LangDescText = kind {
                if rest.len() < 3 {
                    return Fix::Drop;
                }
                out.extend_from_slice(&rest[..3]);
                rest = &rest[3..];
            }
            let (desc, after) = split_terminated(enc, rest);
            out.extend(fix_utf16_bom(enc, desc));
            out.extend_from_slice(terminator(enc));
            match kind {
                Kind::DescUrl => out.extend_from_slice(after), // URL is always Latin-1
                _ => out.extend(fix_text_values(enc, after)),
            }
        }
    }
    if out == data {
        Fix::Keep
    } else {
        Fix::Replace(out)
    }
}

fn terminator(enc: u8) -> &'static [u8] {
    if enc == 1 || enc == 2 {
        &[0, 0]
    } else {
        &[0]
    }
}

/// Split at the first terminator (2-byte aligned for UTF-16).  The second part
/// excludes the terminator; with no terminator everything is the first part.
fn split_terminated(enc: u8, s: &[u8]) -> (&[u8], &[u8]) {
    if enc == 1 || enc == 2 {
        let mut i = 0;
        while i + 1 < s.len() {
            if s[i] == 0 && s[i + 1] == 0 {
                return (&s[..i], &s[i + 2..]);
            }
            i += 2;
        }
        (s, &[])
    } else {
        match s.iter().position(|b| *b == 0) {
            Some(i) => (&s[..i], &s[i + 1..]),
            None => (s, &[]),
        }
    }
}

/// A UTF-16-with-BOM string must start with FF FE or FE FF.  Leading NUL code
/// units are dropped; if there is still no BOM, little-endian is assumed.
fn fix_utf16_bom(enc: u8, s: &[u8]) -> Vec<u8> {
    if enc != 1 || s.is_empty() {
        return s.to_vec();
    }
    let mut start = 0;
    while start + 1 < s.len() && s[start] == 0 && s[start + 1] == 0 {
        start += 2;
    }
    let s = &s[start..];
    if s.is_empty() || s.starts_with(&[0xFF, 0xFE]) || s.starts_with(&[0xFE, 0xFF]) {
        return s.to_vec();
    }
    [&[0xFF, 0xFE][..], s].concat()
}

/// Fix every value of a (possibly multi-value) UTF-16 text field.  Empty values
/// created by stray NULs are removed; a trailing terminator is preserved.
fn fix_text_values(enc: u8, s: &[u8]) -> Vec<u8> {
    if enc != 1 {
        return s.to_vec();
    }
    let mut values = Vec::new();
    let mut rest = s;
    let mut trailing = false;
    while !rest.is_empty() {
        let (v, after) = split_terminated(enc, rest);
        let had_term = v.len() < rest.len();
        if !v.is_empty() {
            values.push(fix_utf16_bom(enc, v));
        }
        trailing = had_term && after.is_empty();
        rest = after;
    }
    let mut out = Vec::with_capacity(s.len() + 2);
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(&[0, 0]);
        }
        out.extend_from_slice(v);
    }
    if trailing && !values.is_empty() {
        out.extend_from_slice(&[0, 0]);
    }
    out
}

fn synchsafe(b: &[u8]) -> u32 {
    ((b[0] as u32 & 0x7f) << 21) | ((b[1] as u32 & 0x7f) << 14) | ((b[2] as u32 & 0x7f) << 7) | (b[3] as u32 & 0x7f)
}

/// Read a file into memory with its leading ID3v2 tag repaired (see
/// [`repair_id3v2_frames`]).  `None` when there was nothing to repair.
pub fn read_with_repaired_id3v2(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let mut buf = std::fs::read(path)?;
    let end = first_tag_len(&buf);
    if end == 0 || repair_id3v2_frames(&mut buf[..end]) == 0 {
        return Ok(None);
    }
    Ok(Some(buf))
}

/// Repair the leading ID3v2 tag of a file on disk, rewriting only the tag's
/// own bytes (same length, so the audio is not touched).  Returns how many
/// frames were fixed; 0 means the file was not written.
pub fn repair_id3v2_in_place(path: &Path) -> io::Result<usize> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut header = [0u8; 10];
    if f.read_exact(&mut header).is_err() || &header[0..3] != b"ID3" || header[6..10].iter().any(|b| b & 0x80 != 0) {
        return Ok(0);
    }
    let len = 10 + synchsafe(&header[6..10]) as usize;
    let mut tag = header.to_vec();
    tag.resize(len, 0);
    f.read_exact(&mut tag[10..])?;
    let n = repair_id3v2_frames(&mut tag);
    if n > 0 {
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&tag)?;
        f.flush()?;
    }
    Ok(n)
}

fn first_tag_len(buf: &[u8]) -> usize {
    if buf.len() < 10 || &buf[0..3] != b"ID3" || buf[6..10].iter().any(|b| b & 0x80 != 0) {
        return 0;
    }
    (10 + synchsafe(&buf[6..10]) as usize).min(buf.len())
}

/// Test helpers shared by the decoder crates.
#[doc(hidden)]
pub mod test_support {
    /// An ID3v2.3 tag whose `WXXX` frame lacks the text-encoding byte (the URL
    /// text starts straight away), exactly as found in some ripped albums.
    /// Symphonia rejects it with "invalid WXXX URL description encoding".
    pub fn malformed_wxxx_tag() -> Vec<u8> {
        let mut frames = Vec::new();
        let url = b"midgards-eldar.blogspot.com";
        frames.extend_from_slice(b"WXXX");
        frames.extend_from_slice(&(url.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend_from_slice(url);
        let title = b"\x00Tone";
        frames.extend_from_slice(b"TIT2");
        frames.extend_from_slice(&(title.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend_from_slice(title);
        frames.extend(std::iter::repeat(0u8).take(64)); // padding
        let size = frames.len() as u32;
        let mut tag = b"ID3\x03\x00\x00".to_vec();
        tag.extend_from_slice(&[(size >> 21) as u8 & 0x7f, (size >> 14) as u8 & 0x7f, (size >> 7) as u8 & 0x7f, size as u8 & 0x7f]);
        tag.extend(frames);
        tag
    }

    /// A short MP3 (2 s, 440 Hz) with no tags, from `testdata/`.
    pub fn plain_mp3() -> Vec<u8> {
        include_bytes!("../../../testdata/tone_noid3.mp3").to_vec()
    }

    /// [`plain_mp3`] behind [`malformed_wxxx_tag`].
    pub fn mp3_with_malformed_wxxx() -> Vec<u8> {
        let mut v = malformed_wxxx_tag();
        v.extend(plain_mp3());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tag(size: u32, version: u8, flags: u8) -> Vec<u8> {
        let mut v = b"ID3".to_vec();
        v.push(version);
        v.push(0);
        v.push(flags);
        v.extend_from_slice(&[(size >> 21) as u8 & 0x7f, (size >> 14) as u8 & 0x7f, (size >> 7) as u8 & 0x7f, size as u8 & 0x7f]);
        v.extend(std::iter::repeat(0xAA).take(size as usize));
        v
    }

    #[test]
    fn measures_tags() {
        assert_eq!(id3v2_prefix_len(&mut Cursor::new(b"\xFF\xFBabc".to_vec())).unwrap(), 0);
        let mut data = tag(300, 3, 0);
        data.extend_from_slice(b"\xFF\xFBaudio");
        assert_eq!(id3v2_prefix_len(&mut Cursor::new(data.clone())).unwrap(), 310);
        // two tags back to back
        let mut two = tag(5, 3, 0);
        two.extend(tag(7, 4, 0));
        two.extend_from_slice(b"x");
        assert_eq!(id3v2_prefix_len(&mut Cursor::new(two)).unwrap(), 15 + 17);
        // v2.4 footer adds 10
        let mut foot = tag(4, 4, 0x10);
        foot.extend([0u8; 10]);
        assert_eq!(id3v2_prefix_len(&mut Cursor::new(foot)).unwrap(), 24);
    }

    #[test]
    fn repairs_wxxx_without_encoding_byte() {
        let mut t = test_support::malformed_wxxx_tag();
        let before = t.len();
        assert_eq!(repair_id3v2_frames(&mut t), 1);
        assert_eq!(t.len(), before);
        // WXXX now: encoding 0, empty description, then the URL
        assert_eq!(&t[10..14], b"WXXX");
        let n = u32::from_be_bytes([t[14], t[15], t[16], t[17]]) as usize;
        assert_eq!(&t[20..22], &[0, 0]);
        assert_eq!(&t[22..20 + n], b"midgards-eldar.blogspot.com");
        // The following frame survived intact
        assert_eq!(&t[20 + n..24 + n], b"TIT2");
        // Idempotent
        assert_eq!(repair_id3v2_frames(&mut t), 0);
    }

    #[test]
    fn drops_broken_frame_when_there_is_no_padding() {
        let mut t = test_support::malformed_wxxx_tag();
        // Remove the padding and fix up the header size
        let body_len = t.len() - 10 - 64;
        t.truncate(10 + body_len);
        let s = body_len as u32;
        t[6..10].copy_from_slice(&[(s >> 21) as u8 & 0x7f, (s >> 14) as u8 & 0x7f, (s >> 7) as u8 & 0x7f, s as u8 & 0x7f]);
        assert_eq!(repair_id3v2_frames(&mut t), 1);
        assert_eq!(&t[10..14], b"TIT2");
    }

    #[test]
    fn strips_stray_nuls_before_a_utf16_bom() {
        // The COMM frame found in a real file: UTF-16, "eng", empty description
        // with BOM, then six zero bytes before the text's BOM.
        let mut data = vec![1u8];
        data.extend_from_slice(b"eng");
        data.extend_from_slice(&[0xFF, 0xFE, 0, 0]);
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0xFF, 0xFE]);
        for c in "[RECO2019]".encode_utf16() {
            data.extend_from_slice(&c.to_le_bytes());
        }
        data.extend_from_slice(&[0, 0]);
        let Fix::Replace(fixed) = sanitize_frame(b"COMM", &data) else { panic!("not repaired") };
        let mut expect = vec![1u8];
        expect.extend_from_slice(b"eng");
        expect.extend_from_slice(&[0xFF, 0xFE, 0, 0, 0xFF, 0xFE]);
        for c in "[RECO2019]".encode_utf16() {
            expect.extend_from_slice(&c.to_le_bytes());
        }
        expect.extend_from_slice(&[0, 0]);
        assert_eq!(fixed, expect);
        // Well-formed frames are left alone
        assert!(matches!(sanitize_frame(b"COMM", &expect), Fix::Keep));
        let good_text = [&[1u8, 0xFF, 0xFE][..], &"Hi".encode_utf16().flat_map(|c| c.to_le_bytes()).collect::<Vec<_>>()].concat();
        assert!(matches!(sanitize_frame(b"TIT2", &good_text), Fix::Keep));
        assert!(matches!(sanitize_frame(b"APIC", &[9, 9, 9]), Fix::Keep));
        // UTF-16 text with no BOM at all gets one (little-endian assumed)
        let Fix::Replace(t) = sanitize_frame(b"TIT2", &[1, b'H', 0, b'i', 0]) else { panic!() };
        assert_eq!(t, vec![1, 0xFF, 0xFE, b'H', 0, b'i', 0]);
    }

    #[test]
    fn leaves_good_and_unsynchronised_tags_alone() {
        let mut good = tag(20, 3, 0);
        let copy = good.clone();
        assert_eq!(repair_id3v2_frames(&mut good), 0);
        assert_eq!(good, copy);
        let mut unsync = test_support::malformed_wxxx_tag();
        unsync[5] |= 0x80;
        assert_eq!(repair_id3v2_frames(&mut unsync), 0);
    }

    #[test]
    fn skip_reader_reads_and_seeks() {
        let mut data = tag(20, 3, 0);
        data.extend_from_slice(b"0123456789");
        let n = id3v2_prefix_len(&mut Cursor::new(data.clone())).unwrap();
        let mut r = SkipPrefix::new(Cursor::new(data), n).unwrap();
        assert_eq!(r.len(), 10);
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        assert_eq!(s, "0123456789");
        assert_eq!(r.seek(SeekFrom::Start(3)).unwrap(), 3);
        let mut b = [0u8; 2];
        r.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"34");
        assert_eq!(r.seek(SeekFrom::End(-1)).unwrap(), 9);
        assert_eq!(r.seek(SeekFrom::Current(-4)).unwrap(), 5);
        assert!(r.seek(SeekFrom::Current(-6)).is_err());
    }
}
