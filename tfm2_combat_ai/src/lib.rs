//! TFM2 Combat AI mod — adds custom champions whose passives encode in-match
//! decisions the built-in planner does NOT make.
//!
//! First champion: **Executioner** — an AD melee assassin.
//!   - Passive "Execute": basic attacks deal bonus damage to low-HP enemies
//!     (the base AI has no per-ability execute targeting).
//!   - Passive "Bloodlust": on takedown, gain a short move/attack-speed snowball.
//!
//! Verified against mod-api API 0.7 / game+SDK 0.4.6. In-match mod code can only
//! READ the battlefield and apply deal_damage/heal/add_buff/apply_cc — see
//! analysis/behavior-design-space.md. Movement & target selection stay with the base AI.

use mod_api::*;

const MOD_ID: &str = "tfm2_combat_ai";

fn init(_ctx: &GameCtx) -> ModRegistration {
    let mut reg = ModRegistration::new(MOD_ID);
    reg.add_champion(Executioner);
    reg
}

declare_mod!(init);

// ----------------------------------------------------------------------------
// Champion
// ----------------------------------------------------------------------------

#[derive(Debug)]
struct Executioner;

impl ModChampionInfo for Executioner {
    fn id(&self) -> &str { "tfm2_executioner" }
    fn name(&self) -> &str { "tfm2_executioner" }
    fn category(&self) -> ChampionCategory { ChampionCategory::Assassin }
    fn tags(&self) -> Vec<ChampionTag> { vec![ChampionTag::AD, ChampionTag::Melee] }

    fn stat(&self) -> EntityStat {
        EntityStat {
            attack: 58,
            magic_power: 0,
            hp: 600,
            defence: 26,
            magic_resistance: 30,
            move_speed: 1150,
            hp_regen: 2,
            stack: 0,
            crit_chance: 0,
        }
    }

    fn growth(&self) -> EntityStat {
        EntityStat {
            attack: 4,
            magic_power: 0,
            hp: 90,
            defence: 3,
            magic_resistance: 3,
            move_speed: 0,
            hp_regen: 1,
            stack: 0,
            crit_chance: 0,
        }
    }

    fn attack(&self) -> Box<dyn ModAction> { Box::new(BasicAttack) }
    fn skill(&self) -> Box<dyn ModAction> { Box::new(LungeStrike) }
    fn skill2(&self) -> Box<dyn ModAction> { Box::new(Bloodthirst) }

    fn passive(&self) -> Option<Box<dyn ModPassive>> {
        Some(Box::new(ExecutePassive::default()))
    }
}

// ----------------------------------------------------------------------------
// Tuning constants (ticks are simulation ticks; ranges use engine units)
// ----------------------------------------------------------------------------

/// Below this HP% an attack is "execute" range and gets bonus damage.
const EXECUTE_THRESHOLD_PCT: usize = 25;
/// Execute bonus as a percent of the target's MAX hp, added to the attack.
const EXECUTE_BONUS_MAXHP_PCT: usize = 8;
/// Snowball buff duration after a takedown.
const SNOWBALL_TICKS: usize = 180;

const MELEE_RANGE: u64 = 13_000;

// ----------------------------------------------------------------------------
// Actions
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct BasicAttack;

impl ModAction for BasicAttack {
    fn clone_box(&self) -> Box<dyn ModAction> { Box::new(self.clone()) }
    fn action_name(&self) -> &str { "attack" }
    fn duration(&self) -> usize { 18 }
    fn cooltime(&self, _stat: &EntityStat, _level: usize) -> usize { 0 }
    fn casting_target(&self) -> CastingTarget { CastingTarget::Enemy }

    fn effect(&self) -> Option<ModEffect> {
        Some(ModEffect {
            range: MELEE_RANGE,
            growth_range: 0,
            start_timing: 9,
            casting: CastingType::Targeting,
            target: CastingTarget::Enemy,
            attack_type: AttackType::BaseAttack,
            effect_type: Box::new(WeaponHit { attack_ratio: 100 }),
        })
    }
}

#[derive(Clone, Debug)]
struct LungeStrike;

impl ModAction for LungeStrike {
    fn clone_box(&self) -> Box<dyn ModAction> { Box::new(self.clone()) }
    fn action_name(&self) -> &str { "skill" }
    fn duration(&self) -> usize { 22 }
    fn cooltime(&self, _stat: &EntityStat, _level: usize) -> usize { 180 }
    fn casting_target(&self) -> CastingTarget { CastingTarget::Enemy }

    fn effect(&self) -> Option<ModEffect> {
        Some(ModEffect {
            range: 30_000,
            growth_range: 0,
            start_timing: 12,
            casting: CastingType::Targeting,
            target: CastingTarget::Enemy,
            attack_type: AttackType::Skill,
            // big single-target hit: 180% of attack + flat
            effect_type: Box::new(WeaponHit { attack_ratio: 180 }),
        })
    }
}

#[derive(Clone, Debug)]
struct Bloodthirst;

impl ModAction for Bloodthirst {
    fn clone_box(&self) -> Box<dyn ModAction> { Box::new(self.clone()) }
    fn action_name(&self) -> &str { "skill2" }
    fn duration(&self) -> usize { 12 }
    fn cooltime(&self, _stat: &EntityStat, _level: usize) -> usize { 300 }
    fn casting_target(&self) -> CastingTarget { CastingTarget::AllyOnlySelf }

    fn effect(&self) -> Option<ModEffect> {
        Some(ModEffect {
            range: 0,
            growth_range: 0,
            start_timing: 4,
            casting: CastingType::None,
            target: CastingTarget::AllyOnlySelf,
            attack_type: AttackType::Skill,
            effect_type: Box::new(BloodBuff),
        })
    }
}

// ----------------------------------------------------------------------------
// Effect types (run when an action fires; receive &mut GameCtx)
// ----------------------------------------------------------------------------

/// Deals physical damage = caster.attack * attack_ratio%.
#[derive(Debug)]
struct WeaponHit {
    attack_ratio: usize,
}

impl ModEffectType for WeaponHit {
    fn apply(&self, ctx: &mut GameCtx, _rng_seed: u64, caster_id: usize, input: InputTarget) {
        let InputTarget::Target { target_id } = input else { return; };
        let ad = ctx
            .get_entity(caster_id)
            .map(|c| c.stat().attack * self.attack_ratio / 100)
            .unwrap_or(0);
        ctx.deal_damage(caster_id, target_id, ad, 0, AttackType::Skill);
    }

    fn expected_damage(&self, caster_stat: &EntityStat) -> (usize, usize) {
        let dmg = caster_stat.attack * self.attack_ratio / 100;
        (dmg, dmg)
    }
}

/// Self attack-speed + lifesteal buff.
#[derive(Debug)]
struct BloodBuff;

impl ModEffectType for BloodBuff {
    fn apply(&self, ctx: &mut GameCtx, _rng_seed: u64, caster_id: usize, _input: InputTarget) {
        ctx.add_buff(
            caster_id,
            BuffState {
                duration: BuffType::Time { tick: 240 },
                attack_speed_mult: 40,
                vamp: 25,
                ..Default::default()
            },
        );
    }

    fn on_caster(&self) -> bool { true }
}

// ----------------------------------------------------------------------------
// Passive: Execute + Bloodlust snowball
// ----------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct ExecutePassive;

impl ModPassive for ExecutePassive {
    fn clone_box(&self) -> Box<dyn ModPassive> { Box::new(self.clone()) }

    /// Execute: low-HP targets take bonus damage on basic attack.
    fn on_attack(
        &mut self,
        ctx: &mut GameCtx,
        _player: usize,
        _entity: usize,
        target: usize,
        damage: &mut usize,
    ) {
        if let Some(t) = ctx.get_entity(target) {
            let hp = t.hp();
            if hp.max > 0 && hp.current * 100 / hp.max < EXECUTE_THRESHOLD_PCT {
                *damage += hp.max * EXECUTE_BONUS_MAXHP_PCT / 100;
            }
        }
    }

    /// Bloodlust: on takedown, gain a short move/attack-speed snowball.
    fn on_kill(&mut self, ctx: &mut GameCtx, _player: usize, entity: usize) {
        ctx.add_buff(
            entity,
            BuffState {
                duration: BuffType::Time { tick: SNOWBALL_TICKS },
                move_speed_mult: 25,
                attack_speed_mult: 30,
                ..Default::default()
            },
        );
    }
}
