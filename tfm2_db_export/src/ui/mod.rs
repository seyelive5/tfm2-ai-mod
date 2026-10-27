//! Display on the game's banpick screen. Rule (ROADMAP §1 G4): game-owned nodes
//! are never edited — everything shown lives in nodes we spawn, and all of them
//! but the two card bodies are `ignore_event`.
//!   cards  — two cards drawn like champion_info's skill boxes (cards.rs): the
//!            rank card (top 5, or the lane plan once my five are in) and the
//!            commentary. Draft: over champion_info — rank on the middle skill
//!            box, commentary on the end away from the player's own coach, whose
//!            dialogue toasts stack up from their side's bottom corner. Swap:
//!            stacked over the opponent's table, right end (layout.rs). A card
//!            body takes the mouse, so the skill box it covers never pops its
//!            tooltip from under it. No title bar or buttons: the game cannot
//!            drag a mod's popup (its draggable_popup runner drags by a handle
//!            height + min size + resize border that game code writes into each
//!            of its own popups after building them, and no property or SDK
//!            call reaches; 2026-10-27 tests). Tab hides
//!            both for the rest of this banpick screen, leaving a small hint.
//!   tiles  — per tile: rank badge + tier chip in the free top-left corner and
//!            an outline ring in the turn color (tiles.rs).
//! Draw order: the draft cards hang from champion_info, so the game's later
//! `main` children (tooltips, popups, the phase banner) still draw over them;
//! the swap cards hang from `main` itself (the swap tables stack their
//! children) and stand aside while one of those overlays is over them.
//! A node is written only when its value differs from what we applied.
//! `.ui` grammar and property values are the ones the game's own templates use
//! (0.6.5 bundle): `#child:kind`, a bare style line, `size`/`align_*` on labels,
//! `line_height` + the label's own wrapping for multi-line text, on `color`
//! runners with `stroke` the `color` is the OUTLINE and `back_color` the fill,
//! `image` runners take `source` and a `color` tint.

mod cards;
mod layout;
mod tiles;

use crate::diag;
use crate::markup;
use crate::recommender::Output;
use cards::Mode;
use mod_api_stable::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tiles::TileLook;

/// What the frame's reading + recommendation produced, for the stamp pass.
#[derive(Clone, Default)]
pub(crate) struct View {
    pub out: Option<Arc<Output>>,
    pub status: String,
    pub tiles: Vec<String>,
    pub taken: HashSet<String>,
    /// my side (None = unresolved) — picks the commentary card's end
    pub my_blue: Option<bool>,
    /// the game's own champion names (board::Read::names)
    pub names: Arc<HashMap<String, String>>,
    /// (bans + picks made, the draft's total) for the rank card's step counter
    pub step: Option<(usize, usize)>,
}

/// Node writes. `put_*` write; `text` / `props` / `vis` / `face` write only what
/// differs from the last successful write, keyed by path (+ property name).
#[derive(Default)]
struct Applied {
    map: HashMap<String, String>,
    set_fails: usize,
    icon_fails: usize,
    /// texts read back unequal to what was set (first example kept)
    readback_bad: usize,
    readback_first: String,
}

impl Applied {
    fn same(&self, key: &str, val: &str) -> bool {
        self.map.get(key).map(String::as_str) == Some(val)
    }

    fn put_text(&mut self, ctx: &mut StableClient, path: &str, s: &str) -> bool {
        if !ctx.ui_set_text(path, s) {
            self.set_fails += 1;
            return false;
        }
        // garbled-text report (2026-10-27): stored ≠ set means the host mangles the
        // string; stored = set points at rendering instead
        let got = ctx.ui_text(path);
        if got.as_deref() != Some(s) {
            self.readback_bad += 1;
            if self.readback_first.is_empty() {
                self.readback_first = format!("{} want={:?} got={:?}", path, s, got);
            }
        }
        true
    }

    fn put_props(&mut self, ctx: &mut StableClient, path: &str, props: &str) -> bool {
        let ok = ctx.ui_set_properties(path, props);
        if !ok {
            self.set_fails += 1;
        }
        ok
    }

    fn put_vis(&mut self, ctx: &mut StableClient, path: &str, v: bool) -> bool {
        let ok = ctx.ui_set_visible(path, v);
        if !ok {
            self.set_fails += 1;
        }
        ok
    }

    fn text(&mut self, ctx: &mut StableClient, path: &str, s: &str) {
        let key = format!("{}|text", path);
        if !self.same(&key, s) && self.put_text(ctx, path, s) {
            self.map.insert(key, s.to_string());
        }
    }

    /// Plain `key: value;` pairs only (no blocks); each property cached apart.
    fn props(&mut self, ctx: &mut StableClient, path: &str, props: &str) {
        let pairs: Vec<(String, &str)> = props
            .split(';')
            .map(str::trim)
            .filter(|kv| !kv.is_empty())
            .map(|kv| (format!("{}|{}", path, kv.split(':').next().unwrap_or("").trim()), kv))
            .filter(|(key, kv)| !self.same(key, kv))
            .collect();
        if pairs.is_empty() {
            return;
        }
        let send: String = pairs.iter().map(|(_, kv)| format!("{}; ", kv)).collect();
        if self.put_props(ctx, path, &send) {
            for (key, kv) in pairs {
                self.map.insert(key, kv.to_string());
            }
        }
    }

    fn vis(&mut self, ctx: &mut StableClient, path: &str, v: bool) {
        let key = format!("{}|vis", path);
        let val = if v { "1" } else { "0" };
        if !self.same(&key, val) && self.put_vis(ctx, path, v) {
            self.map.insert(key, val.to_string());
        }
    }

    /// A champion face into an image node; false = the host could not (the
    /// caller keeps the node hidden). One attempt per champion.
    fn face(&mut self, ctx: &mut StableClient, path: &str, champ: &str, px: f32) -> bool {
        let key = format!("{}|face", path);
        if let Some(v) = self.map.get(&key) {
            if v.strip_prefix('!').unwrap_or(v) == champ {
                return !v.starts_with('!');
            }
        }
        let ok = ctx.ui_set_champion_icon(path, champ, px, px, 2.0);
        if !ok {
            self.icon_fails += 1;
        }
        self.map.insert(key, if ok { champ.to_string() } else { format!("!{}", champ) });
        ok
    }

    /// Drop what we know about `path` and its subtree (the node is gone or new).
    fn forget(&mut self, path: &str) {
        let (own, sub) = (format!("{}|", path), format!("{}.", path));
        self.map.retain(|k, _| !k.starts_with(&own) && !k.starts_with(&sub));
    }
}

#[derive(Default)]
struct UiState {
    ap: Applied,
    /// where the cards hang now (None = nothing spawned on this screen yet)
    parent: Option<&'static str>,
    /// frame of the last failed card spawn → retried every 120 frames
    card_fail_at: Option<u64>,
    /// Tab: cards hidden until this banpick screen is left
    hidden: bool,
    mode: Mode,
    /// the overlay the swap cards stand aside for
    yield_to: Option<&'static str>,
    /// last applied look per tile; absent = nodes not (yet) verified
    tiles: HashMap<String, TileLook>,
    /// tiles whose spawn failed → retried only every 120 frames
    tile_fail: HashMap<String, u64>,
    frame: u64,
    spawns: usize,
    spawn_fails: usize,
    tabs: usize,
    keys: Vec<String>,
}

static UI: Mutex<Option<UiState>> = Mutex::new(None);

/// Left the banpick screen: the next one starts shown, with fresh caches.
pub(crate) fn reset() {
    if let Ok(mut g) = UI.lock() {
        *g = None;
    }
}

/// Typing in the champion search box must not toggle the cards.
fn typing(ctx: &StableClient) -> bool {
    ctx.ui_state_json("main.champion_search")
        .map(|j| j.replace(' ', "").contains("\"is_editing\":true"))
        .unwrap_or(false)
}

/// A subtree as lines (path "" = the root): name, runner, visible, rect, label
/// text — `max_depth` levels below `path`, until `out` holds `cap` lines.
pub(crate) fn dump_tree(ctx: &StableClient, path: &str, depth: usize, max_depth: usize, cap: usize, out: &mut Vec<String>) {
    if out.len() >= cap {
        return;
    }
    let runner = ctx.ui_runner_name(path).unwrap_or_default();
    let mut line = format!(
        "{}{} [{}] vis={:?} rect={:?}",
        "  ".repeat(depth),
        if path.is_empty() { "(root)" } else { path.rsplit('.').next().unwrap_or(path) },
        runner,
        ctx.ui_visible(path),
        ctx.ui_node_rect(path).map(|(x, y, w, h)| (x as i32, y as i32, w as i32, h as i32))
    );
    if let Some(t) = ctx.ui_text(path).filter(|t| !t.is_empty()) {
        line.push_str(&format!(" text={:?}", t.chars().take(64).collect::<String>()));
    }
    out.push(line);
    if depth < max_depth {
        for c in ctx.ui_child_names(path) {
            let child = if path.is_empty() { c } else { format!("{}.{}", path, c) };
            dump_tree(ctx, &child, depth + 1, max_depth, cap, out);
        }
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
        st.hidden = !st.hidden;
    }

    // unsure tier letters are drawn dimmed; at a cold start that is every letter,
    // and the rank card says the list is provisional (cards::TierDim)
    let dim = view.out.as_deref().map_or(cards::TierDim::Off, |o| cards::tier_dim(&o.tiers));
    let swap = ctx.ui_visible("main.swap") == Some(true);
    let place = layout::place(swap, view.my_blue == Some(false));
    cards::stamp(ctx, st, view, &place, dim);
    tiles::stamp(ctx, st, view, dim.on());

    if st.frame % 30 == 1 {
        let ranked = st.tiles.values().filter(|l| l.ring.is_some()).count();
        let tagged = st.tiles.values().filter(|l| l.chip.is_some()).count();
        let rank = format!("{}.{}", place.parent, cards::RANK);
        let note = format!("{}.{}", place.parent, cards::NOTE);
        diag::set(
            "6 ui",
            format!(
                "cards parent={:?} mode={:?} hidden_by_tab={} yield_to={:?} tier_dim={:?} markup_on={}\nrank at={:?} rect={:?} vis={:?} | note at={:?} rect={:?} vis={:?}\ninfo_visible={:?} swap_visible={:?} my_blue={:?} step={:?}\ntiles on_screen={} with_nodes={} ranked={} tagged={} spawn_retry={}\nspawns={} spawn_fails={} set_fails={} icon_fails={} readback_bad={}\ntab_presses={} keys_seen={:?} search_state={:?}",
                st.parent,
                st.mode,
                st.hidden,
                st.yield_to,
                dim,
                markup::ON,
                place.rank,
                ctx.ui_node_rect(&rank),
                ctx.ui_visible(&rank),
                place.note,
                ctx.ui_node_rect(&note),
                ctx.ui_visible(&note),
                ctx.ui_visible("main.champion_info"),
                ctx.ui_visible("main.swap"),
                view.my_blue,
                view.step,
                view.tiles.len(),
                st.tiles.len(),
                ranked,
                tagged,
                st.tile_fail.len(),
                st.spawns,
                st.spawn_fails,
                st.ap.set_fails,
                st.ap.icon_fails,
                st.ap.readback_bad,
                st.tabs,
                st.keys,
                ctx.ui_state_json("main.champion_search"),
            ),
        );
    }
    if st.frame % 60 == 2 {
        let mut lines = vec![format!(
            "readback_bad={} first={}",
            st.ap.readback_bad,
            if st.ap.readback_first.is_empty() { "-" } else { st.ap.readback_first.as_str() }
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
        if let Some(parent) = st.parent {
            dump_tree(ctx, &format!("{}.{}", parent, cards::RANK), 0, 2, 90, &mut lines);
            dump_tree(ctx, &format!("{}.{}", parent, cards::NOTE), 0, 1, 90, &mut lines);
        }
        let tile = st
            .tiles
            .iter()
            .find(|(_, l)| l.badge.is_some())
            .map(|(c, _)| c.clone())
            .or_else(|| view.tiles.first().cloned());
        if let Some(c) = tile {
            dump_tree(ctx, &format!("main.champions.contents.{}", c), 0, 2, 90, &mut lines);
        }
        diag::set("9 tree", lines.join("\n"));
    }
}
