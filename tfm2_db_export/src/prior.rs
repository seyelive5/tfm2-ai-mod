//! The game's own draft AI as a cold-start prior (v3.0 semantics, review F1).
//! The draft score hook fires for the simulated league drafts; an EMPTY-board
//! ban scan has no context, so its score is the AI's intrinsic valuation of a
//! champ — the engine borrows it only for champs the stats can't rate yet.
//! Keyed by champion NAME via the draft context's own brief table (stable draft
//! ids index `champion_briefs`) — the classic build relied on an id-order guess.
//! The hook may run off the main thread: everything here is behind mutexes /
//! atomics, and the main thread only snapshots. Every call also feeds foresee
//! (the opponent's likely next move), which is otherwise free of the SDK.

use crate::diag;
use mod_api_stable::*;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
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

/// Whose draft a call scores, seen from the player's team.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    /// the AI drafts FOR the player's team (coach delegation)
    Ally,
    /// the AI drafts AGAINST the player's team (the opponent in the player's match)
    Enemy,
}

fn side_of(my: Option<usize>, ally: Option<usize>, enemy: Option<usize>) -> Option<Side> {
    let my = my?;
    if ally == Some(my) {
        Some(Side::Ally)
    } else if enemy == Some(my) {
        Some(Side::Enemy)
    } else {
        None
    }
}

// Review experiment E1 (docs/research/2026-10-28-advanced-directions.md): does
// the hook fire for the player's own drafts? The ABI-10 team ids say whose draft
// a call scores; counted always — two compares per call.
/// the player's team (db poll), usize::MAX = unknown
static MY_TEAM: AtomicUsize = AtomicUsize::new(usize::MAX);
static WITH_IDS: AtomicU64 = AtomicU64::new(0);
/// [as ally outside banpick, as ally in banpick, as enemy outside, as enemy in]
static MINE: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// the latest such call, with its board, to hold against the screen
static MINE_LAST: Mutex<String> = Mutex::new(String::new());

pub(crate) fn set_my_team(team: Option<usize>) {
    MY_TEAM.store(team.unwrap_or(usize::MAX), Ordering::Relaxed);
}

fn count_mine(phase: char, ctx: &StableDraftContext, cand: usize, score: f32) {
    let (ally, enemy) = (ctx.ally_team(), ctx.enemy_team());
    if ally.is_some() && enemy.is_some() {
        WITH_IDS.fetch_add(1, Ordering::Relaxed);
    }
    let my = Some(MY_TEAM.load(Ordering::Relaxed)).filter(|&t| t != usize::MAX);
    let Some(side) = side_of(my, ally, enemy) else { return };
    let banpick = crate::IN_BANPICK.load(Ordering::Relaxed);
    MINE[(side == Side::Enemy) as usize * 2 + banpick as usize].fetch_add(1, Ordering::Relaxed);
    if let Ok(mut last) = MINE_LAST.lock() {
        let names = |v: &[usize]| v.iter().map(|&i| ctx.champion_name(i).unwrap_or("?")).collect::<Vec<_>>().join(",");
        *last = format!(
            "{} {:?} banpick={} t={}s cand={} {:.3} | ally picks[{}] bans[{}] | enemy picks[{}] bans[{}]",
            phase,
            side,
            banpick,
            diag::now_ms() / 1000,
            ctx.champion_name(cand).unwrap_or("?"),
            score,
            names(ctx.ally_picks()),
            names(ctx.ally_bans()),
            names(ctx.enemy_picks()),
            names(ctx.enemy_bans()),
        );
    }
}

/// Leaving the InGame scene: the next save starts from nothing.
pub(crate) fn reset() {
    if let Ok(mut p) = PRIOR.lock() {
        *p = None;
    }
    if let Ok(mut a) = LAST_ANCHOR.lock() {
        *a = None;
    }
    set_my_team(None);
    if let Ok(mut l) = MINE_LAST.lock() {
        l.clear();
    }
}

pub(crate) fn on_patch_anchor(anchor: Option<&str>) {
    let Ok(mut last) = LAST_ANCHOR.lock() else { return };
    if last.as_deref() != anchor {
        if last.is_some() {
            if let Ok(mut p) = PRIOR.lock() {
                *p = None;
            }
            crate::foresee::reset();
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

/// foresee's input: the call keyed by its decision (phase + both teams + the
/// board it scores) and its draft (the team pair). Calls without team ids
/// (≈1% in E1) cannot be told apart by draft and are left out.
fn feed_foresee(phase: char, ctx: &StableDraftContext, cand: usize, score: f32) {
    let (Some(ally), Some(enemy)) = (ctx.ally_team(), ctx.enemy_team()) else { return };
    let Some(name) = ctx.champion_name(cand) else { return };
    let lists = [ctx.ally_picks(), ctx.enemy_picks(), ctx.ally_bans(), ctx.enemy_bans()];
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (phase, ally, enemy, lists).hash(&mut h);
    let my = Some(MY_TEAM.load(Ordering::Relaxed)).filter(|&t| t != usize::MAX);
    let call = crate::foresee::Call {
        phase,
        key: h.finish(),
        pair: (ally.min(enemy), ally.max(enemy)),
        size: lists.iter().map(|l| l.len()).sum(),
        vs_me: side_of(my, Some(ally), Some(enemy)) == Some(Side::Enemy),
        cand: name,
        score,
    };
    crate::foresee::observe(&call, || {
        lists.iter().flat_map(|l| l.iter()).map(|&i| ctx.champion_name(i).unwrap_or("?").to_string()).collect()
    });
}

fn observe(phase: char, ctx: &StableDraftContext, cand: usize, score: f32) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    count_mine(phase, ctx, cand, score);
    feed_foresee(phase, ctx, cand, score);
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
    let scans = SCANS.load(Ordering::Relaxed);
    let mine = MINE.each_ref().map(|c| c.load(Ordering::Relaxed));
    let my = MY_TEAM.load(Ordering::Relaxed);
    let last = MINE_LAST.lock().map(|l| l.clone()).unwrap_or_default();
    diag::set(
        "7 prior",
        format!(
            "hook_calls={} empty_board_ban_scans={} champs={} unnamed={} resets={} anchor={:?}\n\
             E1 team_ids={} my_team={} mine: as_ally={} (in banpick {}) as_enemy={} (in banpick {})\n\
             E1 last_mine={}\n\
             E3 drafts_seen≈{}",
            CALLS.load(Ordering::Relaxed),
            scans,
            keys,
            UNNAMED.load(Ordering::Relaxed),
            RESETS.load(Ordering::Relaxed),
            anchor,
            WITH_IDS.load(Ordering::Relaxed),
            if my == usize::MAX { "-".to_string() } else { my.to_string() },
            mine[0] + mine[1],
            mine[1],
            mine[2] + mine[3],
            mine[3],
            if last.is_empty() { "-" } else { &last },
            // a draft's first ban scans every candidate on an empty board; a rescan
            // of that board counts again, so this is an upper bound
            if keys > 0 { (scans as usize / keys).to_string() } else { "-".to_string() },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_scoring_for_my_team_is_mine_as_ally() {
        assert_eq!(side_of(Some(8), Some(8), Some(7)), Some(Side::Ally));
    }

    #[test]
    fn a_call_scoring_against_my_team_is_mine_as_enemy() {
        assert_eq!(side_of(Some(8), Some(7), Some(8)), Some(Side::Enemy));
    }

    #[test]
    fn other_drafts_and_unknown_ids_are_not_mine() {
        assert_eq!(side_of(Some(8), Some(3), Some(7)), None);
        assert_eq!(side_of(Some(8), None, None), None);
        assert_eq!(side_of(None, Some(8), Some(7)), None);
    }
}
