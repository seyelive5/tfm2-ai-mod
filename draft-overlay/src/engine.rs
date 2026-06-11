//! Pure recommender engine (no hudhook / no imgui) — shared by the overlay DLL
//! (`lib.rs`) and the offline self-test (`../../overlay-selftest`, via #[path]).
//!
//! Two regimes, blended by data confidence:
//!   - DATA term: champ winrate delta + synergy + counter + banrate (from matches).
//!     Sample-shrunk, so it fades to ~0 when there are few matches.
//!   - THEORY term: tag-based comp-need + synergy + counter (from taxonomy only).
//!     Always available; weighted by (1 - confidence) so it DRIVES the ranking at
//!     cold-start (empty save) and recedes as real stats accumulate.
//! An optional available-champion POOL restricts candidates (the game can release
//! champions gradually).

use std::collections::HashSet;

pub const WEIGHTS_TSV: &str = include_str!("draft_weights.tsv");
pub const STATE_PATH: &str = "C:\\tmp\\tfm2_draft_state.txt";
pub const TOP_N: usize = 8;

// champion id -> Korean display name (all 60; banpick-data.js champions[]).
pub const CHAMPS: &[(&str, &str)] = &[
    ("vampire", "흡혈귀"), ("monk", "몽크"), ("circus_blade", "곡예사"), ("android", "안드로이드"),
    ("inquisitor", "이단심문관"), ("lancer", "창술사"), ("ninja", "닌자"), ("fighter", "격투가"),
    ("ice_mage", "얼음술사"), ("wind_mage", "바람술사"), ("exorcist", "엑소시스트"), ("ghost", "유령"),
    ("pole_warrior", "봉술사"), ("dual_blader", "듀얼 블레이더"), ("demon", "악마"), ("swordman", "검사"),
    ("white_mage", "백마술사"), ("shadowmancer", "그림자술사"), ("pythoness", "무녀"), ("hunter", "사냥꾼"),
    ("jiangshi", "강시"), ("guardian_spirit", "수호령"), ("druid", "드루이드"), ("siege_breaker", "공성병"),
    ("knight", "기사"), ("werewolf", "늑대인간"), ("dark_mage", "흑마술사"), ("magic_knight", "마검사"),
    ("illusionist", "환영술사"), ("berserker", "광전사"), ("pyromancer", "화염술사"), ("bomber", "폭탄병"),
    ("necromancer", "네크로맨서"), ("priest", "성직자"), ("lightning_mage", "번개술사"), ("prisoner", "죄수"),
    ("dokkaebi", "도깨비"), ("executioner", "처형인"), ("barrier_magician", "결계사"), ("shield_bearer", "방패병"),
    ("archer", "궁수"), ("gambler", "도박사"), ("whip_master", "채찍술사"), ("hitman", "히트맨"),
    ("chef", "요리사"), ("spirit_caller", "정령사"), ("taoist", "도사"), ("gunner", "총잡이"),
    ("hammerer", "중보병"), ("clown", "광대"), ("cavalry_knight", "기병"), ("enchanter", "인챈터"),
    ("soldier", "소총수"), ("plague_doctor", "역병의사"), ("dancer", "무희"), ("boomerang_hunter", "부메랑 헌터"),
    ("voodoo_shaman", "부두술사"), ("poison_dart_hunter", "독침술사"), ("bard", "음유시인"), ("ogre", "오우거"),
];

pub fn ko_name(id: &str) -> String {
    CHAMPS.iter().find(|(i, _)| *i == id).map(|(_, k)| k.to_string()).unwrap_or_else(|| id.to_string())
}

// tunable weights. NOTE (2026-06-01 validation): a non-circular holdout showed the
// DATA synergy/counter terms add no measurable predictive power at this career's
// sample size (68 team matches: synergy +0.003 AUC ≈ noise, counter −0.004 ≈ harmful),
// while champ-winrate (C) is the real signal (AUC 0.574). So synergy/counter are now
// (a) DEMOTED below C, (b) faded by data confidence (×self.confidence(), so they grow
// back as matches accumulate), and (c) the counter no longer double-counts. See VALIDATION.md.
// 2026-06-04 HOLDOUT VALIDATION (validate-terms.rs, 3719 matches, 80/20 split):
// champ-winrate (base) predicts winners at 59.3%. synergy/counter add NOTHING
// incrementally (base+syn 59.0%, base+cnt 58.9% — ≤ base; their high standalone is
// confounding with champ strength). comp HURTS when weighted (base+comp ≤ base at
// every weight). So base must DOMINATE; the rest are tiny cold-start/guardrail terms.
const W_SYN: f32 = 0.15; // was 0.7 — validation: no incremental value (confounded with base)
const W_CNT: f32 = 0.15; // was 0.5 — "
const W_WEAK: f32 = 0.15; // was 0.5 — "
const W_COMP: f32 = 0.2; // was 0.6 — comp barely predicts wins; keep only as a small anti-degenerate guardrail
const W_DUP: f32 = 0.4;
const W_BAN_CNT: f32 = 0.4; // was 1.0 — same noise tables, trimmed
const W_BAN_SYN: f32 = 0.3; // was 0.8 — "
// was 0.5 → 0.2 → 0.05: banrate is a RAW 0..1 fraction (top champs hit ~0.64),
// so at 0.2 the meta term (≈0.13) outweighed the entire base range (max C-delta
// ≈0.09) — a delta-0 champ banned 60% outranked the best S-tier, i.e. the
// circular raw-banrate signal still led. 0.05 caps meta at ≈0.032 (< one S-tier
// base of 0.06): winrate now actually leads, banrate only breaks ties.
const W_BAN_META: f32 = 0.05;
const COMP_UNIT: f32 = 0.12;
// early-pick flexibility bonus, per extra lane a champ can play (capped at +2 lanes).
// First picks are "blind" → favor flex champs that hide your lane assignment.
const W_FLEX: f32 = 0.01;
// FEARLESS flex bonus: a 2-lane champ fills whichever lane dries up across sets, so flex
// is worth far more — and it applies to EVERY pick, not just early. DATA-ANCHORED (2026-06-06):
// our tier step ≈ 4%p (S+6/A+2/B-2/C-6), so 0.030/lane × max 2 lanes = +0.06 ≈ 1.5 tiers —
// the research's "+1~2 tiers", leaning aggressive because the live pool is genuinely shallow
// (jungle only 2 S/A, support 3) and a 60-champ pool depletes far faster than LoL. Tune by feel.
const W_FLEX_FEARLESS: f32 = 0.030;
// confidence: full trust in DATA terms at this many matches (theory fully faded).
const FULL_CONF_MATCHES: f32 = 200.0;
// a champ with fewer games than this behind its winrate is flagged low-confidence
// in the UI (its `C` is too thin to trust). Unknown game count = not flagged.
const LOW_CONF_GAMES: u32 = 15;
// a lane needs at least this many games before its winrate is trusted to pick a
// champ's PRIMARY lane (else a 1-game off-lane could win the tiebreak).
const LANE_MIN_GAMES: u32 = 10;
// DATA-driven role thresholds on the per-game ratios in `RP` rows (tanking/dealing,
// healing/dealing). Calibrated from real patch stats — clean gaps separate the roles:
//   healers (guardian 4.8, monk 4.7, pythoness 2.7) ≫ bruisers (vampire 0.9, fighter 0.7)
//   tanks (android 2.4, ogre 1.8) ≫ carries (archer 0.6, soldier 0.4)
const HEAL_RATIO: f32 = 1.5; // healRatio ≥ → dedicated healer/enchanter
const TANK_RATIO: f32 = 1.6; // tankRatio ≥ (and not a healer) → hard frontline/tank
const FRONT_RATIO: f32 = 1.25; // tankRatio ≥ (and not a healer) → frontline (incl. bruiser)

/// Lane (position) display names, index 0..5. Matches the launcher's POS rows
/// (0=Top 1=Jungle 2=Mid 3=Bottom 4=Support, from champion_patch_statistics).
pub const LANES: [&str; 5] = ["탑", "정글", "미드", "바텀", "서폿"];

#[derive(Default)]
struct Taxon {
    category: String,
    damage: String,
    tags: Vec<String>,
}

pub struct Tables {
    champ_delta: std::collections::HashMap<String, f32>,
    /// games behind each champ's delta (optional 4th field of a `C` row); used to
    /// flag thin-sample recommendations as low-confidence in the UI.
    champ_games: std::collections::HashMap<String, u32>,
    /// per-(champ, lane index) winrate delta + games behind it (`CP` rows). Lane-
    /// SPECIFIC — so a champ strong in its main lane isn't credited that winrate in
    /// an off-lane. Falls back to the champ-level `champ_delta` when a cell is absent.
    champ_pos_delta: std::collections::HashMap<(String, u8), (f32, u32)>,
    synergy: std::collections::HashMap<(String, String), f32>,
    counter: std::collections::HashMap<(String, String), f32>,
    taxon: std::collections::HashMap<String, Taxon>,
    banrate: std::collections::HashMap<String, f32>,
    /// total matches behind these stats (from an `M\t<n>` row); 0 = cold-start.
    pub matches: usize,
    /// which save/career these stats came from (from a `SAVE\t<name>` row).
    pub save_name: String,
    /// available champion pool for this save (from a `POOL\t<csv ids>` row);
    /// empty = unknown → no restriction. The launcher derives it from which
    /// champions appear in the save (gradual champion release).
    pub pool: HashSet<String>,
    /// champion -> primary lane index (most-played position), from `POS` rows.
    pos_primary: std::collections::HashMap<String, u8>,
    /// champion -> all lanes it plays meaningfully (flex), from `POS` rows.
    /// Empty overall = no position data → the lane board falls back to a list.
    pos_play: std::collections::HashMap<String, Vec<u8>>,
    /// champion -> (tankRatio, healRatio) = tanking/dealing, healing/dealing per
    /// game (from `RP` rows). DATA-driven role signal: thresholded to detect
    /// frontline/healer for comp balance, catching champs the static tags miss.
    role: std::collections::HashMap<String, (f32, f32)>,
    /// opponent-aware bans: (team id, champ) -> (games, wins) that team played it.
    /// Parsed but currently UNREAD: the OppBan view (opp_ban_ranking/has_opp) was
    /// dead code with no caller in either UI and got removed; the data stays
    /// parsed so a future opponent-aware ban feature only needs a new consumer.
    #[allow(dead_code)]
    opp: std::collections::HashMap<(u32, String), (u32, u32)>,
    /// team id -> total games (usage-share denominator), from `OPPN` rows.
    #[allow(dead_code)]
    opp_team_games: std::collections::HashMap<u32, u32>,
    /// team id -> display name (from `TEAM` rows), sorted by name for the picker.
    pub team_names: Vec<(u32, String)>,
    /// champs buffed/nerfed in the CURRENT patch (from `CHG` rows): champ ->
    /// '+' buff / '-' nerf / '~' mixed-rework. Their winrate stats were reset
    /// (stale prev-patch blend dropped), so the UI tags them ▲/▼/⚡.
    chg: std::collections::HashMap<String, char>,
}

/// Baked fallback table (compiled-in draft_weights.tsv).
pub fn parse_tables() -> Tables {
    parse_tables_from_str(WEIGHTS_TSV)
}

/// Parse tables from TSV text — used both for the baked fallback and for a
/// refreshed draft_weights.tsv loaded from disk at runtime.
pub fn parse_tables_from_str(tsv: &str) -> Tables {
    use std::collections::HashMap;
    let mut t = Tables {
        champ_delta: HashMap::new(),
        champ_games: HashMap::new(),
        champ_pos_delta: HashMap::new(),
        synergy: HashMap::new(),
        counter: HashMap::new(),
        taxon: HashMap::new(),
        banrate: HashMap::new(),
        matches: 0,
        save_name: String::new(),
        pool: HashSet::new(),
        pos_primary: HashMap::new(),
        pos_play: HashMap::new(),
        role: HashMap::new(),
        opp: HashMap::new(),
        opp_team_games: HashMap::new(),
        team_names: Vec::new(),
        chg: HashMap::new(),
    };
    for line in tsv.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "C" if f.len() >= 3 => {
                t.champ_delta.insert(f[1].into(), f[2].parse().unwrap_or(0.0));
                // optional 4th field = games behind this delta (low-confidence flag)
                if let Some(g) = f.get(3).and_then(|x| x.parse::<u32>().ok()) {
                    t.champ_games.insert(f[1].into(), g);
                }
            }
            // CP<TAB><champ><TAB><laneIdx><TAB><delta><TAB><games> — per-lane winrate.
            "CP" if f.len() >= 5 => {
                // drop out-of-range lane indices, don't fold them into Support (lane 4).
                if let Ok(li) = f[2].parse::<u8>() {
                    if li < 5 {
                        let d = f[3].parse().unwrap_or(0.0);
                        let g = f[4].parse().unwrap_or(0);
                        t.champ_pos_delta.insert((f[1].into(), li), (d, g));
                    }
                }
            }
            "S" if f.len() >= 4 => {
                t.synergy.insert((f[1].into(), f[2].into()), f[3].parse().unwrap_or(0.0));
            }
            "K" if f.len() >= 4 => {
                t.counter.insert((f[1].into(), f[2].into()), f[3].parse().unwrap_or(0.0));
            }
            "T" if f.len() >= 4 => {
                let tags = if f.len() >= 5 {
                    f[4].split(',').map(|s| s.to_string()).collect()
                } else {
                    Vec::new()
                };
                t.taxon.insert(f[1].into(), Taxon { category: f[2].into(), damage: f[3].into(), tags });
            }
            "B" if f.len() >= 3 => {
                t.banrate.insert(f[1].into(), f[2].parse().unwrap_or(0.0));
            }
            // M<TAB><total_matches> — drives the data-confidence blend.
            "M" if f.len() >= 2 => {
                t.matches = f[1].parse().unwrap_or(0);
            }
            // SAVE<TAB><name> — which save/career these stats came from.
            "SAVE" if f.len() >= 2 => {
                t.save_name = f[1].to_string();
            }
            // POOL<TAB><id1,id2,...> — available champions for this save.
            "POOL" if f.len() >= 2 => {
                t.pool = f[1].split(',').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect();
            }
            // POS<TAB><champ><TAB><primaryIdx><TAB><idx,idx,...> — lane positions.
            "POS" if f.len() >= 4 => {
                let prim: u8 = f[2].parse().unwrap_or(0);
                let mut plays: Vec<u8> =
                    f[3].split(',').filter_map(|x| x.parse().ok()).filter(|l| *l < 5).collect();
                if plays.is_empty() {
                    plays.push(prim);
                }
                t.pos_primary.insert(f[1].into(), prim.min(4));
                t.pos_play.insert(f[1].into(), plays);
            }
            // CHG<TAB><champ><TAB><dir> — champ changed in the current patch
            // ('+' buff / '-' nerf / '~' mixed). Stats for it were reset.
            "CHG" if f.len() >= 2 => {
                let d = f.get(2).and_then(|x| x.chars().next()).unwrap_or('~');
                t.chg.insert(f[1].into(), d);
            }
            // RP<TAB><champ><TAB><tankRatio><TAB><healRatio> — data role signal.
            "RP" if f.len() >= 4 => {
                let tr = f[2].parse().unwrap_or(0.0);
                let hr = f[3].parse().unwrap_or(0.0);
                t.role.insert(f[1].into(), (tr, hr));
            }
            // OPP<TAB><team><TAB><champ><TAB><games><TAB><wins> — opponent usage+winrate.
            "OPP" if f.len() >= 5 => {
                if let Ok(tm) = f[1].parse::<u32>() {
                    let g = f[3].parse().unwrap_or(0);
                    let w = f[4].parse().unwrap_or(0);
                    t.opp.insert((tm, f[2].into()), (g, w));
                }
            }
            // OPPN<TAB><team><TAB><games> — team total games.
            "OPPN" if f.len() >= 3 => {
                if let Ok(tm) = f[1].parse::<u32>() {
                    t.opp_team_games.insert(tm, f[2].parse().unwrap_or(0));
                }
            }
            // TEAM<TAB><id><TAB><name> — opponent picker labels.
            "TEAM" if f.len() >= 3 => {
                if let Ok(id) = f[1].parse::<u32>() {
                    t.team_names.push((id, f[2].to_string()));
                }
            }
            _ => {}
        }
    }
    t.team_names.sort_by(|a, b| a.1.cmp(&b.1));
    t
}


impl Tables {
    pub fn len(&self) -> usize { self.champ_delta.len() }
    /// 0.0 (no data → pure theory) .. 1.0 (rich data → pure stats).
    pub fn confidence(&self) -> f32 { (self.matches as f32 / FULL_CONF_MATCHES).min(1.0) }
    fn delta(&self, c: &str) -> f32 { *self.champ_delta.get(c).unwrap_or(&0.0) }
    /// games behind a champ's winrate; u32::MAX if unknown (→ never flagged).
    fn games(&self, c: &str) -> u32 { *self.champ_games.get(c).unwrap_or(&u32::MAX) }
    fn low_conf(&self, c: &str) -> bool { self.games(c) < LOW_CONF_GAMES }
    /// patch-change tag: ▲ buffed / ▼ nerfed / ⚡ mixed-rework this patch, "" if unchanged.
    pub fn chg_mark(&self, c: &str) -> &'static str {
        match self.chg.get(c) {
            Some('+') => "▲",
            Some('-') => "▼",
            Some(_) => "⚡",
            None => "",
        }
    }
    /// any champ changed in the current patch? (drives the UI legend line)
    pub fn any_chg(&self) -> bool { !self.chg.is_empty() }
    /// Lane-specific winrate delta: the (champ, lane) cell if present, else the
    /// champ-level delta SHRUNK HARD toward neutral. A POS-playable lane with no
    /// CP cell means ZERO games there in the current patch window — valuing it at
    /// the overall delta (inflated by main-lane games) let assign_lanes move a
    /// champ to a no-data lane over its real main lane, displacing a genuine
    /// main-laner. ×0.25 keeps a little signal without beating any real CP cell.
    fn delta_for(&self, c: &str, lane: Option<u8>) -> f32 {
        if let Some(l) = lane {
            if let Some((d, _)) = self.champ_pos_delta.get(&(c.into(), l)) {
                return *d;
            }
            return 0.25 * self.delta(c);
        }
        self.delta(c)
    }
    /// games behind the lane-specific delta (for the low-confidence "?" flag).
    fn games_for(&self, c: &str, lane: Option<u8>) -> u32 {
        if let Some(l) = lane {
            if let Some((_, g)) = self.champ_pos_delta.get(&(c.into(), l)) {
                return *g;
            }
        }
        self.games(c)
    }
    fn low_conf_for(&self, c: &str, lane: Option<u8>) -> bool {
        self.games_for(c, lane) < LOW_CONF_GAMES
    }
    fn syn(&self, a: &str, b: &str) -> f32 { *self.synergy.get(&(a.into(), b.into())).unwrap_or(&0.0) }
    fn cnt(&self, a: &str, b: &str) -> f32 { *self.counter.get(&(a.into(), b.into())).unwrap_or(&0.0) }
    fn ban(&self, c: &str) -> f32 { *self.banrate.get(c).unwrap_or(&0.0) }
    fn has_tag(&self, c: &str, tag: &str) -> bool {
        self.taxon.get(c).map(|t| t.tags.iter().any(|x| x == tag)).unwrap_or(false)
    }
    fn damage(&self, c: &str) -> &str { self.taxon.get(c).map(|t| t.damage.as_str()).unwrap_or("AD") }
    fn category(&self, c: &str) -> &str { self.taxon.get(c).map(|t| t.category.as_str()).unwrap_or("") }

    // ---- DATA-driven role classification (RP rows) — augments the static tags ----
    /// (tankRatio, healRatio) for a champ; (0,0) if no role data.
    fn role(&self, c: &str) -> (f32, f32) { self.role.get(c).copied().unwrap_or((0.0, 0.0)) }
    /// dedicated healer/enchanter by data (heals far more than it deals).
    fn is_healer_data(&self, c: &str) -> bool { self.role(c).1 >= HEAL_RATIO }
    /// does this champ have DATA-driven role info (an RP row)? Without it,
    /// comp_counts silently reads as "no tank/heal" — callers must not draw
    /// conclusions from absence of data.
    pub fn role_known(&self, c: &str) -> bool { self.role.contains_key(c) }
    /// (tanks, healers) among the given champ ids — comp summary for the AI panel.
    pub fn comp_counts(&self, ids: &[&str]) -> (usize, usize) {
        (
            ids.iter().filter(|c| self.is_tank_data(c)).count(),
            ids.iter().filter(|c| self.is_healer_data(c)).count(),
        )
    }
    /// hard frontline/tank by data (soaks much more than it deals, and isn't a healer).
    fn is_tank_data(&self, c: &str) -> bool {
        let (t, h) = self.role(c);
        t >= TANK_RATIO && h < 1.0
    }
    /// frontline (tank OR bruiser) by data — not a healer.
    fn is_frontline_data(&self, c: &str) -> bool {
        let (t, h) = self.role(c);
        t >= FRONT_RATIO && h < 1.2
    }
    /// Meta TIER letter (S/A/B/C/D) from the champ's winrate delta — per-lane if a
    /// lane is given, else the champ's overall (blended) winrate. "" when there is no
    /// winrate data (cold start), so the grid stays clean until stats exist. Same
    /// thresholds as the HTML report so the two views agree.
    pub fn tier_of(&self, c: &str, lane: Option<u8>) -> &'static str {
        let has = match lane {
            Some(l) => self.champ_pos_delta.contains_key(&(c.to_string(), l)),
            None => self.champ_delta.contains_key(c),
        };
        if !has {
            return "";
        }
        let d = self.delta_for(c, lane);
        if d >= 0.06 {
            "S"
        } else if d >= 0.02 {
            "A"
        } else if d >= -0.02 {
            "B"
        } else if d >= -0.06 {
            "C"
        } else {
            "D"
        }
    }

    /// Count of STRONG (S/A tier) champions still available per lane (index 0..5),
    /// among `available`. For the fearless series "depletion" view — so you don't
    /// spend a thin lane's only strong champ in an early game.
    pub fn strong_per_lane(&self, available: &HashSet<String>) -> [u32; 5] {
        let mut c = [0u32; 5];
        for id in available {
            for lane in 0u8..5 {
                if self.plays(id, lane) {
                    let t = self.tier_of(id, Some(lane));
                    if t == "S" || t == "A" {
                        c[lane as usize] += 1;
                    }
                }
            }
        }
        c
    }

    /// Short role glyph for the UI: 힐(healer)/탱(tank)/브(bruiser·frontline)/딜(carry).
    /// "" when there's no role data yet (cold start / <5 games) — don't guess.
    /// Role glyph — DISABLED 2026-06-04. The single-bucket label (힐/탱/브/딜/서폿) is
    /// unvalidated (holdout: role doesn't predict wins) AND misleading for flex/util
    /// champs: e.g. exorcist (category Util → "서폿") actually wins +6% at TOP (43g), so
    /// "서폿 엑소시스트 · 탑 라인" reads as a contradiction. Tier(S~D)+lane stay; the AI
    /// panel covers role/comp nuance. comp_need still uses is_tank/is_heal/is_frontline.
    pub fn role_tag(&self, _c: &str) -> &'static str {
        ""
    }
    fn category_team(&self, allies: &[&str], cat: &str) -> bool {
        allies.iter().any(|a| self.category(a) == cat)
    }

    // ---- lane / position helpers (POS rows) ----
    fn has_pos(&self) -> bool { !self.pos_play.is_empty() }
    /// Primary lane = where the champ actually WINS most (enough games), not the
    /// most-PLAYED lane. A carry frequently farmed into an off-lane (e.g. a mage
    /// fielded support but losing there) shouldn't be slotted into that off-lane.
    fn primary(&self, c: &str) -> Option<u8> {
        self.best_winrate_lane(c).or_else(|| self.pos_primary.get(c).copied())
    }
    /// Best-winrate lane among the champ's played lanes that have enough games.
    fn best_winrate_lane(&self, c: &str) -> Option<u8> {
        let plays = self.pos_play.get(c)?;
        let mut best: Option<(u8, f32)> = None;
        for &l in plays {
            if let Some(&(d, g)) = self.champ_pos_delta.get(&(c.to_string(), l)) {
                if g >= LANE_MIN_GAMES && best.map_or(true, |(_, bd)| d > bd) {
                    best = Some((l, d));
                }
            }
        }
        best.map(|(l, _)| l)
    }
    fn plays(&self, c: &str, lane: u8) -> bool {
        self.pos_play.get(c).map(|v| v.contains(&lane)).unwrap_or(false)
    }
    /// number of lanes this champ plays (1 if unknown → no flex bonus).
    fn flex(&self, c: &str) -> u8 { self.pos_play.get(c).map(|v| v.len() as u8).unwrap_or(1) }
    /// counter advantage of `cand` over `enemy`: data counter diff + a faded
    /// tag-based term (so the lane counter-flag still fires at cold-start).
    fn counters(&self, cand: &str, enemy: &str) -> f32 {
        let data = self.cnt(cand, enemy) - self.cnt(enemy, cand);
        let theory = (1.0 - self.confidence()) * self.tag_counter(cand, &[enemy]);
        data + theory
    }

    fn comp_need(&self, cand: &str, allies: &[&str]) -> f32 {
        // First pick (no allies yet): there is no identifiable comp GAP, so this
        // term must be 0 — otherwise an empty team "needs everything" and champs
        // with the most utility tags (CC/Heal/Shield/Backline = mostly SUPPORTS)
        // wrongly top the first pick. Ramp the signal up as the team forms.
        if allies.is_empty() {
            return 0.0;
        }
        let team_has = |tag: &str| allies.iter().any(|a| self.has_tag(a, tag));
        // role need is satisfied by the static TAG or the DATA signal (RP) — so a
        // champ the tags miss but that empirically tanks/heals still fills the gap.
        let is_tank = |c: &str| self.has_tag(c, "Tank") || self.is_tank_data(c);
        let is_front = |c: &str| self.has_tag(c, "Frontline") || self.is_frontline_data(c);
        let is_heal = |c: &str| self.has_tag(c, "Heal") || self.has_tag(c, "Shield") || self.is_healer_data(c);
        let mut s = 0.0;
        if !allies.iter().any(|a| is_tank(a)) && is_tank(cand) {
            s += COMP_UNIT;
        } else if !allies.iter().any(|a| is_front(a)) && is_front(cand) {
            s += COMP_UNIT * 0.5;
        }
        if !team_has("CC") && self.has_tag(cand, "CC") {
            s += COMP_UNIT * 0.7;
        }
        if !allies.iter().any(|a| is_heal(a)) && is_heal(cand) {
            s += COMP_UNIT * 0.6;
        }
        if !(team_has("Backline") || self.category_team(allies, "Range"))
            && (self.has_tag(cand, "Backline") || self.category(cand) == "Range")
        {
            s += COMP_UNIT * 0.6;
        }
        // AD/AP balance bonus REMOVED 2026-06-05: holdout (4298 matches) shows damage-type
        // balance does NOT help in TFM2 — AD-only teams win MORE (55% vs 49% balanced), and
        // beat balanced teams head-to-head 56%. Forcing AD+AP just dilutes a strong AD core.
        // signal strength grows with team size (1 pick = weak gap evidence, 4+ = full)
        s * (allies.len() as f32 / 4.0).min(1.0)
    }

    fn overlap(&self, cand: &str, allies: &[&str]) -> f32 {
        let cat = self.category(cand);
        if cat.is_empty() {
            return 0.0;
        }
        let n = allies.iter().filter(|a| self.category(a) == cat).count();
        n as f32 * COMP_UNIT * 0.5
    }

    /// Tag-based SYNERGY (no match data): engage→follow-up, peel-for-carry.
    /// Cold-start proxy for the data-driven `syn`; faded out by confidence.
    fn tag_synergy(&self, cand: &str, allies: &[&str]) -> f32 {
        let team = |tag: &str| allies.iter().any(|a| self.has_tag(a, tag));
        let mut s = 0.0;
        // engage (CC) + AOE/DOT follow-up
        if team("CC") && (self.has_tag(cand, "AOE") || self.has_tag(cand, "DOT")) {
            s += 0.5;
        }
        if team("AOE") && self.has_tag(cand, "CC") {
            s += 0.5;
        }
        // peel for a squishy backline carry
        if (team("Backline") || self.category_team(allies, "Range"))
            && (self.has_tag(cand, "Heal") || self.has_tag(cand, "Shield") || self.has_tag(cand, "CC"))
        {
            s += 0.5;
        }
        s * COMP_UNIT
    }

    /// Tag-based COUNTER vs the enemy comp (no match data). Cold-start proxy for
    /// the data-driven `cnt`; faded out by confidence.
    fn tag_counter(&self, cand: &str, enemies: &[&str]) -> f32 {
        let cnt = |tag: &str| enemies.iter().filter(|e| self.has_tag(e, tag)).count() as f32;
        let cat = |c: &str| enemies.iter().filter(|e| self.category(e) == c).count() as f32;
        let mut s = 0.0;
        // enemy squishy backline -> dive it (mobility / assassin)
        let backline = cnt("Backline") + cat("Range");
        if backline >= 2.0 && (self.has_tag(cand, "Mobility") || self.category(cand) == "Assassin") {
            s += 0.4 * (backline - 1.0).min(3.0);
        }
        // enemy tanky frontline -> whittle (poke / dot)
        let front = cnt("Tank") + cnt("Frontline");
        if front >= 2.0 && (self.has_tag(cand, "Poke") || self.has_tag(cand, "DOT")) {
            s += 0.4 * (front - 1.0).min(3.0);
        }
        // enemy poke -> engage (frontline / mobility to close)
        if cnt("Poke") >= 2.0 && (self.has_tag(cand, "Frontline") || self.has_tag(cand, "Mobility")) {
            s += 0.3;
        }
        // enemy heavy CC -> mitigate (mobility / shield)
        if cnt("CC") >= 3.0 && (self.has_tag(cand, "Mobility") || self.has_tag(cand, "Shield")) {
            s += 0.3;
        }
        s * COMP_UNIT
    }

    /// PICK desirability. DATA terms mirror tfm2_draft_ai::score_pick; the tag
    /// THEORY terms (tag_synergy/tag_counter) are added, scaled by (1-confidence),
    /// so they fold into the synergy/counter buckets and drive cold-start ranking.
    pub fn score_pick(&self, cand: &str, allies: &[&str], enemies: &[&str]) -> (f32, &'static str) {
        self.score_pick_in(cand, allies, enemies, None, 0)
    }

    /// Like `score_pick`, but for a SPECIFIC lane: the champ-power term uses that
    /// lane's winrate (`delta_for`) instead of the champ's overall winrate. Pass
    /// `None` for a lane-agnostic score (flat list / ban context). `order` is the
    /// manual pick-order hint (0=auto, 1=선픽, 2=후픽) — first pick favors power/flex,
    /// later pick favors counters.
    pub fn score_pick_in(
        &self,
        cand: &str,
        allies: &[&str],
        enemies: &[&str],
        lane: Option<u8>,
        order: i8,
    ) -> (f32, &'static str) {
        let picks_made = allies.len() as f32;
        let late = match order {
            1 => 0.0, // 선픽(first): power/flex, less counter
            2 => 1.0, // 후픽(later): counter emphasis
            _ => (picks_made / 4.0).min(1.0),
        };
        let cnt_ramp = 0.5 + 0.5 * late;
        let power_ramp = 1.0 - 0.4 * late;
        let data = self.confidence(); // trust pairwise DATA proportional to match volume
        let theory = 1.0 - data;

        let base = power_ramp * self.delta_for(cand, lane);
        let mut syn = 0.0;
        for a in allies {
            syn += data * W_SYN * self.syn(cand, a);
        }
        syn += theory * self.tag_synergy(cand, allies);
        let mut cnt = 0.0;
        for e in enemies {
            cnt += data * W_CNT * cnt_ramp * self.cnt(cand, e);
            cnt -= data * W_WEAK * cnt_ramp * self.cnt(e, cand);
        }
        cnt += theory * self.tag_counter(cand, enemies);
        let comp = W_COMP * self.comp_need(cand, allies);
        let dup = W_DUP * self.overlap(cand, allies);
        let total = base + syn + cnt + comp - dup;
        (total, dominant(&[("강챔", base), ("시너지", syn), ("카운터", cnt), ("조합", comp)]))
    }

    /// BAN desirability. DATA terms mirror tfm2_draft_ai::score_ban; at cold-start
    /// add a faded tag term for champs that counter our picks / fit the meta.
    pub fn score_ban(&self, cand: &str, allies: &[&str], enemies: &[&str], order: i8) -> (f32, &'static str) {
        let data = self.confidence();
        let theory = 1.0 - data;
        // 선픽(first): you can pick a strong champ yourself, so lean bans toward THREATS
        // rather than generically-strong champs — but only MILDLY, so blind early bans
        // (no enemy picks yet) still rank by strength.
        let base_mult = if order == 1 { 0.8 } else { 1.0 };
        let base = base_mult * self.delta(cand);
        let mut cnt = 0.0;
        for a in allies {
            cnt += data * W_BAN_CNT * self.cnt(cand, a);
        }
        // cold-start: ban champs whose tags counter OUR picks (allies)
        cnt += theory * self.tag_counter(cand, allies);
        let mut syn = 0.0;
        for e in enemies {
            syn += data * W_BAN_SYN * self.syn(cand, e);
        }
        let meta = W_BAN_META * self.ban(cand);
        let total = base + cnt + syn + meta;
        (total, dominant(&[("강챔", base), ("우리킬", cnt), ("적시너지", syn), ("밴율", meta)]))
    }

    // (opp_ban_ranking / has_opp / OppBan removed 2026-06-12: dead code — no
    // caller in either UI — and its lane resolution inverted primary()'s
    // best-winrate-lane fix. The O/OPPN data stays parsed above for a future
    // opponent-aware ban consumer.)
}

fn dominant(parts: &[(&'static str, f32)]) -> &'static str {
    parts
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| *l)
        .unwrap_or("")
}

// ---- IPC draft state (kept for the offline self-test / legacy IPC format) ----
#[derive(Default, Clone)]
pub struct DraftState {
    pub ok: bool,
    pub ts: u64,
    pub phase: String, // "BAN" | "PICK"
    pub explore: bool,
    pub ally_pick: Vec<String>,
    pub enemy_pick: Vec<String>,
    pub ally_ban: Vec<String>,
    pub enemy_ban: Vec<String>,
    pub avail: usize,
    /// manual pick-order hint: 0 = auto (infer from board), 1 = 선픽(first), 2 = 후픽(later).
    pub pick_order: i8,
}

fn split_csv(v: &str) -> Vec<String> {
    v.split(',').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect()
}

pub fn parse_state(txt: &str) -> DraftState {
    let mut s = DraftState::default();
    for line in txt.lines() {
        let (k, v) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        match k {
            "ts" => s.ts = v.parse().unwrap_or(0),
            "phase" => s.phase = v.to_string(),
            "explore" => s.explore = v == "1",
            "ally_pick" => s.ally_pick = split_csv(v),
            "enemy_pick" => s.enemy_pick = split_csv(v),
            "ally_ban" => s.ally_ban = split_csv(v),
            "enemy_ban" => s.enemy_ban = split_csv(v),
            "avail" => s.avail = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    s.ok = !s.phase.is_empty();
    s
}

pub fn read_state() -> Option<DraftState> {
    let txt = std::fs::read_to_string(STATE_PATH).ok()?;
    Some(parse_state(&txt))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct Row {
    pub ko: String,
    pub id: String,
    pub score: f32,
    pub label: &'static str,
    /// true if this champ's winrate is based on too few games to trust (<15).
    pub low_conf: bool,
    /// data-driven role glyph (힐/탱/브/딜), "" if unknown.
    pub role: &'static str,
    /// meta tier letter (S/A/B/C/D), "" if no winrate data.
    pub tier: &'static str,
    /// patch-change tag (▲ buff / ▼ nerf / ⚡ rework), "" if unchanged this patch.
    pub chg: &'static str,
}

pub struct View {
    pub phase_ban: bool,
    pub explore: bool,
    pub age_ms: u64,
    pub my_pick: Vec<String>,  // ko names
    pub opp_pick: Vec<String>, // ko names
    pub my_ban: Vec<String>,
    pub opp_ban: Vec<String>,
    pub rows: Vec<Row>,
    pub matches: usize, // data volume behind the stats (for the confidence label)
    pub pool_size: usize, // number of available champions considered (0 = all)
}

/// Rank not-yet-taken champions for the current phase, from the human's side.
/// `pool` (if Some) restricts candidates to currently-available champions.
pub fn recommend(
    tables: &Tables,
    s: &DraftState,
    human_is_enemy: bool,
    pool: Option<&HashSet<String>>,
) -> View {
    let (my_pick, opp_pick, my_ban, opp_ban) = if human_is_enemy {
        (&s.enemy_pick, &s.ally_pick, &s.enemy_ban, &s.ally_ban)
    } else {
        (&s.ally_pick, &s.enemy_pick, &s.ally_ban, &s.enemy_ban)
    };
    let phase_ban = s.phase == "BAN";

    let mut taken: HashSet<&str> = HashSet::new();
    for v in [&s.ally_pick, &s.enemy_pick, &s.ally_ban, &s.enemy_ban] {
        for id in v {
            taken.insert(id.as_str());
        }
    }

    let allies: Vec<&str> = my_pick.iter().map(|x| x.as_str()).collect();
    let enemies: Vec<&str> = opp_pick.iter().map(|x| x.as_str()).collect();

    let mut rows: Vec<Row> = Vec::new();
    for (id, _ko) in CHAMPS {
        if taken.contains(id) {
            continue;
        }
        if let Some(p) = pool {
            if !p.contains(*id) {
                continue; // not yet released / not available
            }
        }
        let (score, label) = if phase_ban {
            tables.score_ban(id, &allies, &enemies, s.pick_order)
        } else {
            tables.score_pick_in(id, &allies, &enemies, None, s.pick_order)
        };
        rows.push(Row {
            ko: ko_name(id),
            id: id.to_string(),
            score,
            label,
            low_conf: tables.low_conf(id),
            role: tables.role_tag(id),
            tier: tables.tier_of(id, None),
            chg: tables.chg_mark(id),
        });
    }
    rows.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(TOP_N);

    let ko_list = |ids: &[String]| ids.iter().map(|id| ko_name(id)).collect::<Vec<_>>();
    View {
        phase_ban,
        explore: s.explore,
        age_ms: now_ms().saturating_sub(s.ts),
        my_pick: ko_list(my_pick),
        opp_pick: ko_list(opp_pick),
        my_ban: ko_list(my_ban),
        opp_ban: ko_list(opp_ban),
        rows,
        matches: tables.matches,
        pool_size: pool.map(|p| p.len()).unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// Lane-aware PICK board: one slot per position (탑/정글/미드/바텀/서폿). Fills
// the lanes my picks cover (by primary→flex→any), then for each OPEN lane ranks
// the best champs that can play it. Pick timing (선픽/후픽) tunes the ranking:
// early picks get a flexibility bonus; late picks lean on counters (cnt_ramp in
// score_pick) and surface a counter flag vs the enemy's revealed lane champ.
// ---------------------------------------------------------------------------
pub struct LaneCand {
    pub ko: String,
    pub id: String,
    pub score: f32,
    pub label: &'static str,
    pub low_conf: bool,
    /// data-driven role glyph (힐/탱/브/딜), "" if unknown.
    pub role: &'static str,
    /// meta tier letter (S/A/B/C/D) for THIS lane, "" if no winrate data.
    pub tier: &'static str,
    /// patch-change tag (▲ buff / ▼ nerf / ⚡ rework), "" if unchanged this patch.
    pub chg: &'static str,
}

pub struct Lane {
    pub name: &'static str,
    /// ko name of my pick assigned here; `None` = open (use `cands`).
    pub filled: Option<String>,
    /// top candidates that can play this lane (open lanes only).
    pub cands: Vec<LaneCand>,
    /// ko of an enemy champ that can play here and our top candidate counters.
    pub counter_of: Option<String>,
    /// the recommended lane to pick this turn (★).
    pub star: bool,
}

pub struct LaneView {
    pub explore: bool,
    pub age_ms: u64,
    pub my_pick: Vec<String>,
    pub opp_pick: Vec<String>,
    pub my_ban: Vec<String>,
    pub opp_ban: Vec<String>,
    pub lanes: Vec<Lane>, // always 5
    pub picks_made: usize,
    pub timing: &'static str, // 선픽 / 중반 / 후픽
    pub tip: &'static str,
    pub matches: usize,
    pub pool_size: usize,
    /// false when no POS data is loaded → caller should fall back to a flat list.
    pub has_pos: bool,
}

// off-lane placement penalty: a pick forced into a lane it can't play scores this (so any
// fully-playable assignment is preferred; off-lane only when picks outnumber a shared lane).
const OFF_LANE_PENALTY: f32 = -1.0;

/// Assign my picks to DISTINCT lanes OPTIMALLY — not greedy-by-pick-order. Each pick goes to a
/// lane it can play, maximizing the team's total per-lane winrate; off-lane only as a last
/// resort. Brute-forces all assignments (≤5 picks → ≤120 leaves). Fixes greedy's two failures:
/// (a) a flex pick stealing a single-lane pick's only lane, and (b) a champ dumped into a lane
/// it cannot play once its lanes are taken.
fn assign_lanes<'a>(tables: &Tables, allies: &[&'a str]) -> [Option<&'a str>; 5] {
    let n = allies.len().min(5);
    let mut filled: [Option<&str>; 5] = [None; 5];
    if n == 0 {
        return filled;
    }
    // value[pick][lane] = per-lane winrate if playable, else OFF_LANE_PENALTY. No POS data for a
    // champ → treat every lane as playable at its overall winrate (it fills whatever's left).
    let mut val = [[0f32; 5]; 5];
    for (i, p) in allies.iter().take(5).enumerate() {
        let pl = tables.pos_play.get(*p);
        for l in 0u8..5 {
            let playable = pl.map(|v| v.contains(&l)).unwrap_or(true);
            val[i][l as usize] = if playable { tables.delta_for(p, Some(l)) } else { OFF_LANE_PENALTY };
        }
    }
    let mut best_score = f32::NEG_INFINITY;
    let mut best: [usize; 5] = [usize::MAX; 5]; // lane → pick index
    let mut cur: [usize; 5] = [usize::MAX; 5];
    assign_rec(&val, n, 0, 0.0, &mut cur, &mut best_score, &mut best);
    for (l, &pi) in best.iter().enumerate() {
        if pi != usize::MAX {
            filled[l] = Some(allies[pi]);
        }
    }
    filled
}

fn assign_rec(
    val: &[[f32; 5]; 5],
    n: usize,
    pick: usize,
    score: f32,
    cur: &mut [usize; 5],
    best_score: &mut f32,
    best: &mut [usize; 5],
) {
    if pick == n {
        if score > *best_score {
            *best_score = score;
            *best = *cur;
        }
        return;
    }
    for l in 0..5 {
        if cur[l] == usize::MAX {
            cur[l] = pick;
            assign_rec(val, n, pick + 1, score + val[pick][l], cur, best_score, best);
            cur[l] = usize::MAX;
        }
    }
}

pub fn recommend_lanes(
    tables: &Tables,
    s: &DraftState,
    human_is_enemy: bool,
    pool: Option<&HashSet<String>>,
    fearless: bool,
) -> LaneView {
    let (my_pick, opp_pick, my_ban, opp_ban) = if human_is_enemy {
        (&s.enemy_pick, &s.ally_pick, &s.enemy_ban, &s.ally_ban)
    } else {
        (&s.ally_pick, &s.enemy_pick, &s.ally_ban, &s.enemy_ban)
    };

    let mut taken: HashSet<&str> = HashSet::new();
    for v in [&s.ally_pick, &s.enemy_pick, &s.ally_ban, &s.enemy_ban] {
        for id in v {
            taken.insert(id.as_str());
        }
    }
    let allies: Vec<&str> = my_pick.iter().map(|x| x.as_str()).collect();
    let enemies: Vec<&str> = opp_pick.iter().map(|x| x.as_str()).collect();

    // assign my picks to lanes OPTIMALLY (global max-winrate, playable-respecting) — not the old
    // greedy pick-order pass that could dump a champ into an unplayable lane (see assign_lanes).
    let filled: [Option<&str>; 5] = assign_lanes(tables, &allies);

    let picks_made = allies.len();
    // manual pick-order override (s.pick_order): 1=선픽 forces early, 2=후픽 forces late.
    let (early, late) = match s.pick_order {
        1 => (true, false),
        2 => (false, true),
        _ => (picks_made <= 1, picks_made >= 3),
    };
    let (timing, tip) = if early {
        ("선픽", "플렉스·강픽 우선 (라인 안 들키게)")
    } else if late {
        ("후픽", "카운터 우선 (상대 드러난 라인 받아치기)")
    } else {
        ("중반", "시너지·조합 채우기")
    };
    // fearless: depletion management dominates → explain why ★ moves to the thin lane.
    let tip = if fearless { "피어리스: 얕은 라인 먼저 확보 · 플렉스 우선" } else { tip };

    // fearless: remaining S/A-tier depth per lane → drives "secure the shallowest lane first".
    let strong_left: Option<[u32; 5]> = if fearless {
        let avail: HashSet<String> = CHAMPS
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !taken.contains(id) && pool.map(|p| p.contains(*id)).unwrap_or(true))
            .map(|id| id.to_string())
            .collect();
        Some(tables.strong_per_lane(&avail))
    } else {
        None
    };
    let mut lanes: Vec<Lane> = Vec::with_capacity(5);
    let mut open: Vec<(usize, f32)> = Vec::new(); // (lane index, top-candidate score) for open lanes
    for lane in 0u8..5 {
        if let Some(p) = filled[lane as usize] {
            lanes.push(Lane {
                name: LANES[lane as usize],
                filled: Some(ko_name(p)),
                cands: vec![],
                counter_of: None,
                star: false,
            });
            continue;
        }
        // open lane → rank champs that can play it
        let mut cands: Vec<LaneCand> = Vec::new();
        for (id, _ko) in CHAMPS {
            if taken.contains(id) {
                continue;
            }
            if let Some(pl) = pool {
                if !pl.contains(*id) {
                    continue;
                }
            }
            if !tables.plays(id, lane) {
                continue;
            }
            let (mut score, label) = tables.score_pick_in(id, &allies, &enemies, Some(lane), s.pick_order);
            if early {
                score += W_FLEX * (tables.flex(id).saturating_sub(1).min(2) as f32);
            }
            // fearless: flex pays off on EVERY pick (it can fill whichever lane runs dry).
            if fearless {
                score += W_FLEX_FEARLESS * (tables.flex(id).saturating_sub(1).min(2) as f32);
            }
            cands.push(LaneCand {
                ko: ko_name(id),
                id: id.to_string(),
                score,
                label,
                // low-confidence flag is now per-LANE: a champ with few games IN THIS
                // lane is flagged "?", even if it has many games in its main lane.
                low_conf: tables.low_conf_for(id, Some(lane)),
                role: tables.role_tag(id),
                tier: tables.tier_of(id, Some(lane)),
                chg: tables.chg_mark(id),
            });
        }
        cands.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        cands.truncate(3);

        // counter flag: our top cand vs an enemy that can also play this lane
        let mut counter_of = None;
        if let Some(top) = cands.first() {
            let mut best = 0.03f32;
            for e in &enemies {
                if tables.plays(e, lane) {
                    let adv = tables.counters(&top.id, e);
                    if adv > best {
                        best = adv;
                        counter_of = Some(ko_name(e));
                    }
                }
            }
        }

        if let Some(top) = cands.first() {
            open.push((lane as usize, top.score));
        }
        lanes.push(Lane { name: LANES[lane as usize], filled: None, cands, counter_of, star: false });
    }
    // ★ = the lane to secure THIS turn. Classic: the highest-scoring pick. Fearless: the
    // SHALLOWEST open lane (fewest S/A champs left) — grab it before it dries up — with pick
    // quality as the tie-breaker. This is the depletion fix (verified pro principle).
    let star = if let Some(sl) = strong_left {
        // Urgency: a lane with FEW-BUT-NONZERO strong champs is about to dry up → secure it now.
        // A lane ALREADY at 0 strong has nothing strong left to lose, so it is NOT urgent — rank
        // it last. Otherwise the ★ races to an already-dead lane (e.g. all lanes 0 strong except
        // support=2) instead of grabbing the last scarce strong champs while they exist.
        let urgency = |l: usize| -> u32 { if sl[l] == 0 { u32::MAX } else { sl[l] } };
        open.iter().copied().min_by(|a, b| {
            urgency(a.0)
                .cmp(&urgency(b.0))
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        })
    } else {
        open.iter().copied().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    };
    if let Some((vi, _)) = star {
        lanes[vi].star = true;
    }

    let ko_list = |ids: &[String]| ids.iter().map(|id| ko_name(id)).collect::<Vec<_>>();
    LaneView {
        explore: s.explore,
        age_ms: now_ms().saturating_sub(s.ts),
        my_pick: ko_list(my_pick),
        opp_pick: ko_list(opp_pick),
        my_ban: ko_list(my_ban),
        opp_ban: ko_list(opp_ban),
        lanes,
        picks_made,
        timing,
        tip,
        matches: tables.matches,
        pool_size: pool.map(|p| p.len()).unwrap_or(0),
        has_pos: tables.has_pos(),
    }
}
