# Teamfight Manager 2 — Built-in AI Architecture (reverse-engineered from TeamfightManager2.exe v0.4.6)

Method: string/symbol/panic-path mining of the Rust release binary (no disassembly). Identifier
*names* are strong evidence (leaked via `#[derive(Debug)]`/serde and `struct X with N elements`
markers + module paths from panic locations). Numeric constants (hp%, ranges, ticks, learning
rates, weights) are compiled literals and are **not** recoverable from strings — those are marked.

Source corpus: `analysis/strings_ascii.txt` (446k lines), `analysis/strings_utf16.txt`, `analysis/src_map.txt` (939 game source files).

## 0. Two pillars

| Pillar | What | Learned? | Where | Mod-exposed? |
|---|---|---|---|---|
| **A. In-match behavior** | how each champion moves/fights/objectives | **NO** — hand-authored hierarchical planner | `game-core\src\simulation\state\player\aos_ver_hamster\` | ❌ internal (only `ModPlayerInputAi` final-Input replace; no reads) |
| **B. Meta decisions** | draft/ban, team strategy, item builds, scouting/valuation | **YES** — online logistic/SGD models, pre-seeded + learn during campaign, saved | `game-core\src\banpick\`, `…\simulation\item_network\`, `…\data\training_ai.rs` | ⚠️ partially — via seed-asset data packs + `ModDraftScoreHook` |

The "AI" players often think of (in-match micro/macro play) is **Pillar A and is NOT a neural net or learned** — it is a deterministic, versioned, hand-tuned planner. Pillar B (draft/item/scouting) is the learned part.

---

## A. In-match behavior planner ("aos_ver_hamster")

Codename `aos_ver_hamster` (AOS = Aeon-of-Strife / MOBA). Runtime decision stack (confirmed by the
debug log format `!no action candidates, team_plan: …, plan: …, sub_plan: …`):

```
TeamPlan  →  Plan  →  SubPlan  →  Operation  →  SmallAction  →  AbstractInput  →  engine Input
(team coord)  (role)   (task)      (request)    (atomic)        (Positioning/Around/Skill…)
```

Two code generations coexist (hybrid at runtime, bridged by a `CraftTransition` message):
- **`plan_legacy\`** — original rich trees + flat 16-variant SubPlan. *Superseded but still compiled/used for lane/jungle/nexus/steal.*
- **`craft\` + `plan_v2\`** — active refactor: `recognition.rs` (perception) + `operation.rs` (execution) + a slim 5-variant SubPlan (`EpicHunt/EpicPoke/EpicCheck/LineSafe/Recall`). *Active for objective + safety + recall micro.*

### A.1 SubPlan vocabulary (the tasks the AI picks among)
Legacy flat enum (16): `LineDefense, LineAttack, LineSafe, LineTotal, LineWait, Recall, Jungle,
EpicCheck, EpicHunt, EpicPoke, SerpenCheck, SerpenHunt, SerpenPoke, AttackNexus, DefenseNexus, Steal`.
Legacy "old" Plan trees: `PassiveLine, PassiveJungle, LineGankCover, LineGanker, EpicHuntAndPoke,
EpicHuntAndBattle, SerpenHuntAndPoke, SerpenHuntAndBattle, ActiveRecall, AttackNexus, (Epic|Serpen)DefenseNexus`.
Objective naming: **serpen = dragon**, **morgard = baron/epic boss**, **epic = epic monster**.

### A.2 Operation vocabulary + team coordination (the "ping/call" protocol)
Operations: `battle.{top,mid,bottom}`, `battle.focus`, `battle.dive.focus`, `help.battle`,
`gank.{top,mid,bottom}`, `gank_to.*`, `line_cover.{top,mid,bottom}`, `lead`, `repair`, `split`,
`press` / `press_change`, `counter_jungle`, `attack_nexus`, `defense_nexus`.
Objective ops (mirror for serpen & morgard): `*.prepare/.setup/.check/.enemy_hunt/.assemble/.hunt/.battle/.give_up`, `*.steal.{lurk,commit}`.
Team-comm negotiation (v2): `propose.*`, `call.*`, `early_plan.{gank_top/mid/bot,counter_jungle,full_clear,switch_bot/top/farm,recall_replan}`,
`accept_after.{camp_clear,wave_push,recall}`, `decline.{hp_low,outnumbered,busy,too_far}`,
`counter.{push_lane,other_line,objective}`, `ready.{moving,pulling_wave,in_position,go,clearing_camp}`.
Craft hand-off ops: `craft.{epic_hunt,epic_poke,epic_check,serpen_hunt,serpen_poke,serpen_check,line_safe_top/mid/bot,recall,enemy_info}`.

### A.3 Battle tactics (per-target combat posture)
`Trace, Focus, Protect, Kiting, KitingBack, RunAway, Assassin, AssassinReady, End, SafeWait,
HardDisengage, SoftDisengage` (field `BattleSubPlan.tactic`).
Combat role/mode enum: `Standard, Frontline, Backline, DPS, SkillBurst, Peel, AllIn, Disengage`.

### A.4 SmallAction → AbstractInput (atomic outputs, per tick)
AbstractInput: `Positioning{x,y}` (move-to), `Around{target_id}` (follow/auto-attack a target),
`AroundPosition` (orbit/kite anchor), `Dodge` (reactive sidestep), `Skill`, `Skill2`, `Ult`.
Targeting kinds: `Targeting / Position / Direction / None`.
(These correspond to the mod API's `Input` enum — but the *selection logic* above it is internal.)

### A.5 Recognition signals (what `recognition.rs` perceives)
- **Lethality clocks:** `die_tick`, `die_tick_with_tower`, `me_die_tick`, `enemy_die_tick` (ticks-until-death estimate, ± tower help).
- **Burst:** `start_my_burst_damage`, `max_my_burst_damage` → `burst_risk`.
- **Counts/proximity:** `near_allies`, `near_enemies`, `can_near_enemies`, `near_wide_allies`, `in_camp_ally/enemy`, `near_(camp_)epic_ally/enemy`, `near_serpen_ally/enemy`, `near_projectile_count`, `top_count/mid_count/bottom_count`.
- **Tower dive:** `with_dive`, `is_in_tower_focused`, `is_in_tower_range`, `tower_focus`, `move_to_tower_tick`.
- **Risk/wave:** `score_risk`, `high_risk`, `risk_damage`, `risk_epic_damage`, `has_big_wave`, `has_enemy_minion_in_range`, `trajectory`.
- **Vision/MIA:** `VisionTrackingState{epic/serpen_giveup_tick, *_spawn_call_tick}`, `AllyRegionInfo{mia_call_ticks, last_visible_ticks, last_hp_ratio}`, `last_serpen_seen/hp`, `epic_stance/serpen_stance`.
- **Objective discipline:** `objective_misunderstanding`, `solo_hunt`, `opposite_group`.
- Versioned rule tags appear as log prefixes `v19..v27` (lane danger, tower cover, objective setup, morgard/serpen posture, backfight support, objective discipline).

### A.6 Abort / cancel conditions (when the AI bails)
`battle.stop.{tower_focused, tower_dive_fail, outnumbered, low_hp, burst_risk, low_hp_match}`;
`cancel.{low_hp_self, low_hp_ally, target_missing, plan_change, operation_abort}`;
objective `give_up.{stack_ahead, outnumbered}`.
Execution quality is modulated by athlete stats: `positioning, control_speed, judgement, concentration,
roaming, aggressive, ego, skill_avoid, skill_hit, last_hit, mental, order`.

---

## B. Learned meta-models (Pillar B)

All four ship a pre-trained baseline (seed assets `asset/base/setting/base_network`,
`base_item_network`) and **continue learning online during a campaign, persisted in the save**
(`Database.{networks, base_network, item_network, knowledge_bases, rating_system, meta_top_history, adapter_states}`).
All are **linear** logistic/SGD models (no neural-net symbols found).

### B.1 Draft/ban model — `FactoredBanpickAgent` / `SgdV2RecentHorizon`
Factored win/total + sum/count tallies → mini-SGD + meta logistic layer (`mini_sgd_weights`,
`meta_weights`, `meta_bias`, `meta_update_count`). 59-field feature set incl.:
`global/pos/synergy/counter/lane/triple/late_game_wins+total`, `lane_exp`, `matchup_exp`, `gank`,
`ganked`, `lane_gold`, `matchup_gold`, `bot_pair_gold`, `bot_matchup_gold`, and plan-coupled features
`plan_meta_*`, `team_plan_*`, `side_plan_meta_*`, `side_team_plan_*`. Config: `draft_plan_mode`,
`draft_plan_preferences`, `recent_horizon`, `recent_horizon_scoring_enabled`, `evidence_aging_enabled`.
V2 adds opponent modeling: `opponent_open_(pick/not_pick)_count/wins`, `own_ban_count/wins`.

### B.2 Strategy selection — `strategy_sgd` (`LogisticSGDStrategyAgent`)
Bare weight-vector logistic regression choosing the macro `Strategy` preset:
`focused, early_jungle, early_serpen[_top], object_buildup, object_battle, morgard_use, tower_press,
morgard_defense, object_finish, minion_wave, game_finish` (with sub-options Gather/Split14/Split131,
Poking/Initiating, KillPriority/BattlePriority/WavePriority/JoinPriority, Stable/Flexible/Aggressive…).

### B.3 Opponent adaptation — `V3TargetResponseLayer` (being deprecated → `_legacy_v3_response_layer`) + `TeamAdapterState`
Per-opponent `stable_*` vs `epoch_*` decaying counters (pick threat, ban response, opens picked/denied).

### B.4 Item-build model — `item_network` (`item_network\sgd.rs`)
Linear SGD with a `RegressionCoeffs{alpha,beta}` sigmoid calibrator. Inputs (inferred): champion +
enemy state matched against `ItemTag` categories (AD/AP/AS/Defense/MR/HP/DefensePenetration/Vamp/
HealReduce/ShieldBreak/MoveSpeed/AttackRange/HpPercentDamage/…). Players can force `ItemBuildOverride`.

### B.5 Management AI — `training_ai.rs` (mostly rule-based) + `KnowledgeBase` (learned valuations)
Categorical strategy selectors (`EarlyJungleStrategy, TowerPressStrategy, ObjectBuildupStrategy,
ObjectBattleStrategy, ObjectFinishStrategy, MinionWaveStrategy, GameFinishStrategy, FocusedAreaStrategy,
TeamRisk(Safe/Bold), FuturePlan(WinNow/FuturePlan), …`). `KnowledgeBase` accumulates scouting knowledge
(`base/region/team_knowledge`, `player_reports`).

---

## C. Moddability map — where new behavior CAN live

Cross-referenced with the verified mod API (API 0.7). See also memory `tfm2-modding-reference`.

| Target | Can a mod change it? | How |
|---|---|---|
| In-match planner (Plan/SubPlan/Operation/SmallAction/recognition) | ❌ **No** | internal, not exposed; no hook adds plans/operations/signals |
| Final per-tick Input | ⚠️ Very limited | `ModPlayerInputAi::think` → replace with run-away/recall/manual Input; **no enemy reads** |
| Per-champion reactive logic | ✅ **Yes** | `ModChampionInfo` + `ModPassive`/`ModEffectType` get `&GameCtx` (full battlefield reads + deal_damage/heal/buff/cc). Movement/auto-target/cast-timing still the planner's. |
| New champions/items/effects | ✅ Yes | data champions or native `ModChampionInfo`/`ModItemInfo` |
| Draft scoring | ✅ Yes (additive) | `ModDraftScoreHook::score_pick/score_ban` adds/replaces on top of `FactoredBanpickAgent` |
| Learned-model priors (draft/item) | ✅ Yes (data) | replace seed assets `asset/base/setting/{base_network,base_item_network}` in a data pack |
| Macro strategy presets / weights | ❌ effectively no | `strategy_sgd` weights internal; presets are engine enums |
| Item-build inputs | ⚠️ Indirect | modding items/tags reshapes what `item_network` sees |

### C.1 The real gaps a mod CAN fill ("behavior the base AI lacks")
The base planner does **macro/positional** play well but is generic; it has **no champion-specific
reactive ability AI** beyond casting-target/range heuristics. That is exactly the moddable seam:

- **Execute / threshold abilities:** passive reads enemy `hp()` in range → bonus/true damage when below X% (base AI has no per-ability execute logic).
- **Reactive protection:** `on_update` finds the lowest-HP / `burst_risk` ally in range → auto-shield/heal/cleanse (base AI only self-preserves via `cancel.low_hp_self`).
- **Conditional AoE CC:** detect `near_enemies >= N` clustered → auto-cast area CC (base AI won't "wait for" clustering on a custom kit).
- **State-machine kits:** `SwitchByBuff`/stack logic to change effect behavior by combat state.
- **Draft identity:** `ModDraftScoreHook` to make the AI value a playstyle/archetype the base model underrates.

> Hard limit: you are adding **reactive ability behavior on top of the base navigation**, not rewriting
> the planner. Movement, target selection, lane assignment, objective timing remain Pillar A (internal).

---

## D. Follow-up anchors (for deeper digs)
- Active decision flow lives in `…\aos_ver_hamster\plan\team_plan.rs`, `plan_v2\mod.rs`, `craft\operation.rs`, `craft\recognition.rs`, `score_parameter.rs` — numeric thresholds/weights require disassembly (not in strings).
- String index anchors: ops/coordination `strings_ascii.txt:421859-421876`; Strategy enum `392527`; legacy chat + cancel/give-up `424833-424855`; craft SubPlan `427229`; legacy SubPlan/old Plans `432187-432226`; AbstractInput/tactics `432243-432251`; SGD feature struct `425435-425529`.
