//! Minimal file logger. Normal operation logs only state changes (provider failures and
//! recoveries, hotkey registration, game start/stop), never individual sensor readings.
//! The file is rotated once at 1 MB so it cannot grow without bound.
//! Set `FREEHM_LOG=debug` for verbose output.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_BYTES: u64 = 1024 * 1024;

struct Logger {
    path: PathBuf,
    file: Mutex<Option<File>>,
    level: log::LevelFilter,
    also_stderr: bool,
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.level && (m.target().starts_with("freehm") || m.level() <= log::Level::Warn)
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let line = format!("{} {:5} [{}] {}\n", fmt_utc(secs), r.level(), r.target(), r.args());
        if self.also_stderr {
            eprint!("{line}");
        }
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().and_then(|f| f.metadata().ok()).is_some_and(|m| m.len() > MAX_BYTES) {
            *guard = None;
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if guard.is_none() {
            *guard = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Some(f) = self.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = f.flush();
        }
    }
}

/// ISO-8601 UTC timestamp without pulling in a date crate.
fn fmt_utc(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn init(path: &Path, also_stderr: bool) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let level = match std::env::var("FREEHM_LOG").ok().as_deref() {
        Some("trace") => log::LevelFilter::Trace,
        Some("debug") => log::LevelFilter::Debug,
        Some("warn") => log::LevelFilter::Warn,
        _ => log::LevelFilter::Info,
    };
    let logger = Logger { path: path.to_path_buf(), file: Mutex::new(None), level, also_stderr };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_formatting() {
        assert_eq!(super::fmt_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::fmt_utc(1_792_454_400), "2026-10-20T00:00:00Z");
        assert_eq!(super::fmt_utc(1_709_211_909), "2024-02-29T13:05:09Z");
    }
}
