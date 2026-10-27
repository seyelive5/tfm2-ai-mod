//! Data-discovery dumps behind probe/DEBUG_DUMPS (review experiment E2,
//! docs/research/2026-10-28-advanced-directions.md): record counts, which match
//! category owns the replays, one whole record of each kind a later feature
//! would read (athletes, team, scouting knowledge, matches, a replay), and the
//! UI tree once per banpick phase and per other screen — the field paths for
//! athlete chips and the post-set review, and a baseline to diff a game
//! update's layout against. Nothing here feeds the recommender; each dump is
//! written once per InGame session.

use crate::json::JsonVal;
use crate::{board, diag};
use mod_api_stable::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

/// a record can be MBs (replays): the probe wants its shape
const RECORD_CAP: usize = 64 * 1024;
const TREE_DEPTH: usize = 6;
const TREE_LINES: usize = 2000;
/// distinct trees per session (banpick phases + other screens)
const MAX_TREES: usize = 24;

#[derive(Default)]
struct Probe {
    records_done: bool,
    trees: HashSet<String>,
    written: Vec<String>,
    frame: u64,
}

static PROBE: Mutex<Option<Probe>> = Mutex::new(None);

/// Leaving the InGame scene: the next session dumps afresh.
pub(crate) fn reset() {
    if let Ok(mut p) = PROBE.lock() {
        *p = None;
    }
}

/// Banpick frames (pre_update, after the board read).
pub(crate) fn banpick(ctx: &StableClient, read: &board::Read, rec_ban: Option<bool>) {
    if !diag::debug_dumps() {
        return;
    }
    let Ok(mut g) = PROBE.lock() else { return };
    let p = g.get_or_insert_with(Probe::default);
    if !p.records_done {
        p.records_done = true;
        // one-off: walks every match record once (≈131 µs per read)
        let body = records(ctx, read);
        write(p, "records.txt", &body);
    }
    let key = bp_key(
        ctx.ui_visible("main.swap") == Some(true),
        rec_ban,
        ctx.ui_visible("main.coach_delegation_indicator") == Some(true),
    );
    tree_once(ctx, p, &format!("bp_{}", key));
}

/// Other InGame frames: one tree per screen the player passes through.
pub(crate) fn screen(ctx: &StableClient) {
    if !diag::debug_dumps() {
        return;
    }
    let Ok(mut g) = PROBE.lock() else { return };
    let p = g.get_or_insert_with(Probe::default);
    p.frame += 1;
    if p.frame % 30 != 0 {
        return;
    }
    let key = format!("screen_{:?}_{}", ctx.client_scene_kind(), ctx.client_main_tab().unwrap_or_default());
    tree_once(ctx, p, &key);
}

fn tree_once(ctx: &StableClient, p: &mut Probe, key: &str) {
    let key = file_safe(key);
    if p.trees.len() >= MAX_TREES || !p.trees.insert(key.clone()) {
        return;
    }
    let mut lines = Vec::new();
    crate::ui::dump_tree(ctx, "", 0, TREE_DEPTH, TREE_LINES, &mut lines);
    write(p, &format!("tree_{}.txt", key), &lines.join("\n"));
}

fn records(ctx: &StableClient, read: &board::Read) -> String {
    use RecordKindV1::*;
    let mut s = String::from("== record counts ==\n");
    for k in [
        Team,
        Athlete,
        League,
        Tournament,
        Staff,
        MatchNormal,
        MatchPractice,
        MatchTutorial,
        MatchSoloRank,
        LeagueCompetition,
        TournamentCompetition,
        SoloRankMatch,
        KnowledgeBase,
        YearSchedule,
        MatchReplay,
    ] {
        s.push_str(&format!("{:?}: {}\n", k, ctx.record_ids(k).len()));
    }
    // a replay has no back-pointer; its match record's `replays` array does
    let mut owner: HashMap<usize, RecordKindV1> = HashMap::new();
    for k in [MatchNormal, MatchPractice, MatchTutorial, MatchSoloRank] {
        for id in ctx.record_ids(k) {
            if let Some(v) = ctx.record_get_json(k, id, "replays").as_deref().and_then(JsonVal::parse) {
                for r in v.as_arr().into_iter().flatten().filter_map(|x| x.as_usize()) {
                    owner.insert(r, k);
                }
            }
        }
    }
    let replays = ctx.record_ids(MatchReplay);
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for r in &replays {
        *by.entry(owner.get(r).map_or("unclaimed".to_string(), |k| format!("{:?}", k))).or_default() += 1;
    }
    let by: Vec<String> = by.iter().map(|(k, n)| format!("{}={}", k, n)).collect();
    s.push_str(&format!("\n== replay owners (E3) ==\n{}\n", by.join(" ")));

    let my_team = ctx.player_team_id();
    let first = |v: &[Option<usize>; 5]| v.iter().flatten().next().copied();
    for (label, k, id) in [
        ("my first starter", Athlete, first(&read.my_starters)),
        ("enemy first starter", Athlete, first(&read.enemy_starters)),
        ("my team", Team, my_team),
        ("my team's scouting", KnowledgeBase, my_team),
        ("latest official match", MatchNormal, ctx.record_ids(MatchNormal).into_iter().max()),
        ("latest practice match", MatchPractice, ctx.record_ids(MatchPractice).into_iter().max()),
        ("latest replay", MatchReplay, replays.iter().max().copied()),
    ] {
        let body = match id {
            Some(id) => ctx.record_get_json(k, id, "").map_or("(no record)".to_string(), |j| clip(&j, RECORD_CAP)),
            None => "(no id)".to_string(),
        };
        s.push_str(&format!("\n== {} — {:?} {:?} ==\n{}\n", label, k, id, body));
    }
    s
}

fn write(p: &mut Probe, name: &str, body: &str) {
    let dir = format!("{}/probe", diag::base());
    let _ = std::fs::create_dir_all(&dir);
    diag::atomic_write(&format!("{}/{}", dir, name), body);
    p.written.push(format!("{} {}B", name, body.len()));
    diag::set("10 probe", format!("E2 written: {}", p.written.join(", ")));
}

/// The banpick phase a tree dump belongs to.
fn bp_key(swap: bool, rec_ban: Option<bool>, deleg: bool) -> String {
    let phase = match (swap, rec_ban) {
        (true, _) => "swap",
        (false, Some(true)) => "ban",
        (false, Some(false)) => "pick",
        (false, None) => "wait",
    };
    if deleg {
        format!("{}_deleg", phase)
    } else {
        phase.to_string()
    }
}

/// `s` cut to at most `max` bytes on a char boundary, marked when cut.
fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [cut at {} of {} bytes]", &s[..end], end, s.len())
}

/// A dump key as a file name: letters, digits and `_` only.
fn file_safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banpick_trees_are_keyed_by_phase_and_delegation() {
        assert_eq!(bp_key(false, Some(true), false), "ban");
        assert_eq!(bp_key(false, Some(false), true), "pick_deleg");
        assert_eq!(bp_key(true, Some(false), false), "swap");
        assert_eq!(bp_key(false, None, false), "wait");
    }

    #[test]
    fn clip_cuts_on_a_char_boundary_and_says_so() {
        assert_eq!(clip("abc", 8), "abc");
        // 3-byte chars: only 가 fits in 4 bytes
        let c = clip("가나다", 4);
        assert!(c.starts_with("가…"), "{}", c);
        assert!(c.contains("of 9 bytes"), "{}", c);
    }

    #[test]
    fn file_names_keep_only_safe_chars() {
        assert_eq!(file_safe("screen_Some(MatchResult)_Home"), "screen_Some_MatchResult__Home");
    }
}
