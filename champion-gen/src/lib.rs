//! Champion generator core: parse `bundle.game_data`, then turn a champion spec into the
//! files a TFM2 mod needs (`.data_champion`, the champion-text override, `mod.override_info`,
//! `mod.mod_info`). Reused by the creator GUI.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rand::seq::SliceRandom;
use serde::Deserialize;
use serde_json::{json, Map, Value};

pub mod validate;

// ============================================================================
// Catalog (mined from the bundle)
// ============================================================================

pub struct Catalog {
    pub base_champion_i18n: Value,
    pub skill_icon_tags: Vec<String>,
    pub champion_sprites: Vec<String>,
    pub category_stat_avg: BTreeMap<String, Map<String, Value>>,
    pub category_growth_avg: BTreeMap<String, Map<String, Value>>,
    pub stat_ranges: BTreeMap<String, (i64, i64, i64)>,
    pub growth_ranges: BTreeMap<String, (i64, i64, i64)>,
    pub base_power_avg: f64,
    pub icon_sheet_png: Vec<u8>,
    pub icon_rects: BTreeMap<String, [f32; 4]>,
    /// Names of base skill-effect animations (e.g. explosion, fire, freeze) for impact visuals.
    pub effect_anims: Vec<String>,
    /// Names of base projectile/effect sprites for projectile visuals.
    pub projectile_sprites: Vec<String>,
    /// PNG bytes per projectile sprite name (single-image sprites) — for previews.
    pub projectile_png: BTreeMap<String, Vec<u8>>,
    /// (#sheet PNG bytes, first-frame pixel rect [x,y,w,h]) per effect anim name — for previews.
    pub effect_preview: BTreeMap<String, (Vec<u8>, [f32; 4])>,
}

pub const STAT_FIELDS: &[&str] = &[
    "attack", "magic_power", "hp", "defence", "magic_resistance", "move_speed", "hp_regen", "stack", "crit_chance",
];

/// Stat weights for the balance "power score".
const POWER_W: &[(&str, f64)] = &[
    ("attack", 1.0), ("magic_power", 1.0), ("hp", 0.15), ("defence", 2.5),
    ("magic_resistance", 2.5), ("move_speed", 0.08), ("hp_regen", 1.0), ("crit_chance", 1.0),
];
const POWER_LEVEL: f64 = 9.0;

fn score_with(get: impl Fn(&str) -> (i64, i64)) -> f64 {
    POWER_W.iter().map(|(k, w)| { let (s, g) = get(k); w * (s as f64 + g as f64 * POWER_LEVEL) }).sum()
}
pub fn power_score_maps(stat: &Map<String, Value>, growth: &Map<String, Value>) -> f64 {
    score_with(|k| (
        stat.get(k).and_then(|v| v.as_i64()).unwrap_or(0),
        growth.get(k).and_then(|v| v.as_i64()).unwrap_or(0),
    ))
}
pub fn power_score_pairs(stat: &[(String, i64)], growth: &[(String, i64)]) -> f64 {
    let look = |list: &[(String, i64)], k: &str| list.iter().find(|(kk, _)| kk == k).map(|(_, v)| *v).unwrap_or(0);
    score_with(|k| (look(stat, k), look(growth, k)))
}

fn compute_stat_reference(
    info: &Value,
) -> (BTreeMap<String, Map<String, Value>>, BTreeMap<String, Map<String, Value>>, BTreeMap<String, (i64, i64, i64)>, BTreeMap<String, (i64, i64, i64)>) {
    let mut cat_stat_sum: BTreeMap<String, (BTreeMap<String, i64>, i64)> = BTreeMap::new();
    let mut cat_growth_sum: BTreeMap<String, (BTreeMap<String, i64>, i64)> = BTreeMap::new();
    let mut stat_mma: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    let mut growth_mma: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    let mut n = 0i64;
    let Some(obj) = info.as_object() else { return (BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), BTreeMap::new()); };
    for (name, c) in obj {
        if name == "mod_champions" { continue; }
        let (Some(stat), Some(growth)) = (c.get("stat").and_then(|x| x.as_object()), c.get("growth").and_then(|x| x.as_object())) else { continue; };
        let category = c.get("category").and_then(|x| x.as_str()).unwrap_or("Melee").to_string();
        n += 1;
        let cs = cat_stat_sum.entry(category.clone()).or_default();
        let cg = cat_growth_sum.entry(category).or_default();
        cs.1 += 1; cg.1 += 1;
        for f in STAT_FIELDS {
            let sv = stat.get(*f).and_then(|v| v.as_i64()).unwrap_or(0);
            let gv = growth.get(*f).and_then(|v| v.as_i64()).unwrap_or(0);
            *cs.0.entry(f.to_string()).or_insert(0) += sv;
            *cg.0.entry(f.to_string()).or_insert(0) += gv;
            let e = stat_mma.entry(f.to_string()).or_insert((i64::MAX, 0, i64::MIN));
            e.0 = e.0.min(sv); e.1 += sv; e.2 = e.2.max(sv);
            let g = growth_mma.entry(f.to_string()).or_insert((i64::MAX, 0, i64::MIN));
            g.0 = g.0.min(gv); g.1 += gv; g.2 = g.2.max(gv);
        }
    }
    let to_avg_map = |sums: BTreeMap<String, (BTreeMap<String, i64>, i64)>| {
        sums.into_iter().map(|(cat, (fields, cnt))| {
            let m: Map<String, Value> = fields.into_iter()
                .map(|(k, sum)| (k, json!(if cnt > 0 { sum / cnt } else { 0 }))).collect();
            (cat, m)
        }).collect::<BTreeMap<_, _>>()
    };
    let to_ranges = |mma: BTreeMap<String, (i64, i64, i64)>| {
        mma.into_iter().map(|(k, (mn, sum, mx))| (k, (mn, if n > 0 { sum / n } else { 0 }, mx))).collect::<BTreeMap<_, _>>()
    };
    (to_avg_map(cat_stat_sum), to_avg_map(cat_growth_sum), to_ranges(stat_mma), to_ranges(growth_mma))
}

impl Catalog {
    pub fn skill_icon_champions(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for t in &self.skill_icon_tags {
            if let Some(idx) = t.rfind('_') {
                if t[idx + 1..].parse::<u32>().is_ok() { set.insert(t[..idx].to_string()); }
            }
        }
        set.into_iter().collect()
    }
    pub fn role_stat_seed(&self, role: &str) -> Vec<(String, i64)> {
        match self.category_stat_avg.get(category_for_role(role_from_str(role))) {
            Some(m) => map_to_pairs(m), None => role_base_stats(role),
        }
    }
    pub fn role_growth_seed(&self, role: &str) -> Vec<(String, i64)> {
        match self.category_growth_avg.get(category_for_role(role_from_str(role))) {
            Some(m) => map_to_pairs(m), None => role_base_growth(role),
        }
    }
}

fn read_u32(buf: &[u8], pos: &mut usize) -> Result<u32> {
    let b = buf.get(*pos..*pos + 4).ok_or_else(|| anyhow!("EOF u32"))?;
    *pos += 4;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn read_str(buf: &[u8], pos: &mut usize) -> Result<String> {
    let n = read_u32(buf, pos)? as usize;
    let s = buf.get(*pos..*pos + n).ok_or_else(|| anyhow!("EOF str"))?;
    *pos += n;
    Ok(String::from_utf8_lossy(s).into_owned())
}
fn maybe_gunzip(data: &[u8]) -> String {
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        let mut out = String::new();
        if flate2::read::GzDecoder::new(data).read_to_string(&mut out).is_ok() { return out; }
    }
    String::from_utf8_lossy(data).into_owned()
}
fn maybe_gunzip_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        let mut out = Vec::new();
        if flate2::read::GzDecoder::new(data).read_to_end(&mut out).is_ok() { return out; }
    }
    data.to_vec()
}

pub fn load_catalog(bundle_path: &Path) -> Result<Catalog> {
    let buf = std::fs::read(bundle_path).with_context(|| format!("reading bundle {}", bundle_path.display()))?;
    let mut pos = 0usize;
    let _magic = read_u32(&buf, &mut pos)?;

    let mut base_champion_i18n: Option<Value> = None;
    let mut skill_icon_tags: Vec<String> = Vec::new();
    let mut sprites: BTreeSet<String> = BTreeSet::new();
    let mut champion_info: Option<Value> = None;
    let mut icon_sheet_png: Vec<u8> = Vec::new();
    let mut icon_rects: BTreeMap<String, [f32; 4]> = BTreeMap::new();
    let mut effect_anims: BTreeSet<String> = BTreeSet::new();
    let mut projectile_sprites: BTreeSet<String> = BTreeSet::new();
    let mut projectile_png: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut effect_sheets: BTreeMap<String, Vec<u8>> = BTreeMap::new();   // name -> #sheet PNG
    let mut effect_first_rect: BTreeMap<String, [f32; 4]> = BTreeMap::new(); // name -> first-frame pixel rect

    while pos + 8 < buf.len() {
        let _ext = read_str(&buf, &mut pos)?;
        let path = read_str(&buf, &mut pos)?;
        let dlen = read_u32(&buf, &mut pos)? as usize;
        let data_start = pos;
        pos += dlen;

        if path == "asset/base/text/champion" {
            let txt = maybe_gunzip(&buf[data_start..data_start + dlen]);
            base_champion_i18n = Some(serde_json::from_str(&txt).context("parsing base champion i18n")?);
        } else if path == "asset/base/aseprite_resources/UI_aseprite/skill_icon#data" {
            let txt = maybe_gunzip(&buf[data_start..data_start + dlen]);
            let v: Value = serde_json::from_str(&txt).context("parsing skill_icon #data")?;
            if let Some(imgs) = v.get("images").and_then(|x| x.as_object()) {
                skill_icon_tags = imgs.keys().cloned().collect();
                for (tag, r) in imgs {
                    let g = |k: &str| r.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                    icon_rects.insert(tag.clone(), [g("x"), g("y"), g("w"), g("h")]);
                }
            }
        } else if path == "asset/base/aseprite_resources/UI_aseprite/skill_icon#sheet" {
            icon_sheet_png = maybe_gunzip_bytes(&buf[data_start..data_start + dlen]);
        } else if path == "asset/base/setting/champion_info" {
            let txt = maybe_gunzip(&buf[data_start..data_start + dlen]);
            champion_info = Some(serde_json::from_str(&txt).context("parsing base champion_info")?);
        } else if let Some(rest) = path.strip_prefix("asset/base/aseprite_resources/champions/") {
            let name = rest.split('#').next().unwrap_or(rest);
            if !name.is_empty() && name != "character_sheet" && !name.ends_with("_ult") {
                sprites.insert(name.to_string());
            }
        } else if let Some(rest) = path.strip_prefix("asset/base/aseprite_resources/skill_effect/") {
            let n = rest.split('#').next().unwrap_or(rest).to_string();
            if !n.is_empty() {
                effect_anims.insert(n.clone());
                if rest.ends_with("#sheet") {
                    effect_sheets.insert(n, maybe_gunzip_bytes(&buf[data_start..data_start + dlen]));
                } else if rest.ends_with("#anim") {
                    // first frame's pixel rect: anims.<firstTag>.frames[0].data {x,y,w,h}
                    let txt = maybe_gunzip(&buf[data_start..data_start + dlen]);
                    if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                        if let Some(anims) = v.get("anims").and_then(|a| a.as_object()) {
                            if let Some(first) = anims.values().next() {
                                if let Some(d) = first.get("frames").and_then(|f| f.get(0)).and_then(|f0| f0.get("data")) {
                                    let g = |k: &str| d.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                                    effect_first_rect.insert(n, [g("x"), g("y"), g("w"), g("h")]);
                                }
                            }
                        }
                    }
                }
            }
        } else if let Some(rest) = path.strip_prefix("asset/base/sprite/") {
            let n = rest.split('#').next().unwrap_or(rest).to_string();
            if !n.is_empty() && !rest.contains('#') {
                projectile_sprites.insert(n.clone());
                projectile_png.insert(n, maybe_gunzip_bytes(&buf[data_start..data_start + dlen]));
            } else if !n.is_empty() {
                projectile_sprites.insert(n);
            }
        }
    }

    let (category_stat_avg, category_growth_avg, stat_ranges, growth_ranges) = match &champion_info {
        Some(v) => compute_stat_reference(v),
        None => Default::default(),
    };

    let mut base_power_avg = 0.0;
    if let Some(obj) = champion_info.as_ref().and_then(|v| v.as_object()) {
        let mut sum = 0.0; let mut n = 0.0;
        for (name, c) in obj {
            if name == "mod_champions" { continue; }
            if let (Some(s), Some(g)) = (c.get("stat").and_then(|x| x.as_object()), c.get("growth").and_then(|x| x.as_object())) {
                sum += power_score_maps(s, g); n += 1.0;
            }
        }
        if n > 0.0 { base_power_avg = sum / n; }
    }

    Ok(Catalog {
        base_champion_i18n: base_champion_i18n.ok_or_else(|| anyhow!("asset/base/text/champion not found"))?,
        skill_icon_tags,
        champion_sprites: sprites.into_iter().collect(),
        category_stat_avg, category_growth_avg, stat_ranges, growth_ranges,
        base_power_avg, icon_sheet_png, icon_rects,
        effect_anims: effect_anims.into_iter().collect(),
        projectile_sprites: projectile_sprites.into_iter().collect(),
        projectile_png,
        effect_preview: effect_sheets.into_iter()
            .filter_map(|(name, png)| {
                let rect = effect_first_rect.get(&name).copied().unwrap_or([0.0, 0.0, 0.0, 0.0]);
                Some((name, (png, rect)))
            })
            .collect(),
    })
}

pub fn category_for_role(role: Role) -> &'static str {
    match role {
        Role::Mage => "Magician", Role::AdCarry => "Range", Role::Assassin => "Assassin",
        Role::Tank => "Melee", Role::Support => "Util",
    }
}

// ============================================================================
// Spec
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ModSpec {
    pub mod_id: String,
    #[serde(default)] pub author: String,
    #[serde(default = "default_version")] pub version: String,
    pub champions: Vec<ChampionSpec>,
}
fn default_version() -> String { "0.1.0".into() }

#[derive(Debug, Deserialize)]
pub struct ChampionSpec {
    pub id: String,
    pub name: LocalizedName,
    pub role: Role,
    #[serde(default)] pub tags: Vec<String>,
    #[serde(default)] pub stat_overrides: Map<String, Value>,
    #[serde(default)] pub growth_overrides: Map<String, Value>,
    #[serde(default)] pub sprite: AssetPick,
    #[serde(default)] pub skill_icon: AssetPick,
    #[serde(default)] pub skill_icon_uploads: Vec<String>,
    pub skills: Vec<SkillSpec>,
}

#[derive(Debug, Deserialize)]
pub struct LocalizedName { pub ko: String, pub en: String }

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Role { Mage, AdCarry, Assassin, Tank, Support }

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AssetPick {
    Select { value: String },
    Random,
    Upload { path: String },
}
impl Default for AssetPick { fn default() -> Self { AssetPick::Random } }

/// A composable, user-friendly skill: delivery + damage + status add-ons + support + numbers.
#[derive(Debug, Deserialize)]
pub struct SkillSpec {
    pub slot: Slot,
    #[serde(default)] pub delivery: Delivery,
    #[serde(default)] pub damage: DamageKind,
    #[serde(default)] pub damage_ratio: i64,
    #[serde(default)] pub damage_flat: i64,
    #[serde(default)] pub statuses: Vec<StatusSpec>,
    #[serde(default)] pub heal: i64,
    #[serde(default)] pub shield: i64,
    #[serde(default)] pub buff: BuffSpec,
    #[serde(default)] pub ally_target: bool,
    #[serde(default = "d_range")] pub range: i64,
    #[serde(default = "d_radius")] pub radius: i64,
    #[serde(default = "d_speed")] pub speed: i64,
    #[serde(default = "d_cool")] pub cooltime: i64,
    #[serde(default)] pub penetrate: bool,
    /// impact effect animation name (from Catalog.effect_anims); "" = none.
    #[serde(default)] pub impact: String,
    /// projectile visual name (sprite from projectile_sprites, or effect anim); "" = default arrow.
    #[serde(default)] pub projectile: String,
}
fn d_range() -> i64 { 60000 }
fn d_radius() -> i64 { 13000 }
fn d_speed() -> i64 { 4200 }
fn d_cool() -> i64 { 480 }

#[derive(Debug, Deserialize, Default, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    #[default] LinearProjectile,
    HomingProjectile,
    AreaSelf,
    AreaTarget,
    Dash,
    Support,
}

#[derive(Debug, Deserialize, Default, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DamageKind { #[default] None, Ad, Ap, Fixed }

#[derive(Debug, Deserialize)]
pub struct StatusSpec {
    pub kind: StatusKind,
    #[serde(default)] pub v: i64,
    #[serde(default)] pub v2: i64,
}
#[derive(Debug, Deserialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind { Stun, Bind, Fear, Knockback, Slow, PoisonDot, FireDot }

#[derive(Debug, Deserialize, Default)]
pub struct BuffSpec {
    #[serde(default)] pub attack: i64,
    #[serde(default)] pub magic_power: i64,
    #[serde(default)] pub attack_speed_mult: i64,
    #[serde(default)] pub move_speed_mult: i64,
    #[serde(default)] pub vamp: i64,
    #[serde(default)] pub skill_cooldown_mult: i64,
    #[serde(default = "d_buff_tick")] pub tick: i64,
}
fn d_buff_tick() -> i64 { 180 }
impl BuffSpec {
    fn any(&self) -> bool {
        self.attack != 0 || self.magic_power != 0 || self.attack_speed_mult != 0
            || self.move_speed_mult != 0 || self.vamp != 0 || self.skill_cooldown_mult != 0
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Slot { Skill, Skill2, Ult }
impl Slot {
    fn action_name(self) -> &'static str {
        match self { Slot::Skill => "skill", Slot::Skill2 => "skill2", Slot::Ult => "ult" }
    }
}

// ============================================================================
// Role presets + seeds
// ============================================================================

struct RolePreset {
    category: &'static str,
    tags: &'static [&'static str],
    ranged: bool,
    stat: Value,
    growth: Value,
}

fn role_preset(role: Role) -> RolePreset {
    let s = |a, mp, hp, def, mr, ms, hpr| {
        json!({ "attack": a, "magic_power": mp, "hp": hp, "defence": def,
                "magic_resistance": mr, "move_speed": ms, "hp_regen": hpr, "stack": 0, "crit_chance": 0 })
    };
    match role {
        Role::Mage => RolePreset { category: "Magician", tags: &["AP", "Range"], ranged: true,
            stat: s(82, 34, 925, 22, 21, 933, 0), growth: s(8, 8, 100, 4, 4, 0, 0) },
        Role::AdCarry => RolePreset { category: "Range", tags: &["AD", "Range"], ranged: true,
            stat: s(100, 0, 900, 21, 15, 900, 0), growth: s(12, 0, 95, 4, 3, 0, 0) },
        Role::Assassin => RolePreset { category: "Assassin", tags: &["AD", "Melee"], ranged: false,
            stat: s(116, 0, 900, 24, 15, 1056, 0), growth: s(14, 0, 95, 4, 3, 0, 0) },
        Role::Tank => RolePreset { category: "Melee", tags: &["Tank", "Melee"], ranged: false,
            stat: s(95, 1, 1014, 33, 23, 1033, 0), growth: s(9, 0, 120, 6, 5, 0, 0) },
        Role::Support => RolePreset { category: "Util", tags: &["Heal", "Shield"], ranged: true,
            stat: s(82, 22, 975, 24, 21, 1000, 0), growth: s(8, 5, 100, 4, 4, 0, 0) },
    }
}

pub fn role_from_str(role: &str) -> Role {
    match role {
        "mage" => Role::Mage, "ad_carry" => Role::AdCarry, "assassin" => Role::Assassin,
        "tank" => Role::Tank, _ => Role::Support,
    }
}

pub const EDIT_FIELDS: &[&str] = &[
    "attack", "magic_power", "hp", "defence", "magic_resistance", "move_speed", "hp_regen", "crit_chance",
];
fn map_to_pairs(m: &Map<String, Value>) -> Vec<(String, i64)> {
    EDIT_FIELDS.iter().map(|k| (k.to_string(), m.get(*k).and_then(|v| v.as_i64()).unwrap_or(0))).collect()
}
pub fn role_base_stats(role: &str) -> Vec<(String, i64)> {
    map_to_pairs(&role_preset(role_from_str(role)).stat.as_object().cloned().unwrap_or_default())
}
pub fn role_base_growth(role: &str) -> Vec<(String, i64)> {
    map_to_pairs(&role_preset(role_from_str(role)).growth.as_object().cloned().unwrap_or_default())
}
pub fn role_base_tags(role: &str) -> Vec<String> {
    role_preset(role_from_str(role)).tags.iter().map(|s| s.to_string()).collect()
}

// ============================================================================
// Build actions
// ============================================================================

fn basic_attack(id: &str, ranged: bool) -> Value {
    if ranged {
        json!({
            "action_name": "attack", "duration": 18, "cooltime": 60, "start_timing": 10,
            "cancelable": true, "range": 52000, "casting_type": "Targeting",
            "casting_target": "Enemy", "attack_type": "BaseAttack",
            "effect": { "type": "TargetProjectile", "speed": 4500, "name": format!("{id}_attack"),
                "applied_target": "Enemy", "applied_effects": [
                    { "effect": { "type": "Attack", "damage": 0, "attack_ratio": 100 }, "casting_type": "Targeting" } ] }
        })
    } else {
        json!({
            "action_name": "attack", "duration": 18, "cooltime": 0, "start_timing": 9,
            "range": 13000, "casting_type": "Targeting", "casting_target": "Enemy", "attack_type": "BaseAttack",
            "effect": { "type": "RangeEffect", "shape": { "Circle": { "radius": 13000 } },
                "target": "Enemy", "apply_type": "AroundCaster",
                "effects": [ { "type": "Attack", "damage": 0, "attack_ratio": 100 } ] }
        })
    }
}

/// Compose a data action from a friendly, composable skill spec.
fn build_skill(id: &str, s: &SkillSpec) -> Result<Value> {
    let name = s.slot.action_name();
    let desc = format!("#asset/base/text/champion?description.{id}.{name}");

    let status_effect = |st: &StatusSpec| -> Value {
        match st.kind {
            StatusKind::Stun => json!({"type":"Stun","duration": st.v.max(1)}),
            StatusKind::Bind => json!({"type":"Bind","duration": st.v.max(1)}),
            StatusKind::Fear => json!({"type":"Fear","tick": st.v.max(1)}),
            StatusKind::Knockback => json!({"type":"Knockback","speed": if st.v2 > 0 { st.v2 } else { 2200 }, "tick": if st.v > 0 { st.v } else { 10 }}),
            StatusKind::Slow => json!({"type":"AddBuff","buff_state":{"name":"slow","duration":{"Time":{"tick": st.v.max(1)}},"move_speed_mult": -(st.v2.max(0))}}),
            StatusKind::PoisonDot => json!({"type":"AddCasted","duration": st.v.max(1),"period":30,"casted_type":"Poison","effects":[{"type":"ApAttack","damage": st.v2.max(0),"attack_ratio":0}]}),
            StatusKind::FireDot => json!({"type":"AddCasted","duration": st.v.max(1),"period":30,"casted_type":"Fire","effects":[{"type":"ApAttack","damage": st.v2.max(0),"attack_ratio":0}]}),
        }
    };

    let mut payload: Vec<Value> = Vec::new();
    match s.damage {
        DamageKind::Ad => payload.push(json!({"type":"Attack","damage":s.damage_flat,"attack_ratio":s.damage_ratio})),
        DamageKind::Ap => payload.push(json!({"type":"ApAttack","damage":s.damage_flat,"attack_ratio":s.damage_ratio})),
        DamageKind::Fixed => payload.push(json!({"type":"FixedAttack","damage":s.damage_flat})),
        DamageKind::None => {}
    }
    for st in &s.statuses { payload.push(status_effect(st)); }
    if !s.impact.is_empty() { payload.push(json!({"type":"ViewEffect","name":format!("{id}_{name}_impact")})); }
    let wrapped: Vec<Value> = payload.iter().map(|e| json!({"effect": e, "casting_type":"Targeting"})).collect();
    let shape = json!({"Circle": {"radius": s.radius}});

    let action = match s.delivery {
        Delivery::LinearProjectile => json!({
            "action_name":name,"description":desc,"duration":20,"cooltime":s.cooltime,"start_timing":10,"range":s.range,
            "casting_type":"Direction","casting_target":"Enemy","attack_type":"Skill",
            "effect":{"type":"LinearProjectile","penetrate":s.penetrate,"speed":s.speed,"range":s.range,"name":format!("{id}_{name}"),
                "shape":shape,"applied_target":"Enemy","applied_effects": wrapped}
        }),
        Delivery::HomingProjectile => json!({
            "action_name":name,"description":desc,"duration":20,"cooltime":s.cooltime,"start_timing":10,"range":s.range,
            "casting_type":"Targeting","casting_target":"Enemy","attack_type":"Skill",
            "effect":{"type":"TargetProjectile","speed":s.speed,"name":format!("{id}_{name}"),
                "applied_target":"Enemy","applied_effects": wrapped}
        }),
        Delivery::AreaSelf => json!({
            "action_name":name,"description":desc,"duration":22,"cooltime":s.cooltime,"start_timing":10,"range":s.radius,
            "casting_type":"None","casting_target":"Enemy","attack_type":"Skill",
            "effect":{"type":"RangeEffect","shape":shape,"target":"Enemy","apply_type":"AroundCaster","effects": payload}
        }),
        Delivery::AreaTarget => json!({
            "action_name":name,"description":desc,"duration":22,"cooltime":s.cooltime,"start_timing":12,"range":s.range,
            "casting_type":"Position","casting_target":"Enemy","attack_type":"Skill",
            "effect":{"type":"RangeProjectile","name":format!("{id}_{name}"),"delay":24,"apply":30,"shape":shape,
                "applied_target":"Enemy","applied_effects": wrapped}
        }),
        Delivery::Dash => json!({
            "action_name":name,"description":desc,"duration":14,"cooltime":s.cooltime,"start_timing":2,"range":s.range,
            "casting_type":"Position","casting_target":"None","attack_type":"Skill",
            "effect":{"type":"Rush","speed":s.speed,"range":s.range,"casting_target":"Enemy","penetrate":s.penetrate,
                "applied_effects": wrapped}
        }),
        Delivery::Support => {
            let target = if s.ally_target { "AllyChampion" } else { "AllyOnlySelf" };
            let mut eff: Vec<Value> = Vec::new();
            if s.heal > 0 { eff.push(json!({"type":"Heal","amount":s.heal,"heal_type": if s.ally_target {"Ally"} else {"Caster"}})); }
            if s.shield > 0 { eff.push(json!({"type":"Shield","amount":s.shield,"tick":300})); }
            if s.buff.any() {
                eff.push(json!({"type":"AddCasterBuff","buff_state":{
                    "name":format!("{id}_{name}_buff"),"duration":{"Time":{"tick": s.buff.tick}},
                    "attack":s.buff.attack,"magic_power":s.buff.magic_power,
                    "attack_speed_mult":s.buff.attack_speed_mult,"move_speed_mult":s.buff.move_speed_mult,
                    "vamp":s.buff.vamp,"skill_cooldown_mult":s.buff.skill_cooldown_mult}}));
            }
            if eff.is_empty() { eff.push(json!({"type":"Heal","amount":1,"heal_type":"Caster"})); }
            json!({
                "action_name":name,"description":desc,"duration":16,"cooltime":s.cooltime,"start_timing":6,"range":0,
                "casting_type":"None","casting_target":target,"attack_type":"Skill",
                "effect":{"type":"RangeEffect","shape":shape,"target":target,"apply_type":"AroundCaster","effects": eff}
            })
        }
    };
    Ok(action)
}

fn skill_blurb(s: &SkillSpec) -> String {
    let delivery = match s.delivery {
        Delivery::LinearProjectile => "직선 투사체", Delivery::HomingProjectile => "유도 투사체",
        Delivery::AreaSelf => "주변 광역", Delivery::AreaTarget => "지정 광역",
        Delivery::Dash => "돌진", Delivery::Support => "지원",
    };
    let mut parts: Vec<String> = Vec::new();
    match s.damage {
        DamageKind::Ad => parts.push(format!("공격력 {}% 피해", s.damage_ratio)),
        DamageKind::Ap => parts.push(format!("주문력 {}% 피해", s.damage_ratio)),
        DamageKind::Fixed => parts.push(format!("고정 {} 피해", s.damage_flat)),
        DamageKind::None => {}
    }
    for st in &s.statuses {
        parts.push(match st.kind {
            StatusKind::Stun => format!("기절 {}틱", st.v), StatusKind::Bind => format!("속박 {}틱", st.v),
            StatusKind::Fear => format!("공포 {}틱", st.v), StatusKind::Knockback => "넉백".to_string(),
            StatusKind::Slow => format!("슬로우 {}%", st.v2), StatusKind::PoisonDot => "독 도트".to_string(),
            StatusKind::FireDot => "화상 도트".to_string(),
        });
    }
    if s.heal > 0 { parts.push(format!("치유 {}", s.heal)); }
    if s.shield > 0 { parts.push(format!("보호막 {}", s.shield)); }
    if s.buff.any() { parts.push("자기 강화".to_string()); }
    if parts.is_empty() { delivery.to_string() } else { format!("{delivery} — {}", parts.join(", ")) }
}

// ============================================================================
// Emit
// ============================================================================

pub struct Emitted {
    pub data_champion: Value,
    pub i18n: Vec<(String, Value)>,
}

fn resolve_sprite(pick: &AssetPick, cat: &Catalog, rng: &mut impl rand::Rng,
    mod_id: &str, champ_id: &str, out_dir: &Path) -> Result<(String, Option<String>)> {
    match pick {
        AssetPick::Select { value } =>
            Ok((format!("asset/base/aseprite_resources/champions/{value}"), Some(String::new()))),
        AssetPick::Random => {
            let n = cat.champion_sprites.choose(rng).cloned().ok_or_else(|| anyhow!("no champion sprites"))?;
            Ok((format!("asset/base/aseprite_resources/champions/{n}"), Some(String::new())))
        }
        AssetPick::Upload { path } => {
            let dir = out_dir.join("champions");
            std::fs::create_dir_all(&dir)?;
            std::fs::copy(path, dir.join(format!("{champ_id}.png"))).with_context(|| format!("copying sprite '{path}'"))?;
            Ok((format!("asset/{mod_id}/champions/{champ_id}"), None))
        }
    }
}

fn resolve_icon(pick: &AssetPick, cat: &Catalog, rng: &mut impl rand::Rng,
    mod_id: &str, champ_id: &str, out_dir: &Path) -> Result<(String, Value)> {
    let sheet = |champ: &str| json!({
        "source": "asset/base/aseprite_resources/UI_aseprite/skill_icon",
        "tags": [format!("{champ}_0"), format!("{champ}_1"), format!("{champ}_2")]
    });
    match pick {
        AssetPick::Select { value } => Ok(("skill_icon".into(), sheet(value))),
        AssetPick::Random => {
            let c = cat.skill_icon_champions().choose(rng).cloned().ok_or_else(|| anyhow!("no icon champions"))?;
            Ok(("skill_icon".into(), sheet(&c)))
        }
        AssetPick::Upload { path } => {
            let dir = out_dir.join("icons");
            std::fs::create_dir_all(&dir)?;
            std::fs::copy(path, dir.join(format!("{champ_id}.png"))).with_context(|| format!("copying icon '{path}'"))?;
            let p = format!("asset/{mod_id}/icons/{champ_id}");
            Ok(("skill_icons".into(), json!([p, p, p])))
        }
    }
}

/// Resolve skill icons, supporting per-slot uploads which override the AssetPick.
fn build_icons(spec: &ChampionSpec, cat: &Catalog, rng: &mut impl rand::Rng,
    mod_id: &str, out_dir: &Path) -> Result<(String, Value)> {
    if spec.skill_icon_uploads.iter().any(|p| !p.is_empty()) {
        let dir = out_dir.join("icons");
        std::fs::create_dir_all(&dir)?;
        let mut slot: [Option<String>; 3] = [None, None, None];
        for i in 0..3 {
            if let Some(src) = spec.skill_icon_uploads.get(i) {
                if !src.is_empty() {
                    std::fs::copy(src, dir.join(format!("{}_{}.png", spec.id, i))).with_context(|| format!("copying icon '{src}'"))?;
                    slot[i] = Some(format!("asset/{mod_id}/icons/{}_{}", spec.id, i));
                }
            }
        }
        let first = slot.iter().flatten().next().cloned().unwrap_or_default();
        let arr: Vec<String> = (0..3).map(|i| slot[i].clone().unwrap_or_else(|| first.clone())).collect();
        Ok(("skill_icons".into(), json!(arr)))
    } else {
        resolve_icon(&spec.skill_icon, cat, rng, mod_id, &spec.id, out_dir)
    }
}

pub fn build_champion(spec: &ChampionSpec, cat: &Catalog, rng: &mut impl rand::Rng,
    mod_id: &str, out_dir: &Path) -> Result<Emitted> {
    let preset = role_preset(spec.role);
    let cat_name = category_for_role(spec.role);

    let mut stat = cat.category_stat_avg.get(cat_name).cloned().map(Value::Object).unwrap_or_else(|| preset.stat.clone());
    if let Some(obj) = stat.as_object_mut() { for (k, v) in &spec.stat_overrides { obj.insert(k.clone(), v.clone()); } }
    let mut growth = cat.category_growth_avg.get(cat_name).cloned().map(Value::Object).unwrap_or_else(|| preset.growth.clone());
    if let Some(obj) = growth.as_object_mut() { for (k, v) in &spec.growth_overrides { obj.insert(k.clone(), v.clone()); } }

    let (sprite_path, anim_prefix) = resolve_sprite(&spec.sprite, cat, rng, mod_id, &spec.id, out_dir)?;
    let (icon_field, icon_val) = build_icons(spec, cat, rng, mod_id, out_dir)?;

    let mut champ = Map::new();
    champ.insert("id".into(), json!(spec.id));
    champ.insert("category".into(), json!(preset.category));
    let tags: Vec<String> = if spec.tags.is_empty() { preset.tags.iter().map(|s| s.to_string()).collect() } else { spec.tags.clone() };
    champ.insert("tags".into(), json!(tags));
    champ.insert("sprite".into(), json!(sprite_path));
    if let Some(ap) = anim_prefix { champ.insert("anim_prefix".into(), json!(ap)); }
    champ.insert(icon_field, icon_val);
    champ.insert("stat".into(), stat);
    champ.insert("growth".into(), growth);
    champ.insert("attack".into(), basic_attack(&spec.id, preset.ranged));

    let is_fx = |n: &str| cat.effect_anims.iter().any(|x| x == n);
    let mut view_projectiles = vec![
        json!({ "type": "Sprite", "name": format!("{}_attack", spec.id), "sprite": "asset/base/sprite/arrow" }),
    ];
    let mut view_effects: Vec<Value> = Vec::new();
    for s in &spec.skills {
        let slot = s.slot.action_name();
        let action = build_skill(&spec.id, s)?;
        champ.insert(slot.into(), action);
        if matches!(s.delivery, Delivery::LinearProjectile | Delivery::HomingProjectile | Delivery::AreaTarget) {
            let pname = format!("{}_{}", spec.id, slot);
            if s.projectile.is_empty() {
                view_projectiles.push(json!({"type":"Sprite","name":pname,"sprite":"asset/base/sprite/arrow"}));
            } else if is_fx(&s.projectile) {
                view_projectiles.push(json!({"type":"Animated","name":pname,
                    "anim":format!("asset/base/aseprite_resources/skill_effect/{}", s.projectile),"tag":s.projectile,"repeat":true}));
            } else {
                view_projectiles.push(json!({"type":"Sprite","name":pname,"sprite":format!("asset/base/sprite/{}", s.projectile)}));
            }
        }
        if !s.impact.is_empty() {
            view_effects.push(json!({"type":"Animation","name":format!("{}_{}_impact", spec.id, slot),
                "anim":format!("asset/base/aseprite_resources/skill_effect/{}", s.impact),"tag":s.impact}));
        }
    }
    champ.insert("view_projectiles".into(), json!(view_projectiles));
    if !view_effects.is_empty() { champ.insert("view_effects".into(), json!(view_effects)); }

    let mk = |name: &str| {
        let mut m = Map::new();
        m.insert("name".into(), json!(name));
        for s in &spec.skills { m.insert(s.slot.action_name().into(), json!(skill_blurb(s))); }
        Value::Object(m)
    };
    let i18n = vec![("ko".to_string(), mk(&spec.name.ko)), ("en".to_string(), mk(&spec.name.en))];

    Ok(Emitted { data_champion: Value::Object(champ), i18n })
}

/// Generate the whole mod into `out_dir`. Re-extracts base text every run.
pub fn generate(spec: &ModSpec, cat: &Catalog, out_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut rng = rand::thread_rng();
    let mut written = Vec::new();
    std::fs::create_dir_all(out_dir)?;

    let mut full = cat.base_champion_i18n.clone();
    let mut champs: Vec<Emitted> = Vec::new();
    for c in &spec.champions {
        let e = build_champion(c, cat, &mut rng, &spec.mod_id, out_dir)
            .with_context(|| format!("building champion '{}'", c.id))?;
        let problems = validate::validate_champion(&e.data_champion);
        if !problems.is_empty() {
            bail!("champion '{}' failed validation:\n  - {}", c.id, problems.join("\n  - "));
        }
        champs.push(e);
    }
    inject_i18n(&mut full, spec, &champs);

    let champ_dir = out_dir.join("champion");
    std::fs::create_dir_all(&champ_dir)?;
    for (spec_c, e) in spec.champions.iter().zip(&champs) {
        let f = champ_dir.join(format!("{}.data_champion", spec_c.id));
        std::fs::write(&f, serde_json::to_vec_pretty(&e.data_champion)?)?;
        written.push(f);
    }

    let text_dir = out_dir.join("text");
    std::fs::create_dir_all(&text_dir)?;
    let tf = text_dir.join("champion_full.i18n");
    std::fs::write(&tf, serde_json::to_vec(&full)?)?;
    written.push(tf);

    let oi = out_dir.join("mod.override_info");
    std::fs::write(&oi, serde_json::to_vec_pretty(&json!({
        "asset/base/text/champion": {
            "remapping": format!("asset/{}/text/champion_full", spec.mod_id),
            "type": "override"
        }
    }))?)?;
    written.push(oi);

    let mi = out_dir.join("mod.mod_info");
    std::fs::write(&mi, serde_json::to_vec_pretty(&json!({
        "name": "TFM2 Champion Creator",
        "author": spec.author,
        "version": spec.version,
        "description": format!("Generated champions ({}).", spec.champions.len()),
        "dependencies": [ { "mod_id": "base", "version": ">=0.4.6" } ]
    }))?)?;
    written.push(mi);

    Ok(written)
}

fn inject_i18n(full: &mut Value, _spec: &ModSpec, champs: &[Emitted]) {
    let Some(root) = full.as_object_mut() else { return };
    let langs: Vec<String> = root.keys().cloned().collect();
    for lang in langs {
        let Some(sec) = root.get_mut(&lang).and_then(|s| s.as_object_mut()) else { continue };
        let Some(desc) = sec.get_mut("description").and_then(|d| d.as_object_mut()) else { continue };
        for e in champs {
            let entry = if lang == "ko" { &e.i18n[0].1 } else { &e.i18n[1].1 };
            if let Some(id) = e.data_champion.get("id").and_then(|x| x.as_str()) {
                desc.insert(id.to_string(), entry.clone());
            }
        }
    }
}
