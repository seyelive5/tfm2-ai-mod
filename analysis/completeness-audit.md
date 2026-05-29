# Champion Creator — Completeness Audit (what's covered vs missing)

Goal: make sure the creator sets every champion aspect a **mod data_champion** can, with realistic values.
Done 2026-05-30 after extracting real base data from `asset/base/setting/champion_info` (60 champions).

## Critical distinction
- **Base champions** (champion_info) are **native/hardcoded**: their skills use a huge bespoke param set
  (`airborne_decay`, `bleed_ratio`, `kill_extend_count`, `ghoul_duration`, `execute_threshold`, ... ~300 fields).
  These are NOT the mod schema and can't be 1:1 reproduced by a data mod.
- **Mod data champions** use the `DataActionDef` + `DataEffectDef` schema (effects.md catalogue). That is the
  boundary we work within. We DON'T try to emit native fields — we compose from data effects.
- BUT base champion **stat/growth** ARE the same `EntityStat` format → we use them as real reference. ✓

## Coverage status

| Champion aspect | data_champion can set? | Creator exposes? | Notes |
|---|---|---|---|
| `id`, name (ko/en) | yes | ✅ | i18n via override-replace |
| `category` | yes | ✅ (from role) | |
| `tags` | yes (AD/AP/CC/Dot/Heal/Magic/Range/Shield/Tank/Melee) | ⚠️ **from role preset only** | base champs use varied tags; user can't pick → affects UI/draft-AI synergy. **MISSING: tag multi-select** |
| `stat` (Lv.1, 9 fields) | yes | ✅ editable, **real per-category avg seed + range shown** | |
| `growth` (per level) | yes | ✅ editable, **real per-category avg seed + range shown** | added 2026-05-30 |
| `sprite` + `anim_prefix` | yes | ✅ select/random (base sprites) | ⚠️ upload / preview not yet; anim_prefix fixed "" |
| `skill_icon`(s) | yes | ✅ select/random (base sheet) | ⚠️ upload / preview not yet |
| `attack`/`skill`/`skill2`/`ult` actions | yes (DataActionDef) | ✅ via templates + params | bounded by template set |
| └ action `cooltime` | yes | ✅ per-skill param | base range ~120–3600 ticks |
| └ action `range` | yes | ✅ per-skill param | base range ~27k–960k |
| └ action `duration`,`start_timing`,`cancelable`,`can_use_with_move` | yes | ❌ **fixed in template** | timing/feel knobs — **MISSING** |
| └ effect params (damage/ratio/radius/stun/buff fields) | yes | ✅ per-template params | partial; each template exposes its main knobs |
| `view_effects`/`view_projectiles`/`view_buffs` | yes | ⚠️ minimal (arrow sprite for projectiles) | custom effect visuals/recolor not yet |
| passive | N/A for data champions | — | base champs have no data passive; native-only |

## Skill template set (current) vs space
Exposed templates: `skillshot_ap/ad, self_buff, dash, heal, aoe_stun`. The DataEffectDef catalogue is much
larger (Stun/Airborne/Knockback/Grab/Pull/Fear/Charm/Bind/Taunt/Block*, Rush/Teleport/MoveTo/RushMoveToBack,
many projectile shapes, RangePeriod/Parabolic, AddCasted DoT, Shield, Combine/Delayed/SwitchByBuff/RandomTarget...).
**MISSING: more templates** (shield, dot/poison, knockback/grab/pull, multi-projectile, dash+hit combos, channel).

## Real reference numbers (for sane ranges / UI hints)
- Stat Lv.1 ranges (all base): attack 80–120, magic_power 0–40, hp 900–1400, defence 20–40, MR 15–30, move_speed 900–1200.
- Per-category stat averages: Assassin atk116; Magician atk82 ap34; Melee atk95 hp1014 def33; Range atk100; Util atk82 ap22.
- Skill params (base): cooltime 120–3600 (avg ~1289), attack_ratio 5–200 (avg ~76), range 27k–960k, stun ≤60.
  (Most base champ cooltimes are far higher than our template defaults of 200–900 — our skills are currently
  lower-cooldown / stronger uptime than base. Consider raising default cooltimes toward base norms.)

## Prioritized "missed parts" backlog
1. **Tag multi-select** (cheap, real impact on classification + draft AI synergy/counter). 
2. **Default cooltimes vs base** — our defaults look low vs base (120–3600); rebalance template defaults + show range.
3. **Action timing knobs** (duration/start_timing/cancelable/can_use_with_move) — optional advanced section.
4. **More skill templates** (shield, dot, hard-CC, dash-strike, multishot) — expands creativity.
5. **Sprite/icon preview + upload** (visual, higher effort: bundle sheet extraction).
6. **Effect visuals / recolor**, **pixel-art canvas** (Phase 3).
7. **Balance budget** (point-buy so totals stay in a sane band vs base power).
