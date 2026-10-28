//! Lyrics: plain text or LRC (time-stamped) parsing, sidecar discovery.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    /// Milliseconds from the start of the track, when the lyrics are synced.
    pub time_ms: Option<i64>,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
    /// True when at least one line carries a timestamp.
    pub synced: bool,
    /// LRC metadata such as `[ar:Artist]`, `[ti:Title]`, `[offset:+300]`.
    pub meta: Vec<(String, String)>,
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.text.trim().is_empty())
    }

    /// Index of the line that should be highlighted at `pos_ms`.
    pub fn current_line(&self, pos_ms: i64) -> Option<usize> {
        if !self.synced {
            return None;
        }
        let pos = pos_ms + self.offset_ms();
        let mut current = None;
        for (i, line) in self.lines.iter().enumerate() {
            match line.time_ms {
                Some(t) if t <= pos => current = Some(i),
                Some(_) => break,
                None => {}
            }
        }
        current
    }

    pub fn offset_ms(&self) -> i64 {
        self.meta
            .iter()
            .find(|(k, _)| k == "offset")
            .and_then(|(_, v)| v.trim().parse::<i64>().ok())
            .unwrap_or(0)
    }

    pub fn plain_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Parse LRC or plain text.  Lines with several timestamps are expanded.
pub fn parse(text: &str) -> Lyrics {
    let mut out = Lyrics::default();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let mut rest = line;
        let mut times = Vec::new();
        loop {
            let trimmed = rest.trim_start();
            if !trimmed.starts_with('[') {
                rest = trimmed;
                break;
            }
            let Some(close) = trimmed.find(']') else {
                rest = trimmed;
                break;
            };
            let inner = &trimmed[1..close];
            if let Some(ms) = parse_timestamp(inner) {
                times.push(ms);
                rest = &trimmed[close + 1..];
            } else if let Some((k, v)) = inner.split_once(':') {
                if times.is_empty() && !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic()) {
                    out.meta.push((k.to_ascii_lowercase(), v.to_string()));
                    rest = "";
                    break;
                }
                rest = trimmed;
                break;
            } else {
                rest = trimmed;
                break;
            }
        }
        if times.is_empty() {
            if rest.is_empty() && line.trim().starts_with('[') {
                continue; // metadata-only line
            }
            out.lines.push(LyricLine {
                time_ms: None,
                text: rest.trim().to_string(),
            });
        } else {
            out.synced = true;
            for t in times {
                out.lines.push(LyricLine {
                    time_ms: Some(t),
                    text: rest.trim().to_string(),
                });
            }
        }
    }
    if out.synced {
        out.lines.sort_by_key(|l| l.time_ms.unwrap_or(i64::MAX));
    }
    // Trim trailing blank lines
    while out
        .lines
        .last()
        .map(|l| l.text.trim().is_empty())
        .unwrap_or(false)
    {
        out.lines.pop();
    }
    out
}

/// `mm:ss`, `mm:ss.xx`, `mm:ss.xxx`, `hh:mm:ss.xx`
fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || !s.chars().next()?.is_ascii_digit() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut total_ms: i64 = 0;
    for (i, p) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        if last {
            let (sec, frac) = p.split_once(['.', ',']).unwrap_or((p, ""));
            let secs: i64 = sec.parse().ok()?;
            let frac_ms = if frac.is_empty() {
                0
            } else {
                let digits: String = frac.chars().take(3).collect();
                let v: i64 = digits.parse().ok()?;
                match digits.len() {
                    1 => v * 100,
                    2 => v * 10,
                    _ => v,
                }
            };
            total_ms = total_ms * 60 + secs;
            total_ms = total_ms * 1000 + frac_ms;
        } else {
            let v: i64 = p.parse().ok()?;
            total_ms = total_ms * 60 + v;
        }
    }
    Some(total_ms)
}

/// `.lrc` (preferred) or `.txt` next to the audio file.
pub fn find_sidecar(audio_path: &Path) -> Option<PathBuf> {
    for ext in ["lrc", "LRC", "txt"] {
        let p = audio_path.with_extension(ext);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub fn read_sidecar(audio_path: &Path) -> Option<String> {
    let p = find_sidecar(audio_path)?;
    let bytes = std::fs::read(p).ok()?;
    Some(String::from_utf8_lossy(&bytes).to_string())
}

/// Write lyrics to a sidecar `.lrc` file next to the audio.
pub fn write_sidecar(audio_path: &Path, text: &str) -> std::io::Result<PathBuf> {
    let p = audio_path.with_extension("lrc");
    std::fs::write(&p, text)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let l = parse("[ar:Nujabes]\n[ti:Feather]\n[00:12.50]first\n[00:20.000][00:40.0]twice\nplain tail\n");
        assert!(l.synced);
        assert_eq!(l.meta.len(), 2);
        assert_eq!(l.lines.len(), 4);
        assert_eq!(l.lines[0].time_ms, Some(12_500));
        assert_eq!(l.lines[1].time_ms, Some(20_000));
        assert_eq!(l.lines[2].time_ms, Some(40_000));
        assert_eq!(l.lines[3].time_ms, None);
        assert_eq!(l.current_line(25_000), Some(1));
        assert_eq!(l.current_line(1_000), None);
    }

    #[test]
    fn parses_plain() {
        let l = parse("line one\nline two\n\n");
        assert!(!l.synced);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.plain_text(), "line one\nline two");
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp("01:02.3"), Some(62_300));
        assert_eq!(parse_timestamp("1:02:03.004"), Some(3_723_004));
        assert_eq!(parse_timestamp("ar"), None);
    }
}
