//! bp_probe — settles whether a mod can read the LIVE banpick board.
//!
//! Hypothesis (CLAUDE.md §4a): client `send_mod_command` → `ClientPacket::ModCommand`
//! (binary-confirmed sibling of `BanpickSelect`) → server `handle_command` →
//! `ctx.server_state.running_matches` → `set_info` (the live draft board). The ONE
//! unproven link is whether the in-process server worker dispatches the command to
//! `handle_command` DURING the manual banpick (while the management tick is paused),
//! or buffers it until the day is proceeded.
//!
//! This mod logs exactly that to C:\tmp\bp_probe.log and changes nothing in-game:
//!   - Client `pre_update` (fires during banpick): ~4×/sec `send_mod_command("scan")`,
//!     then drains any `scan_result` events back from the server.
//!   - Server `handle_command`: on "scan", logs that it FIRED + dumps every
//!     `running_matches[*].set_info[*]`, and echoes the snapshot back to the client.
//!
//! PASS  = during banpick, [S-HANDLE] lines appear AND a running_matches entry's
//!         set_info shows a banpick phase whose picks/bans grow as you click.
//! FAIL  = no [S-HANDLE] during banpick (buffered until proceed) OR running_matches
//!         empty / only unrelated decided sims / never reflects your live picks.

use mod_api::*;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

const MOD_ID: &str = "bp_probe";
const LOG: &str = "C:\\tmp\\bp_probe.log";

static CFRAME: AtomicU64 = AtomicU64::new(0);
static SCMD: AtomicU64 = AtomicU64::new(0);
static TICK: AtomicU64 = AtomicU64::new(0);
// draft-score-hook observation. KEY test: is the candidate PLACED into the board
// being scored (→ ONE match scored per-candidate, committed board = real draft) or
// independent of it (→ separate matches)? Log cand + whether it's the last elem of
// a pick/ban list. Cap raw lines so the log stays analyzable.
static DLOG: AtomicU64 = AtomicU64::new(0);

fn log(line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(LOG) {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "{} {}", t, line);
    }
}

struct Client;

impl ModExtension for Client {
    fn pre_update(&self, scene: &mut Scene, _ui: &mut GameUI, _a: &mut Assets, _dt: f32) {
        let Scene::InGame { data } = scene else {
            return;
        };
        let n = CFRAME.fetch_add(1, Ordering::Relaxed);
        if n % 15 == 0 {
            let ok = data.send_mod_command(MOD_ID, "scan", Vec::<u8>::new());
            log(&format!("[C] frame={} send_mod_command->{}", n, ok));
        }
        for ev in data.take_mod_events(MOD_ID) {
            let body = String::from_utf8_lossy(&ev.payload);
            log(&format!("[C-EVENT] {} :: {}", ev.event, body));
        }
    }
}

struct Server;

impl ModServerExtension for Server {
    fn on_server_start(&self, _ctx: &mut ServerModContext) {
        log("[S] on_server_start");
    }

    // Does the management tick run, and does it drain the queued "scan" commands
    // (i.e. does handle_command fire right after a tick)? And what does the server
    // see in running_matches AT tick time vs during a (paused) banpick?
    fn before_management_tick(&self, ctx: &mut ServerModContext) {
        let n = TICK.fetch_add(1, Ordering::Relaxed);
        let rms = &ctx.server_state.running_matches;
        log(&format!("[S-TICK #{}] running_matches={}", n, rms.len()));
        for (mid, rm) in rms.iter() {
            for (i, si) in rm.set_info.iter().enumerate() {
                log(&format!("[S-TICK-SNAP] m{} set{} {:?}", mid, i, si));
            }
        }
    }

    fn handle_command(
        &self,
        ctx: &mut ServerModContext,
        command: &ModServerCommand,
    ) -> ModServerCommandResult {
        if command.command.as_str() != "scan" {
            return ModServerCommandResult::Pass;
        }
        let n = SCMD.fetch_add(1, Ordering::Relaxed);
        let rms = &ctx.server_state.running_matches;
        log(&format!("[S-HANDLE #{}] *** FIRED *** running_matches={}", n, rms.len()));
        let mut snap = String::new();
        for (mid, rm) in rms.iter() {
            snap.push_str(&format!("m{}(sets={}) ", mid, rm.set_info.len()));
            for (i, si) in rm.set_info.iter().enumerate() {
                snap.push_str(&format!("[set{} {:?}] ", i, si));
            }
        }
        log(&format!("[S-SNAP #{}] {}", n, snap));
        ctx.emit_event_to_command_sender(command, "scan_result", snap.into_bytes());
        ModServerCommandResult::Handled
    }
}

// The MOST promising live-read path: this hook fires DURING the banpick with the
// actual ally/enemy picks+bans. The only problem was sim pollution — so we test
// whether `is_explore == false` isolates the human's real board. We log every
// DISTINCT is_explore=false board (dedup) + count the explore=true noise, so the
// log reveals: one clean growing board (=isolable → auto-read works) vs churn.
#[derive(Debug)]
struct DraftHook;

impl ModDraftScoreHook for DraftHook {
    fn id(&self) -> &str {
        "bp_probe:draftlog"
    }
    fn score_ban(&self, ctx: &DraftScoreContext, cand: usize, _base: f32) -> DraftScoreDecision {
        observe_draft(ctx, cand);
        DraftScoreDecision::Pass
    }
    fn score_pick(&self, ctx: &DraftScoreContext, cand: usize, _base: f32) -> DraftScoreDecision {
        observe_draft(ctx, cand);
        DraftScoreDecision::Pass
    }
}

fn observe_draft(ctx: &DraftScoreContext, cand: usize) {
    let n = DLOG.fetch_add(1, Ordering::Relaxed);
    if n >= 25000 {
        return; // cap the raw log
    }
    // is the candidate the LAST element of one of the board's lists? if so, the
    // candidate is being placed into a hypothetical board → committed board = real.
    let placed = if ctx.ally_ban.last() == Some(&cand) {
        "ab"
    } else if ctx.enemy_ban.last() == Some(&cand) {
        "eb"
    } else if ctx.ally_pick.last() == Some(&cand) {
        "ap"
    } else if ctx.enemy_pick.last() == Some(&cand) {
        "ep"
    } else {
        "NONE"
    };
    log(&format!(
        "[D#{}] cand={} placed={} expl={} {:?} ap={:?} ep={:?} ab={:?} eb={:?} avail={}",
        n, cand, placed, ctx.is_explore, ctx.phase,
        ctx.ally_pick, ctx.enemy_pick, ctx.ally_ban, ctx.enemy_ban, ctx.available_champions.len()
    ));
}

fn init(_c: &GameCtx) -> ModRegistration {
    log("[INIT] bp_probe loaded");
    let mut r = ModRegistration::new(MOD_ID);
    r.set_extension(Client);
    r.set_server_extension(Server);
    r.add_draft_score_hook(DraftHook);
    r
}

declare_mod!(init);
