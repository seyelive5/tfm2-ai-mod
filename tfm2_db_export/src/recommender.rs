//! In-mod recommender — the FULL pipeline (live stats → Tables → board state →
//! recommendations → display strings), integrated from the retired launcher
//! (inject-overlay.exe) and overlay (draft_overlay.dll) for the v3.0 single-mod
//! architecture: zero executables, zero injection, Workshop-distributable.
//!
//! The engine itself is shared source with the old overlay (single source of
//! truth via #[path]). The data-S/K (synergy/counter) rows the old gen.exe
//! computed are intentionally DROPPED: backtests showed they add ~nothing over
//! winrate. The baked T rows (tags) remain for comp; the engine's tag-theory
//! synergy/counter terms are weighted 0 since E7a (2026-10-29, no winner signal).

#[path = "../../draft-overlay/src/engine.rs"]
#[allow(dead_code)] // shared source: the retired overlay's surface stays in-tree
pub mod engine;

use engine::{parse_tables_from_str, recommend, recommend_lanes, DraftState, Tables, CHAMPS};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
// E7b (2026-10-29, v4.5.0): a champ CHANGED this patch starts from its posterior
// in the version it changed from, moved by the patch: μ = clamp(post + CARRY_A +
// CARRY_B·size, 0.2, 0.8) at prior strength KAPPA_CARRY. It replaces the reset +
// count nudge: career replay, log loss ×1000 on matches with a changed champ
// under 30 current games +3.1 [+0.4, +6.0] (patches 1.1–3.1), holdout 4.0/4.1
// +4.8 [−1.4, +10.3]. a, b = OLS over all 62 points of the nine real-size
// patches (analysis/harness-e7b-holdout.txt, "product fit").
const CARRY_A: f64 = -0.0041;
const CARRY_B: f64 = 0.00072;
const KAPPA_CARRY: f64 = 80.0;

/// The carry's shift when the note has no numeric values: the S2-2 count nudge
/// (2026-07-05; T-nudge measured ≈+0.0125 delta per net change, r +0.66, n 46).
/// Uniform direction 0.01 + 0.005·min(n, 4) (no counts → n = 2, the legacy
/// ±0.02); mixed '~' 0.02·(b − n′)/n·min(1, n/4). It was a score term faded over
/// 30 fresh games until v4.5.0; as a prior shift the posterior does the fading.
fn count_shift(dir: char, buffs: u32, nerfs: u32) -> f64 {
    let n = buffs + nerfs;
    let uni = 0.01 + 0.005 * (if n == 0 { 2 } else { n.min(4) }) as f64;
    match dir {
        '+' => uni,
        '-' => -uni,
        '~' if n > 0 => 0.02 * (buffs as f64 - nerfs as f64) / n as f64 * (n as f64 / 4.0).min(1.0),
        _ => 0.0,
    }
}

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
use crate::markup;
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
/// previous ×fading). Champs CHANGED this patch (`carry`: champ → prior shift)
/// skip every row at/below the diff anchor and start from their posterior in the
/// version they changed from, moved by the shift (E7b).
fn patch_current(patch: &str, carry: &HashMap<String, f64>, chg_prev: Option<&str>) -> (Vec<String>, Vec<String>, u32) {
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
    // a changed champ's prior comes from the version it changed from, that
    // version alone (as in the measured replay); its lanes there keep a cell
    let base = chg_prev.or(prev).map(vkey);
    let mut base_c: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    let mut base_lanes: BTreeSet<(&str, u8)> = BTreeSet::new();
    for f in &rows {
        if !carry.contains_key(f[2]) || base.as_ref() != Some(&vkey(f[0])) {
            continue;
        }
        let w: f64 = f[5].parse().unwrap_or(0.0);
        let m: f64 = f[6].parse().unwrap_or(0.0);
        let e = base_c.entry(f[2]).or_insert((0.0, 0.0));
        e.0 += w;
        e.1 += m;
        if let Some(li) = lane_idx(f[3]).filter(|_| m > 0.0) {
            base_lanes.insert((f[2], li));
        }
    }
    for f in &rows {
        if carry.contains_key(f[2]) {
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
    // champ posteriors (mean, sd, current games). C rows: posterior MEAN − 0.5
    // (κ-shrunk; see the note above beta_post). A changed champ gets a row even
    // with no game yet this patch: its carried prior is the estimate.
    let mut post: BTreeMap<&str, (f64, f64, f64)> = by
        .iter()
        .filter(|(c, _)| !carry.contains_key(c.as_str()))
        .map(|(c, (w, m))| {
            let (mean, sd) = beta_post(*w, *m, 0.5, KAPPA_CHAMP);
            (c.as_str(), (mean, sd, *m))
        })
        .collect();
    for (c, shift) in carry {
        let (bw, bm) = base_c.get(c.as_str()).copied().unwrap_or((0.0, 0.0));
        let mu = (beta_post(bw, bm, 0.5, KAPPA_CHAMP).0 + shift).clamp(0.2, 0.8);
        let (w, m) = by.get(c).copied().unwrap_or((0.0, 0.0));
        let (mean, sd) = beta_post(w, m, mu, KAPPA_CARRY);
        post.insert(c.as_str(), (mean, sd, m));
    }
    // 5th field = posterior sd (honest uncertainty) → engine's tier "?" gate.
    let c_rows = post
        .iter()
        .map(|(c, (mean, sd, m))| format!("C\t{}\t{:.3}\t{}\t{:.4}", c, mean - 0.5, m.round() as u32, sd))
        .collect();
    // CP rows: posterior MEAN − 0.5 with the champ's own mean as the prior — a
    // thin lane cell now reads ≈ the champ's strength instead of champ−1sd (the
    // old z·sd artifact made a 1-game lane score BELOW a 0-game lane).
    let mut cp_rows: Vec<String> = byp
        .iter()
        .filter(|(_, (_, m))| *m > 0.0)
        .map(|((c, li), (w, m))| {
            let prior = post.get(c.as_str()).map_or(0.5, |p| p.0);
            let (mean, sd) = beta_post(*w, *m, prior, KAPPA_LANE);
            format!("CP\t{}\t{}\t{:.3}\t{}\t{:.4}", c, li, mean - 0.5, m.round() as u32, sd)
        })
        .collect();
    // E7b kept lanes: a lane the changed champ played in the version it changed
    // from, with no game there yet this patch, reads its carried value (0 games)
    for &(c, li) in &base_lanes {
        if byp.get(&(c.to_string(), li)).map_or(false, |p| p.1 > 0.0) {
            continue;
        }
        if let Some((mean, sd, _)) = post.get(c) {
            cp_rows.push(format!("CP\t{}\t{}\t{:.3}\t0\t{:.4}", c, li, mean - 0.5, sd));
        }
    }
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
/// the diff anchor (same stale-row drop as the C rows).
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
    /// (champ, buffs, nerfs) authored change counts — the carry's shift when the
    /// note has no sizes (`count_shift`; missing counts → n = 2, the legacy ±0.02).
    pub counts: Vec<(String, u32, u32)>,
    /// (champ, signed size) — Σ±min(100, |after − pre|/|pre|·100) over its change
    /// lines (E7b). Empty when the note has no numeric values: the count nudge
    /// then stands in for the size shift.
    pub sizes: Vec<(String, f64)>,
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
    // E7b prior shift per changed champ: the size fit, or the count nudge when
    // the note has no numeric values
    let cnt_of: HashMap<&str, (u32, u32)> = chg.counts.iter().map(|(c, b, n)| (c.as_str(), (*b, *n))).collect();
    let size_of: HashMap<&str, f64> = chg.sizes.iter().map(|(c, s)| (c.as_str(), *s)).collect();
    let carry: HashMap<String, f64> = chg
        .dirs
        .iter()
        .map(|(c, d)| {
            let shift = if chg.sizes.is_empty() {
                let (b, n) = cnt_of.get(c.as_str()).copied().unwrap_or((0, 0));
                count_shift(*d, b, n)
            } else {
                CARRY_A + CARRY_B * size_of.get(c.as_str()).copied().unwrap_or(0.0)
            };
            (c.clone(), shift)
        })
        .collect();
    let (c_rows, cp_rows, total) = patch_current(patch_tsv, &carry, anchor);
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
    // CHG rows: the ▲▼◆ marks only — the patch shift is in the C rows' prior (E7b)
    out.extend(chg.dirs.iter().map(|(c, d)| format!("CHG\t{}\t{}", c, d)));
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
    /// my side (blue = true); None → the expected win rate drops its side term
    pub my_blue: Option<bool>,
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
    /// bare tier letter — no "?": shown in the game's own fonts a trailing "?"
    /// reads as a broken glyph (2026-10-27 test)
    pub tier: String,
    pub chg: String,
    pub score: String,
    pub reason: String,
}

/// A champion's grid tag: tier letter, its uncertainty, patch-change mark.
#[derive(Clone)]
pub struct TierMark {
    /// S/A/B/C/D, "" without winrate data
    pub letter: &'static str,
    /// the letter is a coin-flip with an adjacent tier (engine `tier_unsure`)
    pub unsure: bool,
    /// ▲ / ▼ / ◆, "" if unchanged this patch
    pub chg: &'static str,
}

/// One lane of the swap-phase assignment.
#[derive(Clone, Debug)]
pub struct SwapRow {
    pub lane: &'static str,
    pub id: String,
    pub ko: String,
}

/// Everything the in-game display needs this frame.
#[derive(Default, Clone)]
pub struct Output {
    pub rec_ban: bool,
    /// ranked (id, ko, signed score string)
    pub recs: Vec<(String, String, String)>,
    /// structured top-5 rows for the UI card
    pub detail_rows: Vec<RecRow>,
    /// lane assignment once my 5 picks are in, in lane order
    pub swap: Vec<SwapRow>,
    /// commentary lines ('\n'-joined)
    pub say: String,
    /// the commentary minus the runner-up list — for displays that already show
    /// `detail_rows` (the headline stays: it carries the why, tier and warnings)
    pub notes: Vec<String>,
    /// champ id -> grid tag (champions with neither a letter nor a mark absent)
    pub tiers: HashMap<String, TierMark>,
    /// effective sample size behind the stats (Tables::matches)
    pub matches: usize,
}

static SERIES: Mutex<Option<Series>> = Mutex::new(None);
static OUTPUT: Mutex<Option<(String, Arc<Output>)>> = Mutex::new(None); // (board key, out)

/// "+12" in the score color.
fn score_text(score: f32) -> String {
    markup::num(&format!("{:+.0}", score * 100.0))
}

/// "검사 +12" with the name and the score in their colors.
fn name_score(ko: &str, score: f32) -> String {
    format!("{} {}", markup::name(ko), score_text(score))
}

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
        "{}|{}|{}|{}|{}|{}|{}|{:?}|{}|{}|{}|{}|{:?}",
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
        board.my_blue,    // the expected win rate's side term
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
                        tier: c.letter.to_string(),
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
                tier: r.map(|x| x.letter.to_string()).unwrap_or_default(),
                chg: r.map(|x| x.chg.to_string()).unwrap_or_default(),
                score: sc.clone(),
                reason: r.map(|x| x.label.to_string()).unwrap_or_default(),
            }
        })
        .collect();

    // ── swap lane assignment (my 5 picks in)
    let swap: Vec<SwapRow> = match lane_view.as_ref().filter(|lv| lv.has_pos && board.my_pick.len() >= 5) {
        Some(lv) => lv
            .lanes
            .iter()
            .filter_map(|l| {
                // the lane board carries the name only; ko_name is exact on my picks
                let ko = l.filled.as_ref()?;
                let id = board.my_pick.iter().find(|p| engine::ko_name(p) == *ko)?;
                Some(SwapRow { lane: l.name, id: id.clone(), ko: ko.clone() })
            })
            .collect(),
        None => Vec::new(),
    };

    // ── commentary, in the game's label markup (names / scores / warnings)
    let mut parts: Vec<String> = Vec::new();
    // the runner-up list restates detail_rows → kept out of `notes`
    let mut runner_up: Option<usize> = None;
    if let Some(op) = &op_gap {
        parts.push(format!(
            "점수 격차가 큽니다 — 지금은 {} 확보가 우선입니다(라인 조합은 다음 픽에서).",
            markup::name(op)
        ));
    }
    if !swap.is_empty() {
        // one card line (48 units): the note card shows 6 lines in all
        parts.push("픽 완료 — 위 배치가 전체 승률이 가장 높습니다.".to_string());
        // E7c: calibrated on champion strength alone (AI prior, comp, familiarity unproven)
        let mine: Vec<&str> = board.my_pick.iter().map(|x| x.as_str()).collect();
        let theirs: Vec<&str> = board.enemy_pick.iter().map(|x| x.as_str()).collect();
        if let Some(p) = engine::win_probability(tables, &mine, &theirs, board.my_blue) {
            parts.push(format!("예상 승률(챔피언 강도 기준) 약 {}", markup::num(&format!("{:.0}%", p * 100.0))));
        }
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
            let mut t = format!("1순위는 {} {}", lane.name, markup::name(&c.ko));
            if !c.letter.is_empty() {
                t.push_str(&format!("({}티어)", c.letter));
            }
            t.push_str(&format!(", 평점 {}", score_text(c.score)));
            if !c.label.is_empty() {
                t.push_str(&format!(" — 근거: {}", c.label));
            }
            if !c.chg.is_empty() {
                t.push_str(&format!(" (이번 패치 변경 {})", c.chg));
            }
            if c.chg == "▼" && c.label == "숙련" {
                t.push_str(&format!(" — {}", markup::warn("너프됐지만 선수 숙련 보정으로 상위입니다. 대체 픽도 검토하세요")));
            }
            if c.low_conf {
                t.push_str(&format!(". {}", markup::warn("다만 표본이 적어 확신은 낮습니다")));
            }
            t.push('.');
            parts.push(t);
            if lane.cands.len() > 1 {
                parts.push(format!(
                    "같은 라인 대안: {}.",
                    lane.cands
                        .iter()
                        .filter(|x| x.id != c.id)
                        .take(2)
                        .map(|c| name_score(&c.ko, c.score))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(cof) = &lane.counter_of {
                // counters() is driven by laning-dominance (LP z) + cold-start tags;
                // the head-to-head record (MU direct) is weight-0 in the mod (BT
                // residual τ≈0), so DON'T claim "상대 전적상 우위" — laning metric only
                // (2026-07-05 review F10). Both drivers are weight-0 now (LP since
                // S2-4, the tags since E7a), so this line stays dark in the mod.
                parts.push(format!(
                    "1순위 픽은 상대 {}에게 라인전 지표상 우위가 있습니다.",
                    markup::name(cof)
                ));
            }
            // ★ lane-order guidance, as a SEPARATE suggestion when it differs
            // from the #1's lane (e.g. fearless depletion urges a thin lane).
            if let Some(star) = lv.lanes.iter().find(|l| l.star && l.filled.is_none()) {
                if star.name != lane.name {
                    if let Some(sc) = star.cands.first() {
                        parts.push(format!(
                            "라인 순서 제안: {} 라인을 먼저 확보하는 것도 좋습니다 — 해당 라인 1순위 {}.",
                            star.name,
                            name_score(&sc.ko, sc.score)
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
        let mut t = format!("{} 1순위는 {}", if rec_ban { "밴" } else { "픽" }, markup::name(&r.ko));
        if !r.letter.is_empty() {
            t.push_str(&format!("({}티어)", r.letter));
        }
        t.push_str(&format!(", 평점 {}", score_text(r.score)));
        if !r.label.is_empty() {
            t.push_str(&format!(" — 근거: {}", r.label));
        }
        if !r.chg.is_empty() {
            t.push_str(&format!(" (이번 패치 변경 {})", r.chg));
        }
        if r.low_conf {
            t.push_str(&format!(". {}", markup::warn("다만 표본이 적어 확신은 낮습니다")));
        }
        t.push('.');
        parts.push(t);
        if view.rows.len() > 1 {
            runner_up = Some(parts.len());
            parts.push(format!(
                "차순위 후보: {}.",
                view.rows
                    .iter()
                    .skip(1)
                    .take(3)
                    .map(|r| name_score(&r.ko, r.score))
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
            // counts only (2026-10-29): the "no frontline → dive" and "healer →
            // burst/CC" advice had no backing — "no frontline = worst" was refuted
            // (stratified residual ≈ 0, n=3,077) and the counter rules showed no
            // signal (E7a)
            parts.push(format!(
                "상대 조합 {}픽 — 탱커 {} · 힐러 {}.",
                ids.len(),
                markup::num(&tanks.to_string()),
                markup::num(&heals.to_string())
            ));
        }
    }
    // own-comp "no frontline" warning REMOVED (2026-10-29): its premise — the comp
    // the autobattle punishes hardest — was refuted by the 2026-07-05 review F3
    // (the more-frontline side wins 50.0%, stratified residual ≈ 0, n=3,077). The
    // score dropped the matching boost then; the warning outlived it.
    // enemy-AI-intent commentary REMOVED (code review 2026-07-04): the draft-score
    // hook that feeds board.enemy_intent fires only for BACKGROUND league sims, never
    // the player's opponent (CLAUDE.md §2c) — so the line presented league-wide noise
    // as "the enemy's likely next picks". Same root cause as the disabled deny/steal
    // term. The field is still built (harmless) but no longer surfaced to the user.
    // honesty line: without matchup data the ranking cannot react to enemy picks
    // (no ranking left to qualify once the swap assignment is up).
    if !tables.has_matchup() && !board.enemy_pick.is_empty() && swap.is_empty() {
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
            "이번 패치 변경: {} — 변경 챔프는 지난 패치 성적에 변경 폭을 반영해 출발합니다.",
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
                markup::num(&my_locked.len().to_string())
            ));
        } else {
            parts.push(format!(
                "피어리스 {}(노멀) — 사용 불가: 내 {}챔 · 상대 {}챔.",
                set_label,
                markup::num(&my_locked.len().to_string()),
                markup::num(&enemy_locked.len().to_string())
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
        // an empty lane in the warning color: the thin lanes are the point
        let depth = |n: u32| if n == 0 { markup::warn("0") } else { markup::num(&n.to_string()) };
        // NOTE(2026-07-05 review F10): don't flatly say "아껴두세요" — it contradicts the
        // ★ guidance (which urges securing a thin lane NOW before the enemy takes it).
        // Frame the thin lanes as a per-set allocation call, not a blanket "save".
        parts.push(format!(
            "남은 강챔(S/A) ▶ 탑 {} · 정글 {} · 미드 {} · 바텀 {} · 서폿 {} — 적은 라인은 세트 배분에 주의(이번 세트 확보 vs 다음 세트 보존).",
            depth(d[0]),
            depth(d[1]),
            depth(d[2]),
            depth(d[3]),
            depth(d[4])
        ));
    }
    // last: the first line a full card gives up (the card's pill keeps the count)
    parts.push(markup::dim(&format!(
        "데이터: 유효 표본 {}판(직전 패치 블렌드 포함){}",
        tables.matches,
        if tables.matches < 50 { " — 콜드스타트 구간, 순위 근거가 약합니다." } else { "." }
    )));

    // ── tier labels for the grid
    let tiers: HashMap<String, TierMark> = CHAMPS
        .iter()
        .filter_map(|(id, _)| {
            let m = TierMark {
                letter: tables.tier_of(id, None),
                unsure: tables.tier_unsure(id, None),
                chg: tables.chg_mark(id),
            };
            (!m.letter.is_empty() || !m.chg.is_empty()).then(|| (id.to_string(), m))
        })
        .collect();

    let notes: Vec<String> =
        parts.iter().enumerate().filter(|(i, _)| Some(*i) != runner_up).map(|(_, p)| p.clone()).collect();
    let out = Arc::new(Output {
        rec_ban,
        recs,
        detail_rows,
        swap,
        say: parts.join("\n"),
        notes,
        tiers,
        matches: tables.matches,
    });
    if let Ok(mut g) = OUTPUT.lock() {
        *g = Some((key, out.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markup;

    /// Empty stats + the baked T rows: the engine still ranks (cold start).
    /// Each test owns its `teams` key — update() keeps series state across calls.
    fn run(teams: &str, b: Board, matches: usize) -> Arc<Output> {
        let mut t = build_tables("", &[], &ChgInfo::default(), "");
        t.matches = matches;
        update(&Board { teams: teams.to_string(), ..b }, &t, 7_000_000 + matches)
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn swap_board() -> Board {
        Board {
            my_pick: ids(&["fighter", "swordman", "jiangshi", "pole_warrior", "vampire"]),
            enemy_pick: ids(&["ogre", "archer", "monk", "ninja", "priest"]),
            ..Default::default()
        }
    }

    fn plain_notes(o: &Output) -> Vec<String> {
        o.notes.iter().map(|n| markup::plain(n)).collect()
    }

    #[test]
    fn pick_notes_open_with_the_first_choice_sentence() {
        let n = plain_notes(&run("t-pick-first", Board::default(), 0));
        assert!(n[0].starts_with("1순위는 "), "{:?}", n);
    }

    #[test]
    fn pick_notes_keep_the_same_lane_alternatives() {
        let n = plain_notes(&run("t-pick-alt", Board::default(), 0));
        assert!(n.iter().any(|x| x.starts_with("같은 라인 대안: ")), "{:?}", n);
    }

    #[test]
    fn ban_notes_open_with_the_first_choice_sentence() {
        let n = plain_notes(&run("t-ban-first", Board { ban_count: 3, ..Default::default() }, 0));
        assert!(n[0].starts_with("밴 1순위는 "), "{:?}", n);
    }

    #[test]
    fn ban_notes_leave_the_runner_up_list_to_the_rows() {
        let o = run("t-ban-runner", Board { ban_count: 3, ..Default::default() }, 0);
        assert!(markup::plain(&o.say).contains("차순위 후보: "));
        assert!(!plain_notes(&o).iter().any(|x| x.contains("차순위 후보")));
    }

    #[test]
    fn commentary_paints_names_and_scores() {
        let o = run("t-paint", Board::default(), 0);
        let (_, ko, sc) = &o.recs[0];
        assert!(o.notes[0].contains(&markup::name(ko)), "{}", o.notes[0]);
        assert!(o.notes[0].contains(&markup::num(sc)), "{}", o.notes[0]);
    }

    #[test]
    fn data_line_closes_the_notes_dimmed() {
        let o = run("t-data", Board::default(), 0);
        let last = o.notes.last().unwrap();
        let text = markup::plain(last);
        assert!(text.starts_with("데이터: "), "{}", text);
        assert_eq!(*last, markup::dim(&text));
    }

    #[test]
    fn output_carries_the_sample_size() {
        assert_eq!(run("t-matches", Board::default(), 37).matches, 37);
    }

    #[test]
    fn swap_rows_name_each_lane_with_its_champion_id() {
        let b = swap_board();
        let o = run("t-swap-rows", b.clone(), 0);
        let lanes: Vec<&str> = o.swap.iter().map(|r| r.lane).collect();
        assert_eq!(lanes, engine::LANES.to_vec());
        for r in &o.swap {
            assert!(b.my_pick.contains(&r.id), "{}", r.id);
            assert_eq!(r.ko, engine::ko_name(&r.id));
        }
    }

    #[test]
    fn swap_notes_skip_the_ranking_caveat() {
        let n = plain_notes(&run("t-swap-caveat", swap_board(), 0));
        assert!(!n.iter().any(|x| x.contains("이 순위는")), "{:?}", n);
    }

    /// E7a (2026-10-29): the tag theory showed no winner signal in a new save's
    /// first 200 matches, so it moves no score. The synthetic tags make each rule
    /// fire at full theory weight (0 matches): ogre is a ranged backliner that is
    /// also a tank (counter rules), archer brings CC for monk's AOE (synergy rule).
    fn theory_tables() -> engine::Tables {
        engine::parse_tables_from_str(
            "T\tvampire\tAssassin\tAD\tMobility,Poke\n\
             T\togre\tRange\tAD\tBackline,Tank,Frontline\n\
             T\tarcher\tRange\tAD\tCC\n\
             T\tmonk\tFighter\tAD\tAOE\n\
             POS\tvampire\t0\t0\n\
             POS\togre\t0\t0",
        )
    }

    #[test]
    fn tag_theory_moves_no_pick_score() {
        let t = theory_tables();
        assert_eq!(t.score_pick("vampire", &[], &["ogre"]).0, 0.0);
        assert_eq!(t.score_pick("monk", &["archer"], &[]).0, 0.0);
    }

    #[test]
    fn tag_theory_moves_no_ban_score() {
        assert_eq!(theory_tables().score_ban("vampire", &["ogre"], &[], 0, 0, None).0, 0.0);
    }

    #[test]
    fn tag_theory_raises_no_lane_counter_flag() {
        let s = engine::DraftState { enemy_pick: ids(&["ogre"]), ..Default::default() };
        let v = engine::recommend_lanes(&theory_tables(), &s, false, None, false);
        assert_eq!(v.lanes[0].counter_of, None);
    }

    /// the reason tag names the part that lifted the score; with no positive part
    /// there is none (ties at 0 used to fall to the LAST label, the zero-weight
    /// "저격픽" — more common at cold start once the theory went to 0)
    #[test]
    fn no_reason_tag_without_a_positive_part() {
        let t = theory_tables();
        assert_eq!(t.score_pick("monk", &[], &[]).1, "");
        assert_eq!(t.score_ban("monk", &[], &[], 0, 0, None).1, "");
        let s = engine::parse_tables_from_str("C\tmonk\t0.050\t30\t0.02");
        assert_eq!(s.score_pick("monk", &[], &[]).1, "강챔");
        assert_eq!(s.score_ban("monk", &[], &[], 0, 0, None).1, "강챔");
    }

    #[test]
    fn cold_start_data_line_does_not_credit_the_theory() {
        let text = markup::plain(run("t-data-cold", Board::default(), 0).notes.last().unwrap());
        assert!(text.contains("콜드스타트 구간"), "{}", text);
        assert!(!text.contains("이론"), "{}", text);
    }

    /// the mid/late memo used to advise counters and synergy; the harness found
    /// no effect for either and the score has neither term (2026-10-29)
    #[test]
    fn strategy_memo_names_what_the_ranking_does() {
        for (key, mine) in [("t-memo-mid", &["fighter", "swordman"][..]), ("t-memo-late", &["fighter", "swordman", "jiangshi"][..])] {
            let n = plain_notes(&run(key, Board { my_pick: ids(mine), ..Default::default() }, 0));
            let memo = n.iter().find(|x| x.starts_with("전략 메모: ")).expect("memo");
            assert!(memo.contains("남은 강챔 우선"), "{}", memo);
            assert!(!memo.contains("카운터 우선") && !memo.contains("채우기"), "{}", memo);
        }
    }

    /// baked T/POS rows plus data roles (RP = tank ratio, heal ratio)
    fn role_tables(rp: &[(&str, f32, f32)]) -> engine::Tables {
        let mut tsv: Vec<String> = engine::WEIGHTS_TSV
            .lines()
            .filter(|l| l.starts_with("T\t") || l.starts_with("POS\t"))
            .map(str::to_string)
            .collect();
        tsv.extend(rp.iter().map(|(c, t, h)| format!("RP\t{}\t{:.2}\t{:.2}", c, t, h)));
        engine::parse_tables_from_str(&tsv.join("\n"))
    }

    /// the comp lines keep the counts but drop advice the evidence does not back:
    /// "no frontline = worst" was refuted (stratified residual ≈ 0, n=3,077) and
    /// the counter rules showed no signal (E7a)
    #[test]
    fn comp_lines_state_counts_without_unproven_advice() {
        let flat: Vec<(&str, f32, f32)> =
            ["fighter", "swordman", "jiangshi", "ogre", "archer", "monk"].iter().map(|c| (*c, 0.8, 0.1)).collect();
        let b = Board {
            teams: "t-comp-flat".to_string(),
            my_pick: ids(&["fighter", "swordman", "jiangshi"]),
            enemy_pick: ids(&["ogre", "archer", "monk"]),
            ..Default::default()
        };
        let n = plain_notes(&update(&b, &role_tables(&flat), 7_100_001));
        let enemy = n.iter().find(|x| x.starts_with("상대 조합 ")).expect("enemy comp line");
        assert!(enemy.contains("탱커 0") && !enemy.contains("돌진"), "{}", enemy);
        assert!(!n.iter().any(|x| x.contains("앞라인이 없습니다")), "{:?}", n);

        let b = Board { teams: "t-comp-heal".to_string(), enemy_pick: ids(&["ogre", "priest"]), ..Default::default() };
        let n = plain_notes(&update(&b, &role_tables(&[("ogre", 1.8, 0.2), ("priest", 0.5, 2.0)]), 7_100_002));
        let enemy = n.iter().find(|x| x.starts_with("상대 조합 ")).expect("enemy comp line");
        assert!(enemy.contains("힐러 1") && !enemy.contains("고려하세요"), "{}", enemy);
    }

    /// stats TSV from (version, champ, lane, wins, games)
    fn stats(rows: &[(&str, &str, &str, u32, u32)]) -> String {
        let mut s = String::from("version\ttotal_match\tchampion\tposition\tbans\twins\tmatches\n");
        for (v, c, p, w, m) in rows {
            s.push_str(&format!("{}\t100\t{}\t{}\t0\t{}\t{}\n", v, c, p, w, m));
        }
        s
    }

    /// archer won 36 of 60 at Bottom last patch (κ40 posterior 0.56) and has no
    /// game yet in this one; monk keeps the current patch populated
    fn carry_tables(archer_now: Option<(u32, u32)>, chg: ChgInfo) -> engine::Tables {
        let mut rows = vec![
            ("2026.0.0", "archer", "Bottom", 36, 60),
            ("2026.0.0", "monk", "Support", 30, 60),
            ("2026.1.0", "monk", "Support", 5, 10),
        ];
        if let Some((w, m)) = archer_now {
            rows.push(("2026.1.0", "archer", "Bottom", w, m));
        }
        build_tables(&stats(&rows), &[], &chg, "")
    }

    fn archer_changed(dir: char, counts: (u32, u32), size: Option<f64>) -> ChgInfo {
        ChgInfo {
            anchor: Some("2026.0.0".to_string()),
            dirs: vec![("archer".to_string(), dir)],
            counts: vec![("archer".to_string(), counts.0, counts.1)],
            sizes: size.map(|s| vec![("archer".to_string(), s)]).unwrap_or_default(),
        }
    }

    /// E7b (v4.5.0): a changed champ starts from last patch's posterior moved by
    /// the patch size (0.56 − 0.0041 − 0.00072·40 = 0.527 → A), instead of
    /// restarting blank. The lane it played last patch keeps that value; one it
    /// never played gets no cell. With no game yet the letter is unsure.
    #[test]
    fn changed_champ_keeps_a_tier_from_its_last_patch() {
        let t = carry_tables(None, archer_changed('-', (0, 2), Some(-40.0)));
        assert_eq!(t.tier_display("archer", None), "A?");
        assert_eq!(t.tier_display("archer", Some(3)), "A?");
        assert_eq!(t.tier_of("archer", Some(0)), "");
    }

    /// the size sets how far the carried strength moves: −100 → 0.484 (B),
    /// +100 → 0.628 (S)
    #[test]
    fn patch_size_moves_the_carried_strength() {
        assert_eq!(carry_tables(None, archer_changed('-', (0, 5), Some(-100.0))).tier_of("archer", None), "B");
        assert_eq!(carry_tables(None, archer_changed('+', (5, 0), Some(100.0))).tier_of("archer", None), "S");
    }

    /// a note without numeric values moves the carry by the count nudge instead:
    /// a four-line buff on a 0.50 champ → 0.53 (A); reading it as size 0 would
    /// give 0.496 (B)
    #[test]
    fn note_without_sizes_moves_the_carry_by_the_count_nudge() {
        let rows = [("2026.0.0", "archer", "Bottom", 20, 40), ("2026.1.0", "monk", "Support", 5, 10)];
        let t = build_tables(&stats(&rows), &[], &archer_changed('+', (4, 0), None), "");
        assert_eq!(t.tier_of("archer", None), "A");
    }

    /// this patch's games pull toward the data at prior strength 80: 4 wins in 20
    /// on the 0.527 carry → 0.462 (C); the old κ40 restart read 0.40 (D)
    #[test]
    fn current_games_update_the_carry_at_strength_80() {
        let t = carry_tables(Some((4, 20)), archer_changed('-', (0, 2), Some(-40.0)));
        assert_eq!(t.tier_of("archer", None), "C");
    }

    /// the patch shift lives in the changed champ's prior now, so the ▲▼ mark
    /// itself adds nothing to a pick or ban score
    #[test]
    fn patch_marks_add_nothing_to_scores() {
        let t = engine::parse_tables_from_str(
            "C\tarcher\t0.030\t10\t0.03\nC\tmonk\t0.030\t10\t0.03\nC\togre\t0.030\t10\t0.03\n\
             CHG\tarcher\t+\t4\t0\nCHG\tmonk\t-\t0\t4",
        );
        for c in ["archer", "monk"] {
            assert_eq!(t.score_pick(c, &[], &[]).0, t.score_pick("ogre", &[], &[]).0, "{}", c);
            assert_eq!(t.score_ban(c, &[], &[], 0, 0, None).0, t.score_ban("ogre", &[], &[], 0, 0, None).0, "{}", c);
        }
    }

    /// without the patch nudge the prior part is the game AI's own valuation
    /// alone, so its reason tag names that (it read "패치/사전")
    #[test]
    fn ai_seed_reason_tag_names_the_ai_score() {
        let mut t = engine::parse_tables_from_str("POS\tvampire\t0\t0");
        t.set_ai_prior([("vampire".to_string(), 0.05)].into_iter().collect());
        assert_eq!(t.score_pick("vampire", &[], &[]).1, "AI 점수");
    }

    /// changed champs no longer restart blank, so the patch line must not say
    /// their stats were reset
    #[test]
    fn patch_line_says_changed_champs_carry_their_last_patch() {
        let chg = ChgInfo { dirs: vec![("archer".to_string(), '+')], ..Default::default() };
        let b = Board { teams: "t-patch-line".to_string(), ..Default::default() };
        let n = plain_notes(&update(&b, &build_tables("", &[], &chg, ""), 7_100_003));
        let line = n.iter().find(|x| x.starts_with("이번 패치 변경: ")).expect("patch line");
        assert!(line.contains("지난 패치 성적") && !line.contains("리셋"), "{}", line);
    }

    const MINE: [&str; 5] = ["fighter", "swordman", "jiangshi", "pole_warrior", "vampire"];
    const THEIRS: [&str; 5] = ["ogre", "archer", "monk", "ninja", "priest"];

    /// E7c: every champ pinned to lane i; mine carry the given lane deltas,
    /// theirs 0; M sets the data confidence
    fn winp_tables(mine: [f32; 5], m: usize) -> engine::Tables {
        let mut s = String::new();
        for l in 0..5 {
            s += &format!("POS\t{}\t{l}\t{l}\nCP\t{}\t{l}\t{}\t50\n", MINE[l], MINE[l], mine[l]);
            s += &format!("POS\t{}\t{l}\t{l}\nCP\t{}\t{l}\t0\t50\n", THEIRS[l], THEIRS[l]);
        }
        engine::parse_tables_from_str(&format!("{}M\t{}", s, m))
    }

    #[test]
    fn expected_win_rate_maps_the_lane_strength_gap() {
        // Δ = +0.10 at full confidence: σ(side·0.0619 + 1.975·0.10)
        let t = winp_tables([0.04, 0.03, 0.02, 0.01, 0.0], 400);
        let p = |side| engine::win_probability(&t, &MINE, &THEIRS, side).unwrap();
        assert!((p(Some(true)) - 0.5645).abs() < 1e-3, "blue {}", p(Some(true)));
        assert!((p(Some(false)) - 0.5339).abs() < 1e-3, "red {}", p(Some(false)));
        assert!((p(None) - 0.5492).abs() < 1e-3, "no side {}", p(None));
    }

    #[test]
    fn expected_win_rate_shrinks_with_thin_data() {
        // M = 100 → confidence 0.5: σ(0.0619 + 1.975·0.5·0.10)
        let t = winp_tables([0.04, 0.03, 0.02, 0.01, 0.0], 100);
        let p = engine::win_probability(&t, &MINE, &THEIRS, Some(true)).unwrap();
        assert!((p - 0.5401).abs() < 1e-3, "{}", p);
    }

    #[test]
    fn expected_win_rate_waits_for_both_full_teams() {
        let t = winp_tables([0.0; 5], 400);
        assert!(engine::win_probability(&t, &MINE, &THEIRS[..4], Some(true)).is_none());
        assert!(engine::win_probability(&t, &MINE[..4], &THEIRS, Some(true)).is_none());
    }

    #[test]
    fn swap_notes_state_the_expected_win_rate() {
        // no stats → no strength gap → the blue side term alone, σ(0.0619) ≈ 52%
        let b = Board { my_blue: Some(true), ..swap_board() };
        let n = plain_notes(&run("t-swap-winp", b, 0));
        assert!(n.iter().any(|x| x == "예상 승률(챔피언 강도 기준) 약 52%"), "{:?}", n);
    }

    #[test]
    fn swap_headline_fits_one_card_line() {
        // the note card wraps at 48 units (ui::cards NOTE_UNITS); a two-line headline
        // pushed a fearless set's remaining-champs note off the card (10-30 game)
        let o = run("t-swap-headline", swap_board(), 0);
        let w = markup::width(&o.notes[0]);
        assert!(w <= 48, "{} units: {}", w, markup::plain(&o.notes[0]));
    }

    #[test]
    fn notes_skip_the_win_rate_until_the_enemy_is_full() {
        let mut b = swap_board();
        b.enemy_pick.pop();
        let n = plain_notes(&run("t-swap-winp-4", b, 0));
        assert!(!n.iter().any(|x| x.contains("예상 승률")), "{:?}", n);
    }
}
