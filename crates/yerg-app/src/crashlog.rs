//! A permanent crash log.
//!
//! * Every panic, on any thread, is appended to `crash.log` in the data folder
//!   with the time, thread, message, source location and a backtrace.  (A
//!   Rust panic exits without a Windows crash report, so without this a
//!   crash looks the same as the window being closed.)
//! * `session.lock` exists while the app runs and is removed on a normal
//!   exit, so the next start can tell the app ended some other way (killed,
//!   power loss, a crash in a driver) even when nothing panicked.
//!
//! The next start shows a notice for either case (see [`take_startup_notice`]).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Keep the log from growing without bound: past this size the oldest half goes.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

static NOTICE: Mutex<Option<String>> = Mutex::new(None);

fn data_dir() -> PathBuf {
    yerg_core::paths::data_dir()
}

pub fn log_path() -> PathBuf {
    data_dir().join("crash.log")
}

fn pending_path() -> PathBuf {
    data_dir().join("crash.pending")
}

fn lock_path() -> PathBuf {
    data_dir().join("session.lock")
}

/// Install the panic hook and work out what to tell the user about the last
/// run.  Call first thing in `main`.
pub fn install() {
    let _ = std::fs::create_dir_all(data_dir());

    let crashed = pending_path().exists();
    let unclean = lock_path().exists();
    let notice = if crashed {
        Some(format!(
            "Yerg Library stopped because of an error last time. The details are in {} (Settings › About).",
            log_path().display()
        ))
    } else if unclean {
        Some("Yerg Library did not shut down normally last time (it may have been ended from outside, or the computer lost power).".to_string())
    } else {
        None
    };
    let _ = std::fs::remove_file(pending_path());
    *NOTICE.lock().unwrap_or_else(|e| e.into_inner()) = notice;
    let _ = std::fs::write(lock_path(), format!("pid {}\n", std::process::id()));

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_panic(info);
        // Still print it as usual (to the console in debug builds).
        default_hook(info);
    }));
}

/// Call after the window has closed normally.
pub fn clean_exit() {
    let _ = std::fs::remove_file(lock_path());
}

/// The notice for the start-up toast, once.
pub fn take_startup_notice() -> Option<String> {
    NOTICE.lock().unwrap_or_else(|e| e.into_inner()).take()
}

pub fn has_log() -> bool {
    log_path().is_file()
}

/// Record an error that stopped the app without a panic (e.g. no window
/// could be created).
pub fn record_error(what: &str) {
    let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z");
    let entry = format!(
        "==== {when} | Yerg Library {} | {} {}\n{what}\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = f.write_all(entry.as_bytes());
    }
}

fn record_panic(info: &std::panic::PanicHookInfo<'_>) {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_string());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown location".to_string());
    let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
    let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z");
    let backtrace = std::backtrace::Backtrace::force_capture();
    let entry = format!(
        "==== {when} | Yerg Library {} | {} {} | thread '{thread}'\n\
         panicked at {location}:\n{message}\n\nbacktrace:\n{backtrace}\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );

    let path = log_path();
    trim_if_large(&path);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(entry.as_bytes());
    }
    // Panics on helper threads (the audio engine recovers from some) are
    // logged too, but only one on the main thread ends the app.
    if thread == "main" {
        let _ = std::fs::write(pending_path(), when.to_string());
    }
}

fn trim_if_large(path: &std::path::Path) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    if let Ok(text) = std::fs::read_to_string(path) {
        // Keep the newer half, starting at an entry boundary.  (Never panic
        // here: this runs inside the panic hook.)
        let mut half = text.len() / 2;
        while !text.is_char_boundary(half) {
            half += 1;
        }
        let start = text[half..].find("==== ").map(|p| half + p).unwrap_or(half);
        let _ = std::fs::write(path, &text[start..]);
    }
}
