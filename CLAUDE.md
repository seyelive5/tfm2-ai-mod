# CLAUDE.md — TFM2 Combat AI Mod

Project context for Claude Code. Goal: a **native Rust code mod** that customizes the
**in-match player AI (combat behavior)** of *Teamfight Manager 2* (TFM2).

## TL;DR

- **Game engine:** custom **Rust** engine (panic strings, `struct … with N elements`, glutin/egui). Not Unity/Godot.
- **Mod kinds:** (1) **Database Pack** = JSON/data only (`.tfm2db`); (2) **Code Mod** = native **Rust `cdylib`** built against the `mod_api` crate. We build a **code mod**.
- **Our hook:** `ModPlayerInputAi` — per-tick `think()` returns `PlayerInputDecision::{Pass, Replace(Input)}`, running *after* the built-in AI's base input.
- **SDK / docs source of truth:** GitHub repo `teamsamoyed/TeamfightManager2Mod` ("TFM2 Mod Documents"). Local clone at `c:\tmp\TFM2Mod-docs`.
- **Game + SDK version:** **0.4.6**. `mod_api` `API_VERSION == (0, 7)`.
- **Toolchain:** Rust via rustup. The 0.4.6 rlib was built with `rustc 1.98.0-nightly (cced03bfd 2026-05-28)`. The rustup channel that ships that exact commit is **`nightly-2026-05-29`** (NOT `-05-28`, which ships `57d06900f` and is rejected with E0514). Pinned in the mod's `rust-toolchain.toml`. **MSVC C++ build tools + Windows SDK are also REQUIRED** for the linker (`link.exe`) — rustc's msvc target cannot produce the DLL without them.

## Repo layout

```
c:\tmp\tfm2-ai-mod\               # git repo / dev workspace (this folder)
  CLAUDE.md                       # this file
  tfm2_combat_ai\                 # the deployable mod package (folder name == mod id)
    mod.mod_info                  # mod metadata + dependencies (base >= 0.4.6)
    Cargo.toml                    # cdylib; do NOT list mod-api (SDK injects it)
    src\lib.rs                    # ModPlayerInputAi hook (baseline: low-HP retreat)
  sdk\                            # (gitignored) extracted TFM2 Mod SDK 0.4.6 — see below
```

**Naming rule (critical):** mod folder name == `ModRegistration::new("…")` id == `mod.mod_info` implied id == produced DLL name. All four are **`tfm2_combat_ai`**. Keep them aligned or the game won't load the DLL.

## Key external locations

- **Game install:** `C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2\`
  - `TeamfightManager2.exe`, `TFM2ModUploader.exe`, `steam_api64.dll`, `bundle.game_data` (1.1 GB packed assets).
  - `mods\base_unpacked\` — unpacked reference base assets + `setting\*.json` (game data). Produced by Uploader → **Unpack Base Bundle**.
  - `config\game\mods.json` — `enabled_mods`, etc. `config\game\base.json` — `lang`, resolution.
- **Local mods folder (where deployed mods live):** `…\Teamfight Manager2\mods\<mod_id>\`.
- **SDK cache (managed by Uploader):** `%LOCALAPPDATA%\TeamSamoyed\TeamfightManager2\mod-sdk-cache` (env overrides: `TFM2_MOD_SDK_CACHE`, `TFM2_MOD_SDK_DIR`).
- **Mod docs/SDK repo:** https://github.com/teamsamoyed/TeamfightManager2Mod (releases carry the SDK zip, e.g. `0.4.6.zip`, ~330 MB).
- **Docs clone:** `c:\tmp\TFM2Mod-docs\docs\` — read `native-ai-hooks.md`, `native-rust-mods.md`, `native-mod-api-reference.md`, `getting-started.md`, `workshop-upload.md`, `troubleshooting.md`.

## The combat-AI API (what we can actually use)

`ModPlayerInputAi` (see `native-ai-hooks.md`):
- `clone_box`, `id`, `priority` (lower runs first; higher runs later & overrides), `matches(&PlayerAiInitContext) -> bool`, `think(&mut PlayerAiContext, base_input: Option<Input>) -> PlayerInputDecision`.

`PlayerAiInitContext` (attach-time scoping): `player_id, athlete_id, team, position, champion_name`.

`PlayerAiContext` (per-tick) helpers available in API 0.7:
- identity: `player_id, athlete_id, team, position, champion_name, tick`
- hp: `hp, max_hp, hp_ratio_percent, is_hp_below_percent(p)`
- intents: `get_run_away_input`, `get_run_away_without_skill_input`, `get_recall_input`, `is_safe_to_recall`
- validation: `is_valid_input(&Input)` — call this when you build an `Input` by hand.

`Input` (build manually for richer behavior): `Move{x,y} | Return | Attack{target} | Skill{target} | Skill2{target} | Ult{target}`, where `InputTarget = Target{target_id} | Dir{dir_x,dir_y} | Pos{x,y} | None`.

**VERIFIED constraint (compiler-checked against the 0.4.6 rlib, not just docs):**
`PlayerAiContext` in API 0.7 exposes ONLY: identity (`player_id`/`athlete_id`/`team`/`position`/`champion_name`), own-hp (`hp`/`max_hp`/`hp_ratio_percent`/`is_hp_below_percent`), `tick`, the prebuilt intents (`get_run_away_input`/`get_run_away_without_skill_input`/`get_recall_input`/`is_safe_to_recall`), and `is_valid_input`.
It does **NOT** have `game_ctx`, `get_entity`, `entity_at`, `entity_count`, `enemies`, `visible_enemies`, `enemy_champions`, `target`, `current_target`, or any `get_attack_input`/`get_skill_input`/`move_input`/`base_input` (probed — methods do not exist, no similar names). 
Consequence: from `think()` you can build any `Input` (Move/Attack/Skill/Skill2/Ult/Return), but you have **no way to discover enemy/ally ids or positions**, so manually targeting a specific entity is not feasible from the combat-AI hook alone in 0.4.6. Rich battlefield reads (`GameCtx`/`EntityRef`/`PlayerRef`: `get_entity`, `entity_at`, `distance_sq`, `is_visible`, `EntityRef::{hp,stat,team,shield,buff_at,pos}`, …) ARE real and public — but only reachable from `&GameCtx` callbacks (`ModEffectType::apply`, `ModPassive::on_*`, item callbacks), i.e. inside custom champions/items, not inside `ModPlayerInputAi::think`.

Other registrable hooks (not our focus, but available): `ModDraftScoreHook` (ban/pick scoring), `ModChampionInfo` (+`ModAction`/`ModEffect`/`ModEffectType`/`ModPassive`), `ModItemInfo`, `ModExtension` (client UI/scene lifecycle, `ClientData`), `ModServerExtension` (authoritative server-side), runtime services between mods.

## Build → deploy → test loop

Building requires the **Mod SDK** (prebuilt `mod_api` injected by the SDK build script). Two build paths:

1. **Via the SDK script** (recommended; guarantees ABI match). Run from **PowerShell**, and
   either `cd` into the mod folder first or set the default toolchain — rustup resolves
   `rust-toolchain.toml` from the **current working dir**, not the manifest path, so building
   from elsewhere silently uses the wrong nightly and fails E0514:
   ```powershell
   $env:PATH="$env:USERPROFILE\.cargo\bin;$env:PATH"
   rustup default nightly-2026-05-29          # one-time; the ABI-correct toolchain
   cd C:\tmp\tfm2-ai-mod\tfm2_combat_ai
   & C:\tmp\tfm2-ai-mod\sdk\mod-sdk\build_mod_cargo.ps1 -Project . -SdkDir C:\tmp\tfm2-ai-mod\sdk\mod-sdk
   ```
   Produces `tfm2_combat_ai.dll` in the mod folder. **Verified working**: exports
   `tfm2_mod_entry` + `tfm2_mod_api_version` (checked with `dumpbin /EXPORTS`).

2. **Via TFM2ModUploader.exe** — pick the mod folder, check *Build native Rust code before uploading*, then upload. It builds with the SDK's `mod_api` and skips `src/`, `target/`, `Cargo.toml`, `Cargo.lock` from the upload (source is not published).

**Deploy for local testing:** copy or symlink `tfm2_combat_ai\` into `…\Teamfight Manager2\mods\tfm2_combat_ai\` (must contain the built `tfm2_combat_ai.dll` + `mod.mod_info`). Enable in the title-screen **Mods** menu, then **restart the game** (DLLs load at startup). Read the title-screen diagnostics popup if load fails.

> Native DLLs run **inside the game process** — the game warns players that code mods are a trust/security surface. Keep `think()` small, deterministic, and panic-safe.

## Local dev against mod-api (editor / cargo check)

Per the docs, do **not** put `mod-api` in `[dependencies]` for the SDK/upload workflow. For `cargo check` / rust-analyzer to resolve `use mod_api::*;`, point at the SDK's `mod-api` (path dep or `[patch]`) **only locally** — TBD after inspecting the extracted SDK (`sdk\mod-sdk\{deps,native,template}\`). Check `template\Cargo.toml` for the exact intended setup, and pin the toolchain from `rust-toolchain.toml` / `toolchain_version.txt`. Do not commit a machine-specific path dep into the upload package.

## Conventions for this repo

- Only call `mod_api` items documented for **API 0.7** / present in the extracted SDK. Don't invent methods; verify against `native-mod-api-reference.md` and the SDK source.
- All units are integers (`u64`/`usize`); positions are large integer coordinates (ranges like `65000`). HP/stat math is integer.
- After any **game update**, the SDK may change → rebuild the DLL and re-test; bump `mod.mod_info` `base` version if compatibility shifts.
- Verify before claiming success: a real build (DLL produced) + in-game load (no diagnostics error) is the bar, not "it compiles in theory".

## Status / next steps

- [x] Recon: engine, mod system, code-mod API surface, build/upload flow, versions.
- [x] Scaffolding: `tfm2_combat_ai` package (Cargo.toml, src/lib.rs baseline, mod.mod_info, rust-toolchain.toml), git init.
- [x] SDK 0.4.6 downloaded/extracted → `sdk\mod-sdk\`. Rust nightly-2026-05-29 installed; baseline **compiles** (ABI verified, metadata-only).
- [x] Verified the PlayerAiContext API surface against the rlib (see VERIFIED constraint above).
- [x] MSVC C++ build tools 14.44.35207 installed; rustup default set to nightly-2026-05-29.
- [x] **First full DLL build succeeded** — `tfm2_combat_ai.dll` (562 KB), exports verified.
- [ ] Deploy to `…\Teamfight Manager2\mods\tfm2_combat_ai\` (DLL + mod.mod_info), enable in Mods menu, restart, verify load (no title-screen diagnostics error).
- [ ] **Switch to the custom-champion path** (user's choice): refactor toward `ModChampionInfo` + `ModPassive`/`ModEffectType` (GameCtx battlefield reads). Pick the concrete champion/behavior first. The `ModPlayerInputAi` baseline stays only as the build-validation artifact.
