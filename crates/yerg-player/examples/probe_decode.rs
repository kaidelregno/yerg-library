//! Diagnostic: decode files the way playback does (converted to a device
//! format) and time it.
//!   cargo run --release -p yerg-player --example probe_decode -- <files...>
use std::time::{Duration, Instant};

use rodio::Source;
use yerg_player::decode::OutputFormat;

fn main() {
    let fmt = OutputFormat { channels: 2, sample_rate: 48_000 };
    for arg in std::env::args().skip(1) {
        let path = std::path::Path::new(&arg);
        println!("== {}", path.file_name().unwrap().to_string_lossy());
        let t = Instant::now();
        let (mut src, dur) = match yerg_player::open_source_for(path, Some(fmt)) {
            Ok(v) => v,
            Err(e) => {
                println!("   open failed after {:?}: {e:#}", t.elapsed());
                continue;
            }
        };
        println!("   open {:?}, duration {:?}, {} Hz x{}", t.elapsed(), dur, src.sample_rate(), src.channels());
        let t = Instant::now();
        let got = src.by_ref().take(48_000 * 2 * 10).count();
        println!("   first 10 s: {got} samples in {:?}", t.elapsed());
        let seek_to = dur.map(|d| d / 2).unwrap_or(Duration::from_secs(30));
        let t = Instant::now();
        let r = src.try_seek(seek_to);
        println!("   seek to half: {:?} in {:?}", r.map_err(|e| e.to_string()), t.elapsed());
        let t = Instant::now();
        let rest = src.count();
        let expect = dur.map(|d| (d - seek_to).as_secs_f64() * 96_000.0);
        println!("   rest: {rest} samples (expected {:?}) in {:?}", expect.map(|e| e.round() as u64), t.elapsed());
    }
}
