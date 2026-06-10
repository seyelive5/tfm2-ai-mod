//! tfm2_pool_reader — read-only ModExtension for the draft recommender overlay.
//!
//! Two passive jobs (changes NOTHING in-game):
//!   1) Export the game's REAL champion pool — `db.available_champions`, which includes
//!      custom-pool settings — to C:\tmp\tfm2-overlay\pool.txt. The launcher prefers this
//!      over its appearance-based heuristic, so the overlay shows your EXACT pool, live.
//!   2) One-time, auto-launch the overlay injector (inject-overlay.exe) when you enter a
//!      game. So enabling this mod = the overlay runs automatically — no manual launcher.

use mod_api::*;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static LAST: Mutex<String> = Mutex::new(String::new());
static SPAWNED: AtomicBool = AtomicBool::new(false);
const OUT: &str = "C:\\tmp\\tfm2-overlay\\pool.txt";
const LAUNCHER: &str = "C:\\tmp\\tfm2-overlay\\inject-overlay.exe";

struct PoolReader;

impl ModExtension for PoolReader {
    fn pre_update(&self, scene: &mut Scene, _ui: &mut GameUI, _a: &mut Assets, _dt: f32) {
        let Scene::InGame { data } = scene else {
            return;
        };

        // 1) export the real available-champion pool (write only when it changes)
        let db = data.db();
        let mut ids: Vec<&str> = db.available_champions.iter().map(|s| s.as_str()).collect();
        ids.sort_unstable();
        let body = ids.join(",");
        if !body.is_empty() {
            let changed = if let Ok(mut last) = LAST.lock() {
                if *last != body {
                    *last = body.clone();
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if changed {
                let tmp = "C:\\tmp\\tfm2-overlay\\pool.txt.tmp";
                let mut ok = false;
                if let Ok(mut f) = std::fs::File::create(tmp) {
                    if f.write_all(body.as_bytes()).is_ok() {
                        ok = true;
                    }
                }
                if ok {
                    let _ = std::fs::rename(tmp, OUT);
                } else {
                    let _ = std::fs::remove_file(tmp);
                }
            }
        }

        // 2) one-time: auto-launch the overlay injector (it self-guards against doubles)
        if !SPAWNED.swap(true, Ordering::Relaxed) {
            let _ = std::process::Command::new(LAUNCHER).spawn();
        }
    }
}

fn init(_c: &GameCtx) -> ModRegistration {
    let mut r = ModRegistration::new("tfm2_pool_reader");
    r.set_extension(PoolReader);
    r
}

declare_mod!(init);
