//! Offline validator for generated `.data_champion` JSON.
//!
//! The game loads champions via serde; an invalid enum value or the wrong unit-vs-struct form
//! makes the champion silently vanish (only the log shows the error) and costs a full game restart
//! to discover. This catches those classes BEFORE deploy.
//!
//! Whitelists are ground-truth: enum variant names extracted from the binary's serde data
//! (TeamfightManager2.exe) + the SDK `enums.md`/`effects.md`, cross-checked against what actually
//! loaded in-game. Struct-variant enums (must be `{ "Variant": { .. } }`, never a bare string) are
//! marked; getting this wrong is exactly the `apply_type:"Forward"` bug that bit us.

use serde_json::Value;

/// Valid `DataEffectDef` `type` tags. (binary cluster + Heal/Teleport confirmed by docs + in-game load)
const EFFECT_TYPES: &[&str] = &[
    "Attack", "ApAttack", "FixedAttack", "Heal", "Shield",
    "Stun", "Airborne", "Knockback", "Grab", "Pull", "Fear", "Charm", "Bind", "Taunt",
    "BlockAttack", "BlockSkill", "BlockMoveSkill", "Invisible", "Banish",
    "Rush", "RushTime", "Teleport", "DirTeleport", "MoveBack", "MoveTo", "MoveToTarget", "RushMoveToBack",
    "LinearProjectile", "BackToCasterLinearProjectile", "TargetProjectile", "TargetProjectileFromProjectile",
    "TargetSplashProjectile", "AutoTargetProjectile", "RangeProjectile", "LineRangeProjectile",
    "RangePeriodProjectile", "ApplyInProjectile", "ParabolicProjectile",
    "RangeEffect", "ShrinkingBarrier",
    "AddBuff", "AddCasterBuff", "RemoveCasterBuff", "AddCasted",
    "Combine", "Delayed", "WithSelf", "SwitchByBuff", "SwitchByLevel3", "RandomTarget",
    "ViewEffect", "CasterViewEffect", "CasterAnimation", "RemoveCasterAnimation", "Sfx", "TargetSfx",
];

const CASTING_TYPE: &[&str] = &["Targeting", "Position", "Direction", "None"];
const CASTING_TARGET: &[&str] = &[
    "Ally", "AllyChampion", "AllyChampionInCC", "AllyNotSelf", "AllyOnlySelf",
    "Enemy", "EnemyWithoutTower", "EnemyChampion", "EnemyChampionInCC", "EnemyChampionRecentlyAttacked",
    "Both", "BothWithoutTower", "BothChampion", "None",
];
const ATTACK_TYPE: &[&str] = &["BaseAttack", "Skill", "Dot", "DotIgnoreShield", "Item", "Well"];
const CATEGORY: &[&str] = &["Melee", "Range", "Magician", "Util", "Assassin"];
const CHAMPION_TAG: &[&str] = &["AD", "AP", "Heal", "Shield", "Dot", "CC", "Range", "Melee", "Tank", "Magic"];
const CASTED_TYPE: &[&str] = &["Bleed", "Poison", "Fire", "Heal"];

/// Validate a generated champion `Value`. Returns the list of problems (empty = OK).
pub fn validate_champion(v: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    // top-level required fields + scalar enums
    require_str(v, "id", &mut errs);
    if let Some(c) = v.get("category") { check_unit(c, "category", CATEGORY, &mut errs); }
    if let Some(tags) = v.get("tags").and_then(|t| t.as_array()) {
        for t in tags { check_unit(t, "tags[]", CHAMPION_TAG, &mut errs); }
    }
    for slot in ["attack", "skill", "skill2", "ult"] {
        if let Some(a) = v.get(slot) { walk(a, &format!("{slot}"), &mut errs); }
    }
    errs
}

/// Recursively walk any sub-value, validating every enum-typed field we recognize by key name.
fn walk(v: &Value, path: &str, errs: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let p = format!("{path}.{k}");
                match k.as_str() {
                    "type" if child.is_string() => check_in(child, &p, EFFECT_TYPES, errs),
                    "casting_type" => check_unit(child, &p, CASTING_TYPE, errs),
                    "casting_target" | "applied_target" | "target" if child.is_string() =>
                        check_unit(child, &p, CASTING_TARGET, errs),
                    "attack_type" => check_unit(child, &p, ATTACK_TYPE, errs),
                    "casted_type" => check_unit(child, &p, CASTED_TYPE, errs),
                    "apply_type" => check_apply_type(child, &p, errs),
                    "heal_type" => check_heal_type(child, &p, errs),
                    "attack_effect_type" => check_attack_effect_type(child, &p, errs),
                    "duration" if !child.is_number() => check_buff_type(child, &p, errs),
                    "shape" => check_shape(child, &p, errs),
                    _ => walk(child, &p, errs),
                }
            }
        }
        Value::Array(arr) => for (i, c) in arr.iter().enumerate() { walk(c, &format!("{path}[{i}]"), errs); },
        _ => {}
    }
}

fn require_str(v: &Value, key: &str, errs: &mut Vec<String>) {
    if v.get(key).and_then(|x| x.as_str()).map_or(true, |s| s.is_empty()) {
        errs.push(format!("missing/empty required string `{key}`"));
    }
}

fn check_unit(v: &Value, path: &str, allowed: &[&str], errs: &mut Vec<String>) {
    match v.as_str() {
        Some(s) if allowed.contains(&s) => {}
        Some(s) => errs.push(format!("{path}: invalid value \"{s}\" (allowed: {})", allowed.join(", "))),
        None => errs.push(format!("{path}: expected one of [{}], got non-string {v}", allowed.join(", "))),
    }
}

/// effect `type` may legitimately be a string we don't know (rare) — warn, don't hard-fail unknowns,
/// but DO recurse into the object's other fields regardless (handled by caller via walk of siblings).
fn check_in(v: &Value, path: &str, allowed: &[&str], errs: &mut Vec<String>) {
    if let Some(s) = v.as_str() {
        if !allowed.contains(&s) {
            errs.push(format!("{path}: unknown effect type \"{s}\" (not in known DataEffectDef set)"));
        }
    }
}

fn check_apply_type(v: &Value, path: &str, errs: &mut Vec<String>) {
    match v {
        Value::String(s) if s == "AroundCaster" => {}
        Value::String(s) => errs.push(format!(
            "{path}: invalid apply_type \"{s}\" — use \"AroundCaster\" or {{\"Forward\":{{\"offset\":N}}}}")),
        Value::Object(m) if m.contains_key("Forward") => {}
        _ => errs.push(format!("{path}: apply_type must be \"AroundCaster\" or {{\"Forward\":{{\"offset\":N}}}}")),
    }
}

fn check_heal_type(v: &Value, path: &str, errs: &mut Vec<String>) {
    match v {
        Value::String(s) if ["Caster", "Any", "Ally"].contains(&s.as_str()) => {}
        Value::Object(m) if m.contains_key("AllyAll") => {}
        _ => errs.push(format!("{path}: heal_type must be Caster/Any/Ally or {{\"AllyAll\":{{\"range\":N}}}}")),
    }
}

fn check_attack_effect_type(v: &Value, path: &str, errs: &mut Vec<String>) {
    match v {
        Value::String(s) if ["Target", "EnemyTarget"].contains(&s.as_str()) => {}
        Value::Object(m) if m.contains_key("EnemyAll") => {}
        _ => errs.push(format!("{path}: attack_effect_type must be Target/EnemyTarget or {{\"EnemyAll\":{{\"range\":N}}}}")),
    }
}

fn check_buff_type(v: &Value, path: &str, errs: &mut Vec<String>) {
    match v {
        Value::String(s) if ["Permanent", "WithShield"].contains(&s.as_str()) => {}
        Value::Object(m) if m.contains_key("Time") => {
            if m["Time"].get("tick").and_then(|t| t.as_i64()).is_none() {
                errs.push(format!("{path}: BuffType Time needs {{\"tick\":N}}"));
            }
        }
        _ => errs.push(format!("{path}: duration must be \"Permanent\"/\"WithShield\" or {{\"Time\":{{\"tick\":N}}}}")),
    }
}

fn check_shape(v: &Value, path: &str, errs: &mut Vec<String>) {
    match v {
        Value::Object(m) if m.len() == 1 => {
            let k = m.keys().next().unwrap();
            if !["Circle", "Rect", "DirDot", "Line"].contains(&k.as_str()) {
                errs.push(format!("{path}: shape variant \"{k}\" invalid (Circle/Rect/DirDot/Line)"));
            }
        }
        _ => errs.push(format!("{path}: shape must be {{\"Circle\":{{\"radius\":N}}}} etc.")),
    }
}
