//! Display on the game's banpick screen. Rule (ROADMAP §1 G4): game-owned nodes
//! are never edited — everything shown lives in nodes we spawn, and all of them
//! except the panel itself and its buttons are `ignore_event`, so clicks reach
//! the game.
//!   per tile — `tfm2dc_ring`: recommendation outline (teal pick / violet ban,
//!              thicker for #1 — v3.0 colors); `tfm2dc_tag`: rank glyph + our
//!              tier letter in the tile's free top-left corner (the game's ban /
//!              order / fearless marks sit top-right, its own `#tier` at x6 y68).
//!              No "?" on an unstable letter — next to a letter in these fonts it
//!              reads as a broken glyph (2026-10-27 test); such letters are drawn
//!              dimmed instead, and only while some letters are stable.
//!   panel    — `main.tfm2dc_panel`: a window in the look of the game's
//!              coach-dialogue popup: top 5, notes, status. ◀ ▶ in its header
//!              move it, Tab folds it (reopens it when closed), X hides it until
//!              the next banpick screen; the spot and fold choices persist
//!              across sets. A plain `color` box: the game's `draggable_popup`
//!              runner drags only by a handle height (+ min size, resize border)
//!              that game code writes into each of its popups after building them
//!              itself, which no property
//!              or SDK call reaches, so a mod-spawned one never moves; it also
//!              drew its fill see-through (2026-10-27 tests).
//! Placement (0.6.5 layout, 1920×1080): no area is free, so the panel covers
//! INFORMATION, never controls — during the draft champion_info (x310–1610,
//! y690–970: a row of eight stat cells over three skill boxes) at its left end,
//! centre or right end (◀ ▶), by default the end AWAY from the player's own
//! coach, whose dialogue toasts stack up from their side's bottom corner (red:
//! x≈1388–1908, y≈700–950): blue → the right end (ult box x1183–1610 + magic
//! resistance / range / move speed cells), red → its mirror x316–756 (first
//! skill box + attack / spell power / attack speed cells). In the swap phase
//! `main.swap` covers the centre with its coach / confirm buttons at y903–970,
//! so the panel moves over the OPPONENT's table instead, by default at its right
//! end (stat columns).
//! A node is written only when its desired state differs from what we applied.
//! `.ui` grammar and property values are the ones the game's own templates use
//! (0.6.5 bundle): `#child:kind`, a bare style line, `size`/`align_*` on labels,
//! `line_height` + the label's own wrapping for multi-line text, and on `color`
//! runners with `stroke` the `color` is the OUTLINE and `back_color` the fill.

use crate::diag;
use crate::recommender::{Output, RecRow};
use mod_api_stable::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const PANEL: &str = "main.tfm2dc_panel";
const BODY: &str = "main.tfm2dc_panel.tfm2dc_body";
const CLOSE: &str = "main.tfm2dc_panel.tfm2dc_close";
const TITLE: &str = "main.tfm2dc_panel.tfm2dc_head.tfm2dc_title";
const LEFT: &str = "main.tfm2dc_panel.tfm2dc_left";
const RIGHT: &str = "main.tfm2dc_panel.tfm2dc_right";
const RANK_GLYPH: [&str; 5] = ["①", "②", "③", "④", "⑤"];
/// v3.0 REC_PICK_COLOR [0.22, 0.84, 0.70] / REC_BAN_COLOR [0.78, 0.40, 0.95]
const PICK_COL: &str = "#38d6b3";
const BAN_COL: &str = "#c766f2";
const PANEL_W: i32 = 440;
const PANEL_H: i32 = 274;
const HEAD_H: i32 = 34;
/// panel left edge per ◀ ▶ spot: champion_info's left end, centre, right end
/// (6px in from its x310 / x1610 edges — over its first / middle / last skill box)
const RIGHT_X: i32 = 1164;
const SPOT_X: [i32; 3] = [1920 - RIGHT_X - PANEL_W, (1920 - PANEL_W) / 2, RIGHT_X];
/// panel top edge: draft (bottom at champion_info's bottom, y970) / swap over the
/// red table (bottom 4px above the confirm buttons) / swap over the blue table
const TOP_DRAFT: i32 = 970 - PANEL_H;
const TOP_SWAP_LOW: i32 = 899 - PANEL_H;
const TOP_SWAP_HIGH: i32 = 64;
const DIM_LEGEND: &str = "흐린 티어 글자: 표본이 적어 아직 불확실한 티어";
/// text budgets in half-em units (ASCII 1, other 2): row label 384px @ size 14,
/// notes label 416px @ size 13 × 4 lines — with margin for the label's word wrap
const ROW_UNITS: usize = 52;
const NOTE_UNITS: usize = 56;
const NOTE_LINES: usize = 4;

/// What the frame's reading + recommendation produced, for the stamp pass.
#[derive(Clone, Default)]
pub(crate) struct View {
    pub out: Option<Arc<Output>>,
    pub status: String,
    pub tiles: Vec<String>,
    pub taken: HashSet<String>,
    /// my side (None = unresolved) — picks the panel's default spot
    pub my_blue: Option<bool>,
    /// the game's own champion names (board::Read::names)
    pub names: Arc<HashMap<String, String>>,
}

#[derive(Clone, PartialEq)]
enum Ring {
    Off,
    On { ban: bool, first: bool },
}

#[derive(Clone, PartialEq)]
enum Tag {
    Off,
    On { text: String, fill: String, ink: String },
}

#[derive(Clone, PartialEq)]
struct TileLook {
    ring: Ring,
    tag: Tag,
}

#[derive(Default)]
struct UiState {
    /// last applied look per tile; absent = nodes not (yet) verified
    tiles: HashMap<String, TileLook>,
    /// tiles whose spawn failed → retried only every 120 frames
    tile_fail: HashMap<String, u64>,
    panel_ok: bool,
    panel_kind: &'static str,
    panel_visible: Option<bool>,
    applied_fold: Option<bool>,
    applied_pos: Option<(i32, i32)>,
    /// ◀ ▶ spawned and bound; their greyed state (at the left / right end) applied
    arrows_ok: bool,
    applied_ends: Option<(bool, bool)>,
    title: Option<String>,
    rows: [Option<(String, String)>; 5],
    say: Option<String>,
    frame: u64,
    spawns: usize,
    spawn_fails: usize,
    set_fails: usize,
    icon_fails: usize,
    click: String,
    tabs: usize,
    keys: Vec<String>,
    /// texts read back unequal to what was set (first example kept)
    readback_bad: usize,
    readback_first: String,
}

static UI: Mutex<Option<UiState>> = Mutex::new(None);
static USER_CLOSED: AtomicBool = AtomicBool::new(false);
static FOLDED: AtomicBool = AtomicBool::new(false);
/// the player's ◀ ▶ spot (index into SPOT_X; usize::MAX = none yet → side
/// default), session-wide like the fold choice
static USER_SPOT: AtomicUsize = AtomicUsize::new(usize::MAX);
/// ◀ ▶ clicks not yet applied (−1 / +1 each)
static SPOT_STEP: AtomicI32 = AtomicI32::new(0);
static ARROW_CLICKS: AtomicUsize = AtomicUsize::new(0);
static CLOSE_CLICKS: AtomicUsize = AtomicUsize::new(0);

/// Left the banpick screen: our nodes died with it; a closed panel reopens next time.
pub(crate) fn reset() {
    if let Ok(mut g) = UI.lock() {
        *g = None;
    }
    USER_CLOSED.store(false, Ordering::Relaxed);
}

fn units(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

fn clip(s: &str, max: usize) -> String {
    if units(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = if c.is_ascii() { 1 } else { 2 };
        if used + w + 3 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push_str("...");
    out
}

/// Notes that fit the label's box, in the recommender's order (the label wraps
/// by itself; this only keeps the total inside its height).
fn fit_notes(notes: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut used = 0;
    for n in notes {
        let need = (units(n) + NOTE_UNITS - 1) / NOTE_UNITS;
        if used + need > NOTE_LINES {
            if used < NOTE_LINES {
                out.push(clip(n, (NOTE_LINES - used) * NOTE_UNITS));
            }
            break;
        }
        out.push(n.clone());
        used += need;
    }
    out.join("\n")
}

fn panel_source((x, top): (i32, i32)) -> String {
    let mut rows = String::new();
    for i in 0..5 {
        rows.push_str(&format!(
            r#"
    #tfm2dc_r{i}:empty {{
      x: 12px;
      y: {y}px;
      width: 416px;
      height: 28px;
      ignore_event: true;
      #tfm2dc_i:image {{
        width: 24px;
        height: 24px;
        y: 2px;
        visible: false;
        ignore_event: true;
      }}
      #tfm2dc_t:label {{
        @"asset/base/style/main#label";
        x: 32px;
        width: 384px;
        height: 28px;
        size: 14;
        align_y: Center;
        ignore_event: true;
        text: "";
      }}
    }}"#,
            i = i,
            y = i * 30
        ));
    }
    format!(
        r#"tfm2dc_panel:color {{
  width: {w}px;
  height: {h}px;
  x: {x}px;
  y: {top}px;
  stroke: 1;
  color: #4a4c56ff;
  back_color: #161721ff;
  rounding: Uniform {{
    rounding: 8;
  }}
  #tfm2dc_head:color {{
    width: 100%;
    height: {head}px;
    ignore_event: true;
    color: #0f1016ff;
    rounding: Individual {{
      top_left: 8;
      top_right: 8;
    }}
    #tfm2dc_title:label {{
      @"asset/base/style/main#bold_label";
      x: 12px;
      anchor_y: 0.5;
      pivot_y: 0.5;
      width: 300px;
      height: 24px;
      size: 14;
      align_y: Center;
      ignore_event: true;
      text: "AI 추천기";
    }}
  }}
  #tfm2dc_close:button {{
    width: 14px;
    height: 14px;
    anchor_x: 1;
    pivot_x: 1;
    x: -12px;
    y: 10px;
    source: "asset/base/ui/icons/cross";
    color: #c2c6ceff;
    hover: {{
      color: #e8e8e8ff;
    }}
    active: {{
      color: #e8e8e8ff;
    }}
  }}
  #tfm2dc_body:empty {{
    y: 40px;
    width: 100%;
    height: {body}px;
    ignore_event: true;{rows}
    #tfm2dc_say:label {{
      @"asset/base/style/main#label";
      x: 12px;
      y: 154px;
      width: 416px;
      height: 72px;
      size: 13;
      line_height: 18;
      color: #a3a9b6ff;
      ignore_event: true;
      text: "";
    }}
  }}
}}"#,
        w = PANEL_W,
        h = PANEL_H,
        x = x,
        top = top,
        head = HEAD_H,
        body = PANEL_H - 46,
        rows = rows
    )
}

/// ◀ / ▶ over the header, left of X and like X a child of the panel itself (the
/// header ignores events) — the game's own category arrows (banpick
/// `prev_champion_category`: tertiary_button style, 6×8 `left` / `right` icon)
/// at 24px. Spawned apart from the panel so a refusal costs only the arrows.
fn arrow_source(name: &str, icon: &str, x: i32) -> String {
    format!(
        r#"{name}:color_icon_button {{
  @"asset/base/style/main#tertiary_button";
  width: 24px;
  height: 24px;
  anchor_x: 1;
  pivot_x: 1;
  x: {x}px;
  y: 5px;
  icon: {{
    source: "asset/base/ui/icons/{icon}";
    rect: {{
      x: 9;
      y: 8;
      w: 6;
      h: 8;
    }}
  }}
}}"#
    )
}

const RING_SRC: &str = r#"tfm2dc_ring:color {
  width: 100%;
  height: 100%;
  stroke: 3;
  color: #38d6b3ff;
  back_color: #00000000;
  rounding: Uniform {
    rounding: 12;
  }
  visible: false;
  ignore_event: true;
}"#;

const TAG_SRC: &str = r#"tfm2dc_tag:color {
  x: 4px;
  y: 4px;
  width: 52px;
  height: 20px;
  color: #0b111cc8;
  rounding: Uniform {
    rounding: 6;
  }
  visible: false;
  ignore_event: true;
  #tfm2dc_t:label {
    @"asset/base/style/main#bold_label";
    width: 100%;
    height: 100%;
    size: 13;
    align_x: Center;
    align_y: Center;
    color: #e8e8e8ff;
    ignore_event: true;
    text: "";
  }
}"#;

/// tag ink per tier letter, without alpha
fn tier_rgb(letter: &str) -> &'static str {
    match letter {
        "S" => "#ffd959",
        "A" => "#5ce0e6",
        "B" => "#e8e8e8",
        "C" => "#a3a9b6",
        "D" => "#7d8290",
        _ => "#a3a9b6",
    }
}

fn row_text(i: usize, r: &RecRow, names: &HashMap<String, String>) -> String {
    let mut s = format!("{} ", RANK_GLYPH[i.min(4)]);
    if !r.lane.is_empty() && r.lane != "밴" && r.lane != "픽" {
        s.push_str(&format!("[{}] ", r.lane));
    }
    s.push_str(names.get(&r.id).unwrap_or(&r.ko));
    let badge = format!("{}{}", r.tier, r.chg);
    if !badge.trim().is_empty() {
        s.push_str(&format!("  {}", badge));
    }
    s.push_str(&format!("  {}", r.score));
    if !r.reason.is_empty() {
        s.push_str(&format!("  · {}", r.reason));
    }
    clip(&s, ROW_UNITS)
}

/// Typing in the champion search box must not toggle the panel.
fn typing(ctx: &StableClient) -> bool {
    ctx.ui_state_json("main.champion_search")
        .map(|j| j.replace(' ', "").contains("\"is_editing\":true"))
        .unwrap_or(false)
}

fn ensure_panel(ctx: &mut StableClient, st: &mut UiState, pos: (i32, i32)) {
    if ctx.ui_exists(PANEL) {
        if !st.panel_ok {
            // kept alive from an earlier visit (its handlers are still bound);
            // the fresh caches below re-apply every property
            st.panel_ok = true;
            st.panel_kind = "kept";
            st.arrows_ok = ctx.ui_exists(LEFT) && ctx.ui_exists(RIGHT);
        }
        return;
    }
    let ok = ctx.ui_spawn_source("main", &panel_source(pos));
    st.spawns += 1;
    st.panel_ok = ok;
    st.panel_kind = "color";
    if !ok {
        st.spawn_fails += 1;
        return;
    }
    st.panel_visible = None;
    st.applied_fold = None;
    st.applied_pos = Some(pos);
    st.applied_ends = None;
    st.title = None;
    st.rows = Default::default();
    st.say = None;
    // handlers bind per UI build → register on every (re)spawn of the panel
    let ok = ctx.ui_register_click(CLOSE, "", |c: &mut StableClient<'_>| {
        CLOSE_CLICKS.fetch_add(1, Ordering::Relaxed);
        USER_CLOSED.store(true, Ordering::Relaxed);
        let _ = c.ui_set_visible(PANEL, false);
    });
    st.click = format!("close registered={}", ok);
    st.arrows_ok = true;
    for (path, name, icon, x, step) in [(LEFT, "tfm2dc_left", "left", -70, -1), (RIGHT, "tfm2dc_right", "right", -40, 1)] {
        st.spawns += 1;
        let ok = ctx.ui_spawn_source(PANEL, &arrow_source(name, icon, x))
            && ctx.ui_register_click(path, "", move |_: &mut StableClient<'_>| {
                ARROW_CLICKS.fetch_add(1, Ordering::Relaxed);
                SPOT_STEP.fetch_add(step, Ordering::Relaxed);
            });
        if !ok {
            st.spawn_fails += 1;
            st.arrows_ok = false;
        }
    }
}

fn set_text(ctx: &mut StableClient, st: &mut UiState, path: &str, text: &str) {
    if !ctx.ui_set_text(path, text) {
        st.set_fails += 1;
        return;
    }
    // garbled-text report (2026-10-27): stored ≠ set means the host mangles the
    // string; stored = set points at rendering instead
    let got = ctx.ui_text(path);
    if got.as_deref() != Some(text) {
        st.readback_bad += 1;
        if st.readback_first.is_empty() {
            st.readback_first = format!("{} want={:?} got={:?}", path, text, got);
        }
    }
}

/// Our subtree (and one tile's) as lines: path, runner, visible, rect, label text.
fn dump_tree(ctx: &StableClient, path: &str, depth: usize, out: &mut Vec<String>) {
    if out.len() >= 60 {
        return;
    }
    let runner = ctx.ui_runner_name(path).unwrap_or_default();
    let mut line = format!(
        "{}{} [{}] vis={:?} rect={:?}",
        "  ".repeat(depth),
        path.rsplit('.').next().unwrap_or(path),
        runner,
        ctx.ui_visible(path),
        ctx.ui_node_rect(path).map(|(x, y, w, h)| (x as i32, y as i32, w as i32, h as i32))
    );
    if let Some(t) = ctx.ui_text(path).filter(|t| !t.is_empty()) {
        line.push_str(&format!(" text={:?}", t.chars().take(48).collect::<String>()));
    }
    out.push(line);
    if depth < 4 {
        for c in ctx.ui_child_names(path) {
            dump_tree(ctx, &format!("{}.{}", path, c), depth + 1, out);
        }
    }
}

fn set_props(ctx: &mut StableClient, st: &mut UiState, path: &str, props: &str) {
    if !ctx.ui_set_properties(path, props) {
        st.set_fails += 1;
    }
}

fn set_vis(ctx: &mut StableClient, st: &mut UiState, path: &str, v: bool) {
    if !ctx.ui_set_visible(path, v) {
        st.set_fails += 1;
    }
}

/// The ◀ ▶ spot: the player's (after applying pending clicks), else the side
/// default — module doc: Placement.
fn spot(swap: bool, red: bool) -> usize {
    let last = SPOT_X.len() - 1;
    let mut s = USER_SPOT.load(Ordering::Relaxed);
    if s > last {
        s = if !swap && red { 0 } else { last };
    }
    let step = SPOT_STEP.swap(0, Ordering::Relaxed);
    if step != 0 {
        s = (s as i32 + step).clamp(0, last as i32) as usize;
        USER_SPOT.store(s, Ordering::Relaxed);
    }
    s
}

fn stamp_panel(ctx: &mut StableClient, st: &mut UiState, view: &View, dim: bool) {
    let swap = ctx.ui_visible("main.swap") == Some(true);
    let red = view.my_blue == Some(false);
    let s = spot(swap, red);
    let top = match (swap, red) {
        (false, _) => TOP_DRAFT,
        (true, true) => TOP_SWAP_HIGH,
        (true, false) => TOP_SWAP_LOW,
    };
    let pos = (SPOT_X[s], top);
    ensure_panel(ctx, st, pos);
    if !st.panel_ok {
        return;
    }
    let visible = !USER_CLOSED.load(Ordering::Relaxed);
    if st.panel_visible != Some(visible) {
        set_vis(ctx, st, PANEL, visible);
        st.panel_visible = Some(visible);
    }
    if !visible {
        return;
    }
    if st.applied_pos != Some(pos) {
        set_props(ctx, st, PANEL, &format!("x: {}px; y: {}px;", pos.0, pos.1));
        st.applied_pos = Some(pos);
    }
    // the arrow that would step past an end is greyed out
    let ends = (s == 0, s == SPOT_X.len() - 1);
    if st.arrows_ok && st.applied_ends != Some(ends) {
        set_props(ctx, st, LEFT, &format!("disable: {};", ends.0));
        set_props(ctx, st, RIGHT, &format!("disable: {};", ends.1));
        st.applied_ends = Some(ends);
    }
    let folded = FOLDED.load(Ordering::Relaxed);
    if st.applied_fold != Some(folded) {
        let h = if folded { HEAD_H } else { PANEL_H };
        set_props(ctx, st, PANEL, &format!("height: {}px;", h));
        set_vis(ctx, st, BODY, !folded);
        st.applied_fold = Some(folded);
    }

    let out = view.out.as_deref();
    let what = match out {
        Some(o) if !o.swap.is_empty() => " · 스왑 배치",
        Some(o) if !o.recs.is_empty() && o.rec_ban => " · 밴 추천",
        Some(o) if !o.recs.is_empty() => " · 픽 추천",
        _ => "",
    };
    let title = format!("AI 추천기{}    [Tab {}]", what, if folded { "펼치기" } else { "접기" });
    if st.title.as_deref() != Some(title.as_str()) {
        set_text(ctx, st, TITLE, &title);
        st.title = Some(title);
    }
    if folded {
        return;
    }

    // rows: (text, icon champ id); swap lines carry no icon
    let mut want: [(String, String); 5] = Default::default();
    match out {
        Some(o) if !o.swap.is_empty() => {
            for (i, line) in o.swap.iter().take(5).enumerate() {
                want[i] = (clip(line, ROW_UNITS), String::new());
            }
        }
        Some(o) => {
            for (i, r) in o.detail_rows.iter().take(5).enumerate() {
                want[i] = (row_text(i, r, &view.names), r.id.clone());
            }
        }
        None => {}
    }
    for i in 0..5 {
        if st.rows[i].as_ref() == Some(&want[i]) {
            continue;
        }
        let row = format!("{}.tfm2dc_r{}", BODY, i);
        let (txt, icon) = &want[i];
        set_vis(ctx, st, &row, !txt.is_empty());
        set_text(ctx, st, &format!("{}.tfm2dc_t", row), txt);
        let icon_path = format!("{}.tfm2dc_i", row);
        // shown only once filled — an unfilled image node is never on screen
        let filled = !icon.is_empty() && ctx.ui_set_champion_icon(&icon_path, icon, 24.0, 24.0, 2.0);
        if !icon.is_empty() && !filled {
            st.icon_fails += 1;
        }
        set_vis(ctx, st, &icon_path, filled);
        st.rows[i] = Some(want[i].clone());
    }
    let mut notes = out.map(|o| o.notes.clone()).unwrap_or_default();
    if dim {
        // last = first to give way when the notes run out of lines
        notes.push(DIM_LEGEND.to_string());
    }
    let say = match out {
        Some(_) if !view.status.is_empty() => format!("{}\n{}", view.status, fit_notes(&notes)),
        Some(_) => fit_notes(&notes),
        None if !view.status.is_empty() => view.status.clone(),
        None => "밴픽 정보를 읽는 중...".to_string(),
    };
    if st.say.as_deref() != Some(say.as_str()) {
        set_text(ctx, st, &format!("{}.tfm2dc_say", BODY), &say);
        st.say = Some(say);
    }
}

fn desired_tile(champ: &str, view: &View, dim: bool) -> TileLook {
    let off = TileLook { ring: Ring::Off, tag: Tag::Off };
    let Some(o) = view.out.as_deref() else { return off };
    if view.taken.contains(champ) {
        return off;
    }
    let (letter, unsure, chg) = o.tiers.get(champ).map_or(("", false, ""), |m| (m.letter, m.unsure, m.chg));
    let tier = format!("{}{}", letter, chg);
    match o.recs.iter().position(|(id, _, _)| id == champ) {
        Some(rk) => {
            let col = if o.rec_ban { BAN_COL } else { PICK_COL };
            TileLook {
                ring: Ring::On { ban: o.rec_ban, first: rk == 0 },
                tag: Tag::On {
                    text: format!("{}{}", RANK_GLYPH[rk.min(4)], tier),
                    fill: format!("{}e6", col),
                    ink: "#0b111cff".to_string(),
                },
            }
        }
        None if !tier.is_empty() => TileLook {
            ring: Ring::Off,
            tag: Tag::On {
                text: tier,
                fill: "#0b111cc8".to_string(),
                ink: format!("{}{}", tier_rgb(letter), if dim && unsure { "80" } else { "ff" }),
            },
        },
        None => off,
    }
}

fn stamp_tiles(ctx: &mut StableClient, st: &mut UiState, view: &View, dim: bool) {
    for champ in &view.tiles {
        let base = format!("main.champions.contents.{}", champ);
        let ring = format!("{}.tfm2dc_ring", base);
        let tag = format!("{}.tfm2dc_tag", base);
        if !ctx.ui_exists(&tag) {
            // new screen or a rebuilt tile: (re)spawn, forget what we applied
            st.tiles.remove(champ);
            if let Some(at) = st.tile_fail.get(champ) {
                if st.frame < at + 120 {
                    continue;
                }
            }
            let ok = (ctx.ui_exists(&ring) || ctx.ui_spawn_source(&base, RING_SRC))
                && ctx.ui_spawn_source(&base, TAG_SRC);
            st.spawns += 1;
            if !ok {
                st.spawn_fails += 1;
                st.tile_fail.insert(champ.clone(), st.frame);
                continue;
            }
            st.tile_fail.remove(champ);
            // a surviving ring may still be lit: force it off along with the new tag
            set_vis(ctx, st, &ring, false);
            st.tiles.insert(champ.clone(), TileLook { ring: Ring::Off, tag: Tag::Off });
        } else if !st.tiles.contains_key(champ) {
            // nodes from an earlier visit of a screen the game kept alive: their
            // state is unknown to this cache → force both off, then apply below
            set_vis(ctx, st, &ring, false);
            set_vis(ctx, st, &tag, false);
            st.tiles.insert(champ.clone(), TileLook { ring: Ring::Off, tag: Tag::Off });
        }
        let want = desired_tile(champ, view, dim);
        let Some(have) = st.tiles.get(champ).cloned() else { continue };
        if have == want {
            continue;
        }
        if have.ring != want.ring {
            match &want.ring {
                Ring::Off => set_vis(ctx, st, &ring, false),
                Ring::On { ban, first } => {
                    let col = if *ban { BAN_COL } else { PICK_COL };
                    set_props(ctx, st, &ring, &format!("stroke: {}; color: {}ff;", if *first { 4 } else { 3 }, col));
                    set_vis(ctx, st, &ring, true);
                }
            }
        }
        if have.tag != want.tag {
            match &want.tag {
                Tag::Off => set_vis(ctx, st, &tag, false),
                Tag::On { text, fill, ink } => {
                    set_props(ctx, st, &tag, &format!("color: {};", fill));
                    let label = format!("{}.tfm2dc_t", tag);
                    set_props(ctx, st, &label, &format!("color: {};", ink));
                    set_text(ctx, st, &label, text);
                    set_vis(ctx, st, &tag, true);
                }
            }
        }
        st.tiles.insert(champ.clone(), want);
    }
}

/// post_update on banpick frames.
pub(crate) fn stamp(ctx: &mut StableClient, view: &View) {
    let Ok(mut g) = UI.lock() else { return };
    let st = g.get_or_insert_with(UiState::default);
    st.frame += 1;

    let events = ctx.input_events();
    for ev in &events {
        if ev.kind == Some(InputEventKindV1::KeyPressed) && st.keys.len() < 16 && !st.keys.contains(&ev.key) {
            st.keys.push(ev.key.clone());
        }
    }
    let tab = events.iter().any(|e| e.kind == Some(InputEventKindV1::KeyPressed) && e.key == "Tab");
    if tab && !typing(ctx) {
        st.tabs += 1;
        if USER_CLOSED.swap(false, Ordering::Relaxed) {
            FOLDED.store(false, Ordering::Relaxed);
        } else {
            FOLDED.fetch_xor(true, Ordering::Relaxed);
        }
    }

    // an unstable letter is drawn dimmed only while some letters are stable: at
    // a cold start every letter is unstable, and dimming them all says nothing
    let dim = view.out.as_deref().map_or(false, |o| {
        let lettered = || o.tiers.values().filter(|m| !m.letter.is_empty());
        lettered().any(|m| m.unsure) && lettered().any(|m| !m.unsure)
    });
    stamp_panel(ctx, st, view, dim);
    stamp_tiles(ctx, st, view, dim);

    if st.frame % 30 == 1 {
        let ranked = st.tiles.values().filter(|l| l.ring != Ring::Off).count();
        let tagged = st.tiles.values().filter(|l| l.tag != Tag::Off).count();
        diag::set(
            "6 ui",
            format!(
                "panel ok={} kind={} runner={:?} rect={:?} visible={:?} pos={:?} user_spot={} arrows_ok={} arrow_clicks={} folded={} closed_by_user={} close_clicks={} dim={}\n{}\ntiles on_screen={} with_nodes={} ranked={} tagged={} spawn_retry={}\nspawns={} spawn_fails={} set_fails={} icon_fails={}\ntab_presses={} keys_seen={:?}\ntag_runner={:?} search_state={:?} swap_visible={:?}",
                st.panel_ok,
                st.panel_kind,
                ctx.ui_runner_name(PANEL),
                ctx.ui_node_rect(PANEL),
                st.panel_visible,
                st.applied_pos,
                match USER_SPOT.load(Ordering::Relaxed) {
                    s if s < SPOT_X.len() => s.to_string(),
                    _ => "-".to_string(),
                },
                st.arrows_ok,
                ARROW_CLICKS.load(Ordering::Relaxed),
                FOLDED.load(Ordering::Relaxed),
                USER_CLOSED.load(Ordering::Relaxed),
                CLOSE_CLICKS.load(Ordering::Relaxed),
                dim,
                st.click,
                view.tiles.len(),
                st.tiles.len(),
                ranked,
                tagged,
                st.tile_fail.len(),
                st.spawns,
                st.spawn_fails,
                st.set_fails,
                st.icon_fails,
                st.tabs,
                st.keys,
                view.tiles
                    .first()
                    .and_then(|c| ctx.ui_runner_name(&format!("main.champions.contents.{}.tfm2dc_tag", c))),
                ctx.ui_state_json("main.champion_search"),
                ctx.ui_visible("main.swap"),
            ),
        );
    }
    if st.frame % 60 == 2 {
        let mut lines = vec![format!(
            "readback_bad={} first={}",
            st.readback_bad,
            if st.readback_first.is_empty() { "-" } else { st.readback_first.as_str() }
        )];
        let main_kids: Vec<String> = ctx
            .ui_child_names("main")
            .into_iter()
            .map(|c| {
                let p = format!("main.{}", c);
                format!("{}[{}{}]", c, ctx.ui_runner_name(&p).unwrap_or_default(), match ctx.ui_visible(&p) {
                    Some(true) => "",
                    _ => ",hidden",
                })
            })
            .collect();
        lines.push(format!("main children: {}", main_kids.join(" ")));
        dump_tree(ctx, PANEL, 0, &mut lines);
        let tile = st
            .tiles
            .iter()
            .find(|(_, l)| l.tag != Tag::Off)
            .map(|(c, _)| c.clone())
            .or_else(|| view.tiles.first().cloned());
        if let Some(c) = tile {
            dump_tree(ctx, &format!("main.champions.contents.{}", c), 0, &mut lines);
        }
        diag::set("9 tree", lines.join("\n"));
    }
}
