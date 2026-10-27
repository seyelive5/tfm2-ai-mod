//! The two cards — rank (top 5, or the swap plan) and note (commentary) — drawn
//! like champion_info's skill boxes (0.6.5): #161721, rounding 12, a 40px icon
//! slot, a bold 18 title, a 1px rule at y60, body text 15/20.

use super::layout::{Place, CARD_H, CARD_W, HINT_H, HINT_W};
use super::{Applied, UiState, View};
use crate::markup;
use crate::recommender::{Output, RecRow};
use mod_api_stable::*;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Mode {
    /// nothing to rank yet (reading the screen, loading stats)
    #[default]
    Wait,
    Ban,
    Pick,
    /// my five are in: the lane plan
    Swap,
}

/// score bar track width / the floor a weak or non-positive score still shows
pub const BAR_W: i32 = 60;
pub const BAR_MIN: i32 = 4;
/// row cells (card-local x): lane icon 18, face 22, then the stat cells
pub const LANE: i32 = 18;
pub const FACE: i32 = 22;
pub const TIER_X: i32 = 207;
pub const CHG_X: i32 = 227;
pub const BAR_X: i32 = 247;
pub const SCORE_X: i32 = 313;
pub const WHY_X: i32 = 357;
/// the card's inner right edge (427 − 10 padding)
pub const INNER_RIGHT: i32 = 417;
/// note body: 6 lines of 20px; a line of size-15 text in half-em units, with
/// margin for the label's own word wrap
pub const NOTE_LINES: usize = 6;
pub const NOTE_UNITS: usize = 48;

pub fn mode(out: Option<&Output>) -> Mode {
    match out {
        Some(o) if !o.swap.is_empty() => Mode::Swap,
        Some(o) if !o.recs.is_empty() => {
            if o.rec_ban {
                Mode::Ban
            } else {
                Mode::Pick
            }
        }
        _ => Mode::Wait,
    }
}

/// Rank numbers, score bars and tile rings / badges, without alpha.
pub fn turn_rgb(m: Mode) -> &'static str {
    match m {
        // the game's magic-damage violet / its accent teal
        Mode::Ban => "a974ff",
        Mode::Pick | Mode::Swap => "37d5b3",
        Mode::Wait => "a3a9b6",
    }
}

pub fn tier_rgb(letter: &str) -> &'static str {
    match letter {
        "S" => "fad33e",
        "A" => "5ce0e6",
        "B" => "e8e8e8",
        "C" => "a3a9b6",
        "D" => "7d8290",
        _ => "a3a9b6",
    }
}

/// The game's lane icon (`asset/base/ui/icons/<name>`) for a lane name.
pub fn lane_icon(lane: &str) -> &'static str {
    match lane {
        "탑" => "top",
        "정글" => "jungle",
        "미드" => "mid",
        "바텀" => "bottom",
        "서폿" => "support",
        _ => "",
    }
}

pub fn title(m: Mode) -> &'static str {
    match m {
        Mode::Wait => "AI 추천",
        Mode::Ban => "밴 추천",
        Mode::Pick => "픽 추천",
        Mode::Swap => "배치 추천",
    }
}

/// Under the title: the draft step being decided, or what the swap card is.
pub fn subtitle(m: Mode, step: Option<(usize, usize)>) -> String {
    match (m, step) {
        (Mode::Swap, _) => "라인 배치".to_string(),
        (Mode::Ban | Mode::Pick, Some((done, total))) if total > 0 => format!("{} / {}", (done + 1).min(total), total),
        _ => String::new(),
    }
}

pub fn pill(matches: usize) -> String {
    // the recommender's cold-start cut (its 콜드스타트 line)
    if matches < 50 {
        format!("표본 {}판 · 초반", matches)
    } else {
        format!("표본 {}판", matches)
    }
}

/// Leading cells per mode: (x, width) of the rank number (the lane name in the
/// swap plan), the lane icon's x, the face's x, (x, width) of the name; `stats`
/// = tier / change / bar / score / reason are shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub n: (i32, i32),
    pub lane: Option<i32>,
    pub face: i32,
    pub name: (i32, i32),
    pub stats: bool,
}

pub fn geom(m: Mode) -> Geom {
    match m {
        Mode::Ban => Geom { n: (10, 16), lane: None, face: 30, name: (58, 145), stats: true },
        Mode::Pick | Mode::Wait => Geom { n: (10, 16), lane: Some(30), face: 52, name: (80, 123), stats: true },
        Mode::Swap => Geom { n: (10, 36), lane: Some(50), face: 74, name: (102, 300), stats: false },
    }
}

/// Half-em units that fit `px` of bold 15 text (a Hangul syllable ≈ 15px = 2).
pub fn units_for(px: i32) -> usize {
    (px.max(0) as f32 / 7.5) as usize
}

/// What one rank-card row shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowView {
    /// the rank digit, or the lane name in the swap plan
    pub n: String,
    /// lane icon name, "" = none
    pub lane: &'static str,
    /// champion id for the face icon
    pub face: String,
    pub name: String,
    /// tier letter ("" = no chip) and its ink (rgba hex)
    pub tier: String,
    pub tier_ink: String,
    pub chg: String,
    /// score bar fill width (px), 0 = no bar
    pub bar: i32,
    pub score: String,
    pub why: String,
}

/// The rank card's rows: top 5 in the draft, the lane plan in the swap phase.
/// `names` = the game's own champion names; `dim` = draw unsure tiers dimmed.
pub fn rows(o: &Output, names: &HashMap<String, String>, dim: bool) -> Vec<RowView> {
    let m = mode(Some(o));
    let fit = units_for(geom(m).name.1);
    let name = |id: &str, ko: &str| clip(names.get(id).map_or(ko, |s| s.as_str()), fit);
    if m == Mode::Swap {
        return o
            .swap
            .iter()
            .take(5)
            .map(|r| RowView {
                n: r.lane.to_string(),
                lane: lane_icon(r.lane),
                face: r.id.clone(),
                name: name(&r.id, &r.ko),
                ..Default::default()
            })
            .collect();
    }
    let score = |r: &RecRow| r.score.parse::<i32>().unwrap_or(0);
    let top = o.detail_rows.iter().take(5).map(score).max().unwrap_or(0);
    o.detail_rows
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, r)| {
            let unsure = o.tiers.get(&r.id).map_or(false, |t| t.unsure);
            RowView {
                n: (i + 1).to_string(),
                lane: lane_icon(&r.lane),
                face: r.id.clone(),
                name: name(&r.id, &r.ko),
                tier: r.tier.clone(),
                tier_ink: format!("{}{}", tier_rgb(&r.tier), if dim && unsure { "80" } else { "ff" }),
                chg: r.chg.clone(),
                bar: if top > 0 { (BAR_W * score(r).max(0) / top).max(BAR_MIN) } else { BAR_MIN },
                score: r.score.clone(),
                why: r.reason.clone(),
            }
        })
        .collect()
}

/// Plain text cut to `max` half-em units, "…" included when cut.
pub fn clip(s: &str, max: usize) -> String {
    if markup::width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = if c.is_ascii() { 1 } else { 2 };
        if used + w + 2 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// The notes that fit `lines` lines of `per_line` units, in order, one note per
/// line start. Markup takes no room; the note that overflows is cut as plain
/// text to the lines left, and everything after it is dropped.
pub fn fit_notes(notes: &[String], lines: usize, per_line: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut used = 0;
    for n in notes {
        if used >= lines {
            break;
        }
        let need = ((markup::width(n) + per_line - 1) / per_line).max(1);
        if used + need > lines {
            // cutting inside a span would leave a dangling code
            out.push(clip(&markup::plain(n), (lines - used) * per_line));
            break;
        }
        out.push(n.clone());
        used += need;
    }
    out.join("\n")
}

// ── nodes

pub const RANK: &str = "tfm2dc_rank";
pub const NOTE: &str = "tfm2dc_note";
pub const HINT: &str = "tfm2dc_hint";
/// every node a card may hang from (layout::place)
const PARENTS: [&str; 2] = ["main.champion_info", "main"];
/// The game's overlays drawn after champion_info: the draft cards sit under
/// them by construction, the swap cards (children of `main`, drawn last) stand
/// aside while one of them is up and over a card.
const OVERLAYS: [&str; 10] = [
    "main.champion_info_tooltip",
    "main.pick_slot_tooltip_layer",
    "main.coach_dialogue_button_tooltip",
    "main.chat_popup",
    "main.coach_dialogue_popup",
    "main.phase_indicator",
    "main.coach_delegation_indicator",
    "main.fade",
    "main.tooltip",
    "main.stats_popup",
];
const DIM_LEGEND: &str = "흐린 티어 글자: 표본이 적어 아직 불확실한 티어";
const LABEL: &str = r#"@"asset/base/style/main#label";"#;
const BOLD: &str = r#"@"asset/base/style/main#bold_label";"#;

/// Hidden image children with a fixed `source`, each named after its icon.
/// Another icon is shown by a visibility switch, never by re-sourcing a sprite
/// at runtime — that would need host support no 0.6.5 template exercises, and a
/// silently ignored write would leave a wrong lane icon on screen.
fn icon_set(icons: &[&str], size: i32, centred: bool) -> String {
    let anchor = if centred { "\n      anchor_x: 0.5;\n      pivot_x: 0.5;\n      anchor_y: 0.5;\n      pivot_y: 0.5;" } else { "" };
    icons
        .iter()
        .map(|icon| {
            format!(
                r#"
    #{icon}:image {{
      width: {size}px;
      height: {size}px;{anchor}
      source: "asset/base/ui/icons/{icon}";
      color: #c2c6ceff;
      visible: false;
      ignore_event: true;
    }}"#,
                icon = icon,
                size = size,
                anchor = anchor
            )
        })
        .collect()
}

/// lane icon names in lane order (lane_icon)
const LANE_ICONS: [&str; 5] = ["top", "jungle", "mid", "bottom", "support"];

/// The skill-box shell both cards share: icon slot, title, subtitle, rule.
fn shell(name: &str, (x, y): (i32, i32), icons: &[&str], title: &str, inner: &str) -> String {
    format!(
        r#"{name}:color {{
  x: {x}px;
  y: {y}px;
  width: {w}px;
  height: {h}px;
  color: #161721ff;
  rounding: Uniform {{
    rounding: 12;
  }}
  #slot:color {{
    x: 10px;
    y: 10px;
    width: 40px;
    height: 40px;
    color: #1d1f2cff;
    rounding: Uniform {{
      rounding: 8;
    }}
    ignore_event: true;{icons}
  }}
  #title:label {{
    {bold}
    x: 60px;
    y: 9px;
    width: 230px;
    height: 24px;
    size: 18;
    align_y: Center;
    color: #e8e8e8ff;
    ignore_event: true;
    text: "{title}";
  }}
  #sub:label {{
    {label}
    x: 60px;
    y: 33px;
    width: 230px;
    height: 18px;
    size: 14;
    align_y: Center;
    color: #7d8290ff;
    ignore_event: true;
    text: "";
  }}
  #rule:color {{
    x: 10px;
    y: 59px;
    width: {inner_w}px;
    height: 1px;
    color: #4a4c56ff;
    ignore_event: true;
  }}{inner}
}}"#,
        name = name,
        x = x,
        y = y,
        w = CARD_W,
        h = CARD_H,
        icons = icon_set(icons, 24, true),
        title = title,
        bold = BOLD,
        label = LABEL,
        inner_w = INNER_RIGHT - 10,
        inner = inner
    )
}

/// One rank row: every cell spawned once; mode changes move / hide cells.
fn row_source(i: usize) -> String {
    format!(
        r#"
  #r{i}:empty {{
    y: {y}px;
    width: {w}px;
    height: 24px;
    visible: false;
    ignore_event: true;
    #n:label {{
      {bold}
      x: 10px;
      width: 16px;
      height: 24px;
      size: 15;
      align_x: Center;
      align_y: Center;
      ignore_event: true;
      text: "";
    }}
    #lane:empty {{
      x: 30px;
      y: 3px;
      width: {lane}px;
      height: {lane}px;
      visible: false;
      ignore_event: true;{lanes}
    }}
    #face:image {{
      x: 52px;
      y: 1px;
      width: {face}px;
      height: {face}px;
      visible: false;
      ignore_event: true;
    }}
    #name:label {{
      {bold}
      x: 80px;
      width: 123px;
      height: 24px;
      size: 15;
      align_y: Center;
      color: #e8e8e8ff;
      ignore_event: true;
      text: "";
    }}
    #tier:color {{
      x: {tier_x}px;
      y: 3px;
      width: 18px;
      height: 18px;
      color: #0f1016ff;
      rounding: Uniform {{
        rounding: 4;
      }}
      visible: false;
      ignore_event: true;
      #t:label {{
        {bold}
        width: 100%;
        height: 100%;
        size: 13;
        align_x: Center;
        align_y: Center;
        ignore_event: true;
        text: "";
      }}
    }}
    #chg:label {{
      {label}
      x: {chg_x}px;
      width: 14px;
      height: 24px;
      size: 11;
      align_x: Center;
      align_y: Center;
      visible: false;
      ignore_event: true;
      text: "";
    }}
    #track:color {{
      x: {bar_x}px;
      y: 10px;
      width: {bar_w}px;
      height: 4px;
      color: #2a2c37ff;
      rounding: Uniform {{
        rounding: 2;
      }}
      visible: false;
      ignore_event: true;
      #fill:color {{
        width: {bar_min}px;
        height: 4px;
        rounding: Uniform {{
          rounding: 2;
        }}
        ignore_event: true;
      }}
    }}
    #sc:label {{
      {bold}
      x: {sc_x}px;
      width: 38px;
      height: 24px;
      size: 14;
      align_x: Right;
      align_y: Center;
      color: #e8e8e8ff;
      visible: false;
      ignore_event: true;
      text: "";
    }}
    #why:color {{
      x: {why_x}px;
      y: 3px;
      width: 60px;
      height: 18px;
      color: #1d1f2cff;
      rounding: Uniform {{
        rounding: 9;
      }}
      visible: false;
      ignore_event: true;
      #t:label {{
        {label}
        width: 100%;
        height: 100%;
        size: 12;
        align_x: Center;
        align_y: Center;
        color: #a3a9b6ff;
        ignore_event: true;
        text: "";
      }}
    }}
  }}"#,
        i = i,
        y = 66 + 25 * i as i32,
        w = CARD_W,
        bold = BOLD,
        label = LABEL,
        tier_x = TIER_X,
        chg_x = CHG_X,
        bar_x = BAR_X,
        bar_w = BAR_W,
        bar_min = BAR_MIN,
        sc_x = SCORE_X,
        why_x = WHY_X,
        lane = LANE,
        lanes = icon_set(&LANE_ICONS, LANE, false),
        face = FACE
    )
}

fn rank_source(pos: (i32, i32)) -> String {
    let mut inner = format!(
        r#"
  #key:color {{
    anchor_x: 1;
    pivot_x: 1;
    x: -10px;
    y: 20px;
    width: 38px;
    height: 20px;
    stroke: 1;
    color: #4a4c56ff;
    back_color: #0f1016ff;
    rounding: Uniform {{
      rounding: 5;
    }}
    ignore_event: true;
    #t:label {{
      {label}
      width: 100%;
      height: 100%;
      size: 12;
      align_x: Center;
      align_y: Center;
      color: #a3a9b6ff;
      ignore_event: true;
      text: "Tab";
    }}
  }}
  #idle:label {{
    {label}
    x: 10px;
    y: 66px;
    width: 407px;
    height: 124px;
    size: 15;
    align_x: Center;
    align_y: Center;
    color: #7d8290ff;
    visible: false;
    ignore_event: true;
    text: "";
  }}"#,
        label = LABEL
    );
    for i in 0..5 {
        inner.push_str(&row_source(i));
    }
    shell(RANK, pos, &["chart", "swap"], title(Mode::Wait), &inner)
}

fn note_source(pos: (i32, i32)) -> String {
    let inner = format!(
        r#"
  #pill:color {{
    anchor_x: 1;
    pivot_x: 1;
    x: -10px;
    y: 19px;
    width: 90px;
    height: 22px;
    color: #1d1f2cff;
    rounding: Uniform {{
      rounding: 11;
    }}
    ignore_event: true;
    #t:label {{
      {label}
      width: 100%;
      height: 100%;
      size: 12;
      align_x: Center;
      align_y: Center;
      color: #a3a9b6ff;
      ignore_event: true;
      text: "";
    }}
  }}
  #body:label {{
    {label}
    x: 10px;
    y: 68px;
    width: 407px;
    height: 122px;
    size: 15;
    line_height: 20;
    color: #c8ccd4ff;
    ignore_event: true;
    text: "";
  }}"#,
        label = LABEL
    );
    shell(NOTE, pos, &["chat"], "AI 분석관", &inner)
}

fn hint_source((x, y): (i32, i32)) -> String {
    format!(
        r#"{name}:color {{
  x: {x}px;
  y: {y}px;
  width: {w}px;
  height: {h}px;
  stroke: 1;
  color: #4a4c56ff;
  back_color: #161721f0;
  rounding: Uniform {{
    rounding: 11;
  }}
  visible: false;
  ignore_event: true;
  #t:label {{
    {label}
    width: 100%;
    height: 100%;
    size: 12;
    align_x: Center;
    align_y: Center;
    color: #a3a9b6ff;
    ignore_event: true;
    text: "AI 분석관 · Tab";
  }}
}}"#,
        name = HINT,
        x = x,
        y = y,
        w = HINT_W,
        h = HINT_H,
        label = LABEL
    )
}

/// All three nodes exist under `place.parent` (spawned when missing; ours under
/// any other parent removed — the draft/swap switch, or a screen the game kept
/// alive from an earlier visit).
fn ensure(ctx: &mut StableClient, st: &mut UiState, place: &Place) -> bool {
    if st.parent != Some(place.parent) {
        for p in PARENTS.iter().filter(|p| **p != place.parent) {
            for n in [RANK, NOTE, HINT] {
                let path = format!("{}.{}", p, n);
                if ctx.ui_exists(&path) {
                    ctx.ui_remove_node(&path);
                }
                st.ap.forget(&path);
            }
        }
        st.parent = Some(place.parent);
    }
    for n in [RANK, NOTE, HINT] {
        let path = format!("{}.{}", place.parent, n);
        if ctx.ui_exists(&path) {
            continue;
        }
        if st.card_fail_at.map_or(false, |at| st.frame < at + 120) {
            return false;
        }
        let src = match n {
            RANK => rank_source(place.rank),
            NOTE => note_source(place.note),
            _ => hint_source(place.hint),
        };
        st.ap.forget(&path);
        st.spawns += 1;
        if !ctx.ui_spawn_source(place.parent, &src) {
            st.spawn_fails += 1;
            st.card_fail_at = Some(st.frame);
            return false;
        }
    }
    st.card_fail_at = None;
    true
}

/// The first of the game's overlays that is up over a swap card (an unreadable
/// rect counts as over).
fn covering(ctx: &StableClient, place: &Place) -> Option<&'static str> {
    if place.parent != "main" {
        return None;
    }
    let over = |(x, y, w, h): (f32, f32, f32, f32)| {
        [place.rank, place.note].iter().any(|&(cx, cy)| {
            let (cx, cy) = (cx as f32, cy as f32);
            x < cx + CARD_W as f32 && cx < x + w && y < cy + CARD_H as f32 && cy < y + h
        })
    };
    OVERLAYS
        .iter()
        .copied()
        .find(|p| ctx.ui_visible(p) == Some(true) && ctx.ui_node_rect(p).map_or(true, over))
}

/// Both cards for this frame: spots, Tab / overlay visibility, contents.
pub(super) fn stamp(ctx: &mut StableClient, st: &mut UiState, view: &View, place: &Place, dim: bool) {
    if !ensure(ctx, st, place) {
        return;
    }
    let o = view.out.as_deref();
    let m = mode(o);
    st.mode = m;
    st.yield_to = covering(ctx, place);
    let [rank, note, hint] = [RANK, NOTE, HINT].map(|n| format!("{}.{}", place.parent, n));
    for (path, (x, y)) in [(&rank, place.rank), (&note, place.note), (&hint, place.hint)] {
        st.ap.props(ctx, path, &format!("x: {}px; y: {}px;", x, y));
    }
    let shown = !st.hidden && st.yield_to.is_none();
    st.ap.vis(ctx, &rank, shown);
    st.ap.vis(ctx, &note, shown && o.is_some());
    st.ap.vis(ctx, &hint, st.hidden && st.yield_to.is_none());
    if !shown {
        return;
    }
    fill_rank(ctx, &mut st.ap, &rank, view, m, dim);
    if let Some(o) = o {
        fill_note(ctx, &mut st.ap, &note, view, o, m, dim);
    }
}

fn fill_rank(ctx: &mut StableClient, ap: &mut Applied, card: &str, view: &View, m: Mode, dim: bool) {
    let turn = turn_rgb(m);
    for icon in ["chart", "swap"] {
        let path = format!("{}.slot.{}", card, icon);
        let on = (icon == "swap") == (m == Mode::Swap);
        if on {
            ap.props(ctx, &path, &format!("color: #{}ff;", turn));
        }
        ap.vis(ctx, &path, on);
    }
    ap.text(ctx, &format!("{}.title", card), title(m));
    ap.text(ctx, &format!("{}.sub", card), &subtitle(m, view.step));
    let idle = format!("{}.idle", card);
    ap.vis(ctx, &idle, m == Mode::Wait);
    if m == Mode::Wait {
        let why = match &view.out {
            _ if !view.status.is_empty() => view.status.as_str(),
            None => "밴픽 정보를 읽는 중...",
            Some(_) => "추천할 후보가 없습니다",
        };
        ap.text(ctx, &idle, why);
    }
    let rows = view.out.as_deref().map(|o| rows(o, &view.names, dim)).unwrap_or_default();
    let g = geom(m);
    for i in 0..5 {
        let r = format!("{}.r{}", card, i);
        let Some(v) = rows.get(i) else {
            ap.vis(ctx, &r, false);
            continue;
        };
        ap.vis(ctx, &r, true);
        let cell = |c: &str| format!("{}.{}", r, c);
        ap.props(ctx, &cell("n"), &format!("x: {}px; width: {}px; color: #{}ff;", g.n.0, g.n.1, turn));
        ap.text(ctx, &cell("n"), &v.n);
        match g.lane.filter(|_| !v.lane.is_empty()) {
            Some(x) => {
                ap.props(ctx, &cell("lane"), &format!("x: {}px;", x));
                for icon in LANE_ICONS {
                    ap.vis(ctx, &cell(&format!("lane.{}", icon)), icon == v.lane);
                }
                ap.vis(ctx, &cell("lane"), true);
            }
            None => ap.vis(ctx, &cell("lane"), false),
        }
        ap.props(ctx, &cell("face"), &format!("x: {}px;", g.face));
        let face = ap.face(ctx, &cell("face"), &v.face, FACE as f32);
        ap.vis(ctx, &cell("face"), face);
        ap.props(ctx, &cell("name"), &format!("x: {}px; width: {}px;", g.name.0, g.name.1));
        ap.text(ctx, &cell("name"), &v.name);
        let tier = g.stats && !v.tier.is_empty();
        ap.vis(ctx, &cell("tier"), tier);
        if tier {
            ap.props(ctx, &cell("tier.t"), &format!("color: #{};", v.tier_ink));
            ap.text(ctx, &cell("tier.t"), &v.tier);
        }
        let chg = g.stats && !v.chg.is_empty();
        ap.vis(ctx, &cell("chg"), chg);
        if chg {
            // buff green / nerf red / mixed amber (the game's heal / CC / duration inks)
            let ink = match v.chg.as_str() {
                "▲" => "6aff55",
                "▼" => markup::WARN,
                _ => markup::NUM,
            };
            ap.props(ctx, &cell("chg"), &format!("color: #{}ff;", ink));
            ap.text(ctx, &cell("chg"), &v.chg);
        }
        ap.vis(ctx, &cell("track"), g.stats);
        ap.vis(ctx, &cell("sc"), g.stats);
        if g.stats {
            ap.props(ctx, &cell("track.fill"), &format!("width: {}px; color: #{}ff;", v.bar, turn));
            ap.text(ctx, &cell("sc"), &v.score);
        }
        let why = g.stats && !v.why.is_empty();
        ap.vis(ctx, &cell("why"), why);
        if why {
            ap.text(ctx, &cell("why.t"), &v.why);
        }
    }
}

fn fill_note(ctx: &mut StableClient, ap: &mut Applied, card: &str, view: &View, o: &Output, m: Mode, dim: bool) {
    let icon = format!("{}.slot.chat", card);
    ap.props(ctx, &icon, &format!("color: #{}ff;", turn_rgb(m)));
    ap.vis(ctx, &icon, true);
    let p = pill(o.matches);
    ap.props(ctx, &format!("{}.pill", card), &format!("width: {}px;", 16 + 6 * markup::width(&p) as i32));
    ap.text(ctx, &format!("{}.pill.t", card), &p);
    let mut notes: Vec<String> = Vec::new();
    if !view.status.is_empty() {
        notes.push(view.status.clone());
    }
    notes.extend(o.notes.iter().cloned());
    if dim {
        // after the data line: the first to give way
        notes.push(markup::dim(DIM_LEGEND));
    }
    ap.text(ctx, &format!("{}.body", card), &fit_notes(&notes, NOTE_LINES, NOTE_UNITS));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommender::{RecRow, SwapRow, TierMark};

    fn row(id: &str, ko: &str, lane: &str, tier: &str, score: &str) -> RecRow {
        RecRow {
            id: id.into(),
            ko: ko.into(),
            lane: lane.into(),
            tier: tier.into(),
            chg: String::new(),
            score: score.into(),
            reason: "강챔".into(),
        }
    }

    fn out(rec_ban: bool, rows: Vec<RecRow>) -> Output {
        Output {
            rec_ban,
            recs: rows.iter().map(|r| (r.id.clone(), r.ko.clone(), r.score.clone())).collect(),
            detail_rows: rows,
            ..Default::default()
        }
    }

    fn none() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn mode_follows_the_output() {
        assert_eq!(mode(None), Mode::Wait);
        assert_eq!(mode(Some(&Output::default())), Mode::Wait);
        assert_eq!(mode(Some(&out(true, vec![row("monk", "몽크", "밴", "A", "+5")]))), Mode::Ban);
        assert_eq!(mode(Some(&out(false, vec![row("monk", "몽크", "탑", "A", "+5")]))), Mode::Pick);
        let mut o = Output::default();
        o.swap = vec![SwapRow { lane: "탑", id: "monk".into(), ko: "몽크".into() }];
        assert_eq!(mode(Some(&o)), Mode::Swap);
    }

    #[test]
    fn pick_row_carries_rank_lane_face_tier_bar_score_and_reason() {
        let mut r = row("fighter", "격투가", "탑", "S", "+12");
        r.chg = "▲".into();
        let got = &rows(&out(false, vec![r]), &none(), false)[0];
        let want = RowView {
            n: "1".into(),
            lane: "top",
            face: "fighter".into(),
            name: "격투가".into(),
            tier: "S".into(),
            tier_ink: "fad33eff".into(),
            chg: "▲".into(),
            bar: BAR_W,
            score: "+12".into(),
            why: "강챔".into(),
        };
        assert_eq!(*got, want);
    }

    #[test]
    fn every_lane_maps_to_a_game_icon() {
        let icons: Vec<&str> = ["탑", "정글", "미드", "바텀", "서폿"].iter().map(|l| lane_icon(l)).collect();
        assert_eq!(icons, ["top", "jungle", "mid", "bottom", "support"]);
        assert_eq!(icons, LANE_ICONS, "the spawned lane images");
        assert_eq!(lane_icon("밴"), "");
    }

    #[test]
    fn ban_rows_carry_no_lane_icon() {
        let o = out(true, vec![row("monk", "몽크", "밴", "A", "+5")]);
        assert_eq!(rows(&o, &none(), false)[0].lane, "");
    }

    #[test]
    fn bars_scale_to_the_top_score() {
        let o = out(
            false,
            vec![row("a", "가", "탑", "", "+12"), row("b", "나", "탑", "", "+6"), row("c", "다", "탑", "", "+0")],
        );
        let bars: Vec<i32> = rows(&o, &none(), false).iter().map(|r| r.bar).collect();
        assert_eq!(bars, vec![BAR_W, BAR_W / 2, BAR_MIN]);
    }

    #[test]
    fn bars_stay_minimal_without_a_positive_top() {
        let o = out(true, vec![row("a", "가", "밴", "", "+0"), row("b", "나", "밴", "", "-3")]);
        let bars: Vec<i32> = rows(&o, &none(), false).iter().map(|r| r.bar).collect();
        assert_eq!(bars, vec![BAR_MIN, BAR_MIN]);
    }

    #[test]
    fn names_prefer_the_games_own_and_fit_their_cell() {
        let names: HashMap<String, String> = [("fighter".to_string(), "아주아주긴이름의격투가".to_string())].into();
        let r = &rows(&out(false, vec![row("fighter", "격투가", "탑", "S", "+1")]), &names, false)[0];
        assert!(r.name.starts_with("아주아주") && r.name.ends_with('…'), "{}", r.name);
        assert!(markup::width(&r.name) <= units_for(geom(Mode::Pick).name.1));
    }

    #[test]
    fn unsure_tier_ink_dims_only_while_dim_is_on() {
        let mut o = out(false, vec![row("fighter", "격투가", "탑", "S", "+1")]);
        o.tiers.insert("fighter".into(), TierMark { letter: "S", unsure: true, chg: "" });
        assert_eq!(rows(&o, &none(), true)[0].tier_ink, "fad33e80");
        assert_eq!(rows(&o, &none(), false)[0].tier_ink, "fad33eff");
    }

    #[test]
    fn rows_stop_at_five() {
        let many = (0..7).map(|i| row(&format!("c{}", i), "가", "탑", "", "+1")).collect();
        assert_eq!(rows(&out(false, many), &none(), false).len(), 5);
    }

    #[test]
    fn swap_rows_lead_with_the_lane() {
        let mut o = Output::default();
        o.swap = vec![SwapRow { lane: "정글", id: "swordman".into(), ko: "검사".into() }];
        let r = &rows(&o, &none(), false)[0];
        assert_eq!((r.n.as_str(), r.lane, r.face.as_str(), r.name.as_str()), ("정글", "jungle", "swordman", "검사"));
        assert_eq!((r.tier.as_str(), r.score.as_str(), r.bar), ("", "", 0));
    }

    #[test]
    fn row_cells_never_overlap_in_any_mode() {
        for m in [Mode::Ban, Mode::Pick, Mode::Swap] {
            let g = geom(m);
            let mut x = g.n.0 + g.n.1;
            if let Some(l) = g.lane {
                assert!(l >= x, "{:?} lane", m);
                x = l + LANE;
            }
            assert!(g.face >= x, "{:?} face", m);
            x = g.face + FACE;
            assert!(g.name.0 >= x, "{:?} name", m);
            assert!(g.name.0 + g.name.1 <= if g.stats { TIER_X } else { INNER_RIGHT }, "{:?} name end", m);
        }
        assert!(TIER_X + 18 <= CHG_X && CHG_X + 14 <= BAR_X && BAR_X + BAR_W <= SCORE_X);
        assert!(SCORE_X + 38 <= WHY_X && WHY_X + 60 <= INNER_RIGHT);
    }

    #[test]
    fn subtitle_counts_the_step_being_decided() {
        assert_eq!(subtitle(Mode::Ban, Some((0, 16))), "1 / 16");
        assert_eq!(subtitle(Mode::Pick, Some((15, 16))), "16 / 16");
        assert_eq!(subtitle(Mode::Pick, Some((16, 16))), "16 / 16");
        assert_eq!(subtitle(Mode::Swap, Some((16, 16))), "라인 배치");
        assert_eq!(subtitle(Mode::Pick, None), "");
    }

    #[test]
    fn pill_counts_the_sample_and_flags_the_cold_start() {
        assert_eq!(pill(128), "표본 128판");
        assert_eq!(pill(12), "표본 12판 · 초반");
    }

    #[test]
    fn clip_keeps_text_that_fits() {
        assert_eq!(clip("격투가", 6), "격투가");
    }

    #[test]
    fn clip_ends_long_text_with_an_ellipsis_inside_the_budget() {
        assert_eq!(clip("이단심문관", 7), "이단…");
    }

    #[test]
    fn notes_fill_the_card_in_order() {
        let notes = vec!["가".repeat(10), "나".repeat(10)];
        assert_eq!(fit_notes(&notes, 6, 48), format!("{}\n{}", notes[0], notes[1]));
    }

    #[test]
    fn markup_takes_no_room() {
        let n = markup::name(&"가".repeat(24));
        assert_eq!(fit_notes(&[n.clone(), "나".into()], 2, 48), format!("{}\n나", n));
    }

    #[test]
    fn the_overflowing_note_is_cut_plain_to_the_lines_left() {
        let a = "가".repeat(24 * 4);
        let b = markup::dim(&"나".repeat(24 * 5));
        let got = fit_notes(&[a.clone(), b], 6, 48);
        let (first, rest) = got.split_once('\n').unwrap();
        assert_eq!(first, a);
        assert!(rest.starts_with("나나") && rest.ends_with('…') && !rest.contains('<'), "{}", rest);
        assert!(markup::width(rest) <= 2 * 48);
    }

    #[test]
    fn sources_balance_their_braces() {
        for src in [rank_source((0, 0)), note_source((0, 0)), hint_source((0, 0))] {
            assert_eq!(src.matches('{').count(), src.matches('}').count());
            assert!(!src.contains("{{") && !src.contains("}}"), "an unformatted brace pair");
        }
    }

    #[test]
    fn every_node_the_fill_writes_is_spawned() {
        let rank = rank_source((0, 0));
        for node in [
            "#slot:color", "#chart:image", "#swap:image", "#title:label", "#sub:label", "#idle:label", "#r0:empty",
            "#r4:empty", "#n:label", "#lane:empty", "#top:image", "#support:image", "#face:image", "#name:label",
            "#tier:color", "#chg:label", "#track:color", "#fill:color", "#sc:label", "#why:color", "#t:label",
        ] {
            assert!(rank.contains(node), "{}", node);
        }
        let note = note_source((0, 0));
        for node in ["#chat:image", "#pill:color", "#body:label"] {
            assert!(note.contains(node), "{}", node);
        }
    }

    #[test]
    fn notes_past_a_full_card_are_dropped() {
        let a = "가".repeat(24 * 6);
        assert_eq!(fit_notes(&[a.clone(), "나".into()], 6, 48), a);
    }
}
