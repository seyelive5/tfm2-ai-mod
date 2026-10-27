//! Save records → engine tables (stable API, game 0.6.5+).
//!
//! Source parity with v3.0: per-patch champion stats, the MU/OPP rows and the
//! athlete fallback history all come from the save's MatchReplay table — the same
//! table classic read as `db.match_replays`, practice sets included (LT probe B4b:
//! field-for-field identical). Classic took the per-patch stats from the game's own
//! `champion_patch_statistics`, which the stable API does not expose; the
//! replay-derived TSV below is exactly the input the harness's S0-3 port validated.
//!
//! Cost: one record read ≈131 µs (LT probe: 12,145 replays ≈ 1.6 s), so the table
//! is parsed INCREMENTALLY under a per-frame time budget, in any InGame screen.
//! Tables rebuild when the parsed set grows — but not during a banpick unless none
//! exist yet, so the recommendations don't shift under the player mid-draft.

use crate::json::JsonVal;
use crate::recommender::{self, engine::Tables, ChgInfo};
use crate::{diag, vkey};
use mod_api_stable::*;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// Engine tables built from the live stats.
pub(crate) static TABLES: Mutex<Option<Tables>> = Mutex::new(None);
/// Bumped on every TABLES rebuild — invalidates the recommendation cache.
pub(crate) static TABLES_GEN: AtomicUsize = AtomicUsize::new(0);

const POLL_MS: u128 = 2000;
/// per-frame replay-parse budget (µs): management screens / banpick or live
/// match playback / banpick before the first build (finish fast, the panel says
/// "loading").
const BUDGET_US: u128 = 4000;
const BUDGET_BUSY_US: u128 = 1000;
const BUDGET_COLD_US: u128 = 8000;

/// What the client is showing — sets the background budget and whether the
/// tables may rebuild (a rebuild costs tens of ms: never during a draft the
/// player is reading, nor during match playback where it would stutter).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Screen {
    Menu,
    Banpick,
    Playback,
}

struct Part {
    athlete_id: usize,
    position: u8, // 0=Top .. 4=Support, 9 = unknown
    champion: String,
    dealing: u64,
    tanking: u64,
    healing: u64,
    gold_lp: u64,
}

struct Replay {
    version: String,
    blue_team_id: usize,
    red_team_id: usize,
    blue_win: bool,
    blue_ban: Vec<String>,
    red_ban: Vec<String>,
    blue: Vec<Part>,
    red: Vec<Part>,
}

type Notes = BTreeMap<Vec<u32>, (String, HashMap<String, (u32, u32)>)>;

#[derive(Default)]
struct Db {
    known: HashSet<usize>,
    queue: VecDeque<usize>,
    /// id-ordered → deterministic aggregation (classic iterated a HashMap; the
    /// aggregates are order-free, BTreeMap just keeps rebuilds byte-identical).
    replays: BTreeMap<usize, Replay>,
    failed: usize,
    listed: usize,
    last_poll: Option<Instant>,
    /// replays.len() at the last table build
    built: Option<usize>,
    teams: HashMap<usize, String>,
    player_team: Option<usize>,
    athletes: Option<HashMap<String, Vec<usize>>>,
    athletes_at: Option<Instant>,
    /// newest played version at the last all-inbox note scan that found nothing
    notes_scanned_at: Option<Vec<u32>>,
    parse_us: u128,
    build_ms: u128,
    builds: usize,
    chg_line: String,
    first_keys: String,
}

static DB: Mutex<Option<Db>> = Mutex::new(None);

/// Leaving the InGame scene (save unload / switch): drop everything.
pub(crate) fn reset() {
    if let Ok(mut g) = DB.lock() {
        *g = None;
    }
    if let Ok(mut t) = TABLES.lock() {
        *t = None;
    }
    TABLES_GEN.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn tables_ready() -> bool {
    TABLES.lock().map(|t| t.is_some()).unwrap_or(false)
}

/// (records handled, records listed) — the panel's loading line.
pub(crate) fn progress() -> (usize, usize) {
    DB.lock()
        .ok()
        .and_then(|g| g.as_ref().map(|d| (d.replays.len() + d.failed, d.listed)))
        .unwrap_or((0, 0))
}

pub(crate) fn player_team() -> Option<usize> {
    DB.lock().ok().and_then(|g| g.as_ref().and_then(|d| d.player_team))
}

/// Team ids whose database name is exactly `name`.
pub(crate) fn teams_named(name: &str) -> Vec<usize> {
    DB.lock()
        .ok()
        .and_then(|g| {
            g.as_ref().map(|d| {
                let mut v: Vec<usize> =
                    d.teams.iter().filter(|(_, n)| n.as_str() == name).map(|(id, _)| *id).collect();
                v.sort();
                v
            })
        })
        .unwrap_or_default()
}

/// Once per frame in the InGame scene.
pub(crate) fn tick(ctx: &StableClient, screen: Screen) {
    let Ok(mut guard) = DB.lock() else { return };
    let db = guard.get_or_insert_with(Db::default);
    if db.last_poll.map_or(true, |t| t.elapsed().as_millis() >= POLL_MS) {
        poll(ctx, db);
        db.last_poll = Some(Instant::now());
    }
    let ready = tables_ready();
    let budget = match screen {
        Screen::Menu => BUDGET_US,
        Screen::Banpick if !ready => BUDGET_COLD_US,
        _ => BUDGET_BUSY_US,
    };
    let t0 = Instant::now();
    while let Some(id) = db.queue.pop_front() {
        let parsed =
            ctx.record_get_json(RecordKindV1::MatchReplay, id, "").as_deref().and_then(JsonVal::parse);
        if db.first_keys.is_empty() {
            if let Some(v) = parsed.as_ref() {
                db.first_keys = v.keys();
            }
        }
        match parsed.as_ref().and_then(parse_replay) {
            Some(r) => {
                db.replays.insert(id, r);
            }
            None => db.failed += 1,
        }
        if t0.elapsed().as_micros() >= budget {
            break;
        }
    }
    db.parse_us += t0.elapsed().as_micros();
    if db.queue.is_empty() && db.built != Some(db.replays.len()) && (screen == Screen::Menu || !ready) {
        build(ctx, db);
    }
    diag::set(
        "3 db",
        format!(
            "player_team={:?} name={:?} teams={}\nreplays listed={} parsed={} failed={} queued={} parse_ms_total={}\nreplay_fields={}\ntables builds={} on_replays={:?} last_build_ms={} gen={}\nchg {}",
            db.player_team,
            db.player_team.and_then(|id| db.teams.get(&id)),
            db.teams.len(),
            db.listed,
            db.replays.len(),
            db.failed,
            db.queue.len(),
            db.parse_us / 1000,
            db.first_keys,
            db.builds,
            db.built,
            db.build_ms,
            TABLES_GEN.load(Ordering::Relaxed),
            db.chg_line,
        ),
    );
}

fn poll(ctx: &StableClient, db: &mut Db) {
    let ids = ctx.record_ids(RecordKindV1::MatchReplay);
    // backstop for a save switch that never left InGame: a listing that lost ids
    // we already know cannot be the same save.
    if ids.len() < db.known.len() {
        *db = Db::default();
        if let Ok(mut t) = TABLES.lock() {
            *t = None;
        }
        TABLES_GEN.fetch_add(1, Ordering::Relaxed);
    }
    db.player_team = ctx.player_team_id();
    db.teams.clear(); // cheap typed reads; names can change (rebrands)
    for id in ctx.team_ids() {
        if let Some(n) = ctx.team_name(id) {
            db.teams.insert(id, n.trim().to_string());
        }
    }
    db.listed = ids.len();
    for id in ids {
        if db.known.insert(id) {
            db.queue.push_back(id);
        }
    }
}

fn pos_to_u8(s: &str) -> u8 {
    match s {
        "Top" | "top" => 0,
        "Jungle" | "jungle" => 1,
        "Mid" | "mid" => 2,
        "Bottom" | "bottom" | "Bot" | "bot" => 3,
        "Support" | "support" => 4,
        _ => 9,
    }
}

fn pos_to_name(idx: u8) -> &'static str {
    match idx {
        0 => "Top",
        1 => "Jungle",
        2 => "Mid",
        3 => "Bottom",
        _ => "Support",
    }
}

fn parse_replay(v: &JsonVal) -> Option<Replay> {
    let version = v.get("version")?.as_str()?.to_string();
    let blue_team_id = v.get("blue_team_id")?.as_usize()?;
    let red_team_id = v.get("red_team_id")?.as_usize()?;
    let blue_win = v.get("blue_team_win")?.as_bool()?;
    let bans = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_arr())
            .map(|a| a.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    };
    let side = |team_key: &str, perf_key: &str| -> Vec<Part> {
        let gold_lp: Vec<u64> = v
            .get(perf_key)
            .and_then(|p| p.get("gold_line_phase"))
            .and_then(|g| g.as_arr())
            .map(|a| a.iter().map(|x| x.as_u64().unwrap_or(0)).collect())
            .unwrap_or_default();
        let Some(arr) = v.get(team_key).and_then(|x| x.as_arr()) else {
            return Vec::new();
        };
        arr.iter()
            .enumerate()
            .filter_map(|(slot, p)| {
                let stat = |k: &str| {
                    p.get("statistics").and_then(|s| s.get(k)).and_then(|x| x.as_u64()).unwrap_or(0)
                };
                let position = p
                    .get("position")
                    .and_then(|x| x.as_str().map(pos_to_u8).or_else(|| x.as_u64().map(|n| n as u8)))
                    .filter(|l| *l < 5)
                    .unwrap_or((slot as u8).min(4));
                Some(Part {
                    athlete_id: p.get("athlete_id").and_then(|x| x.as_usize()).unwrap_or(usize::MAX),
                    position,
                    champion: p.get("champion")?.as_str()?.to_string(),
                    dealing: stat("dealing"),
                    tanking: stat("tanking"),
                    healing: stat("healing"),
                    gold_lp: gold_lp.get(slot).copied().unwrap_or(0),
                })
            })
            .collect()
    };
    Some(Replay {
        version,
        blue_team_id,
        red_team_id,
        blue_win,
        blue_ban: bans("blue_ban"),
        red_ban: bans("red_ban"),
        blue: side("blue_team", "blue_performance"),
        red: side("red_team", "red_performance"),
    })
}

/// → (champion_patch_statistics-format TSV, MU/OPP/OPPN rows, played versions).
/// TSV columns match classic `patch_tsv` (unread counters written as 0);
/// MU/OPP/OPPN match classic `replay_rows`.
fn aggregate(replays: &BTreeMap<usize, Replay>) -> (String, String, BTreeMap<Vec<u32>, String>) {
    #[derive(Default)]
    struct PosAgg {
        wins: u32,
        matches: u32,
        dealing: u64,
        tanking: u64,
        healing: u64,
        gold_lp: u64,
    }
    let mut ver_matches: BTreeMap<String, u32> = BTreeMap::new();
    let mut ver_bans: BTreeMap<(String, String), u32> = BTreeMap::new();
    let mut pos_stats: BTreeMap<(String, String, u8), PosAgg> = BTreeMap::new();
    let mut played: BTreeMap<Vec<u32>, String> = BTreeMap::new();
    let mut mu: BTreeMap<(String, String), (u32, u32)> = BTreeMap::new();
    let mut opp: BTreeMap<(usize, String), (u32, u32)> = BTreeMap::new();
    let mut teamn: BTreeMap<usize, u32> = BTreeMap::new();

    for rep in replays.values() {
        played.entry(vkey(&rep.version)).or_insert_with(|| rep.version.clone());
        *ver_matches.entry(rep.version.clone()).or_insert(0) += 1;
        for b in rep.blue_ban.iter().chain(rep.red_ban.iter()) {
            *ver_bans.entry((rep.version.clone(), b.clone())).or_insert(0) += 1;
        }
        let mut lanes: [[Option<&str>; 5]; 2] = [[None; 5]; 2];
        for (si, (side, won)) in [(&rep.blue, rep.blue_win), (&rep.red, !rep.blue_win)].into_iter().enumerate() {
            for p in side.iter().filter(|p| p.position < 5) {
                lanes[si][p.position as usize] = Some(p.champion.as_str());
                let e = pos_stats.entry((rep.version.clone(), p.champion.clone(), p.position)).or_default();
                e.matches += 1;
                if won {
                    e.wins += 1;
                }
                e.dealing += p.dealing;
                e.tanking += p.tanking;
                e.healing += p.healing;
                e.gold_lp += p.gold_lp;
            }
        }
        for l in 0..5 {
            if let (Some(b), Some(r)) = (lanes[0][l], lanes[1][l]) {
                let e = mu.entry((b.to_string(), r.to_string())).or_insert((0, 0));
                e.1 += 1;
                if rep.blue_win {
                    e.0 += 1;
                }
                let e2 = mu.entry((r.to_string(), b.to_string())).or_insert((0, 0));
                e2.1 += 1;
                if !rep.blue_win {
                    e2.0 += 1;
                }
            }
        }
        for (tid, side, won) in
            [(rep.blue_team_id, &rep.blue, rep.blue_win), (rep.red_team_id, &rep.red, !rep.blue_win)]
        {
            *teamn.entry(tid).or_insert(0) += 1;
            for p in side {
                let e = opp.entry((tid, p.champion.clone())).or_insert((0, 0));
                e.0 += 1;
                if won {
                    e.1 += 1;
                }
            }
        }
    }

    let mut pt = String::with_capacity(64 * 1024);
    pt.push_str("version\ttotal_match\tchampion\tposition\tbans\twins\tmatches\tdealing\ttanking\thealing\tkills\tdeaths\tcs\tgold\tdealing_line_phase\ttanking_line_phase\thealing_line_phase\tgold_line_phase\tcs_line_phase\n");
    for ((ver, champ, pos), st) in &pos_stats {
        pt.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t0\t0\t0\t0\t0\t0\t0\t{}\t0\n",
            ver,
            ver_matches.get(ver).copied().unwrap_or(0),
            champ,
            pos_to_name(*pos),
            ver_bans.get(&(ver.clone(), champ.clone())).copied().unwrap_or(0),
            st.wins,
            st.matches,
            st.dealing,
            st.tanking,
            st.healing,
            st.gold_lp,
        ));
    }
    let mut rr = String::with_capacity(64 * 1024);
    for ((a, b), (w, g)) in &mu {
        rr.push_str(&format!("MU\t{}\t{}\t{}\t{}\n", a, b, w, g));
    }
    for ((t, c), (g, w)) in &opp {
        rr.push_str(&format!("OPP\t{}\t{}\t{}\t{}\n", t, c, g, w));
    }
    for (t, g) in &teamn {
        rr.push_str(&format!("OPPN\t{}\t{}\n", t, g));
    }
    (pt, rr, played)
}

fn build(ctx: &StableClient, db: &mut Db) {
    let t0 = Instant::now();
    let (pt, rr, played) = aggregate(&db.replays);
    let (chg, src) = authored_chg(ctx, db, &played);
    let pool = ctx.champion_names();
    let tables = recommender::build_tables(&pt, &pool, &chg, &rr);
    if let Ok(mut t) = TABLES.lock() {
        *t = Some(tables);
    }
    TABLES_GEN.fetch_add(1, Ordering::Relaxed);
    crate::prior::on_patch_anchor(chg.anchor.as_deref());
    db.built = Some(db.replays.len());
    db.builds += 1;
    db.build_ms = t0.elapsed().as_millis();
    db.chg_line = format!(
        "src={} anchor={} pool={} changed={} [{}]",
        src,
        chg.anchor.as_deref().unwrap_or("-"),
        pool.len(),
        chg.dirs.len(),
        chg.dirs.iter().map(|(c, d)| format!("{}{}", c, d)).collect::<Vec<_>>().join(" ")
    );
    if diag::debug_dumps() {
        let dir = format!("{}/probe", diag::base());
        let _ = std::fs::create_dir_all(&dir);
        diag::atomic_write(&format!("{}/champion_patch_statistics.tsv", dir), &pt);
        diag::atomic_write(&format!("{}/replay_rows.tsv", dir), &rr);
    }
}

// ───────────────────── authored patch changes (PatchNote news) ─────────────────────

/// `news` → PatchNote champion changes per version (buffs, nerfs). Notes are
/// single-instance per version (v3.0 dump: 120 teams, 7 notes / 7 versions), so
/// the first non-empty copy of a version wins. Returns how many notes were seen.
fn notes_into(news: &JsonVal, by_ver: &mut Notes) -> usize {
    let mut seen = 0;
    for item in news.values() {
        let Some(pn) = item.get("ty").and_then(|t| t.get("PatchNote")) else {
            continue;
        };
        let Some(ver) = pn.get("version").and_then(|v| v.as_str()) else {
            continue;
        };
        seen += 1;
        let entry = by_ver.entry(vkey(ver)).or_insert_with(|| (ver.to_string(), HashMap::new()));
        if !entry.1.is_empty() {
            continue;
        }
        for (champ, changes) in pn.get("champion_patch_data").map(|c| c.entries()).unwrap_or_default() {
            let Some(changes) = changes.as_arr() else { continue };
            if changes.is_empty() {
                continue; // an empty change list would misclassify as '~' (spurious ◆ + stats reset)
            }
            let e = entry.1.entry(champ).or_insert((0, 0));
            for c in changes {
                if c.get("is_buff").and_then(|b| b.as_bool()).unwrap_or(false) {
                    e.0 += 1;
                } else {
                    e.1 += 1;
                }
            }
        }
    }
    seen
}

/// v3.0 `authored_chg` semantics: the latest note is the current patch; anchor =
/// the last PLAYED version before it; None = currency guard (a newer version is
/// already being played than the newest note).
fn chg_from_notes(by_ver: &Notes, played: &BTreeMap<Vec<u32>, String>) -> Option<ChgInfo> {
    let (lk, (_, latest)) = by_ver.iter().next_back()?;
    if let Some((smax, _)) = played.iter().next_back() {
        if smax > lk {
            return None;
        }
    }
    let anchor = played.iter().filter(|(k, _)| *k < lk).next_back().map(|(_, v)| v.clone());
    let mut dirs: Vec<(String, char)> = latest
        .iter()
        .map(|(champ, (b, n))| {
            let dir = if *b > 0 && *n == 0 {
                '+'
            } else if *n > 0 && *b == 0 {
                '-'
            } else {
                '~'
            };
            (champ.clone(), dir)
        })
        .collect();
    dirs.sort();
    let mut counts: Vec<(String, u32, u32)> = latest.iter().map(|(c, (b, n))| (c.clone(), *b, *n)).collect();
    counts.sort();
    Some(ChgInfo { anchor, dirs, counts })
}

/// The player's inbox first (LT export of this save: all 10 notes are there);
/// every inbox only when it has none — classic scanned all teams — and then at
/// most once per newly played version. No defs-diff fallback exists on stable
/// (no ability numbers), so "no note" = no change marks.
fn authored_chg(ctx: &StableClient, db: &mut Db, played: &BTreeMap<Vec<u32>, String>) -> (ChgInfo, &'static str) {
    let mut by_ver: Notes = BTreeMap::new();
    let mut src = "player-inbox";
    if let Some(pt) = db.player_team {
        if let Some(v) = ctx.record_get_json(RecordKindV1::Team, pt, "news").as_deref().and_then(JsonVal::parse) {
            notes_into(&v, &mut by_ver);
        }
    }
    let newest = played.keys().next_back().cloned();
    if by_ver.is_empty() && db.notes_scanned_at != newest {
        src = "all-inboxes";
        let ids: Vec<usize> = db.teams.keys().copied().collect();
        for tid in ids {
            if let Some(v) = ctx.record_get_json(RecordKindV1::Team, tid, "news").as_deref().and_then(JsonVal::parse) {
                notes_into(&v, &mut by_ver);
            }
        }
        if by_ver.is_empty() {
            db.notes_scanned_at = newest;
        }
    }
    if by_ver.is_empty() {
        return (ChgInfo::default(), "no-notes");
    }
    match chg_from_notes(&by_ver, played) {
        Some(ci) => (ci, src),
        None => (ChgInfo::default(), "stale-note"),
    }
}

// ───────────────────── athletes (starters → familiarity) ─────────────────────

/// Athlete ids whose name is `name` (map built lazily; rebuilt at most every 10 s
/// when a lookup misses — rookies get generated during a save).
pub(crate) fn athletes_named(ctx: &StableClient, name: &str) -> Vec<usize> {
    let Ok(mut g) = DB.lock() else { return Vec::new() };
    let Some(db) = g.as_mut() else { return Vec::new() };
    let stale = db.athletes.as_ref().map_or(true, |m| !m.contains_key(name))
        && db.athletes_at.map_or(true, |t| t.elapsed().as_secs() >= 10);
    if stale {
        let mut m: HashMap<String, Vec<usize>> = HashMap::new();
        for id in ctx.athlete_ids() {
            if let Some(n) = ctx.athlete_name(id) {
                m.entry(n.trim().to_string()).or_default().push(id);
            }
        }
        db.athletes = Some(m);
        db.athletes_at = Some(Instant::now());
    }
    db.athletes.as_ref().and_then(|m| m.get(name).cloned()).unwrap_or_default()
}

/// The athlete's contracted team (`contract.InContract.team_id`) — disambiguates
/// two athletes sharing a name.
pub(crate) fn athlete_team(ctx: &StableClient, aid: usize) -> Option<usize> {
    let v = ctx.record_get_json(RecordKindV1::Athlete, aid, "contract").as_deref().and_then(JsonVal::parse)?;
    v.get("InContract").and_then(|c| c.get("team_id")).and_then(|x| x.as_usize())
}

/// Career champion history (games, wins) from the athlete record's
/// `season_statistics` — v3.0's source (every season × position × champion).
/// Tolerant of the map/array encodings serde may use.
fn season_hist(v: &JsonVal) -> HashMap<String, (f64, f64)> {
    let mut m: HashMap<String, (f64, f64)> = HashMap::new();
    let seasons: Vec<&JsonVal> = if v.get("by_position").is_some() { vec![v] } else { v.values() };
    for ss in seasons {
        let Some(bp) = ss.get("by_position") else { continue };
        for st in bp.values() {
            let Some(cd) = st.get("champion_detail") else { continue };
            for (c, d) in cd.entries() {
                let g = d.get("matches").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let w = d.get("wins").and_then(|x| x.as_f64()).unwrap_or(0.0);
                if g > 0.0 {
                    let e = m.entry(c).or_insert((0.0, 0.0));
                    e.0 += g;
                    e.1 += w;
                }
            }
        }
    }
    m
}

/// Phase-2 athlete terms for this match's starters (lane order), v3.0 formulas:
///   af((champ, lane)) = min(0.05, 0.15·centered-wr)  — F15, famg removed
///   pocket(champ)     = 0.015·ln(1+min(g,20)), max over ENEMY starters
/// History = the athlete's `season_statistics`; when that can't be read the
/// save's replays stand in (src=replays in the debug string).
pub(crate) fn athlete_tables(
    ctx: &StableClient,
    my: &[Option<usize>; 5],
    enemy: &[Option<usize>; 5],
) -> (HashMap<(String, u8), f32>, HashMap<String, f32>, String) {
    let ids: Vec<usize> = my.iter().chain(enemy.iter()).flatten().copied().collect();
    let mut hist: HashMap<usize, (HashMap<String, (f64, f64)>, &'static str)> = HashMap::new();
    for &aid in &ids {
        if let Some(v) =
            ctx.record_get_json(RecordKindV1::Athlete, aid, "season_statistics").as_deref().and_then(JsonVal::parse)
        {
            let h = season_hist(&v);
            if !h.is_empty() {
                hist.insert(aid, (h, "season"));
            }
        }
    }
    let missing: HashSet<usize> = ids.iter().copied().filter(|a| !hist.contains_key(a)).collect();
    if !missing.is_empty() {
        if let Ok(g) = DB.lock() {
            if let Some(db) = g.as_ref() {
                for rep in db.replays.values() {
                    for (side, won) in [(&rep.blue, rep.blue_win), (&rep.red, !rep.blue_win)] {
                        for p in side.iter().filter(|p| missing.contains(&p.athlete_id)) {
                            let h = &mut hist.entry(p.athlete_id).or_insert_with(|| (HashMap::new(), "replays")).0;
                            let e = h.entry(p.champion.clone()).or_insert((0.0, 0.0));
                            e.0 += 1.0;
                            if won {
                                e.1 += 1.0;
                            }
                        }
                    }
                }
            }
        }
    }
    let mut dbg = String::new();
    let mut af: HashMap<(String, u8), f32> = HashMap::new();
    for (lane, aid) in my.iter().enumerate() {
        let Some(aid) = aid else {
            dbg.push_str(&format!("{}:? ", lane));
            continue;
        };
        let Some((h, src)) = hist.get(aid) else {
            dbg.push_str(&format!("{}:{}:none ", lane, aid));
            continue;
        };
        let (tg, tw) = h.values().fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
        let wr_a = (tw + 4.0) / (tg + 8.0); // athlete overall winrate, shrunk
        for (c, (g, w)) in h {
            let famc = 0.15 * ((w + 4.0 * wr_a) / (g + 4.0) - wr_a);
            af.insert((c.clone(), lane as u8), famc.min(0.05) as f32);
        }
        dbg.push_str(&format!("{}:{}:{}({}g) ", lane, aid, src, tg));
    }
    let mut pocket: HashMap<String, f32> = HashMap::new();
    for aid in enemy.iter().flatten() {
        if let Some((h, _)) = hist.get(aid) {
            for (c, (g, _)) in h {
                let v = (0.015 * (1.0 + g.min(20.0)).ln()) as f32;
                let e = pocket.entry(c.clone()).or_insert(0.0);
                if v > *e {
                    *e = v;
                }
            }
        }
    }
    (af, pocket, dbg.trim_end().to_string())
}
