//! draft-overlay — injected OpenGL overlay: a manual-input ban/pick RECOMMENDER for TFM2.
//!
//! No game mod. Click the 60-champ grid (or the recommendation rows) to mark
//! my-pick / enemy-pick / my-ban / enemy-ban; it instantly ranks the rest.
//! Stats + the per-save champion pool come from C:\tmp\tfm2-overlay\draft_weights.tsv
//! (written by inject-overlay.exe, auto-reloaded when it changes). Ctrl+F10 toggles.

mod engine;

use engine::{recommend, recommend_lanes, DraftState, Tables, CHAMPS};
use hudhook::hooks::opengl3::ImguiOpenGl3Hooks;
use hudhook::imgui::*;
use hudhook::{hudhook, ImguiRenderLoop, MessageFilter, RenderContext};
use std::collections::HashSet;
use std::time::SystemTime;

const FONT_PATH: &str = "C:\\Windows\\Fonts\\malgun.ttf";
const COLS: usize = 6;

/// Shared data dir = %LOCALAPPDATA%\tfm2-overlay — user-agnostic; the mod, the launcher,
/// and this injected overlay all read/write here. Falls back to C:\tmp if env missing.
fn shared(name: &str) -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:\\tmp".into());
    std::path::PathBuf::from(base).join("tfm2-overlay").join(name)
}
const BTN_W: f32 = 94.0;
const RELOAD_EVERY_FRAMES: u64 = 120;

// palette
const ACCENT: [f32; 4] = [0.32, 0.84, 0.80, 1.0]; // teal
const GOLD: [f32; 4] = [0.99, 0.83, 0.38, 1.0];
const GREEN: [f32; 4] = [0.46, 0.92, 0.56, 1.0];
const RED: [f32; 4] = [0.98, 0.46, 0.46, 1.0];
const WHITE: [f32; 4] = [0.88, 0.90, 0.93, 1.0];
const GREY: [f32; 4] = [0.58, 0.62, 0.68, 1.0];

/// Meta-tier color (matches report.html): S red · A orange · B yellow · C green · D grey.
fn tier_color(t: &str) -> [f32; 4] {
    // strip a trailing "?" (statistically-unstable tier marker) before matching.
    match t.trim_end_matches('?') {
        "S" => [1.0, 0.36, 0.36, 1.0],
        "A" => [1.0, 0.62, 0.26, 1.0],
        "B" => [1.0, 0.79, 0.34, 1.0],
        "C" => [0.56, 0.82, 0.31, 1.0],
        "D" => [0.55, 0.60, 0.68, 1.0],
        _ => WHITE,
    }
}

/// Marking-mode label + a distinct color (so the current click target is obvious).
fn mode_badge(m: Mode) -> (&'static str, [f32; 4]) {
    match m {
        Mode::MyPick => ("내 픽", [0.36, 0.85, 0.50, 1.0]),
        Mode::EnemyPick => ("상대 픽", [0.95, 0.45, 0.45, 1.0]),
        Mode::MyBan => ("내 밴", [0.92, 0.80, 0.38, 1.0]),
        Mode::EnemyBan => ("상대 밴", [0.82, 0.50, 0.86, 1.0]),
        Mode::Clear => ("지움", GREY),
        Mode::Pool => ("풀 편집", ACCENT),
    }
}

#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(v_key: i32) -> i16;
}
const VK_CONTROL: i32 = 0x11;
const VK_F10: i32 = 0x79;
fn key_down(vk: i32) -> bool {
    (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
}

fn tsv_mtime() -> Option<SystemTime> {
    std::fs::metadata(shared("draft_weights.tsv")).and_then(|m| m.modified()).ok()
}

/// (ui_scale, btn_w, bg_alpha, bp_auto) — settings, persisted to settings.txt.
fn load_settings() -> (f32, f32, f32, bool) {
    let (mut us, mut bw, mut ba) = (1.0f32, BTN_W, 0.97f32);
    let mut auto = true; // live banpick sync on by default (needs the bp_reader mod)
    for line in std::fs::read_to_string(shared("settings.txt")).unwrap_or_default().lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "bp_auto" {
                auto = v.trim() != "0";
                continue;
            }
            let val: f32 = v.trim().parse().unwrap_or(f32::NAN);
            if val.is_finite() {
                match k.trim() {
                    "ui_scale" => us = val,
                    "btn_w" => bw = val,
                    "bg_alpha" => ba = val,
                    _ => {}
                }
            }
        }
    }
    (us.clamp(0.6, 2.5), bw.clamp(50.0, 200.0), ba.clamp(0.25, 1.0), auto)
}

fn save_settings(us: f32, bw: f32, ba: f32, auto: bool) {
    let _ = std::fs::write(
        shared("settings.txt"),
        format!("ui_scale={}\nbtn_w={}\nbg_alpha={}\nbp_auto={}\n", us, bw, ba, auto as u8),
    );
}

/// bp_live.txt (tfm2_bp_reader mod) = the LIVE banpick board read straight out of
/// the game's banpick screen (MatchUIRunner). Returns (my_ban, enemy_ban, my_pick,
/// enemy_pick) with sides resolved via my_team vs team1/team2; None when the file
/// is absent/unparsable or my_team matches neither side.
struct BpLive {
    my_ban: Vec<String>,
    enemy_ban: Vec<String>,
    my_pick: Vec<String>,
    enemy_pick: Vec<String>,
    ban_count: usize,
    /// the two team ids — a CHANGED pair means a new match (reset the series).
    teams: String,
    /// champs the GAME actually locks in the grid (disabled minus bans/picks —
    /// fearless ground truth; falls back to the fearless icon marks).
    locked: Vec<String>,
}

fn read_bp_live() -> Option<BpLive> {
    let s = std::fs::read_to_string(shared("bp_live.txt")).ok()?;
    let mut kv = std::collections::HashMap::new();
    for l in s.lines() {
        if let Some((k, v)) = l.split_once('=') {
            kv.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let my = kv.get("my_team")?;
    let mine_is_t1 = if my == kv.get("team1")? {
        true
    } else if my == kv.get("team2")? {
        false
    } else {
        return None;
    };
    let list = |k: &str| -> Vec<String> {
        kv.get(k)
            .map(|v| v.split(',').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect())
            .unwrap_or_default()
    };
    let mut locked = list("locked_d");
    if locked.is_empty() {
        locked = list("locked_x");
        for c in list("locked_i") {
            if !locked.contains(&c) {
                locked.push(c);
            }
        }
    }
    let (my_ban, enemy_ban, my_pick, enemy_pick) = if mine_is_t1 {
        (list("team1_ban"), list("team2_ban"), list("team1_pick"), list("team2_pick"))
    } else {
        (list("team2_ban"), list("team1_ban"), list("team2_pick"), list("team1_pick"))
    };
    Some(BpLive {
        my_ban,
        enemy_ban,
        my_pick,
        enemy_pick,
        ban_count: kv.get("ban_count").and_then(|v| v.parse().ok()).unwrap_or(0),
        teams: format!("{}|{}", kv.get("team1").cloned().unwrap_or_default(), kv.get("team2").cloned().unwrap_or_default()),
        locked,
    })
}


/// "save_20260528_004140.data" -> "05-28 00:41"
fn fmt_date(filename: &str) -> String {
    let s = filename.strip_prefix("save_").unwrap_or(filename);
    let s = s.strip_suffix(".data").unwrap_or(s);
    if let Some((date, time)) = s.split_once('_') {
        if date.len() == 8 && time.len() >= 4 {
            return format!("{}-{} {}:{}", &date[4..6], &date[6..8], &time[0..2], &time[2..4]);
        }
    }
    s.to_string()
}

/// saves.tsv (filename<TAB>count<TAB>sizeMB<TAB>team<TAB>manager) → [(filename, label)].
/// Display leads with the player's MANAGER name + team (from the save header) so the
/// user instantly tells careers apart: "asdf (Gen.G) · 05-31 20:35 · 25MB · 8판".
fn load_saves() -> Vec<(String, String)> {
    std::fs::read_to_string(shared("saves.tsv"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let f = it.next()?.to_string();
            if f.is_empty() {
                return None;
            }
            let count = it.next().unwrap_or("?");
            let mb = it.next().unwrap_or("?");
            let team = it.next().unwrap_or("").trim();
            let manager = it.next().unwrap_or("").trim();
            let cnt = if count == "?" { String::new() } else { format!("  ·  {}판", count) };
            let who = if !manager.is_empty() {
                if team.is_empty() { manager.to_string() } else { format!("{} ({})", manager, team) }
            } else {
                team.to_string()
            };
            let who_p = if who.is_empty() { String::new() } else { format!("{}  ·  ", who) };
            Some((f.clone(), format!("{}{}  ·  {}MB{}", who_p, fmt_date(&f), mb, cnt)))
        })
        .collect()
}

fn load_pin() -> String {
    std::fs::read_to_string(shared("pin.txt")).unwrap_or_default().trim().to_string()
}

fn save_pin(name: &str) {
    let _ = std::fs::write(shared("pin.txt"), name);
}

/// combo index for the current pin (0 = auto/newest).
fn sel_from_pin(saves: &[(String, String)], pin: &str) -> usize {
    if pin.is_empty() {
        return 0;
    }
    saves.iter().position(|(f, _)| f == pin).map(|i| i + 1).unwrap_or(0)
}

fn load_tables() -> (Tables, String) {
    match std::fs::read_to_string(shared("draft_weights.tsv")) {
        // Use the launcher-written stats whenever they belong to a real SAVE — even a
        // brand-new game with ZERO champ-winrate (C) rows (= cold-start). Keying on a
        // `C` row was the bug: a fresh game has no C rows, so the overlay fell back to
        // the BAKED table and wrongly showed its old 901-match stats for a new career.
        Ok(s) if s.lines().any(|l| l.starts_with("SAVE\t")) => {
            let champs = s.lines().filter(|l| l.starts_with("C\t")).count();
            let src = if champs == 0 {
                "새 세이브 · 데이터 없음(콜드스타트)".to_string()
            } else {
                format!("{}챔프", champs)
            };
            (engine::parse_tables_from_str(&s), src)
        }
        _ => (engine::parse_tables(), "내장 기본값".to_string()),
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    MyPick,
    EnemyPick,
    MyBan,
    EnemyBan,
    Clear,
    Pool,
}

#[derive(PartialEq, Clone, Copy)]
enum Slot {
    Avail,
    MyPick,
    EnemyPick,
    MyBan,
    EnemyBan,
}

/// Series ban/pick rule (TFM2 banpick_style). Champions PICKED in earlier sets
/// get locked out of later sets:
///   Off    = classic (no carry-over).
///   Normal = 피어리스: each team can't repeat ITS OWN prior picks (per-team).
///   Hard   = 하드 피어리스: neither team can pick anything EITHER team used.
#[derive(PartialEq, Clone, Copy)]
enum Fearless {
    Off,
    Normal,
    Hard,
}

fn slot_colors(s: Slot) -> [[f32; 4]; 3] {
    match s {
        Slot::Avail => [[0.16, 0.18, 0.23, 1.0], [0.27, 0.33, 0.42, 1.0], [0.32, 0.40, 0.50, 1.0]],
        Slot::MyPick => [[0.14, 0.45, 0.24, 1.0], [0.19, 0.58, 0.30, 1.0], [0.23, 0.66, 0.34, 1.0]],
        Slot::EnemyPick => [[0.52, 0.17, 0.19, 1.0], [0.66, 0.23, 0.25, 1.0], [0.74, 0.27, 0.29, 1.0]],
        Slot::MyBan => [[0.30, 0.27, 0.16, 1.0], [0.40, 0.36, 0.22, 1.0], [0.46, 0.41, 0.26, 1.0]],
        Slot::EnemyBan => [[0.34, 0.20, 0.30, 1.0], [0.44, 0.26, 0.40, 1.0], [0.50, 0.30, 0.46, 1.0]],
    }
}

// Korean initial-consonant (초성) quick-filter — lets the user find champs by
// CLICKING instead of typing Korean (the injected overlay can't take Korean IME
// input). 14 base groups; the 5 tense consonants fold into their plain group.
const CHOSUNG: [&str; 14] =
    ["ㄱ", "ㄴ", "ㄷ", "ㄹ", "ㅁ", "ㅂ", "ㅅ", "ㅇ", "ㅈ", "ㅊ", "ㅋ", "ㅌ", "ㅍ", "ㅎ"];

/// 초성 group (0..14) of a Korean name's first syllable; None if not Hangul.
fn chosung_group(name: &str) -> Option<u8> {
    let s = name.chars().next()? as u32;
    if !(0xAC00..=0xD7A3).contains(&s) {
        return None;
    }
    // 19 leading-jamo index → 14 base groups (ㄲ→ㄱ, ㄸ→ㄷ, ㅃ→ㅂ, ㅆ→ㅅ, ㅉ→ㅈ)
    const MAP: [u8; 19] = [0, 0, 1, 2, 2, 3, 4, 5, 5, 6, 6, 7, 8, 8, 9, 10, 11, 12, 13];
    Some(MAP[((s - 0xAC00) / 588) as usize])
}

fn apply_mode(cur: Slot, mode: Mode) -> Slot {
    let target = match mode {
        Mode::MyPick => Slot::MyPick,
        Mode::EnemyPick => Slot::EnemyPick,
        Mode::MyBan => Slot::MyBan,
        Mode::EnemyBan => Slot::EnemyBan,
        Mode::Clear => return Slot::Avail,
        Mode::Pool => return cur,
    };
    if cur == target {
        Slot::Avail
    } else {
        target
    }
}

struct Overlay {
    tables: Tables,
    tsv_source: String,
    tsv_mtime: Option<SystemTime>,
    pool: HashSet<String>,
    slots: Vec<Slot>,
    mode: Mode,
    rec_ban: bool,
    search: String,
    saves: Vec<(String, String)>,
    sel_idx: usize,
    ui_scale: f32,
    btn_w: f32,
    bg_alpha: f32,
    show_settings: bool,
    show_help: bool,
    pick_order: i8, // 0=auto, 1=선픽, 2=후픽
    visible: bool,
    prev_combo: bool,
    font_loaded: bool,
    frames: u64,
    last_reload_frame: u64,
    // fearless series tracking
    fearless: Fearless,
    series_my: HashSet<String>,    // champs MY team picked in earlier sets
    series_enemy: HashSet<String>, // champs ENEMY picked in earlier sets
    series_game: u32,              // current set number (1-based)
    chosung: Option<u8>,           // active 초성 grid filter (None = all)
    // live banpick sync (bp_live.txt from the tfm2_bp_reader mod)
    bp_auto: bool,                 // snap the grid to the live board on change
    bp_mtime: Option<SystemTime>,  // last seen bp_live.txt mtime
    // last recommendation export (recs.txt → in-game border highlights)
    last_recs: String,
    // set-boundary tracking for fearless (replays only attach post-match)
    bp_teams: String,
    bp_full_board: Option<(Vec<String>, Vec<String>)>,
    /// detected at the draft's FIRST pick (unambiguous moment): am I first-pick?
    bp_first_pick: Option<bool>,
}

impl Overlay {
    fn new() -> Self {
        let (tables, tsv_source) = load_tables();
        let pool = tables.pool.clone();
        let (ui_scale, btn_w, bg_alpha, bp_auto) = load_settings();
        let saves = load_saves();
        let sel_idx = sel_from_pin(&saves, &load_pin());
        Overlay {
            tables,
            tsv_source,
            tsv_mtime: tsv_mtime(),
            pool,
            slots: vec![Slot::Avail; CHAMPS.len()],
            mode: Mode::MyBan,   // draft starts with bans
            rec_ban: true,        // …so recommend bans first
            search: String::new(),
            saves,
            sel_idx,
            ui_scale,
            btn_w,
            bg_alpha,
            show_settings: false,
            show_help: false,
            pick_order: 0,
            // start HIDDEN: the in-game display (tier text / rec borders / header
            // line / AI panel) replaced the window; the engine keeps running and
            // Ctrl+F10 summons the window for settings / 통계 새로고침 / debugging.
            visible: false,
            prev_combo: false,
            font_loaded: false,
            frames: 0,
            last_reload_frame: 0,
            fearless: Fearless::Off,
            series_my: HashSet::new(),
            series_enemy: HashSet::new(),
            series_game: 1,
            chosung: None,
            bp_auto,
            bp_mtime: None,
            last_recs: String::new(),
            bp_teams: String::new(),
            bp_full_board: None,
            bp_first_pick: None,
        }
    }

    fn current_state(&self) -> DraftState {
        let mut st = DraftState::default();
        st.ok = true;
        st.phase = if self.rec_ban { "BAN".into() } else { "PICK".into() };
        // 자동(0)일 때 라이브 밴픽에서 감지한 선픽/후픽을 적용 (수동 선택은 우선)
        st.pick_order = if self.pick_order == 0 {
            match self.bp_first_pick {
                Some(true) => 1,  // 내가 선픽
                Some(false) => 2, // 내가 후픽
                None => 0,
            }
        } else {
            self.pick_order
        };
        for (i, (id, _ko)) in CHAMPS.iter().enumerate() {
            match self.slots[i] {
                Slot::MyPick => st.ally_pick.push(id.to_string()),
                Slot::EnemyPick => st.enemy_pick.push(id.to_string()),
                Slot::MyBan => st.ally_ban.push(id.to_string()),
                Slot::EnemyBan => st.enemy_ban.push(id.to_string()),
                Slot::Avail => {}
            }
        }
        st
    }
}

/// Apply a click on champion index `i` according to the current mode.
fn click_champ(i: usize, mode: Mode, slots: &mut [Slot], pool: &mut HashSet<String>) {
    let id = CHAMPS[i].0;
    if mode == Mode::Pool {
        if pool.contains(id) {
            pool.remove(id);
        } else {
            pool.insert(id.to_string());
        }
    } else {
        slots[i] = apply_mode(slots[i], mode);
    }
}

fn set_theme(ctx: &mut Context) {
    let s = ctx.style_mut();
    s.window_rounding = 8.0;
    s.child_rounding = 6.0;
    s.frame_rounding = 6.0;
    s.grab_rounding = 5.0;
    s.popup_rounding = 6.0;
    s.window_padding = [14.0, 12.0];
    s.frame_padding = [9.0, 5.0];
    s.item_spacing = [8.0, 7.0];
    s.item_inner_spacing = [7.0, 5.0];
    s.window_border_size = 0.0;
    s.frame_border_size = 0.0;
    s.scrollbar_rounding = 8.0;
    let c = &mut s.colors;
    c[StyleColor::WindowBg as usize] = [0.065, 0.075, 0.10, 0.97];
    c[StyleColor::ChildBg as usize] = [0.09, 0.10, 0.135, 0.6];
    c[StyleColor::Text as usize] = [0.88, 0.90, 0.93, 1.0];
    c[StyleColor::TextDisabled as usize] = [0.50, 0.54, 0.60, 1.0];
    c[StyleColor::TitleBg as usize] = [0.065, 0.075, 0.10, 1.0];
    c[StyleColor::TitleBgActive as usize] = [0.10, 0.30, 0.31, 1.0];
    c[StyleColor::FrameBg as usize] = [0.14, 0.16, 0.21, 1.0];
    c[StyleColor::FrameBgHovered as usize] = [0.18, 0.28, 0.34, 1.0];
    c[StyleColor::FrameBgActive as usize] = [0.20, 0.40, 0.44, 1.0];
    c[StyleColor::Button as usize] = [0.16, 0.18, 0.23, 1.0];
    c[StyleColor::ButtonHovered as usize] = [0.20, 0.42, 0.46, 1.0];
    c[StyleColor::ButtonActive as usize] = [0.24, 0.52, 0.56, 1.0];
    c[StyleColor::Header as usize] = [0.16, 0.30, 0.33, 0.55];
    c[StyleColor::HeaderHovered as usize] = [0.20, 0.44, 0.48, 0.85];
    c[StyleColor::HeaderActive as usize] = [0.24, 0.54, 0.58, 1.0];
    c[StyleColor::CheckMark as usize] = ACCENT;
    c[StyleColor::SliderGrab as usize] = ACCENT;
    c[StyleColor::Separator as usize] = [0.20, 0.24, 0.30, 1.0];
    c[StyleColor::ScrollbarBg as usize] = [0.065, 0.075, 0.10, 0.6];
    c[StyleColor::ScrollbarGrab as usize] = [0.22, 0.26, 0.32, 1.0];
}

impl ImguiRenderLoop for Overlay {
    fn initialize<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        if let Ok(ttf) = std::fs::read(FONT_PATH) {
            let cfg = FontConfig {
                size_pixels: 19.0,
                glyph_ranges: FontGlyphRanges::korean(),
                oversample_h: 1,
                oversample_v: 1,
                ..FontConfig::default()
            };
            ctx.fonts().add_font(&[FontSource::TtfData {
                data: &ttf,
                size_pixels: 19.0,
                config: Some(cfg),
            }]);
            self.font_loaded = true;
        }
        set_theme(ctx);
    }

    fn message_filter(&self, io: &Io) -> MessageFilter {
        if !self.visible {
            return MessageFilter::empty();
        }
        let mut f = MessageFilter::empty();
        if io.want_capture_mouse {
            f |= MessageFilter::InputMouse;
        }
        if io.want_capture_keyboard {
            f |= MessageFilter::InputKeyboard;
        }
        f
    }

    fn render(&mut self, ui: &mut Ui) {
        let combo = key_down(VK_CONTROL) && key_down(VK_F10);
        if combo && !self.prev_combo {
            self.visible = !self.visible;
        }
        self.prev_combo = combo;
        // NOTE: the visibility (Ctrl+F10) gate is BELOW the data work — the reload /
        // live-banpick sync / recommendation export must keep running while the
        // window is hidden (the in-game display depends on them).

        self.frames += 1;
        if self.frames - self.last_reload_frame >= RELOAD_EVERY_FRAMES {
            self.last_reload_frame = self.frames;
            // refresh the save list + which one is selected (pin)
            self.saves = load_saves();
            self.sel_idx = sel_from_pin(&self.saves, &load_pin());
            let m = tsv_mtime();
            if m != self.tsv_mtime {
                self.tsv_mtime = m;
                let (t, src) = load_tables();
                self.tables = t;
                self.tsv_source = src;
                self.pool = self.tables.pool.clone();
            }
            // ── live banpick sync — the tfm2_bp_reader mod exports the board read
            // straight from the game's banpick screen; on change, snap the grid to
            // it. Manual clicks still work between snaps, and turning 자동연동 off
            // restores the fully-manual grid (mod absent → file never changes).
            if self.bp_auto {
                let bm = std::fs::metadata(shared("bp_live.txt")).and_then(|x| x.modified()).ok();
                if bm != self.bp_mtime {
                    // adopt every CHANGE; on the first sighting after startup only
                    // adopt a recent file (an old draft's leftovers would confuse).
                    let adopt = self.bp_mtime.is_some()
                        || bm
                            .and_then(|t| t.elapsed().ok())
                            .map(|e| e.as_secs() < 600)
                            .unwrap_or(false);
                    self.bp_mtime = bm;
                    if adopt {
                        if let Some(bp) = read_bp_live() {
                            let slots = &mut self.slots;
                            for s in slots.iter_mut() {
                                *s = Slot::Avail;
                            }
                            let mut mark = |ids: &[String], slot: Slot| {
                                for id in ids {
                                    if let Some(i) = CHAMPS.iter().position(|(cid, _)| *cid == id.as_str()) {
                                        slots[i] = slot;
                                    }
                                }
                            };
                            mark(&bp.my_ban, Slot::MyBan);
                            mark(&bp.enemy_ban, Slot::EnemyBan);
                            mark(&bp.my_pick, Slot::MyPick);
                            mark(&bp.enemy_pick, Slot::EnemyPick);
                            // auto phase: bans still open → recommend bans; else picks
                            if bp.ban_count > 0 {
                                self.rec_ban = bp.my_ban.len() + bp.enemy_ban.len() < bp.ban_count * 2;
                            }
                            // ── fearless series auto-tracking. The game attaches set
                            // replays to match_info only AFTER the match, so we track
                            // set boundaries ourselves: a completed board (5+5 picks)
                            // followed by a reset board with the SAME team pair = the
                            // next set; a new team pair = a new match (fresh series).
                            if bp.teams != self.bp_teams {
                                self.bp_teams = bp.teams.clone();
                                self.series_game = 1;
                                self.series_my.clear();
                                self.series_enemy.clear();
                                self.bp_full_board = None;
                                self.bp_first_pick = None;
                            } else if let Some((pm, pe)) = self.bp_full_board.take() {
                                let total_now = bp.my_pick.len() + bp.enemy_pick.len() + bp.my_ban.len() + bp.enemy_ban.len();
                                if total_now < pm.len() + pe.len() {
                                    // board reset after a finished draft → next set
                                    self.series_game += 1;
                                    self.series_my.extend(pm);
                                    self.series_enemy.extend(pe);
                                    self.bp_first_pick = None;
                                } // (not a reset → keep waiting; restored below)
                            }
                            // 선픽/후픽 감지 — 드래프트의 첫 픽 시점(전체 픽 1개)이
                            // 유일하게 모호하지 않은 순간: 그때 픽한 쪽이 선픽.
                            if bp.my_pick.len() + bp.enemy_pick.len() == 1 && self.bp_first_pick.is_none() {
                                self.bp_first_pick = Some(bp.my_pick.len() == 1);
                            }
                            if bp.my_pick.len() == 5 && bp.enemy_pick.len() == 5 {
                                self.bp_full_board = Some((bp.my_pick.clone(), bp.enemy_pick.clone()));
                            }
                            // ── fearless STYLE + recovery from the game's OWN locks
                            // (disabled cells minus bans/picks = exactly what I can't
                            // pick this set — works even if the overlay restarted).
                            if !bp.locked.is_empty() {
                                let enemy_only_locked = self
                                    .series_enemy
                                    .iter()
                                    .filter(|c| !self.series_my.contains(*c))
                                    .any(|c| bp.locked.contains(c));
                                self.fearless = if enemy_only_locked { Fearless::Hard } else { Fearless::Normal };
                                if self.series_my.is_empty() && self.series_enemy.is_empty() {
                                    // no tracked history (overlay restarted mid-series):
                                    // adopt the game's lock list as my restriction set.
                                    self.series_my = bp.locked.iter().cloned().collect();
                                    if self.series_game == 1 {
                                        self.series_game = 2;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // fearless: champions I may NOT pick this set (Normal=my prior picks; Hard=either
        // team's). Used for the grid tint + the lock label.
        let my_locked: HashSet<String> = match self.fearless {
            Fearless::Off => HashSet::new(),
            Fearless::Normal => self.series_my.clone(),
            Fearless::Hard => self.series_my.iter().chain(self.series_enemy.iter()).cloned().collect(),
        };
        // champions the ENEMY may NOT pick this set. Banning one is pointless (they can't
        // pick it anyway), so BAN recommendations exclude this set instead of mine.
        let enemy_locked: HashSet<String> = match self.fearless {
            Fearless::Off => HashSet::new(),
            Fearless::Normal => self.series_enemy.clone(),
            Fearless::Hard => self.series_my.iter().chain(self.series_enemy.iter()).cloned().collect(),
        };
        // PHASE-aware: pick recs drop what I can't pick; ban recs drop what the enemy
        // can't pick (so we never suggest banning an already-used, unpickable champ — and
        // in Normal fearless we DO still suggest banning MY used champs to deny the enemy).
        let rec_lock: &HashSet<String> = if self.rec_ban { &enemy_locked } else { &my_locked };
        let restrict = !self.pool.is_empty() || !rec_lock.is_empty();
        let effective: HashSet<String> = if !restrict {
            HashSet::new()
        } else if self.pool.is_empty() {
            CHAMPS.iter().map(|(id, _)| id.to_string()).filter(|id| !rec_lock.contains(id)).collect()
        } else {
            self.pool.iter().filter(|id| !rec_lock.contains(*id)).cloned().collect()
        };
        let pool_opt = if restrict { Some(&effective) } else { None };
        // fearless series "depletion": how many STRONG (S/A) champs are still pickable
        // per lane — so you don't burn a thin lane's only strong champ in an early game.
        let depletion: Option<[u32; 5]> = if self.fearless != Fearless::Off {
            let taken: HashSet<&str> = self
                .slots
                .iter()
                .enumerate()
                .filter(|(_, s)| **s != Slot::Avail)
                .map(|(i, _)| CHAMPS[i].0)
                .collect();
            let avail: HashSet<String> = CHAMPS
                .iter()
                .map(|(id, _)| id.to_string())
                .filter(|id| {
                    (self.pool.is_empty() || self.pool.contains(id))
                        && !my_locked.contains(id)
                        && !taken.contains(id.as_str())
                })
                .collect();
            Some(self.tables.strong_per_lane(&avail))
        } else {
            None
        };

        let view = recommend(&self.tables, &self.current_state(), false, pool_opt);
        // PICK phase → lane board (one slot per position); BAN phase stays a list.
        let lane_view = if self.rec_ban {
            None
        } else {
            Some(recommend_lanes(&self.tables, &self.current_state(), false, pool_opt, self.fearless != Fearless::Off))
        };

        // ── live recommendation export → recs.txt: the in-game grid borders (the
        // db_export mod tints recommended cells — teal=pick, violet=ban). Mirrors
        // exactly what this overlay currently recommends, so it moves with every
        // board change (auto-synced via bp_live.txt).
        {
            // entries: "<id>:<ko name>:<signed score>" — the mod renders rank glyphs,
            // names and scores in-game without needing its own champion table.
            let entry = |id: &str, ko: &str, score: f32| format!("{}:{}:{:+.0}", id, ko, score * 100.0);
            let mut recs: Vec<String> = Vec::new();
            let mut seen: Vec<String> = Vec::new();
            if let Some(lv) = lane_view.as_ref().filter(|lv| lv.has_pos) {
                if let Some(star) = lv.lanes.iter().find(|l| l.star) {
                    for c in star.cands.iter().take(3) {
                        recs.push(entry(&c.id, &c.ko, c.score));
                        seen.push(c.id.clone());
                    }
                }
                for l in lv.lanes.iter().filter(|l| !l.star && l.filled.is_none()) {
                    if let Some(c) = l.cands.first() {
                        if !seen.contains(&c.id) {
                            recs.push(entry(&c.id, &c.ko, c.score));
                            seen.push(c.id.clone());
                        }
                    }
                }
                recs.truncate(5);
            } else {
                recs.extend(view.rows.iter().take(5).map(|r| entry(&r.id, &r.ko, r.score)));
            }
            // swap-phase lane assignment: once MY 5 picks are in, export the engine's
            // optimal lane placement (the in-game header shows it during champion swap).
            let my_picks_done = self.slots.iter().filter(|s| **s == Slot::MyPick).count() >= 5;
            let swap = match lane_view.as_ref().filter(|lv| lv.has_pos && my_picks_done) {
                Some(lv) => lv
                    .lanes
                    .iter()
                    .filter_map(|l| l.filled.as_ref().map(|ko| format!("{}:{}", l.name, ko)))
                    .collect::<Vec<_>>()
                    .join(","),
                None => String::new(),
            };
            // template commentary ("A안"): compose Korean sentences from the factors
            // the engine already computed — shown in-game as an AI-coach message.
            // '|' separates lines (the kv file format can't hold raw newlines).
            let say: String = {
                let mut parts: Vec<String> = Vec::new();
                if !swap.is_empty() {
                    parts.push("픽이 끝났습니다. 전체 승률이 가장 높아지는 배치는 위 추천 배치입니다.".to_string());
                } else if let Some(lv) = lane_view.as_ref().filter(|lv| lv.has_pos) {
                    if let Some(star) = lv.lanes.iter().find(|l| l.star) {
                        if let Some(c) = star.cands.first() {
                            let mut s = format!("{} 라인을 먼저 확보하는 편이 좋아 보입니다. 1순위는 {}", star.name, c.ko);
                            if !c.tier.is_empty() {
                                s.push_str(&format!("({}티어)", c.tier));
                            }
                            s.push_str(&format!(", 평점 {:+.0}", c.score * 100.0));
                            if !c.label.is_empty() {
                                s.push_str(&format!(" — 근거: {}", c.label));
                            }
                            if !c.chg.is_empty() {
                                s.push_str(&format!(" (이번 패치 변경 {})", c.chg));
                            }
                            if c.low_conf {
                                s.push_str(". 다만 표본이 적어 확신은 낮습니다");
                            }
                            s.push('.');
                            parts.push(s);
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
                            if lv.tip != "" {
                                parts.push(format!("전략 메모: {}.", lv.tip));
                            }
                        }
                    }
                } else if let Some(r) = view.rows.first() {
                    let mut s = format!("밴 1순위는 {}", r.ko);
                    if !r.tier.is_empty() {
                        s.push_str(&format!("({}티어)", r.tier));
                    }
                    s.push_str(&format!(", 평점 {:+.0}", r.score * 100.0));
                    if !r.label.is_empty() {
                        s.push_str(&format!(" — 근거: {}", r.label));
                    }
                    if !r.chg.is_empty() {
                        s.push_str(&format!(" (이번 패치 변경 {})", r.chg));
                    }
                    s.push('.');
                    parts.push(s);
                    if view.rows.len() > 1 {
                        parts.push(format!(
                            "차순위 밴 후보: {}.",
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
                // ── 상대 조합 분석 (탱/힐 구성 → 한 줄 코멘트)
                let enemy_ids: Vec<&str> = CHAMPS
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| self.slots[*i] == Slot::EnemyPick)
                    .map(|(_, (id, _))| *id)
                    .collect();
                if !enemy_ids.is_empty() {
                    let (tanks, heals) = self.tables.comp_counts(&enemy_ids);
                    let mut s = format!("상대 조합 {}픽 — 탱커 {} · 힐러 {}", enemy_ids.len(), tanks, heals);
                    if tanks == 0 {
                        s.push_str(". 앞라인이 없어 돌진·암살 가치가 높습니다");
                    } else if heals >= 1 {
                        s.push_str(". 힐러가 있어 장기전 불리 — 폭딜이나 CC 연계를 고려하세요");
                    }
                    s.push('.');
                    parts.push(s);
                }
                // ── 이번 패치 변경 챔프 (▲버프 ▼너프 ⚡리워크)
                let changed: Vec<String> = CHAMPS
                    .iter()
                    .filter_map(|(id, ko)| {
                        let m = self.tables.chg_mark(id);
                        if m.is_empty() { None } else { Some(format!("{}{}", ko, m)) }
                    })
                    .take(10)
                    .collect();
                if !changed.is_empty() {
                    parts.push(format!("이번 패치 변경: {} — 변경 챔프는 통계가 리셋되어 티어가 비어있을 수 있습니다.", changed.join(" ")));
                }
                // ── 피어리스 현황
                if self.fearless != Fearless::Off {
                    parts.push(format!(
                        "피어리스 {}세트({}) — 사용 불가: 내 {}챔 · 상대 {}챔.",
                        self.series_game,
                        if self.fearless == Fearless::Hard { "하드" } else { "노멀" },
                        self.series_my.len(),
                        self.series_enemy.len()
                    ));
                }
                // ── 데이터 신뢰도
                parts.push(format!(
                    "데이터: 현재 패치 {}판{}",
                    view.matches,
                    if view.matches < 50 { " — 콜드스타트 구간, 이론 비중이 큽니다." } else { "." }
                ));
                parts.join("|")
            };
            let body = format!(
                "phase={}\nrec={}\nswap={}\nsay={}\n",
                if self.rec_ban { "BAN" } else { "PICK" },
                recs.join(","),
                swap,
                say
            );
            if body != self.last_recs {
                let tmp = shared("recs.txt.tmp");
                if std::fs::write(&tmp, &body).is_ok() && std::fs::rename(&tmp, shared("recs.txt")).is_ok() {
                    self.last_recs = body;
                }
            }
        }

        // everything below is WINDOW DRAWING — skippable while hidden (Ctrl+F10);
        // the data pipeline above keeps feeding the in-game display regardless.
        if !self.visible {
            return;
        }
        let font_loaded = self.font_loaded;
        let tsv_source = self.tsv_source.clone();
        let conf = self.tables.confidence();
        let matches = self.tables.matches;
        let save_name = self.tables.save_name.clone();
        let pool_n = self.pool.len();
        let auto_pool = self.tables.pool.clone();
        // per-champ meta tier (overall), index-aligned with CHAMPS — shown on every
        // grid button so the draft grid doubles as a live tier table.
        let grid_tiers: Vec<&'static str> = CHAMPS.iter().map(|(id, _)| self.tables.tier_display(id, None)).collect();
        // patch-change tag per champ (▲버프/▼너프/⚡변경) — shown on the grid + a legend
        // line, so a blank/reset tier reads as "patched, stats refilling" not "no data".
        let grid_chg: Vec<&'static str> = CHAMPS.iter().map(|(id, _)| self.tables.chg_mark(id)).collect();
        let any_chg = self.tables.any_chg();
        let mut mode = self.mode;
        let mut rec_ban = self.rec_ban;
        let mut bp_auto = self.bp_auto;
        // live-sync status: seconds since bp_live.txt last changed (None = no file)
        let bp_age: Option<u64> = std::fs::metadata(shared("bp_live.txt"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs());
        let mut slots = self.slots.clone();
        let mut pool = self.pool.clone();
        let mut search = self.search.clone();
        let mut chosung = self.chosung;
        let saves = self.saves.clone();
        let mut sel_idx = self.sel_idx;
        let mut new_pin: Option<String> = None;
        let mut ui_scale = self.ui_scale;
        let mut btn_w = self.btn_w;
        let mut bg_alpha = self.bg_alpha;
        let mut show_settings = self.show_settings;
        let mut show_help = self.show_help;
        let mut pick_order = self.pick_order;
        let mut reset = false;
        let mut do_refresh = false;
        // fearless UI state + display
        let mut fearless = self.fearless;
        let mut next_set = false;
        let mut clear_series = false;
        let locked_me = my_locked.clone(); // champs I can't pick this set (grid tint)
        let my_locked_n = my_locked.len();
        let enemy_locked_n = match self.fearless {
            Fearless::Off => 0,
            Fearless::Normal => self.series_enemy.len(),
            Fearless::Hard => my_locked.len(),
        };
        let series_game = self.series_game;

        ui.window("TFM2 드래프트 추천기")
            .size([600.0, 880.0], Condition::FirstUseEver)
            .position([24.0, 24.0], Condition::FirstUseEver)
            .bg_alpha(bg_alpha)
            .build(|| {
                ui.set_window_font_scale(ui_scale);
                if !font_loaded {
                    ui.text_colored(RED, "malgun.ttf load failed");
                }

                // ── header: SAVE PICKER (newest is not always the one you loaded) ──
                ui.text_colored(GREY, "세이브");
                ui.same_line();
                ui.set_next_item_width(248.0);
                let mut items: Vec<String> = Vec::with_capacity(saves.len() + 1);
                items.push("자동 (최신 세이브)".to_string());
                for (_f, disp) in &saves {
                    items.push(disp.clone());
                }
                if ui.combo("##save", &mut sel_idx, &items, |s| std::borrow::Cow::Borrowed(s.as_str())) {
                    new_pin = Some(if sel_idx == 0 {
                        String::new()
                    } else {
                        saves.get(sel_idx - 1).map(|(f, _)| f.clone()).unwrap_or_default()
                    });
                }
                // fresh/under-sampled patch → the new meta isn't measured yet; warn loudly.
                let fresh = matches > 0 && conf < 1.0;
                let conf_label = if matches == 0 {
                    "데이터 없음 · 이론 추천".to_string()
                } else if fresh {
                    format!("새 패치 {}판 · 데이터 부족(이론 {}%) · 무난한 픽 권장", matches, ((1.0 - conf) * 100.0) as i32)
                } else {
                    format!("현재패치 {}판 · 통계", matches)
                };
                let pool_label = if pool_n == 0 { "풀 전체".into() } else { format!("풀 {}/60", pool_n) };
                let loaded = if save_name.is_empty() { tsv_source.clone() } else { save_name.clone() };
                ui.text_colored(GREY, format!("로드됨 {}", loaded));
                // gold when the patch is fresh/low-sample (recs are theory-heavy), else grey
                ui.text_colored(
                    if fresh || matches == 0 { GOLD } else { GREY },
                    format!("{}  ·  {}  ·  Ctrl+F10", conf_label, pool_label),
                );
                ui.separator();

                // ── phase (big) + mode ───────────────────────────────────────
                ui.text_colored(WHITE, "단계");
                ui.same_line();
                ui.radio_button("밴", &mut rec_ban, true);
                ui.same_line();
                ui.radio_button("픽", &mut rec_ban, false);
                // manual pick-order hint (선픽=먼저 픽 → 강픽/플렉스+위협밴, 후픽=나중 → 카운터)
                ui.same_line();
                ui.text_colored(GREY, "   픽순서");
                ui.same_line();
                ui.radio_button("자동", &mut pick_order, 0i8);
                ui.same_line();
                ui.radio_button("선픽", &mut pick_order, 1i8);
                ui.same_line();
                ui.radio_button("후픽", &mut pick_order, 2i8);
                // marking options follow the PHASE: ban phase shows only ban-marks, pick
                // phase only pick-marks (+ 지움/풀 always). Keep the mode valid for the phase.
                if rec_ban && (mode == Mode::MyPick || mode == Mode::EnemyPick) {
                    mode = Mode::MyBan;
                } else if !rec_ban && (mode == Mode::MyBan || mode == Mode::EnemyBan) {
                    mode = Mode::MyPick;
                }
                ui.text_colored(GREY, "마킹");
                ui.same_line();
                if rec_ban {
                    ui.radio_button("내 밴", &mut mode, Mode::MyBan);
                    ui.same_line();
                    ui.radio_button("상대 밴", &mut mode, Mode::EnemyBan);
                } else {
                    ui.radio_button("내 픽", &mut mode, Mode::MyPick);
                    ui.same_line();
                    ui.radio_button("상대 픽", &mut mode, Mode::EnemyPick);
                }
                ui.same_line();
                ui.radio_button("지움", &mut mode, Mode::Clear);
                ui.same_line();
                ui.radio_button("풀", &mut mode, Mode::Pool);
                // make the current click target unmistakable (prevents mis-marking)
                ui.same_line();
                let (mlabel, mcol) = mode_badge(mode);
                ui.text_colored(mcol, format!("« 클릭 = {}", mlabel));

                if ui.button("픽/밴 초기화") {
                    reset = true;
                }
                ui.same_line();
                if ui.button("통계 새로고침") {
                    do_refresh = true;
                }
                ui.same_line();
                if ui.button("설정") {
                    show_settings = !show_settings;
                }
                ui.same_line();
                if ui.button("도움말") {
                    show_help = !show_help;
                }
                ui.same_line();
                ui.checkbox("자동연동", &mut bp_auto);
                if bp_auto {
                    ui.same_line();
                    match bp_age {
                        Some(a) if a < 60 => ui.text_colored(GREEN, "●"),
                        Some(_) => ui.text_colored(GREY, "●"),
                        None => ui.text_colored(GREY, "(밴픽리더 없음)"),
                    }
                    if ui.is_item_hovered() {
                        ui.tooltip_text("게임 밴픽 화면을 자동으로 읽어 보드에 반영 (tfm2_bp_reader 모드 필요)\n● 초록=최근 갱신 · 회색=대기");
                    }
                }
                if mode == Mode::Pool {
                    ui.same_line();
                    if ui.button("풀 전체") {
                        pool = CHAMPS.iter().map(|(id, _)| id.to_string()).collect();
                    }
                    ui.same_line();
                    if ui.button("자동") {
                        pool = auto_pool.clone();
                    }
                }

                // ── fearless (series) rule ───────────────────────────────────
                ui.text_colored(GREY, "피어리스");
                ui.same_line();
                ui.radio_button("없음", &mut fearless, Fearless::Off);
                ui.same_line();
                ui.radio_button("피어리스", &mut fearless, Fearless::Normal);
                ui.same_line();
                ui.radio_button("하드", &mut fearless, Fearless::Hard);
                if fearless != Fearless::Off {
                    ui.same_line();
                    ui.text_colored(ACCENT, format!("{}세트", series_game));
                    ui.same_line();
                    if ui.button("다음 세트 »") {
                        next_set = true;
                    }
                    ui.same_line();
                    if ui.button("시리즈 초기화") {
                        clear_series = true;
                    }
                    let lock_label = if fearless == Fearless::Hard {
                        format!("금지(공통) {}", my_locked_n)
                    } else {
                        format!("금지 내 {}  ·  상대 {}", my_locked_n, enemy_locked_n)
                    };
                    ui.text_colored(GREY, format!("이전 세트 사용 챔프 » 추천·그리드 제외  ·  {}", lock_label));
                    if let Some(d) = depletion {
                        ui.text_colored(GOLD, format!("남은 강챔(S/A)  탑 {} · 정글 {} · 미드 {} · 바텀 {} · 서폿 {}   (적은 라인은 아껴두기)", d[0], d[1], d[2], d[3], d[4]));
                    }
                }
                ui.separator();

                // ── current board ────────────────────────────────────────────
                let join = |v: &[String]| if v.is_empty() { "-".to_string() } else { v.join(", ") };
                // compact board: parallel 픽 / 밴 rows, each 내 · 상대 (was 3 lines → 2)
                ui.text_colored(GREY, "픽  내 ");
                ui.same_line();
                ui.text_colored(GREEN, join(&view.my_pick));
                ui.same_line();
                ui.text_colored(GREY, " · 상대 ");
                ui.same_line();
                ui.text_colored(RED, join(&view.opp_pick));
                ui.text_colored(GREY, "밴  내 ");
                ui.same_line();
                ui.text_colored(WHITE, join(&view.my_ban));
                ui.same_line();
                ui.text_colored(GREY, " · 상대 ");
                ui.same_line();
                ui.text_colored(WHITE, join(&view.opp_ban));
                ui.separator();

                // ── recommendations ──────────────────────────────────────────
                if let Some(lv) = lane_view.as_ref().filter(|lv| lv.has_pos) {
                    // PICK: lane board — one slot per position, click a champ = my pick
                    // ▶ prominent #1 recommendation = the starred lane's top candidate
                    if let Some((tier, role, name, chg, lane_name, label)) = lv
                        .lanes
                        .iter()
                        .find(|l| l.star)
                        .and_then(|l| l.cands.first().map(|c| (c.tier, c.role, c.ko.clone(), c.chg, l.name, c.label)))
                    {
                        let badge = if tier.is_empty() { String::new() } else { format!("[{}] ", tier) };
                        let rsp = if role.is_empty() { String::new() } else { format!("{} ", role) };
                        ui.text_colored(tier_color(tier), format!("» 지금 추천: {}{}{}{}  ·  {} 라인  ({})", badge, rsp, name, chg, lane_name, label));
                        ui.spacing();
                    }
                    ui.text_colored(ACCENT, "추천 픽 (라인별)");
                    ui.same_line();
                    ui.text_colored(GREY, format!("{} (내픽 {}/5) · {}", lv.timing, lv.picks_made, lv.tip));
                    ui.same_line();
                    let mut legend = "?=표본부족 · »=추천 라인 · 티어 S~D=현 패치 승률".to_string();
                    if any_chg {
                        legend.push_str(" · ▲버프 ▼너프 ⚡변경(통계 리셋중)");
                    }
                    ui.text_colored(GREY, legend);
                    ui.spacing();
                    for lane in &lv.lanes {
                        if let Some(champ) = &lane.filled {
                            ui.text_colored(GREY, lane.name);
                            ui.same_line();
                            ui.text_colored(GREEN, format!("[내픽] {}", champ));
                            continue;
                        }
                        let lane_col = if lane.star { ACCENT } else { WHITE };
                        ui.text_colored(lane_col, format!("{}{}", if lane.star { "» " } else { "   " }, lane.name));
                        for (ci, c) in lane.cands.iter().enumerate() {
                            ui.same_line();
                            // button text colored by META TIER (S red…D grey); order still = rank
                            let _tc = ui.push_style_color(StyleColor::Text, tier_color(c.tier));
                            let rp = if c.role.is_empty() { String::new() } else { format!("{} ", c.role) };
                            if ui.button(format!("{}{}{} {:+.0}{}##L{}_{}", rp, c.ko, c.chg, c.score * 100.0, if c.low_conf { "?" } else { "" }, lane.name, c.id)) {
                                if let Some(idx) = CHAMPS.iter().position(|(cid, _)| *cid == c.id) {
                                    slots[idx] = apply_mode(slots[idx], Mode::MyPick);
                                }
                            }
                        }
                        if lane.cands.is_empty() {
                            ui.same_line();
                            ui.text_colored(GREY, "후보 없음");
                        }
                        if let Some(cof) = &lane.counter_of {
                            ui.same_line();
                            ui.text_colored(GOLD, format!("[카운터:{}]", cof));
                        }
                    }
                } else {
                    // BAN (or no position data): flat ranked list — click a row to mark
                    // ▶ prominent #1 recommendation
                    if let Some(r) = view.rows.first() {
                        let badge = if r.tier.is_empty() { String::new() } else { format!("[{}] ", r.tier) };
                        let rsp = if r.role.is_empty() { String::new() } else { format!("{} ", r.role) };
                        let what = if view.phase_ban { "추천 밴" } else { "추천 픽" };
                        ui.text_colored(tier_color(r.tier), format!("» 지금 {}: {}{}{}{}  ({})", what, badge, rsp, r.ko, r.chg, r.label));
                        ui.spacing();
                    }
                    let head = if view.phase_ban { "추천 밴" } else { "추천 픽" };
                    ui.text_colored(ACCENT, format!("{}  ", head));
                    ui.same_line();
                    let mut legend = "행 클릭=마킹 · ?=표본부족 · 티어 S~D=현 패치 승률".to_string();
                    if any_chg {
                        legend.push_str(" · ▲버프 ▼너프 ⚡변경(통계 리셋중)");
                    }
                    ui.text_colored(GREY, legend);
                    ui.spacing();
                    for (i, r) in view.rows.iter().enumerate() {
                        // each row colored by its META TIER (S red … D grey)
                        let _t = ui.push_style_color(StyleColor::Text, tier_color(r.tier));
                        let mark = format!("{}{}", r.chg, if r.low_conf { " ?" } else { "" });
                        let rp = if r.role.is_empty() { "  ".to_string() } else { format!("{} ", r.role) };
                        let label = format!("{:>2}. {}{:<11}{:+5.0}   {}{}", i + 1, rp, r.ko, r.score * 100.0, r.label, mark);
                        if ui.selectable(label) {
                            if let Some(idx) = CHAMPS.iter().position(|(cid, _)| *cid == r.id) {
                                click_champ(idx, mode, &mut slots, &mut pool);
                            }
                        }
                    }
                }
                ui.separator();

                // ── champion grid (search: 초성 click + English id) ──────────
                // 초성 quick-filter — click instead of typing Korean (the overlay
                // can't take Korean IME input). The text box matches the English id.
                ui.text_colored(GREY, "초성");
                for gi in 0..CHOSUNG.len() {
                    if gi != 7 {
                        ui.same_line(); // wrap to a 2nd row at the halfway point
                    }
                    let active = chosung == Some(gi as u8);
                    let _c = active.then(|| ui.push_style_color(StyleColor::Button, ACCENT));
                    let _t = active.then(|| ui.push_style_color(StyleColor::Text, [0.06, 0.08, 0.10, 1.0]));
                    if ui.button_with_size(format!("{}##cs{}", CHOSUNG[gi], gi), [34.0, 0.0]) {
                        chosung = if active { None } else { Some(gi as u8) };
                    }
                }
                ui.same_line();
                if ui.button("전체##csall") {
                    chosung = None;
                }
                ui.text_colored(GREY, "검색(영문)");
                ui.same_line();
                ui.set_next_item_width(170.0);
                ui.input_text("##q", &mut search).build();
                ui.same_line();
                ui.text_colored(GREY, "예: monk, archer · 한글은 초성 · 챔프앞 S~D=현 패치 티어");
                if !search.is_empty() {
                    ui.same_line();
                    if ui.button("×") {
                        search.clear();
                    }
                }
                let q = search.to_lowercase();
                let mut shown = 0usize;
                for i in 0..CHAMPS.len() {
                    let (id, ko) = CHAMPS[i];
                    if !q.is_empty() && !ko.contains(search.as_str()) && !id.to_lowercase().contains(&q) {
                        continue;
                    }
                    if let Some(cs) = chosung {
                        if chosung_group(ko) != Some(cs) {
                            continue;
                        }
                    }
                    if shown % COLS != 0 {
                        ui.same_line();
                    }
                    shown += 1;
                    let out_of_pool = !pool.is_empty() && !pool.contains(id);
                    let mut c = slot_colors(slots[i]);
                    if out_of_pool {
                        for col in c.iter_mut() {
                            col[0] *= 0.35;
                            col[1] *= 0.35;
                            col[2] *= 0.35;
                        }
                    }
                    // fearless: champion used in an earlier set → I can't pick it (magenta)
                    if slots[i] == Slot::Avail && locked_me.contains(id) {
                        c = [[0.30, 0.13, 0.36, 1.0], [0.40, 0.18, 0.46, 1.0], [0.46, 0.22, 0.52, 1.0]];
                    }
                    let _b = ui.push_style_color(StyleColor::Button, c[0]);
                    let _h = ui.push_style_color(StyleColor::ButtonHovered, c[1]);
                    let _a = ui.push_style_color(StyleColor::ButtonActive, c[2]);
                    // tier prefix (S/A/B/C/D) → the grid reads as a live tier table;
                    // ▲/▼/⚡ = changed in the current patch (stats resetting).
                    let tp = if grid_tiers[i].is_empty() { String::new() } else { format!("{}·", grid_tiers[i]) };
                    if ui.button_with_size(format!("{}{}{}##{}", tp, grid_chg[i], ko, i), [btn_w * ui_scale, 0.0]) {
                        click_champ(i, mode, &mut slots, &mut pool);
                    }
                }
            });

        // ── separate settings window ─────────────────────────────────────────
        if show_settings {
            ui.window("설정")
                .size([330.0, 0.0], Condition::FirstUseEver)
                .position([650.0, 40.0], Condition::FirstUseEver)
                .bg_alpha(bg_alpha)
                .build(|| {
                    ui.set_window_font_scale(ui_scale);
                    ui.text_colored(GREY, "모양 설정 (자동 저장)");
                    ui.spacing();
                    ui.text_colored(WHITE, "글씨 크기");
                    ui.set_next_item_width(230.0);
                    ui.slider("##fs", 0.7f32, 2.2f32, &mut ui_scale);
                    ui.text_colored(WHITE, "버튼 너비");
                    ui.set_next_item_width(230.0);
                    ui.slider("##bw", 60.0f32, 170.0f32, &mut btn_w);
                    ui.text_colored(WHITE, "배경 진하기");
                    ui.set_next_item_width(230.0);
                    ui.slider("##ba", 0.35f32, 1.0f32, &mut bg_alpha);
                    ui.spacing();
                    ui.separator();
                    if ui.button("기본값으로") {
                        ui_scale = 1.0;
                        btn_w = BTN_W;
                        bg_alpha = 0.97;
                    }
                    ui.same_line();
                    if ui.button("닫기") {
                        show_settings = false;
                    }
                });
        }

        if show_help {
            ui.window("도움말 — 사용법")
                .size([410.0, 0.0], Condition::FirstUseEver)
                .position([650.0, 40.0], Condition::FirstUseEver)
                .bg_alpha(bg_alpha)
                .build(|| {
                    ui.set_window_font_scale(ui_scale);
                    ui.text_colored(ACCENT, "이렇게 쓰세요");
                    ui.spacing();
                    ui.text_colored(WHITE, "1. 단계 고르기 (밴/픽)");
                    ui.text_colored(GREY, "   지금 밴 차례면 [밴], 픽 차례면 [픽].");
                    ui.text_colored(GREY, "   고른 단계의 추천이 맨 위에 떠요.");
                    ui.text_colored(WHITE, "2. 진행되는 밴/픽을 클릭으로 표시");
                    ui.text_colored(GREY, "   아래 챔프 그리드에서 클릭 → 마킹됨.");
                    ui.text_colored(GREY, "   '내 밴/상대 밴' 라디오로 누구 건지 선택.");
                    ui.text_colored(WHITE, "3. 맨 위 '지금 추천'을 참고해 픽/밴");
                    ui.spacing();
                    ui.separator();
                    ui.text_colored(ACCENT, "화면 보는 법");
                    ui.text_colored(GREY, "· 티어 S(빨강) > A > B > C > D(회색) = 현 패치 강함");
                    ui.text_colored(GREY, "· 역할 힐 / 탱 / 브(브루저) / 딜 = 실제 데이터");
                    ui.text_colored(GREY, "· 숫자(+7 등) = 추천 점수 (높을수록 좋음)");
                    ui.text_colored(GREY, "· ? = 표본 적어 신뢰 낮음");
                    ui.text_colored(GREY, "· 어두운 챔프 = 내 풀에 없음(못 뽑음)");
                    ui.spacing();
                    ui.separator();
                    ui.text_colored(ACCENT, "단축키 / 찾기");
                    ui.text_colored(GREY, "· Ctrl+F10 = 오버레이 켜기/끄기");
                    ui.text_colored(GREY, "· 초성 버튼(ㄱㄴㄷ) 또는 영문 검색으로 챔프 찾기");
                    ui.text_colored(GREY, "· '픽/밴 초기화' = 보드 비우기");
                    ui.spacing();
                    ui.text_colored(GREY, "통계는 판수가 쌓일수록 정확해집니다. 참고용으로!");
                    ui.spacing();
                    if ui.button("닫기##help") {
                        show_help = false;
                    }
                });
        }

        if reset {
            for s in &mut slots {
                *s = Slot::Avail;
            }
        }
        if next_set {
            // bank this set's PICKS into the series history, then clear the board.
            for (i, (id, _)) in CHAMPS.iter().enumerate() {
                match slots[i] {
                    Slot::MyPick => {
                        self.series_my.insert(id.to_string());
                    }
                    Slot::EnemyPick => {
                        self.series_enemy.insert(id.to_string());
                    }
                    _ => {}
                }
            }
            for s in &mut slots {
                *s = Slot::Avail;
            }
            self.series_game += 1;
        }
        if clear_series {
            self.series_my.clear();
            self.series_enemy.clear();
            self.series_game = 1;
        }
        self.fearless = fearless;
        self.mode = mode;
        self.rec_ban = rec_ban;
        self.slots = slots;
        self.pool = pool;
        self.search = search;
        self.chosung = chosung;
        self.sel_idx = sel_idx;
        if let Some(p) = new_pin {
            save_pin(&p); // launcher picks this up → regen for the chosen save
        }
        if (ui_scale, btn_w, bg_alpha, bp_auto) != (self.ui_scale, self.btn_w, self.bg_alpha, self.bp_auto) {
            save_settings(ui_scale, btn_w, bg_alpha, bp_auto);
        }
        self.bp_auto = bp_auto;
        self.ui_scale = ui_scale;
        self.btn_w = btn_w;
        self.bg_alpha = bg_alpha;
        self.show_settings = show_settings;
        self.show_help = show_help;
        self.pick_order = pick_order;
        if do_refresh {
            // signal the launcher to FORCE a full stats regen (ignore caches) — just
            // re-reading the TSV only reloads whatever the launcher last published.
            // The launcher consumes the file on its next tick (≤8s) and the auto-
            // reloader picks up the republished TSV when its mtime changes.
            let _ = std::fs::write(shared("force_regen.txt"), "1");
            let (t, src) = load_tables();
            self.tables = t;
            self.tsv_source = src;
            self.tsv_mtime = tsv_mtime();
            self.pool = self.tables.pool.clone();
        }
    }
}

hudhook!(ImguiOpenGl3Hooks, Overlay::new());
