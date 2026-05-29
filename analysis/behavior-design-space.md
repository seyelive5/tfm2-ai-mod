# Behavior Design Space — what an in-match mod can actually do (compiler-verified, API 0.7 / 0.4.6)

Verified by probing the SDK rlib with the compiler (not docs alone). This defines the hard
capability boundary for any "new behavior" we implement via a custom champion / passive / effect.

## Verified capability boundary

**WHERE mod code runs in-match (all receive `&mut GameCtx`):**
- `ModPassive`: `on_spawn, on_attack(…, damage: &mut usize), on_damaged, on_kill, on_assist, on_dead, on_base_attack, on_update` (per tick).
- `ModEffectType::apply(ctx, rng_seed, caster_id, input)` — fires when the planner casts the action.
- `ModItemInfo`: `on_attack(…, damage: &mut usize, damage_type), on_skill_hit, on_healed, on_damaged, on_kill, on_spawn, update`.

**READS available (the whole battlefield):**
- `GameCtx`: tick, seed, score_diff, is_end, entity_count/entity_at/get_entity, player_count/player_at/get_player, champion_count/id_at, tower_count/id_at, projectile_count/at, kill_log_count/at, distance_sq, is_visible.
- `EntityRef`: id, stat (attack/magic_power/hp/defence/magic_resistance/**move_speed**/hp_regen/stack/crit_chance), pos, hp(current/max), team, level, is_alive/champion/tower/minion, shield, buff_count/buff_at, cc_count/cc_at, radius, is_targetable.
- `PlayerRef`: champion, level, gold, position (lane role), team, is_alive, respawn_time, kills, deaths, assists, cs.

**MUTATIONS available — ONLY these four (+ debug draw):**
`deal_damage(attacker,target,ad,ap,type)`, `heal(caster,target,amount)`, `add_buff(target, BuffState)`, `apply_cc(target, CCState)`. Plus `on_attack`/item `damage: &mut usize` you can rewrite.

**VERIFIED ABSENT (do NOT design around these):** spawn/summon/create_projectile, set_input/issue_input/command/order/move_to/move_entity (no way to tell a champion where to go or whom to attack), teleport/dash/blink/knockback helpers (displacement only via `apply_cc(ForceMove{…})`), get/set/reduce cooldown, remove_buff/clear_buff/dispel/cleanse (buffs are **add-only**, can't be removed by us), kill/execute (only via deal_damage), add_gold, reveal/vision, set_position, recall. EntityRef has NO current_action/target/is_casting/is_moving/attack_range/state (can't read what an entity is *doing* — only its stats/hp/buffs/cc/pos). PlayerRef has NO plan/sub_plan/operation/target (can't read the base AI's intent).

**Effect toolbox** (what `add_buff`/`apply_cc`/data-effects give): BuffState ~38 stat/status fields (move_speed_mult, attack_speed_mult, damaged_amplify/reduce, defence/MR penetration, vamp, heal_reduce, cc_immune, undying, crit_chance, dot_amplify, base_attack_enemy_max_hp_damage, …). CCState: Airborne, Stun, Bind, BlockAttack, BlockSkill, BlockMoveSkill, ForceMove{dx,dy,speed}, Taunt{target}, Fear, Charm, Animation. (Shield is grantable via the data `Shield` effect type, not via add_buff.)

## Consequence
We can build **reactive ability behavior** that reads the full battlefield and applies damage/heal/
buff/cc to any entity, per tick or on events — but we **cannot** drive movement/targeting/cooldowns/
summons or read the base AI's plan or an entity's current action. Positioning & navigation stay 100%
with the base planner. So "new behavior" = autonomous *ability/effect* decisions layered on top.

## Behavior catalogue (feasibility-rated, each tied to a base-AI gap)

Legend: ✅ fully feasible · 🟡 feasible but constrained · ❌ not possible.

### ✅ Damage-shaping (mechanism: on_attack `&mut damage`, deal_damage, BuffState)
1. **Execute** — `on_attack`: read `target.hp().current/max`; if below T%, multiply damage or `deal_damage` bonus (optionally `Fixed`/`%max-hp`). *Gap: base AI has no per-ability execute targeting.* 
2. **Anti-tank / %max-HP on-hit** — scale bonus by `target.max_hp`. *Gap: generic AI ignores enemy HP pools.*
3. **Ramping damage** — `on_attack` increments a self `stack` buff; damage scales with stacks. 🟡 (buffs add-only, so use a decaying timed buff or track via repeated short buffs).
4. **Conditional crit/penetration** — add `crit_chance`/`defence_penetration` buff when a condition (low HP target, score behind) holds.

### ✅ Reactive support (mechanism: on_update scan + heal / protective buff)
5. **Triage heal/shield** — `on_update`: iterate allies (`entity_at`+`team`), pick lowest `hp_ratio`; if below T% `heal` or grant `damaged_reduce`/`undying` buff. *Gap: base AI only self-preserves (`cancel.low_hp_self`), never proactively peels for a teammate's HP.*
6. **Burst-guard** — track an ally's HP delta across ticks (store prev HP in the passive struct); if a big single-tick drop → emergency `damaged_reduce`/`cc_immune` buff. *Gap: base AI's `burst_risk` only protects self.*
7. **Anti-CC bubble** — if ally has a hard CC in `cc_at`, grant nearby allies short `cc_immune`. 🟡 (can't remove the existing CC — add-only — but can pre-empt spread).

### ✅ Combat-state self/aura behavior (mechanism: on_update + distance_sq + buffs)
8. **Outnumbered berserk** — count enemies within range via `distance_sq`; if `near_enemies >= N` grant self attack/attack_speed/vamp (or `damaged_reduce`). *Gap: generic AI doesn't build state-specific power spikes for a custom kit.*
9. **Lane-bully / scaling aura** — buff nearby allies based on tick (game time) or `score_diff`.
10. **Positional reward** — buff when near a tower/ally cluster (read tower/ally positions).

### ✅ Event-triggered behavior (mechanism: on_kill/on_assist/on_dead/on_damaged)
11. **Snowball on takedown** — `on_kill`/`on_assist`: self move_speed+attack buff. *Gap: base AI has no kit-specific snowball.*
12. **Retaliation** — `on_damaged` by a champion: `apply_cc`/`deal_damage` back to attacker (thorns/stun-on-hit). 
13. **Martyr** — `on_dead`: AoE buff allies / debuff enemies near death spot.

### ✅ Custom skill effects with smart targeting (mechanism: ModEffectType::apply + full reads)
14. **Multi-target / smart-target skills** — when the skill fires, *we* pick targets via GameCtx (e.g., all enemies in radius, or the lowest-HP enemy, or the most-clustered point) and apply effects — richer than the engine's single-target casting. *Gap: base casting is target/range heuristic; effect-internal target logic is ours.*
15. **Conditional AoE CC** — in `apply`, only commit CC if `>=N` enemies clustered (count via distance_sq); else minimal effect. *Gap: base AI won't "hold" a cast for clustering on a custom kit.*
16. **Displacement plays** — `apply_cc(ForceMove/Airborne/Taunt/Fear)` to reposition enemies (peel, engage, scatter). 🟡 (aim dx/dy from pos math).

### 🟡 Indirect influence on base AI
17. **Target marking** — debuff a chosen enemy with `damaged_amplify`; the base AI's `EnemyChampionRecentlyAttacked` casting heuristic *may* synergize so allies focus it. 🟡 (indirect, unverified coupling).

### ❌ Out of scope (verified impossible via mod API)
New plans/operations/pathing; telling a champion where to move or whom to attack; vision/ward behavior;
recall-timing rewrite (beyond `ModPlayerInputAi`'s limited self-retreat/recall replace); reading enemy
cast/intent; cooldown manipulation; cleanses; summons.

## Design notes / gotchas
- Buffs are **add-only & timed** — no removal. Model "modes" as short re-applied timed buffs, or use
  `BuffType::Time{tick}` and re-up each tick in `on_update`.
- No per-entity attack-range read (`EntityStat` has no range) — use `radius` + a constant, or `distance_sq` thresholds.
- `on_update` runs every tick for every entity with the passive → keep it O(entities) and cheap; it runs inside the sim.
- Determinism: use the provided `rng_seed`, not wall-clock RNG, so multiplayer/replays stay in sync.
- Most behaviors attach to a **custom champion's passive**; to affect *existing* champions you'd need them to carry your passive (custom champ) — you can't inject a passive into base champions.
