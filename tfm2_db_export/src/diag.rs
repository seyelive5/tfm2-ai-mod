//! In-game verification output (ROADMAP §5) under %LOCALAPPDATA%/tfm2-overlay:
//!   diag.txt       — current state, one section per subsystem; rewritten at most
//!                    once a second, only when a section changed.
//!   board_log.txt  — append-only, one line per accepted board change (rotated at 2 MB).
//! Both are small and always on — they are how a build that has never run in-game
//! gets judged from one play session. Heavy dumps stay behind probe/DEBUG_DUMPS.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Shared directory = %LOCALAPPDATA%/tfm2-overlay (falls back to C:/tmp/tfm2-overlay).
pub(crate) fn base() -> String {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:/tmp".to_string());
    format!("{}/tfm2-overlay", local.replace('\\', "/"))
}

pub(crate) fn atomic_write(path: &str, body: &str) -> bool {
    let tmp = format!("{}.tmp", path);
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        true
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

pub(crate) fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

static SECTIONS: Mutex<BTreeMap<&'static str, String>> = Mutex::new(BTreeMap::new());
static DIRTY: AtomicBool = AtomicBool::new(false);
static LAST_FLUSH: Mutex<Option<Instant>> = Mutex::new(None);

/// Replace one section of diag.txt (keys sort the file: "1 mod", "2 scene", …).
pub(crate) fn set(key: &'static str, body: String) {
    if let Ok(mut s) = SECTIONS.lock() {
        if s.get(key) != Some(&body) {
            s.insert(key, body);
            DIRTY.store(true, Ordering::Relaxed);
        }
    }
}

/// Called once per frame; writes at most once a second.
pub(crate) fn flush() {
    if !DIRTY.load(Ordering::Relaxed) {
        return;
    }
    {
        let Ok(mut last) = LAST_FLUSH.lock() else { return };
        if last.map_or(false, |t| t.elapsed().as_millis() < 1000) {
            return;
        }
        *last = Some(Instant::now());
    }
    DIRTY.store(false, Ordering::Relaxed);
    let body = match SECTIONS.lock() {
        Ok(s) => s
            .iter()
            .map(|(k, v)| format!("[{}]\n{}\n", k, v.trim_end()))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => return,
    };
    let _ = std::fs::create_dir_all(base());
    atomic_write(
        &format!("{}/diag.txt", base()),
        &format!("{} — written_ms={}\n\n{}", crate::MOD_TAG, now_ms(), body),
    );
}

/// One line to board_log.txt (timestamped; rotated to board_log.old.txt at 2 MB).
pub(crate) fn log_line(line: &str) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(base());
    let path = format!("{}/board_log.txt", base());
    if std::fs::metadata(&path).map(|m| m.len() > 2_000_000).unwrap_or(false) {
        let old = format!("{}/board_log.old.txt", base());
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&path, &old);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "ts={} {}", now_ms(), line);
    }
}

static DUMPS_ON: AtomicBool = AtomicBool::new(false);
static DUMPS_CHECKED: Mutex<Option<Instant>> = Mutex::new(None);

/// Re-reads the probe/DEBUG_DUMPS marker every 10 s (main thread, once per frame).
pub(crate) fn refresh_debug_dumps() {
    let Ok(mut at) = DUMPS_CHECKED.lock() else { return };
    if at.map_or(false, |t| t.elapsed().as_secs() < 10) {
        return;
    }
    *at = Some(Instant::now());
    let on = std::path::Path::new(&format!("{}/probe/DEBUG_DUMPS", base())).exists();
    DUMPS_ON.store(on, Ordering::Relaxed);
}

/// Heavy research dumps (stats TSV, AI score log) are written only while the
/// probe/DEBUG_DUMPS marker file exists — the v3.0 rule (REVIEW-FIXES #2).
pub(crate) fn debug_dumps() -> bool {
    DUMPS_ON.load(Ordering::Relaxed)
}
