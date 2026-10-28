//! Small formatting helpers for the UI.

pub fn fmt_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

pub fn fmt_relative(ts: i64) -> String {
    let now = yerg_core::now_unix();
    let d = now - ts;
    if d < 0 {
        return fmt_date(ts);
    }
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=604_799 => format!("{} d ago", d / 86_400),
        _ => chrono::DateTime::from_timestamp(ts, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
    }
}

pub fn stars(rating: i32) -> String {
    let r = rating.clamp(0, 5) as usize;
    format!("{}{}", "★".repeat(r), "☆".repeat(5 - r))
}

pub fn fmt_secs(secs: f64) -> String {
    yerg_core::fmt_duration((secs * 1000.0) as i64)
}
