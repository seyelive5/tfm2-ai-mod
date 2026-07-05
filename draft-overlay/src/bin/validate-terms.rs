//! validate-terms — HONEST holdout test of each scoring term's predictive power.
//! v2 (2026-06-13, PLAN.md Phase 0): 5-fold CV (single 20% split on 668 matches
//! has ±4%p noise; CV evaluates every match exactly once → ±1.9%p), lane-aware
//! terms (CP), the new matchup axis (MU same-lane head-to-head, LP laning
//! dominance), and the PLANNED athlete axis (familiarity + lane-fit) so Phase 2
//! is validated BEFORE it is built. The replay dump stores each athlete's stat
//! snapshot AT MATCH TIME → no leakage for fit; familiarity is train-fold-built.
//!
//! v3.2 (2026-07-05, ALGO-FIXES S0-3): production-math base — the SHIPPED
//! patch-local state (current + prev·0.35·(1−cm/40), κ40/κ20 beta_post,
//! changed-champ prev-drop from the patch-note dump), expanding in play order
//! (replay `id`, verified monotone with version) so what gets validated is the
//! model we actually ship, not the harness's pooled-history approximation.
//!
//! Usage: validate-terms [--legacy] [--notes=patch_notes_dump.txt] <match_replays.debug.txt> [draft_weights.tsv] [champion_patch_statistics.tsv] [ai_intrinsic.tsv]
//!   draft_weights.tsv               → T rows (taxonomy) for the comp term
//!   champion_patch_statistics.tsv   → LP (gold_line_phase z) — CAVEAT: career-
//!                                     cumulative (includes test matches); champ
//!                                     laning style is near-stationary, treat the
//!                                     LP number as an upper bound.
//!   ai_intrinsic.tsv                → champ<TAB>score: the GAME draft-AI's
//!                                     empty-board valuations (observed via the
//!                                     score hook). CAVEAT: the model state has
//!                                     learned from these same matches → treat as
//!                                     an upper bound like LP.

use std::collections::{HashMap, HashSet};

// ---------- balanced-scan parse ----------
fn read_balanced(b: &[u8], open_idx: usize, open: u8, close: u8) -> Option<(usize, usize)> {
    if open_idx >= b.len() || b[open_idx] != open {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open_idx;
    while i < b.len() {
        if b[i] == open {
            depth += 1;
        } else if b[i] == close {
            depth -= 1;
            if depth == 0 {
                return Some((open_idx, i));
            }
        }
        i += 1;
    }
    None
}
fn find_from(h: &[u8], n: &[u8], from: usize) -> Option<usize> {
    if n.is_empty() || from >= h.len() {
        return None;
    }
    let last = h.len().checked_sub(n.len())?;
    (from..=last).find(|&i| &h[i..i + n.len()] == n)
}
fn parse_bool(block: &str, name: &str) -> Option<bool> {
    let key = format!("{}: ", name);
    let rest = &block[block.find(&key)? + key.len()..];
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}
fn quoted(block: &str, field: &str) -> Option<String> {
    let key = format!("{}: \"", field);
    let rest = &block[block.find(&key)? + key.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
fn raw_field(block: &str, field: &str) -> Option<String> {
    let key = format!("{}: ", field);
    let rest = &block[block.find(&key)? + key.len()..];
    let end = rest.find([',', '\n']).unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}
fn num_field(block: &str, field: &str) -> Option<f64> {
    raw_field(block, field)?.parse().ok()
}
fn extract_array<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{}: [", name);
    let kidx = block.find(&key)?;
    let bracket = kidx + key.len() - 1;
    let (s, e) = read_balanced(block.as_bytes(), bracket, b'[', b']')?;
    Some(&block[s..=e])
}

const LANE_KEYS: [&str; 5] = ["top", "jungle", "mid", "bottom", "support"];
fn lane_of(p: &str) -> usize {
    match p {
        "Top" => 0,
        "Jungle" => 1,
        "Mid" => 2,
        "Bottom" => 3,
        "Support" => 4,
        _ => 9,
    }
}

struct Ath {
    champ: String,
    lane: usize, // 9 = unknown
    aid: u64,
    /// the athlete's proficiency stat for THEIR assigned lane, AT MATCH TIME.
    fit: f64,
}
struct Match {
    blue: Vec<Ath>,
    red: Vec<Ath>,
    blue_win: bool,
    /// patch version this replay was played on (MatchReplayData.version, e.g.
    /// "2026.3.1") — drives the causal rolling-origin split.
    version: String,
    /// chronological match id (top-level MatchReplayData.id; verified: sorting by
    /// id makes version monotone, 0 violations) — play order for expanding tables.
    id: u64,
}

fn parse_team(arr: &str) -> Vec<Ath> {
    let b = arr.as_bytes();
    let needle = b"MatchReplayAthlete {";
    let mut out = Vec::new();
    let mut off = 0;
    while let Some(start) = find_from(b, needle, off) {
        let brace = start + needle.len() - 1;
        let (s, e) = match read_balanced(b, brace, b'{', b'}') {
            Some(v) => v,
            None => break,
        };
        let blk = &arr[s..=e];
        off = e + 1;
        let champ = match quoted(blk, "champion") {
            Some(c) => c,
            None => continue,
        };
        let lane = raw_field(blk, "position").map(|p| lane_of(&p)).unwrap_or(9);
        let aid = num_field(blk, "athlete_id").unwrap_or(0.0) as u64;
        let fit = if lane < 5 { num_field(blk, LANE_KEYS[lane]).unwrap_or(0.0) } else { 0.0 };
        out.push(Ath { champ, lane, aid, fit });
    }
    out
}

fn parse_matches(text: &str) -> Vec<Match> {
    let b = text.as_bytes();
    let needle = b"MatchReplayData {";
    let mut out = Vec::new();
    let mut off = 0;
    while let Some(start) = find_from(b, needle, off) {
        let brace = start + needle.len() - 1;
        let (s, e) = match read_balanced(b, brace, b'{', b'}') {
            Some(v) => v,
            None => break,
        };
        let block = &text[s..=e];
        off = e + 1;
        let blue_win = match parse_bool(block, "blue_team_win") {
            Some(v) => v,
            None => continue,
        };
        let blue = extract_array(block, "blue_team").map(parse_team).unwrap_or_default();
        let red = extract_array(block, "red_team").map(parse_team).unwrap_or_default();
        if blue.len() < 2 || red.len() < 2 {
            continue;
        }
        let version = quoted(block, "version").unwrap_or_default();
        // top-level `id:` is the block's first field, so find() hits it before any
        // athlete_id (which contains "id: " as a substring but appears later).
        let id = num_field(block, "id").unwrap_or(0.0) as u64;
        out.push(Match { blue, red, blue_win, version, id });
    }
    out
}

// ---------- taxonomy (T rows) for comp ----------
struct Taxon {
    category: String,
    tags: Vec<String>,
}
fn parse_taxonomy(tsv: &str) -> HashMap<String, Taxon> {
    let mut m = HashMap::new();
    for line in tsv.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.first() == Some(&"T") && f.len() >= 5 {
            m.insert(
                f[1].to_string(),
                Taxon { category: f[2].to_string(), tags: f[4].split(',').map(|s| s.to_string()).collect() },
            );
        }
    }
    m
}

// ---------- LP (laning dominance z) from the cumulative stats TSV ----------
fn parse_lane_dom(tsv: &str) -> HashMap<(String, usize), f64> {
    let mut agg: HashMap<(String, usize), (f64, f64)> = HashMap::new(); // (gold, matches)
    for line in tsv.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 19 {
            continue;
        }
        let lane = lane_of(f[3]);
        if lane > 4 {
            continue;
        }
        let gold: f64 = f[17].parse().unwrap_or(0.0);
        let m: f64 = f[6].parse().unwrap_or(0.0);
        let e = agg.entry((f[2].to_string(), lane)).or_insert((0.0, 0.0));
        e.0 += gold;
        e.1 += m;
    }
    let mut out = HashMap::new();
    for lane in 0..5usize {
        let cells: Vec<(&(String, usize), f64)> =
            agg.iter().filter(|((_, l), (_, m))| *l == lane && *m >= 5.0).map(|(k, (g, m))| (k, g / m)).collect();
        if cells.len() < 3 {
            continue;
        }
        let n = cells.len() as f64;
        let mean = cells.iter().map(|(_, v)| v).sum::<f64>() / n;
        let sd = (cells.iter().map(|(_, v)| (v - mean) * (v - mean)).sum::<f64>() / n).sqrt();
        if sd < 1e-9 {
            continue;
        }
        for (k, v) in cells {
            out.insert(k.clone(), ((v - mean) / sd).clamp(-2.5, 2.5));
        }
    }
    out
}

// ---------- WL accumulator ----------
#[derive(Default, Clone, Copy)]
struct WL {
    w: f64,
    g: f64,
}
impl WL {
    fn rec(&mut self, won: bool) {
        self.g += 1.0;
        if won {
            self.w += 1.0;
        }
    }
    fn delta(&self, k: f64) -> f64 {
        (self.w + k * 0.5) / (self.g + k) - 0.5
    }
    fn delta_p(&self, k: f64, prior: f64) -> f64 {
        (self.w + k * prior) / (self.g + k) - 0.5
    }
}

// ---------- production scoring math (S0-3) — mirrors tfm2_db_export/src/recommender.rs:23-151 ----------
const PROD_PREV_W: f64 = 0.35; // PREV_PATCH_WEIGHT
const PROD_CONF_CHAMP: f64 = 40.0; // per-champ current-patch matches at which the prev blend fades out
const PROD_KAPPA_CHAMP: f64 = 40.0; // KAPPA_CHAMP
const PROD_KAPPA_LANE: f64 = 20.0; // KAPPA_LANE

fn beta_mean(w: f64, m: f64, prior_mean: f64, kappa: f64) -> f64 {
    let pm = prior_mean.clamp(0.001, 0.999);
    let a = w.max(0.0) + pm * kappa;
    let b = (m - w).max(0.0) + (1.0 - pm) * kappa;
    a / (a + b)
}

/// champ posterior MEAN under the shipped patch-local state: current-version WL
/// + previous-version WL at 0.35·(1−cm/40) (cm = champ's current games so far);
/// champs in this version's patch note drop the prev blend (stale — restart).
fn prod_champ_post(c: &str, cur: &HashMap<&str, WL>, prev: &HashMap<&str, WL>, changed: &HashSet<String>) -> f64 {
    let cu = cur.get(c).copied().unwrap_or_default();
    let pv = if changed.contains(c) { WL::default() } else { prev.get(c).copied().unwrap_or_default() };
    let pw = PROD_PREV_W * (1.0 - (cu.g / PROD_CONF_CHAMP).min(1.0));
    beta_mean(cu.w + pw * pv.w, cu.g + pw * pv.g, 0.5, PROD_KAPPA_CHAMP)
}

/// lane delta under the shipped state: κ20 beta with the champ's own posterior
/// mean as prior; the prev fade uses CHAMP-level current games (recommender.rs
/// keys cur_m by champ, not (champ,lane)); an empty lane cell falls back to champ.
fn prod_lane_delta(
    a: &Ath,
    cur_c: &HashMap<&str, WL>,
    prev_c: &HashMap<&str, WL>,
    cur_cp: &HashMap<(&str, usize), WL>,
    prev_cp: &HashMap<(&str, usize), WL>,
    changed: &HashSet<String>,
) -> f64 {
    let cmean = prod_champ_post(&a.champ, cur_c, prev_c, changed);
    if a.lane < 5 {
        let cu = cur_cp.get(&(a.champ.as_str(), a.lane)).copied().unwrap_or_default();
        let pv = if changed.contains(&a.champ) {
            WL::default()
        } else {
            prev_cp.get(&(a.champ.as_str(), a.lane)).copied().unwrap_or_default()
        };
        let champ_cm = cur_c.get(a.champ.as_str()).map_or(0.0, |w| w.g);
        let pw = PROD_PREV_W * (1.0 - (champ_cm / PROD_CONF_CHAMP).min(1.0));
        let (w, m) = (cu.w + pw * pv.w, cu.g + pw * pv.g);
        if m > 0.0 {
            return beta_mean(w, m, cmean, PROD_KAPPA_LANE) - 0.5;
        }
    }
    cmean - 0.5
}

/// `=== version V ===` sections + `[champ]` line prefixes from patch_notes_dump.txt
/// → per-version changed-champ sets (the authored reset rule, same as authored_chg).
fn parse_patch_notes(text: &str) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let lt = line.trim();
        if let Some(rest) = lt.strip_prefix("=== version ") {
            let v = rest.split_whitespace().next().unwrap_or("").to_string();
            out.entry(v.clone()).or_default();
            cur = Some(v);
        } else if let (Some(v), Some(rest)) = (cur.as_ref(), lt.strip_prefix('[')) {
            if let Some(end) = rest.find(']') {
                out.get_mut(v).unwrap().insert(rest[..end].to_string());
            }
        }
    }
    out
}

/// per-match term values (blue minus red), computed under that match's CV fold.
#[derive(Default, Clone, Copy)]
struct Terms {
    base: f64,      // champ winrate (lane-agnostic), κ8
    base_lane: f64, // per-(champ,lane) winrate, κ20 prior=champ mean, fallback base
    base40: f64,      // pooled-history champ at κ40 (the κ8-vs-κ40 ship report, S0-3)
    base_lane40: f64, // pooled-history lane on the κ40 champ prior
    /// production-math champ base (S0-3): patch-local cur + 0.35·fade·prev, κ40 beta.
    base_prod: f64,
    /// production-math lane base — the SHIPPED model's true causal anchor.
    base_lane_prod: f64,
    syn: f64,       // legacy same-team pair
    cnt: f64,       // legacy cross-team pair (all-vs-all)
    mu: f64,        // same-LANE head-to-head, κ12 (the new matchup axis)
    lp: f64,        // laning-dominance z diff per lane (CAVEAT: cumulative source)
    comp: f64,      // taxonomy completeness diff
    fam: f64,       // athlete-champ familiarity (train (athlete,champ) WL, κ4)
    /// fam CENTERED on the athlete's overall winrate — strips the team-strength
    /// confound; what remains is the ACTIONABLE part ("this athlete is better on
    /// THIS champ than their usual"), the only part a pick recommendation can use.
    fam_c: f64,
    /// games-based familiarity (no win info): does the athlete HAVE games on the
    /// champ (vs first-time)? log1p(games) diff — the "most champion" proxy.
    fam_g: f64,
    /// fam_g centered on the athlete's own mean log-games across their champs —
    /// strips career length (veteran-heavy teams) from the comfort signal.
    fam_gc: f64,
    fit: f64,       // athlete lane proficiency diff (as-of-match stat, /100)
    aip: f64,       // game-AI intrinsic valuation diff (empty-board scan scores)
    /// CV group this record was scored under (origin index for rolling, fold for legacy).
    group: usize,
    win: bool,
}

fn main() {
    // --legacy = the old non-causal i%5 interleave (kept for comparison); default is
    // the causal rolling-origin split (train on strictly-earlier patches only).
    let raw: Vec<String> = std::env::args().collect();
    let legacy = raw.iter().any(|a| a == "--legacy");
    // --notes=<patch_notes_dump.txt> → per-version changed-champ sets for the S0-3
    // production base (stale-blend-drop). Optional: absent = no resets (reported).
    let notes_path: Option<String> = raw.iter().find_map(|a| a.strip_prefix("--notes=").map(str::to_string));
    let args: Vec<String> = raw.into_iter().filter(|a| !a.starts_with("--")).collect();
    if args.len() < 2 {
        eprintln!("usage: validate-terms [--legacy] [--notes=patch_notes_dump.txt] <match_replays.debug.txt> [draft_weights.tsv] [champion_patch_statistics.tsv] [ai_intrinsic.tsv]");
        std::process::exit(2);
    }
    let matches = parse_matches(&std::fs::read_to_string(&args[1]).unwrap_or_default());
    let taxon = if args.len() >= 3 {
        parse_taxonomy(&std::fs::read_to_string(&args[2]).unwrap_or_default())
    } else {
        HashMap::new()
    };
    let lane_dom = if args.len() >= 4 {
        parse_lane_dom(&std::fs::read_to_string(&args[3]).unwrap_or_default())
    } else {
        HashMap::new()
    };
    let ai_intr: HashMap<String, f64> = if args.len() >= 5 {
        std::fs::read_to_string(&args[4])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (c, v) = l.split_once('\t')?;
                Some((c.to_string(), v.trim().parse().ok()?))
            })
            .collect()
    } else {
        HashMap::new()
    };
    let notes: HashMap<String, HashSet<String>> = notes_path
        .as_deref()
        .map(|p| parse_patch_notes(&std::fs::read_to_string(p).unwrap_or_default()))
        .unwrap_or_default();
    let n_pos = matches
        .iter()
        .filter(|m| m.blue.iter().all(|a| a.lane < 5) && m.red.iter().all(|a| a.lane < 5))
        .count();

    let comp_score = |team: &[Ath]| -> f64 {
        let has = |tag: &str| team.iter().any(|a| taxon.get(&a.champ).map_or(false, |t| t.tags.iter().any(|x| x == tag)));
        let has_cat = |cat: &str| team.iter().any(|a| taxon.get(&a.champ).map_or(false, |t| t.category == cat));
        let mut s = 0.0;
        if has("Tank") {
            s += 1.0;
        } else if has("Frontline") {
            s += 0.5;
        }
        if has("CC") {
            s += 0.7;
        }
        if has("Heal") || has("Shield") {
            s += 0.6;
        }
        if has("Backline") || has_cat("Range") {
            s += 0.6;
        }
        s
    };

    // ---- CV: every match scored once, under tables built from OTHER data. Default =
    // rolling-origin (train on strictly-EARLIER patch versions → causal, matches
    // deployment where only the past exists); --legacy = the old i%5 interleave, which
    // trains on each test match's FUTURE and inflates every number (2026-07-05 review S0).
    let vkey = |v: &str| -> Vec<u32> { v.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect() };
    let mut vsorted: Vec<Vec<u32>> = matches.iter().map(|m| vkey(&m.version)).collect();
    vsorted.sort();
    vsorted.dedup();
    let groups: Vec<usize> = matches
        .iter()
        .enumerate()
        .map(|(i, m)| if legacy { i % 5 } else { vsorted.binary_search(&vkey(&m.version)).unwrap_or(0) })
        .collect();
    let n_groups = if legacy { 5 } else { vsorted.len() };
    // train membership: legacy = all OTHER folds; rolling-origin = only EARLIER groups.
    let is_train = |i: usize, g: usize| if legacy { groups[i] != g } else { groups[i] < g };
    let mut recs: Vec<Terms> = Vec::with_capacity(matches.len());
    for g in 0..n_groups {
        // rolling-origin's earliest version has no earlier data → nothing to train on.
        if !(0..matches.len()).any(|i| is_train(i, g)) {
            continue;
        }
        let mut champ: HashMap<&str, WL> = HashMap::new();
        let mut cp: HashMap<(&str, usize), WL> = HashMap::new();
        let mut syn: HashMap<(String, String), WL> = HashMap::new();
        let mut cnt: HashMap<(String, String), WL> = HashMap::new();
        let mut mu: HashMap<(String, String), WL> = HashMap::new();
        let mut fam: HashMap<(u64, &str), WL> = HashMap::new();
        let mut ath_all: HashMap<u64, WL> = HashMap::new();
        let pair = |a: &str, b: &str| if a < b { (a.to_string(), b.to_string()) } else { (b.to_string(), a.to_string()) };
        for (i, m) in matches.iter().enumerate() {
            if !is_train(i, g) {
                continue; // not in this origin/fold's training set
            }
            for (team, won) in [(&m.blue, m.blue_win), (&m.red, !m.blue_win)] {
                for a in team {
                    champ.entry(&a.champ).or_default().rec(won);
                    if a.lane < 5 {
                        cp.entry((&a.champ, a.lane)).or_default().rec(won);
                    }
                    fam.entry((a.aid, &a.champ)).or_default().rec(won);
                    ath_all.entry(a.aid).or_default().rec(won);
                }
                for x in 0..team.len() {
                    for y in (x + 1)..team.len() {
                        syn.entry(pair(&team[x].champ, &team[y].champ)).or_default().rec(won);
                    }
                }
            }
            for b in &m.blue {
                for r in &m.red {
                    cnt.entry((b.champ.clone(), r.champ.clone())).or_default().rec(m.blue_win);
                    cnt.entry((r.champ.clone(), b.champ.clone())).or_default().rec(!m.blue_win);
                    if b.lane < 5 && b.lane == r.lane {
                        mu.entry((b.champ.clone(), r.champ.clone())).or_default().rec(m.blue_win);
                        mu.entry((r.champ.clone(), b.champ.clone())).or_default().rec(!m.blue_win);
                    }
                }
            }
        }
        // per-athlete mean log-games across their played champs (for fam_gc)
        let mut ath_lg: HashMap<u64, (f64, f64)> = HashMap::new(); // (sum log1p g, n champs)
        for ((aid, _), w) in &fam {
            let e = ath_lg.entry(*aid).or_insert((0.0, 0.0));
            e.0 += (1.0 + w.g).ln();
            e.1 += 1.0;
        }
        // score this fold's held-out matches
        let cdelta = |c: &str| champ.get(c).map_or(0.0, |w| w.delta(8.0));
        let lane_delta = |a: &Ath| -> f64 {
            if a.lane < 5 {
                if let Some(w) = cp.get(&(a.champ.as_str(), a.lane)) {
                    let prior = cdelta(&a.champ) + 0.5;
                    return w.delta_p(20.0, prior);
                }
            }
            cdelta(&a.champ)
        };
        // κ40 pooled variants — the "does κ8 validation speak for the κ40 ship?" report (S0-3/F7/F11)
        let cdelta40 = |c: &str| champ.get(c).map_or(0.0, |w| w.delta(40.0));
        let lane_delta40 = |a: &Ath| -> f64 {
            if a.lane < 5 {
                if let Some(w) = cp.get(&(a.champ.as_str(), a.lane)) {
                    return w.delta_p(20.0, cdelta40(&a.champ) + 0.5);
                }
            }
            cdelta40(&a.champ)
        };
        // production patch-local state (S0-3): memory = current version (expanding in
        // play order) + previous version at 0.35·(1−cm/40); older versions FORGOTTEN,
        // changed champs drop their prev blend — mirrors recommender.rs patch_current.
        let mut prev_c: HashMap<&str, WL> = HashMap::new();
        let mut prev_cp: HashMap<(&str, usize), WL> = HashMap::new();
        if !legacy && g >= 1 {
            for (i, m) in matches.iter().enumerate() {
                if groups[i] != g - 1 {
                    continue;
                }
                for (team, won) in [(&m.blue, m.blue_win), (&m.red, !m.blue_win)] {
                    for a in team {
                        prev_c.entry(&a.champ).or_default().rec(won);
                        if a.lane < 5 {
                            prev_cp.entry((&a.champ, a.lane)).or_default().rec(won);
                        }
                    }
                }
            }
        }
        let empty_changed: HashSet<String> = HashSet::new();
        let changed: &HashSet<String> = if legacy {
            &empty_changed
        } else {
            let label = vsorted[g].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".");
            notes.get(&label).unwrap_or(&empty_changed)
        };
        let mut cur_c: HashMap<&str, WL> = HashMap::new();
        let mut cur_cp: HashMap<(&str, usize), WL> = HashMap::new();
        let mut test_idx: Vec<usize> = (0..matches.len()).filter(|&i| groups[i] == g).collect();
        test_idx.sort_by_key(|&i| matches[i].id); // play order — production tables must expand causally
        for &i in &test_idx {
            let m = &matches[i];
            let mut t = Terms { win: m.blue_win, group: g, ..Default::default() };
            let side = |team: &[Ath]| -> (f64, f64, f64, f64, f64, f64, f64) {
                let b: f64 = team.iter().map(|a| cdelta(&a.champ)).sum();
                let bl: f64 = team.iter().map(lane_delta).sum();
                let mut sy = 0.0;
                for x in 0..team.len() {
                    for y in (x + 1)..team.len() {
                        sy += syn.get(&pair(&team[x].champ, &team[y].champ)).map_or(0.0, |w| w.delta(6.0));
                    }
                }
                let f: f64 = team.iter().map(|a| fam.get(&(a.aid, a.champ.as_str())).map_or(0.0, |w| w.delta(4.0))).sum();
                // centered: champ WL minus the athlete's OVERALL WL (team-strength stripped)
                let fc: f64 = team
                    .iter()
                    .map(|a| {
                        let cw = fam.get(&(a.aid, a.champ.as_str())).copied().unwrap_or_default();
                        let aw = ath_all.get(&a.aid).copied().unwrap_or_default();
                        cw.delta_p(4.0, aw.delta(8.0) + 0.5)
                    })
                    .sum();
                let fg: f64 = team
                    .iter()
                    .map(|a| (1.0 + fam.get(&(a.aid, a.champ.as_str())).map_or(0.0, |w| w.g)).ln())
                    .sum();
                let fgc: f64 = team
                    .iter()
                    .map(|a| {
                        let lg = (1.0 + fam.get(&(a.aid, a.champ.as_str())).map_or(0.0, |w| w.g)).ln();
                        let mean = ath_lg.get(&a.aid).map_or(0.0, |(s, n)| if *n > 0.0 { s / n } else { 0.0 });
                        lg - mean
                    })
                    .sum();
                (b, bl, sy, f, fc, fg, fgc)
            };
            let (bb, bbl, bsy, bf, bfc, bfg, bfgc) = side(&m.blue);
            let (rb, rbl, rsy, rf, rfc, rfg, rfgc) = side(&m.red);
            t.base = bb - rb;
            t.base_lane = bbl - rbl;
            t.syn = bsy - rsy;
            t.fam = bf - rf;
            t.fam_c = bfc - rfc;
            t.fam_g = bfg - rfg;
            t.fam_gc = bfgc - rfgc;
            for b in &m.blue {
                for r in &m.red {
                    t.cnt += cnt.get(&(b.champ.clone(), r.champ.clone())).map_or(0.0, |w| w.delta(6.0));
                    t.cnt -= cnt.get(&(r.champ.clone(), b.champ.clone())).map_or(0.0, |w| w.delta(6.0));
                    if b.lane < 5 && b.lane == r.lane {
                        t.mu += mu.get(&(b.champ.clone(), r.champ.clone())).map_or(0.0, |w| w.delta(12.0));
                        t.mu -= mu.get(&(r.champ.clone(), b.champ.clone())).map_or(0.0, |w| w.delta(12.0));
                        t.lp += lane_dom.get(&(b.champ.clone(), b.lane)).copied().unwrap_or(0.0)
                            - lane_dom.get(&(r.champ.clone(), r.lane)).copied().unwrap_or(0.0);
                    }
                }
            }
            t.comp = comp_score(&m.blue) - comp_score(&m.red);
            t.fit = (m.blue.iter().map(|a| a.fit).sum::<f64>() - m.red.iter().map(|a| a.fit).sum::<f64>()) / 100.0;
            let aips = |team: &[Ath]| -> f64 {
                team.iter().map(|a| ai_intr.get(&a.champ).copied().unwrap_or(0.0)).sum()
            };
            t.aip = aips(&m.blue) - aips(&m.red);
            t.base40 = m.blue.iter().map(|a| cdelta40(&a.champ)).sum::<f64>()
                - m.red.iter().map(|a| cdelta40(&a.champ)).sum::<f64>();
            t.base_lane40 =
                m.blue.iter().map(lane_delta40).sum::<f64>() - m.red.iter().map(lane_delta40).sum::<f64>();
            if !legacy {
                let prod_c = |team: &[Ath]| -> f64 {
                    team.iter().map(|a| prod_champ_post(&a.champ, &cur_c, &prev_c, changed) - 0.5).sum()
                };
                let prod_l = |team: &[Ath]| -> f64 {
                    team.iter().map(|a| prod_lane_delta(a, &cur_c, &prev_c, &cur_cp, &prev_cp, changed)).sum()
                };
                t.base_prod = prod_c(&m.blue) - prod_c(&m.red);
                t.base_lane_prod = prod_l(&m.blue) - prod_l(&m.red);
            }
            recs.push(t);
            if !legacy {
                // ingest AFTER scoring: the production tables see only strictly-earlier matches
                for (team, won) in [(&m.blue, m.blue_win), (&m.red, !m.blue_win)] {
                    for a in team {
                        cur_c.entry(&a.champ).or_default().rec(won);
                        if a.lane < 5 {
                            cur_cp.entry((&a.champ, a.lane)).or_default().rec(won);
                        }
                    }
                }
            }
        }
    }

    let n = recs.len().max(1) as f64;
    let acc = |f: &dyn Fn(&Terms) -> f64| -> f64 {
        let mut c = 0.0;
        for t in &recs {
            let s = f(t);
            if s.abs() < 1e-12 {
                c += 0.5;
            } else if (s > 0.0) == t.win {
                c += 1.0;
            }
        }
        c / n * 100.0
    };
    let blue_rate = recs.iter().filter(|t| t.win).count() as f64 / n * 100.0;

    println!(
        "=== term validation v3 ({}) ===",
        if legacy { "LEGACY i%5 interleave — NON-CAUSAL, inflated" } else { "rolling-origin — causal, deployment-realistic" }
    );
    println!(
        "matches scored: {} of {} ({} with full position data){} · noise 1sd ≈ ±{:.1}%p",
        recs.len(),
        matches.len(),
        n_pos,
        if legacy { String::new() } else { format!(" · {} patch origins", n_groups) },
        50.0 / n.sqrt()
    );
    println!("baseline always-blue: {:.1}%\n", blue_rate);

    println!("STANDALONE:");
    for (name, f) in [
        ("base      (champ winrate)", (&|t: &Terms| t.base) as &dyn Fn(&Terms) -> f64),
        ("base_lane (per-lane winrate)", &|t: &Terms| t.base_lane),
        ("syn       (legacy pair)", &|t: &Terms| t.syn),
        ("cnt       (legacy all-vs-all)", &|t: &Terms| t.cnt),
        ("mu        (same-lane h2h) ★new", &|t: &Terms| t.mu),
        ("lp        (laning z) ★new/caveat", &|t: &Terms| t.lp),
        ("comp      (taxonomy)", &|t: &Terms| t.comp),
        ("fam       (athlete-champ) ★ph2", &|t: &Terms| t.fam),
        ("fam_c     (centered, no team confound)", &|t: &Terms| t.fam_c),
        ("fam_g     (games-only familiarity)", &|t: &Terms| t.fam_g),
        ("fam_gc    (career-centered comfort)", &|t: &Terms| t.fam_gc),
        ("fit       (athlete lane stat) ★ph2", &|t: &Terms| t.fit),
        ("aip       (game-AI intrinsic) ★caveat", &|t: &Terms| t.aip),
    ] {
        println!("  {:<38}: {:.1}%", name, acc(f));
    }

    println!("\nINCREMENTAL over base_lane (scale sweep; >noise lift = earns weight):");
    let anchor = |t: &Terms| t.base_lane;
    println!("  base_lane alone                  : {:.1}%", acc(&anchor));
    for (name, term, scales) in [
        ("mu", (&|t: &Terms| t.mu) as &dyn Fn(&Terms) -> f64, [0.25, 0.5, 1.0, 2.0]),
        ("lp", &|t: &Terms| t.lp, [0.004, 0.008, 0.016, 0.032]),
        ("cnt", &|t: &Terms| t.cnt, [0.05, 0.1, 0.15, 0.3]),
        ("syn", &|t: &Terms| t.syn, [0.05, 0.1, 0.15, 0.3]),
        ("comp", &|t: &Terms| t.comp, [0.01, 0.02, 0.04, 0.08]),
        ("fam", &|t: &Terms| t.fam, [0.25, 0.5, 1.0, 2.0]),
        ("famc", &|t: &Terms| t.fam_c, [0.25, 0.5, 1.0, 2.0]),
        ("famg", &|t: &Terms| t.fam_g, [0.02, 0.04, 0.08, 0.16]),
        ("famgc", &|t: &Terms| t.fam_gc, [0.02, 0.04, 0.08, 0.16]),
        ("fit", &|t: &Terms| t.fit, [0.02, 0.05, 0.1, 0.2]),
        ("aip", &|t: &Terms| t.aip, [0.25, 0.5, 1.0, 2.0]),
    ] {
        let mut line = format!("  base_lane + s*{:<4}", name);
        for s in scales {
            line.push_str(&format!("  s={:<5}: {:.1}%", s, acc(&|t: &Terms| anchor(t) + s * term(t))));
        }
        println!("{}", line);
    }

    println!("\nCOMBOS (base anchor too — base beat base_lane standalone):");
    for (name, f) in [
        ("base_lane + 0.5*mu", (&|t: &Terms| t.base_lane + 0.5 * t.mu) as &dyn Fn(&Terms) -> f64),
        ("base_lane + 0.5*fam", &|t: &Terms| t.base_lane + 0.5 * t.fam),
        ("base      + 0.5*fam", &|t: &Terms| t.base + 0.5 * t.fam),
        ("base      + 0.5*fam_c", &|t: &Terms| t.base + 0.5 * t.fam_c),
        ("base      + 0.5*fam + 0.15*syn", &|t: &Terms| t.base + 0.5 * t.fam + 0.15 * t.syn),
        ("base      + 0.5*fam + 0.05*fit", &|t: &Terms| t.base + 0.5 * t.fam + 0.05 * t.fit),
        ("base+blend(0.5*lane) + 0.5*fam", &|t: &Terms| {
            0.5 * (t.base + t.base_lane) + 0.5 * t.fam
        }),
        ("base + 0.5*fam + 0.15*syn + 0.05*fit + 0.008*lp", &|t: &Terms| {
            t.base + 0.5 * t.fam + 0.15 * t.syn + 0.05 * t.fit + 0.008 * t.lp
        }),
        ("base_lane + 0.08*famg", &|t: &Terms| t.base_lane + 0.08 * t.fam_g),
        ("base_lane + 0.08*famgc", &|t: &Terms| t.base_lane + 0.08 * t.fam_gc),
        ("base_lane + 0.08*famg + 0.25*fam_c", &|t: &Terms| {
            t.base_lane + 0.08 * t.fam_g + 0.25 * t.fam_c
        }),
        ("base_lane + 0.08*famg + 0.25*famc + 0.05*fit", &|t: &Terms| {
            t.base_lane + 0.08 * t.fam_g + 0.25 * t.fam_c + 0.05 * t.fit
        }),
        ("base_lane + 0.5*aip", &|t: &Terms| t.base_lane + 0.5 * t.aip),
        ("base_lane + 0.08*famg + 0.5*aip", &|t: &Terms| {
            t.base_lane + 0.08 * t.fam_g + 0.5 * t.aip
        }),
        ("aip + 0.08*famg (AI + athlete only)", &|t: &Terms| t.aip + 0.08 * t.fam_g),
        ("kitchen sink (yesterday's engine shape)", &|t: &Terms| {
            t.base_lane + 0.5 * t.mu + 0.008 * t.lp + 0.15 * t.syn + 0.15 * t.cnt + 0.02 * t.comp + 0.5 * t.fam + 0.05 * t.fit
        }),
    ] {
        println!("  {:<48}: {:.1}%", name, acc(f));
    }

    // ---- PER-ORIGIN breakdown: the leakage-vs-starvation discriminator (2026-07-05).
    // The rolling-origin famg collapse has two candidate explanations: (a) the legacy
    // i%5 lift was future-volume leakage, or (b) early origins simply train on too
    // little data. They separate at LATE origins (train = thousands of matches):
    // leakage predicts Δfamg stays ≈0/negative at every origin regardless of train
    // size; starvation predicts Δfamg recovering positive as train grows.
    let acc_in = |set: &[usize], f: &dyn Fn(&Terms) -> f64| -> f64 {
        let (mut c, mut k) = (0.0, 0usize);
        for t in recs.iter().filter(|t| set.contains(&t.group)) {
            k += 1;
            let s = f(t);
            if s.abs() < 1e-12 {
                c += 0.5;
            } else if (s > 0.0) == t.win {
                c += 1.0;
            }
        }
        if k > 0 { c / k as f64 * 100.0 } else { 50.0 }
    };
    println!("\nPER-ORIGIN (Δ = accuracy minus base_lane, within that origin's test set):");
    println!(
        "  {:<9} {:>5} {:>5} | {:>7} {:>7} {:>8} {:>8} {:>8} | {:>5}",
        "origin", "train", "test", "base_ln", "famg", "Δ.02*fg", "Δ.08*fg", "Δ.25*fc", "±1sd"
    );
    let mut scored_groups: Vec<usize> = Vec::new();
    for g in 0..n_groups {
        let k = recs.iter().filter(|t| t.group == g).count();
        if k == 0 {
            continue;
        }
        scored_groups.push(g);
        let train_n = (0..matches.len()).filter(|&i| is_train(i, g)).count();
        let label = if legacy {
            format!("fold{}", g)
        } else {
            vsorted[g].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".")
        };
        let set = [g];
        let bl = acc_in(&set, &|t: &Terms| t.base_lane);
        println!(
            "  {:<9} {:>5} {:>5} | {:>6.1}% {:>6.1}% {:>+8.1} {:>+8.1} {:>+8.1} | {:>5.1}",
            label,
            train_n,
            k,
            bl,
            acc_in(&set, &|t: &Terms| t.fam_g),
            acc_in(&set, &|t: &Terms| t.base_lane + 0.02 * t.fam_g) - bl,
            acc_in(&set, &|t: &Terms| t.base_lane + 0.08 * t.fam_g) - bl,
            acc_in(&set, &|t: &Terms| t.base_lane + 0.25 * t.fam_c) - bl,
            50.0 / (k as f64).sqrt()
        );
    }
    if !legacy && scored_groups.len() >= 4 {
        let late: Vec<usize> = scored_groups[scored_groups.len() - 3..].to_vec();
        let early: Vec<usize> = scored_groups[..scored_groups.len() - 3].to_vec();
        let min_train =
            late.iter().map(|&g| (0..matches.len()).filter(|&i| is_train(i, g)).count()).min().unwrap_or(0);
        for (name, set, note) in [
            ("EARLY-POOLED", &early, String::new()),
            ("LATE-POOLED ", &late, format!(", train≥{}", min_train)),
        ] {
            let k = recs.iter().filter(|t| set.contains(&t.group)).count();
            let bl = acc_in(set, &|t: &Terms| t.base_lane);
            println!(
                "  {} ({} origins{}) n={:>4} | base_lane {:>5.1}%  Δ.08*famg {:>+5.1}  Δ.25*famc {:>+5.1}  (±{:.1}%p)",
                name,
                set.len(),
                note,
                k,
                bl,
                acc_in(set, &|t: &Terms| t.base_lane + 0.08 * t.fam_g) - bl,
                acc_in(set, &|t: &Terms| t.base_lane + 0.25 * t.fam_c) - bl,
                50.0 / (k as f64).sqrt()
            );
        }
        println!("  → leakage: Δfamg ≈0/negative in BOTH pools · data starvation: negative early, POSITIVE late.");
    }

    if !legacy {
        println!(
            "\nPRODUCTION-MATH BASE (S0-3) — patch-local: current(expanding, play order) + prev·0.35·(1−cm/40), changed-champ prev-drop, κ40/κ20 beta_post [notes: {}]:",
            if notes.is_empty() { "MISSING — no resets applied".to_string() } else { format!("{} versions", notes.len()) }
        );
        println!("  base_prod      (champ, production state)     : {:.1}%", acc(&|t: &Terms| t.base_prod));
        println!("  base_lane_prod (lane,  production state)     : {:.1}%", acc(&|t: &Terms| t.base_lane_prod));
        println!(
            "  pooled-history comparison: base κ8 {:.1}% / κ40 {:.1}%  ·  base_lane κ8 {:.1}% / κ40 {:.1}%",
            acc(&|t: &Terms| t.base),
            acc(&|t: &Terms| t.base40),
            acc(&|t: &Terms| t.base_lane),
            acc(&|t: &Terms| t.base_lane40)
        );
        println!("  INCREMENTAL over base_lane_prod (does each athlete term earn weight on the SHIPPED base?):");
        let panchor = |t: &Terms| t.base_lane_prod;
        for (name, term, scales) in [
            ("famc", (&|t: &Terms| t.fam_c) as &dyn Fn(&Terms) -> f64, [0.25, 0.5, 1.0, 2.0]),
            ("fam ", &|t: &Terms| t.fam, [0.25, 0.5, 1.0, 2.0]),
            ("famg", &|t: &Terms| t.fam_g, [0.02, 0.04, 0.08, 0.16]),
            ("fit ", &|t: &Terms| t.fit, [0.02, 0.05, 0.1, 0.2]),
        ] {
            let mut line = format!("    base_lane_prod + s*{}", name);
            for s in scales {
                line.push_str(&format!("  s={:<5}: {:.1}%", s, acc(&|t: &Terms| panchor(t) + s * term(t))));
            }
            println!("{}", line);
        }
        println!("  PER-ORIGIN (Δ over base_lane_prod; chg = champs reset by that version's note):");
        println!("    {:<9} {:>4} | {:>8} {:>8} {:>8} {:>8}", "origin", "chg", "bl_prod", "Δ.25*fc", "Δ.5*fc", "Δ.08*fg");
        for &g in &scored_groups {
            let label = vsorted[g].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".");
            let set = [g];
            let bl = acc_in(&set, &|t: &Terms| t.base_lane_prod);
            println!(
                "    {:<9} {:>4} | {:>7.1}% {:>+8.1} {:>+8.1} {:>+8.1}",
                label,
                notes.get(&label).map_or(0, |s| s.len()),
                bl,
                acc_in(&set, &|t: &Terms| t.base_lane_prod + 0.25 * t.fam_c) - bl,
                acc_in(&set, &|t: &Terms| t.base_lane_prod + 0.5 * t.fam_c) - bl,
                acc_in(&set, &|t: &Terms| t.base_lane_prod + 0.08 * t.fam_g) - bl,
            );
        }
    }
    println!("\n>> A term EARNS engine weight only if it lifts base_lane beyond ~±{:.1}%p noise.", 50.0 / n.sqrt());
}
