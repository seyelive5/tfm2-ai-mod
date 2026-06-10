//! draftwatch (gameview locator) — game_view is Debug-dumpable. During banpick
//! (client pre_update fires; mgmt tick is paused), dump format!("{:?}", db.game_view)
//! to C:\tmp\tfm2_gameview_dump.txt whenever it changes, so we can see how the live
//! draft (picks/bans) appears and where, then parse it.

use mod_api::*;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

static LAST_LEN: AtomicUsize = AtomicUsize::new(0);

fn log(s: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
        .open("C:\\tmp\\draftwatch.log") { let _ = writeln!(f, "{}", s); }
}
fn write_file(path: &str, s: &str) {
    if let Ok(mut f) = std::fs::File::create(path) { let _ = f.write_all(s.as_bytes()); }
}

struct E;
impl ModExtension for E {
    fn pre_update(&self, scene: &mut Scene, _ui: &mut GameUI, _a: &mut Assets, _dt: f32) {
        if let Scene::InGame { data } = scene {
            let db = data.db();
            let s = format!("{:?}", db.game_view);
            let len = s.len();
            if len <= 8 { return; } // "None" etc — nothing yet
            let prev = LAST_LEN.swap(len, Ordering::Relaxed);
            if prev != len {
                write_file("C:\\tmp\\tfm2_gameview_dump.txt", &s);
                log(&format!("gameview dump: {} bytes (monk={}, gunner={}, knight={}, pick={}, ban={})",
                    len, s.contains("monk"), s.contains("gunner"), s.contains("knight"),
                    s.contains("pick"), s.contains("ban")));
            }
        }
    }
}

fn init(_c: &GameCtx) -> ModRegistration {
    log("init: draftwatch (gameview) loaded");
    let mut r = ModRegistration::new("draftwatch");
    r.set_extension(E);
    r
}
declare_mod!(init);
