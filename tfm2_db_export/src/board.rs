//! Banpick screen → `recommender::Board`, side-resolved. Read-only: every value
//! comes from the game's own UI nodes (0.6.5 paths — ROADMAP §2) plus the
//! player's team id. Picks carry side + order on the grid tiles; bans carry
//! neither, so a ban is attributed to whoever's turn it was on the frame before
//! it appeared (logged per ban to board_log.txt, observed vs guessed).

use crate::json::JsonVal;
use crate::recommender::Board;
use crate::{db, diag};
use mod_api_stable::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Blue,
    Red,
}

impl Side {
    fn key(self) -> &'static str {
        match self {
            Side::Blue => "blue",
            Side::Red => "red",
        }
    }
    fn idx(self) -> usize {
        match self {
            Side::Blue => 0,
            Side::Red => 1,
        }
    }
    fn other(self) -> Side {
        match self {
            Side::Blue => Side::Red,
            Side::Red => Side::Blue,
        }
    }
}

const SIDES: [Side; 2] = [Side::Blue, Side::Red];

/// One frame's reading of the banpick screen.
pub(crate) struct Read {
    /// None = the draft can't be attributed to the player (see `status`).
    pub board: Option<Board>,
    pub status: String,
    pub my_starters: [Option<usize>; 5],
    pub enemy_starters: [Option<usize>; 5],
    /// champion pool as shown by the game: union of tile ids this session
    pub grid_pool: Vec<String>,
    /// tile ids on screen this frame
    pub tiles: Vec<String>,
    /// tiles nobody can take any more for me: banned, picked, or fearless-X
    pub taken: HashSet<String>,
    /// my side (None = unresolved)
    pub my_blue: Option<bool>,
    /// champ id → the game's own display name (its tile label, i18n-resolved);
    /// absent = unresolved → the engine's table name stands in
    pub names: Arc<HashMap<String, String>>,
}

/// The player's running match record (MatchNormal, else MatchPractice): team1
/// acts first (T2) and `replays` counts the sets already played.
#[derive(Default)]
struct MatchProbe {
    at: Option<Instant>,
    found: Option<(RecordKindV1, usize)>,
    /// pending ids, popped from the END (= newest MatchNormal first)
    scan: Vec<(RecordKindV1, usize)>,
    scanned: bool,
    /// full passes that found nothing (backs the retry off to 30 s)
    misses: u32,
    team1: Option<usize>,
    team2: Option<usize>,
    sets_done: Option<usize>,
}

#[derive(Default)]
struct Track {
    draft: String,
    prev_turn: Option<Side>,
    first_turn: Option<Side>,
    /// attributed bans per side [blue, red], in attribution order
    bans: [Vec<String>; 2],
    guessed: usize,
    grid_pool: BTreeSet<String>,
    starters_key: String,
    starters_at: Option<Instant>,
    starters: ([Option<usize>; 5], [Option<usize>; 5]),
    starters_dbg: String,
    names: Arc<HashMap<String, String>>,
    /// how the first tile name resolved (diag: settles the i18n key format)
    name_dbg: String,
    last_log: String,
    probe: MatchProbe,
}

static TRACK: Mutex<Option<Track>> = Mutex::new(None);

/// Leaving the InGame scene: forget everything (match probe included).
pub(crate) fn reset() {
    if let Ok(mut t) = TRACK.lock() {
        *t = None;
    }
}

/// Leaving a banpick screen (the set is played next): per-screen state goes,
/// the match probe stays — the next set's first frame must carry the same
/// match id, or the recommender's series memory (fearless my_used) resets.
pub(crate) fn leave() {
    if let Ok(mut g) = TRACK.lock() {
        if let Some(t) = g.as_mut() {
            let probe = std::mem::take(&mut t.probe);
            *t = Track { probe, ..Track::default() };
        }
    }
}

/// The banpick screen's own nodes (banpick_065.ui). The client scene code is
/// only logged (diag "2 scene"): it has never been observed in-game, and a
/// wrong guess there would silence the whole mod.
pub(crate) fn is_banpick(ctx: &StableClient) -> bool {
    ctx.ui_exists("main.champions.contents") && ctx.ui_exists("main.blue_picks")
}

fn vis(ctx: &StableClient, path: &str) -> bool {
    ctx.ui_visible(path) == Some(true)
}

fn text(ctx: &StableClient, path: &str) -> String {
    ctx.ui_text(path).map(|t| t.trim().to_string()).unwrap_or_default()
}

/// Clean database names first (exact), then the decorated bottom-bar names
/// (contains). Never defaults: an unresolved side means no recommendations.
fn resolve_side(my: &str, clean: &[String; 2], deco: &[String; 2]) -> (Option<Side>, &'static str) {
    if my.is_empty() {
        return (None, "내 팀 이름을 읽지 못함");
    }
    let eq = |s: &String| !s.is_empty() && s == my;
    match (eq(&clean[0]), eq(&clean[1])) {
        (true, false) => return (Some(Side::Blue), "clean-name"),
        (false, true) => return (Some(Side::Red), "clean-name"),
        (true, true) => return (None, "양쪽 팀 이름이 같음"),
        _ => {}
    }
    let has = |s: &String| !s.is_empty() && s.contains(my);
    match (has(&deco[0]), has(&deco[1])) {
        (true, false) => (Some(Side::Blue), "decorated-name"),
        (false, true) => (Some(Side::Red), "decorated-name"),
        (true, true) => (None, "양쪽 팀 이름에 내 팀 이름이 들어 있음"),
        _ => (None, "내 팀이 이 밴픽에 없음 (관전?)"),
    }
}

enum Lock {
    Own,
    Both,
    Color(Side),
    Unknown,
}

/// `ui_text` returns the raw i18n key ("#asset/base/text/ui?banpick.fearless_tooltip_red");
/// localized phrasings (v3.0 list) stay as a fallback in case a version resolves them.
fn classify_lock(tip: &str) -> Lock {
    const OWN_T: [&str; 5] = ["우리 팀", "Your team", "自チーム", "你的队伍", "你的隊伍"];
    const BOTH_T: [&str; 5] = ["양팀", "Neither team", "両チーム", "双方", "雙方"];
    const BLUE_T: [&str; 5] = ["블루팀", "Blue team", "ブルーチーム", "蓝方", "藍方"];
    const RED_T: [&str; 5] = ["레드팀", "Red team", "レッドチーム", "红方", "紅方"];
    let any = |l: &[&str]| l.iter().any(|p| tip.contains(p));
    if tip.contains("fearless_tooltip_both") || any(&BOTH_T) {
        Lock::Both
    } else if tip.contains("fearless_tooltip_own") || any(&OWN_T) {
        Lock::Own
    } else if tip.contains("fearless_tooltip_blue") || any(&BLUE_T) {
        Lock::Color(Side::Blue)
    } else if tip.contains("fearless_tooltip_red") || any(&RED_T) {
        Lock::Color(Side::Red)
    } else {
        Lock::Unknown
    }
}

fn is_running(json: Option<String>) -> bool {
    match json.as_deref().and_then(JsonVal::parse) {
        Some(JsonVal::Str(s)) => s == "Running",
        Some(v) => v.get("Running").is_some(),
        None => false,
    }
}

/// Finds/validates the player's running match. A found match is re-validated
/// every 2 s; the search walks ids newest-first under a 1.5 ms/frame budget.
fn probe_match(ctx: &StableClient, p: &mut MatchProbe, my: Option<usize>) {
    let Some(my) = my else { return };
    if let Some((kind, id)) = p.found {
        if p.at.map_or(false, |t| t.elapsed().as_millis() < 2000) {
            return;
        }
        p.at = Some(Instant::now());
        if is_running(ctx.record_get_json(kind, id, "running_state")) {
            p.sets_done = ctx
                .record_get_json(kind, id, "replays")
                .as_deref()
                .and_then(JsonVal::parse)
                .and_then(|v| v.as_arr().map(|a| a.len()));
            return;
        }
        *p = MatchProbe::default();
    }
    if !p.scanned {
        let wait_ms = if p.misses >= 2 { 30_000 } else { 2000 };
        if p.at.map_or(false, |t| t.elapsed().as_millis() < wait_ms) {
            return; // a full pass found nothing: retry later
        }
        for kind in [RecordKindV1::MatchPractice, RecordKindV1::MatchNormal] {
            let mut ids = ctx.record_ids(kind);
            ids.sort_unstable();
            p.scan.extend(ids.into_iter().map(|id| (kind, id)));
        }
        p.scanned = true;
    }
    let t0 = Instant::now();
    while let Some((kind, id)) = p.scan.pop() {
        if is_running(ctx.record_get_json(kind, id, "running_state")) {
            let t1 = ctx.record_get_i64(kind, id, "team1").map(|v| v as usize);
            let t2 = ctx.record_get_i64(kind, id, "team2").map(|v| v as usize);
            if t1 == Some(my) || t2 == Some(my) {
                p.found = Some((kind, id));
                p.team1 = t1;
                p.team2 = t2;
                p.scan.clear();
                p.at = None; // validate (and read the set count) next frame
                return;
            }
        }
        if t0.elapsed().as_micros() >= 1500 {
            return;
        }
    }
    p.scanned = false;
    p.misses += 1;
    p.at = Some(Instant::now());
}

struct SlotVis {
    id: String,
    shown: bool,
    icon: bool,
    ban_icon: bool,
    in_turn: bool,
    later: bool,
}

/// Whose turn it is: header arrows, else the in-turn ban/pick slot outline.
fn read_turn(ctx: &StableClient, ban_turn: [bool; 2], pick_slots: &[Vec<String>; 2]) -> (Option<Side>, String) {
    let arrow = match (vis(ctx, "main.header.desc.blue_arrow"), vis(ctx, "main.header.desc.red_arrow")) {
        (true, false) => Some(Side::Blue),
        (false, true) => Some(Side::Red),
        _ => None,
    };
    let mut slot: Option<Side> = None;
    let mut slot_hits = 0;
    for s in SIDES {
        let picks =
            pick_slots[s.idx()].iter().any(|p| vis(ctx, &format!("main.{}_picks.{}.in_turn", s.key(), p)));
        if ban_turn[s.idx()] || picks {
            slot = Some(s);
            slot_hits += 1;
        }
    }
    if slot_hits > 1 {
        slot = None;
    }
    let dbg = format!("arrow={:?} slot={:?}", arrow, slot);
    (arrow.or(slot), dbg)
}

/// A game label's text is its i18n KEY ("#asset/base/text/…?…", LT B1) unless the
/// runner wrote a literal; either way → the display string, or None.
fn display_name(ctx: &StableClient, raw: &str) -> (Option<String>, &'static str) {
    if raw.is_empty() {
        return (None, "empty");
    }
    let Some(key) = raw.strip_prefix('#') else {
        return (Some(raw.to_string()), "literal");
    };
    let ok = |t: &String| !t.is_empty() && !t.starts_with('#') && t != key;
    if let Some(t) = ctx.i18n(key).filter(ok) {
        return (Some(t), "i18n(key)");
    }
    if let Some(t) = ctx.i18n(raw).filter(ok) {
        return (Some(t), "i18n(#key)");
    }
    (None, "unresolved")
}

fn starter_name(ctx: &StableClient, side: Side, slot: &str) -> String {
    let base = format!("main.{}_picks.{}", side.key(), slot);
    for state in ["done", "in_turn", "wait"] {
        if vis(ctx, &format!("{}.{}", base, state)) {
            let n = text(ctx, &format!("{}.{}.name", base, state));
            if !n.is_empty() {
                return n;
            }
        }
    }
    for state in ["done", "in_turn", "wait"] {
        let n = text(ctx, &format!("{}.{}.name", base, state));
        if !n.is_empty() {
            return n;
        }
    }
    String::new()
}

/// Athlete id for a starter name; duplicate names are settled by contract team.
fn athlete_for(ctx: &StableClient, name: &str, team: Option<usize>) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let ids = db::athletes_named(ctx, name);
    match ids.len() {
        0 => None,
        1 => Some(ids[0]),
        _ => {
            let team = team?;
            let hits: Vec<usize> = ids.into_iter().filter(|a| db::athlete_team(ctx, *a) == Some(team)).collect();
            (hits.len() == 1).then(|| hits[0])
        }
    }
}

pub(crate) fn read(ctx: &StableClient) -> Read {
    let mut guard = match TRACK.lock() {
        Ok(g) => g,
        Err(_) => {
            return Read {
                board: None,
                status: "내부 오류 (lock)".into(),
                my_starters: [None; 5],
                enemy_starters: [None; 5],
                grid_pool: Vec::new(),
                tiles: Vec::new(),
                taken: HashSet::new(),
                my_blue: None,
                names: Arc::default(),
            }
        }
    };
    let tr = guard.get_or_insert_with(Track::default);

    // ── who am I
    let clean = [
        text(ctx, "main.blue_picks.pick_slot_0.popup.header.team_name"),
        text(ctx, "main.red_picks.pick_slot_0.popup.header.team_name"),
    ];
    let deco = [text(ctx, "main.bottom.blue_side.name"), text(ctx, "main.bottom.red_side.name")];
    let my_id = ctx.player_team_id().or_else(db::player_team);
    let my_name = my_id.and_then(|id| ctx.team_name(id)).map(|s| s.trim().to_string()).unwrap_or_default();
    let (side, how) = resolve_side(&my_name, &clean, &deco);
    probe_match(ctx, &mut tr.probe, my_id);

    // ── slots (ids enumerated, not assumed). A slot under `later_phase` belongs to
    // a later ban phase (ban_slot.ui): the current phase's allowance — and the
    // recommender's ban_count, whose `bans < 2·ban_count` gates ban advice — counts
    // only the visible slots not under it.
    let ban_vis: [Vec<SlotVis>; 2] = SIDES.map(|s| {
        ctx.ui_child_names(&format!("main.bottom.{}_side.bans", s.key()))
            .into_iter()
            .map(|id| {
                let p = format!("main.bottom.{}_side.bans.{}", s.key(), id);
                SlotVis {
                    shown: vis(ctx, &p),
                    icon: vis(ctx, &format!("{}.icon", p)),
                    ban_icon: vis(ctx, &format!("{}.ban_icon", p)),
                    in_turn: vis(ctx, &format!("{}.in_turn", p)),
                    later: vis(ctx, &format!("{}.later_phase", p)),
                    id,
                }
            })
            .collect()
    });
    let pick_slots: [Vec<String>; 2] = SIDES.map(|s| ctx.ui_child_names(&format!("main.{}_picks", s.key())));
    let caps: [usize; 2] = [0, 1].map(|i| ban_vis[i].iter().filter(|b| b.shown && !b.later).count());
    let ban_count = caps[0].max(caps[1]);
    let ban_turn: [bool; 2] = [0, 1].map(|i| ban_vis[i].iter().any(|b| b.shown && b.in_turn));
    let (turn, turn_dbg) = read_turn(ctx, ban_turn, &pick_slots);
    // candidate "filled" flags per side (icon / ban_icon visible) — logged with each
    // ban until the in-game log shows which one tracks the ban count
    let slot_fill = |i: usize| {
        (ban_vis[i].iter().filter(|b| b.shown && b.icon).count(), ban_vis[i].iter().filter(|b| b.shown && b.ban_icon).count())
    };
    let fills = [slot_fill(0), slot_fill(1)];

    // ── the grid: bans, side+order picks, fearless locks
    let tiles = ctx.ui_child_names("main.champions.contents");
    let mut grid_bans: Vec<String> = Vec::new();
    let mut picks: [Vec<(u8, String)>; 2] = [Vec::new(), Vec::new()];
    let mut taken: HashSet<String> = HashSet::new();
    let mut locks: Vec<(String, Lock, bool)> = Vec::new(); // (champ, lock, x mark)
    for id in &tiles {
        tr.grid_pool.insert(id.clone());
        let base = format!("main.champions.contents.{}", id);
        if !tr.names.contains_key(id) {
            let raw = text(ctx, &format!("{}.name", base));
            let (name, how) = display_name(ctx, &raw);
            if tr.name_dbg.is_empty() {
                tr.name_dbg = format!("{}: raw={:?} -> {:?} via {}", id, raw, name, how);
            }
            if let Some(n) = name {
                Arc::make_mut(&mut tr.names).insert(id.clone(), n);
            }
        }
        if vis(ctx, &format!("{}.ban", base)) {
            grid_bans.push(id.clone());
            taken.insert(id.clone());
        }
        for s in SIDES {
            if vis(ctx, &format!("{}.{}", base, s.key())) {
                let ord = text(ctx, &format!("{}.{}.text", base, s.key())).parse::<u8>().unwrap_or(99);
                picks[s.idx()].push((ord, id.clone()));
                taken.insert(id.clone());
            }
        }
        let icon = vis(ctx, &format!("{}.fearless_icon", base));
        let x = vis(ctx, &format!("{}.fearless_x", base));
        if x {
            taken.insert(id.clone());
        }
        if icon || x {
            let tip = text(ctx, &format!("{}.fearless_tooltip.text", base));
            locks.push((id.clone(), classify_lock(&tip), x));
        }
    }
    for p in picks.iter_mut() {
        p.sort_by_key(|(o, _)| *o);
    }
    let pick_ids: [Vec<String>; 2] = [
        picks[0].iter().map(|(_, c)| c.clone()).collect(),
        picks[1].iter().map(|(_, c)| c.clone()).collect(),
    ];

    // ── ban attribution (per set). The draft key is the team names only — the
    // match probe resolves ~1 s in, and keying on it would wipe bans already
    // attributed; set boundaries show up as the board emptying.
    let set_no = tr.probe.sets_done.map(|n| n + 1);
    let match_id = tr.probe.found.map(|(_, id)| id);
    let draft = format!("{}|{}", clean[0], clean[1]);
    // a frame without tiles (screen rebuilding) says nothing about bans: skip it,
    // or `retain` below would drop every attributed ban and re-guess them later
    let grid_ok = !tiles.is_empty();
    let empty = grid_ok && grid_bans.is_empty() && pick_ids[0].is_empty() && pick_ids[1].is_empty();
    if tr.draft != draft || (empty && tr.bans.iter().any(|b| !b.is_empty())) {
        tr.draft = draft;
        tr.bans = [Vec::new(), Vec::new()];
        tr.guessed = 0;
        tr.first_turn = None;
        tr.prev_turn = None;
    }
    if empty && tr.first_turn.is_none() {
        tr.first_turn = turn;
    }
    if grid_ok {
        for b in tr.bans.iter_mut() {
            b.retain(|c| grid_bans.contains(c));
        }
        let new_bans: Vec<String> =
            grid_bans.iter().filter(|c| !tr.bans.iter().any(|b| b.contains(c))).cloned().collect();
        // one new ban with a known previous turn = observed; anything else (several
        // at once — joined mid-draft — or no turn seen yet) alternates from there
        let single = new_bans.len() == 1 && tr.prev_turn.is_some();
        let done = tr.bans[0].len() + tr.bans[1].len();
        let mut s = tr.prev_turn.or(turn).unwrap_or_else(|| {
            let first = tr.first_turn.unwrap_or(Side::Blue);
            if done % 2 == 0 {
                first
            } else {
                first.other()
            }
        });
        for c in new_bans {
            let mut how = if single { "observed" } else { "guessed" };
            if caps[s.idx()] > 0 && tr.bans[s.idx()].len() >= caps[s.idx()] {
                s = s.other(); // that side's allowance is used up
                how = "guessed(cap)";
            }
            diag::log_line(&format!(
                "BAN {} -> {} ({}) prev_turn={:?} turn={:?} caps={:?} slot_fill(icon,ban_icon) b={:?} r={:?}",
                c, s.key(), how, tr.prev_turn, turn, caps, fills[0], fills[1]
            ));
            tr.bans[s.idx()].push(c);
            if how != "observed" {
                tr.guessed += 1;
            }
            s = s.other();
        }
    }
    if turn.is_some() {
        tr.prev_turn = turn;
    }

    // ── starters (lane order = pick slot order; ASSUMPTION carried from v3.0's
    // team*_starting check — verified there via the starting_my debug line)
    let names: [Vec<String>; 2] = SIDES.map(|s| {
        let mut ids = pick_slots[s.idx()].clone();
        ids.sort();
        ids.iter().take(5).map(|slot| starter_name(ctx, s, slot)).collect()
    });
    let enemy_id = match (side, tr.probe.team1, tr.probe.team2, my_id) {
        (_, Some(t1), Some(t2), Some(me)) if t1 == me => Some(t2),
        (_, Some(t1), Some(t2), Some(me)) if t2 == me => Some(t1),
        (Some(sd), ..) => {
            let ids = db::teams_named(&clean[sd.other().idx()]);
            (ids.len() == 1).then(|| ids[0])
        }
        _ => None,
    };
    if let Some(sd) = side {
        let key = format!("{:?}|{:?}|{:?}", names, sd, enemy_id);
        let unresolved = tr.starters.0.iter().chain(tr.starters.1.iter()).any(|a| a.is_none());
        let retry = unresolved && tr.starters_at.map_or(true, |t| t.elapsed().as_secs() >= 10);
        if tr.starters_key != key || retry {
            tr.starters_at = Some(Instant::now());
            let mine: Vec<Option<usize>> =
                names[sd.idx()].iter().map(|n| athlete_for(ctx, n, my_id)).collect();
            let theirs: Vec<Option<usize>> =
                names[sd.other().idx()].iter().map(|n| athlete_for(ctx, n, enemy_id)).collect();
            let mut a = [None; 5];
            let mut b = [None; 5];
            for i in 0..5 {
                a[i] = mine.get(i).copied().flatten();
                b[i] = theirs.get(i).copied().flatten();
            }
            tr.starters = (a, b);
            tr.starters_dbg = format!("my={:?} -> {:?}\nenemy={:?} -> {:?}", names[sd.idx()], a, names[sd.other().idx()], b);
            tr.starters_key = key;
        }
    }

    // ── fearless lists, side-resolved (v3.0 runtime rule 2026-06-12: _own = mine,
    // the color key = the other side's usage — a champ both teams used renders as
    // the color key only, which the recommender's own pick tracking re-adds)
    let (mut fl_my, mut fl_enemy) = (Vec::new(), Vec::new());
    let mut attributed = false;
    let mut own_color = 0; // color key naming MY side — not expected, counted for diag
    if let Some(sd) = side {
        for (c, lock, x) in &locks {
            match lock {
                Lock::Own => {
                    fl_my.push(c.clone());
                    attributed = true;
                }
                Lock::Color(s) if *s == sd.other() => {
                    fl_enemy.push(c.clone());
                    attributed = true;
                }
                Lock::Color(_) => {
                    fl_my.push(c.clone());
                    attributed = true;
                    own_color += 1;
                }
                Lock::Both => {
                    fl_my.push(c.clone());
                    fl_enemy.push(c.clone());
                    attributed = true;
                }
                Lock::Unknown => {
                    fl_my.push(c.clone());
                    fl_enemy.push(c.clone());
                }
            }
            // belt: an X plausibly means "YOU can't pick this" — never recommend past it.
            if *x && !fl_my.contains(c) {
                fl_my.push(c.clone());
            }
        }
    }

    // ── who acts first this set: observed first turn > match record team1 (T2)
    let i_am_team1 = match (side, tr.first_turn) {
        (Some(sd), Some(ft)) => Some(sd == ft),
        _ => match (tr.probe.team1, my_id) {
            (Some(t1), Some(me)) => Some(t1 == me),
            _ => None,
        },
    };

    let board = side.map(|sd| {
        let (me, them) = (sd.idx(), sd.other().idx());
        let ids_key = match (my_id, enemy_id) {
            (Some(a), Some(b)) => format!("{}|{}", a.min(b), a.max(b)),
            _ => format!("{}|{}", clean[0], clean[1]),
        };
        Board {
            my_ban: tr.bans[me].clone(),
            enemy_ban: tr.bans[them].clone(),
            my_pick: pick_ids[me].clone(),
            enemy_pick: pick_ids[them].clone(),
            ban_count,
            // match id → a rematch vs the same pair is a NEW series (v3.0); the set
            // number must NOT be in here — the recommender's fearless memory spans sets.
            teams: format!("{:?}|{}", match_id, ids_key),
            my_locked: fl_my.clone(),
            enemy_locked: fl_enemy.clone(),
            attributed,
            enemy_team: enemy_id.unwrap_or(0) as u32,
            i_am_team1,
            enemy_intent: Vec::new(),
        }
    });

    let status = match side {
        Some(_) => String::new(),
        None => format!("추천 중지: {}", how),
    };

    // ── diagnostics + board log
    let probe = &tr.probe;
    diag::set(
        "4 side",
        format!(
            "my_team={:?} name={:?}\nclean=[{:?}, {:?}] deco=[{:?}, {:?}]\nside={:?} via={}\nenemy_team={:?}\nmatch={:?} team1={:?} team2={:?} sets_done={:?}\nfirst_turn={:?} i_am_team1={:?}\n{}",
            my_id, my_name, clean[0], clean[1], deco[0], deco[1], side, how, enemy_id, probe.found, probe.team1,
            probe.team2, probe.sets_done, tr.first_turn, i_am_team1, tr.starters_dbg
        ),
    );
    let ban_slot_dbg: Vec<String> = SIDES
        .iter()
        .map(|s| {
            let parts: Vec<String> = ban_vis[s.idx()]
                .iter()
                .map(|b| {
                    format!(
                        "{}[v{} icon{} ban_icon{} in_turn{} later{}]",
                        b.id, b.shown as u8, b.icon as u8, b.ban_icon as u8, b.in_turn as u8, b.later as u8
                    )
                })
                .collect();
            format!("{} bans: {}", s.key(), parts.join(" "))
        })
        .collect();
    diag::set(
        "5 board",
        format!(
            "header step={:?} desc={:?} swap_phase={} need_win={:?}\nturn={:?} ({})\nban_slots cap={:?} ban_count={}\n{}\npick_slots blue={:?} red={:?}\ntiles={} grid_pool={} names={} ({})\nbans blue={:?} red={:?} guessed={}\npicks blue={:?} red={:?}\nlocks my={:?} enemy={:?} attributed={} own_color={}\nlock_tips={}",
            text(ctx, "main.header.step"),
            text(ctx, "main.header.desc"),
            vis(ctx, "main.header.swap_phase"),
            text(ctx, "main.bottom.need_win"),
            turn,
            turn_dbg,
            caps,
            ban_count,
            ban_slot_dbg.join("\n"),
            pick_slots[0],
            pick_slots[1],
            tiles.len(),
            tr.grid_pool.len(),
            tr.names.len(),
            tr.name_dbg,
            tr.bans[0],
            tr.bans[1],
            tr.guessed,
            pick_ids[0],
            pick_ids[1],
            fl_my,
            fl_enemy,
            attributed,
            own_color,
            locks
                .iter()
                .take(6)
                .map(|(c, l, x)| format!(
                    "{}:{}{}",
                    c,
                    match l {
                        Lock::Own => "own",
                        Lock::Both => "both",
                        Lock::Color(Side::Blue) => "blue",
                        Lock::Color(Side::Red) => "red",
                        Lock::Unknown => "?",
                    },
                    if *x { "+x" } else { "" }
                ))
                .collect::<Vec<_>>()
                .join(" "),
        ),
    );
    let line = format!(
        "side={:?} set={:?} ban_count={} turn={:?} bans=b[{}] r[{}] picks=b[{}] r[{}] locks=my[{}] en[{}] i_am_team1={:?}",
        side,
        set_no,
        ban_count,
        turn,
        tr.bans[0].join(","),
        tr.bans[1].join(","),
        pick_ids[0].join(","),
        pick_ids[1].join(","),
        fl_my.join(","),
        fl_enemy.join(","),
        i_am_team1
    );
    if line != tr.last_log {
        diag::log_line(&line);
        tr.last_log = line;
    }

    Read {
        board,
        status,
        my_starters: tr.starters.0,
        enemy_starters: tr.starters.1,
        grid_pool: tr.grid_pool.iter().cloned().collect(),
        tiles,
        taken,
        my_blue: side.map(|s| s == Side::Blue),
        names: tr.names.clone(),
    }
}
