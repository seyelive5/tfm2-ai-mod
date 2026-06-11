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
const KAPPA_CHAMP: f64 = 40.0;
const KAPPA_LANE: f64 = 20.0;
const LCB_Z: f64 = 1.0;

fn beta_post(w: f64, m: f64, prior_mean: f64, kappa: f64) -> (f64, f64) {
    let pm = prior_mean.clamp(0.001, 0.999);
    let a = w.max(0.0) + pm * kappa;
    let b = (m - w).max(0.0) + (1.0 - pm) * kappa;
    let t = a + b;
    (a / t, (a * b / (t * t * (t + 1.0))).sqrt())
}
fn beta_lcb(w: f64, m: f64, prior_mean: f64, kappa: f64, z: f64) -> f64 {
    let (mean, sd) = beta_post(w, m, prior_mean, kappa);
    (mean - z * sd).clamp(0.0, 1.0)
}

fn vkey(v: &str) -> Vec<u32> {
    v.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect()
}

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
    let conf = (total as f64 / CONF_FULL).min(1.0);
    let prev_w = PREV_PATCH_WEIGHT * (1.0 - conf);
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
            prev_w
        } else {
            continue;
        };
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
    let c_rows = by
        .iter()
        .map(|(c, (w, m))| {
            let lcb = beta_lcb(*w, *m, 0.5, KAPPA_CHAMP, LCB_Z);
            format!("C\t{}\t{:.3}\t{}", c, lcb - 0.5, m.round() as u32)
        })
        .collect();
    let champ_mean: BTreeMap<String, f64> =
        by.iter().map(|(c, (w, m))| (c.clone(), beta_post(*w, *m, 0.5, KAPPA_CHAMP).0)).collect();
    let cp_rows = byp
        .iter()
        .filter(|(_, (_, m))| *m > 0.0)
        .map(|((c, li), (w, m))| {
            let prior = champ_mean.get(c).copied().unwrap_or(0.5);
            let lcb = beta_lcb(*w, *m, prior, KAPPA_LANE, LCB_Z);
            format!("CP\t{}\t{}\t{:.3}\t{}", c, li, lcb - 0.5, m.round() as u32)
        })
        .collect();
    (c_rows, cp_rows, total)
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

/// B rows: SEASON banrate per champ (bans summed over all patch versions ÷ the
/// season's total matches) — a single fresh patch's handful of games must not
/// swing the W_BAN_META term.
fn compute_banrate(patch: &str) -> Vec<String> {
    let rows: Vec<Vec<&str>> = patch
        .lines()
        .skip(1)
        .map(|l| l.split('\t').collect::<Vec<&str>>())
        .filter(|f| f.len() >= 7)
        .collect();
    // per-version total (first row of that version) + per-(version,champ) bans
    let mut ver_total: BTreeMap<&str, f64> = BTreeMap::new();
    let mut bans: BTreeMap<(&str, &str), f64> = BTreeMap::new();
    for f in &rows {
        ver_total.entry(f[0]).or_insert_with(|| f[1].parse().unwrap_or(0.0));
        bans.entry((f[0], f[2])).or_insert_with(|| f[4].parse().unwrap_or(0.0));
    }
    let season_total: f64 = ver_total.values().sum();
    if season_total <= 0.0 {
        return vec![];
    }
    let mut per_champ: BTreeMap<&str, f64> = BTreeMap::new();
    for ((_, c), b) in &bans {
        *per_champ.entry(c).or_insert(0.0) += b;
    }
    per_champ.iter().map(|(c, b)| format!("B\t{}\t{:.3}", c, b / season_total)).collect()
}

/// Patch-change info handed over by champ_changes_update (lib.rs).
#[derive(Default, Clone)]
pub struct ChgInfo {
    pub anchor: Option<String>,
    /// (champ, dir): '+' buff '-' nerf '~' mixed 'R' rework
    pub dirs: Vec<(String, char)>,
}

/// Build the engine Tables from the in-memory stats TSV + live pool + patch
/// changes. Baked T (taxonomy) rows come from the shared engine's WEIGHTS_TSV.
pub fn build_tables(patch_tsv: &str, pool: &[String], chg: &ChgInfo) -> Tables {
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
    let pos_rows = compute_pos(patch_tsv, &reworked, anchor);
    if pos_rows.is_empty() {
        out.extend(engine::WEIGHTS_TSV.lines().filter(|l| l.starts_with("POS\t")).map(|s| s.to_string()));
    } else {
        out.extend(pos_rows);
    }
    out.extend(compute_roles(patch_tsv, &reworked, anchor));
    out.extend(compute_banrate(patch_tsv));
    out.extend(chg.dirs.iter().map(|(c, d)| format!("CHG\t{}\t{}", c, d)));
    if !pool.is_empty() {
        out.push(format!("POOL\t{}", pool.join(",")));
    }
    out.push(format!("M\t{}", total));
    out.push("SAVE\tlive".to_string());
    parse_tables_from_str(&out.join("\n"))
}

// ───────────────────── live draft state + recommendations ─────────────────────

/// Board read each frame from MatchUIRunner (already side-resolved: my/enemy).
#[derive(Default, Clone, PartialEq)]
pub struct Board {
    pub my_ban: Vec<String>,
    pub enemy_ban: Vec<String>,
    pub my_pick: Vec<String>,
    pub enemy_pick: Vec<String>,
    pub ban_count: usize,
    pub teams: String,
    pub locked: Vec<String>,
}

/// Cross-frame fearless/series/first-pick tracking (ported from the overlay).
#[derive(Default)]
struct Series {
    teams: String,
    game: u32,
    my: HashSet<String>,
    enemy: HashSet<String>,
    full_board: Option<(Vec<String>, Vec<String>)>,
    first_pick: Option<bool>,
    hard: bool,
    fearless_on: bool,
    /// false after the lock-recovery branch (mod started mid-series): the
    /// my/enemy split is then a guess — display totals, not the split.
    unattributed: bool,
}

/// Everything the in-game display needs this frame.
#[derive(Default, Clone)]
pub struct Output {
    pub rec_ban: bool,
    /// ranked (id, ko, signed score string)
    pub recs: Vec<(String, String, String)>,
    /// "라인 이름" pairs once my 5 picks are in
    pub swap: Vec<String>,
    /// commentary lines ('\n'-joined)
    pub say: String,
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

    // ── series / fearless tracking. board.teams includes the MATCH id, so a
    // rematch against the same team pair is a NEW series, not a phantom next set.
    if board.teams != s.teams {
        s.teams = board.teams.clone();
        s.game = 1;
        s.my.clear();
        s.enemy.clear();
        s.full_board = None;
        s.first_pick = None;
        s.fearless_on = false;
        s.hard = false;
        s.unattributed = false;
    } else if let Some((pm, pe)) = s.full_board.take() {
        let total_now =
            board.my_pick.len() + board.enemy_pick.len() + board.my_ban.len() + board.enemy_ban.len();
        // require a CLEARLY smaller board: a one-slot transient misread (9 < 10)
        // must not fire a phantom set transition.
        if total_now + 3 < pm.len() + pe.len() {
            s.game += 1;
            s.my.extend(pm);
            s.enemy.extend(pe);
            s.first_pick = None;
        } else {
            // not (yet) a reset — PUT THE BOARD BACK and keep waiting, otherwise a
            // single non-triggering frame would silently drop the whole set.
            s.full_board = Some((pm, pe));
        }
    }
    if board.my_pick.len() == 5 && board.enemy_pick.len() == 5 {
        s.full_board = Some((board.my_pick.clone(), board.enemy_pick.clone()));
    }
    if board.my_pick.len() + board.enemy_pick.len() == 1 && s.first_pick.is_none() {
        s.first_pick = Some(board.my_pick.len() == 1);
    }
    if !board.locked.is_empty() {
        s.fearless_on = true;
        // style RECOMPUTED every pass (the old overlay's semantics) — a single
        // mis-read frame must not latch Hard for the whole series.
        s.hard = s.enemy.iter().filter(|c| !s.my.contains(*c)).any(|c| board.locked.contains(c));
        if s.my.is_empty() && s.enemy.is_empty() {
            // mod started mid-series: adopt the game's lock list wholesale. The
            // my/enemy split is unknowable here (in HARD the union filters
            // correctly anyway) — flag it so the display doesn't fake a split.
            s.my = board.locked.iter().cloned().collect();
            s.unattributed = true;
            if s.game == 1 {
                s.game = 2;
            }
        }
    } else if s.game == 1 {
        s.fearless_on = false;
        s.hard = false;
    }

    // ── recompute only when the board or stats changed
    let key = format!(
        "{}|{}|{}|{}|{}|{}|{:?}|{}|{}",
        board.my_ban.join(","),
        board.enemy_ban.join(","),
        board.my_pick.join(","),
        board.enemy_pick.join(","),
        board.locked.join(","),
        tables_gen,
        s.first_pick,
        s.game,
        s.hard,
    );
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

    // fearless lock filtering happens through the POOL argument (engine design):
    // pick recs drop what I can't pick; ban recs drop what the ENEMY can't pick.
    // CRITICAL: gated on fearless_on — series tracking accumulates picks in ANY
    // multi-set match, but locks only exist under the fearless rule (a non-
    // fearless Bo3 may re-pick set-1 champs freely).
    let (my_locked, enemy_locked): (HashSet<String>, HashSet<String>) = if s.fearless_on {
        let ml: HashSet<String> = if s.hard {
            s.my.union(&s.enemy).cloned().collect()
        } else {
            s.my.clone()
        };
        let el = if s.hard { ml.clone() } else { s.enemy.clone() };
        (ml, el)
    } else {
        (HashSet::new(), HashSet::new())
    };
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
        Some(recommend_lanes(tables, &st, false, pool_opt, s.fearless_on))
    };

    // ── recs (id, ko, score)
    let entry = |id: &str, ko: &str, score: f32| (id.to_string(), ko.to_string(), format!("{:+.0}", score * 100.0));
    let mut recs: Vec<(String, String, String)> = Vec::new();
    if let Some(lv) = lane_view.as_ref().filter(|lv| lv.has_pos) {
        if let Some(star) = lv.lanes.iter().find(|l| l.star) {
            recs.extend(star.cands.iter().take(3).map(|c| entry(&c.id, &c.ko, c.score)));
        }
        for l in lv.lanes.iter().filter(|l| !l.star && l.filled.is_none()) {
            if let Some(c) = l.cands.first() {
                if !recs.iter().any(|(id, _, _)| *id == c.id) {
                    recs.push(entry(&c.id, &c.ko, c.score));
                }
            }
        }
        recs.truncate(5);
    } else {
        recs.extend(view.rows.iter().take(5).map(|r| entry(&r.id, &r.ko, r.score)));
    }

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
    if !swap.is_empty() {
        parts.push("픽이 끝났습니다. 전체 승률이 가장 높아지는 배치는 위 추천 배치입니다.".to_string());
    } else if let Some(lv) = lane_view.as_ref().filter(|lv| lv.has_pos) {
        if let Some(star) = lv.lanes.iter().find(|l| l.star) {
            if let Some(c) = star.cands.first() {
                let mut t = format!("{} 라인을 먼저 확보하는 편이 좋아 보입니다. 1순위는 {}", star.name, c.ko);
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
                if c.low_conf {
                    t.push_str(". 다만 표본이 적어 확신은 낮습니다");
                }
                t.push('.');
                parts.push(t);
                if star.cands.len() > 1 {
                    parts.push(format!(
                        "같은 라인 대안: {}.",
                        star.cands
                            .iter()
                            .skip(1)
                            .map(|c| format!("{} {:+.0}", c.ko, c.score * 100.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if let Some(cof) = &star.counter_of {
                    parts.push(format!("1순위 픽은 상대 {} 상대로 실측 카운터 우위가 있습니다.", cof));
                }
                if !lv.tip.is_empty() {
                    parts.push(format!("전략 메모: {}.", lv.tip));
                }
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
        t.push('.');
        parts.push(t);
        if view.rows.len() > 1 {
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
    if s.fearless_on {
        if s.unattributed {
            // mid-series recovery: the split, the set number AND the style are all
            // guesses here — claim none of them, show only the honest lock total.
            let total: HashSet<&String> = s.my.iter().chain(s.enemy.iter()).collect();
            parts.push(format!(
                "피어리스 — 사용 불가 {}챔 (게임 잠금 기준; 모드가 시리즈 도중 시작되어 세트/팀 구분은 생략).",
                total.len()
            ));
        } else {
            parts.push(format!(
                "피어리스 {}세트({}) — 사용 불가: 내 {}챔 · 상대 {}챔.",
                s.game,
                if s.hard { "하드" } else { "노멀" },
                s.my.len(),
                s.enemy.len()
            ));
        }
    }
    parts.push(format!(
        "데이터: 현재 패치 {}판{}",
        tables.matches,
        if tables.matches < 50 { " — 콜드스타트 구간, 이론 비중이 큽니다." } else { "." }
    ));

    // ── tier labels for the grid
    let tiers: HashMap<String, String> = CHAMPS
        .iter()
        .filter_map(|(id, _)| {
            let t = tables.tier_of(id, None);
            let m = tables.chg_mark(id);
            if t.is_empty() && m.is_empty() {
                None
            } else {
                Some((id.to_string(), format!("{}{}", t, m)))
            }
        })
        .collect();

    let out = Arc::new(Output { rec_ban, recs, swap, say: parts.join("\n"), tiers });
    if let Ok(mut g) = OUTPUT.lock() {
        *g = Some((key, out.clone()));
    }
    out
}
