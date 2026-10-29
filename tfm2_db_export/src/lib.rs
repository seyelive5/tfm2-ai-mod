//! tfm2_db_export v4.3 — the in-game draft recommender on the stable mod API
//! (game 0.6.5+, `mod-api-stable`). This file is wiring only; the plan, the v4.0
//! defect table and the in-game verification steps live in ROADMAP.md.
//!   db           save records → engine tables (incremental, time-budgeted)
//!   board        banpick screen → side-resolved `Board`
//!   recommender  Board × Tables → `Output` (+ the shared engine; the score model
//!                is unchanged vs 70cb62c — ROADMAP G2)
//!   prior        the game's draft AI → cold-start prior
//!   foresee      the game's draft AI → the opponent's likely next move (E1b),
//!                shown on request (F2); the score model never sees it
//!   ui           Output → our own nodes on the banpick screen
//!   diag         diag.txt / board_log.txt (always on, small)
//!   json         record JSON reader
//! Frame: pre_update reads (records, screen) and computes; post_update draws.
//! Lock order: board TRACK → db DB → db TABLES → recommender's own; INPUTS
//! (here) → DB → TABLES. Nothing takes them in the other direction.

mod board;
mod db;
mod diag;
mod foresee;
mod json;
mod markup;
mod prior;
mod probe;
mod recommender;
mod ui;

use mod_api_stable::*;
use recommender::Output;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) const MOD_TAG: &str = "tfm2_db_export v4.6.1";

/// Numeric per-component version key ("2026.10.0" sorts above "2026.2.0").
pub(crate) fn vkey(v: &str) -> Vec<u32> {
    v.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect()
}

/// pre_update → post_update hand-off (None = not on a banpick screen).
static VIEW: Mutex<Option<ui::View>> = Mutex::new(None);
static IN_GAME: AtomicBool = AtomicBool::new(false);
static IN_BANPICK: AtomicBool = AtomicBool::new(false);
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Per-match engine inputs outside the tables build — athlete terms (keyed by
/// the resolved starters), the AI prior, the screen's champion pool. Applied to
/// TABLES only when they change, and folded into the generation handed to
/// `recommender::update`, whose cache otherwise sees only the board + tables.
#[derive(Default)]
struct Inputs {
    ath_key: String,
    af: HashMap<(String, u8), f32>,
    pocket: HashMap<String, f32>,
    ath_dbg: String,
    applied: String,
    gen: usize,
}

static INPUTS: Mutex<Option<Inputs>> = Mutex::new(None);

fn recommend(ctx: &StableClient, read: &board::Read) -> (Option<Arc<Output>>, String, String) {
    let Some(board) = read.board.as_ref() else {
        return (None, read.status.clone(), String::new());
    };
    if !db::tables_ready() {
        let (n, total) = db::progress();
        return (None, format!("통계 불러오는 중... {}/{}", n, total), String::new());
    }
    let Ok(mut ig) = INPUTS.lock() else {
        return (None, "내부 오류 (lock)".to_string(), String::new());
    };
    let inp = ig.get_or_insert_with(Inputs::default);
    let tables_gen = db::TABLES_GEN.load(Ordering::Relaxed);
    // record reads ≈131 µs each → once per starters change, not per frame
    let ath_key = format!("{:?}|{:?}|{}", read.my_starters, read.enemy_starters, tables_gen);
    if inp.ath_key != ath_key {
        let (af, pocket, dbg) = db::athlete_tables(ctx, &read.my_starters, &read.enemy_starters);
        inp.af = af;
        inp.pocket = pocket;
        inp.ath_dbg = dbg;
        inp.ath_key = ath_key;
    }
    let (prior_map, prior_n) = prior::snapshot();
    // the grid shows what this save can pick; below 10 tiles it is still building
    let pool_n = if read.grid_pool.len() >= 10 { read.grid_pool.len() } else { 0 };
    let sig = format!("{}|{}|{}|{}", tables_gen, inp.ath_key, prior_n, pool_n);
    let Ok(mut tg) = db::TABLES.lock() else {
        return (None, "내부 오류 (lock)".to_string(), String::new());
    };
    let Some(tables) = tg.as_mut() else {
        return (None, "통계 불러오는 중...".to_string(), String::new());
    };
    if inp.applied != sig {
        tables.set_athlete(inp.af.clone(), inp.pocket.clone());
        tables.set_ai_prior(prior_map);
        if pool_n > 0 {
            tables.pool = read.grid_pool.iter().cloned().collect();
        }
        inp.applied = sig;
        inp.gen += 1;
    }
    let out = recommender::update(board, tables, tables_gen.wrapping_mul(1 << 20).wrapping_add(inp.gen));
    let dbg = format!(
        "inputs gen={} athletes {} | af={} pocket={} prior={} pool={}",
        inp.gen,
        inp.ath_dbg,
        inp.af.len(),
        inp.pocket.len(),
        prior_n,
        tables.pool.len()
    );
    (Some(out), read.status.clone(), dbg)
}

fn frame(ctx: &StableClient) {
    let frames = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    let scene = ctx.scene_kind();
    if scene != Some(SceneKindV1::InGame) {
        if IN_GAME.swap(false, Ordering::Relaxed) {
            // save unloaded / switched: nothing carries over
            db::reset();
            board::reset();
            prior::reset();
            foresee::reset();
            probe::reset();
            ui::reset();
            if let Ok(mut i) = INPUTS.lock() {
                *i = None;
            }
            diag::log_line("left InGame: state reset");
        }
        IN_BANPICK.store(false, Ordering::Relaxed);
        if let Ok(mut v) = VIEW.lock() {
            *v = None;
        }
        diag::set("2 scene", format!("scene={:?} (not InGame) frames={}", scene, frames));
        return;
    }
    if !IN_GAME.swap(true, Ordering::Relaxed) {
        diag::log_line("entered InGame");
    }
    let bp = board::is_banpick(ctx);
    // the playback code only shapes background work, so an unverified scene code
    // costs nothing worse than an earlier/later rebuild
    let screen = if bp {
        db::Screen::Banpick
    } else if ctx.client_scene_kind() == Some(ClientSceneKindV1::InGame) {
        db::Screen::Playback
    } else {
        db::Screen::Menu
    };
    db::tick(ctx, screen);
    prior::tick();
    foresee::tick();
    diag::set(
        "2 scene",
        format!(
            "scene=InGame client={:?} main_tab={:?} screen={:?} frames={}",
            ctx.client_scene_kind(),
            ctx.client_main_tab(),
            screen,
            frames
        ),
    );
    if !bp {
        if IN_BANPICK.swap(false, Ordering::Relaxed) {
            board::leave();
            foresee::leave();
            ui::reset();
            if let Ok(mut v) = VIEW.lock() {
                *v = None;
            }
            diag::log_line("left banpick");
        }
        probe::screen(ctx);
        return;
    }
    if !IN_BANPICK.swap(true, Ordering::Relaxed) {
        diag::log_line(&format!("entered banpick (client scene {:?})", ctx.client_scene_kind()));
    }
    let read = board::read(ctx);
    let (out, status, inputs_dbg) = recommend(ctx, &read);
    probe::banpick(ctx, &read, out.as_deref().map(|o| o.rec_ban));
    diag::set(
        "8 rec",
        match out.as_deref() {
            Some(o) => format!(
                "rec_ban={} recs=[{}]\nswap=[{}]\nnotes={} status={:?}\n{}\nsay:\n{}",
                o.rec_ban,
                o.recs.iter().map(|(id, _, sc)| format!("{}{}", id, sc)).collect::<Vec<_>>().join(" "),
                o.swap.iter().map(|r| format!("{} {}", r.lane, r.id)).collect::<Vec<_>>().join(" / "),
                o.notes.len(),
                status,
                inputs_dbg,
                markup::plain(&o.say)
            ),
            None => format!("no output — status={:?}", status),
        },
    );
    let step = read.board.as_ref().map(|b| {
        (b.my_ban.len() + b.enemy_ban.len() + b.my_pick.len() + b.enemy_pick.len(), b.ban_count * 2 + 10)
    });
    let opp = foresee::frame(read.board.as_ref(), &read.grid_pool, &read.names);
    if let Ok(mut v) = VIEW.lock() {
        *v = Some(ui::View {
            out,
            status,
            tiles: read.tiles,
            taken: read.taken,
            my_blue: read.my_blue,
            names: read.names,
            step,
            opp,
        });
    }
}

struct Coach;

impl StableExtension for Coach {
    fn pre_update(&self, ctx: &mut StableClient<'_>, _dt_micros: u64) {
        diag::refresh_debug_dumps();
        frame(ctx);
        diag::flush();
    }

    fn post_update(&self, ctx: &mut StableClient<'_>, _dt_micros: u64) {
        let view = VIEW.lock().ok().and_then(|mut v| v.take());
        // the game's own update ran in between: draw only if the screen survived it
        if let Some(view) = view {
            if board::is_banpick(ctx) {
                ui::stamp(ctx, &view);
            }
        }
    }
}

fn init(host: &StableHost) -> StableMod {
    // The SDK shims catch panics (the game survives), but a panic under one of
    // our locks poisons it and the mod goes quiet. Leave the reason behind. The
    // hook belongs to this DLL's own std — the game's panic handling is untouched.
    static PANICS: AtomicU64 = AtomicU64::new(0);
    std::panic::set_hook(Box::new(|info| {
        if PANICS.fetch_add(1, Ordering::Relaxed) < 20 {
            diag::log_line(&format!("PANIC {}", info));
        }
    }));
    let v = host.game_version();
    host.log(LogLevel::Info, &format!("{} loaded (game {}.{}.{})", MOD_TAG, v.major, v.minor, v.patch));
    diag::set(
        "1 mod",
        format!(
            "{} game={}.{}.{} sdk_abi={} out={}",
            MOD_TAG,
            v.major,
            v.minor,
            v.patch,
            ABI_LEVEL,
            diag::base()
        ),
    );
    diag::log_line(&format!("init {} game {}.{}.{}", MOD_TAG, v.major, v.minor, v.patch));
    foresee::load_setting();
    foresee::load_record(&format!("{}.{}.{}", v.major, v.minor, v.patch));
    let mut m = StableMod::new("tfm2_db_export");
    m.set_extension(Coach);
    m.add_draft_score_hook(prior::Hook);
    m
}

// MatchReplay (the whole data source) + champion icons arrived by level 5; an
// older game gets the loader's "update the game" diagnostic, not an inert mod.
declare_stable_mod!(init, requires = 5);
