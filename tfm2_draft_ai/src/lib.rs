//! TFM2 Draft AI — additive `ModDraftScoreHook` that makes the base ban/pick AI
//! draft better using (a) real match statistics and (b) pro-MOBA draft theory.
//! Design: analysis/draft-ai-design.md. Data: src/draft_weights.tsv (include_str!).
//!
//! candidate/ally_pick/enemy_pick are champion INDICES into CHAMPION_ORDER
//! (banpick-data.js order). Verify in-game once via the score_pick log.

use mod_api::*;
use std::collections::HashMap;
use std::sync::Mutex;

const MOD_ID: &str = "tfm2_draft_ai";
const WEIGHTS_TSV: &str = include_str!("draft_weights.tsv");

// TEMP diagnostic v2: capture TEMPORAL order to tell a single manual draft apart
// from concurrent AI sims. Log only when the (ally,enemy) tuple CHANGES from the
// immediately-previous logged one (collapses per-candidate repeats, keeps order).
static DIAG_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_LAST: Mutex<String> = Mutex::new(String::new());
fn diag_snapshot(phase: &str, explore: bool, allies: &[&str], enemies: &[&str], avail: usize) {
    let cur = format!("{}|x{}|A:{}|E:{}|n{}", phase, explore as u8, allies.join(","), enemies.join(","), avail);
    if let Ok(mut last) = DIAG_LAST.lock() {
        if *last == cur { return; }
        *last = cur;
    }
    let seq = DIAG_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // wall-clock millis: sim bursts share a timestamp; the player's interactive
    // picks are spread seconds apart → use the gap to isolate the real draft.
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open("C:\\tmp\\draft_ai_diag.log")
    {
        let _ = writeln!(f, "{} #{:04} {} avail={} ally=[{}] enemy=[{}]",
            ms, seq, phase, avail, allies.join(","), enemies.join(","));
    }
}

// IPC for the player-facing overlay: write the CURRENT draft board (all four
// arrays as champion ids/names + phase + explore + avail + wall-clock ts) to a
// small state file the overlay reads each frame. De-duped on the non-ts key so
// we rewrite only when the board actually changes (score_* is called once per
// candidate with an identical board). Atomic write (tmp + rename) so the overlay
// never reads a half-written file. NAMES (not indices) are written so the
// overlay needs no champion-order table and there is one less index-mismatch bug.
static STATE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static STATE_LAST: Mutex<String> = Mutex::new(String::new());
fn write_state(
    phase: &str, explore: bool,
    ally_pick: &[&str], enemy_pick: &[&str],
    ally_ban: &[&str], enemy_ban: &[&str], avail: usize,
) {
    let (ap, ep) = (ally_pick.join(","), enemy_pick.join(","));
    let (ab, eb) = (ally_ban.join(","), enemy_ban.join(","));
    let key = format!("{}|{}|{}|{}|{}|{}|{}", phase, explore as u8, ap, ep, ab, eb, avail);
    // Hold STATE_LAST across the WHOLE write. The hook can fire from concurrent
    // AI-sim threads; serializing here makes the file-replace order match the
    // dedup-decision order (consistent last-writer-wins) instead of racing on
    // rename(). The write is tiny + deduped, so serialization cost is nil.
    let mut last = match STATE_LAST.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if *last == key { return; }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let body = format!(
        "ts={}\nphase={}\nexplore={}\nally_pick={}\nenemy_pick={}\nally_ban={}\nenemy_ban={}\navail={}\n",
        ms, phase, explore as u8, ap, ep, ab, eb, avail,
    );
    use std::io::Write;
    let seq = STATE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = format!("C:\\tmp\\tfm2_draft_state_{}.tmp", seq);
    let mut wrote = false;
    if let Ok(mut f) = std::fs::File::create(&tmp) {
        if f.write_all(body.as_bytes()).is_ok() {
            wrote = true;
        }
    }
    // tmp + rename so the overlay never reads a half-written file. The reader
    // (std::fs::read_to_string) opens with FILE_SHARE_DELETE, so MoveFileExW can
    // replace the destination even while a read is in flight. Advance the dedup
    // key only on a fully successful write so a transient failure is retried.
    let ok = wrote && std::fs::rename(&tmp, "C:\\tmp\\tfm2_draft_state.txt").is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    if ok {
        *last = key;
    }
}

// Index basis for candidate:usize. Must match banpick-data.js champions[] order.
const CHAMPION_ORDER: &[&str] = &[
    "vampire","monk","circus_blade","android","inquisitor","lancer","ninja","fighter",
    "ice_mage","wind_mage","exorcist","ghost","pole_warrior","dual_blader","demon","swordman",
    "white_mage","shadowmancer","pythoness","hunter","jiangshi","guardian_spirit","druid",
    "siege_breaker","knight","werewolf","dark_mage","magic_knight","illusionist","berserker",
    "pyromancer","bomber","necromancer","priest","lightning_mage","prisoner","dokkaebi",
    "executioner","barrier_magician","shield_bearer","archer","gambler","whip_master","hitman",
    "chef","spirit_caller","taoist","gunner","hammerer","clown","cavalry_knight","enchanter",
    "soldier","plague_doctor","dancer","boomerang_hunter","voodoo_shaman","poison_dart_hunter",
    "bard","ogre",
];

// ---- tunable weights (start values; tune in-game) ----
const SCALE: f32 = 30.0;     // overall magnitude vs base_score (TUNE after logging base range)
const W_SYN: f32 = 1.2;      // ally synergy
const W_CNT: f32 = 1.0;      // counters enemy (×order ramp)
const W_WEAK: f32 = 1.0;     // countered by enemy (×order ramp)
const W_COMP: f32 = 0.6;     // team-comp need fill
const W_DUP: f32 = 0.4;      // role/category overlap penalty
const W_BAN_CNT: f32 = 1.0;  // ban: strong into our picks
const W_BAN_SYN: f32 = 0.8;  // ban: completes enemy synergy
const W_BAN_META: f32 = 0.5; // ban: high banrate
const COMP_UNIT: f32 = 0.12; // winrate-unit value of filling one comp need

#[derive(Default)]
struct Taxon { category: String, damage: String, tags: Vec<String> }

struct Tables {
    champ_delta: HashMap<String, f32>,
    synergy: HashMap<(String, String), f32>,
    counter: HashMap<(String, String), f32>,
    taxon: HashMap<String, Taxon>,
    banrate: HashMap<String, f32>,
}

fn parse_tables() -> Tables {
    let mut t = Tables {
        champ_delta: HashMap::new(), synergy: HashMap::new(), counter: HashMap::new(),
        taxon: HashMap::new(), banrate: HashMap::new(),
    };
    for line in WEIGHTS_TSV.lines() {
        if line.starts_with('#') || line.is_empty() { continue; }
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "C" if f.len() >= 3 => { t.champ_delta.insert(f[1].into(), f[2].parse().unwrap_or(0.0)); }
            "S" if f.len() >= 4 => { t.synergy.insert((f[1].into(), f[2].into()), f[3].parse().unwrap_or(0.0)); }
            "K" if f.len() >= 4 => { t.counter.insert((f[1].into(), f[2].into()), f[3].parse().unwrap_or(0.0)); }
            "T" if f.len() >= 4 => {
                let tags = if f.len() >= 5 { f[4].split(',').map(|s| s.to_string()).collect() } else { Vec::new() };
                t.taxon.insert(f[1].into(), Taxon { category: f[2].into(), damage: f[3].into(), tags });
            }
            "B" if f.len() >= 3 => { t.banrate.insert(f[1].into(), f[2].parse().unwrap_or(0.0)); }
            _ => {}
        }
    }
    t
}

fn name_of(idx: usize) -> Option<&'static str> { CHAMPION_ORDER.get(idx).copied() }

impl Tables {
    fn delta(&self, c: &str) -> f32 { *self.champ_delta.get(c).unwrap_or(&0.0) }
    fn syn(&self, a: &str, b: &str) -> f32 { *self.synergy.get(&(a.into(), b.into())).unwrap_or(&0.0) }
    fn cnt(&self, a: &str, b: &str) -> f32 { *self.counter.get(&(a.into(), b.into())).unwrap_or(&0.0) }
    fn ban(&self, c: &str) -> f32 { *self.banrate.get(c).unwrap_or(&0.0) }
    fn has_tag(&self, c: &str, tag: &str) -> bool {
        self.taxon.get(c).map(|t| t.tags.iter().any(|x| x == tag)).unwrap_or(false)
    }
    fn damage(&self, c: &str) -> &str { self.taxon.get(c).map(|t| t.damage.as_str()).unwrap_or("AD") }
    fn category(&self, c: &str) -> &str { self.taxon.get(c).map(|t| t.category.as_str()).unwrap_or("") }

    /// Names of allies already picked (skip out-of-range indices).
    fn names<'a>(&self, idxs: &[usize]) -> Vec<&'static str> {
        idxs.iter().filter_map(|&i| name_of(i)).collect()
    }

    /// Comp-need term: reward candidate for filling a missing role on the ally team.
    fn comp_need(&self, cand: &str, allies: &[&str]) -> f32 {
        let team_has = |tag: &str| allies.iter().any(|a| self.has_tag(a, tag));
        let mut s = 0.0;
        // frontline/tank
        if !allies.iter().any(|a| self.has_tag(a, "Tank")) && self.has_tag(cand, "Tank") { s += COMP_UNIT; }
        else if !allies.iter().any(|a| self.has_tag(a, "Frontline")) && self.has_tag(cand, "Frontline") { s += COMP_UNIT * 0.5; }
        if !team_has("CC") && self.has_tag(cand, "CC") { s += COMP_UNIT * 0.7; }
        if !(team_has("Heal") || team_has("Shield")) && (self.has_tag(cand, "Heal") || self.has_tag(cand, "Shield")) { s += COMP_UNIT * 0.6; }
        if !(team_has("Backline") || self.category_team(allies, "Range")) && (self.has_tag(cand, "Backline") || self.category(cand) == "Range") { s += COMP_UNIT * 0.6; }
        // damage-type balance: reward opposite type if team is mono
        let ad = allies.iter().filter(|a| self.damage(a).contains("AD")).count();
        let ap = allies.iter().filter(|a| self.damage(a).contains("AP")).count();
        if !allies.is_empty() {
            if ap == 0 && self.damage(cand).contains("AP") { s += COMP_UNIT * 0.5; }
            if ad == 0 && self.damage(cand).contains("AD") { s += COMP_UNIT * 0.5; }
        }
        s
    }
    fn category_team(&self, allies: &[&str], cat: &str) -> bool {
        allies.iter().any(|a| self.category(a) == cat)
    }

    /// Overlap penalty: same category already stacked on the team.
    fn overlap(&self, cand: &str, allies: &[&str]) -> f32 {
        let cat = self.category(cand);
        if cat.is_empty() { return 0.0; }
        let n = allies.iter().filter(|a| self.category(a) == cat).count();
        n as f32 * COMP_UNIT * 0.5
    }
}

fn init(_ctx: &GameCtx) -> ModRegistration {
    let mut reg = ModRegistration::new(MOD_ID);
    reg.add_draft_score_hook(DraftAi { t: parse_tables() });
    reg
}
declare_mod!(init);

struct DraftAi { t: Tables }

impl std::fmt::Debug for DraftAi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DraftAi({} champs)", self.t.champ_delta.len())
    }
}

impl ModDraftScoreHook for DraftAi {
    fn id(&self) -> &str { "tfm2_draft_ai:stats_theory" }

    fn score_pick(&self, ctx: &DraftScoreContext, candidate: usize, base: f32) -> DraftScoreDecision {
        let cand = match name_of(candidate) { Some(n) => n, None => return DraftScoreDecision::Pass };
        // ★ COACH-PROBE (TEMP): absurdly boost ONE fixed champ — if the new head-coach
        // recommendation border lands on 오우거(픽)/바드(밴) in the player's banpick, the
        // coach runs through this hook → we can steer the NATIVE UI with our stats AND
        // read the live board here. REMOVE after the experiment.
        if cand == "ogre" {
            return DraftScoreDecision::Replace(base + 10_000.0);
        }
        let allies = self.t.names(ctx.ally_pick);
        let enemies = self.t.names(ctx.enemy_pick);
        let ally_bans = self.t.names(ctx.ally_ban);
        let enemy_bans = self.t.names(ctx.enemy_ban);
        // log the ctx's own phase/difficulty too — a coach-only difference there would
        // be the player-board identification handle the 0.4.6 probe lacked.
        diag_snapshot(&format!("PICK p={:?} d={:?}", ctx.phase, ctx.difficulty), ctx.is_explore, &allies, &enemies, ctx.available_champions.len());
        write_state("PICK", ctx.is_explore, &allies, &enemies, &ally_bans, &enemy_bans, ctx.available_champions.len());

        // pick-order ramps: counters matter more late (more enemy info, last answer)
        let picks_made = ctx.ally_pick.len() as f32; // 0..=4
        let late = (picks_made / 4.0).min(1.0);      // 0 early → 1 late
        let cnt_ramp = 0.5 + 0.5 * late;             // 0.5..1.0
        let power_ramp = 1.0 - 0.4 * late;           // 1.0..0.6

        let mut d = power_ramp * self.t.delta(cand);
        for a in &allies { d += W_SYN * self.t.syn(cand, a); }
        for e in &enemies { d += W_CNT * cnt_ramp * self.t.cnt(cand, e); }
        for e in &enemies { d -= W_WEAK * cnt_ramp * self.t.cnt(e, cand); }
        d += W_COMP * self.t.comp_need(cand, &allies);
        d -= W_DUP * self.t.overlap(cand, &allies);

        let mut scale = SCALE;
        if ctx.is_explore { scale *= 0.5; } // let the game explore alternatives
        DraftScoreDecision::Add(scale * d)
    }

    fn score_ban(&self, ctx: &DraftScoreContext, candidate: usize, base: f32) -> DraftScoreDecision {
        let cand = match name_of(candidate) { Some(n) => n, None => return DraftScoreDecision::Pass };
        // ★ COACH-PROBE (TEMP): see score_pick — ban side boosts 바드.
        if cand == "bard" {
            return DraftScoreDecision::Replace(base + 10_000.0);
        }
        let allies = self.t.names(ctx.ally_pick);
        let enemies = self.t.names(ctx.enemy_pick);
        let ally_bans = self.t.names(ctx.ally_ban);
        let enemy_bans = self.t.names(ctx.enemy_ban);
        diag_snapshot(&format!("BAN p={:?} d={:?}", ctx.phase, ctx.difficulty), ctx.is_explore, &allies, &enemies, ctx.available_champions.len());
        write_state("BAN", ctx.is_explore, &allies, &enemies, &ally_bans, &enemy_bans, ctx.available_champions.len());

        let mut d = self.t.delta(cand);                       // ban OP / strong champs
        for a in &allies { d += W_BAN_CNT * self.t.cnt(cand, a); } // strong INTO our picks
        for e in &enemies { d += W_BAN_SYN * self.t.syn(cand, e); } // completes enemy synergy
        d += W_BAN_META * self.t.ban(cand);                  // high banrate in data

        let mut scale = SCALE;
        if ctx.is_explore { scale *= 0.5; }
        DraftScoreDecision::Add(scale * d)
    }
}
