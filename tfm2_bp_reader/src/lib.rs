//! tfm2_bp_reader — reads the LIVE banpick board from the banpick screen's
//! `MatchUIRunner` (game_view) and exports it for the recommender overlay.
//!
//! Route (compiler-proven 2026-06-11): `GameUI::query("main")` → `Node.runner`
//! (pub field) → `as_any().downcast_ref::<game_view::MatchUIRunner>()` (pub
//! re-export at the crate root) → pub fields `team1_ban/team2_ban/team1_pick/
//! team2_pick: Vec<String>` (champion ids) + `team1/team2: usize` (team ids,
//! compared against the roster's MYTEAM to tell which side is the player).
//! This is the same lookup the game itself performs (cf. the exe diagnostic
//! "[SPECTATE] 'main' node found but no MatchUIRunner").
//!
//! Writes (atomic, deduped) %LOCALAPPDATA%/tfm2-overlay/bp_live.txt:
//!   ts=<unix ms>            (freshness)
//!   my_team=<id or ->       (player team id when the scene exposes it)
//!   team1=/team2=<id>       (board sides)
//!   team1_ban=/team2_ban=/team1_pick=/team2_pick=<comma ids>
//!   team1_order=/team2_order=<comma usize>  (pick-slot order indices)
//! Read-only w.r.t. the game. Outside the banpick screen "main" resolves to a
//! different/absent runner → no writes (the file simply goes stale).

use game_view::MatchUIRunner;
use mod_api::*;
use std::sync::Mutex;

static LAST: Mutex<String> = Mutex::new(String::new());

fn base() -> String {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:/tmp".to_string());
    format!("{}/tfm2-overlay", local.replace('\\', "/"))
}

struct E;
impl ModExtension for E {
    fn pre_update(&self, scene: &mut Scene, ui: &mut GameUI, _a: &mut Assets, _dt: f32) {
        let my_team = match scene {
            Scene::InGame { data } => format!("{}", data.player_team_id()),
            _ => "-".to_string(),
        };
        // GATE 1: only in the banpick screen (a banpick-only node must exist).
        if ui.query("main.timer_area.timer_text").is_none() {
            return;
        }
        let Some(node) = ui.query("main") else { return };
        // The game exe and the SDK rlib are separate compile sessions of the same
        // source → different crate disambiguators → different TypeIds → safe
        // `downcast_ref` ALWAYS fails (diag-proven: expected 0x2d59…, actual 0x11b8…).
        // Same source + same rustc (f20a92ec0) ⇒ identical struct LAYOUT, so a raw
        // cast is sound — guarded by GATE 1 above (we're on the banpick screen,
        // whose "main" runner is MatchUIRunner — the game's own spectate diagnostic
        // relies on exactly that) and GATE 2 below (full content validation; any
        // implausible value ⇒ reject, publish nothing).
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

        let key = format!(
            "my_team={}\nteam1={}\nteam2={}\nban_count={}\nteam1_ban={}\nteam2_ban={}\nteam1_pick={}\nteam2_pick={}\nteam1_order={}\nteam2_order={}\n",
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
        );
        let mut last = match LAST.lock() {
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
        if std::fs::write(&tmp, format!("ts={}\n{}", ms, key)).is_ok()
            && std::fs::rename(&tmp, &path).is_ok()
        {
            *last = key;
        }
    }
}

fn init(_c: &GameCtx) -> ModRegistration {
    let mut r = ModRegistration::new("tfm2_bp_reader");
    r.set_extension(E);
    r
}

declare_mod!(init);
