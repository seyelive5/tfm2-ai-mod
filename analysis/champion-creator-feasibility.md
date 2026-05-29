# Feasibility & Architecture — In-Game Champion Creator Mod

Goal (user's real intent): a mod that lets the **end player create new champions inside the game** —
random pixel-art combination, upload your own portrait/sprite, pick a role (Tank / AD / ranged or melee
carry / mage / support…), set stats, choose skills, etc. Not a single hand-coded champion.

## The key enabler

Champions can be defined as **`.data_champion` JSON files** (no Rust, no recompile) — see
`data-champion.md` / `data-champion-schema/`. The game loads every `*.data_champion` under a mod folder
at startup. Skills are composed from a large prebuilt **DataEffectDef** library (damage/heal/shield, CC,
movement, projectiles, areas, buffs, branching). Sprites can be a **static PNG** (one-frame + auto
fallback anims), a **reused base champion sprite** (`sprite: asset/base/aseprite_resources/champions/<x>`),
or a **PNG + `#anim`/`#data` sheet** you author.

➡️ **A champion creator = a generator that turns UI choices into `.data_champion` JSON + i18n + sprite
assets, written into a mod folder; champions appear after a restart.** No end-user coding.

## Architecture

```
[In-game Creator UI]  (native ModExtension + GameUI)
   pick role/stats/skills, randomize or upload sprite, name it
        │  on "Save champion"
        ▼
[Generator]  -> writes into a USER-WRITABLE mod folder:
   champion/<id>.data_champion          (stats/growth/actions/effects from templates)
   text/champion.i18n        (merge)    (name + skill descriptions)
   champions/<id>.png / #sheet+#data    (uploaded or composed sprite)
   icons/<id>_skill*.png                (skill icons; can default)
   mod.override_info                    (merge i18n into base champion text)
        │  restart game (assets load at startup)
        ▼
[Game loads champions]  -> new champions in ban/pick pool, playable, AI uses them
```

Two viable packagings:
- **(P) One mod**: the creator DLL + the generated data live in the SAME mod folder (folder must be writable → install as a *local* mod, not a read-only Workshop copy).
- **(Q) Split**: creator DLL in one mod; it writes generated champions into a separate *data-pack* folder the user can also share to Workshop. Cleaner sharing, slightly more plumbing.

## Per-pillar feasibility

| Pillar | Feasible? | How / notes |
|---|---|---|
| **Role presets** (Tank/AD-carry/Mage/Assassin/Support…) | ✅ | each role = a template: `category`+`tags`+stat/growth ranges + default skill set. Pure JSON emission. |
| **Stat selection** | ✅ | UI sliders → `stat`/`growth` JSON. Optionally a point-budget to balance. |
| **Skill selection** | ✅ (templated) | offer a menu of skill *templates* (e.g. "skillshot nuke", "dash", "self-buff", "AoE stun", "heal") each = a parameterized `DataEffectDef`. User tweaks numbers. **Not** free scripting — bounded by the DataEffectDef catalogue. |
| **Upload your own portrait/sprite** | ✅ (static) | write uploaded PNG as `champions/<id>.png`, set `sprite: asset/<mod>/champions/<id>` with no `anim_prefix` → one-frame sprite + auto fallback anims (idle/attack/skill/…). Animated upload needs a `#anim` sheet (harder). |
| **Random pixel-art combination** | 🟡 | ship a library of part layers (body/head/weapon/palette); compose into a PNG atlas + `.fanim`/`.sprite_sheet` and write it. Needs an authored part library + a compositor. Simpler v1: random recolor/tint of reused base sprites. |
| **Skill icons** | ✅ | generate/scale a PNG per skill, or reuse base icons; `skill_icons` paths. |
| **Names & descriptions** | ✅ | emit `text/champion.i18n` (en/ko) + `mod.override_info` merge. |
| **Creator UI in-game** | 🟡 (big) | `ModExtension` gets `GameUI` (egui-family) + `Scene` + `Assets`. Building dropdowns/sliders/preview is real UI engineering; substantial but possible. A config-file-driven generator can precede the polished UI. |
| **Image file picking** | 🟡 | the game already supports custom athlete faces / team logos (`ChangeAthleteFace face_image_data`, `custom_logos`); a native file dialog (`rfd` crate, already in SDK deps) can pick a PNG. Verify usable from the game process. |
| **AI plays the created champion** | ✅ | the base planner uses any champion via casting_target/range + the learned draft/item models; created champions enter the pool like any data champion. |

## Hard constraints / risks (be honest)

1. **Restart to load** — new champions register at startup, not live mid-match. The creation flow is "build → restart → play." (Acceptable for an editor; not live spawning.)
2. **Writable folder** — generated files must land where the loader scans (a mod folder). The game-install `mods/` is under Program Files (write needs elevation / may be read-only for Workshop items). Plan: install the creator as a **local** mod folder in a writable path, or write to a dedicated data-pack folder. *Verify where the loader scans and whether the DLL can write there.* (open Q1)
3. **Skills are templated, not scripted** — end users compose from the DataEffectDef catalogue; they can't write arbitrary logic. Rich but bounded. (Native `ModEffectType` logic is author-side only.)
4. **Sprite quality** — easy path (static PNG / recolor) looks basic; full animated custom champions need real art + `#anim` sheets. Random-combine needs a part library we author.
5. **Save / multiplayer compatibility** — generated champions change the mod content; saves track `saved_mod_signatures` / `mod_session_snapshot`. Adding champions to an existing save may trigger the mod-compat warning; multiplayer requires both sides to have identical generated content. (open Q2)
6. **UI runtime asset registration** — showing an uploaded sprite as a live preview *before* restart needs runtime asset registration via `Assets`; otherwise preview is limited until reload. (open Q3)
7. **Balance/validation** — must validate generated JSON (ranges, required fields) so a bad champion doesn't break load (the diagnostics popup catches JSON errors, but we should pre-validate in the generator).

## Open questions to verify in Phase 1 (cheap probes)
- Q1: Can the mod DLL write files into a loader-scanned folder at runtime, and where (local mod folder vs %APPDATA% vs Program Files)? Does the loader scan any user-writable dir?
- Q2: Does adding a generated `.data_champion` to an existing mod change its signature enough to trip the save-compat warning? Is there a clean per-save flow?
- Q3: Can `Assets` register a newly written sprite at runtime for in-creator preview, or is restart required to see it?
- Q4: `GameUI`/`Node`/`NodeTemplate` capability depth — can we build dropdowns/sliders/scroll lists/image preview? (inspect `ui/layout/*` base layouts + the re-exported UI types)
- Q5: Is there a usable in-process file dialog for upload (rfd), or do we reuse the engine's face/logo upload path?

## Phased roadmap (build incrementally, prove each pillar)

- **Phase 0 — Generation pipeline MVP (no UI).** A tiny generator (driven by a JSON/TOML "spec" file or a couple of constants) that emits a valid `.data_champion` + i18n + reused base sprite into a writable mod folder. Restart → champion appears in pool & is playable. *Proves: generate → load → play. De-risks Q1/Q2.* (Reuses everything we learned building the Executioner.)
- **Phase 1 — Role/stat/skill templates.** A clean template library (roles → stat presets; skill menu → DataEffectDef params) + a validator. Still spec-file driven. Output: pick role + numbers → balanced champion JSON.
- **Phase 2 — In-game Creator UI.** `ModExtension` screen: role dropdown, stat sliders, skill pickers, name field, "Save & queue for next restart." Writes via the Phase-1 generator.
- **Phase 3 — Sprite: upload + random combine.** Static PNG upload first (Q5), then a part-library random compositor; runtime preview if Q3 allows.
- **Phase 4 — Polish & share.** Validation, balance budget, icons, i18n, Workshop packaging of created champions.

## Relationship to existing work
The Executioner (native champion) and our verified effect/stat/buff knowledge directly feed the **template
library** (Phase 1): each skill template is a parameterized version of effects we already validated. The
build pipeline (toolchain, SDK, DLL) is reused for the Creator's `ModExtension` DLL.
