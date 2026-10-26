//! The game's own draft AI as a cold-start prior (v3.0 semantics, review F1).
//! The draft score hook fires for the simulated league drafts; an EMPTY-board
//! ban scan has no context, so its score is the AI's intrinsic valuation of a
//! champ — the engine borrows it only for champs the stats can't rate yet.
//! Keyed by champion NAME via the draft context's own brief table (stable draft
//! ids index `champion_briefs`) — the classic build relied on an id-order guess.
//! The hook may run off the main thread: everything here is behind mutexes /
//! atomics, and the main thread only snapshots.

use crate::diag;
use mod_api_stable::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

/// champ → (score sum, observations) over empty-board ban scans
static PRIOR: Mutex<Option<HashMap<String, (f64, u32)>>> = Mutex::new(None);
/// authored-patch anchor of the last table build: a move drops the averages so
/// pre/post-patch scans never blend (F1). The first build only records it.
static LAST_ANCHOR: Mutex<Option<String>> = Mutex::new(None);
static CALLS: AtomicU64 = AtomicU64::new(0);
static SCANS: AtomicU64 = AtomicU64::new(0);
static UNNAMED: AtomicU64 = AtomicU64::new(0);
static RESETS: AtomicU32 = AtomicU32::new(0);
/// ai_scores.log lines — built only while probe/DEBUG_DUMPS exists
static LOG_BUF: Mutex<String> = Mutex::new(String::new());
static FLUSH_TICK: AtomicU32 = AtomicU32::new(0);

/// Leaving the InGame scene: the next save starts from nothing.
pub(crate) fn reset() {
    if let Ok(mut p) = PRIOR.lock() {
        *p = None;
    }
    if let Ok(mut a) = LAST_ANCHOR.lock() {
        *a = None;
    }
}

pub(crate) fn on_patch_anchor(anchor: Option<&str>) {
    let Ok(mut last) = LAST_ANCHOR.lock() else { return };
    if last.as_deref() != anchor {
        if last.is_some() {
            if let Ok(mut p) = PRIOR.lock() {
                *p = None;
            }
            RESETS.fetch_add(1, Ordering::Relaxed);
        }
        *last = anchor.map(|s| s.to_string());
    }
}

/// (champ → mean empty-board ban score, champs observed). One scan suffices:
/// rescans of the same board return identical scores (deterministic model).
pub(crate) fn snapshot() -> (HashMap<String, f32>, usize) {
    let Ok(p) = PRIOR.lock() else { return (HashMap::new(), 0) };
    let m: HashMap<String, f32> = p
        .as_ref()
        .map(|m| m.iter().filter(|(_, (_, n))| *n >= 1).map(|(c, (s, n))| (c.clone(), (*s / *n as f64) as f32)).collect())
        .unwrap_or_default();
    let n = m.len();
    (m, n)
}

fn observe(phase: char, ctx: &StableDraftContext, cand: usize, score: f32) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    let empty = ctx.ally_picks().is_empty()
        && ctx.enemy_picks().is_empty()
        && ctx.ally_bans().is_empty()
        && ctx.enemy_bans().is_empty();
    if phase == 'B' && empty {
        SCANS.fetch_add(1, Ordering::Relaxed);
        match ctx.champion_name(cand) {
            Some(name) => {
                if let Ok(mut p) = PRIOR.lock() {
                    let e = p.get_or_insert_with(HashMap::new).entry(name.to_string()).or_insert((0.0, 0));
                    e.0 += score as f64;
                    e.1 += 1;
                }
            }
            None => {
                UNNAMED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if diag::debug_dumps() {
        if let Ok(mut b) = LOG_BUF.lock() {
            if b.len() > 2 * 1024 * 1024 {
                return; // runaway cap until the next flush
            }
            let nums = |v: &[usize]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
            b.push_str(&format!(
                "{}\t{}\t{}\t{:.4}\t{}\t{}\tap={} ep={} ab={} eb={} avail_n={} teams={:?}/{:?}\n",
                phase,
                cand,
                ctx.champion_name(cand).unwrap_or("?"),
                score,
                ctx.is_explore() as u8,
                diag::now_ms() / 1000,
                nums(ctx.ally_picks()),
                nums(ctx.enemy_picks()),
                nums(ctx.ally_bans()),
                nums(ctx.enemy_bans()),
                ctx.available_champions().len(),
                ctx.ally_team(),
                ctx.enemy_team(),
            ));
        }
    }
}

pub(crate) struct Hook;

impl StableDraftHook for Hook {
    fn id(&self) -> String {
        "tfm2_db_export:observe_scores".to_string()
    }
    fn score_ban(&self, ctx: &StableDraftContext<'_>, candidate: usize, base_score: f32) -> StableDraftDecision {
        observe('B', ctx, candidate, base_score);
        StableDraftDecision::Pass
    }
    fn score_pick(&self, ctx: &StableDraftContext<'_>, candidate: usize, base_score: f32) -> StableDraftDecision {
        observe('P', ctx, candidate, base_score);
        StableDraftDecision::Pass
    }
}

/// Main thread, once per frame: diag section + the debug log flush (every 600 frames).
pub(crate) fn tick() {
    let keys = PRIOR.lock().ok().and_then(|p| p.as_ref().map(|m| m.len())).unwrap_or(0);
    let anchor = LAST_ANCHOR.lock().ok().and_then(|a| a.clone());
    diag::set(
        "7 prior",
        format!(
            "hook_calls={} empty_board_ban_scans={} champs={} unnamed={} resets={} anchor={:?}",
            CALLS.load(Ordering::Relaxed),
            SCANS.load(Ordering::Relaxed),
            keys,
            UNNAMED.load(Ordering::Relaxed),
            RESETS.load(Ordering::Relaxed),
            anchor
        ),
    );
    if FLUSH_TICK.fetch_add(1, Ordering::Relaxed) % 600 != 0 {
        return;
    }
    let body = match LOG_BUF.lock() {
        Ok(mut b) if !b.is_empty() => std::mem::take(&mut *b),
        _ => return,
    };
    use std::io::Write;
    let base = diag::base();
    let _ = std::fs::create_dir_all(&base);
    let path = format!("{}/ai_scores.log", base);
    if std::fs::metadata(&path).map(|m| m.len() > 8 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, format!("{}/ai_scores.old.log", base));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(body.as_bytes());
    }
}
