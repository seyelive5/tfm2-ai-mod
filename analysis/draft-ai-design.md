# TFM2 Draft AI — Design (full, no feature cuts)

Goal: a `ModDraftScoreHook` mod that makes the base ban/pick AI draft *as well as possible*,
considering **everything** the hook can see, scored by (a) real match statistics and (b) pro-MOBA
draft theory for the structural factors statistics can't capture. No difficulty dial — the AI just
drafts well, always.

## What the hook gives us (verified, API 0.7 / 0.4.6)
`score_pick(&self, ctx, candidate: usize, base_score: f32) -> DraftScoreDecision`
(same for `score_ban`). `DraftScoreContext`:
- `phase: DraftScorePhase::{Ban,Pick}`
- `available_champions: &[usize]`, `ally_ban`, `enemy_ban`, `ally_pick`, `enemy_pick: &[usize]`
- `is_explore: bool`, `difficulty: Difficulty`
- `candidate` = champion index into the game's champion list. **Assumed = banpick-data.js order**
  (0=vampire … 59=ogre); verify in-game once by logging candidate→name.

Decision: `Add(delta)` adds to the game's own score; we never `Replace` (keep base knowledge,
layer ours on top). Final = `base_score + SCALE * total_delta`.

## Champion identity & data
- 60 champions, recorded as NAME strings. Index basis = CHAMPION_ORDER (banpick-data.js).
- Stats from save_probe (837 matches): per-champ winrate, per-position winrate, synergy pairs,
  counter pairs — all shrunk toward 0.5 by sample (see draft-weights-gen). Stored as `delta` in
  roughly [-0.5, 0.5] (winrate - 0.5, shrunk).

## Taxonomy (from banpick-data.js — real tags, not invented)
- category: Melee(18) / Magician(12) / Util(12) / Assassin(9) / Range(9)
- damage type (rawTags): AD(38) / AP(23)  — every champ is exactly one (1 is both)
- structural tags: Tank(14), Frontline(22), CC(39), Heal(14), Shield(15), Backline(9),
  Mobility(33), Poke(24), AOE(43), DOT(11), Summon(11), Range(12)

## The score (everything considered)

`total_delta(cand)` = sum of these terms. Statistics terms are in winrate-units already
(comparable); structural terms are theory-based and tuned to the same scale.

### Statistical terms (data decides magnitude)
1. **Champion power** `champ_delta[cand]` — standalone winrate (primary-position basis).
2. **Ally synergy** `+ W_SYN * Σ synergy[cand][a] for a in ally_pick`
   - Pro theory: "synergy beats individual comfort" → W_SYN ≥ 1.0.
3. **Counters enemy** `+ W_CNT * Σ counter[cand][e] for e in enemy_pick`
   - Pro theory: counterpicking disrupts enemy. Weight ramps with pick order (see dynamic below).
4. **Countered by enemy** `- W_WEAK * Σ counter[e][cand] for e in enemy_pick`
   - Avoid picking into a hard matchup the enemy already locked.

### Structural terms (pro theory decides; stats can't see these)
5. **Team-comp needs** `+ W_COMP * comp_need(cand, ally_pick)`
   - Compute current ally comp's gaps, reward a candidate that fills them:
     - no Tank/Frontline yet → +full if cand is Tank, +half if Frontline
     - no CC yet → + if cand has CC
     - no Heal/Shield (peel) yet → + if cand provides it
     - no Backline/Range carry yet → + if cand is one
     - damage-type balance: if ally is all-AD (or all-AP), reward the opposite type
       (enemy can itemize one defense against a mono-damage comp)
   - Diminishing: each need only rewarded until satisfied (first tank matters, 3rd doesn't).
6. **Role / position overlap** `- W_DUP * overlap(cand, ally_pick)`
   - Penalize stacking the same category/position (e.g. 3 Assassins, or two of the same
     primary position) — they compete for the same lane/role.

### Dynamic weighting (pick-order theory)
Pro rule: *blind-pick strong champs early, save counters for last picks.*
Let `picks_made = ally_pick.len()` (0..=4 over the draft).
- Early (picks_made small): emphasize champion power + comp foundation; counter weight LOW
  (you don't yet know enemy lanes; committing a counter early wastes it / can be countered).
- Late (picks_made large): counter weight HIGH (you know enemy comp, last pick should answer it);
  comp-need weight HIGH (fill the last gap); champion-power weight lower (flex is moot).
- Implement as multipliers on terms 3,4 (rising) and term 1 (falling) by picks_made.

## Ban scoring (`score_ban`) — also full theory
`ban_delta(cand)` rewards banning a champion that is:
7. **Globally strong** `+ champ_delta[cand]` (OP / high winrate — the classic "ban the OP").
8. **Strong INTO us** `+ W_BAN_CNT * Σ counter[cand][a] for a in ally_pick`
   - they would counter what we've already committed → deny it.
9. **Synergy enabler for enemy** `+ W_BAN_SYN * Σ synergy[cand][e] for e in enemy_pick`
   - completes a strong enemy duo → deny it.
10. **High ban-rate in data** `+ W_BAN_META * banrate[cand]` (champion_patch_statistics bans).
Skip candidates already picked/banned (hook usually filters, but guard anyway).

## Determinism / safety
- Pure function of ctx + static tables. No RNG (respect `is_explore` only by NOT fighting it —
  if `is_explore`, optionally reduce SCALE so the game can explore alternatives).
- If candidate index→name lookup fails (out of range / unknown), return `Pass`.
- Hook runs inside draft; keep it O(picks) per call. Tables are HashMaps loaded once at init.

## Tuning plan (in-game)
- SCALE and the W_* are unknown vs base_score magnitude → log base_score range first call,
  then set SCALE so our max term (~0.5 * W) is comparable to base spread. Start SCALE so that a
  +0.1 winrate edge meaningfully shifts ranking but doesn't always override base.
- Defaults to start: W_SYN=1.2, W_CNT=1.0(×order ramp), W_WEAK=1.0(×order ramp), W_COMP=0.6,
  W_DUP=0.4, W_BAN_CNT=1.0, W_BAN_SYN=0.8, W_BAN_META=0.5. Adjust after watching real drafts.

## Data plumbing
draft-weights-gen emits a runtime file the mod reads at init. Extend it to also emit:
- per-champ: delta, primaryPos, **tags[], category, damageType** (from banpick-data.js)
- per-champ: **banrate** (champion_patch_statistics bans / total)
- synergy[a][b].delta, counter[a][b].delta  (already present)
Format: keep the flat TSV (mod parses without a JSON lib). Add row kinds:
  `T <champ> <category> <damageType> <tag1,tag2,...>`  (taxonomy)
  `B <champ> <banrate>`                                (ban rate)
Ship the generated file inside the mod package (e.g. `data/draft_weights.tsv`) read via
`include_str!` or from the mod dir at init.
