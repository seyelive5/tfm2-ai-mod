//! tfm2_ui_probe — RUNTIME EXPERIMENT: can a mod DRAW on the banpick screen?
//! Writes "★" into every champion cell's #tier label, thickens cell borders, and
//! dumps a structure report (cell children / name labels) to ui_probe.txt.
//! Raw casts (not downcast_ref): the game exe and SDK rlib have different crate
//! disambiguators → TypeIds never match; same source + same rustc ⇒ same layout.

use game_view::BanpickChampionSlotRunner;
use engine_ui::runner::LabelRunner;
use mod_api::*;
use std::sync::atomic::{AtomicU64, Ordering};

static FRAME: AtomicU64 = AtomicU64::new(0);

fn base() -> String {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:/tmp".to_string());
    format!("{}/tfm2-overlay", local.replace('\\', "/"))
}

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

unsafe fn as_label(n: &mut Node) -> &mut LabelRunner {
    &mut *(n.runner.as_any_mut() as *mut dyn std::any::Any as *mut LabelRunner)
}

struct E;
impl ModExtension for E {
    fn post_update(&self, _scene: &mut Scene, ui: &mut GameUI, _a: &mut Assets, _dt: f32) {
        let f = FRAME.fetch_add(1, Ordering::Relaxed);
        if ui.query("main.timer_area.timer_text").is_none() {
            return;
        }
        let Some(root) = ui.query_mut("main") else { return };
        let mut report = String::new();
        let Some(champs) = find_mut(root, "champions") else {
            if f % 120 == 0 {
                let _ = std::fs::write(format!("{}/ui_probe.txt", base()), "no #champions node\n");
            }
            return;
        };
        let Some(contents) = find_mut(champs, "contents") else {
            if f % 120 == 0 {
                let _ = std::fs::write(format!("{}/ui_probe.txt", base()), "no #contents under champions\n");
            }
            return;
        };
        report.push_str(&format!("cells={}\n", contents.child.len()));

        for (i, cell) in contents.child.iter_mut().enumerate() {
            if i < 4 {
                let kids: Vec<&str> = cell.child.iter().map(|c| c.id.as_str()).collect();
                report.push_str(&format!("cell{} id={} children=[{}]\n", i, cell.id, kids.join(",")));
            }
            // read the #name label (champion identity check)
            if i < 4 {
                match find_mut(cell, "name") {
                    Some(nl) => {
                        let t = unsafe { as_label(nl) }.text.clone();
                        report.push_str(&format!("  name.text={:?}\n", t));
                    }
                    None => report.push_str("  (no name label)\n"),
                }
            }
            // WRITE 1: tier label ← "★"
            match find_mut(cell, "tier") {
                Some(tl) => {
                    tl.visible = true;
                    let lr = unsafe { as_label(tl) };
                    if lr.text != "★" {
                        lr.text = "★".to_string();
                        tl.runner.set_dirty(true);
                    }
                }
                None => {
                    if i < 4 {
                        report.push_str("  (no tier label)\n");
                    }
                }
            }
            // WRITE 2: thicker border + swap normal↔hover color (no Color construction
            // needed — we only move values the game already owns)
            let sr = unsafe {
                &mut *(cell.runner.as_any_mut() as *mut dyn std::any::Any as *mut BanpickChampionSlotRunner)
            };
            if sr.line_width < 3.0 {
                sr.line_width = 3.0;
                sr.line_color.normal = sr.line_color.hover;
                cell.runner.set_dirty(true);
            }
        }

        if f % 120 == 0 {
            let _ = std::fs::write(format!("{}/ui_probe.txt", base()), &report);
        }
    }
}

fn init(_c: &GameCtx) -> ModRegistration {
    let mut r = ModRegistration::new("tfm2_ui_probe");
    r.set_extension(E);
    r
}

declare_mod!(init);
