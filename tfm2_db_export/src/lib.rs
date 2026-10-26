//! tfm2_db_export — Complete In-Game AI Draft Coach (v4.0, Stable SDK / Game 0.6.5+).
//!
//! Runs 100% in-process on top of `mod-api-stable` (ABI level 10):
//!   - Incremental JSON database cache (`RecordKindV1::MatchReplay`, `SoloRankMatch`,
//!     `Team`, `Athlete`) — parses existing records once on save load and only
//!     newly played match IDs thereafter (< 2ms per match).
//!   - Live Ban/Pick UI reader (`StableClient` UI path queries) — reads team names,
//!     starting 5 athletes, turn arrows, bans, ordered picks, fearless locks, and swap phase.
//!   - In-game UI overlay (`ui_spawn_source`, `ui_set_properties`, `ui_set_text`,
//!     `ui_set_champion_icon`) — renders non-colliding per-tile tier badges & borders,
//!     header/swap-modal optimal lane assignment, and a collapsible Top-5 AI Coach card.

mod recommender;

use mod_api_stable::*;
use recommender::{Board, ChgInfo, Output};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Engine tables built from the live stats (rebuilt whenever match counts change).
static TABLES: Mutex<Option<recommender::engine::Tables>> = Mutex::new(None);
/// Bumped on every TABLES rebuild — invalidates the recommendation cache.
static TABLES_GEN: AtomicUsize = AtomicUsize::new(0);
/// Current frame's recommendations/commentary/tiers for the in-game display.
static OUT_NOW: Mutex<Option<Arc<Output>>> = Mutex::new(None);

/// Shared directory = %LOCALAPPDATA%/tfm2-overlay (falls back to C:/tmp/tfm2-overlay).
fn base() -> String {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:/tmp".to_string());
    format!("{}/tfm2-overlay", local.replace('\\', "/"))
}

fn atomic_write(path: &str, body: &str) -> bool {
    let tmp = format!("{}.tmp", path);
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        true
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

/// Numeric per-component version key ("2026.10.0" sorts above "2026.2.0").
pub(crate) fn vkey(v: &str) -> Vec<u32> {
    v.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect()
}

// ───────────────────── Minimal Fast Zero-Dep JSON Parser ─────────────────────

#[derive(Clone, Debug)]
enum JsonVal {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<JsonVal>),
    Obj(Vec<(String, JsonVal)>),
}

impl JsonVal {
    fn parse(s: &str) -> Option<JsonVal> {
        let b = s.as_bytes();
        let mut i = 0;
        let v = Self::parse_val(b, &mut i)?;
        Some(v)
    }

    fn skip_ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\r' | b'\n') {
            *i += 1;
        }
    }

    fn parse_val(b: &[u8], i: &mut usize) -> Option<JsonVal> {
        Self::skip_ws(b, i);
        if *i >= b.len() {
            return None;
        }
        match b[*i] {
            b'"' => Self::parse_str(b, i).map(JsonVal::Str),
            b'{' => Self::parse_obj(b, i),
            b'[' => Self::parse_arr(b, i),
            b't' => {
                if b.get(*i..*i + 4) == Some(b"true") {
                    *i += 4;
                    Some(JsonVal::Bool(true))
                } else {
                    None
                }
            }
            b'f' => {
                if b.get(*i..*i + 5) == Some(b"false") {
                    *i += 5;
                    Some(JsonVal::Bool(false))
                } else {
                    None
                }
            }
            b'n' => {
                if b.get(*i..*i + 4) == Some(b"null") {
                    *i += 4;
                    Some(JsonVal::Null)
                } else {
                    None
                }
            }
            c if c == b'-' || c.is_ascii_digit() => Self::parse_num(b, i),
            _ => None,
        }
    }

    fn parse_str(b: &[u8], i: &mut usize) -> Option<String> {
        if b.get(*i) != Some(&b'"') {
            return None;
        }
        *i += 1;
        let start = *i;
        // Fast path: scan for closing quote without escapes
        let mut has_esc = false;
        while *i < b.len() {
            let c = b[*i];
            if c == b'"' {
                let end = *i;
                *i += 1;
                if !has_esc {
                    return std::str::from_utf8(&b[start..end]).ok().map(|s| s.to_string());
                }
                break;
            } else if c == b'\\' {
                has_esc = true;
                *i += 2;
            } else {
                *i += 1;
            }
        }
        if !has_esc {
            return None;
        }
        // Slow path with escape decoding
        let mut j = start;
        let mut out = String::new();
        while j < b.len() {
            let c = b[j];
            if c == b'"' {
                return Some(out);
            } else if c == b'\\' {
                j += 1;
                if j >= b.len() {
                    return None;
                }
                match b[j] {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        if j + 4 < b.len() {
                            if let Ok(hex) = std::str::from_utf8(&b[j + 1..j + 5]) {
                                if let Ok(code) = u32::from_str_radix(hex, 16) {
                                    if let Some(ch) = char::from_u32(code) {
                                        out.push(ch);
                                    }
                                }
                            }
                            j += 4;
                        }
                    }
                    other => out.push(other as char),
                }
                j += 1;
            } else {
                let ch_start = j;
                j += 1;
                while j < b.len() && (b[j] & 0b1100_0000) == 0b1000_0000 {
                    j += 1;
                }
                if let Ok(s) = std::str::from_utf8(&b[ch_start..j]) {
                    out.push_str(s);
                }
            }
        }
        None
    }

    fn parse_num(b: &[u8], i: &mut usize) -> Option<JsonVal> {
        let start = *i;
        while *i < b.len()
            && matches!(b[*i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        {
            *i += 1;
        }
        let s = std::str::from_utf8(&b[start..*i]).ok()?;
        s.parse::<f64>().ok().map(JsonVal::Num)
    }

    fn parse_arr(b: &[u8], i: &mut usize) -> Option<JsonVal> {
        *i += 1; // skip '['
        let mut arr = Vec::new();
        loop {
            Self::skip_ws(b, i);
            if *i >= b.len() {
                return None;
            }
            if b[*i] == b']' {
                *i += 1;
                return Some(JsonVal::Arr(arr));
            }
            let v = Self::parse_val(b, i)?;
            arr.push(v);
            Self::skip_ws(b, i);
            if *i < b.len() && b[*i] == b',' {
                *i += 1;
            }
        }
    }

    fn parse_obj(b: &[u8], i: &mut usize) -> Option<JsonVal> {
        *i += 1; // skip '{'
        let mut obj = Vec::new();
        loop {
            Self::skip_ws(b, i);
            if *i >= b.len() {
                return None;
            }
            if b[*i] == b'}' {
                *i += 1;
                return Some(JsonVal::Obj(obj));
            }
            let k = Self::parse_str(b, i)?;
            Self::skip_ws(b, i);
            if b.get(*i) != Some(&b':') {
                return None;
            }
            *i += 1;
            let v = Self::parse_val(b, i)?;
            obj.push((k, v));
            Self::skip_ws(b, i);
            if *i < b.len() && b[*i] == b',' {
                *i += 1;
            }
        }
    }

    fn get(&self, key: &str) -> Option<&JsonVal> {
        match self {
            JsonVal::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            JsonVal::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            JsonVal::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn as_u64(&self) -> Option<u64> {
        self.as_f64().map(|n| n.max(0.0) as u64)
    }

    fn as_usize(&self) -> Option<usize> {
        self.as_f64().map(|n| n.max(0.0) as usize)
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            JsonVal::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn as_arr(&self) -> Option<&[JsonVal]> {
        match self {
            JsonVal::Arr(a) => Some(a.as_slice()),
            _ => None,
        }
    }
}

// ───────────────────── Incremental Database Cache ─────────────────────

#[derive(Clone)]
struct ReplayParticipant {
    athlete_id: usize,
    position: u8, // 0=Top .. 4=Support
    champion: String,
    dealing: u64,
    tanking: u64,
    healing: u64,
    gold_lp: u64,
}

#[derive(Clone)]
struct CachedReplay {
    version: String,
    blue_team_id: u32,
    red_team_id: u32,
    blue_win: bool,
    blue_ban: Vec<String>,
    red_ban: Vec<String>,
    blue_team: Vec<ReplayParticipant>,
    red_team: Vec<ReplayParticipant>,
}

#[derive(Clone)]
struct CachedSolo {
    version: String,
    blue_win: bool,
    blue_team: Vec<ReplayParticipant>,
    red_team: Vec<ReplayParticipant>,
}

#[derive(Default)]
struct DbCache {
    replays: HashMap<usize, CachedReplay>,
    solos: HashMap<usize, CachedSolo>,
    teams: HashMap<usize, String>,
    team_by_name: HashMap<String, usize>,
    athlete_names: HashMap<usize, String>,
    athlete_by_name: HashMap<String, usize>,
    player_team_id: Option<usize>,
    last_replay_len: usize,
    last_solo_len: usize,
    last_team_len: usize,
}

static DB_CACHE: Mutex<Option<DbCache>> = Mutex::new(None);

fn pos_to_u8(s: &str) -> u8 {
    match s {
        "Top" | "top" | "0" => 0,
        "Jungle" | "jungle" | "1" => 1,
        "Mid" | "mid" | "2" => 2,
        "Bottom" | "bottom" | "Bot" | "bot" | "3" => 3,
        "Support" | "support" | "4" => 4,
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

fn parse_replay_json(raw: &str) -> Option<CachedReplay> {
    let v = JsonVal::parse(raw)?;
    let version = v.get("version")?.as_str()?.to_string();
    let blue_team_id = v.get("blue_team_id")?.as_u64()? as u32;
    let red_team_id = v.get("red_team_id")?.as_u64()? as u32;
    let blue_win = v.get("blue_team_win")?.as_bool()?;
    let parse_bans = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_arr())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let blue_ban = parse_bans("blue_ban");
    let red_ban = parse_bans("red_ban");

    let parse_side = |team_key: &str, perf_key: &str| -> Vec<ReplayParticipant> {
        let gold_lp: Vec<u64> = v
            .get(perf_key)
            .and_then(|p| p.get("gold_line_phase"))
            .and_then(|g| g.as_arr())
            .map(|arr| arr.iter().map(|x| x.as_u64().unwrap_or(0)).collect())
            .unwrap_or_default();
        let Some(arr) = v.get(team_key).and_then(|x| x.as_arr()) else {
            return Vec::new();
        };
        arr.iter()
            .enumerate()
            .filter_map(|(slot, p)| {
                let champion = p.get("champion")?.as_str()?.to_string();
                let athlete_id = p.get("athlete_id").and_then(|x| x.as_usize()).unwrap_or(0);
                let position = p
                    .get("position")
                    .and_then(|x| x.as_str())
                    .map(pos_to_u8)
                    .filter(|&l| l < 5)
                    .unwrap_or((slot as u8).min(4));
                let stats = p.get("statistics");
                let dealing = stats
                    .and_then(|s| s.get("dealing"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                let tanking = stats
                    .and_then(|s| s.get("tanking"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                let healing = stats
                    .and_then(|s| s.get("healing"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                let glp = gold_lp.get(slot).copied().unwrap_or(0);
                Some(ReplayParticipant {
                    athlete_id,
                    position,
                    champion,
                    dealing,
                    tanking,
                    healing,
                    gold_lp: glp,
                })
            })
            .collect()
    };

    Some(CachedReplay {
        version,
        blue_team_id,
        red_team_id,
        blue_win,
        blue_ban,
        red_ban,
        blue_team: parse_side("blue_team", "blue_performance"),
        red_team: parse_side("red_team", "red_performance"),
    })
}

fn parse_solo_json(raw: &str) -> Option<CachedSolo> {
    let v = JsonVal::parse(raw)?;
    let version = v.get("version")?.as_str()?.to_string();
    let blue_win = v.get("blue_team_win")?.as_bool()?;
    let parse_side = |team_key: &str| -> Vec<ReplayParticipant> {
        let Some(arr) = v.get(team_key).and_then(|x| x.as_arr()) else {
            return Vec::new();
        };
        arr.iter()
            .enumerate()
            .filter_map(|(slot, p)| {
                let champion = p.get("champion")?.as_str()?.to_string();
                let athlete_id = p.get("athlete_id").and_then(|x| x.as_usize()).unwrap_or(0);
                let dealing = p.get("dealing").and_then(|x| x.as_u64()).unwrap_or(0);
                let tanking = p.get("tanking").and_then(|x| x.as_u64()).unwrap_or(0);
                let healing = p.get("healing").and_then(|x| x.as_u64()).unwrap_or(0);
                Some(ReplayParticipant {
                    athlete_id,
                    position: (slot as u8).min(4),
                    champion,
                    dealing,
                    tanking,
                    healing,
                    gold_lp: 0,
                })
            })
            .collect()
    };
    Some(CachedSolo {
        version,
        blue_win,
        blue_team: parse_side("blue_team"),
        red_team: parse_side("red_team"),
    })
}

/// Extract authored patch notes from the player team's `news` JSON.
fn parse_authored_chg(news_json: &str, played_versions: &BTreeMap<Vec<u32>, String>) -> Option<ChgInfo> {
    let arr = JsonVal::parse(news_json)?;
    let items = arr.as_arr()?;
    let mut by_ver: BTreeMap<Vec<u32>, (String, HashMap<String, (u32, u32)>)> = BTreeMap::new();
    for item in items {
        let Some(pn) = item.get("ty").and_then(|t| t.get("PatchNote")) else {
            continue;
        };
        let Some(ver) = pn.get("version").and_then(|v| v.as_str()) else {
            continue;
        };
        let entry = by_ver
            .entry(vkey(ver))
            .or_insert_with(|| (ver.to_string(), HashMap::new()));
        if entry.1.is_empty() {
            if let Some(cpd_arr) = pn.get("champion_patch_data").and_then(|x| x.as_arr()) {
                for pair in cpd_arr {
                    let Some(pair_items) = pair.as_arr() else {
                        continue;
                    };
                    if pair_items.len() < 2 {
                        continue;
                    }
                    let Some(champ) = pair_items[0].as_str() else {
                        continue;
                    };
                    let Some(changes) = pair_items[1].as_arr() else {
                        continue;
                    };
                    if changes.is_empty() {
                        continue;
                    }
                    let mut bn = (0u32, 0u32);
                    for c in changes {
                        if c.get("is_buff").and_then(|b| b.as_bool()).unwrap_or(false) {
                            bn.0 += 1;
                        } else {
                            bn.1 += 1;
                        }
                    }
                    let e = entry.1.entry(champ.to_string()).or_insert((0, 0));
                    e.0 += bn.0;
                    e.1 += bn.1;
                }
            }
        }
    }
    if by_ver.is_empty() {
        return None;
    }
    let (lver, latest) = by_ver.iter().next_back().map(|(_, (v, m))| (v.clone(), m))?;
    let lk = vkey(&lver);
    if let Some((smax_k, _)) = played_versions.iter().next_back() {
        if *smax_k > lk {
            return None;
        }
    }
    let anchor = played_versions
        .iter()
        .filter(|(k, _)| **k < lk)
        .next_back()
        .map(|(_, v)| v.clone());
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
    let mut counts: Vec<(String, u32, u32)> = latest
        .iter()
        .map(|(champ, (b, n))| (champ.clone(), *b, *n))
        .collect();
    counts.sort();
    Some(ChgInfo { anchor, dirs, counts })
}

/// Syncs `DB_CACHE` with `StableClient` and rebuilds `TABLES` whenever match counts change.
fn sync_db_and_tables(ctx: &StableClient, force: bool) {
    let replay_ids = ctx.record_ids(RecordKindV1::MatchReplay);
    let solo_ids = ctx.record_ids(RecordKindV1::SoloRankMatch);
    let team_ids = ctx.record_ids(RecordKindV1::Team);
    if team_ids.is_empty() {
        return;
    }

    let mut guard = match DB_CACHE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let cache = guard.get_or_insert_with(DbCache::default);

    // Detect save switch (counts shrank)
    if replay_ids.len() < cache.last_replay_len
        || solo_ids.len() < cache.last_solo_len
        || team_ids.len() != cache.last_team_len
    {
        *cache = DbCache::default();
    }

    let unchanged = !force
        && replay_ids.len() == cache.last_replay_len
        && solo_ids.len() == cache.last_solo_len
        && !cache.teams.is_empty()
        && TABLES.lock().map(|t| t.is_some()).unwrap_or(false);
    if unchanged {
        return;
    }

    // 1. Teams & player_team_id discovery (via non-empty `news`)
    if cache.teams.is_empty() || cache.player_team_id.is_none() {
        let mut best_news_team: Option<(usize, usize)> = None;
        for &tid in &team_ids {
            if !cache.teams.contains_key(&tid) {
                if let Some(raw) = ctx.record_get_json(RecordKindV1::Team, tid, "") {
                    if let Some(v) = JsonVal::parse(&raw) {
                        if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
                            cache.teams.insert(tid, name.to_string());
                            cache.team_by_name.insert(name.to_string(), tid);
                        }
                    }
                }
            }
            if let Some(news_raw) = ctx.record_get_json(RecordKindV1::Team, tid, "news") {
                let len = news_raw.trim().len();
                if len > 4 && best_news_team.map_or(true, |(_, bl)| len > bl) {
                    best_news_team = Some((tid, len));
                }
            }
        }
        if let Some((ptid, _)) = best_news_team {
            cache.player_team_id = Some(ptid);
        }
        cache.last_team_len = team_ids.len();
    }

    // 2. Athletes (name <-> id mapping)
    if cache.athlete_names.is_empty() {
        for aid in ctx.record_ids(RecordKindV1::Athlete) {
            if let Some(raw) = ctx.record_get_json(RecordKindV1::Athlete, aid, "") {
                if let Some(v) = JsonVal::parse(&raw) {
                    if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
                        cache.athlete_names.insert(aid, name.to_string());
                        cache.athlete_by_name.insert(name.to_string(), aid);
                    }
                }
            }
        }
    }

    // 3. Incrementally parse new MatchReplays & SoloRankMatches
    for &rid in &replay_ids {
        if !cache.replays.contains_key(&rid) {
            if let Some(raw) = ctx.record_get_json(RecordKindV1::MatchReplay, rid, "") {
                if let Some(parsed) = parse_replay_json(&raw) {
                    cache.replays.insert(rid, parsed);
                }
            }
        }
    }
    for &sid in &solo_ids {
        if !cache.solos.contains_key(&sid) {
            if let Some(raw) = ctx.record_get_json(RecordKindV1::SoloRankMatch, sid, "") {
                if let Some(parsed) = parse_solo_json(&raw) {
                    cache.solos.insert(sid, parsed);
                }
            }
        }
    }
    cache.last_replay_len = replay_ids.len();
    cache.last_solo_len = solo_ids.len();

    // 4. Build patch_tsv and replay_rows from cached replays + solos
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
    let mut played_versions: BTreeMap<Vec<u32>, String> = BTreeMap::new();

    let mut mu: BTreeMap<(String, String), (u32, u32)> = BTreeMap::new();
    let mut opp: BTreeMap<(u32, String), (u32, u32)> = BTreeMap::new();
    let mut teamn: BTreeMap<u32, u32> = BTreeMap::new();

    for rep in cache.replays.values() {
        played_versions
            .entry(vkey(&rep.version))
            .or_insert_with(|| rep.version.clone());
        *ver_matches.entry(rep.version.clone()).or_insert(0) += 1;
        for b in rep.blue_ban.iter().chain(rep.red_ban.iter()) {
            *ver_bans.entry((rep.version.clone(), b.clone())).or_insert(0) += 1;
        }
        let mut blue_lane: [Option<&str>; 5] = [None; 5];
        let mut red_lane: [Option<&str>; 5] = [None; 5];
        for p in &rep.blue_team {
            if p.position < 5 {
                blue_lane[p.position as usize] = Some(p.champion.as_str());
                let e = pos_stats
                    .entry((rep.version.clone(), p.champion.clone(), p.position))
                    .or_default();
                e.matches += 1;
                if rep.blue_win {
                    e.wins += 1;
                }
                e.dealing += p.dealing;
                e.tanking += p.tanking;
                e.healing += p.healing;
                e.gold_lp += p.gold_lp;
            }
        }
        for p in &rep.red_team {
            if p.position < 5 {
                red_lane[p.position as usize] = Some(p.champion.as_str());
                let e = pos_stats
                    .entry((rep.version.clone(), p.champion.clone(), p.position))
                    .or_default();
                e.matches += 1;
                if !rep.blue_win {
                    e.wins += 1;
                }
                e.dealing += p.dealing;
                e.tanking += p.tanking;
                e.healing += p.healing;
                e.gold_lp += p.gold_lp;
            }
        }
        for l in 0..5 {
            if let (Some(b), Some(r)) = (blue_lane[l], red_lane[l]) {
                let e1 = mu.entry((b.to_string(), r.to_string())).or_insert((0, 0));
                e1.1 += 1;
                if rep.blue_win {
                    e1.0 += 1;
                }
                let e2 = mu.entry((r.to_string(), b.to_string())).or_insert((0, 0));
                e2.1 += 1;
                if !rep.blue_win {
                    e2.0 += 1;
                }
            }
        }
        for (tid, side, won) in [
            (rep.blue_team_id, &rep.blue_team, rep.blue_win),
            (rep.red_team_id, &rep.red_team, !rep.blue_win),
        ] {
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

    // Include SoloRank matches when replay volume is low (< 200 matches)
    if cache.replays.len() < 200 {
        for solo in cache.solos.values() {
            played_versions
                .entry(vkey(&solo.version))
                .or_insert_with(|| solo.version.clone());
            *ver_matches.entry(solo.version.clone()).or_insert(0) += 1;
            for (side, won) in [(&solo.blue_team, solo.blue_win), (&solo.red_team, !solo.blue_win)] {
                for p in side {
                    if p.position < 5 {
                        let e = pos_stats
                            .entry((solo.version.clone(), p.champion.clone(), p.position))
                            .or_default();
                        e.matches += 1;
                        if won {
                            e.wins += 1;
                        }
                        e.dealing += p.dealing;
                        e.tanking += p.tanking;
                        e.healing += p.healing;
                    }
                }
            }
        }
    }

    let mut pt = String::with_capacity(64 * 1024);
    pt.push_str("version\ttotal_match\tchampion\tposition\tbans\twins\tmatches\tdealing\ttanking\thealing\tkills\tdeaths\tcs\tgold\tdealing_line_phase\ttanking_line_phase\thealing_line_phase\tgold_line_phase\tcs_line_phase\n");
    for ((ver, champ, pos), st) in &pos_stats {
        let tot = ver_matches.get(ver).copied().unwrap_or(0);
        let bans = ver_bans.get(&(ver.clone(), champ.clone())).copied().unwrap_or(0);
        pt.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t0\t0\t0\t0\t0\t0\t0\t{}\t0\n",
            ver,
            tot,
            champ,
            pos_to_name(*pos),
            bans,
            st.wins,
            st.matches,
            st.dealing,
            st.tanking,
            st.healing,
            st.gold_lp,
        ));
    }

    let mut rrows = String::with_capacity(64 * 1024);
    for ((a, b), (w, g)) in &mu {
        rrows.push_str(&format!("MU\t{}\t{}\t{}\t{}\n", a, b, w, g));
    }
    for ((t, c), (g, w)) in &opp {
        rrows.push_str(&format!("OPP\t{}\t{}\t{}\t{}\n", t, c, g, w));
    }
    for (t, g) in &teamn {
        rrows.push_str(&format!("OPPN\t{}\t{}\n", t, g));
    }

    // 5. Authored PatchNote changes from player team's `news`
    let chg_info = cache
        .player_team_id
        .and_then(|ptid| ctx.record_get_json(RecordKindV1::Team, ptid, "news"))
        .and_then(|news_raw| parse_authored_chg(&news_raw, &played_versions))
        .unwrap_or_default();

    if let Ok(mut la) = LAST_ANCHOR.lock() {
        if *la != chg_info.anchor {
            if la.is_some() {
                if let Ok(mut p) = AI_PRIOR.lock() {
                    *p = None;
                }
            }
            *la = chg_info.anchor.clone();
        }
    }

    let pool = ctx.champion_names();
    if let Ok(mut order) = CHAMP_ORDER.lock() {
        if !pool.is_empty() {
            *order = pool.clone();
        }
    }

    if let Ok(mut tg) = TABLES.lock() {
        *tg = Some(recommender::build_tables(&pt, &pool, &chg_info, &rrows));
        TABLES_GEN.fetch_add(1, Ordering::Relaxed);
    }

    // Export diagnostic TSVs to %LOCALAPPDATA%/tfm2-overlay/probe
    let b = base();
    let out_dir = format!("{}/probe", b);
    let _ = std::fs::create_dir_all(&out_dir);
    atomic_write(&format!("{}/champion_patch_statistics.tsv", out_dir), &pt);
}

/// Compute athlete familiarity (`af`) and enemy `pocket` from cached replays for the 5 starters.
fn compute_athlete_tables(
    cache: &DbCache,
    my_starters: &[Option<usize>; 5],
    enemy_starters: &[Option<usize>; 5],
) -> (HashMap<(String, u8), f32>, HashMap<String, f32>) {
    let mut hist: HashMap<usize, HashMap<String, (f64, f64)>> = HashMap::new();
    for rep in cache.replays.values() {
        for (side, won) in [(&rep.blue_team, rep.blue_win), (&rep.red_team, !rep.blue_win)] {
            for p in side {
                let e = hist
                    .entry(p.athlete_id)
                    .or_default()
                    .entry(p.champion.clone())
                    .or_insert((0.0, 0.0));
                e.0 += 1.0;
                if won {
                    e.1 += 1.0;
                }
            }
        }
    }
    let mut af: HashMap<(String, u8), f32> = HashMap::new();
    for (lane, aid_opt) in my_starters.iter().enumerate() {
        let Some(aid) = aid_opt else { continue };
        let Some(ch) = hist.get(aid) else { continue };
        let (tg, tw) = ch.values().fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
        let wr_a = (tw + 4.0) / (tg + 8.0);
        for (c, (g, w)) in ch {
            let famc = 0.15 * ((w + 4.0 * wr_a) / (g + 4.0) - wr_a);
            af.insert((c.clone(), lane as u8), famc.min(0.05) as f32);
        }
    }
    let mut pocket: HashMap<String, f32> = HashMap::new();
    for aid_opt in enemy_starters.iter().flatten() {
        if let Some(ch) = hist.get(aid_opt) {
            for (c, (g, _)) in ch {
                let v = (0.015 * (1.0 + g.min(20.0)).ln()) as f32;
                let e = pocket.entry(c.clone()).or_insert(0.0);
                if v > *e {
                    *e = v;
                }
            }
        }
    }
    (af, pocket)
}

// ───────────────────── Live Ban/Pick Reader (StableClient UI) ─────────────────────

#[derive(Default)]
struct BanTracker {
    match_key: String,
    first_blue: Option<bool>,
    last_blue_turn: Option<bool>,
    blue_bans: Vec<String>,
    red_bans: Vec<String>,
}

static BAN_TRACKER: Mutex<Option<BanTracker>> = Mutex::new(None);
static BP_PREKEY: Mutex<String> = Mutex::new(String::new());
static UI_FOLDED: AtomicBool = AtomicBool::new(false);

fn is_banpick_scene(ctx: &StableClient) -> bool {
    ctx.client_scene_kind() == Some(ClientSceneKindV1::Match)
        && ctx.ui_exists("main.blue_picks")
        && ctx.ui_exists("main.champions.contents")
}

fn read_team_name(ctx: &StableClient, is_blue: bool) -> String {
    let p1 = if is_blue {
        "main.bottom.blue_side.name"
    } else {
        "main.bottom.red_side.name"
    };
    if let Some(t) = ctx.ui_text(p1) {
        let t = t.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let p2 = if is_blue {
        "main.blue_picks.pick_slot_0.popup.header.team_name"
    } else {
        "main.red_picks.pick_slot_0.popup.header.team_name"
    };
    ctx.ui_text(p2).unwrap_or_default().trim().to_string()
}

fn read_starter_name(ctx: &StableClient, side: &str, slot: usize) -> String {
    for state in ["done", "in_turn", "wait"] {
        let p = format!("main.{}_picks.pick_slot_{}.{}.name", side, slot, state);
        if ctx.ui_visible(&format!("main.{}_picks.pick_slot_{}.{}", side, slot, state)) == Some(true) {
            if let Some(name) = ctx.ui_text(&p) {
                let n = name.trim();
                if !n.is_empty() {
                    return n.to_string();
                }
            }
        }
    }
    for state in ["done", "in_turn", "wait"] {
        let p = format!("main.{}_picks.pick_slot_{}.{}.name", side, slot, state);
        if let Some(name) = ctx.ui_text(&p) {
            let n = name.trim();
            if !n.is_empty() {
                return n.to_string();
            }
        }
    }
    String::new()
}

fn read_ban_count_per_side(ctx: &StableClient) -> usize {
    let mut cnt = 0;
    for i in 0..5 {
        let v1 = ctx.ui_visible(&format!("main.bottom.blue_side.bans.ban_slot_{}", i)) == Some(true);
        let v2 = ctx.ui_visible(&format!("main.blue_bans.ban_slot_{}", i)) == Some(true);
        if v1 || v2 {
            cnt += 1;
        }
    }
    cnt
}

fn update_live_banpick(ctx: &StableClient) {
    if !is_banpick_scene(ctx) {
        if let Ok(mut og) = OUT_NOW.lock() {
            *og = None;
        }
        return;
    }

    // Ensure DB and tables are synced when entering Ban/Pick
    sync_db_and_tables(ctx, false);

    let blue_name = read_team_name(ctx, true);
    let red_name = read_team_name(ctx, false);

    let (blue_tid, red_tid, player_tid, blue_starters, red_starters) = {
        let Ok(guard) = DB_CACHE.lock() else { return };
        let Some(cache) = guard.as_ref() else { return };
        let b_id = cache.team_by_name.get(&blue_name).copied();
        let r_id = cache.team_by_name.get(&red_name).copied();
        let mut bs = [None; 5];
        let mut rs = [None; 5];
        for i in 0..5 {
            let bn = read_starter_name(ctx, "blue", i);
            let rn = read_starter_name(ctx, "red", i);
            bs[i] = cache.athlete_by_name.get(&bn).copied();
            rs[i] = cache.athlete_by_name.get(&rn).copied();
        }
        (b_id, r_id, cache.player_team_id, bs, rs)
    };

    // Determine which side is the human player
    let mine_is_blue = match (player_tid, blue_tid, red_tid) {
        (Some(p), Some(b), _) if p == b => true,
        (Some(p), _, Some(r)) if p == r => false,
        _ => true, // default to Blue if unknown
    };

    let ban_count = read_ban_count_per_side(ctx);
    let blue_arrow = ctx.ui_visible("main.header.desc.blue_arrow") == Some(true);
    let red_arrow = ctx.ui_visible("main.header.desc.red_arrow") == Some(true);
    let cur_blue_turn = if blue_arrow {
        Some(true)
    } else if red_arrow {
        Some(false)
    } else {
        None
    };

    // Scan champion tiles on the grid
    let raw_tiles = ctx.ui_child_names("main.champions.contents");
    let tile_ids: Vec<String> = if !raw_tiles.is_empty() {
        raw_tiles
    } else {
        recommender::engine::CHAMPS
            .iter()
            .map(|(id, _)| id.to_string())
            .collect()
    };

    let mut grid_bans: Vec<String> = Vec::new();
    let mut blue_picks_ord: Vec<(u8, String)> = Vec::new();
    let mut red_picks_ord: Vec<(u8, String)> = Vec::new();
    let mut fl_my: Vec<String> = Vec::new();
    let mut fl_enemy: Vec<String> = Vec::new();
    let mut locked_x: Vec<String> = Vec::new();
    let mut locked_i: Vec<String> = Vec::new();
    let mut attributed = false;

    const OWN_T: [&str; 6] = [
        "fearless_tooltip_own", "우리 팀", "Your team", "自チーム", "你的队伍", "你的隊伍",
    ];
    const BOTH_T: [&str; 6] = [
        "fearless_tooltip_both", "양팀", "Neither team", "両チーム", "双方", "雙方",
    ];
    const COLOR_T: [&str; 12] = [
        "fearless_tooltip_blue", "fearless_tooltip_red", "블루팀", "레드팀", "Blue team",
        "Red team", "ブルーチーム", "レッドチーム", "蓝方", "红方", "藍方", "紅方",
    ];

    for id in &tile_ids {
        // Ignore our own spawned child nodes if any appear at contents level
        if id.starts_with("ai_") {
            continue;
        }
        let base = format!("main.champions.contents.{}", id);
        if ctx.ui_visible(&format!("{}.ban", base)) == Some(true) {
            grid_bans.push(id.clone());
        }
        if ctx.ui_visible(&format!("{}.blue", base)) == Some(true) {
            let ord = ctx
                .ui_text(&format!("{}.blue.text", base))
                .and_then(|t| t.trim().parse::<u8>().ok())
                .unwrap_or(99);
            blue_picks_ord.push((ord, id.clone()));
        }
        if ctx.ui_visible(&format!("{}.red", base)) == Some(true) {
            let ord = ctx
                .ui_text(&format!("{}.red.text", base))
                .and_then(|t| t.trim().parse::<u8>().ok())
                .unwrap_or(99);
            red_picks_ord.push((ord, id.clone()));
        }
        let icon_vis = ctx.ui_visible(&format!("{}.fearless_icon", base)) == Some(true);
        let x_vis = ctx.ui_visible(&format!("{}.fearless_x", base)) == Some(true);
        if x_vis {
            locked_x.push(id.clone());
        }
        if icon_vis {
            locked_i.push(id.clone());
        }
        if icon_vis || x_vis {
            let tip_txt = ctx
                .ui_text(&format!("{}.fearless_tooltip.text", base))
                .unwrap_or_default();
            let own_t = OWN_T.iter().any(|p| tip_txt.contains(p));
            let color_t = COLOR_T.iter().any(|p| tip_txt.contains(p));
            let both_t = BOTH_T.iter().any(|p| tip_txt.contains(p));
            if own_t && !both_t {
                fl_my.push(id.clone());
                attributed = true;
            } else if color_t && !both_t {
                fl_enemy.push(id.clone());
                attributed = true;
            } else {
                fl_my.push(id.clone());
                fl_enemy.push(id.clone());
                attributed |= both_t;
            }
            if x_vis && !fl_my.contains(id) {
                fl_my.push(id.clone());
            }
        }
    }

    blue_picks_ord.sort_by_key(|(o, _)| *o);
    red_picks_ord.sort_by_key(|(o, _)| *o);
    let blue_picks: Vec<String> = blue_picks_ord.into_iter().map(|(_, c)| c).collect();
    let red_picks: Vec<String> = red_picks_ord.into_iter().map(|(_, c)| c).collect();

    // Update BanTracker to attribute grid_bans to blue_bans vs red_bans and track first-pick side
    let match_key = format!("{}|{}", blue_name, red_name);
    let (blue_bans, red_bans, first_blue) = {
        let Ok(mut bg) = BAN_TRACKER.lock() else { return };
        let bt = bg.get_or_insert_with(BanTracker::default);
        if bt.match_key != match_key || (grid_bans.is_empty() && blue_picks.is_empty() && red_picks.is_empty()) {
            if bt.match_key != match_key || (!bt.blue_bans.is_empty() || !bt.red_bans.is_empty()) {
                bt.match_key = match_key.clone();
                bt.first_blue = None;
                bt.last_blue_turn = None;
                bt.blue_bans.clear();
                bt.red_bans.clear();
            }
        }
        if bt.first_blue.is_none() {
            if grid_bans.is_empty() && blue_picks.is_empty() && red_picks.is_empty() {
                bt.first_blue = cur_blue_turn.or(Some(true));
            } else if blue_picks.len() + red_picks.len() == 1 {
                bt.first_blue = Some(blue_picks.len() == 1);
            } else {
                bt.first_blue = Some(true);
            }
        }
        // Attribute any newly appeared bans in grid_bans
        for c in &grid_bans {
            if !bt.blue_bans.contains(c) && !bt.red_bans.contains(c) {
                let total_so_far = bt.blue_bans.len() + bt.red_bans.len();
                let f_blue = bt.first_blue.unwrap_or(true);
                // Alternating ban turns starting from first_blue (fallback to last_blue_turn)
                let was_blue = bt
                    .last_blue_turn
                    .unwrap_or_else(|| if total_so_far % 2 == 0 { f_blue } else { !f_blue });
                if was_blue && bt.blue_bans.len() < ban_count.max(5) {
                    bt.blue_bans.push(c.clone());
                } else {
                    bt.red_bans.push(c.clone());
                }
            }
        }
        if let Some(t) = cur_blue_turn {
            bt.last_blue_turn = Some(t);
        }
        (bt.blue_bans.clone(), bt.red_bans.clone(), bt.first_blue.unwrap_or(true))
    };

    if fl_my.is_empty() && fl_enemy.is_empty() && !(locked_x.is_empty() && locked_i.is_empty()) {
        let mut u = locked_x.clone();
        for c in &locked_i {
            if !u.contains(c) {
                u.push(c.clone());
            }
        }
        fl_my = u.clone();
        fl_enemy = u;
    }

    let tgen = TABLES_GEN.load(Ordering::Relaxed);
    let prekey = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        match_key,
        mine_is_blue,
        ban_count,
        blue_bans.join(","),
        red_bans.join(","),
        blue_picks.join(","),
        red_picks.join(","),
        fl_my.join(","),
        fl_enemy.join(","),
        first_blue,
        tgen
    );
    let out_ready = OUT_NOW.lock().map(|o| o.is_some()).unwrap_or(false);
    if let Ok(mut pk) = BP_PREKEY.lock() {
        if *pk == prekey && out_ready {
            return;
        }
        *pk = prekey;
    }

    let (my_ban, enemy_ban, my_pick, enemy_pick, my_starters, enemy_starters, enemy_tid) =
        if mine_is_blue {
            (
                blue_bans,
                red_bans,
                blue_picks,
                red_picks,
                blue_starters,
                red_starters,
                red_tid.unwrap_or(0) as u32,
            )
        } else {
            (
                red_bans,
                blue_bans,
                red_picks,
                blue_picks,
                red_starters,
                blue_starters,
                blue_tid.unwrap_or(0) as u32,
            )
        };

    let teams_key = match (blue_tid, red_tid) {
        (Some(b), Some(r)) => format!("{}|{}", b.min(r), b.max(r)),
        _ => match_key,
    };

    let board = Board {
        my_ban,
        enemy_ban,
        my_pick,
        enemy_pick,
        ban_count,
        teams: teams_key,
        my_locked: fl_my,
        enemy_locked: fl_enemy,
        attributed,
        enemy_team: enemy_tid,
        i_am_team1: Some(mine_is_blue == first_blue),
        enemy_intent: Vec::new(),
    };

    let (af, pocket) = {
        let Ok(guard) = DB_CACHE.lock() else { return };
        match guard.as_ref() {
            Some(cache) => compute_athlete_tables(cache, &my_starters, &enemy_starters),
            None => (HashMap::new(), HashMap::new()),
        }
    };

    let mut prior: HashMap<String, f32> = HashMap::new();
    if let (Ok(p), Ok(order)) = (AI_PRIOR.lock(), CHAMP_ORDER.lock()) {
        for (cand, (sum, n)) in p.as_ref().map(|m| m.iter()).into_iter().flatten() {
            if *n >= 1 {
                if let Some(id) = order.get(*cand) {
                    prior.insert(id.clone(), (*sum / *n as f64) as f32);
                }
            }
        }
    }

    if let Ok(mut tg) = TABLES.lock() {
        let tables =
            tg.get_or_insert_with(|| recommender::build_tables("", &[], &ChgInfo::default(), ""));
        tables.set_athlete(af, pocket);
        tables.set_ai_prior(prior);
        let out = recommender::update(&board, tables, tgen);
        if let Ok(mut og) = OUT_NOW.lock() {
            *og = Some(out);
        }
    }
}

// ───────────────────── In-Game UI Stamping (StableClient) ─────────────────────

const RANK_GLYPH: [&str; 5] = ["①", "②", "③", "④", "⑤"];
static HDR_ORIG: Mutex<Option<String>> = Mutex::new(None);
static STAMPED: AtomicBool = AtomicBool::new(false);

fn ensure_tile_nodes(ctx: &mut StableClient, champ_id: &str) {
    let base = format!("main.champions.contents.{}", champ_id);
    let border_path = format!("{}.ai_border", base);
    if !ctx.ui_exists(&border_path) {
        let _ = ctx.ui_spawn_source(
            &base,
            r#"
            ai_border:color {
                width: 100%;
                height: 100%;
                color: #00000000;
                back_color: #38d6b3ff;
                stroke: 2;
                visible: false;
            }
            "#,
        );
    }
    let badge_path = format!("{}.ai_badge", base);
    if !ctx.ui_exists(&badge_path) {
        let _ = ctx.ui_spawn_source(
            &base,
            r#"
            ai_badge:color {
                x: 2;
                y: 2;
                width: 34px;
                height: 18px;
                color: #0b111cd8;
                back_color: #38d6b3ff;
                stroke: 1;
                radius: 3px;
                visible: false;
                txt:label {
                    width: 100%;
                    height: 100%;
                    style: @"asset/base/style/main#label";
                    font_size: 11;
                    text_align: center;
                    vertical_align: middle;
                    color: #ffffff;
                }
            }
            "#,
        );
    }
}

fn ensure_ai_panel(ctx: &mut StableClient) {
    if ctx.ui_exists("main.ai_rec") {
        return;
    }
    let _ = ctx.ui_spawn_source(
        "main",
        r#"
        ai_rec:color {
            x: 16;
            y: 48;
            z: 18;
            width: 346px;
            height: 294px;
            color: #0d131fe8;
            back_color: #38d6b3aa;
            stroke: 1;
            radius: 6px;
            hdr:color {
                x: 0;
                y: 0;
                width: 100%;
                height: 28px;
                color: #162236ff;
                radius: 6px;
                title:label {
                    x: 10;
                    y: 0;
                    width: 326px;
                    height: 28px;
                    style: @"asset/base/style/main#label";
                    font_size: 13;
                    text_align: left;
                    vertical_align: middle;
                    color: #ffd959ff;
                }
            }
            body:empty {
                x: 0;
                y: 30;
                width: 100%;
                height: 262px;
                row_0:color {
                    x: 8; y: 2; width: 330px; height: 30px; color: #18263acc; radius: 4px;
                    icon:image { x: 4; y: 2; width: 26px; height: 26px; }
                    txt:label {
                        x: 36; y: 0; width: 290px; height: 30px;
                        style: @"asset/base/style/main#label";
                        font_size: 13; text_align: left; vertical_align: middle; color: #ffffff;
                    }
                }
                row_1:color {
                    x: 8; y: 35; width: 330px; height: 30px; color: #141f30cc; radius: 4px;
                    icon:image { x: 4; y: 2; width: 26px; height: 26px; }
                    txt:label {
                        x: 36; y: 0; width: 290px; height: 30px;
                        style: @"asset/base/style/main#label";
                        font_size: 12; text_align: left; vertical_align: middle; color: #e6edf7ff;
                    }
                }
                row_2:color {
                    x: 8; y: 68; width: 330px; height: 30px; color: #141f30cc; radius: 4px;
                    icon:image { x: 4; y: 2; width: 26px; height: 26px; }
                    txt:label {
                        x: 36; y: 0; width: 290px; height: 30px;
                        style: @"asset/base/style/main#label";
                        font_size: 12; text_align: left; vertical_align: middle; color: #e6edf7ff;
                    }
                }
                row_3:color {
                    x: 8; y: 101; width: 330px; height: 30px; color: #141f30cc; radius: 4px;
                    icon:image { x: 4; y: 2; width: 26px; height: 26px; }
                    txt:label {
                        x: 36; y: 0; width: 290px; height: 30px;
                        style: @"asset/base/style/main#label";
                        font_size: 12; text_align: left; vertical_align: middle; color: #d0dae8ff;
                    }
                }
                row_4:color {
                    x: 8; y: 134; width: 330px; height: 30px; color: #141f30cc; radius: 4px;
                    icon:image { x: 4; y: 2; width: 26px; height: 26px; }
                    txt:label {
                        x: 36; y: 0; width: 290px; height: 30px;
                        style: @"asset/base/style/main#label";
                        font_size: 12; text_align: left; vertical_align: middle; color: #d0dae8ff;
                    }
                }
                say:label {
                    x: 10;
                    y: 168;
                    width: 326px;
                    height: 90px;
                    style: @"asset/base/style/main#label";
                    font_size: 11;
                    text_align: left;
                    vertical_align: top;
                    color: #b8c7dcff;
                }
            }
        }
        "#,
    );
}

fn stamp_ui(ctx: &mut StableClient) {
    if !is_banpick_scene(ctx) {
        if STAMPED.swap(false, Ordering::Relaxed) {
            if let Ok(mut g) = HDR_ORIG.lock() {
                *g = None;
            }
        }
        return;
    }
    STAMPED.store(true, Ordering::Relaxed);

    if ctx.key_pressed("Tab") {
        let prev = UI_FOLDED.load(Ordering::Relaxed);
        UI_FOLDED.store(!prev, Ordering::Relaxed);
    }
    let folded = UI_FOLDED.load(Ordering::Relaxed);

    let o: Arc<Output> = OUT_NOW
        .lock()
        .ok()
        .and_then(|g| g.as_ref().cloned())
        .unwrap_or_default();
    let (rec_ban, recs, detail_rows, swap, say, tiers) = (
        o.rec_ban,
        &o.recs,
        &o.detail_rows,
        &o.swap,
        o.say.as_str(),
        &o.tiers,
    );

    // 1. Header quick line (and Swap modal banner when swap is active)
    if let Ok(mut orig) = HDR_ORIG.lock() {
        let line = if !swap.is_empty() {
            Some(format!("추천 배치 ▶  {}", swap.join("  ·  ")))
        } else if !recs.is_empty() {
            Some(format!(
                "추천 {} ▶ {}",
                if rec_ban { "밴" } else { "픽" },
                recs.iter()
                    .take(3)
                    .enumerate()
                    .map(|(i, (_, ko, sc))| format!("{}{}{}", RANK_GLYPH[i.min(4)], ko, sc))
                    .collect::<Vec<_>>()
                    .join("  ")
            ))
        } else {
            None
        };
        let ours = |t: &str| t.starts_with("추천 ") && t.contains('▶');
        let cur = ctx.ui_text("main.header.desc").unwrap_or_default();
        match line {
            Some(ref l) => {
                if !ours(&cur) {
                    *orig = Some(cur.clone());
                }
                if cur != *l {
                    let _ = ctx.ui_set_text("main.header.desc", l);
                }
            }
            None => {
                if ours(&cur) {
                    if let Some(o_txt) = orig.as_ref() {
                        let _ = ctx.ui_set_text("main.header.desc", o_txt);
                    }
                }
            }
        }
    }

    // Also stamp Swap banner directly inside `main.swap` if swap screen is visible
    if ctx.ui_visible("main.swap") == Some(true) && !swap.is_empty() {
        if !ctx.ui_exists("main.swap.ai_swap") {
            let _ = ctx.ui_spawn_source(
                "main.swap",
                r#"
                ai_swap:color {
                    x: 460;
                    y: 24;
                    z: 30;
                    width: 1000px;
                    height: 36px;
                    color: #0d131fe8;
                    back_color: #38d6b3ff;
                    stroke: 2;
                    radius: 6px;
                    txt:label {
                        width: 100%;
                        height: 100%;
                        style: @"asset/base/style/main#label";
                        font_size: 15;
                        text_align: center;
                        vertical_align: middle;
                        color: #ffd959ff;
                    }
                }
                "#,
            );
        }
        let _ = ctx.ui_set_visible("main.swap.ai_swap", true);
        let _ = ctx.ui_set_text(
            "main.swap.ai_swap.txt",
            &format!("⇄ AI 최적 스왑 배치 ▶   {}", swap.join("   ·   ")),
        );
    } else if ctx.ui_exists("main.swap.ai_swap") {
        let _ = ctx.ui_set_visible("main.swap.ai_swap", false);
    }

    // 2. Collapsible AI Coach Card (`main.ai_rec`)
    ensure_ai_panel(ctx);
    let has_content = !detail_rows.is_empty() || !swap.is_empty() || !say.is_empty();
    let _ = ctx.ui_set_visible("main.ai_rec", has_content);
    if has_content {
        let hdr_title = if !swap.is_empty() {
            format!(
                "⇄ AI 스왑 최적 배치  {}",
                if folded { "[Tab 펼치기]" } else { "[Tab 접기]" }
            )
        } else {
            format!(
                "AI 코치 {} 추천  {}",
                if rec_ban { "밴(BAN)" } else { "픽(PICK)" },
                if folded { "[Tab 펼치기]" } else { "[Tab 접기]" }
            )
        };
        let _ = ctx.ui_set_text("main.ai_rec.hdr.title", &hdr_title);
        let _ = ctx.ui_set_visible("main.ai_rec.body", !folded);
        let _ = ctx.ui_set_properties(
            "main.ai_rec",
            &format!(
                "height: {}px; back_color: {};",
                if folded { 28 } else { 294 },
                if rec_ban { "#c766f2aa" } else { "#38d6b3aa" }
            ),
        );
        if !folded {
            for i in 0..5 {
                let row_path = format!("main.ai_rec.body.row_{}", i);
                if let Some(r) = detail_rows.get(i) {
                    let _ = ctx.ui_set_visible(&row_path, true);
                    let _ = ctx.ui_set_champion_icon(
                        &format!("{}.icon", row_path),
                        &r.id,
                        26.0,
                        26.0,
                        1.0,
                    );
                    let badge = if r.tier.is_empty() && r.chg.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}{}]", r.tier, r.chg)
                    };
                    let rsn = if r.reason.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", r.reason)
                    };
                    let line = format!(
                        "{} [{}] {}{}  ({}{})",
                        RANK_GLYPH[i], r.lane, r.ko, badge, r.score, rsn
                    );
                    let _ = ctx.ui_set_text(&format!("{}.txt", row_path), &line);
                } else {
                    let _ = ctx.ui_set_visible(&row_path, false);
                }
            }
            let _ = ctx.ui_set_text("main.ai_rec.body.say", say);
        }
    }

    // 3. Per-tile tier badges & recommendation borders on `main.champions.contents.<champ>`
    let raw_tiles = ctx.ui_child_names("main.champions.contents");
    let tile_ids: Vec<String> = if !raw_tiles.is_empty() {
        raw_tiles
    } else {
        recommender::engine::CHAMPS
            .iter()
            .map(|(id, _)| id.to_string())
            .collect()
    };

    for champ in tile_ids {
        if champ.starts_with("ai_") {
            continue;
        }
        ensure_tile_nodes(ctx, &champ);
        let base = format!("main.champions.contents.{}", champ);
        let taken = ctx.ui_visible(&format!("{}.ban", base)) == Some(true)
            || ctx.ui_visible(&format!("{}.blue", base)) == Some(true)
            || ctx.ui_visible(&format!("{}.red", base)) == Some(true)
            || ctx.ui_visible(&format!("{}.fearless_x", base)) == Some(true);

        let border_path = format!("{}.ai_border", base);
        let badge_path = format!("{}.ai_badge", base);
        if taken {
            let _ = ctx.ui_set_visible(&border_path, false);
            let _ = ctx.ui_set_visible(&badge_path, false);
            continue;
        }

        let rank = recs.iter().position(|(id, _, _)| *id == champ);
        let tier_txt = tiers.get(&champ).map(|t| t.as_str()).unwrap_or("");

        match rank {
            Some(rk) => {
                let bcol = if rec_ban { "#c766f2ff" } else { "#38d6b3ff" };
                let stroke = if rk == 0 { 3 } else { 2 };
                let _ = ctx.ui_set_visible(&border_path, true);
                let _ = ctx.ui_set_properties(
                    &border_path,
                    &format!("back_color: {}; stroke: {};", bcol, stroke),
                );
                let txt = format!("{}{}", RANK_GLYPH[rk.min(4)], tier_txt);
                let _ = ctx.ui_set_visible(&badge_path, true);
                let _ = ctx.ui_set_properties(
                    &badge_path,
                    &format!("back_color: {}; color: #0b111cf0;", bcol),
                );
                let _ = ctx.ui_set_text(&format!("{}.txt", badge_path), &txt);
            }
            None => {
                let _ = ctx.ui_set_visible(&border_path, false);
                if !tier_txt.is_empty() {
                    let tcol = if tier_txt.starts_with('S') {
                        "#ffd959cc"
                    } else if tier_txt.starts_with('A') {
                        "#5ce0e6bb"
                    } else {
                        "#68748899"
                    };
                    let _ = ctx.ui_set_visible(&badge_path, true);
                    let _ = ctx.ui_set_properties(
                        &badge_path,
                        &format!("back_color: {}; color: #0b111cc8;", tcol),
                    );
                    let _ = ctx.ui_set_text(&format!("{}.txt", badge_path), tier_txt);
                } else {
                    let _ = ctx.ui_set_visible(&badge_path, false);
                }
            }
        }
    }
}

// ───────────────── Game Draft-AI Score Observation (StableDraftScoreHook) ─────────────────

static AI_SCORE_BUF: Mutex<String> = Mutex::new(String::new());
static AI_SCORE_TICK: AtomicU32 = AtomicU32::new(0);
static CHAMP_ORDER: Mutex<Vec<String>> = Mutex::new(Vec::new());
static AI_PRIOR: Mutex<Option<HashMap<usize, (f64, u32)>>> = Mutex::new(None);
static LAST_ANCHOR: Mutex<Option<String>> = Mutex::new(None);

fn observe_score(phase: &str, ctx: &StableDraftContext, cand: usize, score: f32) {
    if let Ok(mut b) = AI_SCORE_BUF.lock() {
        if b.len() > 2 * 1024 * 1024 {
            return;
        }
        let ally_pick = ctx.ally_picks();
        let enemy_pick = ctx.enemy_picks();
        let ally_ban = ctx.ally_bans();
        let enemy_ban = ctx.enemy_bans();
        let avail = ctx.available_champions();
        let nums = |v: &[usize]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let board_sig = format!(
            "ap={} ep={} ab={} eb={}",
            nums(ally_pick),
            nums(enemy_pick),
            nums(ally_ban),
            nums(enemy_ban),
        );
        b.push_str(&format!(
            "{}\t{}\t{:.4}\t{}\t{}\t{} avail_n={}\n",
            phase,
            cand,
            score,
            ctx.is_explore() as u8,
            ts,
            board_sig,
            avail.len(),
        ));
        if phase == "B"
            && ally_pick.is_empty()
            && enemy_pick.is_empty()
            && ally_ban.is_empty()
            && enemy_ban.is_empty()
        {
            if let Ok(mut p) = AI_PRIOR.lock() {
                let e = p.get_or_insert_with(HashMap::new).entry(cand).or_insert((0.0, 0));
                e.0 += score as f64;
                e.1 += 1;
            }
        }
    }
}

struct ObserveDraftScores;
impl StableDraftHook for ObserveDraftScores {
    fn id(&self) -> String {
        "tfm2_db_export:observe_scores".to_string()
    }
    fn score_ban(
        &self,
        ctx: &StableDraftContext,
        candidate: usize,
        current_score: f32,
    ) -> StableDraftDecision {
        observe_score("B", ctx, candidate, current_score);
        StableDraftDecision::Pass
    }
    fn score_pick(
        &self,
        ctx: &StableDraftContext,
        candidate: usize,
        current_score: f32,
    ) -> StableDraftDecision {
        observe_score("P", ctx, candidate, current_score);
        StableDraftDecision::Pass
    }
}

fn flush_ai_scores() {
    let t = AI_SCORE_TICK.fetch_add(1, Ordering::Relaxed);
    if t % 600 != 0 {
        return;
    }
    let body = match AI_SCORE_BUF.lock() {
        Ok(mut b) if !b.is_empty() => std::mem::take(&mut *b),
        _ => return,
    };
    let path = format!("{}/ai_scores.log", base());
    let _ = std::fs::create_dir_all(base());
    if std::fs::metadata(&path).map(|m| m.len() > 8 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, format!("{}/ai_scores.old.log", base()));
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(body.as_bytes());
    }
}

struct CoachExtension;
impl StableExtension for CoachExtension {
    fn pre_update(&self, ctx: &mut StableClient, _dt_micros: u64) {
        flush_ai_scores();
        update_live_banpick(ctx);
    }

    fn post_update(&self, ctx: &mut StableClient, _dt_micros: u64) {
        stamp_ui(ctx);
    }
}

fn init(host: &StableHost) -> StableMod {
    let v = host.game_version();
    host.log(
        LogLevel::Info,
        &format!("tfm2_db_export v4.0 loaded (game {}.{}.{})", v.major, v.minor, v.patch),
    );
    let mut decl = StableMod::new("tfm2_db_export");
    decl.set_extension(CoachExtension);
    decl.add_draft_score_hook(ObserveDraftScores);
    decl
}

declare_stable_mod!(init);
