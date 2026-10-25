# CLAUDE.md — TFM2 Mod Project (DEFINITIVE REFERENCE)

Project context for *Teamfight Manager 2* (TFM2) modding + the shipped draft-recommender product.
This file is the **verified ground truth**. Where something is proven, it says **PROVEN**; where it is
inferred or not run in-game, it says **UNTESTED**. Do not "re-discover" sealed paths that are marked
IMPOSSIBLE-PROVEN below, and do not downgrade UNTESTED items to IMPOSSIBLE without running the experiment.

> ⚠️ **2026-10-26:** game is now **0.6+**, where the vendor stopped shipping/supporting the **classic** SDK this
> whole document is built on — every mod here is classic and no longer loads; the path forward is the **stable API**
> (`mod-sdk-stable/` in the game folder; see upstream `TeamfightManager2Mod/docs/stable-*.md`). Treat classic-only
> facts below as history. The champion creator (`champion-gen`, `creator-app`, `tfm2_champion_creator`,
> `tfm2_combat_ai` + their analysis docs) was split out to `github.com/seyelive5/tfm2-champion-creator`
> (local: `C:\tmp\tfm2-champion-creator`).
>
> ★ Read the **CAPABILITY VERDICTS** table (§4) before designing anything. The single most important
> correction in this revision: **live banpick auto-read is UNTESTED-NEEDS-EXPERIMENT, NOT impossible.**
> The prior "IMPOSSIBLE-PROVEN" verdict conflated the tick hooks with the command path; the
> `send_mod_command → handle_command → ctx.server_state.running_matches[*].set_info` route is
> architecturally confirmed end-to-end in the binary and has no proven seal. One ~60-line probe settles it.

---

## 0. TL;DR / fixed facts

- **Engine:** custom **Rust** engine (panic strings, `struct … with N elements`, glutin/egui/OpenGL). Not Unity/Godot.
- **Game version: PER-PATCH — do not hardcode.** Current: **0.4.14** (`C:\tmp\mod-sdk`, base_version.txt, 2026-07-04).
  The repo's gitignored `sdk\mod-sdk\` may lag. `mod_api` `API_VERSION == (0, 7)` as of the 0.4.6 SDK — not re-verified on 0.4.14.
- **Toolchain (ABI-critical, PER-SDK — do not hardcode a nightly):** mods are `cdylib`s statically linked against prebuilt
  `.rlib`s and Rust has no stable rlib ABI, so the nightly MUST match the SDK exactly (mismatch → E0514, or *silently* broken
  cross-DLL `Any` downcasts — see the MatchUIRunner raw-cast in §4). The exact nightly is **DERIVED from the SDK's own
  `<SDK>\toolchain_version.txt`** (`rustc … (HASH DATE)` → channel = **DATE + 1**, cross-checked by the SDK's `rust-toolchain.toml`);
  **`build_mod.ps1` derives + applies it automatically** — never hand-pin. Current `C:\tmp\mod-sdk` (0.4.14): **`nightly-2026-05-24`
  (rustc `23a3312d9`)**; the older 0.4.6-era SDK was `nightly-2026-05-29` (`cced03bfd`). **MSVC C++ build tools + Windows SDK
  (link.exe) REQUIRED** — target is `x86_64-pc-windows-msvc`.
- **Two mod kinds:** (1) **Database Pack** = JSON/data only (`.tfm2db`); (2) **Code Mod** = native Rust `cdylib`
  built against the injected `mod_api` crate. This project builds code mods + data/asset content.
- **Entry symbols:** `declare_mod!(init)` exports `tfm2_mod_entry` + `tfm2_mod_api_version`. `fn init(&GameCtx) -> ModRegistration`.
- **Naming rule (critical):** mod folder name == `ModRegistration::new("…")` id == `mod.mod_info` id == produced `.dll` name. All four must match or the DLL won't load.

---

## 1. Project overview + repo / product layout

Two distinct deliverables live here: a **manual draft-recommender overlay** (the shipped player-facing product,
which is NOT a mod) and a set of **TFM2 code-mod experiments**.

### 1a. Dev workspace `c:\tmp\tfm2-ai-mod\` (git repo)
```
CLAUDE.md                  # THIS file — the definitive reference
README.md
analysis\                  # writeups + extracted data (see §6 cross-links)
sdk\mod-sdk\               # (gitignored) extracted TFM2 Mod SDK 0.4.6 (deps/ native/ template/ build scripts)

# --- code-mod experiments (folder name == mod id) ---
# (tfm2_combat_ai\ + tfm2_champion_creator\ moved to the tfm2-champion-creator repo, 2026-10-26)
tfm2_draft_ai\             # ModDraftScoreHook + IPC write_state. SUPERSEDED by overlay; still enabled, spams C:\tmp
tfm2_pool_reader\          # ModExtension that exports db.available_champions + champion_patch_statistics (live stats)
draftwatch\                # gameview Debug-dump probe (ModExtension)
pickcap\ , q\              # throwaway probes

# --- NON-mod tooling for the recommender product ---
draft-overlay\             # source of the hudhook overlay (draft_overlay.dll)
draft-weights-gen\         # generates draft_weights.tsv (10-factor scoring)
# (champion-gen\ + creator-app\ moved to the tfm2-champion-creator repo, 2026-10-26)
```

### 1b. Shipped product `c:\tmp\tfm2-overlay\` (the recommender — NOT a mod)
A **hudhook OpenGL overlay injected into the game process** (TFM2 is OpenGL: wgl*, no D3D/DXGI/Vulkan, no anti-cheat).
The recommender is an overlay because the mod API **cannot render text** (see §4b).
- `draft_overlay.dll` + `inject-overlay.exe` — the injected overlay + injector.
- **Manual** workflow: user clicks a 60-champ grid to mark picks/bans → top-12 ranked **in Korean** from `draft_weights.tsv`.
- `draft_weights.tsv` (10-factor weights), `banpick-data.js` (champion order, 0=vampire … 59=ogre),
  `tfm2_save_probe.exe`/`save_probe` → produces weights from save data, `refresh-stats.ps1`, `HOW_TO_USE.md`.
- **초성-button Korean-IME workaround is REQUIRED and stays** — the overlay can't drive the Korean IME, and nothing in the mod API offers an escape (mod API can't render the recommender text at all).

> Why manual and not auto: live-draft auto-read was the whole question. Current honest status = UNTESTED (see §4a).
> The overlay shipped manual because every *previously-tested* route was sealed; the un-run server-command route is the open door.

---

## 2. TFM2 MOD SDK REFERENCE — capability map by hook kind

`mod_api` is a **thin facade crate**. It owns: `ModRegistration`, the hook **traits**, `GameCtx`, service types,
and `declare_mod!`. The big runtime helpers are **inherent methods on re-exported game types** that physically live in
`libgame_view` (client) and `libgame_core` (server). Everything works via `use mod_api::*;`, but a DLL is hard-bound to
one game/SDK version because the real struct layouts come from the matching crates.

**Registration** (`ModRegistration`): `new(mod_id)`; `add_champion`/`add_item`/`add_draft_score_hook`/`add_player_input_ai`
accumulate into `Vec` (multiple allowed); `set_extension`/`set_server_extension` are `Option` (one each, last wins).

The four runtime contexts where mod code executes:

| Context | Hooks | Battlefield reads | Mutations |
|---|---|---|---|
| **`GameCtx`** (in-sim) | `ModPassive::on_*`, `ModEffectType::apply`, `ModItemInfo::*` | **FULL** (§2a) | `deal_damage`/`heal`/`add_buff`/`apply_cc` + `debug_draw_line/circle` ONLY |
| **`PlayerAiContext`** (in-sim final input) | `ModPlayerInputAi::think` | **NONE** beyond own identity/HP | returns `PlayerInputDecision::{Pass, Replace(Input)}` |
| **`DraftScoreContext`** (draft) | `ModDraftScoreHook::score_pick/score_ban` | draft board indices only | returns `DraftScoreDecision::{Pass, Add(f32), Replace(f32)}` |
| **`Scene`/`GameUI`/`ClientData`** (client loop) | `ModExtension::{on_init,pre_update,post_update,pre_render,post_render,on_end}` | huge read-only `ClientDatabase` | `db_mut`, mod-save writes, `send_mod_command`; **no UI draw** |
| **`ServerModContext`** (mgmt, authoritative) | `ModServerExtension::{on_server_start,before/after_management_tick,handle_command}` | `&mut Database`, `&mut ServerState` (raw internals) | `emit_event*`, save mutation |

### 2a. `GameCtx` — in-match simulation (the rich seam; docs accurate, PROVEN)
**Reads:** `tick`, `seed`, `score_diff(team)`, `is_end`, `get_entity(id)`/`entity_count`/`entity_at`,
`get_player(id)`/`player_count`/`player_at`, `champion_count`/`champion_id_at`, `tower_count`/`tower_id_at`,
`projectile_count`/`projectile_at`, `kill_log_count`/`kill_log_at`, `distance_sq(id,id)`, `is_visible(team,id)`.
- `EntityRef`: `id, stat()->EntityStat, pos()->{x,y:u64}, hp()->{current,max}, team, level, is_alive/is_champion/is_tower/is_minion, shield, buff_count/buff_at, cc_count/cc_at, radius, is_targetable`.
- `PlayerRef`: `champion()->Option<EntityRef>, level, gold, position()->Position, team, is_alive, respawn_time, kills, deaths, assists, cs`.
- `ProjectileRef::info()->{x,y,caster_id,team,is_end}`. `KillLogEntry{tick,killer_team,killer_position,killed_position,assist_count,assist_positions:[u32;4]}`. `CCInfo{cc_type:u32,tick}` (cc_type 0=Airborne…255=Invalid).

**Mutations — EXACTLY four (+ debug draw):** `deal_damage(attacker,target,ad,ap,AttackType)`, `heal(caster,target,amount)`,
`add_buff(target,BuffState)`, `apply_cc(target,CCState)`; `debug_draw_line/debug_draw_circle(...,color:u32)`.
**VERIFIED ABSENT (do NOT design around):** no spawn/summon/create_projectile, no set_input/move_to/order/target,
no teleport/dash helper (displacement only via `apply_cc(CCState::ForceMove{..})`), no cooldown get/set,
no buff removal (**buffs add-only**), no kill/execute (only via deal_damage). `EntityRef` cannot read what an entity is
*doing* (no current_action/target/is_casting/attack_range); `PlayerRef` cannot read AI plan/intent.

### 2b. Content authoring — champions / actions / effects / passives / items (PROVEN, docs accurate)
- **`ModChampionInfo`** (`add_champion`): `id`, `name`, `category()->ChampionCategory`, `tags()->Vec<ChampionTag>`,
  `stat()`/`growth()->EntityStat`, `attack()`/`skill()`/`skill2()->Box<dyn ModAction>` (required), `ult()`/`passive()` (Option).
- **`ModAction`**: `clone_box`, `action_name`, `duration`, `cancelable`, `cooltime(stat,level)`, `casting_target()->CastingTarget`, `effect()->Option<ModEffect>`, `can_use_with_move`, `description`.
- **`ModEffect`** (struct): `range, growth_range, start_timing, casting:CastingType, target:CastingTarget, attack_type:AttackType, effect_type:Box<dyn ModEffectType>`.
- **`ModEffectType`**: `apply(&mut GameCtx, rng_seed, caster_id, input:InputTarget)` does the work; the `expected_*` methods feed the AI evaluator.
- **`ModPassive`**: `on_spawn/on_attack(damage:&mut usize)/on_damaged/on_kill/on_update/on_base_attack/on_assist/on_dead`. `on_attack`'s `&mut usize` amplifies outgoing damage.
- **`ModItemInfo`** (`add_item`): `key, icon, price, tier, stat()->BuffState, next_tier/previous_tier:Vec<String>, tags, category` + runtime callbacks `on_attack/update/on_spawn/on_healed/on_damaged/on_kill/on_skill_hit`. `previous_tier` grafts into an existing build path.
- **`ModEffectBuff`** exists as a type but has **NO `ModRegistration` registration path** in (0,7) — constructible, not publicly wired. UNTESTED for mod authors.
- **Data-only champions** (`.data_champion` JSON, no Rust): required `id/attack/skill/skill2`; 60+ JSON effect types. **Gotcha:** `casting_target` defaults to `Ally` — set explicitly for damage. Engine has MORE effect impls than JSON exposes (summons, `AddEffectBuff`, `TowerAttack`, bespoke champ modules need native Rust). Loader source of truth: `game-core/src/setting/champion/data_driven.rs`.

### 2c. `ModDraftScoreHook` — draft AI scoring (PROVEN limits)
```rust
fn id(&self) -> &str;  fn priority(&self) -> i32;  // lower runs first
fn score_ban(&self, ctx: &DraftScoreContext, candidate: usize, base_score: f32) -> DraftScoreDecision;
fn score_pick(&self, ctx: &DraftScoreContext, candidate: usize, base_score: f32) -> DraftScoreDecision;
// DraftScoreDecision::{Pass, Add(f32), Replace(f32)}
```
`DraftScoreContext` has **EXACTLY 8 fields, NO match/team/side/turn id** (compiler-verified, exhaustive token scan):
`phase:DraftScorePhase{Ban,Pick}`, `available_champions:&[usize]`, `ally_ban`, `enemy_ban`, `ally_pick`, `enemy_pick`
(all `&[usize]`), `is_explore:bool`, `difficulty:Difficulty`. Called **once per candidate**.
- Good for **buffing the AI's draft**. **Useless as a player-draft source:** fires identically for thousands of
  concurrent AI-vs-AI league sims (7974 boards/153s observed), with no id to attribute a call to the human's match.
  `is_explore` does not identify the match. Manual human picks likely don't invoke the scorer at all.
- `candidate:usize` indexes the banpick-data.js champion order (0=vampire … 59=ogre) — runtime-confirmed via score_pick logs.

### 2d. `ModPlayerInputAi` — combat AI override (PROVEN; read-blind)
```rust
fn clone_box(&self)->Box<dyn ModPlayerInputAi>; fn id(&self)->&str; fn priority(&self)->i32;
fn matches(&self, ctx:&PlayerAiInitContext)->bool;            // attach-time gate
fn think(&mut self, ctx:&mut PlayerAiContext, base_input:Option<Input>)->PlayerInputDecision; // {Pass,Replace(Input)}
```
- `PlayerAiInitContext` (for `matches`): `player_id, athlete_id, team, position, champion_name`.
- `PlayerAiContext` (per-tick) exposes ONLY: identity (`player_id/athlete_id/team/position/champion_name`), own HP
  (`hp/max_hp/hp_ratio_percent/is_hp_below_percent`), `tick`, prebuilt intents
  (`get_run_away_input/get_run_away_without_skill_input/get_recall_input/is_safe_to_recall`), `is_valid_input`.
- **NO `game_ctx`, no entity/enemy/ally discovery, no positions/HP of others.** You can build any `Input`
  (Move/Attack/Skill/Skill2/Ult/Return with `InputTarget::{Target,Dir,Pos,None}`) but **cannot discover target ids** →
  manual targeting is infeasible from this hook in 0.4.6. Rich reads exist only via `&GameCtx` (custom champ/item).

### 2e. `ModExtension` — client loop / `ClientData` / `ClientDatabase` (PROVEN)
Hooks: `on_init/pre_update/post_update/pre_render/post_render/on_end`, each `(&mut Scene, &mut GameUI, &mut Assets[, &mut RenderState])`.
Gate on `let Scene::InGame { data } = scene else { return; };` → `data: &mut ClientData`. **Fires during banpick** (PROVEN: `pre_update` logged in-game via draftwatch/tfm2_pool_reader).
- `ClientData` lookups return borrowed `Option<Ref<'_,T>>` guards (drop before next mut borrow); or take `let db = data.db();`
  for `ClientDatabase` (returns direct refs). Helpers: `player_team_id/player_team/team/team_ids/athlete/staff/league/tournament/...`,
  matches by `MatchType` only (`match_info(MatchType)/normal_match/practice_match/tutorial_match/solo_rank_match_info`),
  `champion_info(name)->Option<Arc<dyn ChampionInfo>>`, messaging (`send_mod_command/mod_events/take_mod_events`), full `mod_save_*` family.
- **Scene variants (8):** `Loading, Title, NewGame, DatabaseEdit, InGame{data}, GameTest, Room, Lobby`.
- **`ClientDatabase` keys matches only by `MatchType` (4 variants: Tutorial/Normal/Practice/SoloRank) — one slot each, NO running-matches map.** Client `MatchInfo` has a coarse `running_state:MatchRunningState` but **NO `set_info`/banpick/picks/bans field.** The live draft board is genuinely absent client-side (IMPOSSIBLE-PROVEN, §4a).
- ★ **Mod-readable by value (clean, no sim noise — useful):** `db.available_champions: Vec<String>` and
  `db.champion_patch_statistics: HashMap<String, ChampionPatchStatistics>` → a mod CAN export live in-game winrate/banrate/pool. (tfm2_pool_reader ships this.)

### 2f. `ModServerExtension` — authoritative server (PROVEN structure; one runtime unknown)
`ServerModContext { mod_id:&str, database:&mut Database, server_state:&mut ServerState }`. Hooks: `on_server_start`,
`before/after_management_tick`, `handle_command(ctx, &ModServerCommand) -> ModServerCommandResult{Pass,Handled}`.
Event emitters: `emit_event`, `emit_event_to_player`, `emit_event_to_team`, `emit_event_to_command_sender` (all `-> bool`);
`player_team_id`, `team_player_ids`.
- ★ `ServerState` is **NOT importable by name** from `mod_api` (E0425) but its fields are reachable structurally:
  `ctx.server_state.running_matches : HashMap<usize, RunningMatchInfo>` (PROVEN public field, compiler-verified).
  - `RunningMatchInfo` (8 fields): `id, is_end, team1, team2, game_rule, set_info: Vec<MatchSetInfo>, …`.
  - `MatchSetInfo` (12 fields): `phase:BanpickPhase, game_rule, ban_count, is_team1_blue, team1_player, team2_player,
    team1_bans, team2_bans, team1_picks, team2_picks, team1_orders, team2_orders` — **the live draft board**.
  - `BanpickPhase = {Team1Pick, Team2Pick, Team1Ban, Team2Ban}`; `MatchRunningState = {Wait, Running, End{..}}`,
    `MatchSetPhase = {Swap, Strategy, SelectLineup, StadiumEntrance, End}`.
- **The catch (§4a):** `after_management_tick` is gated by the management tick, which is PAUSED during a manual banpick →
  the `running_matches` you see there (only after pressing proceed) are already-decided league sims, not your live draft.
  The **un-tested** door is `handle_command` on the server worker thread (event-driven, NOT tick-gated).

### 2g. Mod Commands / Events (PROVEN flow)
Client→Server = *command*; Server→Client = *event*, both namespaced by `mod_id`.
`ClientData::send_mod_command(mod_id, command, payload)->bool` → `ClientPacket::ModCommand`
(**binary-confirmed sibling of `ClientPacket::BanpickSelect` in one enum, one packet channel, routed on the server
worker thread by `game-view/src/logic/server/packet_handler.rs`**) → `ModServerExtension::handle_command` →
`ctx.emit_event_*` → `ClientDatabase.mod_events` → `ClientData::take_mod_events(mod_id)` (drain) / `mod_events` (peek).
- `ModServerCommand{mod_id, command, payload:Vec<u8>, sender_player_id:Option<PlayerId>, sender_team_id:Option<usize>}`.
- `ModClientEvent{mod_id, event, payload:Vec<u8>}`. Limits: `MAX_MOD_ID_LEN=128`, `MAX_NAME_LEN=128`, `MAX_PAYLOAD_LEN=1 MiB`. Invalid packets silently rejected → return `false`.

### 2h. Mod Save Data (PROVEN)
Client via `ClientData::mod_save_*` (in `Scene::InGame`); server-direct via `ctx.database.mod_save_data: ModSaveData`.
`get/set_bytes`, `get/set_string`, `keys`, `contains_key`, `remove_key`, `clear_namespace`, `save_version`/`set_version`,
`can_write_mod_save()`. Limits: id/key ≤128 B, value ≤1 MiB. **Multiplayer: only the host can write**; namespace version is your migration counter, independent of mod.mod_info version.

### 2i. Inter-mod Services (PROVEN; game owns the registry)
`GameCtx::register_service(service_id, ModServiceVersion, ModService)->bool` (call in `init` before returning),
`GameCtx::query_service(provider_mod_id, service_id, version_req)->Option<ModService>`. `ModService{data:*mut c_void, vtable:*const c_void}`.
**Rules:** `#[repr(C)]` vtables only; primitives/opaque handles/fn-pointers only; NO `String`/`Vec`/trait objects across the boundary; consumer must declare provider in `mod.mod_info` dependencies. `base` dependency = the game version line (special), not an asset.

### 2j. UI / Node / RenderState (PROVEN limits — see §4b for the verdict)
`mod_api` re-exports `Node, NodeTemplate, UI, UIEvent, UIEventHandlerContext, UIOutEvent, Scene, Assets, RenderState`;
`type GameUI = UI<(), UIOutEvent>`.
- **`UI` has exactly ONE mod-callable method:** `register_handler(&mut self, UIEvent, impl Fn(&mut UIEventHandlerContext))`. No `find/set_text/set_visible/add_node/spawn/load_template` (all E0599). Mutate via **public fields**: `ui.root: Node`, plus `Node::add_child(&Assets, Node)`.
- **`Node` public fields (9):** `id, visible, disabled, focus, runner:Box<dyn NodeRunner>, layout, rect, contents_rect, child:Vec<Node>`. You can show/hide/position/restructure/add children. **The displayed text/number lives inside `runner:Box<dyn NodeRunner>`, which `mod_api` exposes NO setter for and does not even re-export `NodeRunner`.** ⇒ **no way to set/change any node's text or value.**
- **`NodeTemplate`** fields `id, name, style_path, property:HashMap<String,Rc<dyn Any>>, child`; `property` opaque, no `new` constructor → cannot imperatively build a value-bearing widget.
- **`RenderState`** is a plain data struct (`map_size, commands, pass, exports, base_map`); **zero draw methods** (`draw_text/draw_rect/...` all E0599); `RenderCommand/RenderPass/ExportCommand` not re-exported → **no immediate-mode drawing for mods.**
- **`UIEvent` variants (10):** `Click{path,item}, RightClick, CheckboxSelect, TreeViewSelect/RightClick/Move, TextEditComplete, Remove, Custom{path,data:Vec<u8>}, Changed`. Handlers detect clicks (incl. on mod-added nodes) by `path` string and get full `&mut ui` + `out_events`.
- **`debug_text`** exists only as a `FrameVtable` fn-pointer with **no safe `GameCtx::debug_text` wrapper in (0,7)**. The only mod draw primitives are `GameCtx::debug_draw_line/circle` (gizmos on the in-match sim map, not the management/draft UI, not text).

---

## 3. BUILD + DEPLOY pipeline

**Toolchain (one-time, PER SDK):** install the nightly the SDK was built with — read it from
`<SDK>\toolchain_version.txt` (`rustc … (HASH DATE)` → channel = DATE + 1). For the current 0.4.14 SDK:
```powershell
rustup toolchain install nightly-2026-05-24 --profile minimal   # rustc 23a3312d9 (C:\tmp\mod-sdk)
# + MSVC C++ build tools (link.exe) and Windows SDK must be installed
```
Don't `rustup default` it — `build_mod.ps1` sets `RUSTUP_TOOLCHAIN` per-build and restores it after.

**Build the game_view/game_core-linking mods** (`tfm2_db_export`, `tfm2_patch_probe`) — these need the
`game_view`/`engine_ui`/`game_core` rlib externs on top of `mod_api`, which `build_mod_cargo.ps1` does NOT inject.
Use the unified **`build_mod.ps1`** (auto-derives toolchain+rustc hash from the SDK, picks a consistent rlib set by
that hash, restores env):
```powershell
& C:\tmp\tfm2-ai-mod\build_mod.ps1 -Crate C:\tmp\tfm2-ai-mod\tfm2_db_export
& C:\tmp\tfm2-ai-mod\build_mod.ps1 -Crate C:\tmp\tfm2-ai-mod\tfm2_patch_probe -CheckOnly   # compile canary, no DLL
```
(`build_db_export.ps1` / `check_patch_probe.ps1` are thin shims to these; no per-patch pin edits needed.)

**Build via the SDK script** (for `mod_api`-only mods like `tfm2_pool_reader`, or `tfm2_combat_ai` in the creator repo; run from **PowerShell**):
```powershell
$env:PATH="$env:USERPROFILE\.cargo\bin;$env:PATH"
cd C:\tmp\tfm2-ai-mod\<mod_folder>      # rustup resolves rust-toolchain.toml from CWD, not manifest — cd in first!
& C:\tmp\tfm2-ai-mod\sdk\mod-sdk\build_mod_cargo.ps1 -Project . -SdkDir C:\tmp\tfm2-ai-mod\sdk\mod-sdk
```
- The SDK injects `mod_api` by globbing `libmod_api-*.rlib` and passing `--extern mod_api=<rlib>` + `-L dependency=deps`
  + `-L native=native` via `CARGO_ENCODED_RUSTFLAGS`. **Never** put `mod-api` in `[dependencies]`. A plain `cargo build` fails E0432 (expected).
- Cargo builds are **always `--release`**; output `<mod>\target\release\<name>.dll` is copied into the mod folder as `<folder>.dll`.
- Single-file mods (no crates): `build_mod.bat path\to\src\lib.rs` → direct `rustc --crate-type cdylib --edition 2021 ...` (DLL named after the source stem, dropped in CWD).
- `Cargo.toml` must have `[lib] crate-type=["cdylib"]`. If inside another workspace, add an empty `[workspace]` table.

**`mod.mod_info`** (required; only file needed to appear in the Mods menu):
`{ name, author, version, description, last_updated, dependencies:[{mod_id, version}] }`. `"base"` dep = the game version line (semver ranges, sets load order).

**`mod.override_info`** (optional asset merge/override): object keyed by base asset path → `{ <pointer>, "type": "merge"|"override" }`.
⚠️ **Doc discrepancy (UNRESOLVED):** the value key is **`"remapping"`** in `asset-overrides-and-i18n.md` vs **`"path"`** in `getting-started.md`. Parser is in the binary; resolve empirically (invalid override JSON disables the mod with a diagnostics popup). Prefer `"remapping"`.

**Enable / load:** place folder under `…\Teamfight Manager2\mods\<mod_id>\` → title-screen **Mods** menu → enable → **restart game** (DLLs + startup assets load at launch; no hot-reload).

**Logs / diagnostics:** primary = the **title-screen diagnostics popup** (names the failing file/DLL). No documented separate log file. Native-DLL failure causes: missing DLL; built with different SDK/nightly; id ≠ folder name; missing `declare_mod!` (no entry symbol); wrong API version symbol; wrong target/profile; entry panicked. (Verify exports with `dumpbin /EXPORTS` → `tfm2_mod_entry` + `tfm2_mod_api_version`.)

**Uploader (`TFM2ModUploader.exe`, in the game folder):** builds (if "Build native Rust code" checked) + uploads; skips `src/`, `target/`, `Cargo.toml`, `Cargo.lock`, `mod.workshop_id` from the upload. First upload writes `mod.workshop_id` (keep it). "Unpack Base Bundle" writes `mods\base_unpacked\` for reference.

---

## 4. CAPABILITY VERDICTS ★ (the important part)

| Capability | Verdict | Reason / API |
|---|---|---|
| **LIVE banpick auto-read** (read human's in-progress picks/bans in real time) | ★★★ **WORKING — RUNTIME-VERIFIED 2026-06-11 (0.4.11)** | `pre_update(_, ui)` → `ui.query("main")` (`find` doesn't exist — Iterator red herring) → `node.runner` (**PUB field**) → ‼️ safe `downcast_ref::<MatchUIRunner>()` **ALWAYS FAILS at runtime** (game exe vs SDK rlib = separate compile sessions → different crate disambiguators → different TypeIds) → use a **raw cast** `&*(as_any() as *const dyn Any as *const MatchUIRunner)` (same source+rustc ⇒ identical layout), guarded by GATE 1 (banpick-only node `main.timer_area.timer_text` exists) + GATE 2 (content validation: ban_count≤20, sane distinct team ids, vec lens≤10, champ ids ascii-lc/underscore; any fail ⇒ no publish). `game_view::MatchUIRunner` is a **PUB re-export at the crate ROOT** (inner module private). Pub fields: `team1_ban/team2_ban/team1_pick/team2_pick: Vec<String>` (ids, pick-order preserved), `team1_order/team2_order: Vec<usize>`, `team1/team2: usize` (side via my_team), `ban_count`, `match_info`, `database: Rc<RefCell<ClientDatabase>>`. Build: cdylib + CARGO_ENCODED_RUSTFLAGS `--extern game_view=<rlib> --extern engine_ui=<rlib>` (type-only → 144KB DLL). Mod `tfm2_bp_reader\` → %LOCALAPPDATA%/tfm2-overlay/bp_live.txt; overlay 자동연동 snaps the grid on change. Verified against a real draft: 6 bans + 5 picks all exact, order preserved. Probes c:\tmp\probe_ctx\p_mui1-6.rs. engine_ui pub too: `engine_ui::runner::LabelRunner{text}`. Per-patch: game_view rlib hash changes → rebuild bp_reader with each SDK. (The §4a server-command route is moot.) |
| Live draft via client `ClientDatabase`/`MatchInfo`/`game_view` (db route) | **IMPOSSIBLE-PROVEN** (but moot — see MatchUIRunner row) | Client db keys matches by `MatchType` only; `MatchInfo` has no `set_info`; `db.game_view` is `None`/sealed during banpick. |
| Live draft via `ModDraftScoreHook` (player attribution) | **IMPOSSIBLE-PROVEN** (re-confirmed 2026-06-11: player-match enemy AI did 3 bans+1 pick with ZERO hook calls; the 수석코치 also bypasses it — its recommendations are client-side game_view logic over ClientDatabase stats) | Hook fires only for OTHER league matches simulated in lockstep with the player's draft (that's the 0.4.6 "pollution"). Fine for *buffing AI sims*, useless as a player source. Do not retry. |
| Live draft via `after_management_tick` `running_matches` | **IMPOSSIBLE-PROVEN** | Management tick PAUSED during manual banpick; visible `running_matches` are already-decided league sims, not the live board. (This is what the un-tested `handle_command` route avoids.) |
| In-match battlefield reads (`GameCtx`/`EntityRef`/`PlayerRef`) | **POSSIBLE** | Full reads from `&GameCtx` in custom champ/item callbacks (`ModPassive::on_*`, `ModEffectType::apply`). NOT from `ModPlayerInputAi::think`. |
| In-match mutations | **POSSIBLE but limited** | Exactly `deal_damage`/`heal`/`add_buff`/`apply_cc` (+debug draw). No move/target/summon/cooldown/buff-removal. |
| Combat AI override (`ModPlayerInputAi`) | **POSSIBLE but read-blind** | Can `Replace` final `Input`, but no enemy/ally discovery → low-HP retreat/recall logic only; manual targeting infeasible. |
| Custom champions / items / effects / passives | **POSSIBLE** | `ModChampionInfo`+`ModAction`+`ModEffectType`+`ModPassive`; `ModItemInfo`. Data-champions for JSON-only. |
| `ModEffectBuff` registration | **UNTESTED** | Type exists, no `ModRegistration` path in (0,7); reachable only via internal engine effects not exposed to data champions. |
| Live in-game winrate/banrate/pool export | **POSSIBLE** | `db.available_champions` + `db.champion_patch_statistics` readable by value, clean (no sim noise). |
| **Authored per-patch champion buff/nerf** (exact pre→after + direction + reasons) | **★ POSSIBLE — RUNTIME-VERIFIED 2026-06-20** | CLIENT-readable, NO server ext needed: `ClientDatabase.teams: HashMap<usize, Team>` → `Team.news: Vec<News>` → `News.ty: NewsType::PatchNote { version:String, champion_patch_data: Vec<(champ, Vec<ChampionPatchData{ key, target_action:Option<String>, pre_value:String, after_value:String, is_buff:bool }>)>, champion_patch_reasons:Vec<String>, .. }`. Latest `version` = current patch; **`is_buff` is the GAME's OWN authoritative direction → the `field_sign` "bigger-is-better" heuristic is obsolete**. `game_core::{NewsType, ChampionPatchData}` are PUB (canonical `game_core::data::news`/`::data::patch`; need `--extern game_core`). Shipped in `tfm2_db_export::authored_chg()` → recommender `chg`/`chg_nudge`/`chg_mark`/stale-blend-drop; the defs-snapshot diff (`champ_changes_update`/`field_sign`) demoted to FALLBACK. ⚠️ Sibling `Database.champion_action_patch_state: HashMap<champ, ChampionActionPatchState{ base_attack/skill1/skill2/ult: HashMap<field, f32 ratio>, *_value: HashMap<field, String> }>` is CUMULATIVE-vs-base (38/60 champs; server-only) — already in winrate ⇒ double-counts ⇒ NOT used. (The client-direct route above IS the shipped one and is verified in-tree via `authored_chg` + the `tfm2_patch_probe` compile canary. A server-ext→mod_save bridge was *also* observed to fire in a throwaway session but its code was NEVER committed — treat as UNVERIFIABLE-FROM-TREE, not needed since the client route works.) 2026-07-04 review hardened `authored_chg`: stats-derived anchor, currency guard, empty-note = `Some([])`, `'R'` rework promotion — see `REVIEW-FIXES.md`. |
| Dynamic in-game recommender UI (text/score readout, reacts to clicks) | **IMPOSSIBLE-PROVEN** | No node text/value setter (`runner` sealed, `NodeTemplate::property` opaque, no `NodeTemplate::new`) + no immediate-mode text draw (`RenderState` has no draw methods, no safe `debug_text`). See §4b. |
| Adding/showing/hiding/positioning UI nodes + click handlers | **POSSIBLE** | `ui.root.add_child`, public `Node` fields, `register_handler` — proven to compile to a real cdylib. Just can't carry dynamic text. |
| Mod commands/events, mod save data, inter-mod services | **POSSIBLE** | §2g–2i. |
| Mod on-screen text rendering (anywhere) | **IMPOSSIBLE-PROVEN** | No text draw API in (0,7); player-facing UI ⇒ external hudhook overlay. |

### 4a. LIVE BANPICK auto-read — the CORRECTED analysis

**Status: UNTESTED-NEEDS-EXPERIMENT (architecturally de-risked). The prior "IMPOSSIBLE-PROVEN" was wrong** because it
conflated two different server entry points. Correction:

- **Tick hooks** (`before/after_management_tick`, gated by `dispatch_mod_after_management_tick`): the management tick only
  advances on Proceed/skip-days. During a manual banpick it does NOT fire, and the `running_matches` visible from it are
  already-decided league sims. **This conclusion is valid — but ONLY for the tick hooks.** (IMPOSSIBLE-PROVEN, kept.)
- **`handle_command`** (event-driven, invoked by the server worker's packet handler — the SAME pump that processes
  `ClientPacket::BanpickSelect`): every static link is **binary-confirmed**:
  - `ClientData::send_mod_command` → `ClientPacket::ModCommand` (**sibling of `BanpickSelect` in one enum, one channel**).
  - → `ModServerExtension::handle_command(ctx, command)`; `ctx.server_state.running_matches: HashMap<usize, RunningMatchInfo>`.
  - `RunningMatchInfo.set_info: Vec<MatchSetInfo>`; `MatchSetInfo` carries `phase:BanpickPhase`, `team1/2_picks`, `team1/2_bans`, `is_team1_blue`.
  - The worker handlers `run_select_champion`/`select_champion` write the human's clicks into `current_set`→`set_info` live; `keep_ingame_running_while_paused` shows the worker keeps running while management is "paused".

**THE ONE UNVERIFIED LINK (make-or-break):** does the in-process server worker **dispatch `ClientPacket::ModCommand` →
`handle_command` DURING the human banpick (while the management tick is paused)**, and is single-player's running match
keyed/visible the same way? These are scheduling facts only the running game reveals (the worker might pump the
ModCommand immediately, or buffer it until management resumes).

**Minimal experiment to settle it** (~60 lines, standard build flow). One mod, id e.g. `bp_probe`, registering BOTH:
- **Client `ModExtension::post_update`** (gated `Scene::InGame`): each frame `data.send_mod_command("bp_probe","scan",&[])`; drain `take_mod_events("bp_probe")` and log each payload with wall-clock + client tick.
- **Server `ModServerExtension::handle_command`:** on `"scan"`, log entry (monotonic counter + which phases present even if empty), serialize every `running_matches[*].set_info[*]` (`phase, team1_picks, team2_picks, team1_bans, team2_bans, is_team1_blue`), `ctx.emit_event_to_command_sender(command,"scan_result",snapshot)`, return `Handled`.
- **Run:** single-player career → enter match → during the **manual banpick** (before proceeding the day) pick/ban a few champs.
- **PASS** if, live, the server log shows `handle_command` firing AND a `running_matches` entry whose `set_info[*].phase` is a
  `Team1Pick/Team2Pick/Team1Ban/Team2Ban` value AND whose picks grow as you click (snapshot mirrors the screen within a frame or two).
- **FAIL (verdict stands)** if `handle_command` never fires during banpick (commands buffered until proceed) OR `running_matches`
  is empty / only unrelated decided sims / never reflects your live picks.
- **Disambiguator** vs the old league-sim-pollution finding: log the count+ids of `running_matches`; if the only populated entry
  matches the on-screen score/count (not dozens of background sims), you have the human match.

### 4b. Dynamic in-game UI — verdict

**A live-updating recommender panel (ranked Korean champion names + scores reacting to clicks) is IMPOSSIBLE-PROVEN in
(0,7).** Independently verified by compiling probes against the ABI-matched binary. You CAN build/attach/show/hide/position
nodes and register click handlers (a node-build + `register_handler` mod compiles to a real `p_positive.dll`), but the
**single hard blocker is text/value output**: no node text/value setter (`runner:Box<dyn NodeRunner>` sealed, `NodeTemplate::property`
opaque, no `NodeTemplate::new`) and no immediate-mode text draw (`RenderState` has no draw methods; `debug_text` has no safe
wrapper). A recommender's entire value is displaying changing ranked text/scores — that capability does not exist.
⇒ **The recommender MUST remain the injected hudhook overlay, and the 초성-button Korean-IME workaround stays required.**

### 4c. Content modding scope

Custom champions/items/effects/passives are fully POSSIBLE via native Rust (`ModChampionInfo`/`ModItemInfo`+building blocks)
and partially via `.data_champion` JSON (60+ effect types; engine has more impls than JSON exposes — summons, `AddEffectBuff`,
`TowerAttack`, bespoke modules need native Rust). Asset/sprite/i18n pipeline works via `mod.override_info` (prefer `merge`).

### 4d. GameCtx in-match reads

Full and reliable (PROVEN, docs accurate) but ONLY from `&GameCtx` callbacks inside custom champions/items — never from
`ModPlayerInputAi::think` (which has no `GameCtx`) and never for the management/draft UI.

---

## 5. GOTCHAS

- **Toolchain ABI (per-SDK, auto-derived — don't hardcode):** the nightly MUST match the SDK; derive it from `<SDK>\toolchain_version.txt` (channel = rustc date + 1). `build_mod.ps1` does this and restores `RUSTUP_TOOLCHAIN` after each build; a mismatch → E0514 or *silently* broken `Any` downcasts. Current 0.4.14 SDK = `nightly-2026-05-24` (`23a3312d9`); the 0.4.6-era SDK was `nightly-2026-05-29` (`cced03bfd`). (The SDK's `rust-toolchain.toml` pins the exact channel; the older one only pinned `channel="nightly"` — hence the historical "pin manually" warning, now handled by `build_mod.ps1`.)
- **Build from PowerShell, cd into the mod folder first:** rustup resolves `rust-toolchain.toml` from the **CWD**, not the manifest path. Building from elsewhere silently uses the wrong nightly → E0514.
- **Never add `mod-api` to `[dependencies]`** — the SDK injects the ABI-matched prebuilt rlib. Plain `cargo build` fails E0432 (expected). For rust-analyzer only, point at the SDK rlib locally; never commit a machine path dep into the upload package.
- **SDK template bug:** `mod-sdk/template/src/lib.rs` ends with `declare_mod(init);` — **missing the `!`** — and won't compile. Use `declare_mod!(init);`.
- **i18n merge-vs-override:** prefer `"merge"` for text/JSON; `"override"` replaces the WHOLE base asset (two overriding mods clobber each other). Multi-mod conflicts: enabled-mod order decides the winner. Invalid override JSON disables the mod.
- **`mod.override_info` value key unresolved:** `"remapping"` vs `"path"` (docs contradict). Resolve empirically via the diagnostics popup.
- **`ServerState`/`RunningMatchInfo`/`MatchSetInfo` are NOT nameable types** from `mod_api` (E0425/E0432); reach them only structurally via `ctx.server_state.running_matches` field access.
- **rmeta is not a full method index:** method names surface only when in doc-comments. Prove absence with compiler probes (`let T{}=v`→E0027 lists fields; `match v{}`→E0004 lists variants; `let _:()=&v.f`→E0308 reveals type), not rmeta grep.
- **Buffs are add-only; no movement/targeting/cooldown control; can't read an entity's current action or the base AI's plan.**
- **Handles/refs are callback-lifetime-bound** (`EntityHandle`/`EntityRef`/etc.) — never store across frames.
- **`tfm2_draft_ai` still enabled** in `mods.json` (superseded by the overlay) and harmlessly spams `C:\tmp`. Disable if it interferes.

---

## 6. Cross-links

**Memory files** (`C:\Users\veria\.claude\projects\…\memory\`):
- `MEMORY.md` — index.
- `tfm2-combat-ai-project.md` — active code-mod project.
- `tfm2-modding-reference.md` — engine = Rust; code mods = cdylib via mod_api; hooks, SDK repo, build/deploy.
- `tfm2-game-install-paths.md` — filesystem locations (game/mods/SDK-cache/docs/toolchain).
- `tfm2-builtin-ai-repertoire.md` — base match AI repertoire + moddability gap analysis.
- `tfm2-meta-dashboard.md` — the recommender overlay product (manual click-grid, draft_weights.tsv).
  ▶ **Update this:** downgrade the live-draft verdict from "IMPOSSIBLE-PROVEN" to "UNTESTED — server-command path (§4a)".

**Analysis writeups** (`c:\tmp\tfm2-ai-mod\analysis\`):
- `recommender-algorithm-review.md` — **2026-07-05 알고리즘 건전성 심사**: 5-도메인 병렬 분석 + 실측(BT 분해 τ≈0, comp 층화, EB κ\*, W_* 출처 전수 감사). 핵심: 코어(κ-수축/posterior-mean/블렌딩)는 건전, ai_prior 신뢰서열 역전·comp 부스트 반증·dup 부호 반대·티어 허위확신·비인과 CV가 결함. Step 0–3 로드맵 + 재론금지 표 포함 — **점수 모델을 만지기 전에 반드시 읽을 것**.
- `draft-ai-design.md` — `ModDraftScoreHook` design notes (base_score scale unknown).
- `src_map.txt`, `strings_ascii.txt`/`strings_utf16.txt` (binary string dumps).
- Moved to the tfm2-champion-creator repo (2026-10-26): `builtin-ai-architecture.md` (`FactoredBanpickAgent` + base AI
  strategies), `behavior-design-space.md` (GameCtx mutations; VERIFIED-ABSENT list), `champion-creator-feasibility.md`,
  `completeness-audit.md`, `base_champion_info.json`.

**Docs / SDK** (`c:\tmp\`):
- `TFM2Mod-docs\docs\` — `native-mod-api-reference.md` (authoritative method surface), `native-ai-hooks.md`, `native-rust-mods.md`, `mod-save-data.md`, `data-champion.md`, `asset-overrides-and-i18n.md`, `mod-package.md`, `getting-started.md`, `troubleshooting.md`.
- `mod-sdk\` — `deps\libmod_api-*.{rlib,rmeta}` (the facade), `libgame_core-*.rmeta` (server types), `libgame_view-*.rmeta` (client types), `native\` (windows import libs), `template\`, `build_mod.bat`, `build_mod_cargo.ps1`, `rust-toolchain.toml`, `toolchain_version.txt`, `base_version.txt`.
- Working compiling examples: `tfm2_combat_ai\src\lib.rs` in the creator repo (champion+passive+effect), `tfm2_pool_reader\` (live-stats export), `draftwatch\` (client probe).

---

## 7. Status / next steps

- [x] Recon: engine, mod system, full API surface, build/upload flow, versions — all binary-verified.
- [x] Capability map complete; UI + content + AI-hook + server-state surfaces compiler-probed.
- [x] `tfm2_combat_ai.dll` builds + exports verified; recommender overlay shipped (manual click-grid).
- [ ] **Run the `bp_probe` experiment (§4a)** — the one decisive test for live banpick auto-read. This is the highest-value open task; it either unlocks an auto-reading overlay or finally closes the question.
- [ ] If §4a passes: build a server-extension that emits the live board to the overlay (replacing manual clicks).
- [ ] Optional: live-stats exporter mod (`db.champion_patch_statistics`), champion-creator content polish.
- [x] **Authored patch-change integration (2026-06-20):** recommender `chg` now driven by the game's OWN `PatchNote.is_buff` (client-direct via `db.teams` → `Team.news` → `NewsType::PatchNote`), replacing the `field_sign` defs-snapshot heuristic (kept as fallback). Shipped in `tfm2_db_export::authored_chg`; writes `chg_authored.txt` for verification. The `champion_action_patch_state` route was rejected (cumulative-vs-base = double-counts winrate). Settled by ~8 compile probes (no game) + 2 in-game runs; `tfm2_patch_probe/` is the throwaway prototype (disable in Mods menu).
- [x] **2026-07-04 max code review follow-up (0.4.14) — Phase 1 + Phase 2 shipped:** 15 findings, all actioned (see **`REVIEW-FIXES.md`** for per-finding detail + do-not-relitigate refutations).
  - **Phase 1 (recommender truthfulness), deployed:** debug-dump gating behind `probe/DEBUG_DUMPS` marker (kills load-freeze/8s hitch); enemy_intent commentary cut (background-sim misattribution); `authored_chg` hardened (stats-derived anchor, currency guard, empty-note = `Some([])`, empty-changeVec skip, `'R'` rework promotion from defs-diff, both-branch `chg_authored.txt`). ⏳ in-game smoke pending (user runs game).
  - **Phase 2 (patch resilience), verified:** `build_mod.ps1` unified builder auto-derives toolchain+rustc hash from the SDK (no more per-patch hand-pins — was finding #1); `tfm2_patch_probe` is now a compile **canary** (`check_patch_probe.ps1`), not a broken/misleading mod (mod.mod_info + build_patch_probe.ps1 deleted); §0/§3/§5 reconciled (this line's siblings).
  - Only Phase 2 leftover: stale disabled `tfm2_patch_probe\` in the *game* mods folder (repo no longer produces it) — user can delete.
- **Algorithm-improvement backlog (2026-07-05):** the soundness review (`analysis/recommender-algorithm-review.md`, F1–F14) is spec'd into an executable plan at **`ALGO-FIXES.md`** — read it (incl. the do-not-relitigate table) before touching the score model.
  - [x] **Step 1 shipped (evidence-complete, per-item commits `1c0667b`…`cc17b9f`, deployed):** ai_prior re-gated to cold-start scope + patch-reset (F1); comp recalibrated, refuted +0.05 frontline boost removed, heal up (F3); W_DUP=0 — wrong sign (F4); zombie/cold-start weight paths documented (F8); display honesty — posterior-sd tier "?" gate, zero-data low_conf, base-label split, counter_of + fearless wording (F2/F10); 3 stale comments fixed (F10). ⏳ in-game smoke pending; `TIER_UNSTABLE=0.10` needs an in-game '?'-density look. Deferred: flex-bonus attribution.
  - [~] **Step 0-1 done — ⚠️ overturned the project's #1 signal:** `validate-terms` folds are now causal rolling-origin (was non-causal i%5; `--legacy` reproduces the old numbers bit-for-bit). Result: **famg ("athlete games" familiarity), treated as the validated strongest signal (+7.9%p) and the dominant `af()` producer component, was a LEAKAGE artifact** — base_lane+0.08·famg 66.3%→57.8% (lift→harm) under causal eval. base_lane robust (59.4→59.0); only fam_c (centered winrate) survives (+1.0%p — **provisional**: per-origin shows it early-concentrated, late-pooled −1.0). **S0-1b per-origin discriminator (07-05): LEAKAGE CONFIRMED, data-starvation refuted** — Δfamg worsens as training grows (+0.7 @train 535 → −4.8 @3291; famg standalone <50% at late origins) while legacy folds at matched train size (~3590) lift +5.4…+10.8 in every fold. New backlog **F15 (fam re-weighting: famg→~0, famc provisional)**; full writeup + artifacts in **`analysis/harness-causal-findings.md`**. ⛔ Do NOT change fam weights until S0-3 (port production math) + a causal famc sweep.
  - [x] **Step 0-3 done (2026-07-05):** production math ported into the harness (validate-terms v3.2: patch-local current(id-ordered expanding) + prev·0.35·(1−cm/40), changed-champ resets from the 7-note dump, κ40/κ20 beta_post). **Shipped patch-local memory BEATS pooled history causally** (base_prod 60.0% / base_lane_prod 59.5% vs pooled 58.6–59.0%); κ8-vs-κ40 pooled difference immaterial (F7/F11 closed). famg dead on the shipped anchor too; **famc +1.6%p (s=0.25 optimal) but STILL early-concentrated (late −0.8…−2.2)** → regime-dependence suspected; famc raise stays gated on S0-4 CI.
  - [x] **Step 0-4 done (2026-07-05):** pre-registered 6-model + 1000× bootstrap CI (validate-terms v3.3; artifacts `analysis/harness-s04-ci.txt`). Only base passes (②−① +7.6 [+5.3,+9.7]); shipped af −0.1, bundle-vs-base +0.7, comp +0.1, LP −0.1 — **all CI-unproven at n=3,953 → cosmetic-budget items, growth forbidden.** New insight: **af's 0.05 cap saturates on dead famg alone at g≈11, crowding out famc** → F15 executable form = remove famg from af (0.15·famc, keep cap); famc RAISE rejected. Overturned: W_MU_LP=0.008 "sweep optimum" (was non-causal; causal contribution −0.1) → Step-2 zero candidate; lib.rs:113 "+2.5%p kept" comment. aip causal standalone 48.4% (consistent with S1-1 gate).
  - [x] **Step 2 adjudicated + shipped (2026-07-05, commits `426822f`/`ac355ea`/`d9b68a3`/`32981f6`, DLL deployed):** F15 af famg-removal ✓ (af = min(0.05, 0.15·famc)); S2-4 W_MU_LP=0 ✓ (matchup axis fully weight-0, power_ramp back to 1.0); S2-2 count-scaled chg_nudge ✓ (T-nudge slope +0.0125/chg, r +0.66, n=46; 0.01+0.005·min(n,4), mixed '~' now directional; CHG rows 5-field, missing counts → legacy ±0.02). Rejected/closed by harness: S2-1 (no n≤2 population; ties at n≥3 — full reset stays), S2-7 (stack Δ−0.1 — no term), S2-8 (no comp component adds value, heal included — W_COMP kept as UX guardrail only), S2-3 (κ sweep flat — 40 confirmed, no EB). S2-5/S2-6 later ran preliminarily on current data (v3.5): BOTH REJECTED — solo prev-blend Δ+0.0 [−0.1,+0.2]; BT θ standalone 58.8% vs base_prod 60.0 with thin-stratum −2.7 (unshrunk joint fit overfits thin champs); recheck post-recapture. Bonus: parse_lane_dom nondeterminism (HashMap order × f64 summation) caught by the --legacy byte-regression → BTreeMap fix. ⏳ in-game smoke: nudge-size differentiation, '~' champs now nudged, counter suggestions disappear (honest — they were noise), af no longer cap-saturated.
  - [x] **Step 3 executable subset shipped (2026-07-05, commits `e1a981b`/`58377b6`/`4420d6e`/`6e4df25`, DLL deployed):** S3-2 snipe ungated (enemy pool = pool−taken when no locks yet); S3-3 pick-side pocket steal (SAME-DAY ZEROED by the T-ban grader — see below); S3-7 soft POS eligibility (≥10 CP games → candidate at −0.01 off-meta, lane grid + assign_core; flex/depth counters unchanged); S3-8 OPP usage shrunk to (g+1)/(tg+3) (banrate-proxy half deferred to T-ban). S3-5 mooted by S2-4 (all matchup consumers weight-0). Remaining Step 3: T1–T3 in-game rule tests (user) → S3-1/S3-4/fearless-steal; T-ban + T4 graders after S0-2.
  - [x] **T1/T2 adjudicated from bp_history (2026-07-05 evening, 51 lines = one 3-set fearless series + 1 league draft):** T1 — bans are SET-scoped (5/6 set-A bans re-banned in set B; a set-A-banned champ picked in set B), pick locks are PER-TEAM (both sides cross-picked each other's used champs, 4 cases) ⇒ **S3-3 fearless ×1.5 premise REFUTED — closed** (my steal does not erase the enemy's series pool). T2 — the team1 slot ALWAYS acts first (4/4 drafts, sides alternate per set) ⇒ **S3-1 shipped** (`d830b15`: Board.i_am_team1 seeds first_pick at set start; pick observation stays as ground-truth override) + **S3-4 shipped** (`27aa020`: first pick → top-delta champ ban ×0.4 / others ×0.8; second pick → ×1.1). T3 — provisionally clean (zero pick-list oscillation in 51 lines → confirmed-only; no debounce). DLL deployed 18:0x.
  - [x] **Step 0-2 done + full 10-version recheck (2026-07-05 evening):** 5,809 replays / 10 versions (4.0=608, 4.1=477), 5,274 scored. **Verdicts moved:** ④−② (shipped bundle vs base_lane) **+2.2 [+0.6,+3.8] → CI-PASS** (pooled base collapsed to 58.1% on the patch-churn origins while the shipped patch-local state held 59.9–60.2 — the design's edge widened); famc early-concentration caveat RESOLVED (both pools +1.4/+1.5; raise still CI-unproven → keep 0.15); famg's monotone-worsening argument RETIRED (late pool flipped to +0.8) but the LEAKAGE conclusion stands via the legacy-gap (+5.4..+10.8 at matched train vs causal ≈0 mixed-sign) — F15 unchanged (cap-saturation grounds); BT θ now significantly NEGATIVE (−1.6 [−3.1,−0.1]); S2-5 solo Δ+0.0 reconfirmed; T4 baseline stable (0.678/0.625, Δ+0.053 [+0.047,+0.060]); T-nudge robust (slope +0.0134, r 0.60, n 62). ⚠️ T-amnesia n≤2 stratum CONTAMINATED by synthesized 4.0/4.1 note sections (n=1 placeholder counts, `analysis/patch_notes_ext.txt`) — S2-1 stays rejected; the news dump was added to the DEBUG_DUMPS gate (deployed 20:57) so future captures carry REAL counts.
  - [x] **T-ban grader built + adjudicated (2026-07-05 night, v3.7, 11,138 chronological predictions):** enemy-pick recall@5 — global pickrate 0.185 vs team-usage OPP 0.184 (Δ−0.002 — NO team identity beyond the meta) vs OPP+pocket 0.162 (Δ−0.021 [−0.024,−0.019] — athlete volume points at old-meta mains, actively counterproductive). Actions shipped: W_BAN_OPP=0, ban-side enemy_pocket term removed, S3-3 W_PICK_DENY=0 (retracted ~3h after shipping, by the grader built to adjudicate it; snipe S3-2 is strength-based — kept). DLL deployed 21:05.
  - [ ] **S2-1 re-adjudication** (needs a real-count notes dump from the next DEBUG_DUMPS capture), **T3 deliberate-hover check** (optional), **S3-6** (optional; over-engineering guard), **S3-6** (optional, over-engineering guard), **T3 deliberate-hover check** (optional, 1 draft). 07-05 17:42 session smoke (file-verified): mod clean on 2026.4.0, `source=authored anchor=2026.3.1` (11 champs), DEBUG_DUMPS gate held, count-scaled nudge circumstantially live (our 3 bans were all note-changed champs). See ALGO-FIXES.md.
