//! `LISTENS.LOG`: the listen log the firmware appends to.
//!
//! One record per line, ASCII, fields separated by single spaces:
//!
//! ```text
//! L <track_id hex8> <started_at unix seconds, 0 if the clock is unset> <played_ms> [<uptime_ms>]
//! # free-form comment (the firmware writes "# boot <unix|0>" at power-up)
//! ```
//!
//! The firmware logs every track that stops, however briefly it played; the
//! importer applies the half-track / four-minute rule itself.  Lines that do
//! not parse are skipped, so a torn write at power loss costs one record.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceListen {
    pub track_id: u32,
    /// `None` when the device clock was not set.
    pub started_at: Option<i64>,
    pub played_ms: i64,
    pub uptime_ms: Option<i64>,
    pub line_no: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedLog {
    pub listens: Vec<DeviceListen>,
    pub boots: usize,
    pub warnings: Vec<String>,
}

/// Earliest plausible timestamp (2020-01-01); anything older is treated as "clock unset".
pub const MIN_VALID_TIMESTAMP: i64 = 1_577_836_800;

pub fn parse(text: &str) -> ParsedLog {
    let mut out = ParsedLog::default();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            if rest.trim_start().starts_with("boot") {
                out.boots += 1;
            }
            continue;
        }
        let mut parts = line.split_ascii_whitespace();
        if parts.next() != Some("L") {
            out.warnings.push(format!("line {}: unknown record", i + 1));
            continue;
        }
        let id = parts.next().and_then(|s| u32::from_str_radix(s, 16).ok());
        let ts = parts.next().and_then(|s| s.parse::<i64>().ok());
        let played = parts.next().and_then(|s| s.parse::<i64>().ok());
        let uptime = parts.next().and_then(|s| s.parse::<i64>().ok());
        match (id, ts, played) {
            (Some(track_id), Some(ts), Some(played_ms)) if track_id != 0 => {
                out.listens.push(DeviceListen {
                    track_id,
                    started_at: if ts >= MIN_VALID_TIMESTAMP { Some(ts) } else { None },
                    played_ms: played_ms.max(0),
                    uptime_ms: uptime,
                    line_no: i + 1,
                });
            }
            _ => out.warnings.push(format!("line {}: malformed record", i + 1)),
        }
    }
    out
}

/// Fill in timestamps for listens recorded without a clock.
///
/// Runs of unknown-time listens are packed backwards from the next known
/// timestamp (or from `import_time` for the final run), each separated by the
/// time it actually played, so relative order and spacing survive.  Returns
/// `(started_at, estimated)` per listen in log order.
pub fn assign_timestamps(listens: &[DeviceListen], import_time: i64) -> Vec<(i64, bool)> {
    let mut out: Vec<(i64, bool)> = Vec::with_capacity(listens.len());
    let mut i = 0;
    while i < listens.len() {
        if let Some(ts) = listens[i].started_at {
            out.push((ts, false));
            i += 1;
            continue;
        }
        // Collect the run of unknowns
        let run_start = i;
        while i < listens.len() && listens[i].started_at.is_none() {
            i += 1;
        }
        let anchor = listens.get(i).and_then(|l| l.started_at).unwrap_or(import_time);
        let run = &listens[run_start..i];
        // Walk backwards from the anchor, 1 second gap between tracks
        let mut cursor = anchor;
        let mut times = Vec::with_capacity(run.len());
        for l in run.iter().rev() {
            cursor -= l.played_ms / 1000 + 1;
            times.push(cursor);
        }
        times.reverse();
        for t in times {
            out.push((t, true));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records_and_comments() {
        let text = "# boot 0\nL 0000abcd 0 120000 5000\nL deadbeef 1700000000 240000\nX junk\nL 0 0 0\n";
        let p = parse(text);
        assert_eq!(p.boots, 1);
        assert_eq!(p.listens.len(), 2);
        assert_eq!(p.listens[0].track_id, 0xabcd);
        assert_eq!(p.listens[0].started_at, None);
        assert_eq!(p.listens[0].uptime_ms, Some(5000));
        assert_eq!(p.listens[1].started_at, Some(1_700_000_000));
        assert_eq!(p.warnings.len(), 2);
    }

    #[test]
    fn estimates_pack_backwards() {
        let l = |ts: Option<i64>, ms: i64| DeviceListen {
            track_id: 1,
            started_at: ts,
            played_ms: ms,
            uptime_ms: None,
            line_no: 0,
        };
        let listens = vec![l(None, 100_000), l(None, 200_000), l(Some(1000_000), 50_000), l(None, 60_000)];
        let out = assign_timestamps(&listens, 2_000_000);
        assert_eq!(out[2], (1_000_000, false));
        assert_eq!(out[1], (1_000_000 - 201, true));
        assert_eq!(out[0], (1_000_000 - 201 - 101, true));
        assert_eq!(out[3], (2_000_000 - 61, true));
    }
}
