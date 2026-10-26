//! In-mod recommender — the FULL pipeline (live stats → Tables → board state →
//! recommendations → display strings), integrated from the retired launcher
//! (inject-overlay.exe) and overlay (draft_overlay.dll) for the v3.0 single-mod
//! architecture: zero executables, zero injection, Workshop-distributable.
//!
//! The engine itself is shared source with the old overlay (single source of
//! truth via #[path]). The data-S/K (synergy/counter) rows the old gen.exe
//! computed are intentionally DROPPED: backtests showed they add ~nothing over
//! winrate; the engine's tag-based theory terms (T rows, baked) remain.

#[path = "../../draft-overlay/src/engine.rs"]
#[allow(dead_code)] // shared source: the retired overlay's surface stays in-tree
pub mod engine;

use engine::{parse_tables_from_str, recommend, recommend_lanes, DraftState, Tables, CHAMPS};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

// ───────────────────────── tables (stats → engine) ─────────────────────────

// Confidence-scaled previous-patch blend (see the retired inject.rs for the
// research notes): current ×1 + previous ×(0.35·(1−min(1, cur_matches/200))).
const PREV_PATCH_WEIGHT: f64 = 0.35;
const CONF_FULL: f64 = 200.0;
// S2-3 (2026-07-05): causal κ sweep on the production champ state — κ20/40/80/150
// → 60.1/60.0/60.1/59.9%: flat. 40 confirmed; adaptive EB κ̂ dropped (nothing to buy).
const KAPPA_CHAMP: f64 = 40.0;
const KAPPA_LANE: f64 = 20.0;
/// per-CHAMP current-patch matches at which its previous-patch blend fades out
/// (the old GLOBAL total/200 gate starved low-pickrate champs of their history).
const CONF_CHAMP: f64 = 40.0;

fn beta_post(w: f64, m: f64, prior_mean: f64, kappa: f64) -> (f64, f64) {
    let pm = prior_mean.clamp(0.001, 0.999);
    let a = w.max(0.0) + pm * kappa;
    let b = (m - w).max(0.0) + (1.0 - pm) * kappa;
    let t = a + b;
    (a / t, (a * b / (t * t * (t + 1.0))).sqrt())
}
// NOTE 2026-06-12: ranking switched from the LOWER CONFIDENCE BOUND to the
// posterior MEAN. Ranking by LCB (mean − 1·sd) double-penalized small samples
// (κ-shrink already handles them): on real data a 72%-winrate champ at 18 games
// scored delta 0.00 while a 65% champ at 77 games scored +0.05 — the "강챔"
// ranking was largely a popularity ranking, and S tier (+6%p) was unreachable.
// The sd now feeds only the low-confidence flag (games-based, engine-side).

// version sort key: single definition in lib.rs — the changes ANCHOR (writer)
// and the stale-row CUTOFF (reader, below) must agree on version ordering.
use crate::vkey;

fn lane_idx(p: &str) -> Option<u8> {
    match p {
        "Top" => Some(0),
        "Jungle" => Some(1),
        "Mid" => Some(2),
        "Bottom" => Some(3),
        "Support" => Some(4),
        _ => None,
    }
}

/// C/CP rows + total matches from the patch-stats TSV (latest patch ×1 +
/// previous ×fading; champs CHANGED this patch skip every row at/below the
/// diff anchor — they restart from neutral).
fn patch_current(patch: &str, changed: &HashSet<String>, chg_prev: Option<&str>) -> (Vec<String>, Vec<String>, u32) {
    let rows: Vec<Vec<&str>> = patch
        .lines()
        .skip(1)
        .map(|l| l.split('\t').collect::<Vec<&str>>())
        .filter(|f| f.len() >= 7)
        .collect();
    if rows.is_empty() {
        return (vec![], vec![], 0);
    }
    let mut vers: Vec<&str> = rows.iter().map(|f| f[0]).collect();
    vers.sort_by(|a, b| vkey(a).cmp(&vkey(b)));
    vers.dedup();
    let latest = *vers.last().unwrap();
    let prev = if vers.len() >= 2 { Some(vers[vers.len() - 2]) } else { None };
    let total: u32 = rows.iter().find(|f| f[0] == latest).and_then(|f| f[1].parse().ok()).unwrap_or(0);
    // per-CHAMP current-patch matches: the prev-patch blend fades per champ (a
    // low-pickrate champ keeps its history even when the league total is high).
    let mut cur_m: BTreeMap<&str, f64> = BTreeMap::new();
    for f in &rows {
        if f[0] == latest {
            *cur_m.entry(f[2]).or_insert(0.0) += f[6].parse().unwrap_or(0.0);
        }
    }
    let mut by: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    let mut byp: BTreeMap<(String, u8), (f64, f64)> = BTreeMap::new();
    for f in &rows {
        if changed.contains(f[2]) {
            let stale = match chg_prev {
                Some(pv) => vkey(f[0]) <= vkey(pv),
                None => Some(f[0]) == prev,
            };
            if stale {
                continue;
            }
        }
        let wgt = if f[0] == latest {
            1.0
        } else if Some(f[0]) == prev {
            let cm = cur_m.get(f[2]).copied().unwrap_or(0.0);
            PREV_PATCH_WEIGHT * (1.0 - (cm / CONF_CHAMP).min(1.0))
        } else {
            continue;
        };
        if wgt <= 0.0 {
            continue;
        }
        let w: f64 = f[5].parse().unwrap_or(0.0);
        let m: f64 = f[6].parse().unwrap_or(0.0);
        let e = by.entry(f[2].to_string()).or_insert((0.0, 0.0));
        e.0 += wgt * w;
        e.1 += wgt * m;
        if let Some(li) = lane_idx(f[3]) {
            let pe = byp.entry((f[2].to_string(), li)).or_insert((0.0, 0.0));
            pe.0 += wgt * w;
            pe.1 += wgt * m;
        }
    }
    // C rows: posterior MEAN − 0.5 (κ-shrunk). See the note above beta_post.
    let c_rows = by
        .iter()
        .map(|(c, (w, m))| {
            let (mean, sd) = beta_post(*w, *m, 0.5, KAPPA_CHAMP);
            // 5th field = posterior sd (honest uncertainty) → engine's tier "?" gate.
            format!("C\t{}\t{:.3}\t{}\t{:.4}", c, mean - 0.5, m.round() as u32, sd)
        })
        .collect();
    let champ_mean: BTreeMap<String, f64> =
        by.iter().map(|(c, (w, m))| (c.clone(), beta_post(*w, *m, 0.5, KAPPA_CHAMP).0)).collect();
    // CP rows: posterior MEAN − 0.5 with the champ's own mean as the prior — a
    // thin lane cell now reads ≈ the champ's strength instead of champ−1sd (the
    // old z·sd artifact made a 1-game lane score BELOW a 0-game lane).
    let cp_rows = byp
        .iter()
        .filter(|(_, (_, m))| *m > 0.0)
        .map(|((c, li), (w, m))| {
            let prior = champ_mean.get(c).copied().unwrap_or(0.5);
            let (mean, sd) = beta_post(*w, *m, prior, KAPPA_LANE);
            format!("CP\t{}\t{}\t{:.3}\t{}\t{:.4}", c, li, mean - 0.5, m.round() as u32, sd)
        })
        .collect();
    // effective evidence: the tables blend ~prev_w×prev matches too — report that,
    // so the engine's theory fade matches the data actually loaded.
    let prev_total: u32 =
        prev.and_then(|pv| rows.iter().find(|f| f[0] == pv)).and_then(|f| f[1].parse().ok()).unwrap_or(0);
    let prev_w_global = PREV_PATCH_WEIGHT * (1.0 - (total as f64 / CONF_FULL).min(1.0));
    let eff = total + (prev_w_global * prev_total as f64).round() as u32;
    (c_rows, cp_rows, eff)
}

/// POS rows (primary + playable lanes). REWORKED champs ('R') use only rows
/// newer than the anchor (their history lies), falling back to all-time.
fn compute_pos(patch: &str, reworked: &HashSet<String>, anchor: Option<&str>) -> Vec<String> {
    let mut by: BTreeMap<String, [u32; 5]> = BTreeMap::new();
    let mut recent: BTreeMap<String, [u32; 5]> = BTreeMap::new();
    for line in patch.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        if let Some(li) = lane_idx(f[3]) {
            let m: u32 = f[6].parse().unwrap_or(0);
            by.entry(f[2].to_string()).or_insert([0; 5])[li as usize] += m;
            if anchor.map(|a| vkey(f[0]) > vkey(a)).unwrap_or(false) {
                recent.entry(f[2].to_string()).or_insert([0; 5])[li as usize] += m;
            }
        }
    }
    by.iter()
        .filter_map(|(c, m_all)| {
            let m = if reworked.contains(c) {
                recent.get(c).filter(|r| r.iter().sum::<u32>() > 0).unwrap_or(m_all)
            } else {
                m_all
            };
            let total: u32 = m.iter().sum();
            if total == 0 {
                return None;
            }
            let primary = (0..5).max_by_key(|&i| m[i]).unwrap() as u8;
            let mut plays: Vec<u8> =
                (0..5).filter(|&i| m[i] as f32 / total as f32 >= 0.20).map(|i| i as u8).collect();
            plays.retain(|&l| l != primary);
            plays.insert(0, primary);
            let list = plays.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(",");
            Some(format!("POS\t{}\t{}\t{}", c, primary, list))
        })
        .collect()
}

/// RP rows (data-driven tank/heal ratios). Reworked champs need ≥5 fresh games
/// or get NO row (old-kit role may be flat wrong).
fn compute_roles(patch: &str, reworked: &HashSet<String>, anchor: Option<&str>) -> Vec<String> {
    let rows: Vec<Vec<&str>> = patch
        .lines()
        .skip(1)
        .map(|l| l.split('\t').collect::<Vec<&str>>())
        .filter(|f| f.len() >= 10)
        .collect();
    if rows.is_empty() {
        return vec![];
    }
    let mut agg: BTreeMap<String, (f64, f64, f64, u32)> = BTreeMap::new();
    let mut recent: BTreeMap<String, (f64, f64, f64, u32)> = BTreeMap::new();
    for f in &rows {
        let add = |e: &mut (f64, f64, f64, u32)| {
            e.0 += f[7].parse().unwrap_or(0.0);
            e.1 += f[8].parse().unwrap_or(0.0);
            e.2 += f[9].parse().unwrap_or(0.0);
            e.3 += f[6].parse().unwrap_or(0u32);
        };
        add(agg.entry(f[2].to_string()).or_insert((0.0, 0.0, 0.0, 0)));
        if anchor.map(|a| vkey(f[0]) > vkey(a)).unwrap_or(false) {
            add(recent.entry(f[2].to_string()).or_insert((0.0, 0.0, 0.0, 0)));
        }
    }
    agg.iter()
        .filter_map(|(c, all)| {
            let (d, t, h, m) = if reworked.contains(c) { *recent.get(c)? } else { *all };
            if m < 5 || d <= 0.0 {
                return None;
            }
            Some(format!("RP\t{}\t{:.2}\t{:.2}", c, t / d, h / d))
        })
        .collect()
}

/// B rows: RECENCY-WEIGHTED banrate per champ (latest patch ×1, previous ×0.5,
/// older ×0.25) — season-wide enough that a fresh patch's handful of games can't
/// swing W_BAN_META, but a champ nerfed into the ground no longer keeps its
/// season-high banrate all year. Changed champs additionally drop rows at/below
/// the diff anchor (same reset as the C rows).
fn compute_banrate(patch: &str, changed: &HashSet<String>, chg_prev: Option<&str>) -> Vec<String> {
    let rows: Vec<Vec<&str>> = patch
        .lines()
        .skip(1)
        .map(|l| l.split('\t').collect::<Vec<&str>>())
        .filter(|f| f.len() >= 7)
        .collect();
    let mut vers: Vec<&str> = rows.iter().map(|f| f[0]).collect();
    vers.sort_by(|a, b| vkey(a).cmp(&vkey(b)));
    vers.dedup();
    let latest = vers.last().copied();
    let prev = if vers.len() >= 2 { Some(vers[vers.len() - 2]) } else { None };
    let vw = |v: &str| {
        if Some(v) == latest {
            1.0
        } else if Some(v) == prev {
            0.5
        } else {
            0.25
        }
    };
    // per-version total (first row of that version) + per-(version,champ) bans
    let mut ver_total: BTreeMap<&str, f64> = BTreeMap::new();
    let mut bans: BTreeMap<(&str, &str), f64> = BTreeMap::new();
    for f in &rows {
        ver_total.entry(f[0]).or_insert_with(|| f[1].parse().unwrap_or(0.0));
        bans.entry((f[0], f[2])).or_insert_with(|| f[4].parse().unwrap_or(0.0));
    }
    let mut num: BTreeMap<&str, f64> = BTreeMap::new();
    let mut den: BTreeMap<&str, f64> = BTreeMap::new();
    for ((v, c), b) in &bans {
        if changed.contains(*c) {
            if let Some(pv) = chg_prev {
                if vkey(v) <= vkey(pv) {
                    continue;
                }
            }
        }
        let w = vw(v);
        *num.entry(c).or_insert(0.0) += w * b;
        *den.entry(c).or_insert(0.0) += w * ver_total.get(v).copied().unwrap_or(0.0);
    }
    num.iter()
        .filter_map(|(c, b)| {
            let d = den.get(c).copied().unwrap_or(0.0);
            if d > 0.0 { Some(format!("B\t{}\t{:.3}", c, b / d)) } else { None }
        })
        .collect()
}

/// LP rows: per-(champ,lane) LANING-PHASE dominance — gold_line_phase per game,
/// z-scored WITHIN the lane (≥5 games). The dense half of the matchup axis: who
/// wins the lane phase predicts champ-vs-champ lane outcomes that direct
/// head-to-head records (≈2 games/pair) are far too sparse to estimate alone.
/// Changed champs drop rows at/below the anchor (their laning changed too).
fn compute_lane_dom(patch: &str, changed: &HashSet<String>, chg_prev: Option<&str>) -> Vec<String> {
    let mut agg: BTreeMap<(String, u8), (f64, f64)> = BTreeMap::new(); // (gold, matches)
    for line in patch.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 19 {
            continue;
        }
        if changed.contains(f[2]) {
            if let Some(pv) = chg_prev {
                if vkey(f[0]) <= vkey(pv) {
                    continue;
                }
            }
        }
        if let Some(li) = lane_idx(f[3]) {
            let gold: f64 = f[17].parse().unwrap_or(0.0);
            let m: f64 = f[6].parse().unwrap_or(0.0);
            let e = agg.entry((f[2].to_string(), li)).or_insert((0.0, 0.0));
            e.0 += gold;
            e.1 += m;
        }
    }
    let mut out = Vec::new();
    for lane in 0u8..5 {
        let cells: Vec<(&String, f64)> = agg
            .iter()
            .filter(|((_, l), (_, m))| *l == lane && *m >= 5.0)
            .map(|((c, _), (g, m))| (c, g / m))
            .collect();
        if cells.len() < 3 {
            continue;
        }
        let n = cells.len() as f64;
        let mean = cells.iter().map(|(_, v)| v).sum::<f64>() / n;
        let sd = (cells.iter().map(|(_, v)| (v - mean) * (v - mean)).sum::<f64>() / n).sqrt();
        if sd < 1e-6 {
            continue;
        }
        for (c, v) in cells {
            let z = ((v - mean) / sd).clamp(-2.5, 2.5);
            out.push(format!("LP\t{}\t{}\t{:.2}", c, lane, z));
        }
    }
    out
}

/// Patch-change info handed over by champ_changes_update (lib.rs).
#[derive(Default, Clone)]
pub struct ChgInfo {
    pub anchor: Option<String>,
    /// (champ, dir): '+' buff '-' nerf '~' mixed 'R' rework
    pub dirs: Vec<(String, char)>,
    /// (champ, buffs, nerfs) authored change counts — S2-2 count-scaled nudge.
    /// Empty on the defs-diff fallback (engine treats missing counts as n=2,
    /// which reproduces the legacy uniform ±0.02).
    pub counts: Vec<(String, u32, u32)>,
}

/// Build the engine Tables from the in-memory stats TSV + live pool + patch
/// changes + replay-derived rows (MU/OPP/OPPN from lib.rs::replay_rows — the
/// matchup axis and the opponent-aware ban model). Baked T (taxonomy) rows come
/// from the shared engine's WEIGHTS_TSV.
pub fn build_tables(patch_tsv: &str, pool: &[String], chg: &ChgInfo, replay_rows: &str) -> Tables {
    let changed: HashSet<String> = chg.dirs.iter().map(|(c, _)| c.clone()).collect();
    let reworked: HashSet<String> =
        chg.dirs.iter().filter(|(_, d)| *d == 'R').map(|(c, _)| c.clone()).collect();
    let anchor = chg.anchor.as_deref();
    let mut out: Vec<String> = engine::WEIGHTS_TSV
        .lines()
        .filter(|l| l.starts_with("T\t"))
        .map(|s| s.to_string())
        .collect();
    let (c_rows, cp_rows, total) = patch_current(patch_tsv, &changed, anchor);
    out.extend(c_rows);
    out.extend(cp_rows);
    // POS: live data, falling back to the BAKED rows when stats are empty (fresh
    // career) — without POS the lane board/swap assignment dies in cold start.
    // PER-CHAMP fallback too: a released-but-never-played champ (zero stat rows,
    // e.g. whip_master on release day) has no live POS row and would otherwise
    // be invisible to the entire lane board.
    let pos_rows = compute_pos(patch_tsv, &reworked, anchor);
    if pos_rows.is_empty() {
        out.extend(engine::WEIGHTS_TSV.lines().filter(|l| l.starts_with("POS\t")).map(|s| s.to_string()));
    } else {
        let covered: HashSet<&str> =
            pos_rows.iter().filter_map(|r| r.split('\t').nth(1)).collect();
        out.extend(
            engine::WEIGHTS_TSV
                .lines()
                .filter(|l| l.starts_with("POS\t"))
                .filter(|l| l.split('\t').nth(1).map(|c| !covered.contains(c)).unwrap_or(false))
                .map(|s| s.to_string()),
        );
        out.extend(pos_rows);
    }
    out.extend(compute_roles(patch_tsv, &reworked, anchor));
    out.extend(compute_banrate(patch_tsv, &changed, anchor));
    out.extend(compute_lane_dom(patch_tsv, &changed, anchor));
    // CHG rows: 5-field when authored counts exist (S2-2 count-scaled nudge);
    // 3-field otherwise (engine falls back to the legacy uniform ±0.02 size).
    let cnt_of: HashMap<&str, (u32, u32)> =
        chg.counts.iter().map(|(c, b, n)| (c.as_str(), (*b, *n))).collect();
    out.extend(chg.dirs.iter().map(|(c, d)| match cnt_of.get(c.as_str()) {
        Some((b, n)) => format!("CHG\t{}\t{}\t{}\t{}", c, d, b, n),
        None => format!("CHG\t{}\t{}", c, d),
    }));
    if !replay_rows.is_empty() {
        out.push(replay_rows.trim_end().to_string());
    }
    if !pool.is_empty() {
        out.push(format!("POOL\t{}", pool.join(",")));
    }
    out.push(format!("M\t{}", total));
    out.push("SAVE\tlive".to_string());
    parse_tables_from_str(&out.join("\n"))
}

// ───────────────────── live draft state + recommendations ─────────────────────

/// Board read each frame from MatchUIRunner (already side-resolved: my/enemy).
/// my_locked/enemy_locked are the GAME's own fearless lock lists
/// (fearless_team1_locked/fearless_team2_locked, side-resolved): per-team ground
/// truth, so fearless style and counts are DERIVED per pass — no cross-frame lock
/// accumulation, no mid-series recovery guessing, no style latching.
#[derive(Default, Clone, PartialEq)]
pub struct Board {
    pub my_ban: Vec<String>,
    pub enemy_ban: Vec<String>,
    pub my_pick: Vec<String>,
    pub enemy_pick: Vec<String>,
    pub ban_count: usize,
    pub teams: String,
    pub my_locked: Vec<String>,
    pub enemy_locked: Vec<String>,
    /// the lock lists came from RECOGNIZED game tooltips (vs the conservative
    /// both-sides fallback) — gates the 하드 style claim in the commentary.
    pub attributed: bool,
    /// enemy team id — keys the opponent-aware ban model (OPP rows).
    pub enemy_team: u32,
    /// am I the board's team1 slot? T2-CONFIRMED rule (2026-07-05 bp_history,
    /// 4/4 drafts, sides alternating per set): team1 always acts FIRST — first
    /// ban AND first pick — so this seeds 선픽/후픽 before any pick exists.
    pub i_am_team1: Option<bool>,
    /// the enemy AI's top remaining candidates from its latest deliberation
    /// (observed via ModDraftScoreHook) — its likely next picks/bans.
    pub enemy_intent: Vec<String>,
}

/// Cross-frame series tracking — ONLY what the game doesn't hand us directly:
/// the set number (board shrink after a seen-full board), first-pick side, and
/// MY completed-set picks (a champ used by BOTH teams renders as the COLOR
/// tooltip only — runtime-verified — so my own usage must come from tracking).
#[derive(Default)]
struct Series {
    teams: String,
    game: u32,
    /// both sides showed 5 picks (set finished drafting) — the next clearly
    /// smaller board is a new set.
    saw_full: bool,
    /// my picks of the possibly-finished set (promoted into my_used on the
    /// next set transition).
    pending_my: Vec<String>,
    /// my picks of COMPLETED sets — unioned into my_locked under fearless.
    my_used: HashSet<String>,
    first_pick: Option<bool>,
}

/// Structured row for the in-game Top-5 UI panel.
#[derive(Default, Clone)]
pub struct RecRow {
    pub id: String,
    pub ko: String,
    pub lane: String,
    pub tier: String,
    pub chg: String,
    pub score: String,
    pub reason: String,
}

/// Everything the in-game display needs this frame.
#[derive(Default, Clone)]
pub struct Output {
    pub rec_ban: bool,
    /// ranked (id, ko, signed score string)
    pub recs: Vec<(String, String, String)>,
    /// structured top-5 rows for the UI card
    pub detail_rows: Vec<RecRow>,
    /// "라인 이름" pairs once my 5 picks are in
    pub swap: Vec<String>,
    /// commentary lines ('\n'-joined)
    pub say: String,
    /// the commentary minus the lines that restate the ranked rows (the ① headline
    /// and the runner-up list) — for displays that already show `detail_rows`.
    pub notes: Vec<String>,
    /// champ id -> tier label text ("S▲" …)
    pub tiers: HashMap<String, String>,
}

static SERIES: Mutex<Option<Series>> = Mutex::new(None);
static OUTPUT: Mutex<Option<(String, Arc<Output>)>> = Mutex::new(None); // (board key, out)

/// Per-frame: feed the live board; recompute recommendations only on change.
/// `tables_gen` = a counter bumped on every tables rebuild, so a mid-banpick
/// stats/pool refresh invalidates the cache even when the board is unchanged.
pub fn update(board: &Board, tables: &Tables, tables_gen: usize) -> Arc<Output> {
    let mut sg = match SERIES.lock() {
        Ok(g) => g,
        Err(_) => return Arc::new(Output::default()),
    };
    let s = sg.get_or_insert_with(Series::default);

    // ── series tracking (set number + first pick only — locks come from the
    // game). board.teams includes the MATCH id, so a rematch against the same
    // team pair is a NEW series, not a phantom next set.
    if board.teams != s.teams {
        s.teams = board.teams.clone();
        s.game = 1;
        s.saw_full = false;
        s.pending_my.clear();
        s.my_used.clear();
        s.first_pick = None;
    } else if s.saw_full {
        // picks-only: counting bans would mask the shrink (6 bans + 1 pick reads
        // as 7 vs 10 and the boundary is missed when the screen is seen late).
        // +3 slack: a one-slot transient misread must not fire a phantom set.
        let picks_now = board.my_pick.len() + board.enemy_pick.len();
        if picks_now + 3 < 10 {
            s.game += 1;
            s.saw_full = false;
            s.my_used.extend(s.pending_my.drain(..));
            s.first_pick = None;
        }
    }
    if board.my_pick.len() == 5 && board.enemy_pick.len() == 5 {
        s.saw_full = true;
        s.pending_my = board.my_pick.clone();
    }
    // S3-1 (2026-07-05, T2 CONFIRMED via bp_history): the team1 slot always acts
    // first, so the side mapping seeds first_pick BEFORE any pick exists — the
    // whole BAN phase now runs with the right 선픽/후픽 lean instead of order=0.
    // The first-pick observation below stays as ground truth (overrides the seed
    // if they ever disagree — auto-fallback per spec).
    if s.first_pick.is_none() {
        s.first_pick = board.i_am_team1;
    }
    if board.my_pick.len() + board.enemy_pick.len() == 1 {
        s.first_pick = Some(board.my_pick.len() == 1);
    }

    // ── fearless: DERIVED from the game's lock lists every pass (no latching).
    // hard = both teams share one identical lock set (the union); in normal each
    // team is locked out of its OWN used champs, so the lists differ.
    let mut my_locked: HashSet<String> = board.my_locked.iter().cloned().collect();
    let enemy_locked: HashSet<String> = board.enemy_locked.iter().cloned().collect();
    let fearless_on = !my_locked.is_empty() || !enemy_locked.is_empty();
    let hard = !my_locked.is_empty() && my_locked == enemy_locked;
    if fearless_on {
        // dupe hole: a champ used by BOTH teams shows only the COLOR tooltip
        // (lands in enemy_locked) — my own used champs from tracking close it.
        // Gated on fearless_on: in a non-fearless multi-set match re-picks are
        // legal and my_used must NOT filter anything.
        my_locked.extend(s.my_used.iter().cloned());
    }

    // ── recompute only when the board or stats changed. ban_count and teams
    // MUST be keyed: rec_ban derives from ban_count, and OUTPUT survives across
    // matches — two matches' empty-board start frames would otherwise collide
    // and serve a wrong-phase Output under a different ban rule.
    let key = format!(
        "{}|{}|{}|{}|{}|{}|{}|{:?}|{}|{}|{}|{}",
        board.my_ban.join(","),
        board.enemy_ban.join(","),
        board.my_pick.join(","),
        board.enemy_pick.join(","),
        board.my_locked.join(","),
        board.enemy_locked.join(","),
        tables_gen,
        s.first_pick,
        s.game,
        board.ban_count,
        board.teams,
        board.attributed, // commentary's style claim depends on it
    ) + &board.enemy_intent.join(","); // kept in the cache key so a restored intent line stays cache-correct
    if let Ok(g) = OUTPUT.lock() {
        if let Some((k, out)) = g.as_ref() {
            if *k == key {
                return out.clone();
            }
        }
    }

    let rec_ban = board.ban_count > 0 && board.my_ban.len() + board.enemy_ban.len() < board.ban_count * 2;
    let mut st = DraftState::default();
    st.ok = true;
    st.phase = if rec_ban { "BAN".into() } else { "PICK".into() };
    st.pick_order = match s.first_pick {
        Some(true) => 1,
        Some(false) => 2,
        None => 0,
    };
    st.ally_ban = board.my_ban.clone();
    st.enemy_ban = board.enemy_ban.clone();
    st.ally_pick = board.my_pick.clone();
    st.enemy_pick = board.enemy_pick.clone();
    st.enemy_team = board.enemy_team;
    st.enemy_locked = board.enemy_locked.clone();

    // fearless lock filtering happens through the POOL argument (engine design):
    // pick recs drop what I can't pick; ban recs drop what the ENEMY can't pick.
    // The lists come straight from the game, so they're exactly right in both
    // styles (in hard each list already IS the union) and trivially empty in a
    // non-fearless multi-set match.
    let rec_lock = if rec_ban { &enemy_locked } else { &my_locked };
    let base_pool: HashSet<String> = tables.pool.clone();
    let effective: HashSet<String> = if base_pool.is_empty() {
        CHAMPS.iter().map(|(id, _)| id.to_string()).filter(|id| !rec_lock.contains(id)).collect()
    } else {
        base_pool.iter().filter(|id| !rec_lock.contains(*id)).cloned().collect()
    };
    let pool_opt = Some(&effective);

    let view = recommend(tables, &st, false, pool_opt);
    let lane_view = if rec_ban {
        None
    } else {
        Some(recommend_lanes(tables, &st, false, pool_opt, fearless_on))
    };

    // ── recs (id, ko, score) — collected from the lane board (★ lane top3 +
    // other open lanes' top1) but RANKED BY RAW SCORE: the ①~⑤ glyphs must mean
    // "best first", not assembly order (the old order put the ★ lane's 2nd/3rd
    // ahead of another lane's outright OP — the "어중간한 첫픽 추천" defect).
    let mut raw: Vec<(String, String, f32)> = Vec::new();
    if let Some(lv) = lane_view.as_ref().filter(|lv| lv.has_pos) {
        if let Some(star) = lv.lanes.iter().find(|l| l.star) {
            raw.extend(star.cands.iter().take(3).map(|c| (c.id.clone(), c.ko.clone(), c.score)));
        }
        for l in lv.lanes.iter().filter(|l| !l.star && l.filled.is_none()) {
            if let Some(c) = l.cands.first() {
                if !raw.iter().any(|(id, _, _)| *id == c.id) {
                    raw.push((c.id.clone(), c.ko.clone(), c.score));
                }
            }
        }
    } else {
        raw.extend(view.rows.iter().take(5).map(|r| (r.id.clone(), r.ko.clone(), r.score)));
    }
    raw.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    raw.truncate(5);
    // OP callout: on the first 1-2 picks, a clear score gap (≥ one tier step)
    // between #1 and #2 means "secure the OP now" beats any positional finesse.
    let op_gap = (!rec_ban
        && board.my_pick.len() + board.enemy_pick.len() <= 2
        && raw.len() >= 2
        && raw[0].2 - raw[1].2 >= 0.04)
        .then(|| raw[0].1.clone());
    let recs: Vec<(String, String, String)> =
        raw.into_iter().map(|(id, ko, sc)| (id, ko, format!("{:+.0}", sc * 100.0))).collect();
    let detail_rows: Vec<RecRow> = recs
        .iter()
        .map(|(id, ko, sc)| {
            if let Some(lv) = lane_view.as_ref().filter(|lv| lv.has_pos) {
                if let Some((l, c)) = lv
                    .lanes
                    .iter()
                    .find_map(|l| l.cands.iter().find(|c| c.id == *id).map(|c| (l, c)))
                {
                    return RecRow {
                        id: id.clone(),
                        ko: ko.clone(),
                        lane: l.name.to_string(),
                        tier: c.tier.to_string(),
                        chg: c.chg.to_string(),
                        score: sc.clone(),
                        reason: c.label.to_string(),
                    };
                }
            }
            let r = view.rows.iter().find(|r| r.id == *id);
            RecRow {
                id: id.clone(),
                ko: ko.clone(),
                lane: if rec_ban { "밴".to_string() } else { "픽".to_string() },
                tier: r.map(|x| x.tier.to_string()).unwrap_or_default(),
                chg: r.map(|x| x.chg.to_string()).unwrap_or_default(),
                score: sc.clone(),
                reason: r.map(|x| x.label.to_string()).unwrap_or_default(),
            }
        })
        .collect();

    // ── swap lane assignment (my 5 picks in)
    let swap: Vec<String> = match lane_view.as_ref().filter(|lv| lv.has_pos && board.my_pick.len() >= 5) {
        Some(lv) => lv
            .lanes
            .iter()
            .filter_map(|l| l.filled.as_ref().map(|ko| format!("{} {}", l.name, ko)))
            .collect(),
        None => Vec::new(),
    };

    // ── commentary
    let mut parts: Vec<String> = Vec::new();
    // indices of `parts` that restate detail_rows (excluded from `notes`)
    let mut restates: Vec<usize> = Vec::new();
    if let Some(op) = &op_gap {
        parts.push(format!(
            "점수 격차가 큽니다 — 지금은 {} 확보가 우선입니다(라인 조합은 다음 픽에서).",
            op
        ));
    }
    if !swap.is_empty() {
        parts.push("픽이 끝났습니다. 전체 승률이 가장 높아지는 배치는 위 추천 배치입니다.".to_string());
    } else if let Some(lv) = lane_view.as_ref().filter(|lv| lv.has_pos) {
        // the headline must describe the SAME ① the header/glyphs show (the
        // GLOBAL score-sorted top) — it diverged from the ★ lane's local top
        // once the glyphs went score-true (user-reported mismatch 2026-06-13).
        let top = recs.first().and_then(|(id, _, _)| {
            lv.lanes
                .iter()
                .find_map(|l| l.cands.iter().find(|c| c.id == *id).map(|c| (l, c)))
        });
        if let Some((lane, c)) = top {
            let mut t = format!("1순위는 {} {}", lane.name, c.ko);
            if !c.tier.is_empty() {
                t.push_str(&format!("({}티어)", c.tier));
            }
            t.push_str(&format!(", 평점 {:+.0}", c.score * 100.0));
            if !c.label.is_empty() {
                t.push_str(&format!(" — 근거: {}", c.label));
            }
            if !c.chg.is_empty() {
                t.push_str(&format!(" (이번 패치 변경 {})", c.chg));
            }
            if c.chg == "▼" && c.label == "숙련" {
                t.push_str(" — 너프됐지만 선수 숙련 보정으로 상위입니다. 대체 픽도 검토하세요");
            }
            if c.low_conf {
                t.push_str(". 다만 표본이 적어 확신은 낮습니다");
            }
            t.push('.');
            restates.push(parts.len());
            parts.push(t);
            if lane.cands.len() > 1 {
                restates.push(parts.len());
                parts.push(format!(
                    "같은 라인 대안: {}.",
                    lane.cands
                        .iter()
                        .filter(|x| x.id != c.id)
                        .take(2)
                        .map(|c| format!("{} {:+.0}", c.ko, c.score * 100.0))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(cof) = &lane.counter_of {
                // counters() is driven by laning-dominance (LP z) + cold-start tags;
                // the head-to-head record (MU direct) is weight-0 in the mod (BT
                // residual τ≈0), so DON'T claim "상대 전적상 우위" — laning metric only
                // (2026-07-05 review F10).
                parts.push(format!(
                    "1순위 픽은 상대 {}에게 라인전 지표상 우위가 있습니다.",
                    cof
                ));
            }
            // ★ lane-order guidance, as a SEPARATE suggestion when it differs
            // from the #1's lane (e.g. fearless depletion urges a thin lane).
            if let Some(star) = lv.lanes.iter().find(|l| l.star && l.filled.is_none()) {
                if star.name != lane.name {
                    if let Some(sc) = star.cands.first() {
                        parts.push(format!(
                            "라인 순서 제안: {} 라인을 먼저 확보하는 것도 좋습니다 — 해당 라인 1순위 {} {:+.0}.",
                            star.name,
                            sc.ko,
                            sc.score * 100.0
                        ));
                    }
                }
            }
            if !lv.tip.is_empty() {
                parts.push(format!("전략 메모: {}.", lv.tip));
            }
        }
    } else if let Some(r) = view.rows.first() {
        // flat-list branch serves BOTH phases (ban, and pick without POS data)
        let mut t = format!("{} 1순위는 {}", if rec_ban { "밴" } else { "픽" }, r.ko);
        if !r.tier.is_empty() {
            t.push_str(&format!("({}티어)", r.tier));
        }
        t.push_str(&format!(", 평점 {:+.0}", r.score * 100.0));
        if !r.label.is_empty() {
            t.push_str(&format!(" — 근거: {}", r.label));
        }
        if !r.chg.is_empty() {
            t.push_str(&format!(" (이번 패치 변경 {})", r.chg));
        }
        if r.low_conf {
            t.push_str(". 다만 표본이 적어 확신은 낮습니다");
        }
        t.push('.');
        restates.push(parts.len());
        parts.push(t);
        if view.rows.len() > 1 {
            restates.push(parts.len());
            parts.push(format!(
                "차순위 후보: {}.",
                view.rows
                    .iter()
                    .skip(1)
                    .take(3)
                    .map(|r| format!("{} {:+.0}", r.ko, r.score * 100.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if !board.enemy_pick.is_empty() {
        let ids: Vec<&str> = board.enemy_pick.iter().map(|s| s.as_str()).collect();
        // only speak about the enemy comp when we actually HAVE role data —
        // comp_counts reads "no data" as "no tank", which would invert the advice.
        let known = ids.iter().filter(|c| tables.role_known(c)).count();
        if known > 0 {
            let (tanks, heals) = tables.comp_counts(&ids);
            let mut t = format!("상대 조합 {}픽 — 탱커 {} · 힐러 {}", ids.len(), tanks, heals);
            if tanks == 0 && known == ids.len() {
                t.push_str(". 앞라인이 없어 돌진·암살 가치가 높습니다");
            } else if heals >= 1 {
                t.push_str(". 힐러가 있어 장기전 불리 — 폭딜이나 CC 연계를 고려하세요");
            }
            t.push('.');
            parts.push(t);
        }
    }
    // own-comp guardrail warning (symmetric to the enemy-comp line): 3+ of MY
    // picks with no frontline is the comp the autobattle punishes hardest.
    if board.my_pick.len() >= 3 && swap.is_empty() {
        let ids: Vec<&str> = board.my_pick.iter().map(|s| s.as_str()).collect();
        let known = ids.iter().filter(|c| tables.role_known(c)).count();
        if known == ids.len() {
            let (tanks, _) = tables.comp_counts(&ids);
            if tanks == 0 {
                parts.push("아군 조합에 앞라인이 없습니다 — 남은 픽에서 탱커/브루저를 고려하세요.".to_string());
            }
        }
    }
    // enemy-AI-intent commentary REMOVED (code review 2026-07-04): the draft-score
    // hook that feeds board.enemy_intent fires only for BACKGROUND league sims, never
    // the player's opponent (CLAUDE.md §2c) — so the line presented league-wide noise
    // as "the enemy's likely next picks". Same root cause as the disabled deny/steal
    // term. The field is still built (harmless) but no longer surfaced to the user.
    // honesty line: without matchup data the ranking cannot react to enemy picks.
    if !tables.has_matchup() && !board.enemy_pick.is_empty() {
        parts.push("상대 픽 대응(매치업) 데이터가 아직 없습니다 — 이 순위는 상대 픽을 반영하지 못합니다.".to_string());
    }
    let changed_list: Vec<String> = CHAMPS
        .iter()
        .filter_map(|(id, ko)| {
            let m = tables.chg_mark(id);
            if m.is_empty() { None } else { Some(format!("{}{}", ko, m)) }
        })
        .take(10)
        .collect();
    if !changed_list.is_empty() {
        parts.push(format!(
            "이번 패치 변경: {} — 변경 챔프는 통계가 리셋되어 티어가 비어있을 수 있습니다.",
            changed_list.join(" ")
        ));
    }
    if fearless_on {
        // locks exist but no earlier set was observed → the mod joined mid-series
        // and the set counter is unknowable; the lock COUNTS stay exact regardless
        // (they come from the game's own cell marks).
        let set_label = if s.game >= 2 {
            format!("{}세트", s.game)
        } else {
            "세트 미상".to_string()
        };
        if hard {
            // identical lock sets: hard fearless when the game's tooltips said so
            // ("양팀 모두…"), otherwise the conservative both-sides fallback.
            parts.push(format!(
                "피어리스 {}({}) — 사용 불가{} {}챔.",
                set_label,
                if board.attributed { "하드" } else { "스타일 미상" },
                if board.attributed { "(공통)" } else { "(공통 처리)" },
                my_locked.len()
            ));
        } else {
            parts.push(format!(
                "피어리스 {}(노멀) — 사용 불가: 내 {}챔 · 상대 {}챔.",
                set_label,
                my_locked.len(),
                enemy_locked.len()
            ));
        }
        // depletion: STRONG (S/A) champs still pickable per lane — don't burn a
        // thin lane's only strong champ in an early set (ported back from the
        // retired overlay; the display was lost in the v3.0 port).
        let taken: HashSet<&str> = board
            .my_ban
            .iter()
            .chain(board.enemy_ban.iter())
            .chain(board.my_pick.iter())
            .chain(board.enemy_pick.iter())
            .map(|s| s.as_str())
            .collect();
        let avail: HashSet<String> = CHAMPS
            .iter()
            .map(|(id, _)| id.to_string())
            .filter(|id| {
                (base_pool.is_empty() || base_pool.contains(id))
                    && !my_locked.contains(id)
                    && !taken.contains(id.as_str())
            })
            .collect();
        let d = tables.strong_per_lane(&avail);
        // NOTE(2026-07-05 review F10): don't flatly say "아껴두세요" — it contradicts the
        // ★ guidance (which urges securing a thin lane NOW before the enemy takes it).
        // Frame the thin lanes as a per-set allocation call, not a blanket "save".
        parts.push(format!(
            "남은 강챔(S/A) ▶ 탑 {} · 정글 {} · 미드 {} · 바텀 {} · 서폿 {} — 적은 라인은 세트 배분에 주의(이번 세트 확보 vs 다음 세트 보존).",
            d[0], d[1], d[2], d[3], d[4]
        ));
    }
    parts.push(format!(
        "데이터: 유효 표본 {}판(직전 패치 블렌드 포함){}",
        tables.matches,
        if tables.matches < 50 { " — 콜드스타트 구간, 이론 비중이 큽니다." } else { "." }
    ));

    // ── tier labels for the grid
    let tiers: HashMap<String, String> = CHAMPS
        .iter()
        .filter_map(|(id, _)| {
            let t = tables.tier_display(id, None);
            let m = tables.chg_mark(id);
            if t.is_empty() && m.is_empty() {
                None
            } else {
                Some((id.to_string(), format!("{}{}", t, m)))
            }
        })
        .collect();

    let notes: Vec<String> =
        parts.iter().enumerate().filter(|(i, _)| !restates.contains(i)).map(|(_, p)| p.clone()).collect();
    let out = Arc::new(Output { rec_ban, recs, detail_rows, swap, say: parts.join("\n"), notes, tiers });
    if let Ok(mut g) = OUTPUT.lock() {
        *g = Some((key, out.clone()));
    }
    out
}
