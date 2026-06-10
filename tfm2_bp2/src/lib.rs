//! tfm2_bp2 — TEMPORARY experiment: can the CLIENT db expose the LIVE banpick board?
//!
//! bp_probe tested the SERVER/command path (send_mod_command → handle_command →
//! server_state.running_matches) and the draft-score-hook — both sealed/sim-noise.
//! It NEVER tested the unexplored CLIENT path: `db.scene` (ClientScene) and
//! `db.matches` (HashMap<MatchType, MatchInfo>, what `ClientData::match_info()` reads).
//!
//! This dumps both (truncated, throttled, capped, on-change) to C:/tmp/bp2/bp2.log so
//! we can SEE whether a banpick board appears and GROWS as you click picks/bans during
//! YOUR champion select. Read-only; no gameplay change. Disable/delete after the test.

use mod_api::*;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

const OUT_DIR: &str = "C:/tmp/bp2";
const OUT: &str = "C:/tmp/bp2/bp2.log";
const MAX_WRITES: u32 = 150;

static FRAME: AtomicU64 = AtomicU64::new(0);
static WRITES: AtomicU32 = AtomicU32::new(0);
static LAST: Mutex<String> = Mutex::new(String::new());

/// char-safe truncation (game data may contain Korean names → can't slice on bytes).
fn clip(s: String, max_chars: usize) -> String {
    if s.chars().count() > max_chars {
        let head: String = s.chars().take(max_chars).collect();
        format!("{}\n…<+{} bytes truncated>", head, s.len())
    } else {
        s
    }
}

struct E;
impl ModExtension for E {
    fn pre_update(&self, scene: &mut Scene, _ui: &mut GameUI, _a: &mut Assets, _dt: f32) {
        let Scene::InGame { data } = scene else {
            return;
        };
        let f = FRAME.fetch_add(1, Ordering::Relaxed);
        if f % 12 != 0 {
            return; // sample ~5×/sec
        }
        if WRITES.load(Ordering::Relaxed) >= MAX_WRITES {
            return; // stop entirely once we've captured enough (also stops the formatting cost)
        }
        let db = data.db();
        // (ClientScene compiler-probe removed — its variants are NOT public → opaque,
        //  cannot read a banpick board from db.scene.)
        // db.matches = HashMap<MatchType, MatchInfo>. MatchInfo is schedule/results
        // (no draft fields seen). DEFINITIVE check: histogram the running_state across
        // ALL matches + FULL-dump any match NOT in Wait/End (= the live/running one,
        // where a draft board would live if anywhere) — no truncation bias this time.
        let mut hist: BTreeMap<String, u32> = BTreeMap::new();
        let mut live = String::new();
        let mut live_n = 0u32;
        for (mt, mi) in db.matches.iter() {
            let s = format!("{:#?}", mi);
            let rs = s
                .split("running_state: ")
                .nth(1)
                .map(|r| {
                    let end = r.find(|c: char| c == '{' || c == ',' || c.is_whitespace()).unwrap_or(r.len());
                    r[..end].to_string()
                })
                .unwrap_or_else(|| "?".into());
            *hist.entry(rs.clone()).or_insert(0) += 1;
            if rs != "Wait" && rs != "End" {
                live_n += 1;
                if live.chars().count() < 30_000 {
                    live.push_str(&format!("\n>> key = {:?}\n{}\n", mt, s));
                }
            }
        }
        let snap = format!(
            "matches={} · non-Wait/End={}\nrunning_state histogram: {:?}\n=== live/running matches (draft board would be HERE) ===\n{}",
            db.matches.len(),
            live_n,
            hist,
            if live.is_empty() { "(none — no active/running match in db.matches)".to_string() } else { live }
        );
        // log only when the snapshot CHANGES, so we capture the board growing as you click
        let mut last = match LAST.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if *last == snap {
            return;
        }
        *last = snap.clone();
        drop(last);
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        let _ = std::fs::create_dir_all(OUT_DIR);
        if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(OUT) {
            let _ = writeln!(fh, "\n################ CHANGE #{} (frame {}) ################\n{}", n, f, snap);
        }
    }
}

fn init(_c: &GameCtx) -> ModRegistration {
    let mut r = ModRegistration::new("tfm2_bp2");
    r.set_extension(E);
    r
}

declare_mod!(init);
