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
//! v3.8 (2026-10-29, E7a): cold-start bench for the tag theory — each patch
//! replayed as a fresh save (patch-local tables, theory weight 1 − M/200) plus
//! a career replay; pre-registered in docs/research/2026-10-28-advanced-directions.md.
//!
//! v3.9 (2026-10-29, E7b): changed champions carried by patch size — log loss
//! with a per-arm logistic map fit on earlier patches only; pre-registered in
//! the same doc.
//!
//! v3.10 (2026-10-29): the production replica follows v4.5.0 — changed champs
//! start from their carry (E7b P4) and the patch nudge left the S0-4 score;
//! E7a and E7b's P2 arm still score the pre-4.5.0 reset.
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

use std::collections::HashMap;

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
    /// both teams' bans (pool exclusion for the T4 advice bench).
    bans: Vec<String>,
    /// team ids (T-ban behavioral grader: per-team usage history).
    blue_tid: u32,
    red_tid: u32,
}

/// plain string array (`["a", "b"]`) → items (quote-delimited).
fn parse_str_array(arr: Option<&str>) -> Vec<String> {
    arr.map(|a| a.split('"').skip(1).step_by(2).map(str::to_string).collect()).unwrap_or_default()
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
        let mut bans = parse_str_array(extract_array(block, "blue_ban"));
        bans.extend(parse_str_array(extract_array(block, "red_ban")));
        let blue_tid = num_field(block, "blue_team_id").unwrap_or(0.0) as u32;
        let red_tid = num_field(block, "red_team_id").unwrap_or(0.0) as u32;
        out.push(Match { blue, red, blue_win, version, id, bans, blue_tid, red_tid });
    }
    out
}

/// solo-rank matches (S2-5): `SoloRankMatch { win, version, blue_team/red_team:
/// [SoloRankAthlete { champion, .. }] }` — 5v5, champs only (no positions).
struct SoloM {
    blue: Vec<String>,
    red: Vec<String>,
    blue_win: bool,
    version: String,
}
fn parse_solo(text: &str) -> Vec<SoloM> {
    let b = text.as_bytes();
    let needle = b"SoloRankMatch {";
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
        let champs = |arr: Option<&str>| -> Vec<String> {
            let arr = match arr {
                Some(a) => a,
                None => return vec![],
            };
            let ab = arr.as_bytes();
            let an = b"SoloRankAthlete {";
            let mut cs = Vec::new();
            let mut ao = 0;
            while let Some(ast) = find_from(ab, an, ao) {
                let abr = ast + an.len() - 1;
                let (as_, ae) = match read_balanced(ab, abr, b'{', b'}') {
                    Some(v) => v,
                    None => break,
                };
                if let Some(c) = quoted(&arr[as_..=ae], "champion") {
                    cs.push(c);
                }
                ao = ae + 1;
            }
            cs
        };
        let blue = champs(extract_array(block, "blue_team"));
        let red = champs(extract_array(block, "red_team"));
        let blue_win = match parse_bool(block, "win") {
            Some(v) => v,
            None => continue,
        };
        if blue.len() < 2 || red.len() < 2 {
            continue;
        }
        let version = quoted(block, "version").unwrap_or_default();
        out.push(SoloM { blue, red, blue_win, version });
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
    // BTreeMap: HashMap iteration order + non-associative f64 summation made the
    // lane mean/sd (and thus lp's last display digit) NON-DETERMINISTIC across
    // runs — caught by the --legacy byte-regression on 2026-07-05.
    let mut agg: std::collections::BTreeMap<(String, usize), (f64, f64)> = Default::default(); // (gold, matches)
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

// ---------- production scoring math (S0-3) — mirrors tfm2_db_export/src/recommender.rs patch_current ----------
const PROD_PREV_W: f64 = 0.35; // PREV_PATCH_WEIGHT
const PROD_CONF_CHAMP: f64 = 40.0; // per-champ current-patch matches at which the prev blend fades out
const PROD_KAPPA_CHAMP: f64 = 40.0; // KAPPA_CHAMP
const PROD_KAPPA_LANE: f64 = 20.0; // KAPPA_LANE
const PROD_CARRY_A: f64 = -0.0041; // CARRY_A (E7b-H product fit)
const PROD_CARRY_B: f64 = 0.00072; // CARRY_B
const PROD_KAPPA_CARRY: f64 = 80.0; // KAPPA_CARRY

fn beta_mean(w: f64, m: f64, prior_mean: f64, kappa: f64) -> f64 {
    let pm = prior_mean.clamp(0.001, 0.999);
    let a = w.max(0.0) + pm * kappa;
    let b = (m - w).max(0.0) + (1.0 - pm) * kappa;
    a / (a + b)
}

/// champ posterior MEAN under the shipped patch-local state: current-version WL
/// + previous-version WL at 0.35·(1−cm/40) (cm = champ's current games so far);
/// champs in this version's patch note drop the prev blend (stale) and, since
/// v4.5.0, start from their carry instead of neutral.
type Changed = HashMap<String, (char, u32, u32)>;

/// v4.5.0: changed champ → its carried prior mean μ (`prod_carry`). Empty = the
/// pre-4.5.0 reset, which E7a and E7b's P2 arm keep.
type Carry = HashMap<String, f64>;

/// the carry the product builds for a patch (recommender build_tables +
/// patch_current): μ = clamp(previous-version posterior κ40 + shift, 0.2, 0.8),
/// shift = A + B·size when the note has numbers, else the count nudge
/// (product count_shift ≡ e7b_nudge)
fn prod_carry(changed: &Changed, sizes: Option<&(bool, HashMap<String, f64>)>, prev_c: &HashMap<&str, WL>) -> Carry {
    changed
        .keys()
        .map(|c| {
            let pv = prev_c.get(c.as_str()).copied().unwrap_or_default();
            let shift = match sizes {
                Some((true, s)) => PROD_CARRY_A + PROD_CARRY_B * s.get(c).copied().unwrap_or(0.0),
                _ => e7b_nudge(changed, c),
            };
            (c.clone(), (beta_mean(pv.w, pv.g, 0.5, PROD_KAPPA_CHAMP) + shift).clamp(0.2, 0.8))
        })
        .collect()
}

/// blended (current ×1 + prev ×0.35·fade) pseudo-counts for a champ under the
/// shipped patch-local state; changed champs drop the prev blend entirely.
fn prod_blend(c: &str, cur: &HashMap<&str, WL>, prev: &HashMap<&str, WL>, changed: &Changed) -> (f64, f64) {
    let cu = cur.get(c).copied().unwrap_or_default();
    let pv = if changed.contains_key(c) { WL::default() } else { prev.get(c).copied().unwrap_or_default() };
    let pw = PROD_PREV_W * (1.0 - (cu.g / PROD_CONF_CHAMP).min(1.0));
    (cu.w + pw * pv.w, cu.g + pw * pv.g)
}

fn prod_champ_post(c: &str, cur: &HashMap<&str, WL>, prev: &HashMap<&str, WL>, changed: &Changed, carry: &Carry) -> f64 {
    if let Some(&mu) = carry.get(c) {
        // carried: this patch's games update the carry at κ80
        let cu = cur.get(c).copied().unwrap_or_default();
        return beta_mean(cu.w, cu.g, mu, PROD_KAPPA_CARRY);
    }
    let (w, m) = prod_blend(c, cur, prev, changed);
    beta_mean(w, m, 0.5, PROD_KAPPA_CHAMP)
}

/// blended (current + faded prev) match count for a champ — the harness twin of
/// engine `games()` (drives the ai-prior 15-game fade, and the pre-4.5.0 nudge's
/// 30-game fade in E7a/E7b). 0.0 = no table row at all (engine's u32::MAX
/// regime → full prior; a carried champ's 0-game row fades the same).
fn prod_games(c: &str, cur: &HashMap<&str, WL>, prev: &HashMap<&str, WL>, changed: &Changed) -> f64 {
    prod_blend(c, cur, prev, changed).1
}

/// lane delta under the shipped state: κ20 beta with the champ's own posterior
/// mean as prior; the prev fade uses CHAMP-level current games (recommender.rs
/// keys cur_m by champ, not (champ,lane)); an empty lane cell falls back to champ
/// (for a carried champ this equals its kept-lane cell).
fn prod_lane_delta(
    a: &Ath,
    cur_c: &HashMap<&str, WL>,
    prev_c: &HashMap<&str, WL>,
    cur_cp: &HashMap<(&str, usize), WL>,
    prev_cp: &HashMap<(&str, usize), WL>,
    changed: &Changed,
    carry: &Carry,
) -> f64 {
    let cmean = prod_champ_post(&a.champ, cur_c, prev_c, changed, carry);
    if a.lane < 5 {
        let cu = cur_cp.get(&(a.champ.as_str(), a.lane)).copied().unwrap_or_default();
        let pv = if changed.contains_key(a.champ.as_str()) {
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

/// EXACT `delta_for` replica (engine delta_for) on the production state: the
/// lane cell posterior when the cell exists (m_eff > 0), else the champ delta
/// SHRUNK HARD (×0.25 — a POS-playable lane with zero games must not inherit the
/// main-lane-inflated overall delta). Differs from `prod_lane_delta` (S0-3 anchor)
/// only in that fallback; this is the one the shipped score actually uses. A
/// carried champ's lanes from last patch keep a 0-game cell at its carry.
fn prod_delta_for(
    c: &str,
    lane: usize,
    cur_c: &HashMap<&str, WL>,
    prev_c: &HashMap<&str, WL>,
    cur_cp: &HashMap<(&str, usize), WL>,
    prev_cp: &HashMap<(&str, usize), WL>,
    changed: &Changed,
    carry: &Carry,
) -> f64 {
    if let Some(&mu) = carry.get(c) {
        // v4.5.0 ships E7b's P4 arm: κ80, the lanes it played last patch keep a cell
        return e7b_carry_delta(c, lane, mu, PROD_KAPPA_CARRY, true, cur_c, cur_cp, prev_cp);
    }
    let cd = prod_champ_post(c, cur_c, prev_c, changed, carry) - 0.5;
    if lane < 5 {
        let cu = cur_cp.get(&(c, lane)).copied().unwrap_or_default();
        let pv = if changed.contains_key(c) {
            WL::default()
        } else {
            prev_cp.get(&(c, lane)).copied().unwrap_or_default()
        };
        let champ_cm = cur_c.get(c).map_or(0.0, |w| w.g);
        let pw = PROD_PREV_W * (1.0 - (champ_cm / PROD_CONF_CHAMP).min(1.0));
        let (w, m) = (cu.w + pw * pv.w, cu.g + pw * pv.g);
        if m > 0.0 {
            return beta_mean(w, m, cd + 0.5, PROD_KAPPA_LANE) - 0.5;
        }
        return 0.25 * cd;
    }
    cd
}

/// `=== version V ===` sections + `[champ] #…±` lines from patch_notes_dump.txt
/// → per-version changed champs WITH direction AND change counts. Each `#…`
/// token's trailing +/− is that change's authored is_buff; the champ direction
/// uses the PRODUCTION reduce (lib.rs:514): all-buff '+', all-nerf '-', mixed
/// '~' (no nudge). Value = (dir, buff_count, nerf_count); key presence alone =
/// "changed" (stale prev-blend drop applies regardless of direction).
fn parse_patch_notes(text: &str) -> HashMap<String, HashMap<String, (char, u32, u32)>> {
    let mut out: HashMap<String, HashMap<String, (char, u32, u32)>> = HashMap::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let lt = line.trim();
        if let Some(rest) = lt.strip_prefix("=== version ") {
            let v = rest.split_whitespace().next().unwrap_or("").to_string();
            out.entry(v.clone()).or_default();
            cur = Some(v);
        } else if let (Some(v), Some(rest)) = (cur.as_ref(), lt.strip_prefix('[')) {
            if let Some(end) = rest.find(']') {
                let (mut b, mut nrf) = (0u32, 0u32);
                for tok in rest[end + 1..].split_whitespace() {
                    match tok.as_bytes().last() {
                        Some(b'+') => b += 1,
                        Some(b'-') => nrf += 1,
                        _ => {}
                    }
                }
                let dir = if b > 0 && nrf == 0 {
                    '+'
                } else if nrf > 0 && b == 0 {
                    '-'
                } else {
                    '~'
                };
                out.get_mut(v).unwrap().insert(rest[..end].to_string(), (dir, b, nrf));
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
    // ---- S0-4: the shipped pick score's board-independent terms, replicated ----
    dfor_prod: f64, // Σ delta_for (engine exact: lane cell else 0.25·champ; changed champs carried) diff
    aip_prod: f64,  // Σ ai_prior_term (0.4·p·15-game-fade, clamp ±0.024) diff
    lp_prod: f64,   // Σ matchup LP (0.008·zdiff, clamp ±0.05, actual same-lane opp) diff
    comp_prod: f64, // Σ W_COMP·comp_need (tags-only; retrospective last-pick) diff
    af: f64,        // Σ shipped af (0.020·ln(1+min(g,20)) + 0.15·cwr, cap 0.05) diff
    // ---- post-hoc S2 diagnostics (not in the pre-registered 6-model list) ----
    af_new: f64,  // F15 form: Σ min(0.05, 0.15·cwr) diff (famg removed)
    stack: f64,   // S2-7: max same-category count, blue − red
    c_front: f64, // S2-8: team-has (Tank|Frontline tag) indicator diff
    c_heal: f64,  // S2-8: team-has (Heal|Shield) indicator diff
    c_cc: f64,    // S2-8: team-has CC indicator diff
    c_back: f64,  // S2-8: team-has (Backline | cat Range) indicator diff
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

// ---------- E7a (2026-10-29): the tag theory where the engine actually uses it ----------
// engine.rs added 0.5·(1−conf)·(tag_synergy + tag_counter) to each pick and the
// counter half to bans; conf = min(M/200, 1), and M = the CURRENT patch's
// matches + a fading 0.35 share of the previous patch's (recommender.rs
// patch_current). So the theory was live through a new save's first 200 matches
// and briefly at each patch start. The S0-4 models assumed theory = 0 and never
// scored it. Pre-registered in docs/research/2026-10-28-advanced-directions.md
// (E7a) before the first run. Its verdict set the engine's W_THEORY to 0
// (v4.4.0); this bench scores the theory at its old 0.5, not that constant.
const E7_COMP_UNIT: f64 = 0.12; // engine COMP_UNIT
const E7_W_COMP: f64 = 0.2; // engine W_COMP
const E7_CONF_FULL: f64 = 200.0; // engine FULL_CONF_MATCHES = recommender CONF_FULL
const E7_CAP: f64 = 0.02; // variant (c): a champ's theory total at most this

// E7-REPLICA-BEGIN — engine tag_synergy / tag_counter / comp_need (tags only).
// Checked against the engine's own score_pick_in / score_ban before the first run.
fn tx_has(taxon: &HashMap<String, Taxon>, c: &str, tag: &str) -> bool {
    taxon.get(c).map_or(false, |t| t.tags.iter().any(|x| x == tag))
}
fn tx_cat<'a>(taxon: &'a HashMap<String, Taxon>, c: &str) -> &'a str {
    taxon.get(c).map_or("", |t| t.category.as_str())
}
fn e7_tag_synergy(taxon: &HashMap<String, Taxon>, cand: &str, allies: &[&str]) -> f64 {
    let team = |tag: &str| allies.iter().any(|a| tx_has(taxon, a, tag));
    let has = |tag: &str| tx_has(taxon, cand, tag);
    let mut s = 0.0;
    if team("CC") && (has("AOE") || has("DOT")) {
        s += 0.5;
    }
    if team("AOE") && has("CC") {
        s += 0.5;
    }
    if (team("Backline") || allies.iter().any(|a| tx_cat(taxon, a) == "Range"))
        && (has("Heal") || has("Shield") || has("CC"))
    {
        s += 0.5;
    }
    s * E7_COMP_UNIT
}
fn e7_tag_counter(taxon: &HashMap<String, Taxon>, cand: &str, enemies: &[&str]) -> f64 {
    let cnt = |tag: &str| enemies.iter().filter(|e| tx_has(taxon, e, tag)).count() as f64;
    let cat = |c: &str| enemies.iter().filter(|e| tx_cat(taxon, e) == c).count() as f64;
    let has = |tag: &str| tx_has(taxon, cand, tag);
    let mut s = 0.0;
    let backline = cnt("Backline") + cat("Range");
    if backline >= 2.0 && (has("Mobility") || tx_cat(taxon, cand) == "Assassin") {
        s += 0.4 * (backline - 1.0).min(3.0);
    }
    let front = cnt("Tank") + cnt("Frontline");
    if front >= 2.0 && (has("Poke") || has("DOT")) {
        s += 0.4 * (front - 1.0).min(3.0);
    }
    if cnt("Poke") >= 2.0 && (has("Frontline") || has("Mobility")) {
        s += 0.3;
    }
    if cnt("CC") >= 3.0 && (has("Mobility") || has("Shield")) {
        s += 0.3;
    }
    s * E7_COMP_UNIT
}
fn e7_comp_need(taxon: &HashMap<String, Taxon>, cand: &str, allies: &[&str]) -> f64 {
    if allies.is_empty() {
        return 0.0;
    }
    let any = |tag: &str| allies.iter().any(|a| tx_has(taxon, a, tag));
    let has = |tag: &str| tx_has(taxon, cand, tag);
    let mut s = 0.0;
    if !any("Tank") && has("Tank") {
        s += E7_COMP_UNIT * 0.2;
    } else if !any("Frontline") && has("Frontline") {
        s += E7_COMP_UNIT * 0.2;
    }
    if !any("CC") && has("CC") {
        s += E7_COMP_UNIT * 0.3;
    }
    if !(any("Heal") || any("Shield")) && (has("Heal") || has("Shield")) {
        s += E7_COMP_UNIT * 0.85;
    }
    if !(any("Backline") || allies.iter().any(|a| tx_cat(taxon, a) == "Range"))
        && (has("Backline") || tx_cat(taxon, cand) == "Range")
    {
        s += E7_COMP_UNIT * 0.3;
    }
    s * (allies.len() as f64 / 4.0).min(1.0)
}
// E7-REPLICA-END

/// one scored match for E7a; every score is blue − red, each champ scored as
/// the LAST pick (allies = its four teammates, enemies = the five opponents)
struct E7Rec {
    /// the product's evidence count at this match (theory weight = 1 − M/200)
    m_ev: f64,
    w: f64,
    /// primary anchor: Σ(delta_for + patch nudge) + comp — the shipped pick score
    /// minus the theory, the AI prior and af. The classic AI file is one late
    /// snapshot, not the value at match time, and has no signal alone (50.2%).
    anchor: f64,
    /// Σ delta_for only
    strength: f64,
    /// Σ the shipped AI prior term (secondary anchor = anchor + this)
    prior: f64,
    syn: f64,    // Σ 0.5·w·tag_synergy
    cnt: f64,    // Σ 0.5·w·tag_counter
    capped: f64, // Σ min(0.5·w·(synergy + counter), E7_CAP)
    win: bool,
}

/// `career` false: every patch replayed as a fresh save — patch-local tables,
/// no previous patch, no notes (a player's first patch). true: the shipped
/// career state — previous patch at 0.35, the notes' resets and nudges, and M
/// with the previous patch's fading share.
fn e7_records(
    matches: &[Match],
    groups: &[usize],
    vsorted: &[Vec<u32>],
    taxon: &HashMap<String, Taxon>,
    ai_intr: &HashMap<String, f64>,
    notes: &HashMap<String, Changed>,
    career: bool,
) -> Vec<E7Rec> {
    let empty: Changed = HashMap::new();
    // E7a scored the theory on the pre-4.5.0 product: changed champs reset
    let no_carry: Carry = HashMap::new();
    let mut out = Vec::new();
    for g in 0..vsorted.len() {
        let mut prev_c: HashMap<&str, WL> = HashMap::new();
        let mut prev_cp: HashMap<(&str, usize), WL> = HashMap::new();
        let mut prev_total = 0.0;
        let mut changed = &empty;
        if career && g >= 1 {
            for (i, m) in matches.iter().enumerate() {
                if groups[i] != g - 1 {
                    continue;
                }
                prev_total += 1.0;
                for (team, won) in [(&m.blue, m.blue_win), (&m.red, !m.blue_win)] {
                    for a in team {
                        prev_c.entry(&a.champ).or_default().rec(won);
                        if a.lane < 5 {
                            prev_cp.entry((&a.champ, a.lane)).or_default().rec(won);
                        }
                    }
                }
            }
            let label = vsorted[g].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".");
            changed = notes.get(&label).unwrap_or(&empty);
        }
        let mut idx: Vec<usize> = (0..matches.len()).filter(|&i| groups[i] == g).collect();
        idx.sort_by_key(|&i| matches[i].id);
        let mut cur_c: HashMap<&str, WL> = HashMap::new();
        let mut cur_cp: HashMap<(&str, usize), WL> = HashMap::new();
        for (k, &i) in idx.iter().enumerate() {
            let m = &matches[i];
            let total = k as f64;
            let m_ev = total + (PROD_PREV_W * (1.0 - (total / E7_CONF_FULL).min(1.0)) * prev_total).round();
            let w = 1.0 - (m_ev / E7_CONF_FULL).min(1.0);
            if w > 0.0 {
                // engine chg_nudge: count-scaled ±(0.01 + 0.005·min(n,4)), '~' by its
                // buff/nerf balance, fading over the champ's first 30 games
                let nudge = |c: &str| -> f64 {
                    let Some(&(dir, b, nrf)) = changed.get(c) else { return 0.0 };
                    let n = b + nrf;
                    let uni = 0.01 + 0.005 * (if n == 0 { 2 } else { n.min(4) }) as f64;
                    let d = match dir {
                        '+' => uni,
                        '-' => -uni,
                        '~' if n > 0 => 0.02 * (b as f64 - nrf as f64) / n as f64 * (n as f64 / 4.0).min(1.0),
                        _ => return 0.0,
                    };
                    let gm = prod_games(c, &cur_c, &prev_c, changed);
                    if gm <= 0.0 {
                        return d;
                    }
                    d * (1.0 - (gm / 30.0).min(1.0)).max(0.0)
                };
                let prior = |c: &str| -> f64 {
                    let Some(&p) = ai_intr.get(c) else { return 0.0 };
                    let gm = prod_games(c, &cur_c, &prev_c, changed);
                    let fresh = if gm <= 0.0 { 1.0 } else { (1.0 - gm / 15.0).max(0.0) };
                    (0.4 * p * fresh).clamp(-0.024, 0.024)
                };
                let side = |team: &[Ath], opp: &[Ath]| -> [f64; 6] {
                    let enemies: Vec<&str> = opp.iter().map(|a| a.champ.as_str()).collect();
                    let mut s = [0.0; 6]; // anchor, strength, prior, syn, cnt, capped
                    for (j, a) in team.iter().enumerate() {
                        let allies: Vec<&str> =
                            team.iter().enumerate().filter(|(x, _)| *x != j).map(|(_, b)| b.champ.as_str()).collect();
                        let d = prod_delta_for(&a.champ, a.lane, &cur_c, &prev_c, &cur_cp, &prev_cp, changed, &no_carry);
                        let sy = 0.5 * w * e7_tag_synergy(taxon, &a.champ, &allies);
                        let cn = 0.5 * w * e7_tag_counter(taxon, &a.champ, &enemies);
                        s[0] += d + nudge(&a.champ) + E7_W_COMP * e7_comp_need(taxon, &a.champ, &allies);
                        s[1] += d;
                        s[2] += prior(&a.champ);
                        s[3] += sy;
                        s[4] += cn;
                        s[5] += (sy + cn).min(E7_CAP);
                    }
                    s
                };
                let (b, r) = (side(&m.blue, &m.red), side(&m.red, &m.blue));
                out.push(E7Rec {
                    m_ev,
                    w,
                    anchor: b[0] - r[0],
                    strength: b[1] - r[1],
                    prior: b[2] - r[2],
                    syn: b[3] - r[3],
                    cnt: b[4] - r[4],
                    capped: b[5] - r[5],
                    win: m.blue_win,
                });
            }
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
    out
}

/// accuracy (ties = 0.5) and the paired difference alt − base with a 95%
/// bootstrap interval over `set` (1000 match resamples, fixed seed — the same
/// draws for every comparison)
fn e7_paired(recs: &[E7Rec], set: &[usize], base: &dyn Fn(&E7Rec) -> f64, alt: &dyn Fn(&E7Rec) -> f64) -> (f64, f64, f64, f64) {
    let corr = |s: f64, win: bool| {
        if s.abs() < 1e-12 {
            0.5
        } else if (s > 0.0) == win {
            1.0
        } else {
            0.0
        }
    };
    let d: Vec<f64> = set.iter().map(|&i| corr(alt(&recs[i]), recs[i].win) - corr(base(&recs[i]), recs[i].win)).collect();
    let acc = set.iter().map(|&i| corr(alt(&recs[i]), recs[i].win)).sum::<f64>() / set.len().max(1) as f64 * 100.0;
    let n = d.len();
    if n == 0 {
        return (acc, 0.0, 0.0, 0.0);
    }
    let mut rng: u64 = 20260705; // the S0-4 seed and percentile convention
    let mut boot: Vec<f64> = (0..1000)
        .map(|_| {
            let mut s = 0.0;
            for _ in 0..n {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                s += d[(rng % n as u64) as usize];
            }
            s / n as f64 * 100.0
        })
        .collect();
    boot.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (acc, d.iter().sum::<f64>() / n as f64 * 100.0, boot[25], boot[975])
}

fn e7_report(fresh: &[E7Rec], career: &[E7Rec]) {
    let all: Vec<usize> = (0..fresh.len()).collect();
    let moved = fresh.iter().filter(|r| (r.syn + r.cnt).abs() > 1e-12).count();
    println!(
        "\nE7a TAG THEORY (pre-registered) — each patch replayed as a fresh save: patch-local tables, no previous patch, no notes; M = matches already seen in that patch; scored while M < {:.0}:",
        E7_CONF_FULL
    );
    println!(
        "  scored {} matches · mean theory weight {:.2} · theory moves the match score in {:.0}% · mean |theory diff| {:.4}",
        fresh.len(),
        fresh.iter().map(|r| r.w).sum::<f64>() / fresh.len().max(1) as f64,
        100.0 * moved as f64 / fresh.len().max(1) as f64,
        fresh.iter().map(|r| (r.syn + r.cnt).abs()).sum::<f64>() / fresh.len().max(1) as f64
    );
    let anchor = |r: &E7Rec| r.anchor;
    let (acc_a, ..) = e7_paired(fresh, &all, &anchor, &anchor);
    println!("  anchor A = Σ(delta_for + patch nudge) + comp (shipped pick score minus theory, AI prior, af): {:.1}%", acc_a);
    let (acc_t, ..) = e7_paired(fresh, &all, &anchor, &|r: &E7Rec| r.syn + r.cnt);
    println!("  theory alone (sign of synergy + counter)          : {:.1}%", acc_t);
    let line = |name: &str, set: &[usize], base: &dyn Fn(&E7Rec) -> f64, alt: &dyn Fn(&E7Rec) -> f64| -> f64 {
        let (acc, d, lo, hi) = e7_paired(fresh, set, base, alt);
        println!("  {:<50}: {:>5.1}%  Δ {:+.1} [{:+.1}, {:+.1}]", name, acc, d, lo, hi);
        lo
    };
    line("A + shipped theory (synergy + counter)", &all, &anchor, &|r: &E7Rec| r.anchor + r.syn + r.cnt);
    let lo_syn = line("A + synergy half", &all, &anchor, &|r: &E7Rec| r.anchor + r.syn);
    let lo_cnt = line("A + counter half", &all, &anchor, &|r: &E7Rec| r.anchor + r.cnt);
    let lo_full = {
        let (_, _, lo, _) = e7_paired(fresh, &all, &anchor, &|r: &E7Rec| r.anchor + r.syn + r.cnt);
        lo
    };
    line("A + (a) counter 0, synergy × 1/4", &all, &anchor, &|r: &E7Rec| r.anchor + 0.25 * r.syn);
    line("A + (c) theory capped at 0.02 a champ", &all, &anchor, &|r: &E7Rec| r.anchor + r.capped);
    println!("  other anchors (Δ of shipped theory / synergy / counter on top):");
    for (name, base) in [
        ("A0 = Σ delta_for only", (&|r: &E7Rec| r.strength) as &dyn Fn(&E7Rec) -> f64),
        ("A + shipped AI prior", &|r: &E7Rec| r.anchor + r.prior),
    ] {
        let mut s = format!("    {:<22}: {:>5.1}%", name, e7_paired(fresh, &all, base, base).0);
        for alt in [
            (&|r: &E7Rec| base(r) + r.syn + r.cnt) as &dyn Fn(&E7Rec) -> f64,
            &|r: &E7Rec| base(r) + r.syn,
            &|r: &E7Rec| base(r) + r.cnt,
        ] {
            let (_, d, lo, hi) = e7_paired(fresh, &all, base, alt);
            s += &format!("  {:+.1} [{:+.1}, {:+.1}]", d, lo, hi);
        }
        println!("{}", s);
    }
    println!("  by M (Δ of shipped theory / synergy / counter over A):");
    for (lo_m, hi_m) in [(0.0, 50.0), (50.0, 100.0), (100.0, 200.0)] {
        let set: Vec<usize> = all.iter().copied().filter(|&i| fresh[i].m_ev >= lo_m && fresh[i].m_ev < hi_m).collect();
        let mut s = format!("    M {:>3.0}–{:<3.0} n={:<4} A {:>5.1}%", lo_m, hi_m - 1.0, set.len(), e7_paired(fresh, &set, &anchor, &anchor).0);
        for alt in [
            (&|r: &E7Rec| r.anchor + r.syn + r.cnt) as &dyn Fn(&E7Rec) -> f64,
            &|r: &E7Rec| r.anchor + r.syn,
            &|r: &E7Rec| r.anchor + r.cnt,
        ] {
            let (_, d, lo, hi) = e7_paired(fresh, &set, &anchor, alt);
            s += &format!("  {:+.1} [{:+.1}, {:+.1}]", d, lo, hi);
        }
        println!("{}", s);
    }
    // both halves pass alone but the shipped sum does not → keep only the half
    // with the higher lower bound
    let clash = lo_syn > 0.0 && lo_cnt > 0.0 && lo_full <= 0.0;
    let keep_syn = lo_syn > 0.0 && !(clash && lo_syn < lo_cnt);
    let keep_cnt = lo_cnt > 0.0 && !(clash && lo_cnt <= lo_syn);
    let word = |k: bool| if k { "KEEP" } else { "ZERO" };
    println!(
        "  VERDICT (pre-registered: a half keeps its shipped size only if its Δ CI lower bound > 0): synergy → {} · counter → {}",
        word(keep_syn),
        word(keep_cnt)
    );
    // post-hoc (added after the first run, descriptive only): the matches where
    // the shipped theory changes the predicted winner, and how often it is right there
    let call = |s: f64| if s.abs() < 1e-12 { 0 } else if s > 0.0 { 1 } else { -1 };
    let (mut flips, mut right) = (0u32, 0.0f64);
    for r in fresh {
        let after = call(r.anchor + r.syn + r.cnt);
        if after != call(r.anchor) {
            flips += 1;
            right += if after == 0 { 0.5 } else if (after > 0) == r.win { 1.0 } else { 0.0 };
        }
    }
    println!(
        "  post-hoc: the shipped theory changes the predicted winner in {} of {} matches ({:.0}%) and is right in {:.1}% of those",
        flips,
        fresh.len(),
        100.0 * flips as f64 / fresh.len().max(1) as f64,
        100.0 * right / (flips.max(1)) as f64
    );
    let call: Vec<usize> = (0..career.len()).collect();
    let mut s = format!(
        "E7a career replay (secondary — previous patch at 0.35, notes, the product's M): scored {} · mean weight {:.2} · A {:.1}%",
        career.len(),
        career.iter().map(|r| r.w).sum::<f64>() / career.len().max(1) as f64,
        e7_paired(career, &call, &anchor, &anchor).0
    );
    for (name, alt) in [
        ("shipped", (&|r: &E7Rec| r.anchor + r.syn + r.cnt) as &dyn Fn(&E7Rec) -> f64),
        ("synergy", &|r: &E7Rec| r.anchor + r.syn),
        ("counter", &|r: &E7Rec| r.anchor + r.cnt),
    ] {
        let (_, d, lo, hi) = e7_paired(career, &call, &anchor, alt);
        s += &format!(" · {} {:+.1} [{:+.1}, {:+.1}]", name, d, lo, hi);
    }
    println!("{}", s);
}

// ---------- E7b (2026-10-29): changed champions carried by patch size ----------
// The pre-4.5.0 product drops a changed champ's previous patch and adds a count
// nudge (P2). E7b starts it from the previous posterior moved by a + b·size
// instead (P3 κ40, P4 κ80), with a and b fit on earlier patches only.
// Pre-registered in docs/research/2026-10-28-advanced-directions.md (E7b)
// before the first run. v4.5.0 ships P4 and the prod_* replicas above follow it
// (`prod_carry`, fixed product a and b); this bench's P2 arm and E7a keep the
// reset through an empty carry.
const E7B_FIT_MIN: usize = 8; // fewer earlier points → the shipped nudge stands in for the shift
const E7B_FIT_GAMES: f64 = 10.0; // a fit point needs this many games in both patches
const E7B_FRESH: f64 = 30.0; // primary: a changed champ under this many current games
const E7B_ARMS: [&str; 5] = [
    "P2 shipped (reset + count nudge)",
    "P3 size carry κ40",
    "P4 size carry κ80",
    "r1 = P4 without the shift",
    "r2 = P4 without kept lanes",
];

/// per version: (the section has real before/after values, champ → signed size
/// Σ±min(100, |after − pre|/|pre|·100)). The 4.0/4.1 sections of the extended
/// notes are synthesized direction-only tokens, so they read as not real.
fn e7b_sizes(text: &str) -> HashMap<String, (bool, HashMap<String, f64>)> {
    let mut out: HashMap<String, (bool, HashMap<String, f64>)> = HashMap::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let lt = line.trim();
        if let Some(rest) = lt.strip_prefix("=== version ") {
            let v = rest.split_whitespace().next().unwrap_or("").to_string();
            out.entry(v.clone()).or_default();
            cur = Some(v);
        } else if let (Some(v), Some(rest)) = (cur.as_ref(), lt.strip_prefix('[')) {
            let Some(end) = rest.find(']') else { continue };
            let e = out.get_mut(v).unwrap();
            let mut s = 0.0;
            for tok in rest[end + 1..].split_whitespace() {
                let buff = match tok.as_bytes().last() {
                    Some(b'+') => true,
                    Some(b'-') => false,
                    _ => continue,
                };
                let body = &tok[..tok.len() - 1];
                let Some((pre, after)) = body.rfind(':').and_then(|k| body[k + 1..].split_once("->")) else { continue };
                let num = |x: &str| x.trim().trim_end_matches('%').parse::<f64>().ok();
                if let (Some(p), Some(a)) = (num(pre), num(after)) {
                    e.0 = true;
                    if p.abs() > 1e-9 {
                        let rel = (((a - p) / p).abs() * 100.0).min(100.0);
                        s += if buff { rel } else { -rel };
                    }
                }
            }
            e.1.insert(rest[..end].to_string(), s);
        }
    }
    out
}

/// the shipped chg_nudge before its 30-game fade (engine chg_nudge)
fn e7b_nudge(changed: &Changed, c: &str) -> f64 {
    let Some(&(dir, b, nrf)) = changed.get(c) else { return 0.0 };
    let n = b + nrf;
    let uni = 0.01 + 0.005 * (if n == 0 { 2 } else { n.min(4) }) as f64;
    match dir {
        '+' => uni,
        '-' => -uni,
        '~' if n > 0 => 0.02 * (b as f64 - nrf as f64) / n as f64 * (n as f64 / 4.0).min(1.0),
        _ => 0.0,
    }
}

/// a changed champ's lane delta when it starts from prior mean `mu` at strength
/// `kappa`: current rows only, the lane cell's prior is the new champ posterior
/// (κ20); `keep` = the lanes it played last patch keep their cell
fn e7b_carry_delta(
    c: &str,
    lane: usize,
    mu: f64,
    kappa: f64,
    keep: bool,
    cur_c: &HashMap<&str, WL>,
    cur_cp: &HashMap<(&str, usize), WL>,
    prev_cp: &HashMap<(&str, usize), WL>,
) -> f64 {
    let cu = cur_c.get(c).copied().unwrap_or_default();
    let cp = beta_mean(cu.w, cu.g, mu, kappa);
    if lane < 5 {
        let cell = cur_cp.get(&(c, lane)).copied().unwrap_or_default();
        if cell.g > 0.0 {
            return beta_mean(cell.w, cell.g, cp, PROD_KAPPA_LANE) - 0.5;
        }
        if keep && prev_cp.get(&(c, lane)).map_or(false, |w| w.g > 0.0) {
            return cp - 0.5;
        }
        return 0.25 * (cp - 0.5);
    }
    cp - 0.5
}

/// one E7b match (career replay); scores are blue − red, in E7B_ARMS order
struct E7bRec {
    g: usize,
    /// match index within its patch (id order)
    k: usize,
    /// a changed champ under E7B_FRESH current-patch games is on the board
    fresh: bool,
    s: [f64; 5],
    win: bool,
}

/// one patch of the replay: (label, changed champs, earlier fit points, the
/// shift fit a + b·size if any, the note has real sizes)
struct E7bPatch {
    label: String,
    changed: usize,
    points: usize,
    fit: Option<(f64, f64)>,
    real: bool,
}

fn e7b_records(
    matches: &[Match],
    groups: &[usize],
    vsorted: &[Vec<u32>],
    taxon: &HashMap<String, Taxon>,
    notes: &HashMap<String, Changed>,
    sizes: &HashMap<String, (bool, HashMap<String, f64>)>,
) -> (Vec<E7bRec>, Vec<E7bPatch>) {
    let empty: Changed = HashMap::new();
    let no_sizes: HashMap<String, f64> = HashMap::new();
    // the P2 arm is the pre-4.5.0 product: changed champs reset
    let no_carry: Carry = HashMap::new();
    let mut out = Vec::new();
    let mut patches = Vec::new();
    // (size, new-patch winrate − previous posterior) from COMPLETED patches only
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let mut prev_c: HashMap<&str, WL> = HashMap::new();
    let mut prev_cp: HashMap<(&str, usize), WL> = HashMap::new();
    for g in 0..vsorted.len() {
        let label = vsorted[g].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".");
        let changed = if g >= 1 { notes.get(&label).unwrap_or(&empty) } else { &empty };
        let (real, size) = sizes.get(&label).map_or((false, &no_sizes), |(r, s)| (*r, s));
        let fit = if real && pts.len() >= E7B_FIT_MIN { Some(e7b_ols(&pts)) } else { None };
        patches.push(E7bPatch { label, changed: changed.len(), points: pts.len(), fit, real });
        // prior means for this patch's changed champs: [P3/P4 shifted, r1 unshifted]
        let mu: HashMap<&str, [f64; 2]> = changed
            .keys()
            .map(|c| {
                let pv = prev_c.get(c.as_str()).copied().unwrap_or_default();
                let post = if pv.g > 0.0 { beta_mean(pv.w, pv.g, 0.5, PROD_KAPPA_CHAMP) } else { 0.5 };
                let shift = match fit {
                    Some((a, b)) => a + b * size.get(c).copied().unwrap_or(0.0),
                    None => e7b_nudge(changed, c),
                };
                (c.as_str(), [(post + shift).clamp(0.2, 0.8), post.clamp(0.2, 0.8)])
            })
            .collect();
        let mut idx: Vec<usize> = (0..matches.len()).filter(|&i| groups[i] == g).collect();
        idx.sort_by_key(|&i| matches[i].id);
        let mut cur_c: HashMap<&str, WL> = HashMap::new();
        let mut cur_cp: HashMap<(&str, usize), WL> = HashMap::new();
        for (k, &i) in idx.iter().enumerate() {
            let m = &matches[i];
            let mut fresh = false;
            let side = |team: &[Ath], fresh: &mut bool| -> [f64; 5] {
                let mut s = [0.0; 5];
                for (j, a) in team.iter().enumerate() {
                    let allies: Vec<&str> =
                        team.iter().enumerate().filter(|(x, _)| *x != j).map(|(_, b)| b.champ.as_str()).collect();
                    let comp = E7_W_COMP * e7_comp_need(taxon, &a.champ, &allies);
                    let d = prod_delta_for(&a.champ, a.lane, &cur_c, &prev_c, &cur_cp, &prev_cp, changed, &no_carry);
                    let Some(&[shifted, unshifted]) = mu.get(a.champ.as_str()) else {
                        for x in s.iter_mut() {
                            *x += d + comp;
                        }
                        continue;
                    };
                    let cg = cur_c.get(a.champ.as_str()).map_or(0.0, |w| w.g);
                    *fresh |= cg < E7B_FRESH;
                    let carry = |prior: f64, kappa: f64, keep: bool| {
                        e7b_carry_delta(&a.champ, a.lane, prior, kappa, keep, &cur_c, &cur_cp, &prev_cp) + comp
                    };
                    s[0] += d + e7b_nudge(changed, &a.champ) * (1.0 - (cg / 30.0).min(1.0)) + comp;
                    s[1] += carry(shifted, 40.0, true);
                    s[2] += carry(shifted, 80.0, true);
                    s[3] += carry(unshifted, 80.0, true);
                    s[4] += carry(shifted, 80.0, false);
                }
                s
            };
            let (b, r) = (side(&m.blue, &mut fresh), side(&m.red, &mut fresh));
            out.push(E7bRec { g, k, fresh, s: core::array::from_fn(|j| b[j] - r[j]), win: m.blue_win });
            for (team, won) in [(&m.blue, m.blue_win), (&m.red, !m.blue_win)] {
                for a in team {
                    cur_c.entry(&a.champ).or_default().rec(won);
                    if a.lane < 5 {
                        cur_cp.entry((&a.champ, a.lane)).or_default().rec(won);
                    }
                }
            }
        }
        // patch complete → its changed champs join the fit (sorted: fixed summation order)
        if g >= 1 && real {
            let mut cs: Vec<&String> = changed.keys().collect();
            cs.sort();
            for c in cs {
                let cu = cur_c.get(c.as_str()).copied().unwrap_or_default();
                let pv = prev_c.get(c.as_str()).copied().unwrap_or_default();
                if cu.g >= E7B_FIT_GAMES && pv.g >= E7B_FIT_GAMES {
                    let post = beta_mean(pv.w, pv.g, 0.5, PROD_KAPPA_CHAMP);
                    pts.push((size.get(c).copied().unwrap_or(0.0), cu.w / cu.g - post));
                }
            }
        }
        prev_c = cur_c;
        prev_cp = cur_cp;
    }
    // what the product would ship: the fit over every real-size patch
    patches.push(E7bPatch {
        label: "(all)".to_string(),
        changed: 0,
        points: pts.len(),
        fit: (pts.len() >= E7B_FIT_MIN).then(|| e7b_ols(&pts)),
        real: true,
    });
    (out, patches)
}

/// unweighted least squares y = a + b·x → (a, b)
fn e7b_ols(pts: &[(f64, f64)]) -> (f64, f64) {
    let n = pts.len() as f64;
    let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx) * (p.0 - mx)).sum();
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let b = if sxx > 1e-12 { sxy / sxx } else { 0.0 };
    (my - b * mx, b)
}

/// logistic maximum likelihood for P(blue) = σ(α + β·x), Newton from (0, 0)
fn e7b_logit(data: &[(f64, bool)]) -> (f64, f64) {
    let (mut a, mut b) = (0.0f64, 0.0f64);
    for _ in 0..100 {
        let (mut g0, mut g1, mut h00, mut h01, mut h11) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &(x, y) in data {
            let p = 1.0 / (1.0 + (-(a + b * x)).exp());
            let r = if y { 1.0 - p } else { -p };
            let w = p * (1.0 - p);
            g0 += r;
            g1 += r * x;
            h00 += w;
            h01 += w * x;
            h11 += w * x * x;
        }
        let det = h00 * h11 - h01 * h01;
        if det.abs() < 1e-12 {
            break;
        }
        let (da, db) = ((h11 * g0 - h01 * g1) / det, (h00 * g1 - h01 * g0) / det);
        a += da;
        b += db;
        if da.abs() + db.abs() < 1e-12 {
            break;
        }
    }
    (a, b)
}

/// mean and 95% paired bootstrap interval of `d` (the S0-4 seed and convention)
fn e7b_boot(d: &[f64]) -> (f64, f64, f64) {
    let n = d.len();
    if n == 0 {
        return (0.0, 0.0, 0.0);
    }
    let mut rng: u64 = 20260705;
    let mut boot: Vec<f64> = (0..1000)
        .map(|_| {
            let mut s = 0.0;
            for _ in 0..n {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                s += d[(rng % n as u64) as usize];
            }
            s / n as f64
        })
        .collect();
    boot.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (d.iter().sum::<f64>() / n as f64, boot[25], boot[975])
}

/// `holdout` (E7b-H, `--e7b-holdout=v1,v2`): patches kept out of the verdict sets
/// and scored together on their own; empty = the pre-registered E7b run
fn e7b_report(recs: &[E7bRec], patches: &[E7bPatch], holdout: &[String]) {
    let (all_fit, patches) = patches.split_last().expect("the (all) entry");
    println!(
        "\nE7b PATCH-SIZE CARRY (pre-registered) — career replay; a changed champ starts from the previous posterior + a + b·size (P3 κ40, P4 κ80) instead of the shipped reset + count nudge (P2); P(blue) = σ(α + β·score), fit per arm on earlier patches only:"
    );
    for (j, name) in E7B_ARMS.iter().enumerate() {
        println!("  arm {}: {}", j, name);
    }
    println!("  patch      changed  sizes  points  shift");
    for p in patches {
        let shift = match (p.fit, p.changed) {
            (_, 0) => "— (no note: all arms equal)".to_string(),
            (Some((a, b)), _) => format!("{:+.4} {:+.5}·size", a, b),
            (None, _) if !p.real => "the shipped nudge (no sizes in the note)".to_string(),
            (None, _) => format!("the shipped nudge (fewer than {} points)", E7B_FIT_MIN),
        };
        println!("  {:<10} {:>7}  {:>5}  {:>6}  {}", p.label, p.changed, if p.real { "yes" } else { "no" }, p.points, shift);
    }
    if let Some((a, b)) = all_fit.fit {
        println!("  product fit (all {} points of the real-size patches): shift = {:+.4} {:+.5}·size", all_fit.points, a, b);
    }
    // per-arm logistic map, refit before each patch on that arm's earlier patches
    let ng = patches.len();
    let mut ll = vec![[0.0f64; 5]; recs.len()];
    let mut br = vec![[0.0f64; 5]; recs.len()];
    let mut maps = vec![[(0.0f64, 0.0f64); 5]; ng];
    for j in 0..5 {
        for g in 1..ng {
            let train: Vec<(f64, bool)> = recs.iter().filter(|r| r.g < g).map(|r| (r.s[j], r.win)).collect();
            let (a, b) = e7b_logit(&train);
            maps[g][j] = (a, b);
            for (i, r) in recs.iter().enumerate().filter(|(_, r)| r.g == g) {
                let p = (1.0 / (1.0 + (-(a + b * r.s[j])).exp())).clamp(1e-6, 1.0 - 1e-6);
                let y = if r.win { 1.0 } else { 0.0 };
                ll[i][j] = -(y * p.ln() + (1.0 - y) * (1.0 - p).ln());
                br[i][j] = (p - y) * (p - y);
            }
        }
    }
    let acc = |r: &E7bRec, j: usize| {
        let s = r.s[j];
        if s.abs() < 1e-12 {
            0.5
        } else if (s > 0.0) == r.win {
            1.0
        } else {
            0.0
        }
    };
    // Δ is signed so that + = the arm beats P2 (lower loss, higher accuracy)
    let line = |name: &str, set: &[usize], verdict: bool| -> [(f64, f64); 5] {
        let mut ci = [(0.0, 0.0); 5];
        if set.is_empty() {
            println!("  {} n=0", name);
            return ci;
        }
        println!("  {} n={}:", name, set.len());
        let mean = |f: &dyn Fn(usize) -> f64| set.iter().map(|&i| f(i)).sum::<f64>() / set.len().max(1) as f64;
        let mut s_ll = format!("    log loss ×1000  P2 {:.1}", 1000.0 * mean(&|i| ll[i][0]));
        let mut s_br = format!("    Brier ×1000     P2 {:.1}", 1000.0 * mean(&|i| br[i][0]));
        let mut s_ac = format!("    accuracy        P2 {:.1}%", 100.0 * mean(&|i| acc(&recs[i], 0)));
        for j in 1..5 {
            let (d, lo, hi) = e7b_boot(&set.iter().map(|&i| 1000.0 * (ll[i][0] - ll[i][j])).collect::<Vec<_>>());
            ci[j] = (lo, hi);
            s_ll += &format!("  | {} {:.1} Δ {:+.1} [{:+.1}, {:+.1}]", &E7B_ARMS[j][..2], 1000.0 * mean(&|i| ll[i][j]), d, lo, hi);
            let (d, lo, hi) = e7b_boot(&set.iter().map(|&i| 1000.0 * (br[i][0] - br[i][j])).collect::<Vec<_>>());
            s_br += &format!("  | {} {:.1} Δ {:+.1} [{:+.1}, {:+.1}]", &E7B_ARMS[j][..2], 1000.0 * mean(&|i| br[i][j]), d, lo, hi);
            let (d, lo, hi) =
                e7b_boot(&set.iter().map(|&i| 100.0 * (acc(&recs[i], j) - acc(&recs[i], 0))).collect::<Vec<_>>());
            s_ac += &format!("  | {} {:.1}% Δ {:+.1} [{:+.1}, {:+.1}]", &E7B_ARMS[j][..2], 100.0 * mean(&|i| acc(&recs[i], j)), d, lo, hi);
        }
        println!("{}", s_ll);
        if !verdict {
            println!("{}", s_br);
            println!("{}", s_ac);
        }
        ci
    };
    let held = |g: usize| holdout.iter().any(|h| *h == patches[g].label);
    let scored: Vec<usize> = (0..recs.len()).filter(|&i| patches[recs[i].g].fit.is_some() && !held(recs[i].g)).collect();
    let primary: Vec<usize> = scored.iter().copied().filter(|&i| recs[i].fresh).collect();
    let names: Vec<&str> =
        (0..patches.len()).filter(|&g| patches[g].fit.is_some() && !held(g)).map(|g| patches[g].label.as_str()).collect();
    println!(
        "  scored patches (real sizes, ≥ {} earlier points): {} · Δ = P2 − arm for losses, arm − P2 for accuracy (+ = arm better), 95% paired bootstrap",
        E7B_FIT_MIN,
        names.join(" ")
    );
    let pci = line(&format!("PRIMARY — a changed champ under {:.0} current games on the board", E7B_FRESH), &primary, false);
    let aci = line("ALL matches of the scored patches", &scored, false);
    println!("  secondary (not used for the verdict):");
    let first50: Vec<usize> = scored.iter().copied().filter(|&i| recs[i].k < 50).collect();
    line("first 50 matches of each scored patch", &first50, true);
    let fallback: Vec<usize> =
        (0..recs.len()).filter(|&i| recs[i].g >= 1 && patches[recs[i].g].real && patches[recs[i].g].fit.is_none()).collect();
    line("patches where the nudge stands in (too few points)", &fallback, true);
    let synth: Vec<usize> = (0..recs.len()).filter(|&i| recs[i].g >= 1 && !patches[recs[i].g].real).collect();
    line("synthesized-note patches (direction only → nudge)", &synth, true);
    for g in 1..patches.len() {
        if patches[g].fit.is_none() {
            continue;
        }
        let set: Vec<usize> = (0..recs.len()).filter(|&i| recs[i].g == g && recs[i].fresh).collect();
        let tag = if held(g) { " (holdout)" } else { "" };
        line(&format!("primary in {}{}", patches[g].label, tag), &set, true);
    }
    // pre-registered rule: primary lower bound > 0 and the all-match upper bound > 0;
    // both pass → the higher primary lower bound
    let pass = |j: usize| pci[j].0 > 0.0 && aci[j].1 > 0.0;
    let pick = match (pass(1), pass(2)) {
        (true, true) => Some(if pci[1].0 > pci[2].0 { 1 } else { 2 }),
        (true, false) => Some(1),
        (false, true) => Some(2),
        (false, false) => None,
    };
    let word = |j: usize| if pass(j) { "PASS" } else { "FAIL" };
    println!(
        "  VERDICT (pre-registered: primary Δ lower bound > 0 and all-match Δ upper bound > 0): P3 → {} · P4 → {} → {}",
        word(1),
        word(2),
        pick.map_or("keep the shipped reset + count nudge".to_string(), |j| format!("adopt {}", E7B_ARMS[j]))
    );
    if !holdout.is_empty() {
        // E7b-H (pre-registered after E7b): patches whose sizes neither the pilot nor
        // any E7b fit has seen; the check is the direction of P4's primary Δ
        let set: Vec<usize> = (0..recs.len())
            .filter(|&i| patches[recs[i].g].fit.is_some() && held(recs[i].g) && recs[i].fresh)
            .collect();
        let mean: f64 = set.iter().map(|&i| 1000.0 * (ll[i][0] - ll[i][2])).sum::<f64>() / set.len().max(1) as f64;
        line(&format!("HOLDOUT E7b-H — primary over {}", holdout.join(" ")), &set, false);
        println!(
            "  HOLDOUT VERDICT (pre-registered: P4 primary Δ > 0 confirms the direction): {}",
            if set.is_empty() {
                "NO DATA"
            } else if mean > 0.0 {
                "CONFIRMED"
            } else {
                "NOT CONFIRMED"
            }
        );
    }
    // post-hoc (added after the first run, descriptive only): the maps the losses used
    let s: Vec<String> = (1..ng)
        .map(|g| format!("{} {:+.3}/{:.2}/{:.2}", patches[g].label, maps[g][0].0, maps[g][0].1, maps[g][2].1))
        .collect();
    println!("  post-hoc: logistic maps per patch (α / β of P2 / β of P4): {}", s.join(" · "));
}

fn main() {
    // --legacy = the old non-causal i%5 interleave (kept for comparison); default is
    // the causal rolling-origin split (train on strictly-earlier patches only).
    let raw: Vec<String> = std::env::args().collect();
    let legacy = raw.iter().any(|a| a == "--legacy");
    // --notes=<patch_notes_dump.txt> → per-version changed-champ sets for the S0-3
    // production base (stale-blend-drop). Optional: absent = no resets (reported).
    let notes_path: Option<String> = raw.iter().find_map(|a| a.strip_prefix("--notes=").map(str::to_string));
    // --solo=<solo_rank_matches.debug.txt> → S2-5 solo-rank power-prior experiment.
    let solo_path: Option<String> = raw.iter().find_map(|a| a.strip_prefix("--solo=").map(str::to_string));
    // --e7b-holdout=<v1,v2> → E7b-H: score these patches apart from the E7b verdict
    let e7b_holdout: Vec<String> = raw
        .iter()
        .find_map(|a| a.strip_prefix("--e7b-holdout="))
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
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
    let notes: HashMap<String, Changed> = notes_path
        .as_deref()
        .map(|p| parse_patch_notes(&std::fs::read_to_string(p).unwrap_or_default()))
        .unwrap_or_default();
    // per version: the note has numbers, champ → signed size (the carry's shift, E7b)
    let sizes: HashMap<String, (bool, HashMap<String, f64>)> = notes_path
        .as_deref()
        .map(|p| e7b_sizes(&std::fs::read_to_string(p).unwrap_or_default()))
        .unwrap_or_default();
    let solo: Vec<SoloM> = solo_path
        .as_deref()
        .map(|p| parse_solo(&std::fs::read_to_string(p).unwrap_or_default()))
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
    // S2-3 κ sweep side-channel: per scored match (rolling only), the production
    // champ-state diff at κ ∈ {20, 80, 150} (κ40 lives in Terms::base_prod).
    let mut kprod: Vec<[f64; 3]> = Vec::new();
    // S2-5/S2-6 side-channels (rolling only, aligned with recs): solo-augmented
    // production champ base at w_solo {0.1, 0.25}, BT θ diff, and the match's
    // mean blended champ games (thin/thick stratification for T-solo).
    let mut solo_v: Vec<[f64; 2]> = Vec::new();
    let mut bt_v: Vec<f64> = Vec::new();
    let mut mgames_v: Vec<f64> = Vec::new();
    // T4 advice bench: per match, (winner-team, loser-team) mean percentile of the
    // ACTUAL picks among their available pools under the shipped score.
    let mut t4_v: Vec<(f64, f64)> = Vec::new();
    // champ universe for the T4 pool (sorted — deterministic iteration).
    let universe: Vec<&str> = {
        let mut s: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for m in &matches {
            for a in m.blue.iter().chain(m.red.iter()) {
                s.insert(a.champ.as_str());
            }
        }
        s.into_iter().collect()
    };
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
        // S2-5: prev tables augmented with PREV-VERSION solo-rank games at w_solo
        // (champ-level only — solo has no positions). Same-version solo is OMITTED:
        // there is no cross-file play order, so including it would risk leakage;
        // this under-estimates the deployment value (conservative).
        let mut prev_c_s: [HashMap<&str, WL>; 2] = [prev_c.clone(), prev_c.clone()];
        if !legacy && g >= 1 && !solo.is_empty() {
            let pv = &vsorted[g - 1];
            for sm in &solo {
                if &vkey(&sm.version) != pv {
                    continue;
                }
                for (team, won) in [(&sm.blue, sm.blue_win), (&sm.red, !sm.blue_win)] {
                    for c in team {
                        for (k, w) in [(0usize, 0.1f64), (1, 0.25)] {
                            let e = prev_c_s[k].entry(c.as_str()).or_default();
                            e.g += w;
                            if won {
                                e.w += w;
                            }
                        }
                    }
                }
            }
        }
        // S2-6: Bradley-Terry-style joint champ strengths on the pooled train
        // window — full-batch logistic GD with L2 (deterministic, no RNG).
        let mut theta: HashMap<&str, f64> = HashMap::new();
        if !legacy {
            let train: Vec<&Match> =
                matches.iter().enumerate().filter(|(i, _)| is_train(*i, g)).map(|(_, m)| m).collect();
            if !train.is_empty() {
                for m in &train {
                    for a in m.blue.iter().chain(m.red.iter()) {
                        theta.entry(&a.champ).or_insert(0.0);
                    }
                }
                let n = train.len() as f64;
                let (lr, l2, epochs) = (2.0, 0.02, 300);
                for _ in 0..epochs {
                    let mut grad: HashMap<&str, f64> = HashMap::new();
                    for m in &train {
                        let d: f64 = m.blue.iter().map(|a| theta[a.champ.as_str()]).sum::<f64>()
                            - m.red.iter().map(|a| theta[a.champ.as_str()]).sum::<f64>();
                        let p = 1.0 / (1.0 + (-d).exp());
                        let e = p - if m.blue_win { 1.0 } else { 0.0 };
                        for a in &m.blue {
                            *grad.entry(&a.champ).or_insert(0.0) += e;
                        }
                        for a in &m.red {
                            *grad.entry(&a.champ).or_insert(0.0) -= e;
                        }
                    }
                    for (c, t) in theta.iter_mut() {
                        let gr = grad.get(c).copied().unwrap_or(0.0) / n;
                        *t -= lr * (gr + l2 * *t);
                    }
                }
            }
        }
        let empty_changed: Changed = HashMap::new();
        let (changed, carry): (&Changed, Carry) = if legacy {
            (&empty_changed, HashMap::new())
        } else {
            let label = vsorted[g].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".");
            let changed = notes.get(&label).unwrap_or(&empty_changed);
            // v4.5.0: the changed champs start from last patch's posterior moved by the patch
            (changed, prod_carry(changed, sizes.get(&label), &prev_c))
        };
        let mut cur_c: HashMap<&str, WL> = HashMap::new();
        let mut cur_cp: HashMap<(&str, usize), WL> = HashMap::new();
        // expanding CAREER tables for the shipped af producer (S0-4): athlete history
        // has no patch reset — all earlier versions + current version up to each match.
        let mut fam_x = fam.clone();
        let mut ath_x = ath_all.clone();
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
                    team.iter().map(|a| prod_champ_post(&a.champ, &cur_c, &prev_c, changed, &carry) - 0.5).sum()
                };
                let prod_l = |team: &[Ath]| -> f64 {
                    team.iter().map(|a| prod_lane_delta(a, &cur_c, &prev_c, &cur_cp, &prev_cp, changed, &carry)).sum()
                };
                t.base_prod = prod_c(&m.blue) - prod_c(&m.red);
                t.base_lane_prod = prod_l(&m.blue) - prod_l(&m.red);
                // ---- S0-4: shipped pick-score terms (retrospective full-board) ----
                // (the patch nudge left the score in v4.5.0: the carry is in delta_for)
                let ai_term = |c: &str| -> f64 {
                    let p = match ai_intr.get(c) {
                        Some(v) => *v,
                        None => return 0.0,
                    };
                    let gm = prod_games(c, &cur_c, &prev_c, changed);
                    let fresh = if gm <= 0.0 { 1.0 } else { (1.0 - gm / 15.0).max(0.0) };
                    (0.4 * p * fresh).clamp(-0.024, 0.024)
                };
                let af_c = |aid: u64, c: &str| -> f64 {
                    let cw = fam_x.get(&(aid, c)).copied().unwrap_or_default();
                    let tot = ath_x.get(&aid).copied().unwrap_or_default();
                    let wr_a = (tot.w + 4.0) / (tot.g + 8.0);
                    let famg = 0.020 * (1.0 + cw.g.min(20.0)).ln();
                    let famc = 0.15 * ((cw.w + 4.0 * wr_a) / (cw.g + 4.0) - wr_a);
                    (famg + famc).min(0.05)
                };
                let af_of = |a: &Ath| -> f64 { af_c(a.aid, &a.champ) };
                let lp_of = |a: &Ath, opp: &[Ath]| -> f64 {
                    if a.lane >= 5 {
                        return 0.0;
                    }
                    let opps: Vec<&Ath> = opp.iter().filter(|e| e.lane == a.lane).collect();
                    if opps.is_empty() {
                        return 0.0;
                    }
                    let sum: f64 = opps
                        .iter()
                        .map(|e| {
                            let z = lane_dom.get(&(a.champ.clone(), a.lane)).copied().unwrap_or(0.0)
                                - lane_dom.get(&(e.champ.clone(), e.lane)).copied().unwrap_or(0.0);
                            (0.008 * z).clamp(-0.05, 0.05)
                        })
                        .sum();
                    sum / opps.len() as f64
                };
                // comp_need replica (engine :687-728), tags-only (no RP-role data in the
                // harness), each athlete scored as the LAST pick (allies = the other N-1).
                let comp_of = |team: &[Ath]| -> f64 {
                    let has = |x: &Ath, tag: &str| {
                        taxon.get(&x.champ).map_or(false, |t| t.tags.iter().any(|g| g == tag))
                    };
                    let cat_of = |x: &Ath| taxon.get(&x.champ).map_or("", |t| t.category.as_str());
                    let mut s = 0.0;
                    for (i, a) in team.iter().enumerate() {
                        let allies: Vec<&Ath> =
                            team.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, x)| x).collect();
                        let any = |f: &dyn Fn(&Ath) -> bool| allies.iter().any(|x| f(x));
                        let is_heal = |x: &Ath| has(x, "Heal") || has(x, "Shield");
                        let mut u = 0.0;
                        if !any(&|x| has(x, "Tank")) && has(a, "Tank") {
                            u += 0.12 * 0.2;
                        } else if !any(&|x| has(x, "Frontline")) && has(a, "Frontline") {
                            u += 0.12 * 0.2;
                        }
                        if !any(&|x| has(x, "CC")) && has(a, "CC") {
                            u += 0.12 * 0.3;
                        }
                        if !any(&is_heal) && is_heal(a) {
                            u += 0.12 * 0.85;
                        }
                        if !any(&|x| has(x, "Backline") || cat_of(x) == "Range")
                            && (has(a, "Backline") || cat_of(a) == "Range")
                        {
                            u += 0.12 * 0.3;
                        }
                        s += 0.2 * u * (allies.len() as f64 / 4.0).min(1.0);
                    }
                    s
                };
                let sum_d = |f: &dyn Fn(&Ath) -> f64| -> f64 {
                    m.blue.iter().map(|a| f(a)).sum::<f64>() - m.red.iter().map(|a| f(a)).sum::<f64>()
                };
                t.dfor_prod = sum_d(&|a| {
                    prod_delta_for(&a.champ, a.lane, &cur_c, &prev_c, &cur_cp, &prev_cp, changed, &carry)
                });
                t.aip_prod = sum_d(&|a| ai_term(&a.champ));
                t.af = sum_d(&af_of);
                t.lp_prod = m.blue.iter().map(|a| lp_of(a, &m.red)).sum::<f64>()
                    - m.red.iter().map(|a| lp_of(a, &m.blue)).sum::<f64>();
                t.comp_prod = comp_of(&m.blue) - comp_of(&m.red);
                // ---- post-hoc S2 diagnostics ----
                let af_new_of = |a: &Ath| -> f64 {
                    let cw = fam_x.get(&(a.aid, a.champ.as_str())).copied().unwrap_or_default();
                    let tot = ath_x.get(&a.aid).copied().unwrap_or_default();
                    let wr_a = (tot.w + 4.0) / (tot.g + 8.0);
                    (0.15 * ((cw.w + 4.0 * wr_a) / (cw.g + 4.0) - wr_a)).min(0.05)
                };
                t.af_new = sum_d(&af_new_of);
                let stack_of = |team: &[Ath]| -> f64 {
                    let mut cnt: HashMap<&str, u32> = HashMap::new();
                    for a in team {
                        if let Some(tx) = taxon.get(&a.champ) {
                            if !tx.category.is_empty() {
                                *cnt.entry(tx.category.as_str()).or_insert(0) += 1;
                            }
                        }
                    }
                    cnt.values().copied().max().unwrap_or(0) as f64
                };
                t.stack = stack_of(&m.blue) - stack_of(&m.red);
                let tag = |a: &Ath, tg: &str| {
                    taxon.get(&a.champ).map_or(false, |tx| tx.tags.iter().any(|x| x == tg))
                };
                let front = |a: &Ath| tag(a, "Tank") || tag(a, "Frontline");
                let healf = |a: &Ath| tag(a, "Heal") || tag(a, "Shield");
                let ccf = |a: &Ath| tag(a, "CC");
                let back = |a: &Ath| {
                    tag(a, "Backline") || taxon.get(&a.champ).map_or(false, |tx| tx.category == "Range")
                };
                let has_team = |team: &[Ath], f: &dyn Fn(&Ath) -> bool| -> f64 {
                    if team.iter().any(|a| f(a)) { 1.0 } else { 0.0 }
                };
                t.c_front = has_team(&m.blue, &front) - has_team(&m.red, &front);
                t.c_heal = has_team(&m.blue, &healf) - has_team(&m.red, &healf);
                t.c_cc = has_team(&m.blue, &ccf) - has_team(&m.red, &ccf);
                t.c_back = has_team(&m.blue, &back) - has_team(&m.red, &back);
                // the sweep varies the blend's κ; a carried champ keeps its carry, so κ40 ≡ base_prod
                let kp = |kappa: f64, team: &[Ath]| -> f64 {
                    team.iter()
                        .map(|a| {
                            if carry.contains_key(a.champ.as_str()) {
                                return prod_champ_post(&a.champ, &cur_c, &prev_c, changed, &carry) - 0.5;
                            }
                            let (w, mm) = prod_blend(&a.champ, &cur_c, &prev_c, changed);
                            beta_mean(w, mm, 0.5, kappa) - 0.5
                        })
                        .sum()
                };
                kprod.push([
                    kp(20.0, &m.blue) - kp(20.0, &m.red),
                    kp(80.0, &m.blue) - kp(80.0, &m.red),
                    kp(150.0, &m.blue) - kp(150.0, &m.red),
                ]);
                let solo_side = |k: usize| -> f64 {
                    let f = |team: &[Ath]| -> f64 {
                        team.iter()
                            .map(|a| prod_champ_post(&a.champ, &cur_c, &prev_c_s[k], changed, &carry) - 0.5)
                            .sum()
                    };
                    f(&m.blue) - f(&m.red)
                };
                solo_v.push([solo_side(0), solo_side(1)]);
                let btd = |team: &[Ath]| -> f64 {
                    team.iter().map(|a| theta.get(a.champ.as_str()).copied().unwrap_or(0.0)).sum()
                };
                bt_v.push(btd(&m.blue) - btd(&m.red));
                let gsum: f64 = m
                    .blue
                    .iter()
                    .chain(m.red.iter())
                    .map(|a| prod_games(&a.champ, &cur_c, &prev_c, changed))
                    .sum();
                mgames_v.push(gsum / (m.blue.len() + m.red.len()) as f64);
                // T4 advice bench: percentile of each ACTUAL pick among its available
                // pool under the shipped board-independent score (comp/LP omitted —
                // board-context terms, ≤~0.02). Pool = champs − both bans − the other
                // 9 picks (the pick itself stays in).
                let taken4: Vec<&str> =
                    m.blue.iter().chain(m.red.iter()).map(|a| a.champ.as_str()).collect();
                let team_pct = |team: &[Ath]| -> f64 {
                    let mut sum_p = 0.0;
                    for a in team {
                        let sc = |c: &str| -> f64 {
                            0.85 * prod_delta_for(c, a.lane, &cur_c, &prev_c, &cur_cp, &prev_cp, changed, &carry)
                                + ai_term(c)
                                + af_c(a.aid, c)
                        };
                        let s_act = sc(&a.champ);
                        let (mut below, mut ties, mut n) = (0u32, 0u32, 0u32);
                        for c in &universe {
                            let c: &str = c;
                            if c != a.champ.as_str()
                                && (taken4.iter().any(|t| *t == c) || m.bans.iter().any(|b| b == c))
                            {
                                continue;
                            }
                            n += 1;
                            let s = sc(c);
                            if s < s_act {
                                below += 1;
                            } else if s == s_act {
                                ties += 1;
                            }
                        }
                        if n > 0 {
                            sum_p += (below as f64 + 0.5 * ties as f64) / n as f64;
                        }
                    }
                    sum_p / team.len().max(1) as f64
                };
                let (bp4, rp4) = (team_pct(&m.blue), team_pct(&m.red));
                t4_v.push(if m.blue_win { (bp4, rp4) } else { (rp4, bp4) });
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
                        fam_x.entry((a.aid, &a.champ)).or_default().rec(won);
                        ath_x.entry(a.aid).or_default().rec(won);
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
            "\nPRODUCTION-MATH BASE (S0-3) — patch-local: current(expanding, play order) + prev·0.35·(1−cm/40), κ40/κ20 beta_post; changed champs carried (v4.5.0: prev posterior + a + b·size, κ80) [notes: {}]:",
            if notes.is_empty() { "MISSING — no carry applied".to_string() } else { format!("{} versions", notes.len()) }
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
        println!("  PER-ORIGIN (Δ over base_lane_prod; chg = champs changed by that version's note):");
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

        // ---- S0-4: pre-registered 6-model comparison + bootstrap CI (fixed list,
        // no additions — ALGO-FIXES §S0-4). Convention: retrospective full-board
        // scoring (every champ as the LAST pick: power_ramp 0.85, mu_ramp 1, comp
        // ramp allies/4; mature career ⇒ data=1/theory=0; in-mod syn/cnt rows
        // absent ⇒ 0; RP-role OR-branch unavailable ⇒ tags-only comp).
        let m4 = |t: &Terms| 0.85 * t.dfor_prod + t.aip_prod + t.lp_prod + t.comp_prod + t.af;
        let models: [(&str, &dyn Fn(&Terms) -> f64); 6] = [
            ("① always-first-side", &|_t: &Terms| 1.0),
            ("② base_lane (pooled κ8 harness anchor)", &|t: &Terms| t.base_lane),
            ("③ ② + shipped af (.020·lng+.15·cwr cap .05)", &|t: &Terms| t.base_lane + t.af),
            ("④ shipped pick score (board-independent)", &m4),
            ("⑤ ④ − comp", &|t: &Terms| 0.85 * t.dfor_prod + t.aip_prod + t.lp_prod + t.af),
            ("⑥ ④ − LP", &|t: &Terms| 0.85 * t.dfor_prod + t.aip_prod + t.comp_prod + t.af),
        ];
        let corr: Vec<Vec<f64>> = models
            .iter()
            .map(|(_, f)| {
                recs.iter()
                    .map(|t| {
                        let s = f(t);
                        if s.abs() < 1e-12 {
                            0.5
                        } else if (s > 0.0) == t.win {
                            1.0
                        } else {
                            0.0
                        }
                    })
                    .collect()
            })
            .collect();
        let nrec = recs.len();
        let point: Vec<f64> = corr.iter().map(|c| c.iter().sum::<f64>() / nrec as f64 * 100.0).collect();
        const B: usize = 1000;
        let mut rng: u64 = 20260705; // fixed seed — deterministic, pre-registered
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut boot: Vec<[f64; 6]> = Vec::with_capacity(B);
        for _ in 0..B {
            let mut s = [0.0f64; 6];
            for _ in 0..nrec {
                let i = (next() % nrec as u64) as usize;
                for (k, sk) in s.iter_mut().enumerate() {
                    *sk += corr[k][i];
                }
            }
            for sk in s.iter_mut() {
                *sk = *sk / nrec as f64 * 100.0;
            }
            boot.push(s);
        }
        let ci = |vals: &mut Vec<f64>| -> (f64, f64) {
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
            (vals[(B as f64 * 0.025) as usize], vals[(B as f64 * 0.975) as usize])
        };
        println!("\nPRE-REGISTERED 6-MODEL COMPARISON + BOOTSTRAP CI (S0-4; {}× match resample, xorshift64 seed 20260705):", B);
        for (k, (name, _)) in models.iter().enumerate() {
            let mut v: Vec<f64> = boot.iter().map(|b| b[k]).collect();
            let (lo, hi) = ci(&mut v);
            println!("  {:<46}: {:>5.1}%  [{:>5.1}, {:>5.1}]", name, point[k], lo, hi);
        }
        println!("  PAIRED DELTAS (adoption gate: bootstrap CI lower bound > 0):");
        for (a, b, name) in [
            (1usize, 0usize, "②−① base_lane vs coin (sanity)"),
            (2, 1, "③−② shipped af contribution"),
            (3, 1, "④−② shipped bundle vs base_lane"),
            (3, 4, "④−⑤ comp contribution"),
            (3, 5, "④−⑥ LP contribution"),
        ] {
            let mut v: Vec<f64> = boot.iter().map(|r| r[a] - r[b]).collect();
            let (lo, hi) = ci(&mut v);
            let gate = if lo > 0.0 { "PASS" } else { "FAIL (미입증 — 예산 항목, 키우기 금지)" };
            println!("    {:<34}: {:>+5.1}  [{:>+5.1}, {:>+5.1}]  {}", name, point[a] - point[b], lo, hi, gate);
        }

        // ---- POST-HOC S2 DIAGNOSTICS (NOT pre-registered — S2 decision tests) ----
        let diag: [(&str, &dyn Fn(&Terms) -> f64); 9] = [
            ("base_lane (anchor A)", &|t: &Terms| t.base_lane),
            ("A + af_new", &|t: &Terms| t.base_lane + t.af_new),
            ("base_lane_prod (anchor B)", &|t: &Terms| t.base_lane_prod),
            ("B + af_new", &|t: &Terms| t.base_lane_prod + t.af_new),
            ("B + 0.01·stack", &|t: &Terms| t.base_lane_prod + 0.01 * t.stack),
            ("B + 0.02·heal_gap", &|t: &Terms| t.base_lane_prod + 0.02 * t.c_heal),
            ("B + 0.02·cc_gap", &|t: &Terms| t.base_lane_prod + 0.02 * t.c_cc),
            ("B + 0.02·front_gap", &|t: &Terms| t.base_lane_prod + 0.02 * t.c_front),
            ("B + 0.02·backline_gap", &|t: &Terms| t.base_lane_prod + 0.02 * t.c_back),
        ];
        let dcorr: Vec<Vec<f64>> = diag
            .iter()
            .map(|(_, f)| {
                recs.iter()
                    .map(|t| {
                        let s = f(t);
                        if s.abs() < 1e-12 {
                            0.5
                        } else if (s > 0.0) == t.win {
                            1.0
                        } else {
                            0.0
                        }
                    })
                    .collect()
            })
            .collect();
        let dpoint: Vec<f64> = dcorr.iter().map(|c| c.iter().sum::<f64>() / nrec as f64 * 100.0).collect();
        let mut rng2: u64 = 20260705; // same seed → same index draws → paired
        let mut next2 = move || {
            rng2 ^= rng2 << 13;
            rng2 ^= rng2 >> 7;
            rng2 ^= rng2 << 17;
            rng2
        };
        let mut dboot: Vec<[f64; 9]> = Vec::with_capacity(B);
        for _ in 0..B {
            let mut s = [0.0f64; 9];
            for _ in 0..nrec {
                let i = (next2() % nrec as u64) as usize;
                for (k, sk) in s.iter_mut().enumerate() {
                    *sk += dcorr[k][i];
                }
            }
            for sk in s.iter_mut() {
                *sk = *sk / nrec as f64 * 100.0;
            }
            dboot.push(s);
        }
        println!("\nPOST-HOC S2 DIAGNOSTICS (not pre-registered; paired bootstrap, same seed):");
        for (a, b, name, is_gate) in [
            (1usize, 0usize, "af_new over base_lane (F15 verify)", false),
            (3, 2, "af_new over prod anchor (F15 verify)", false),
            (4, 2, "0.01·stack (S2-7)", true),
            (5, 2, "0.02·heal_gap (S2-8)", true),
            (6, 2, "0.02·cc_gap (S2-8)", true),
            (7, 2, "0.02·front_gap (S2-8)", true),
            (8, 2, "0.02·backline_gap (S2-8)", true),
        ] {
            let mut v: Vec<f64> = dboot.iter().map(|r| r[a] - r[b]).collect();
            let (lo, hi) = ci(&mut v);
            let verdict = if is_gate {
                if lo > 0.0 { "ADOPT" } else { "REJECT (미입증)" }
            } else {
                ""
            };
            println!(
                "  {:<36}: {:>4.1}% → {:>4.1}%  Δ{:>+5.1} [{:>+5.1}, {:>+5.1}]  {}",
                name, dpoint[b], dpoint[a], dpoint[a] - dpoint[b], lo, hi, verdict
            );
        }
        // S2-3: κ sweep on the production champ state (side-channel aligned with recs)
        if kprod.len() == recs.len() {
            let kacc = |j: usize| -> f64 {
                let mut c = 0.0;
                for (t, k) in recs.iter().zip(kprod.iter()) {
                    let s = k[j];
                    if s.abs() < 1e-12 {
                        c += 0.5;
                    } else if (s > 0.0) == t.win {
                        c += 1.0;
                    }
                }
                c / nrec as f64 * 100.0
            };
            println!(
                "  S2-3 κ sweep (champ prod state): κ20 {:.1}% · κ40 {:.1}% (ship) · κ80 {:.1}% · κ150 {:.1}%",
                kacc(0),
                acc(&|t: &Terms| t.base_prod),
                kacc(1),
                kacc(2)
            );
        }

        // ---- S2-5 / S2-6 experiments (post-hoc; preliminary until the S0-2 recapture) ----
        if solo_v.len() == recs.len() && bt_v.len() == recs.len() {
            println!(
                "\nS2-5/S2-6 EXPERIMENTS (post-hoc; solo file: {} matches{}):",
                solo.len(),
                if solo.is_empty() { " — MISSING → solo variants ≡ base_prod" } else { "" }
            );
            let mk = |f: &dyn Fn(usize) -> f64| -> Vec<f64> {
                recs.iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let s = f(i);
                        if s.abs() < 1e-12 {
                            0.5
                        } else if (s > 0.0) == t.win {
                            1.0
                        } else {
                            0.0
                        }
                    })
                    .collect()
            };
            let ecorr: Vec<Vec<f64>> = vec![
                mk(&|i: usize| recs[i].base_prod),
                mk(&|i: usize| solo_v[i][0]),
                mk(&|i: usize| solo_v[i][1]),
                mk(&|i: usize| bt_v[i]),
            ];
            let epoint: Vec<f64> = ecorr.iter().map(|c| c.iter().sum::<f64>() / nrec as f64 * 100.0).collect();
            let mut sorted_g: Vec<f64> = mgames_v.clone();
            sorted_g.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let med = sorted_g[nrec / 2];
            let strat = |corr: &[f64], thin: bool| -> (f64, usize) {
                let (mut c, mut k) = (0.0, 0usize);
                for (i, v) in corr.iter().enumerate() {
                    if (mgames_v[i] < med) == thin {
                        c += v;
                        k += 1;
                    }
                }
                (if k > 0 { c / k as f64 * 100.0 } else { 50.0 }, k)
            };
            let mut rng3: u64 = 20260705;
            let mut next3 = move || {
                rng3 ^= rng3 << 13;
                rng3 ^= rng3 >> 7;
                rng3 ^= rng3 << 17;
                rng3
            };
            let mut eboot: Vec<[f64; 4]> = Vec::with_capacity(B);
            for _ in 0..B {
                let mut s = [0.0f64; 4];
                for _ in 0..nrec {
                    let i = (next3() % nrec as u64) as usize;
                    for (k, sk) in s.iter_mut().enumerate() {
                        *sk += ecorr[k][i];
                    }
                }
                for sk in s.iter_mut() {
                    *sk = *sk / nrec as f64 * 100.0;
                }
                eboot.push(s);
            }
            for (k, name) in [
                (1usize, "S2-5 w_solo=0.10 (prev-ver solo)"),
                (2, "S2-5 w_solo=0.25 (prev-ver solo)"),
                (3, "S2-6 BT θ standalone"),
            ] {
                let mut v: Vec<f64> = eboot.iter().map(|r| r[k] - r[0]).collect();
                let (lo, hi) = ci(&mut v);
                let (thin_x, tn) = strat(&ecorr[k], true);
                let (thin_b, _) = strat(&ecorr[0], true);
                let (thk_x, _) = strat(&ecorr[k], false);
                let (thk_b, _) = strat(&ecorr[0], false);
                println!(
                    "  {:<34}: {:>4.1}% vs base_prod {:>4.1}%  Δ{:>+5.1} [{:>+5.1}, {:>+5.1}] · thin(n={}) Δ{:>+5.1} · thick Δ{:>+5.1}  {}",
                    name,
                    epoint[k],
                    epoint[0],
                    epoint[k] - epoint[0],
                    lo,
                    hi,
                    tn,
                    thin_x - thin_b,
                    thk_x - thk_b,
                    if lo > 0.0 { "ADOPT-candidate" } else { "REJECT (미입증)" }
                );
            }
            println!(
                "  (θ̂ 비교 축: pooled base κ8 {:.1}% · base_lane {:.1}% — BT는 base 대체 후보로만 평가, 스케일이 달라 블렌드는 무의미)",
                acc(&|t: &Terms| t.base),
                acc(&|t: &Terms| t.base_lane)
            );
        }

        // ---- T4 advice bench (PLAN Phase 0b — offline grader of the PICK RANKING) ----
        if t4_v.len() == recs.len() && !t4_v.is_empty() {
            let n4 = t4_v.len() as f64;
            let wm = t4_v.iter().map(|p| p.0).sum::<f64>() / n4;
            let lm = t4_v.iter().map(|p| p.1).sum::<f64>() / n4;
            let dvec: Vec<f64> = t4_v.iter().map(|p| p.0 - p.1).collect();
            let rate = t4_v
                .iter()
                .map(|p| {
                    if p.0 > p.1 {
                        1.0
                    } else if p.0 == p.1 {
                        0.5
                    } else {
                        0.0
                    }
                })
                .sum::<f64>()
                / n4
                * 100.0;
            let mut rng4: u64 = 20260705;
            let mut next4 = move || {
                rng4 ^= rng4 << 13;
                rng4 ^= rng4 >> 7;
                rng4 ^= rng4 << 17;
                rng4
            };
            let mut b4: Vec<f64> = Vec::with_capacity(B);
            for _ in 0..B {
                let mut s = 0.0;
                for _ in 0..t4_v.len() {
                    let i = (next4() % t4_v.len() as u64) as usize;
                    s += dvec[i];
                }
                b4.push(s / n4);
            }
            let (lo, hi) = ci(&mut b4);
            println!("\nT4 ADVICE BENCH (post-hoc; shipped board-independent score, comp/LP omitted; pool = champs − bans − other picks):");
            println!(
                "  winner-team mean pick percentile {:.3} vs loser {:.3} · Δ{:+.3} [{:+.3}, {:+.3}] · winner-higher {:.1}%",
                wm,
                lm,
                wm - lm,
                lo,
                hi,
                rate
            );
            println!("  (Δ>0 = 승팀의 실픽이 우리 랭킹에서 더 상위 — 픽 랭킹 축이 승리와 정합. 가중 변경 시의 회귀 기준선.)");
        }

        // ---- T-ban behavioral grader (ALGO-FIXES §밴 레이어 검증): do the
        // production opponent-model shapes — per-team usage × wr-excess (score_ban
        // :966-975) + athlete pocket (0.015·ln, max over starters) — predict the
        // ENEMY's actual pick set better than global pickrate? Chronological
        // expanding (id order), strictly-prior tables, both sides predicted.
        {
            let mut order2: Vec<usize> = (0..matches.len()).collect();
            order2.sort_by_key(|&i| matches[i].id);
            let mut gl_g: HashMap<&str, f64> = HashMap::new();
            let mut tm: HashMap<(u32, &str), WL> = HashMap::new();
            let mut tm_tot: HashMap<u32, f64> = HashMap::new();
            let mut ath_g: HashMap<(u64, &str), f64> = HashMap::new();
            let mut tb: Vec<[f64; 3]> = Vec::new(); // recall@5: [global, opp, opp+pocket]
            let mut skipped = 0usize;
            for &i in &order2 {
                let m = &matches[i];
                for (enemy, tid) in [(&m.blue, m.blue_tid), (&m.red, m.red_tid)] {
                    let tg = tm_tot.get(&tid).copied().unwrap_or(0.0);
                    if tg < 20.0 {
                        skipped += 1;
                        continue;
                    }
                    let scored: Vec<(&str, f64, f64, f64)> = universe
                        .iter()
                        .map(|&c| {
                            let g = gl_g.get(c).copied().unwrap_or(0.0);
                            let w = tm.get(&(tid, c)).copied().unwrap_or_default();
                            let usage = (w.g + 1.0) / (tg + 3.0);
                            let wr = (w.w + 3.0) / (w.g + 6.0);
                            let opp = 0.08 * usage * (1.0 + 2.0 * (wr - 0.5));
                            let pocket = enemy
                                .iter()
                                .map(|a| {
                                    let ag = ath_g.get(&(a.aid, c)).copied().unwrap_or(0.0);
                                    0.015 * (1.0 + ag.min(20.0)).ln()
                                })
                                .fold(0.0f64, f64::max);
                            (c, g, opp, opp + pocket)
                        })
                        .collect();
                    let actual: Vec<&str> = enemy.iter().map(|a| a.champ.as_str()).collect();
                    let recall = |key: &dyn Fn(&(&str, f64, f64, f64)) -> f64| -> f64 {
                        let mut v = scored.clone();
                        v.sort_by(|a, b| {
                            key(b)
                                .partial_cmp(&key(a))
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then(a.0.cmp(b.0))
                        });
                        let top: Vec<&str> = v.iter().take(5).map(|x| x.0).collect();
                        actual.iter().filter(|c| top.contains(*c)).count() as f64
                            / actual.len().max(1) as f64
                    };
                    tb.push([recall(&|x| x.1), recall(&|x| x.2), recall(&|x| x.3)]);
                }
                for (team, tid, won_team) in
                    [(&m.blue, m.blue_tid, m.blue_win), (&m.red, m.red_tid, !m.blue_win)]
                {
                    for a in team {
                        *gl_g.entry(&a.champ).or_insert(0.0) += 1.0;
                        tm.entry((tid, &a.champ)).or_default().rec(won_team);
                        *tm_tot.entry(tid).or_insert(0.0) += 1.0;
                        *ath_g.entry((a.aid, &a.champ)).or_insert(0.0) += 1.0;
                    }
                }
            }
            if !tb.is_empty() {
                let ntb = tb.len() as f64;
                let mean = |j: usize| tb.iter().map(|r| r[j]).sum::<f64>() / ntb;
                let (ma, mb, mc) = (mean(0), mean(1), mean(2));
                let mut rng5: u64 = 20260705;
                let mut next5 = move || {
                    rng5 ^= rng5 << 13;
                    rng5 ^= rng5 >> 7;
                    rng5 ^= rng5 << 17;
                    rng5
                };
                let mut d1: Vec<f64> = Vec::with_capacity(B);
                let mut d2: Vec<f64> = Vec::with_capacity(B);
                for _ in 0..B {
                    let (mut s1, mut s2) = (0.0, 0.0);
                    for _ in 0..tb.len() {
                        let i = (next5() % tb.len() as u64) as usize;
                        s1 += tb[i][1] - tb[i][0];
                        s2 += tb[i][2] - tb[i][1];
                    }
                    d1.push(s1 / ntb);
                    d2.push(s2 / ntb);
                }
                let (lo1, hi1) = ci(&mut d1);
                let (lo2, hi2) = ci(&mut d2);
                println!(
                    "\nT-BAN BEHAVIORAL GRADER (post-hoc; chronological, enemy history ≥20 champ-games; {} preds, {} skipped):",
                    tb.len(),
                    skipped
                );
                println!(
                    "  recall@5 of the enemy's actual picks: global-pickrate {:.3} · team-usage OPP {:.3} · OPP+pocket {:.3}",
                    ma, mb, mc
                );
                println!(
                    "    OPP − global  : Δ{:+.3} [{:+.3}, {:+.3}]  {}",
                    mb - ma,
                    lo1,
                    hi1,
                    if lo1 > 0.0 { "PASS — 상대 모델에 실제 예측 신호" } else { "미입증" }
                );
                println!(
                    "    +pocket − OPP : Δ{:+.3} [{:+.3}, {:+.3}]  {}",
                    mc - mb,
                    lo2,
                    hi2,
                    if lo2 > 0.0 { "PASS — pocket 증분 실재" } else { "미입증" }
                );
            }
        }

        if !notes.is_empty() {
            // per-version champ tables for S2-1/S2-2 (early window needs id order)
            let mut order: Vec<usize> = (0..matches.len()).collect();
            order.sort_by_key(|&i| matches[i].id);
            let mut vwl: HashMap<(usize, String), WL> = HashMap::new();
            let mut early20: HashMap<(usize, String), WL> = HashMap::new();
            for &i in &order {
                let m = &matches[i];
                let gv = groups[i];
                for (team, won) in [(&m.blue, m.blue_win), (&m.red, !m.blue_win)] {
                    for a in team {
                        vwl.entry((gv, a.champ.clone())).or_default().rec(won);
                        let e = early20.entry((gv, a.champ.clone())).or_default();
                        if e.g < 20.0 {
                            e.rec(won);
                        }
                    }
                }
            }
            // S2-2 T-nudge: does the SIGNED change count predict the realized delta
            // shift across the boundary? (κ8-shrunk per-version deltas, ≥10 games both)
            let (mut xs, mut ys) = (Vec::new(), Vec::new());
            for g in 1..n_groups {
                let label = vsorted[g].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".");
                if let Some(note) = notes.get(&label) {
                    for (c, (_, b, nrf)) in note {
                        let cur = vwl.get(&(g, c.clone())).copied().unwrap_or_default();
                        let prev = vwl.get(&(g - 1, c.clone())).copied().unwrap_or_default();
                        if cur.g >= 10.0 && prev.g >= 10.0 {
                            xs.push(*b as f64 - *nrf as f64);
                            ys.push(cur.delta(8.0) - prev.delta(8.0));
                        }
                    }
                }
            }
            if xs.len() >= 8 {
                let nn = xs.len() as f64;
                let (mx, my) = (xs.iter().sum::<f64>() / nn, ys.iter().sum::<f64>() / nn);
                let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
                let sxx: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
                let syy: f64 = ys.iter().map(|y| (y - my) * (y - my)).sum();
                let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
                let r = if sxx > 0.0 && syy > 0.0 { sxy / (sxx.sqrt() * syy.sqrt()) } else { 0.0 };
                println!(
                    "  S2-2 T-nudge: realized Δdelta vs signed change-count — slope {:+.4}/chg, r {:+.2}, n {}  → {}",
                    slope,
                    r,
                    xs.len(),
                    if slope > 0.0 && r > 0.15 {
                        "count-scaling has signal → adopt S2-2 sizes"
                    } else {
                        "flat → keep uniform ±0.02"
                    }
                );
            }
            // S2-1 T-amnesia: predict a changed champ's FIRST-20-games winrate in the
            // new patch: (reset 0.5) vs (full 0.35·prev carry) vs (count-scaled carry).
            let (mut cnt2, mut mae2) = ([0u32; 2], [[0.0f64; 3]; 2]);
            for g in 1..n_groups {
                let label = vsorted[g].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".");
                if let Some(note) = notes.get(&label) {
                    for (c, (_, b, nrf)) in note {
                        let nch = (b + nrf) as f64;
                        let act = early20.get(&(g, c.clone())).copied().unwrap_or_default();
                        let prev = vwl.get(&(g - 1, c.clone())).copied().unwrap_or_default();
                        if act.g < 5.0 || prev.g < 5.0 {
                            continue;
                        }
                        let p_act = act.w / act.g;
                        let stratum = if nch <= 2.0 { 0 } else { 1 };
                        let wsc = PROD_PREV_W * (1.0 - nch / 4.0).max(0.0);
                        let preds = [
                            0.5,
                            beta_mean(PROD_PREV_W * prev.w, PROD_PREV_W * prev.g, 0.5, PROD_KAPPA_CHAMP),
                            beta_mean(wsc * prev.w, wsc * prev.g, 0.5, PROD_KAPPA_CHAMP),
                        ];
                        cnt2[stratum] += 1;
                        for (j, p) in preds.iter().enumerate() {
                            mae2[stratum][j] += (p - p_act).abs();
                        }
                    }
                }
            }
            for (si, sname) in ["n≤2 (small tweaks)", "n≥3 (big changes)"].iter().enumerate() {
                if cnt2[si] > 0 {
                    let k = cnt2[si] as f64;
                    println!(
                        "  S2-1 T-amnesia {:<18}: n={:<3} MAE reset {:.3} · full-0.35prev {:.3} · count-scaled {:.3}  → {}",
                        sname,
                        cnt2[si],
                        mae2[si][0] / k,
                        mae2[si][1] / k,
                        mae2[si][2] / k,
                        if si == 0 && mae2[si][2] / k < mae2[si][0] / k {
                            "scaled beats reset → S2-1 adopt candidate"
                        } else {
                            "reset fine"
                        }
                    );
                }
            }
        }
    }
    if !legacy {
        let fresh = e7_records(&matches, &groups, &vsorted, &taxon, &ai_intr, &notes, false);
        let career = e7_records(&matches, &groups, &vsorted, &taxon, &ai_intr, &notes, true);
        e7_report(&fresh, &career);
        if notes_path.is_some() {
            let (recs, patches) = e7b_records(&matches, &groups, &vsorted, &taxon, &notes, &sizes);
            e7b_report(&recs, &patches, &e7b_holdout);
        }
    }
    println!("\n>> A term EARNS engine weight only if it lifts base_lane beyond ~±{:.1}%p noise.", 50.0 / n.sqrt());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wl(w: f64, g: f64) -> WL {
        WL { w, g }
    }

    /// last patch: archer won 36 of 60, all at Bottom (lane 3) — κ40 posterior 0.56
    fn last_patch() -> (HashMap<&'static str, WL>, HashMap<(&'static str, usize), WL>) {
        (HashMap::from([("archer", wl(36.0, 60.0))]), HashMap::from([(("archer", 3), wl(36.0, 60.0))]))
    }

    fn archer_changed() -> Changed {
        HashMap::from([("archer".to_string(), ('+', 2, 0))])
    }

    #[test]
    fn carry_moves_the_last_posterior_by_the_patch_size() {
        let (prev_c, _) = last_patch();
        let sized = (true, HashMap::from([("archer".to_string(), 50.0)]));
        let carry = prod_carry(&archer_changed(), Some(&sized), &prev_c);
        // 0.56 − 0.0041 + 0.00072·50
        assert!((carry["archer"] - 0.5919).abs() < 1e-12, "{:?}", carry);
    }

    #[test]
    fn carry_without_numbers_moves_by_the_count_nudge() {
        let (prev_c, _) = last_patch();
        let unsized_note = (false, HashMap::from([("archer".to_string(), 0.0)]));
        for sizes in [Some(&unsized_note), None] {
            let carry = prod_carry(&archer_changed(), sizes, &prev_c);
            // 0.56 + (0.01 + 0.005·2) for a 2-line buff
            assert!((carry.get("archer").copied().unwrap_or(0.0) - 0.58).abs() < 1e-12, "{:?}", carry);
        }
    }

    #[test]
    fn carry_stays_inside_the_clamp() {
        let prev_c = HashMap::from([("archer", wl(95.0, 100.0))]);
        let sized = (true, HashMap::from([("archer".to_string(), 100.0)]));
        let carry = prod_carry(&archer_changed(), Some(&sized), &prev_c);
        assert_eq!(carry.get("archer").copied(), Some(0.8));
    }

    #[test]
    fn carried_champ_starts_from_its_carry() {
        let (prev_c, prev_cp) = last_patch();
        let (cur_c, cur_cp) = (HashMap::new(), HashMap::new());
        let carry: Carry = HashMap::from([("archer".to_string(), 0.6)]);
        let ch = archer_changed();
        let d = |lane| prod_delta_for("archer", lane, &cur_c, &prev_c, &cur_cp, &prev_cp, &ch, &carry);
        assert!((prod_champ_post("archer", &cur_c, &prev_c, &ch, &carry) - 0.6).abs() < 1e-12);
        assert!((d(3) - 0.1).abs() < 1e-12, "kept lane {}", d(3));
        assert!((d(0) - 0.025).abs() < 1e-12, "unplayed lane ×0.25 {}", d(0));
        assert!((d(9) - 0.1).abs() < 1e-12, "no lane {}", d(9));
        let top = Ath { champ: "archer".to_string(), lane: 0, aid: 1, fit: 0.0 };
        let l = prod_lane_delta(&top, &cur_c, &prev_c, &cur_cp, &prev_cp, &ch, &carry);
        assert!((l - 0.1).abs() < 1e-12, "S0-3 lane falls back to the champ {}", l);
    }

    #[test]
    fn carried_champ_updates_at_strength_80() {
        let (prev_c, prev_cp) = last_patch();
        let cur_c = HashMap::from([("archer", wl(8.0, 10.0))]);
        let cur_cp = HashMap::from([(("archer", 3), wl(8.0, 10.0))]);
        let carry: Carry = HashMap::from([("archer".to_string(), 0.6)]);
        let ch = archer_changed();
        let champ = (8.0 + 0.6 * 80.0) / (10.0 + 80.0);
        let lane = (8.0 + champ * 20.0) / (10.0 + 20.0) - 0.5;
        assert!((prod_champ_post("archer", &cur_c, &prev_c, &ch, &carry) - champ).abs() < 1e-12);
        let d = prod_delta_for("archer", 3, &cur_c, &prev_c, &cur_cp, &prev_cp, &ch, &carry);
        assert!((d - lane).abs() < 1e-12, "{} vs {}", d, lane);
    }

    #[test]
    fn empty_carry_keeps_the_pre_450_reset() {
        // E7a and E7b's P2 arm: a changed champ drops last patch → neutral at no games
        let (prev_c, prev_cp) = last_patch();
        let (cur_c, cur_cp) = (HashMap::new(), HashMap::new());
        let d = prod_delta_for("archer", 3, &cur_c, &prev_c, &cur_cp, &prev_cp, &archer_changed(), &HashMap::new());
        assert!(d.abs() < 1e-12, "{}", d);
    }

    #[test]
    fn carry_leaves_unchanged_champs_alone() {
        let prev_c = HashMap::from([("monk", wl(30.0, 50.0))]);
        let prev_cp = HashMap::from([(("monk", 4), wl(30.0, 50.0))]);
        let (cur_c, cur_cp) = (HashMap::new(), HashMap::new());
        let carry: Carry = HashMap::from([("archer".to_string(), 0.6)]);
        let ch = archer_changed();
        let with = prod_delta_for("monk", 4, &cur_c, &prev_c, &cur_cp, &prev_cp, &ch, &carry);
        let without = prod_delta_for("monk", 4, &cur_c, &prev_c, &cur_cp, &prev_cp, &ch, &HashMap::new());
        assert_eq!(with, without);
    }
}
