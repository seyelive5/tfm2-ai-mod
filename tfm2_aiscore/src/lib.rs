//! tfm2_aiscore (TEMP probe) — captures the GAME's OWN draft-AI scores.
//!
//! `ModDraftScoreHook` hands us `base_score` = the score the game's internal draft AI
//! computed for each candidate (bp_probe IGNORED it as `_base`). The league sim floods
//! this hook, so we aggregate:
//!   - empty-board PICK / BAN scores  → the game's INTRINSIC champ valuation (no synergy/
//!     counter context) = the game's own "tier list", uncorrupted by who-picked-it.
//!   - a few non-empty-board samples → to see whether base_score reacts to allies/enemies
//!     (i.e. whether the dev AI bakes in synergy/counter).
//! Read-only: always returns Pass (changes nothing). Writes C:/tmp/aiscore/aiscore.txt.

use mod_api::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

// champion index -> id (assumed banpick-data.js order; verify names look sane in output)
const ORDER: &[&str] = &[
    "vampire", "monk", "circus_blade", "android", "inquisitor", "lancer", "ninja", "fighter",
    "ice_mage", "wind_mage", "exorcist", "ghost", "pole_warrior", "dual_blader", "demon", "swordman",
    "white_mage", "shadowmancer", "pythoness", "hunter", "jiangshi", "guardian_spirit", "druid",
    "siege_breaker", "knight", "werewolf", "dark_mage", "magic_knight", "illusionist", "berserker",
    "pyromancer", "bomber", "necromancer", "priest", "lightning_mage", "prisoner", "dokkaebi",
    "executioner", "barrier_magician", "shield_bearer", "archer", "gambler", "whip_master", "hitman",
    "chef", "spirit_caller", "taoist", "gunner", "hammerer", "clown", "cavalry_knight", "enchanter",
    "soldier", "plague_doctor", "dancer", "boomerang_hunter", "voodoo_shaman", "poison_dart_hunter",
    "bard", "ogre",
];
fn name(i: usize) -> &'static str {
    ORDER.get(i).copied().unwrap_or("?")
}
fn names(v: &[usize]) -> String {
    v.iter().map(|&i| name(i)).collect::<Vec<_>>().join(",")
}

static CALLS: AtomicU64 = AtomicU64::new(0);
static AGG: Mutex<Option<Agg>> = Mutex::new(None);

#[derive(Default)]
struct Agg {
    pick0: HashMap<usize, (f64, u32)>, // empty-board pick: cand -> (sum base, count)
    ban0: HashMap<usize, (f64, u32)>,  // empty-board ban
    samples: Vec<String>,
}

fn table(title: &str, m: &HashMap<usize, (f64, u32)>, out: &mut String) {
    out.push_str(&format!("\n=== {} (idx  champ  avg_score  n) ===\n", title));
    let mut v: Vec<(&usize, &(f64, u32))> = m.iter().collect();
    v.sort_by(|x, y| {
        let ax = x.1 .0 / x.1 .1.max(1) as f64;
        let ay = y.1 .0 / y.1 .1.max(1) as f64;
        ay.partial_cmp(&ax).unwrap_or(std::cmp::Ordering::Equal)
    });
    for (c, &(sum, n)) in v {
        out.push_str(&format!("{}\t{:<18}\t{:.4}\t{}\n", c, name(*c), sum / n as f64, n));
    }
}

fn dump() {
    let g = match AGG.lock() {
        Ok(x) => x,
        Err(_) => return,
    };
    let a = match g.as_ref() {
        Some(x) => x,
        None => return,
    };
    let mut s = format!("calls={}\n", CALLS.load(Ordering::Relaxed));
    table("empty-board PICK scores", &a.pick0, &mut s);
    table("empty-board BAN scores", &a.ban0, &mut s);
    s.push_str("\n=== context samples (board NOT empty — does base react?) ===\n");
    for l in &a.samples {
        s.push_str(l);
        s.push('\n');
    }
    let _ = std::fs::create_dir_all("C:/tmp/aiscore");
    let _ = std::fs::write("C:/tmp/aiscore/aiscore.txt", s);
}

#[derive(Debug)]
struct Hook;
impl ModDraftScoreHook for Hook {
    fn id(&self) -> &str {
        "tfm2_aiscore"
    }
    fn score_pick(&self, ctx: &DraftScoreContext, cand: usize, base: f32) -> DraftScoreDecision {
        if let Ok(mut g) = AGG.lock() {
            let a = g.get_or_insert_with(Agg::default);
            if ctx.ally_pick.is_empty() && ctx.enemy_pick.is_empty() {
                let e = a.pick0.entry(cand).or_insert((0.0, 0));
                e.0 += base as f64;
                e.1 += 1;
            } else if a.samples.len() < 140 {
                a.samples.push(format!(
                    "PICK {:<16} base={:+.3} | ally=[{}] enemy=[{}] eban=[{}] expl={}",
                    name(cand),
                    base,
                    names(&ctx.ally_pick),
                    names(&ctx.enemy_pick),
                    names(&ctx.enemy_ban),
                    ctx.is_explore
                ));
            }
        }
        if CALLS.fetch_add(1, Ordering::Relaxed) % 50000 == 0 {
            dump();
        }
        DraftScoreDecision::Pass
    }
    fn score_ban(&self, ctx: &DraftScoreContext, cand: usize, base: f32) -> DraftScoreDecision {
        if let Ok(mut g) = AGG.lock() {
            let a = g.get_or_insert_with(Agg::default);
            if ctx.ally_pick.is_empty()
                && ctx.enemy_pick.is_empty()
                && ctx.ally_ban.is_empty()
                && ctx.enemy_ban.is_empty()
            {
                let e = a.ban0.entry(cand).or_insert((0.0, 0));
                e.0 += base as f64;
                e.1 += 1;
            }
        }
        if CALLS.fetch_add(1, Ordering::Relaxed) % 50000 == 0 {
            dump();
        }
        DraftScoreDecision::Pass
    }
}

fn init(_c: &GameCtx) -> ModRegistration {
    let mut r = ModRegistration::new("tfm2_aiscore");
    r.add_draft_score_hook(Hook);
    r
}

declare_mod!(init);
