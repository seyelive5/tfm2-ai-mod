//! tfm2_db_export — PRODUCTION stats exporter for the draft recommender.
//!
//! Replaces `tfm2_save_probe.exe` (a standalone save-FILE decoder whose source is
//! lost and which BROKE when game 0.4.7 changed the save format). Instead of
//! decoding the save off-line, this mod reads the LIVE `ClientDatabase` from
//! inside the running game and writes the exact files the launcher +
//! `draft-weights-gen` already consume. Robust to save-format changes — only needs
//! a rebuild when the mod API itself changes (rare).
//!
//! Writes (atomically) into the launcher's probe dir whenever match / patch / pool
//! counts change (≈ once per new match, patch update, or champion release):
//!   champion_patch_statistics.tsv → drives C(winrate)/POS(lanes)/B(banrate)  [CRITICAL]
//!   solo_rank_matches.debug.txt   → gen synergy/counter (+position)          [Rust-Debug]
//!   match_replays.debug.txt       → gen synergy/counter                       [Rust-Debug]
//!   pool.txt                      → overlay POOL (db.available_champions)
//!   _stamp.txt                    → "<solo>,<replay>,<patchTotal>,<pool>" (launcher regen trigger)
//! ALSO exports the LIVE banpick board (merged from tfm2_bp_reader, 2026-06-11) to
//! bp_live.txt — the overlay's 자동연동 feed (see bp_export below). NOTE: this makes
//! the build require the game_view/engine_ui RLIB externs (see build_db_export.ps1);
//! their hashes change EVERY game patch, so rebuild per patch with the new SDK.
//! Also one-time auto-launches the overlay injector, so enabling this single mod
//! makes the whole recommender run. Read-only w.r.t. the game.

mod recommender;

use engine_ui::runner::{LabelRunner, ScrollViewRunner};
use game_view::{BanpickChampionSlotRunner, MatchUIRunner};
use mod_api::*;
use recommender::{Board, ChgInfo, Output};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

/// engine tables built from the live stats (rebuilt whenever the export fires).
static TABLES: Mutex<Option<recommender::engine::Tables>> = Mutex::new(None);
/// this frame's recommendations/commentary/tiers for the in-game display.
static OUT_NOW: Mutex<Option<Output>> = Mutex::new(None);

/// Shared dir = %LOCALAPPDATA%/tfm2-overlay (user-agnostic; the launcher + overlay use
/// the same). Falls back to C:/tmp if the env var is missing.
fn base() -> String {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:/tmp".to_string());
    format!("{}/tfm2-overlay", local.replace('\\', "/"))
}

/// Sum of growing counts; a change means new data → re-export. usize::MAX = never written.
static LAST_SIG: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Accumulated in-game ms since the last write, to throttle big dumps during fast sim.
static ACC_MS: AtomicU32 = AtomicU32::new(0);
const THROTTLE_MS: u32 = 8000;

fn atomic_write(path: &str, body: &str) {
    let tmp = format!("{}.tmp", path);
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `db.champion_patch_statistics` → the TSV the launcher (patch_current / compute_pos)
/// and the gen (banrate) parse. Column order MUST stay exactly as below; `{:?}` on the
/// position enum renders `Top`/`Jungle`/`Mid`/`Bottom`/`Support` (what compute_pos expects).
fn patch_tsv(db: &ClientDatabase) -> String {
    let mut s = String::with_capacity(64 * 1024);
    s.push_str("version\ttotal_match\tchampion\tposition\tbans\twins\tmatches\tdealing\ttanking\thealing\tkills\tdeaths\tcs\tgold\tdealing_line_phase\ttanking_line_phase\thealing_line_phase\tgold_line_phase\tcs_line_phase\n");
    for (ver, cps) in &db.champion_patch_statistics {
        for (champ, css) in &cps.data {
            for (pos, st) in &css.by_position {
                s.push_str(&format!(
                    "{}\t{}\t{}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    ver,
                    cps.total_match,
                    champ,
                    pos,
                    css.bans,
                    st.wins,
                    st.matches,
                    st.dealing,
                    st.tanking,
                    st.healing,
                    st.kills,
                    st.deaths,
                    st.cs,
                    st.gold,
                    st.dealing_line_phase,
                    st.tanking_line_phase,
                    st.healing_line_phase,
                    st.gold_line_phase,
                    st.cs_line_phase,
                ));
            }
        }
    }
    s
}

/// Split a `{:#?}` ChampionInfoSheet dump into per-champ blocks keyed by champ id
/// (the top-level field name, e.g. `fighter`). Top-level fields sit at EXACTLY a
/// 4-space indent; every line until the next such field belongs to that champ.
fn champ_blocks(dump: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut cur: Option<String> = None;
    let mut buf = String::new();
    for line in dump.lines() {
        let b = line.as_bytes();
        // a new top-level field = 4 spaces, then a lowercase/underscore ident start
        // (5+ spaces → nested; '}' / non-ident → still inside the current champ block).
        let top = b.len() > 4 && &b[..4] == b"    " && (b[4].is_ascii_lowercase() || b[4] == b'_');
        if top {
            if let Some(n) = cur.take() {
                map.insert(n, std::mem::take(&mut buf));
            }
            cur = Some(line[4..].split(':').next().unwrap_or("").trim().to_string());
        }
        if cur.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if let Some(n) = cur.take() {
        map.insert(n, buf);
    }
    map
}

/// Numeric per-component version key ("2026.10.0" sorts above "2026.2.0").
fn vkey(v: &str) -> Vec<u32> {
    v.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect()
}

/// "Bigger is better" sign per champ-def field, for buff/nerf direction scoring.
/// 0 = ambiguous (duration, slow_speed, …) or non-numeric → ignored.
fn field_sign(name: &str) -> i32 {
    match name {
        "attack" | "magic_power" | "hp" | "defence" | "magic_resistance" | "move_speed"
        | "hp_regen" | "crit_chance" | "stack" | "attack_ratio" | "range" | "attack_range"
        | "heal" | "heal_ratio" | "shield" | "shield_ratio" | "stun" | "slow_duration"
        | "speed" => 1,
        "cooltime" | "start_timing" => -1,
        _ => 0,
    }
}

/// Direction of a champ's def change: '+' net buff, '-' net nerf, '~' mixed numeric,
/// 'R' structural REWORK. A balance patch edits numbers in place (same line count,
/// same field names) → pair lines positionally and sum signed deltas. Anything
/// structural (skill added/removed, field renamed, non-numeric change) = 'R' — the
/// launcher additionally distrusts a reworked champ's historical lane/role data.
fn change_dir(old: &str, new: &str) -> char {
    if old.lines().count() != new.lines().count() {
        return 'R';
    }
    fn parse(l: &str) -> Option<(&str, f64)> {
        let (k, v) = l.trim().trim_end_matches(',').split_once(':')?;
        v.trim().parse::<f64>().ok().map(|n| (k.trim(), n))
    }
    let mut score = 0i32;
    for (o, n) in old.lines().zip(new.lines()) {
        if o == n {
            continue;
        }
        match (parse(o), parse(n)) {
            (Some((ko, vo)), Some((kn, vn))) if ko == kn => {
                let s = field_sign(ko);
                if vn > vo {
                    score += s;
                } else if vn < vo {
                    score -= s;
                }
            }
            _ => return 'R',
        }
    }
    match score.signum() {
        1 => '+',
        -1 => '-',
        _ => '~',
    }
}

/// Per-patch champ-change tracking via our OWN defs snapshot (probe/defs_snapshot.txt).
/// Runtime data showed `db.pre_patch_data` holds the CAREER-BASE defs (its key stays at
/// the base version while the career patches forward), so diffing against it is
/// cumulative-since-career-start — NOT "what changed this patch". Instead: persist the
/// current defs dump + the newest stats version seen while those defs were live; when
/// the dump changes, a patch just landed → diff(stored, current) = that patch's
/// change-set, anchored at the stored version (the last version played on the OLD
/// defs). Returns the champ_changes.tsv body (`#prev_version\t<anchor>` + header +
/// `champ\tn_diff` rows); maintains the snapshot file as a side effect. mod_champions
/// is excluded (modded champs live in one Vec field — their changes are NOT detected).
fn champ_changes_update(db: &ClientDatabase, out: &str, fp: &str) -> String {
    let cur_dump = format!("{:#?}", db.champion_info_sheet);
    let cur_ver = db
        .champion_patch_statistics
        .keys()
        .max_by_key(|v| vkey(v.as_str()))
        .cloned()
        .unwrap_or_else(|| "0".to_string());

    let snap_path = format!("{}/defs_snapshot.txt", out);
    let snap = std::fs::read_to_string(&snap_path).unwrap_or_default();
    let (head, old_dump) = snap.split_once("\n===DUMP===\n").unwrap_or(("", ""));
    let field = |k: &str| {
        head.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('\t')))
            .unwrap_or("")
    };
    let (s_fp, s_ver, s_anchor, s_changes) = (field("fp"), field("ver"), field("anchor"), field("changes"));

    let render = |anchor: &str, changes: &str| {
        let mut t = String::new();
        if !anchor.is_empty() && anchor != "-" {
            t.push_str(&format!("#prev_version\t{}\n", anchor));
        }
        t.push_str("champion\tn_diff\tdir\n");
        for c in changes.split(' ').filter(|c| !c.is_empty()) {
            let mut it = c.split(':');
            let (name, nd, dir) = (it.next().unwrap_or(""), it.next().unwrap_or("?"), it.next().unwrap_or(""));
            t.push_str(&format!("{}\t{}\t{}\n", name, nd, dir));
        }
        t
    };
    let save_snap = |ver: &str, anchor: &str, changes: &str, dump: &str| {
        atomic_write(
            &snap_path,
            &format!("fp\t{}\nver\t{}\nanchor\t{}\nchanges\t{}\n===DUMP===\n{}", fp, ver, anchor, changes, dump),
        );
    };

    if old_dump.is_empty() || s_fp != fp {
        // first run / save switch (defs are per-career — a cross-career diff would flag
        // garbage as "changed"): seed and report nothing until the next patch.
        save_snap(&cur_ver, "-", "", &cur_dump);
        return render("-", "");
    }
    if old_dump == cur_dump {
        // no patch since the snapshot; keep the last diff, and advance the version
        // stamp as the current defs accumulate played versions (so the NEXT patch's
        // anchor is the last version actually played on these defs).
        if vkey(&cur_ver) > vkey(s_ver) {
            save_snap(&cur_ver, s_anchor, s_changes, &cur_dump);
        }
        return render(s_anchor, s_changes);
    }
    // defs changed since the snapshot = a patch just landed: diff old → new.
    let prevb = champ_blocks(old_dump);
    let curb = champ_blocks(&cur_dump);
    let anomaly = prevb.len() < 50 || curb.len() < 50; // {:#?} layout drift → parse broke
    let mut changes: Vec<String> = Vec::new();
    if !anomaly {
        let mut names: Vec<&String> = curb.keys().filter(|c| c.as_str() != "mod_champions").collect();
        names.sort();
        for c in names {
            if let (Some(pv), Some(cv)) = (prevb.get(c), curb.get(c)) {
                if cv != pv {
                    // n_diff is ROUGH (positional zip: one inserted line inflates the
                    // rest). Debug info only — the launcher reads just the champ name.
                    let nd = cv.lines().zip(pv.lines()).filter(|(a, b)| a != b).count()
                        + cv.lines().count().abs_diff(pv.lines().count());
                    changes.push(format!("{}:{}:{}", c, nd, change_dir(pv, cv)));
                }
            }
        }
    }
    if anomaly || changes.len() > 25 {
        // parse drift, or an implausibly large diff (a real patch touches ~10-15
        // champs; bigger = missed save switch or format breakage): reseed, fail SAFE
        // (= feature off, stale blends merely return — never wrong zeroing).
        save_snap(&cur_ver, "-", "", &cur_dump);
        return format!("champion\tn_diff\n#anomaly\t{}/{}/{}\n", prevb.len(), curb.len(), changes.len());
    }
    let cs = changes.join(" ");
    save_snap(&cur_ver, s_ver, &cs, &cur_dump); // anchor = last version played on the OLD defs
    render(s_ver, &cs)
}

// ───────────────────── live banpick reader (merged from tfm2_bp_reader) ─────────────────────
// Reads the LIVE banpick board from the banpick screen's `MatchUIRunner` and writes
// %LOCALAPPDATA%/tfm2-overlay/bp_live.txt (deduped, atomic) — the overlay's 자동연동 feed.
//
// Route (compiler+runtime-proven 2026-06-11): `ui.query("main")` → `Node.runner` (pub) →
// raw-cast to `game_view::MatchUIRunner` (pub root re-export). A safe `downcast_ref`
// ALWAYS fails here: the game exe and the SDK rlib are separate compile sessions of the
// same source → different crate disambiguators → different TypeIds. Same source + same
// rustc ⇒ identical struct LAYOUT, so the raw cast is sound — guarded by GATE 1 (we're on
// the banpick screen: a banpick-only node exists; the game's own spectate diagnostic
// expects MatchUIRunner on "main" there) and GATE 2 (full content validation below; any
// implausible value ⇒ publish nothing).

static BP_LAST: Mutex<String> = Mutex::new(String::new());

fn bp_export(scene: &mut Scene, ui: &mut GameUI) {
    let my_team = match scene {
        Scene::InGame { data } => format!("{}", data.player_team_id()),
        _ => "-".to_string(),
    };
    // GATE 1: only in the banpick screen.
    if ui.query("main.timer_area.timer_text").is_none() {
        return;
    }
    let Some(node) = ui.query("main") else { return };
    let any: &dyn std::any::Any = node.runner.as_any();
    let m: &MatchUIRunner = unsafe { &*(any as *const dyn std::any::Any as *const MatchUIRunner) };
    // GATE 2: plain fields first (cheap tripwire before any pointer deref)…
    if m.ban_count > 20 || m.team1 > 1_000_000 || m.team2 > 1_000_000 || m.team1 == m.team2 {
        return;
    }
    let champ_ok = |s: &String| {
        !s.is_empty() && s.len() <= 30 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    };
    let vec_ok = |v: &Vec<String>| v.len() <= 10 && v.iter().all(champ_ok);
    if !(vec_ok(&m.team1_ban) && vec_ok(&m.team2_ban) && vec_ok(&m.team1_pick) && vec_ok(&m.team2_pick)) {
        return;
    }
    if m.team1_order.len() > 10 || m.team2_order.len() > 10 {
        return;
    }

    // set number + previous sets' picks (the fearless feed): completed sets live in
    // match_info.replays (ids into db.match_replays, which carries team-attributed
    // picks). try_borrow: never fight the game over its own RefCell.
    let set_no = m.match_info.replays.len() + 1;
    let (mut prev_my, mut prev_enemy) = (Vec::new(), Vec::new());
    if let (Ok(db), Some(my_id)) = (m.database.try_borrow(), my_team.parse::<usize>().ok().as_ref()) {
        for rid in &m.match_info.replays {
            if let Some(rep) = db.match_replays.get(rid) {
                let (mine, theirs) = if rep.blue_team_id == *my_id {
                    (&rep.blue_team, &rep.red_team)
                } else {
                    (&rep.red_team, &rep.blue_team)
                };
                prev_my.extend(mine.iter().map(|a| a.champion.clone()));
                prev_enemy.extend(theirs.iter().map(|a| a.champion.clone()));
            }
        }
    }
    // fearless locks exactly as the GAME enforces them: a cell that is DISABLED but
    // not banned/picked is fearless-locked for me (ground truth, no style guessing).
    // fearless_icon/fearless_x visibility exported too (icon = "was used" info mark).
    let taken: Vec<&String> = m
        .team1_ban
        .iter()
        .chain(m.team2_ban.iter())
        .chain(m.team1_pick.iter())
        .chain(m.team2_pick.iter())
        .collect();
    let (mut locked_d, mut locked_x, mut locked_i) = (Vec::new(), Vec::new(), Vec::new());
    if let Some(root) = ui.query_mut("main") {
        if let Some(ch) = find_mut(root, "champions") {
            if let Some(contents) = find_mut(ch, "contents") {
                for cell in contents.child.iter() {
                    if cell.disabled && !taken.iter().any(|t| **t == cell.id) {
                        locked_d.push(cell.id.clone());
                    }
                    for k in cell.child.iter() {
                        if k.visible {
                            if k.id == "fearless_x" {
                                locked_x.push(cell.id.clone());
                            } else if k.id == "fearless_icon" {
                                locked_i.push(cell.id.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // ── feed the in-mod recommender directly (no file round-trip): resolve sides,
    // run the engine when the board changed, stash the Output for stamp_grid.
    if let Some(my_id) = my_team.parse::<usize>().ok() {
        if my_id == m.team1 || my_id == m.team2 {
            let mine_is_t1 = my_id == m.team1;
            let board = Board {
                my_ban: if mine_is_t1 { m.team1_ban.clone() } else { m.team2_ban.clone() },
                enemy_ban: if mine_is_t1 { m.team2_ban.clone() } else { m.team1_ban.clone() },
                my_pick: if mine_is_t1 { m.team1_pick.clone() } else { m.team2_pick.clone() },
                enemy_pick: if mine_is_t1 { m.team2_pick.clone() } else { m.team1_pick.clone() },
                ban_count: m.ban_count,
                teams: format!("{}|{}", m.team1, m.team2),
                locked: locked_d.clone(),
            };
            if let Ok(mut tg) = TABLES.lock() {
                let tables = tg.get_or_insert_with(|| recommender::build_tables("", &[], &ChgInfo::default()));
                let out = recommender::update(&board, tables);
                if let Ok(mut og) = OUT_NOW.lock() {
                    *og = Some(out);
                }
            }
        }
    }

    let key = format!(
        "my_team={}\nteam1={}\nteam2={}\nban_count={}\nteam1_ban={}\nteam2_ban={}\nteam1_pick={}\nteam2_pick={}\nteam1_order={}\nteam2_order={}\nset={}\nprev_my={}\nprev_enemy={}\nlocked_x={}\nlocked_i={}\n",
        my_team,
        m.team1,
        m.team2,
        m.ban_count,
        m.team1_ban.join(","),
        m.team2_ban.join(","),
        m.team1_pick.join(","),
        m.team2_pick.join(","),
        m.team1_order.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","),
        m.team2_order.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","),
        set_no,
        prev_my.join(","),
        prev_enemy.join(","),
        locked_x.join(","),
        locked_i.join(","),
    ) + &format!("locked_d={}\n", locked_d.join(","));
    let mut last = match BP_LAST.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if *last == key {
        return;
    }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = format!("{}/bp_live.txt", base());
    let tmp = format!("{}.tmp", path);
    let _ = std::fs::create_dir_all(base());
    if std::fs::write(&tmp, format!("ts={}\n{}", ms, key)).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        *last = key;
    }
}

// ───────────────── in-game grid display (banpick champion grid) ─────────────────
// Two layers, restamped every frame (the game only rewrites on its own signature-
// gated change events, so our writes win):
//  1. TIER TEXT — each cell's #tier label gets OUR tier (S~D) + ▲▼⚡ patch tags.
//  2. RECOMMENDATION BORDERS — teal=pick, violet=ban, rank 1 thicker. Cells get
//     their ORIGINAL border back when no longer recommended, so the marker MOVES.
// All data comes from the in-mod recommender (OUT_NOW) — no file round-trips.
// Cell ids ARE champion ids (runtime-verified).

/// each cell's border (width, rgba) as first seen — restored when not recommended.
static ORIG_BORDER: Mutex<Option<HashMap<String, (f32, [f32; 4])>>> = Mutex::new(None);
/// each cell's #tier label text as first seen — restored when we have nothing to say.
static ORIG_TIER: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
/// the header #desc text the game wrote (saved before we replace it with our recs).
static HDR_ORIG: Mutex<String> = Mutex::new(String::new());

const RANK_GLYPH: [&str; 5] = ["①", "②", "③", "④", "⑤"];

/// diag for the coach-popup injection (written on change → ai_say_diag.txt).
static SAY_DIAG: Mutex<String> = Mutex::new(String::new());


const REC_PICK_COLOR: [f32; 4] = [0.22, 0.84, 0.70, 1.0]; // teal (overlay accent)
const REC_BAN_COLOR: [f32; 4] = [0.78, 0.40, 0.95, 1.0]; // violet (≠ coach red/blue)

/// Depth-first search for a node id in the subtree.
fn find_mut<'a>(n: &'a mut Node, id: &str) -> Option<&'a mut Node> {
    if n.id == id {
        return Some(n);
    }
    for c in n.child.iter_mut() {
        if let Some(f) = find_mut(c, id) {
            return Some(f);
        }
    }
    None
}

/// Raw cast (downcast_ref can't work: game exe vs SDK rlib TypeIds differ; layouts
/// are identical — same source, same rustc). Only call on nodes the layout defines
/// as `label`.
unsafe fn as_label(n: &mut Node) -> &mut LabelRunner {
    &mut *(n.runner.as_any_mut() as *mut dyn std::any::Any as *mut LabelRunner)
}

fn stamp_grid(ui: &mut GameUI, assets: &Assets) {
    // banpick screen only
    if ui.query("main.timer_area.timer_text").is_none() {
        return;
    }
    // everything to display comes from the in-mod recommender (computed in
    // pre_update's bp_export from the live board + live stats — no files).
    let out_guard = match OUT_NOW.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(o) = out_guard.as_ref() else { return };
    let o = o.clone();
    drop(out_guard);
    let (rec_ban, recs, swap, say, tiers) = (o.rec_ban, &o.recs, &o.swap, o.say.as_str(), &o.tiers);

    let mut orig_guard = match ORIG_BORDER.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let orig_map = orig_guard.get_or_insert_with(HashMap::new);
    let mut tier_orig_guard = match ORIG_TIER.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let tier_orig = tier_orig_guard.get_or_insert_with(HashMap::new);

    let Some(root) = ui.query_mut("main") else { return };

    // ── AI panel = a CLONE of the 코치진 대화 내역 popup: native draggable window,
    // close button, opacity slider — all inherited. Title → "AI 추천기", messages
    // trimmed to ONE row carrying our commentary (one row = no stacking/scroll
    // issues). Recreated automatically if the screen rebuilds; NOT re-shown every
    // frame, so the user can close it with the X (next banpick brings it back).
    if !say.is_empty() {
        let mut diag = String::new();
        if find_mut(root, "ai_panel").is_none() {
            let panel = if let Some(popup) = find_mut(root, "coach_dialogue_popup") {
                let has_rows = find_mut(popup, "messages").map(|m| !m.child.is_empty()).unwrap_or(false);
                if has_rows {
                    let mut p = popup.clone();
                    p.id = "ai_panel".to_string();
                    p.visible = true;
                    if let Some(msgs) = find_mut(&mut p, "messages") {
                        // keep the LAST row (its inherited position matches the
                        // popup's bottom-resting scroll → visible immediately)
                        if let Some(last) = msgs.child.pop() {
                            msgs.child.clear();
                            msgs.child.push(last);
                        }
                        if let Some(row) = msgs.child.first_mut() {
                            row.id = "ai_row".to_string();
                            row.visible = true;
                        }
                    }
                    if let Some(t) = find_mut(&mut p, "title") {
                        unsafe { as_label(t) }.text = "AI 추천기".to_string();
                        t.runner.set_dirty(true);
                    }
                    // the cloned scroll view remembers the original's bottom-rest
                    // offset → reset to the top so the row shows without scrolling.
                    if let Some(sc) = find_mut(&mut p, "chat_scroll") {
                        let sv = unsafe {
                            &mut *(sc.runner.as_any_mut() as *mut dyn std::any::Any as *mut ScrollViewRunner)
                        };
                        sv.ratio = 0.0;
                        sc.runner.set_dirty(true);
                    }
                    Some(p)
                } else {
                    diag.push_str("popup-empty;");
                    None
                }
            } else {
                diag.push_str("popup-not-found;");
                None
            };
            if let Some(p) = panel {
                root.add_child(assets, p);
                diag.push_str("panel-created;");
            }
        }
        if let Some(panel) = find_mut(root, "ai_panel") {
            if let Some(row) = find_mut(panel, "ai_row") {
                // the cloned coach message's champion chips are stale → hide
                if let Some(ent) = find_mut(row, "entities") {
                    ent.visible = false;
                }
                if let Some(nm) = find_mut(row, "name") {
                    let lr = unsafe { as_label(nm) };
                    if lr.text != "AI 추천기" {
                        lr.text = "AI 추천기".to_string();
                        nm.runner.set_dirty(true);
                    }
                }
                if let Some(tx) = find_mut(row, "text") {
                    let lr = unsafe { as_label(tx) };
                    if lr.text != say {
                        lr.text = say.to_string();
                        tx.runner.set_dirty(true);
                    }
                    diag.push_str("ok;");
                }
            }
        }
        if let Ok(mut last) = SAY_DIAG.lock() {
            if *last != diag {
                let _ = std::fs::write(format!("{}/ai_say_diag.txt", base()), &diag);
                *last = diag;
            }
        }
    }

    // ── header line: the explicit ranked list, names + scores, in the top banner.
    if let Some(hdr) = find_mut(root, "header") {
        if let Some(desc) = find_mut(hdr, "desc") {
            let lr = unsafe { as_label(desc) };
            if let Ok(mut orig) = HDR_ORIG.lock() {
                // priority: swap lane assignment (my 5 picks done) > recommendations
                let line = if !swap.is_empty() {
                    Some(format!("추천 배치 ▶  {}", swap.join("  ·  ")))
                } else if !recs.is_empty() {
                    // top 3 only — 5 entries overflowed into the phase label
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
                match line {
                    Some(line) => {
                        if !lr.text.starts_with("추천") {
                            *orig = lr.text.clone(); // the game's own instruction text
                        }
                        if lr.text != line {
                            lr.text = line;
                            desc.runner.set_dirty(true);
                        }
                    }
                    None => {
                        if lr.text.starts_with("추천") && !orig.is_empty() {
                            lr.text = orig.clone();
                            desc.runner.set_dirty(true);
                        }
                    }
                }
            }
        }
    }

    let Some(champs) = find_mut(root, "champions") else { return };
    let Some(contents) = find_mut(champs, "contents") else { return };
    for cell in contents.child.iter_mut() {
        let champ = cell.id.clone();
        let rank = recs.iter().position(|(id, _, _)| *id == champ);
        // 1) tier label: "①S▲" for ranked champs, plain tier otherwise; restore the
        // game's original text when we have neither (so stale ranks never linger).
        if let Some(tl) = find_mut(cell, "tier") {
            let cur = unsafe { as_label(tl) }.text.clone();
            let orig_text = tier_orig.entry(champ.clone()).or_insert_with(|| cur.clone());
            let tier_txt = tiers.get(&champ).map(|t| t.as_str()).unwrap_or("");
            let desired = match rank {
                Some(rk) => format!("{}{}", RANK_GLYPH[rk.min(4)], tier_txt),
                None if !tier_txt.is_empty() => tier_txt.to_string(),
                None => orig_text.clone(),
            };
            if cur != desired {
                tl.visible = true;
                unsafe { as_label(tl) }.text = desired;
                tl.runner.set_dirty(true);
            }
        }
        // 2) recommendation border (restore-to-original when not recommended)
        let sr = unsafe {
            &mut *(cell.runner.as_any_mut() as *mut dyn std::any::Any
                as *mut BanpickChampionSlotRunner)
        };
        let orig = *orig_map.entry(champ.clone()).or_insert((
            sr.line_width,
            [sr.line_color.normal.r, sr.line_color.normal.g, sr.line_color.normal.b, sr.line_color.normal.a],
        ));
        let (w, col) = match rank {
            Some(0) => (4.0, if rec_ban { REC_BAN_COLOR } else { REC_PICK_COLOR }),
            Some(_) => (2.8, if rec_ban { REC_BAN_COLOR } else { REC_PICK_COLOR }),
            None => orig,
        };
        if sr.line_width != w
            || sr.line_color.normal.r != col[0]
            || sr.line_color.normal.g != col[1]
            || sr.line_color.normal.b != col[2]
        {
            sr.line_width = w;
            sr.line_color.normal.r = col[0];
            sr.line_color.normal.g = col[1];
            sr.line_color.normal.b = col[2];
            sr.line_color.normal.a = col[3];
            cell.runner.set_dirty(true);
        }
    }
}

struct E;
impl ModExtension for E {
    fn post_update(&self, _scene: &mut Scene, ui: &mut GameUI, a: &mut Assets, _dt: f32) {
        // in-game tier + recommendation display — after the game's own UI updates
        // so our writes win.
        stamp_grid(ui, a);
    }

    fn pre_update(&self, scene: &mut Scene, ui: &mut GameUI, _a: &mut Assets, dt: f32) {
        // live banpick board first — every frame, deduped, banpick-gated (cheap).
        bp_export(scene, ui);

        let Scene::InGame { data } = scene else {
            return;
        };

        let db = data.db();
        // early InGame frames have an empty db (still loading) — wait for it.
        if db.available_champions.is_empty() && db.athletes.is_empty() {
            return;
        }

        // cheap per-frame change signature (all O(1) / tiny). Counts only grow, so the
        // sum changes iff new data arrived (or a different save loaded).
        let solo = db.solo_rank_matches.len();
        let replay = db.match_replays.len();
        let pool_n = db.available_champions.len();
        let patch_total: usize = db.champion_patch_statistics.values().map(|c| c.total_match).sum();
        let sig = solo
            .wrapping_add(replay)
            .wrapping_add(patch_total)
            .wrapping_add(pool_n);

        let first = LAST_SIG.load(Ordering::Relaxed) == usize::MAX;
        if !first {
            if LAST_SIG.load(Ordering::Relaxed) == sig {
                return; // nothing changed
            }
            // changed — but throttle the (multi-MB) dump during fast season sim
            let ms = (dt * 1000.0) as u32;
            let acc = ACC_MS.fetch_add(ms, Ordering::Relaxed).saturating_add(ms);
            if acc < THROTTLE_MS {
                return;
            }
        }
        ACC_MS.store(0, Ordering::Relaxed);
        LAST_SIG.store(sig, Ordering::Relaxed);

        let b = base();
        let out = format!("{}/probe", b);
        let _ = std::fs::create_dir_all(&out);
        // which champs were buffed/nerfed this patch (defs-snapshot rotation) → the
        // recommender drops their stale prev-patch winrate blend. The career
        // fingerprint reseeds the snapshot on a save switch (patches are per-career;
        // cross-career diffs lie).
        let fp = format!(
            "{}|{}|{}",
            data.player_team_id(),
            db.teams.len(),
            db.champion_patch_statistics.keys().min_by_key(|v| vkey(v.as_str())).map(|s| s.as_str()).unwrap_or("0")
        );
        let chg_tsv = champ_changes_update(&db, &out, &fp);
        atomic_write(&format!("{}/champ_changes.tsv", out), &chg_tsv);
        let pt = patch_tsv(&db);
        atomic_write(&format!("{}/champion_patch_statistics.tsv", out), &pt);
        atomic_write(&format!("{}/solo_rank_matches.debug.txt", out), &format!("{:#?}", db.solo_rank_matches));
        atomic_write(&format!("{}/match_replays.debug.txt", out), &format!("{:#?}", db.match_replays));
        let mut pool: Vec<String> = db.available_champions.iter().cloned().collect();
        pool.sort_unstable();
        atomic_write(&format!("{}/pool.txt", b), &pool.join(","));

        // ── rebuild the in-mod recommender tables from the fresh stats. The chg
        // anchor/dirs are parsed back from the TSV we just rendered (single source).
        let chg_info = {
            let mut info = ChgInfo::default();
            for l in chg_tsv.lines() {
                if let Some(v) = l.strip_prefix("#prev_version\t") {
                    info.anchor = Some(v.trim().to_string());
                } else if !l.starts_with('#') && !l.starts_with("champion\t") {
                    let mut f = l.split('\t');
                    if let Some(c) = f.next().filter(|c| !c.is_empty()) {
                        let dir = f.nth(1).and_then(|x| x.chars().next()).unwrap_or('~');
                        info.dirs.push((c.to_string(), dir));
                    }
                }
            }
            info
        };
        if let Ok(mut tg) = TABLES.lock() {
            *tg = Some(recommender::build_tables(&pt, &pool, &chg_info));
        }
        atomic_write(
            &format!("{}/_stamp.txt", out),
            &format!("{},{},{},{}", solo, replay, patch_total, pool_n),
        );

        // roster.tsv — for the "why am I losing" diagnosis: which team is MINE
        // (player_team_id) + every athlete's position affinities + stats.
        let my_team = data.player_team_id();
        let mut rt = format!("MYTEAM\t{}\n", my_team);
        rt.push_str("A\tid\tteam\tname\ttop\tjungle\tmid\tbottom\tsupport\tlast_hit\tskill_avoid\tskill_hit\tpositioning\tcontrol_speed\tconcentration\tmental\tjudgement\torder\troaming\taggressive\tego\n");
        for (aid, a) in &db.athletes {
            let s = &a.stat;
            rt.push_str(&format!(
                "A\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                aid, a.contract.team_id().map(|t| t as i64).unwrap_or(-1), a.name,
                s.top, s.jungle, s.mid, s.bottom, s.support,
                s.last_hit, s.skill_avoid, s.skill_hit, s.positioning, s.control_speed,
                s.concentration, s.mental, s.judgement, s.order, s.roaming, s.aggressive, s.ego
            ));
        }
        atomic_write(&format!("{}/probe/roster.tsv", b), &rt);

        // teams.tsv — id -> name (overlay opponent picker / opponent-aware bans).
        let mut tt = String::from("id\tname\tleague\n");
        for (tid, t) in &db.teams {
            tt.push_str(&format!("{}\t{}\t{}\n", tid, t.name, t.league_id));
        }
        atomic_write(&format!("{}/probe/teams.tsv", b), &tt);

        // (v3.0: no external launcher/overlay anymore — the whole recommender runs
        // inside this mod. File exports above remain for the report tool/debugging.)
    }
}

fn init(_c: &GameCtx) -> ModRegistration {
    let mut r = ModRegistration::new("tfm2_db_export");
    r.set_extension(E);
    r
}

declare_mod!(init);
