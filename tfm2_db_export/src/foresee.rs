//! The opponent AI's next ban/pick — review experiment E1b (2026-10-28,
//! docs/research/2026-10-28-advanced-directions.md "E1b"). At the player's turn
//! its next decision is ranked from what is already known: its own scores from
//! its previous decision of the same phase (made before the current turn) and
//! the league average for that draft slot — every AI decision the draft hook has
//! scored, each centered on its own mean. E1b: top-3 hit 94% for bans, 74% for
//! picks in AI-vs-AI drafts; a learned board-delta model added nothing. The
//! player's own matches hit less (0.7.0, 10-28: 7 of 15 where those rates
//! expected ~12), so the line shows the player's own record instead. Shown only
//! when the player turns it on (F2 or the note card's chip); the score model is
//! untouched. `observe` runs on the hook's threads, everything else on the main
//! thread.

use crate::diag;
use crate::markup;
use crate::recommender::Board;
use mod_api_stable::{StableUiEvent, UiEventKindV1};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// The measured format: two bans a side, then five picks a side.
const BANS: usize = 2;
const PICKS: usize = 10;
/// After the bans, true = the side that acts first: A BB AA BB AA B (E1b, 286/286 sets).
const PICK_FIRST: [bool; PICKS] = [true, false, false, true, true, false, false, true, true, false];
/// E1b top-3 hit rate (%) per draft slot with this module's ranking, in
/// AI-vs-AI drafts; the last pick had no later hook call to grade it with.
/// Logged beside each grade of the player's matches, no longer shown.
const TOP3_PCT: [Option<u8>; 2 * BANS + PICKS] = [
    Some(97),
    Some(88),
    Some(94),
    Some(98),
    Some(92),
    Some(70),
    Some(78),
    Some(69),
    Some(69),
    Some(63),
    Some(78),
    Some(76),
    Some(74),
    None,
];
/// E1b: the league mean ranked well once a slot had ~10 decisions behind it
const MIN_LEAGUE: u32 = 10;
/// decisions still being scored at once (E1b: at most 8 drafts in one second)
const MAX_OPEN: usize = 64;
/// the opponent's decisions held for this match
const MAX_OPP: usize = 32;
/// E1b's league value for a champ never scored at a slot: below any centered score
const UNSEEN: f64 = -1.0;

/// (does the side that acts first make it, phase) for a draft slot.
pub(crate) fn actor(slot: usize) -> Option<(bool, char)> {
    if slot < 2 * BANS {
        Some((slot % 2 == 0, 'B'))
    } else {
        PICK_FIRST.get(slot - 2 * BANS).map(|&first| (first, 'P'))
    }
}

/// The opponent's next decision once `made` decisions are on the board.
pub(crate) fn next_enemy_slot(made: usize, i_am_first: bool) -> Option<(usize, char)> {
    (made..2 * BANS + PICKS).find_map(|s| actor(s).filter(|&(first, _)| first != i_am_first).map(|(_, ph)| (s, ph)))
}

/// Where the turn holding `slot` began — a double pick is one turn.
pub(crate) fn turn_start(slot: usize) -> usize {
    let mut s = slot;
    while s > 0 && actor(s - 1) == actor(slot) {
        s -= 1;
    }
    s
}

/// The side making `slot` already decided in this phase before that turn, so
/// E1b ranked it with those scores (the blend), not the league mean alone.
fn expects_last(slot: usize) -> bool {
    (0..turn_start(slot)).any(|s| actor(s) == actor(slot))
}

/// One scored candidate, as the hook sees it.
pub(crate) struct Call<'a> {
    pub phase: char,
    /// one AI decision: phase, both team ids and the board it scores
    pub key: u64,
    /// the draft's two team ids, lower first
    pub pair: (usize, usize),
    /// bans + picks on that board = the draft slot being decided
    pub size: usize,
    /// the AI drafts against the player's team: the opponent in the player's match
    pub vs_me: bool,
    pub cand: &'a str,
    pub score: f32,
}

struct Open {
    key: u64,
    pair: (usize, usize),
    phase: char,
    size: usize,
    seen: u64,
    scores: HashMap<String, f32>,
}

/// One decision of the opponent AI in the player's match.
pub(crate) struct OppScan {
    key: u64,
    pub phase: char,
    pub size: usize,
    /// every champion on the board it scored
    pub board: Vec<String>,
    pub scores: HashMap<String, f32>,
}

#[derive(Default)]
pub(crate) struct Scans {
    /// decisions still being scored: one per draft, closed when its next starts
    open: Vec<Open>,
    /// (phase, slot) → (decisions, champ → (Σ score − its decision's mean, decisions it was in))
    league: HashMap<(char, usize), (u32, HashMap<String, (f64, u32)>)>,
    /// kept as the calls arrive — the player's own moves make no hook calls, so
    /// the opponent's last decision would otherwise stay open through my turn
    opp: Vec<OppScan>,
    calls: u64,
    pub closed: u64,
    /// closed by the MAX_OPEN cap rather than by their draft moving on
    pub closed_early: u64,
}

impl Scans {
    pub fn add(&mut self, c: &Call, board: impl FnOnce() -> Vec<String>) {
        self.calls += 1;
        let at = match self.open.iter().position(|o| o.key == c.key) {
            Some(i) => i,
            None => {
                // a draft's next decision: its previous one is complete
                if let Some(i) = self.open.iter().position(|o| o.pair == c.pair) {
                    let done = self.open.swap_remove(i);
                    self.close(done);
                }
                if self.open.len() >= MAX_OPEN {
                    if let Some(i) = (0..self.open.len()).min_by_key(|&i| self.open[i].seen) {
                        let stale = self.open.swap_remove(i);
                        self.closed_early += 1;
                        self.close(stale);
                    }
                }
                self.open.push(Open {
                    key: c.key,
                    pair: c.pair,
                    phase: c.phase,
                    size: c.size,
                    seen: 0,
                    scores: HashMap::new(),
                });
                self.open.len() - 1
            }
        };
        self.open[at].seen = self.calls;
        self.open[at].scores.insert(c.cand.to_string(), c.score);
        if !c.vs_me {
            return;
        }
        let i = match self.opp.iter().rposition(|s| s.key == c.key) {
            Some(i) => i,
            None => {
                if self.opp.len() >= MAX_OPP {
                    self.opp.remove(0);
                }
                self.opp.push(OppScan { key: c.key, phase: c.phase, size: c.size, board: board(), scores: HashMap::new() });
                self.opp.len() - 1
            }
        };
        self.opp[i].scores.insert(c.cand.to_string(), c.score);
    }

    fn close(&mut self, o: Open) {
        self.closed += 1;
        if o.scores.is_empty() {
            return;
        }
        let mean = o.scores.values().map(|&v| v as f64).sum::<f64>() / o.scores.len() as f64;
        let (n, champs) = self.league.entry((o.phase, o.size)).or_default();
        *n += 1;
        for (c, v) in o.scores {
            let e = champs.entry(c).or_insert((0.0, 0));
            e.0 += v as f64 - mean;
            e.1 += 1;
        }
    }

    /// The league mean per champ at one draft slot, and the decisions behind it.
    pub fn league_at(&self, phase: char, slot: usize) -> (HashMap<String, f64>, u32) {
        match self.league.get(&(phase, slot)) {
            Some((n, champs)) => (champs.iter().map(|(c, &(sum, k))| (c.clone(), sum / k as f64)).collect(), *n),
            None => (HashMap::new(), 0),
        }
    }

    /// The opponent's latest decision of `phase` made before `before` decisions,
    /// on a board that is part of this one (so: this set).
    pub fn last_opp(&self, phase: char, before: usize, on_board: &HashSet<&str>) -> Option<&OppScan> {
        self.opp
            .iter()
            .rev()
            .find(|s| s.phase == phase && s.size < before && s.board.iter().all(|c| on_board.contains(c.as_str())))
    }

    fn summary(&self) -> String {
        let depth: Vec<String> = (0..2 * BANS + PICKS)
            .map(|s| {
                let ph = actor(s).map_or('?', |a| a.1);
                self.league.get(&(ph, s)).map_or(0, |l| l.0).to_string()
            })
            .collect();
        format!(
            "scans: calls={} open={} closed={} closed_early={} opp_decisions={}\nleague decisions by slot 1-14: {}",
            self.calls,
            self.open.len(),
            self.closed,
            self.closed_early,
            self.opp.len(),
            depth.join(" ")
        )
    }
}

/// Candidates by z(the opponent's last scores) + z(league mean), or by the
/// league alone; ties by id.
pub(crate) fn rank(open: &[String], last: Option<&HashMap<String, f32>>, league: &HashMap<String, f64>) -> Vec<String> {
    // the opponent scored every champ it could take then: one it did not score
    // is gone for it
    let cands: Vec<&String> = open.iter().filter(|c| last.map_or(true, |l| l.contains_key(*c))).collect();
    let lg: Vec<f64> = cands.iter().map(|c| league.get(*c).copied().unwrap_or(UNSEEN)).collect();
    let score: Vec<f64> = match last {
        Some(l) => {
            let mine = z(&cands.iter().map(|c| l[*c] as f64).collect::<Vec<_>>());
            mine.iter().zip(z(&lg)).map(|(a, b)| a + b).collect()
        }
        None => lg,
    };
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&i, &j| score[j].total_cmp(&score[i]).then_with(|| cands[i].cmp(cands[j])));
    order.into_iter().map(|i| cands[i].clone()).collect()
}

/// Standard scores over the candidates (population sd), as E1b blended them.
fn z(v: &[f64]) -> Vec<f64> {
    let n = v.len().max(1) as f64;
    let m = v.iter().sum::<f64>() / n;
    let sd = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n).sqrt().max(1e-9);
    v.iter().map(|x| (x - m) / sd).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Guess {
    pub phase: char,
    pub slot: usize,
    pub top: Vec<String>,
    /// E1b's league rate for this slot, when ranked the way E1b measured it
    pub top3_pct: Option<u8>,
    /// the opponent's own earlier scores took part (else the league alone)
    pub with_last: bool,
    /// decisions behind the league mean at this slot
    pub league_n: u32,
}

/// The opponent's next decision on this board: its likeliest three.
pub(crate) fn guess(b: &Board, pool: &[String], scans: &Scans) -> Option<Guess> {
    if b.ban_count != BANS {
        return None;
    }
    let made = b.my_ban.len() + b.enemy_ban.len() + b.my_pick.len() + b.enemy_pick.len();
    let (slot, phase) = next_enemy_slot(made, b.i_am_team1?)?;
    let (league, league_n) = scans.league_at(phase, slot);
    if league_n < MIN_LEAGUE {
        return None;
    }
    let taken: HashSet<&str> =
        [&b.my_ban, &b.enemy_ban, &b.my_pick, &b.enemy_pick].into_iter().flatten().map(|c| c.as_str()).collect();
    let last = scans.last_opp(phase, turn_start(slot), &taken);
    let open: Vec<String> = match last {
        // what the opponent could take then, less what is gone since
        Some(l) => l.scores.keys().filter(|c| !taken.contains(c.as_str())).cloned().collect(),
        // the grid shows what this save can pick; below 10 tiles it is still building
        None if pool.len() >= 10 => pool
            .iter()
            .filter(|c| !taken.contains(c.as_str()) && !(phase == 'P' && b.enemy_locked.contains(c)))
            .cloned()
            .collect(),
        None => return None,
    };
    let top: Vec<String> = rank(&open, last.map(|l| &l.scores), &league).into_iter().take(3).collect();
    if top.is_empty() {
        return None;
    }
    let with_last = last.is_some();
    Some(Guess {
        phase,
        slot,
        top,
        top3_pct: if with_last == expects_last(slot) { TOP3_PCT[slot] } else { None },
        with_last,
        league_n,
    })
}

/// "상대 다음 밴: A·B·C (내 경기 3/5)" — the game's names where known, and the
/// player's own record for the phase (top-3 hits / decisions graded) once it
/// has one (Record::shown).
pub(crate) fn line(g: &Guess, names: &HashMap<String, String>, record: Option<(u32, u32)>) -> String {
    let who: Vec<String> =
        g.top.iter().map(|c| markup::name(names.get(c).map_or(c.as_str(), |n| n.as_str()))).collect();
    let rate = match record {
        // the record counts lists of three
        Some((graded, hits)) if g.top.len() >= 3 => markup::dim(&format!(" (내 경기 {}/{})", hits, graded)),
        _ => String::new(),
    };
    format!("상대 다음 {}: {}{}", if g.phase == 'B' { "밴" } else { "픽" }, who.join("·"), rate)
}

/// The player's own matches under one game version: [bans, picks] →
/// (decisions graded, top-1 hits, top-3 hits). A game update may change the
/// AI, so a new version starts over. Kept in RECORD_FILE across sessions.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Record {
    pub game: String,
    pub graded: [(u32, u32, u32); 2],
}

/// Graded decisions of a phase before its record is shown: fewer is noise.
const MIN_RECORD: u32 = 5;

impl Record {
    pub fn add(&mut self, phase: char, top1: bool, top3: bool) {
        let e = &mut self.graded[(phase == 'P') as usize];
        *e = (e.0 + 1, e.1 + top1 as u32, e.2 + top3 as u32);
    }

    /// (decisions graded, top-3 hits) for the line, once there are enough.
    pub fn shown(&self, phase: char) -> Option<(u32, u32)> {
        let (n, _, hits) = self.graded[(phase == 'P') as usize];
        (n >= MIN_RECORD).then_some((n, hits))
    }
}

pub(crate) fn format_record(r: &Record) -> String {
    let [(bn, b1, b3), (pn, p1, p3)] = r.graded;
    format!("game {}\nB {} {} {}\nP {} {} {}\n", r.game, bn, b1, b3, pn, p1, p3)
}

/// The record for `game`: the file's counts if they are that version's, else
/// none yet. A row that does not add up is dropped.
pub(crate) fn parse_record(s: &str, game: &str) -> Record {
    let mut r = Record { game: game.to_string(), ..Default::default() };
    let mut same = false;
    for l in s.lines() {
        match l.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["game", v] => same = *v == game,
            [ph @ ("B" | "P"), n, t1, t3] => {
                if let (Ok(n), Ok(t1), Ok(t3)) = (n.parse::<u32>(), t1.parse::<u32>(), t3.parse::<u32>()) {
                    if t1 <= t3 && t3 <= n {
                        r.graded[(*ph == "P") as usize] = (n, t1, t3);
                    }
                }
            }
            _ => {}
        }
    }
    if same {
        r
    } else {
        Record { game: game.to_string(), ..Default::default() }
    }
}

/// One guess graded against the opponent's actual move.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Grade {
    pub phase: char,
    pub top1: bool,
    pub top3: bool,
    /// the board_log line
    pub line: String,
}

/// Every guess against the opponent's actual move (diag "11 opp").
#[derive(Default)]
pub(crate) struct Live {
    /// the latest guess, with the length of the opponent's list for its phase then
    held: Option<(Guess, usize)>,
    /// [bans, picks]: (graded, top-1 hits, top-3 hits)
    pub graded: [(u32, u32, u32); 2],
    pub last: String,
}

impl Live {
    /// Grades the held guess once the opponent's list for its phase grows, then
    /// holds `now`.
    pub fn step(&mut self, enemy_ban: &[String], enemy_pick: &[String], now: Option<&Guess>) -> Option<Grade> {
        let list = |phase: char| if phase == 'B' { enemy_ban } else { enemy_pick };
        let mut graded = None;
        if let Some((g, then)) = self.held.take() {
            let l = list(g.phase);
            match l.len().cmp(&then) {
                std::cmp::Ordering::Equal => self.held = Some((g, then)),
                std::cmp::Ordering::Greater => {
                    let actual = &l[then];
                    let (top1, top3) = (g.top.first() == Some(actual), g.top.contains(actual));
                    let e = &mut self.graded[(g.phase == 'P') as usize];
                    *e = (e.0 + 1, e.1 + top1 as u32, e.2 + top3 as u32);
                    self.last = format!(
                        "opp_next {} slot={} with_last={} league_n={} league_pct={} top=[{}] actual={} top1={} top3={}",
                        g.phase,
                        g.slot + 1,
                        g.with_last as u8,
                        g.league_n,
                        g.top3_pct.map_or_else(|| "-".to_string(), |p| p.to_string()),
                        g.top.join(","),
                        actual,
                        top1 as u8,
                        top3 as u8
                    );
                    graded = Some(Grade { phase: g.phase, top1, top3, line: self.last.clone() });
                }
                // shorter: a new set began, the guess goes ungraded
                std::cmp::Ordering::Less => {}
            }
        }
        if let Some(g) = now {
            match &mut self.held {
                // my own move changed the board: the latest guess is the one that counts
                Some((h, _)) if (h.slot, h.phase) == (g.slot, g.phase) => *h = g.clone(),
                _ => self.held = Some((g.clone(), list(g.phase).len())),
            }
        }
        graded
    }
}

// ── runtime: the hook threads feed STATE, the main thread reads it once a frame

#[derive(Default)]
struct State {
    scans: Scans,
    live: Live,
    /// this frame's guess, for diag
    now: String,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static ON: AtomicBool = AtomicBool::new(false);
static TOGGLES: AtomicU32 = AtomicU32::new(0);
static CLICKED: AtomicBool = AtomicBool::new(false);
static CLICKS: AtomicU32 = AtomicU32::new(0);
static TICK: AtomicU32 = AtomicU32::new(0);
/// "on" in %LOCALAPPDATA%/tfm2-overlay/opp_next.txt shows the line (default off)
const SETTING: &str = "opp_next.txt";
/// the player's own record (Record), next to SETTING
const RECORD_FILE: &str = "opp_next_hits.txt";
static RECORD: Mutex<Option<Record>> = Mutex::new(None);
static CHIP_LOG: Mutex<ChipLog> = Mutex::new(ChipLog::new());

/// The draft hook, any thread: one scored candidate.
pub(crate) fn observe(c: &Call, board: impl FnOnce() -> Vec<String>) {
    if let Ok(mut st) = STATE.lock() {
        st.get_or_insert_with(State::default).scans.add(c, board);
    }
}

/// A banpick frame: the guess on this board, graded once the opponent moves.
/// Returns the note card's line (shown while the player has it on).
pub(crate) fn frame(b: Option<&Board>, pool: &[String], names: &HashMap<String, String>) -> Option<String> {
    let (g, graded) = {
        let Ok(mut st) = STATE.lock() else { return None };
        let st = st.get_or_insert_with(State::default);
        let g = b.and_then(|b| guess(b, pool, &st.scans));
        let graded = b.and_then(|b| st.live.step(&b.enemy_ban, &b.enemy_pick, g.as_ref()));
        st.now = g.as_ref().map_or_else(
            || "-".to_string(),
            |g| {
                format!(
                    "slot={} {} top=[{}] with_last={} league_n={} top3_pct={:?}",
                    g.slot + 1,
                    g.phase,
                    g.top.join(","),
                    g.with_last as u8,
                    g.league_n,
                    g.top3_pct
                )
            },
        );
        (g, graded)
    };
    if let Some(gr) = graded {
        diag::log_line(&gr.line);
        record_grade(&gr);
    }
    g.map(|g| line(&g, names, record_shown(g.phase)))
}

/// Mod init: this game version's record (a new version starts with none).
pub(crate) fn load_record(game: &str) {
    let s = std::fs::read_to_string(format!("{}/{}", diag::base(), RECORD_FILE)).unwrap_or_default();
    if let Ok(mut r) = RECORD.lock() {
        *r = Some(parse_record(&s, game));
    }
}

fn record_grade(gr: &Grade) {
    let body = {
        let Ok(mut r) = RECORD.lock() else { return };
        let Some(r) = r.as_mut() else { return };
        r.add(gr.phase, gr.top1, gr.top3);
        format_record(r)
    };
    let _ = std::fs::create_dir_all(diag::base());
    diag::atomic_write(&format!("{}/{}", diag::base(), RECORD_FILE), &body);
}

fn record_shown(phase: char) -> Option<(u32, u32)> {
    RECORD.lock().ok().and_then(|r| r.as_ref().and_then(|r| r.shown(phase)))
}

/// Each InGame frame: the diag section, every 30 frames.
pub(crate) fn tick() {
    if TICK.fetch_add(1, Ordering::Relaxed) % 30 != 0 {
        return;
    }
    let record = RECORD.lock().ok().and_then(|r| r.as_ref().map(format_record)).unwrap_or_default();
    let chip = CHIP_LOG.lock().map(|l| l.summary()).unwrap_or_default();
    let body = {
        let Ok(mut st) = STATE.lock() else { return };
        let st = st.get_or_insert_with(State::default);
        let [(bn, b1, b3), (pn, p1, p3)] = st.live.graded;
        format!(
            "on={} toggles={} chip: register_click={} {} (F2 or the note card's chip; {})\nnow: {}\n\
             graded this session: bans n={} top1={} top3={} | picks n={} top1={} top3={}\n\
             record ({}, shown from {} a phase): {}\nlast: {}\n{}",
            on(),
            TOGGLES.load(Ordering::Relaxed),
            CLICKS.load(Ordering::Relaxed),
            chip,
            SETTING,
            if st.now.is_empty() { "-" } else { &st.now },
            bn,
            b1,
            b3,
            pn,
            p1,
            p3,
            RECORD_FILE,
            MIN_RECORD,
            record.trim_end().replace('\n', " | "),
            if st.live.last.is_empty() { "-" } else { &st.live.last },
            st.scans.summary()
        )
    };
    diag::set("11 opp", body);
}

/// Left the banpick screen: that set's opponent decisions are done with (the
/// next set may swap sides, and an empty-board decision fits any board).
pub(crate) fn leave() {
    if let Ok(mut st) = STATE.lock() {
        if let Some(st) = st.as_mut() {
            st.scans.opp.clear();
            st.live.held = None;
        }
    }
}

/// Left InGame, or the patch moved (the AI's scores move with it): start over.
pub(crate) fn reset() {
    if let Ok(mut st) = STATE.lock() {
        *st = None;
    }
}

fn setting_path() -> String {
    format!("{}/{}", diag::base(), SETTING)
}

/// Mod init: the player's last choice.
pub(crate) fn load_setting() {
    let on = std::fs::read_to_string(setting_path()).map_or(false, |s| parse_setting(&s));
    ON.store(on, Ordering::Relaxed);
}

pub(crate) fn parse_setting(s: &str) -> bool {
    s.trim() == "on"
}

pub(crate) fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// F2 or the chip: flip, and remember it for the next launch.
pub(crate) fn toggle() {
    let on = !ON.fetch_xor(true, Ordering::Relaxed);
    TOGGLES.fetch_add(1, Ordering::Relaxed);
    let _ = std::fs::create_dir_all(diag::base());
    diag::atomic_write(&setting_path(), if on { "on\n" } else { "off\n" });
    diag::log_line(&format!("opp_next {}", if on { "on" } else { "off" }));
}

/// The chip's click handler (a reduced client context): only a flag.
pub(crate) fn clicked() {
    CLICKS.fetch_add(1, Ordering::Relaxed);
    CLICKED.store(true, Ordering::Relaxed);
}

/// Every UI event that reached the chip's path (`ui_register_path_events`).
/// 0.7.0 (10-28): nine toggles, all F2, and the click handler never ran — this
/// shows whether a click arrives at all, and in what shape.
pub(crate) struct ChipLog {
    pub events: u32,
    pub clicks: u32,
    /// the first few, as the host described them
    pub first: Vec<String>,
}

impl ChipLog {
    pub const fn new() -> ChipLog {
        ChipLog { events: 0, clicks: 0, first: Vec::new() }
    }

    /// Notes one event; true for a click.
    pub fn note(&mut self, ev: Option<&StableUiEvent>) -> bool {
        self.events += 1;
        let click = ev.map_or(false, |e| e.kind == Some(UiEventKindV1::Click));
        self.clicks += click as u32;
        if self.first.len() < 4 {
            self.first.push(ev.map_or_else(
                || "-".to_string(),
                |e| format!("{:?} {} {}", e.kind, e.path, e.payload_json.chars().take(80).collect::<String>()),
            ));
        }
        click
    }

    fn summary(&self) -> String {
        format!("path_events={} path_clicks={} first=[{}]", self.events, self.clicks, self.first.join(" ; "))
    }
}

/// The chip's path-event handler (a reduced client context): a click toggles
/// like the click handler does.
pub(crate) fn chip_event(ev: Option<StableUiEvent>) {
    let click = CHIP_LOG.lock().map_or(false, |mut l| l.note(ev.as_ref()));
    if click {
        CLICKED.store(true, Ordering::Relaxed);
    }
}

/// One toggle per frame, however many handlers the chip has collected.
pub(crate) fn take_click() -> bool {
    CLICKED.swap(false, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn call(phase: char, key: u64, pair: (usize, usize), size: usize, vs_me: bool, cand: &str, score: f32) -> Call<'_> {
        Call { phase, key, pair, size, vs_me, cand, score }
    }

    /// One whole decision: every (candidate, score) under one key.
    fn scan(sc: &mut Scans, phase: char, key: u64, pair: (usize, usize), size: usize, vs_me: bool, board: &[&str], cands: &[(&str, f32)]) {
        for &(c, v) in cands {
            sc.add(&call(phase, key, pair, size, vs_me, c, v), || s(board));
        }
    }

    /// A background decision, closed by its draft's next one.
    fn league_scan(sc: &mut Scans, phase: char, size: usize, pair: (usize, usize), key: u64, cands: &[(&str, f32)]) {
        scan(sc, phase, key, pair, size, false, &[], cands);
        sc.add(&call(phase, key + 1000, pair, size + 1, false, "zz", 0.0), Vec::new);
    }

    /// `n` background decisions at one slot, ids unique per (phase, slot).
    fn league(sc: &mut Scans, phase: char, size: usize, n: usize, cands: &[(&str, f32)]) {
        for d in 0..n {
            let id = 1000 * (size + 1) + 500 * (phase == 'P') as usize + 2 * d;
            scan(sc, phase, id as u64, (id, id + 1), size, false, &[], cands);
            sc.add(&call(phase, id as u64 + 1_000_000, (id, id + 1), size + 1, false, "zz", 0.0), Vec::new);
        }
    }

    fn board(my_ban: &[&str], enemy_ban: &[&str], my_pick: &[&str], enemy_pick: &[&str], first: bool) -> Board {
        Board {
            my_ban: s(my_ban),
            enemy_ban: s(enemy_ban),
            my_pick: s(my_pick),
            enemy_pick: s(enemy_pick),
            ban_count: 2,
            i_am_team1: Some(first),
            ..Default::default()
        }
    }

    fn guessed(phase: char, slot: usize, top: &[&str], pct: Option<u8>) -> Guess {
        Guess { phase, slot, top: s(top), top3_pct: pct, with_last: false, league_n: 5 }
    }

    #[test]
    fn the_draft_runs_two_bans_a_side_then_a_snake_of_picks() {
        let order: Vec<String> = (0..14)
            .map(|i| {
                let (first, ph) = actor(i).unwrap();
                format!("{}{}", if first { 'A' } else { 'B' }, ph)
            })
            .collect();
        assert_eq!(order.join(" "), "AB BB AB BB AP BP BP AP AP BP BP AP AP BP");
        assert_eq!(actor(14), None);
    }

    #[test]
    fn the_next_enemy_decision_skips_my_own() {
        // I act first (A): the opponent is B
        assert_eq!(next_enemy_slot(0, true), Some((1, 'B')));
        assert_eq!(next_enemy_slot(1, true), Some((1, 'B')), "their turn now");
        assert_eq!(next_enemy_slot(4, true), Some((5, 'P')));
        assert_eq!(next_enemy_slot(7, true), Some((9, 'P')), "past my double pick");
        assert_eq!(next_enemy_slot(14, true), None);
        // I act second (B): the opponent is A
        assert_eq!(next_enemy_slot(0, false), Some((0, 'B')));
        assert_eq!(next_enemy_slot(3, false), Some((4, 'P')));
        assert_eq!(next_enemy_slot(13, false), None, "the last pick is mine");
    }

    #[test]
    fn a_double_pick_is_one_turn() {
        assert_eq!(turn_start(2), 2);
        assert_eq!(turn_start(5), 5);
        assert_eq!(turn_start(6), 5);
        assert_eq!(turn_start(8), 7);
    }

    #[test]
    fn a_decision_joins_the_league_once_its_draft_moves_on() {
        let mut sc = Scans::default();
        scan(&mut sc, 'B', 1, (3, 4), 0, false, &[], &[("a", 0.30), ("b", 0.20), ("c", 0.10)]);
        assert_eq!(sc.league_at('B', 0).1, 0, "still being scored");
        sc.add(&call('B', 2, (3, 4), 1, false, "b", 0.2), Vec::new);
        let (m, n) = sc.league_at('B', 0);
        assert_eq!(n, 1);
        assert!((m["a"] - 0.10).abs() < 1e-6 && (m["c"] + 0.10).abs() < 1e-6, "centered on its own mean: {:?}", m);
        assert_eq!(sc.closed, 1);
    }

    #[test]
    fn the_least_recent_open_decision_closes_when_too_many_are_open() {
        let mut sc = Scans::default();
        for d in 0..=MAX_OPEN {
            scan(&mut sc, 'P', 100 + d as u64, (d, 1000 + d), 4, false, &[], &[("a", 0.5), ("b", 0.4)]);
        }
        assert_eq!(sc.league_at('P', 4).1, 1, "the first draft's decision closed");
        assert_eq!((sc.closed, sc.closed_early), (1, 1));
    }

    #[test]
    fn the_league_averages_the_decisions_at_a_slot() {
        let mut sc = Scans::default();
        league_scan(&mut sc, 'B', 0, (1, 2), 1, &[("a", 0.4), ("b", 0.2)]);
        league_scan(&mut sc, 'B', 0, (3, 4), 2, &[("a", 0.2), ("b", 0.4)]);
        league_scan(&mut sc, 'B', 0, (5, 6), 3, &[("a", 0.5), ("b", 0.1)]);
        let (m, n) = sc.league_at('B', 0);
        assert_eq!(n, 3);
        assert!((m["a"] - (0.1 - 0.1 + 0.2) / 3.0).abs() < 1e-6, "{:?}", m);
    }

    #[test]
    fn the_opponents_scores_are_held_while_still_open() {
        let mut sc = Scans::default();
        scan(&mut sc, 'B', 1, (7, 8), 0, true, &[], &[("a", 0.3), ("b", 0.2)]);
        let on: HashSet<&str> = ["x"].into_iter().collect();
        assert_eq!(sc.last_opp('B', 2, &on).expect("held as it arrives").scores.len(), 2);
        assert!(sc.last_opp('P', 2, &on).is_none(), "another phase");
        assert!(sc.last_opp('B', 0, &on).is_none(), "not before the decision asked about");
    }

    #[test]
    fn background_drafts_are_never_the_opponents() {
        let mut sc = Scans::default();
        scan(&mut sc, 'B', 1, (3, 4), 0, false, &[], &[("a", 0.3)]);
        assert!(sc.last_opp('B', 5, &HashSet::new()).is_none());
    }

    #[test]
    fn an_earlier_sets_scores_are_not_this_sets() {
        let mut sc = Scans::default();
        // set 1: the opponent's second ban was scored with "x" banned
        scan(&mut sc, 'B', 1, (7, 8), 1, true, &["x"], &[("a", 0.3)]);
        assert!(sc.last_opp('B', 3, &HashSet::new()).is_none(), "a fresh board");
        let still: HashSet<&str> = ["x", "y"].into_iter().collect();
        assert!(sc.last_opp('B', 3, &still).is_some());
    }

    #[test]
    fn the_blend_ranks_by_both_scores() {
        let open = s(&["a", "b", "c", "d"]);
        let last: HashMap<String, f32> =
            [("a", 0.30), ("b", 0.29), ("c", 0.10), ("d", 0.00)].iter().map(|&(c, v)| (c.to_string(), v)).collect();
        let league: HashMap<String, f64> =
            [("a", -0.05), ("b", 0.05), ("c", 0.0), ("d", 0.0)].iter().map(|&(c, v)| (c.to_string(), v)).collect();
        assert_eq!(rank(&open, Some(&last), &league), s(&["b", "a", "c", "d"]));
        assert_eq!(rank(&open, None, &league)[0], "b");
    }

    #[test]
    fn ties_rank_by_id() {
        assert_eq!(rank(&s(&["c", "a", "b"]), None, &HashMap::new()), s(&["a", "b", "c"]));
    }

    #[test]
    fn the_opponents_first_ban_comes_from_the_league() {
        let mut sc = Scans::default();
        league(&mut sc, 'B', 1, 10, &[("a", 0.1), ("b", 0.5), ("c", 0.3), ("d", 0.2), ("x", 0.6)]);
        // I acted first and banned x: the opponent bans next (slot 1)
        let b = board(&["x"], &[], &[], &[], true);
        let pool = s(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "x"]);
        let g = guess(&b, &pool, &sc).unwrap();
        assert_eq!((g.slot, g.phase, g.with_last, g.league_n), (1, 'B', false, 10));
        assert_eq!(g.top, s(&["b", "c", "d"]));
        assert_eq!(g.top3_pct, Some(88));
    }

    #[test]
    fn the_opponents_second_ban_blends_its_first() {
        let mut sc = Scans::default();
        // the opponent (B) scored its first ban at slot 1, after my ban x …
        scan(&mut sc, 'B', 7, (7, 8), 1, true, &["x"], &[("a", 0.40), ("b", 0.30), ("c", 0.20), ("d", 0.10)]);
        league(&mut sc, 'B', 3, 10, &[("b", 0.3), ("c", 0.2), ("d", 0.1)]);
        // … banned a; I banned y: its next ban is slot 3
        let b = board(&["x", "y"], &["a"], &[], &[], true);
        let g = guess(&b, &s(&["a", "b", "c", "d", "x", "y"]), &sc).unwrap();
        assert_eq!((g.slot, g.with_last), (3, true));
        assert_eq!(g.top, s(&["b", "c", "d"]));
        assert_eq!(g.top3_pct, Some(98));
    }

    #[test]
    fn a_pick_guess_skips_the_opponents_fearless_locks() {
        let mut sc = Scans::default();
        league(&mut sc, 'P', 5, 10, &[("a", 0.5), ("b", 0.4), ("c", 0.3), ("d", 0.2)]);
        let mut b = board(&["w", "x"], &["y", "z"], &["e"], &[], true);
        b.enemy_locked = s(&["a"]);
        let pool = s(&["a", "b", "c", "d", "e", "f", "w", "x", "y", "z"]);
        let g = guess(&b, &pool, &sc).unwrap();
        assert_eq!((g.slot, g.phase), (5, 'P'));
        assert_eq!(g.top, s(&["b", "c", "d"]));
    }

    #[test]
    fn no_guess_until_the_league_has_the_decisions_e1b_needed() {
        let pool = s(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        let b = board(&["x"], &[], &[], &[], true);
        assert!(guess(&b, &pool, &Scans::default()).is_none(), "no league, no last scores");
        let cands = [("a", 0.5), ("b", 0.4)];
        let mut thin = Scans::default();
        league(&mut thin, 'B', 1, MIN_LEAGUE as usize - 1, &cands);
        assert!(guess(&b, &pool, &thin).is_none(), "a thin league");
        let mut warm = Scans::default();
        league(&mut warm, 'B', 1, MIN_LEAGUE as usize, &cands);
        assert!(guess(&b, &pool, &warm).is_some());
    }

    #[test]
    fn a_rate_needs_the_data_it_was_measured_with() {
        let mut sc = Scans::default();
        league(&mut sc, 'B', 3, 10, &[("b", 0.3), ("c", 0.2), ("d", 0.1)]);
        // the opponent's second ban without its first ban's scores (missed: the
        // mod's own team not yet known, say): E1b measured 98% WITH them
        let b = board(&["x", "y"], &["a"], &[], &[], true);
        let g = guess(&b, &s(&["a", "b", "c", "d", "e", "f", "g", "h", "x", "y"]), &sc).unwrap();
        assert_eq!((g.slot, g.with_last, g.top3_pct), (3, false, None));
    }

    #[test]
    fn the_second_of_a_double_pick_is_guessed_without_its_first() {
        let mut sc = Scans::default();
        league(&mut sc, 'P', 6, 10, &[("b", 0.4), ("c", 0.3), ("d", 0.2), ("e", 0.1)]);
        // the opponent (B) scored its first pick at slot 5 and took a: E1b ranked
        // slot 6 from what was known before that turn, so not from these scores
        scan(&mut sc, 'P', 9, (7, 8), 5, true, &["w", "x", "y", "z", "f"], &[("a", 0.9), ("e", 0.8), ("b", 0.1), ("c", 0.0), ("d", 0.0)]);
        let b = board(&["w", "x"], &["y", "z"], &["f"], &["a"], true);
        let g = guess(&b, &s(&["a", "b", "c", "d", "e", "f", "w", "x", "y", "z"]), &sc).unwrap();
        assert_eq!((g.slot, g.with_last, g.top3_pct), (6, false, Some(78)));
        assert_eq!(g.top, s(&["b", "c", "d"]), "the league's three, not e from the slot-5 scores");
    }

    #[test]
    fn no_guess_outside_the_measured_format() {
        let pool = s(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        let mut sc = Scans::default();
        league(&mut sc, 'B', 1, 10, &[("a", 0.5), ("b", 0.4)]);
        let mut three = board(&["x"], &[], &[], &[], true);
        three.ban_count = 3;
        assert!(guess(&three, &pool, &sc).is_none(), "three bans a side: unmeasured");
        let mut unknown = board(&["x"], &[], &[], &[], true);
        unknown.i_am_team1 = None;
        assert!(guess(&unknown, &pool, &sc).is_none(), "who acts first is unknown");
        assert!(guess(&board(&["x"], &[], &[], &[], true), &pool[..4], &sc).is_none(), "the grid is still loading");
    }

    #[test]
    fn the_line_names_three_and_my_matches_record() {
        let names: HashMap<String, String> =
            [("monk", "몽크"), ("executioner", "처형자"), ("knight", "기사")].iter().map(|&(a, b)| (a.to_string(), b.to_string())).collect();
        let l = line(&guessed('B', 1, &["monk", "executioner", "knight"], Some(94)), &names, Some((5, 3)));
        assert_eq!(markup::plain(&l), "상대 다음 밴: 몽크·처형자·기사 (내 경기 3/5)");
    }

    #[test]
    fn the_line_drops_a_record_it_does_not_have() {
        let none = HashMap::new();
        assert_eq!(markup::plain(&line(&guessed('P', 13, &["a", "b", "c"], None), &none, None)), "상대 다음 픽: a·b·c");
        // the league's measured rate overstated the player's own matches: not shown
        assert_eq!(markup::plain(&line(&guessed('P', 5, &["a", "b", "c"], Some(92)), &none, None)), "상대 다음 픽: a·b·c");
        // the record counts lists of three
        assert_eq!(markup::plain(&line(&guessed('P', 5, &["a", "b"], Some(70)), &none, Some((10, 4)))), "상대 다음 픽: a·b");
    }

    #[test]
    fn the_record_shows_once_a_phase_has_five_grades() {
        let mut r = Record { game: "0.7.0".to_string(), ..Default::default() };
        for i in 0..4 {
            r.add('P', false, i % 2 == 0);
        }
        assert_eq!(r.shown('P'), None);
        r.add('P', true, true);
        assert_eq!(r.shown('P'), Some((5, 3)));
        assert_eq!(r.graded[1], (5, 1, 3));
        assert_eq!(r.shown('B'), None, "bans count apart");
    }

    #[test]
    fn the_record_round_trips_through_its_file() {
        let r = Record { game: "0.7.0".to_string(), graded: [(5, 1, 3), (10, 1, 4)] };
        assert_eq!(format_record(&r), "game 0.7.0\nB 5 1 3\nP 10 1 4\n");
        assert_eq!(parse_record(&format_record(&r), "0.7.0"), r);
    }

    #[test]
    fn a_new_game_version_starts_the_record_over() {
        let r = Record { game: "0.7.0".to_string(), graded: [(5, 1, 3), (10, 1, 4)] };
        let fresh = |game: &str| Record { game: game.to_string(), ..Default::default() };
        assert_eq!(parse_record(&format_record(&r), "0.7.1"), fresh("0.7.1"));
        assert_eq!(parse_record("", "0.7.0"), fresh("0.7.0"), "no file yet");
        assert_eq!(parse_record("B 5 1 3\n", "0.7.0"), fresh("0.7.0"), "no version: whose counts?");
    }

    #[test]
    fn a_damaged_record_row_is_dropped() {
        let r = parse_record("game 0.7.0\nB 5 x 3\nP 4 1 9\nzz\n", "0.7.0");
        assert_eq!(r.graded, [(0, 0, 0), (0, 0, 0)]);
        // more hits than decisions, or a top-1 hit outside the top three
        assert_eq!(parse_record("game 0.7.0\nB 5 4 3\n", "0.7.0").graded[0], (0, 0, 0));
    }

    #[test]
    fn a_guess_is_graded_when_the_opponents_list_grows() {
        let mut live = Live::default();
        let g = guessed('B', 1, &["a", "b", "c"], Some(88));
        assert_eq!(live.step(&[], &[], Some(&g)), None, "held");
        assert_eq!(live.step(&[], &[], Some(&g)), None, "the opponent still deciding");
        let gr = live.step(&s(&["b"]), &[], None).expect("graded");
        assert_eq!((gr.phase, gr.top1, gr.top3), ('B', false, true));
        assert!(gr.line.contains("actual=b") && gr.line.contains("top1=0") && gr.line.contains("top3=1"), "{}", gr.line);
        // the league's rate for that slot stays in the log, to set beside the record
        assert!(gr.line.contains("league_pct=88"), "{}", gr.line);
        assert_eq!(live.graded[0], (1, 0, 1));
        assert_eq!(live.step(&s(&["b"]), &[], None), None, "graded once");
    }

    #[test]
    fn the_latest_guess_before_the_move_is_the_one_graded() {
        let mut live = Live::default();
        live.step(&[], &[], Some(&guessed('B', 1, &["a", "b", "c"], Some(88))));
        // my ban changed the board while the opponent waited
        live.step(&[], &[], Some(&guessed('B', 1, &["d", "e", "f"], Some(88))));
        let gr = live.step(&s(&["d"]), &[], None).expect("graded");
        assert!(gr.top1 && gr.line.contains("top1=1"), "{}", gr.line);
    }

    #[test]
    fn a_click_reaching_the_chip_toggles_and_every_event_is_logged() {
        let ev = |kind: Option<UiEventKindV1>, payload: &str| StableUiEvent {
            kind,
            path: "main.champion_info.tfm2dc_opp".to_string(),
            payload_json: payload.to_string(),
        };
        let mut l = ChipLog::new();
        assert!(l.note(Some(&ev(Some(UiEventKindV1::Click), "{\"item\":\"\"}"))));
        assert!(!l.note(Some(&ev(Some(UiEventKindV1::Changed), "{}"))), "not a click");
        assert!(!l.note(None), "the host described nothing");
        assert_eq!((l.events, l.clicks), (3, 1));
        assert!(l.first[0].contains("Click") && l.first[0].contains("tfm2dc_opp") && l.first[0].contains("item"), "{:?}", l.first);
        for _ in 0..10 {
            l.note(None);
        }
        assert_eq!((l.events, l.first.len()), (13, 4), "every event counted, the first few kept");
    }

    #[test]
    fn a_new_set_drops_the_held_guess() {
        let mut live = Live::default();
        live.step(&[], &s(&["q"]), Some(&guessed('P', 5, &["a"], None)));
        assert_eq!(live.step(&[], &[], None), None, "the board emptied");
        assert_eq!(live.step(&[], &s(&["a"]), None), None, "nothing held any more");
        assert_eq!(live.graded[1], (0, 0, 0));
    }

    #[test]
    fn only_an_explicit_on_turns_the_line_on() {
        assert!(parse_setting("on\n"));
        assert!(!parse_setting("off"));
        assert!(!parse_setting(""));
        assert!(!parse_setting("yes"));
    }
}
