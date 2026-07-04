# REVIEW-FIXES — 상세 구현 스펙 (2026-07-04 max 리뷰 후속, 신규 세션 핸드오프용)

대상 커밋: `9a28808` (authored patch buff/nerf). 리뷰 결과 **15건 보고** (11 CONFIRMED / 4 PLAUSIBLE), 3건 기각.
이 문서는 **대화 컨텍스트 없는 새 세션이 그대로 실행**할 수 있도록 각 작업을 현재코드→변경→이유→검증→함정 수준으로 기술한다. 라인 번호는 2026-07-04 워킹트리 기준(±수 줄 오차 가능 — 항상 주변 코드로 재확인).

---

## A. 세션 부트스트랩 (먼저 읽기)

### A-1. 리포 배선 팩트 (오해 금지)
- **`tfm2_db_export/src/recommender.rs` = 추천 파이프라인 본체** (v3.0, in-process): `ChgInfo`(:337), `build_tables`(:347), `patch_current`(:63), `compute_pos`(:154), `compute_roles`(:194), `compute_banrate`(:234), `compute_lane_dom`(:289), `Board.enemy_intent`(:419), 코멘터리 렌더(:758-761), 코멘터리 캐시키(:531).
- recommender.rs의 `#[path = "../../draft-overlay/src/engine.rs"]`(:11)가 **engine.rs를 서브모듈로 include** — engine.rs는 Tables/스코어링 소유: `chg_mark`(:345), `chg_nudge`(:360), 가중치 상수들. engine.rs는 draft-overlay 크레이트에도 공유되지만 **라이브 소비자는 tfm2_db_export 뿐** (v3.0 헤더 lib.rs:3, :11-12: 외부 오버레이/파일 IPC 폐기).
- `tfm2_db_export/src/lib.rs` = 모드 본체: `authored_chg`(:455-498), `champ_changes_update`(:~330-445), 리빌드 블록(:1400-1494), `observe_score`/AI 통계(:1240-1330), bp_export(:~700-900), `atomic_write`(:51), `THROTTLE_MS=8000`(:49), `vkey`(:275-277).
- `tfm2_draft_ai/`는 **은퇴한 구 모드**(자체 Tables 사본) — 건드리지 말 것.

### A-2. 빌드/배포 의식 (2026-07-04 이후 상태)
```powershell
& C:\tmp\tfm2-ai-mod\build_db_export.ps1        # 핀: nightly-2026-05-24 / 23a3312d9 (SDK 0.4.14)
# 산출: C:\tmp\tfm2-ai-mod\tfm2_db_export\tfm2_db_export.dll (당일 빌드 9.6초)
Copy-Item C:\tmp\tfm2-ai-mod\tfm2_db_export\tfm2_db_export.dll `
  "C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2\mods\tfm2_db_export\" -Force
# (구 06-20 DLL 백업이 같은 폴더 tfm2_db_export.dll.bak-0620 로 존재)
```
- P2-1 완료 전까지 **게임 패치마다** 3개 스크립트의 `$Toolchain`/`$RustcHash`를 SDK `C:\tmp\mod-sdk\toolchain_version.txt`(`rustc …(HASH DATE)`; 채널=DATE+1)에 맞춰 수동 갱신해야 함.
- typecheck만 원하면: 빌드가 9.6초라 그냥 빌드해도 됨.
- 런타임 검증 파일: `%LOCALAPPDATA%\tfm2-overlay\probe\` — `chg_authored.txt`, `champ_changes.tsv`, `defs_snapshot.txt`, `champion_patch_statistics.tsv`, `_stamp.txt`. 원시 뉴스 덤프: `C:\tmp\patch_notes_dump.txt` (teams=120, patchnotes=7, distinct_versions=7 — **노트는 버전당 단일 인스턴스**, 팀별 사본 아님).

### A-3. 금지/주의
- `mod-api`를 `[dependencies]`에 넣지 말 것 (CLAUDE.md §3; 스크립트가 rlib 주입).
- deny/steal(`ENEMY_VAL`) 주석 블록들을 부활시키지 말 것 (근거: 훅은 배경 리그 심에서만 발화 — CLAUDE.md §2c; engine.rs:783-784는 재작성돼 언커멘트만으론 픽 측이 복원 안 됨 → P2-4).
- 버전 문자열 네임스페이스는 인게임 커리어 버전 `YYYY.M.P`(예: 2026.3.1) — 게임 제품 버전(0.4.14)과 **다른 것**. 혼동 금지.

---

## 0. 완료됨 (2026-07-04)

- [x] **#1** stale 핀: 3개 스크립트 `nightly-2026-05-24`/`23a3312d9`로 갱신 (게임 exe 당일 0.4.14 확인)
- [x] **#3** `$ErrorActionPreference="Stop"` 복원 (가짜 "Build successful" 회귀 — 전 경로 empirically 재현 후 수정)
- [x] **#14** `RUSTUP_TOOLCHAIN` 누수: `$prevToolchain` 보존 + finally 원복 (실증: 종료 후 `[]`)
- [x] `nightly-2026-05-24` 설치, 빌드 종단 검증, 게임 배포(+백업)
- [x] 위 수정 커밋: `8ad80c5` (Phase 1 커밋과 분리됨)

---

## Phase 1 — 추천기 진실성 (전부 `tfm2_db_export` 안, 한 세션 분량)

### P1-1 (#2 CONFIRMED) 디버그 덤프 게이팅 — `lib.rs:1447-1448`
**현재:** 리빌드 블록이 매 발화마다 프레임 스레드(pre_update)에서:
```rust
atomic_write(&format!("{}/solo_rank_matches.debug.txt", out), &format!("{:#?}", db.solo_rank_matches));
atomic_write(&format!("{}/match_replays.debug.txt", out), &format!("{:#?}", db.match_replays));
```
라이브 파일 110.5MB + 15.3MB — `{:#?}` 문자열 할당 + 동기 fs write+rename. 게이트(:1415-1428): `first`(LAST_SIG==usize::MAX)면 **스로틀 우회**, 아니면 sig 변화 + 누적 8000ms. → 로드 후 첫 InGame 프레임 수 초 프리즈, 시즌 심 중 ~8초마다 히치, 30분 ~25-30GB 기록.
**소비자 없음 확인됨:** 라이브 파이프라인은 `replay_rows(&db)`로 db 직접 읽음; 파일 소비자는 은퇴/오프라인 툴(draft-weights-gen, backtest, inject.rs)뿐, 산출물 06-11 이후 stale.
**변경:** 두 줄을 마커 게이트로 감싼다:
```rust
if std::path::Path::new(&format!("{}/DEBUG_DUMPS", out)).exists() {
    atomic_write(... solo ...);
    atomic_write(... replays ...);
}
```
(파일 삭제가 아니라 게이트인 이유: 리포트 툴을 다시 쓸 때 마커 파일 하나로 재활성.)
**검증:** 마커 없이 커리어 로드 → 첫 프레임 프리즈 소멸, 두 .debug.txt 미갱신(타임스탬프), `_stamp.txt`/`chg_authored.txt`는 갱신됨. 마커 생성 후 → 두 파일 갱신 재개.
**함정:** `champ_changes.tsv`/`champion_patch_statistics.tsv`/`pool.txt` 기록은 게이트 **밖**에 둘 것(작고, 스냅샷/리포트 계약 있음). `first` 스로틀 우회 자체는 이 게이트가 실질 해결하므로 별도 수정 불필요.

### P1-2 (#4 CONFIRMED) enemy_intent 컷 — 렌더 1곳 + doc 1곳 (최소침습)
**근거:** 플레이어 밴픽 중 상대 AI는 draft-score 훅을 **호출하지 않는다**(CLAUDE.md §2c, 2026-06-11 재확인). AI_EVAL(:1257)은 모든 훅 호출(=배경 리그 심)로 채워지고, 유일한 게이트는 120초 신선도(lib.rs:784) — 매치 귀속 가드 없음. 즉 "상대 AI 평가 상위(다음 픽/밴 유력)" 줄은 남의 심 심의를 상대 의도로 표시한다. deny/steal을 비활성화한 것과 동일 근거.
**변경 (옵션 A — 권장, 렌더 지점만):**
1. `recommender.rs:758-761` 블록 삭제:
```rust
    if !board.enemy_intent.is_empty() {
        let kos: Vec<String> = board.enemy_intent.iter().map(|id| engine::ko_name(id)).collect();
        parts.push(format!("상대 AI 평가 상위(다음 픽/밴 유력): {}.", kos.join(" · ")));
    }
```
2. `lib.rs:1254-1256` AI_EVAL doc의 거짓 문장 교체:
```rust
/// latest AI evaluation block: (board signature, unix secs, [(cand, score, phase)]).
/// NOTE(2026-07-04 review): the hook fires only for BACKGROUND league sims — the
/// player's opponent never invokes it (CLAUDE.md §2c), so this is NOT the enemy's
/// deliberation; do not attribute it to the current match. Kept only as raw feed.
```
3. `lib.rs:775-777` enemy_intent 빌드 주석에도 같은 한 줄 추가 (빌드 코드는 유지 — Board 필드/캐시키(:531) 연쇄 수정 회피. 렌더가 없으므로 사용자 노출 0).
**deny/steal의 9-영역 실수 반복 금지** — 자르는 곳은 렌더 한 곳이다.
**검증:** 빌드 후 드래프트 진입 → 코멘터리에 해당 줄 부재. `grep "상대 AI 평가" recommender.rs` = 0건.
**함정:** `CHAMP_ORDER`는 AI_PRIOR 경로(:873-879)도 소비 — 건드리지 말 것. `AI_EVAL` static/feed도 옵션 A에서는 유지(무해).

### P1-3 `authored_chg` 외과수술 번들 — `lib.rs:455-498` + 호출부 `:1457-1486`
현재 함수 전문은 lib.rs:455-498 (이 문서 A-1 참조). 변경은 아래 a→g 순서로 한 번에.

**(a) (#6 CONFIRMED) anchor를 노트 이력이 아닌 stats 키에서 도출** — 현재 `:483-486`:
```rust
let mut iter = by_ver.iter().rev();
let (_lk, (_lver, latest)) = iter.next()?;
let anchor = iter.next().map(|(_, (v, _))| v.clone());
```
문제: anchor=두 번째 노트 버전. (i) 노트가 1개뿐(커리어 첫 밸런스 패치 — **결정론적**)이면 anchor=None → `compute_banrate:266`/`compute_lane_dom:297`의 `if let Some(pv) = chg_prev` 가 stale-drop 전체 스킵 (`patch_current:91`만 None 폴백 보유). (ii) 두 노트 사이에 노트 없는 stats 버전(예: 2026.2.1)이 끼면 anchor가 너무 老 → 그 버전의 pre-change 행이 `vkey(row) <= vkey(anchor)` 필터를 통과.
교체:
```rust
let mut iter = by_ver.iter().rev();
let (_lk, (lver, latest)) = iter.next()?;          // lver = latest note version (String)
// anchor = latest-note 미만의 최대 stats 버전 (노트 이력이 아니라 실제 플레이된 버전 축)
let anchor = db.champion_patch_statistics.keys()
    .filter(|v| vkey(v) < vkey(lver))
    .max_by_key(|v| vkey(v.as_str()))
    .cloned();
```
이유: 소비자(`patch_current`/`compute_banrate`/`compute_lane_dom`)의 의미론은 "현 패치 **직전에 플레이된** stats 버전" — TSV 경로의 `s_ver`(lib.rs:443 "last version played on the OLD defs")와 동일 축. (i)(ii) 동시 해소.

**(b) (#10 PLAUSIBLE) currency 가드** — (a) 직후에 추가:
```rust
if let Some(smax) = db.champion_patch_statistics.keys().max_by_key(|v| vkey(v.as_str())) {
    if vkey(smax) > vkey(lver) {
        return None; // 노트가 stats보다 뒤처짐(프루닝/배달 랙) → 자기교정하는 defs-diff 폴백으로
    }
}
```
주의 방향: 노트가 stats보다 **앞서는** 것(새 패치 노트 도착, 아직 그 버전 경기 0)은 정상 — 차단하지 말 것. (a)의 anchor 도출이 그 경우 자연히 stats 최신( < 노트 )을 anchor로 준다.

**(c) (#11 PLAUSIBLE) 빈 노트 = "변경 없음"을 폴백으로 덮지 않기** — 현재 `:494-496`:
```rust
if dirs.is_empty() { return None; }
```
교체: 삭제하고 `Some(ChgInfo { anchor, dirs })` 그대로 반환 (dirs가 비어도). 게임이 "이 패치 챔프 변경 0"이라 말한 것이므로 changed=∅가 **정답**. None을 주면 폴백이 (스냅샷 intact 시) 이전 패치 변경을 "이번 패치"로 재렌더(:406-413)하거나 (fp 리시드 후) header-only TSV로 전 패치 stale-drop까지 소실.

**(d) (#12 PLAUSIBLE) 빈 변경Vec champ 스킵** — `:467` 루프 첫 줄에:
```rust
for (champ, changes) in champion_patch_data {
    if changes.is_empty() { continue; }   // (0,0)→'~' 오분류(부당 스탯리셋+가짜 ⚡) 차단
    ...
```

**(e) (#5 CONFIRMED) 'R' 리워크 채널 복원 — 방향은 authored, 리워크 감지는 defs-diff** — 호출부 `:1457-1469` Some 암:
`build_tables:349-350`은 `reworked`를 `d=='R'`로만 만들고, `compute_pos:172`/`compute_roles:220`이 그것으로 舊킷 레인/역할 이력을 불신한다. authored는 '+'/'-'/'~'만 내므로 현재 preferred 경로에서 이 기계가 죽어 있다. 반면 `chg_tsv`는 이미 매 리빌드 무조건 계산되고(:1443) `change_dir`(:299,:319)이 'R'을 내며, **스냅샷 advance 후에도 persist된 `s_changes` 재렌더(:406-413)로 'R'이 보존**된다.
Some 암에서 `ci` 반환 직전에 승격 머지:
```rust
Some(mut ci) => {
    // defs-diff가 'R'(구조 변경)로 판정한 champ는 방향 문자보다 리워크가 우선
    let rework: std::collections::HashSet<&str> = chg_tsv.lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("champion\t"))
        .filter_map(|l| { let mut f = l.split('\t');
            let c = f.next()?; if f.nth(1)? .starts_with('R') { Some(c) } else { None } })
        .collect();
    for (c, d) in ci.dirs.iter_mut() {
        if rework.contains(c.as_str()) { *d = 'R'; }
    }
    ...기존 chg_authored.txt 기록...
    ci
}
```
함정: (i) authored dirs에 **없는** champ가 diff에서 'R'이어도 추가하지 않는다 — 노트가 champ 집합의 권위(diff-only champ는 fp/스냅샷 잡음 가능성). (ii) fp 리시드 직후엔 s_changes가 비어 'R' 소실 — 알려진 best-effort 한계로 커밋 메시지에 명기. (iii) 두 소스의 champ 키는 동일 ascii id 네임스페이스(라이브 덤프로 확인됨).

**(f) (#7 CONFIRMED) chg_authored.txt 정직화** — 현재 Some 암에서만 기록(:1465-1468), None이면 이전 커리어의 `source=authored` 파일이 잔존; dirs는 HashMap 순회라 매번 셔플.
변경: (i) `authored_chg` 반환 직전 `dirs.sort();` (champ명 정렬 — 소비자는 HashSet/HashMap이라 무영향, 파일만 안정화). (ii) 기록을 match **뒤**로 이동해 양 분기 공통 1회:
```rust
let (chg_info, chg_src) = match authored_chg(&db) {
    Some(ci) => (ci, "authored"),
    None => ( /* 기존 TSV 파싱 */ , "fallback"),
};
atomic_write(&format!("{}/chg_authored.txt", out),
    &format!("source={} anchor={:?}\n{}", chg_src, chg_info.anchor,
        chg_info.dirs.iter().map(|(c,d)| format!("{}\t{}", c, d)).collect::<Vec<_>>().join("\n")));
```
**(g) (스윕 CONFIRMED) 거짓 주석 수정** — `:457-458`:
```rust
    // version-key -> (version-string, champ -> (buffs, nerfs)). NOTE: patch notes are
    // SINGLE-instance per version across all inboxes (dump 2026-07-04: 120 teams,
    // 1313 news, 7 notes / 7 versions) — the is_empty() guard is defensive only.
```

**P1-3 검증:**
1. 빌드 → 인게임 리빌드 1회 유발(경기 진행/시뮬) → `chg_authored.txt`: `source=authored`, anchor가 stats 버전축(`champion_patch_statistics.tsv` 1열에 존재하는 버전), dirs 알파벳 정렬.
2. 새 커리어(노트 0)에서 `source=fallback` 기록 확인.
3. 'R' 승격은 다음 리워크 패치에서 자연 검증 (`champ_changes.tsv`에 R행 + `chg_authored.txt` 해당 champ 'R').
4. 회귀 확인: 기존 관측 상태(노트 7개, 최신 2026.3.1)에서 dirs 9종 유지되는지.

### P1-4 마무리
- [ ] 재빌드 + 게임 배포(A-2) + 인게임 스모크: 로드 프리즈 소멸 / "상대 AI 평가" 줄 부재 / chg ▲▼⚡ 정상 / bp_live 갱신
- [ ] 커밋 (P1 전체 1커밋 권장; 메시지에 finding# 매핑)

---

## Phase 2 — 패치 내성 (0.4.15 대비)

### P2-1 (#13 CONFIRMED) 빌드 스크립트 통합 + 핀 자동 도출
현재 3개 스크립트가 ~30줄 코어(param/Pick/4-rlib+throw/$flags/[char]31/try-finally) verbatim 3중복, 차이는 `$Crate` 기본값·DLL명·`--emit=metadata`뿐. 핀 2개×3파일=패치당 6군데 lockstep — 0.4.14 SDK 드롭에서 실제 전부 누락됐던 사고가 #1.
- [ ] `build_mod.ps1 -Crate <path> [-CheckOnly] [-Sdk C:\tmp\mod-sdk] [-Toolchain <override>] [-RustcHash <override>]` 단일화
  - DLL명 = `(Split-Path $Crate -Leaf) + ".dll"` (하이픈→언더스코어 치환은 SDK build_mod_cargo.ps1:61 참조)
  - 핀 자동 도출:
    ```powershell
    $tv = Get-Content (Join-Path $Sdk "toolchain_version.txt") -Raw   # "rustc 1.98.0-nightly (23a3312d9 2026-05-23)"
    if ($tv -match '\(([0-9a-f]+) (\d{4}-\d{2}-\d{2})\)') {
        if (-not $RustcHash)  { $RustcHash = $Matches[1] }
        if (-not $Toolchain)  { $Toolchain = "nightly-" + ([datetime]$Matches[2]).AddDays(1).ToString("yyyy-MM-dd") }
    }
    ```
    교차검증: SDK `rust-toolchain.toml`의 `channel = "nightly-2026-05-24"`가 규칙(날짜+1)을 실증.
  - `-CheckOnly`: `--emit=metadata` 추가 + DLL 복사 생략. cargo 실패 시 exit 전파는 빌드 모드에만.
  - 오늘 넣은 세이프티 유지: `$ErrorActionPreference="Stop"`, `$prevToolchain` finally 원복, `$LASTEXITCODE` 체크.
  - 툴체인 미설치면 명시 에러+설치 명령 출력(`rustup toolchain install <ch> --profile minimal`).
- [ ] 기존 3개 스크립트: 삭제하거나 2줄 위임 셔틀로 (`& $PSScriptRoot\build_mod.ps1 -Crate ... @args`)
- [ ] 검증: tfm2_db_export 빌드 해시가 기존 산출물과 동작 동일(재빌드 후 인게임), tfm2_patch_probe `-CheckOnly`가 P2-2 이후 초록.
- 참고: SDK 자체 `build_mod_cargo.ps1`은 무필터 `Select-Object -First 1` glob — 혼합 deps에서 최고(最古) 세대를 잡는다. 다른 모드(tfm2_combat_ai 등) 빌드 시 이 스크립트 대신 통합 스크립트 사용 권장.

### P2-2 (#9 CONFIRMED) tfm2_patch_probe → 패치 카나리 재목적화
현재: 컴파일 불가 유물 — `:10` 의도적 E0308(`let _: () = &db.teams;`) + `:15-16` **비의도** 튜플 순회(`for team in db.teams.iter()` 후 `&team.news` — HashMap iter는 `(&usize, &Team)`) = "경로가 컴파일되나"에 항상 아니오라고 답하는 false-negative 계기. `declare_mod!` 없음(로드 불가). mod.mod_info는 이 커밋이 기각한 champion_action_patch_state 브릿지를 서술.
- [ ] `:9-11` `client_direct` fn 삭제 (답은 `authored_chg`로 출하됨)
- [ ] `:15` → `for (_id, team) in db.teams.iter() {` (출하 코드 lib.rs:460과 동일 패턴)
- [ ] `mod.mod_info` description 교체: `"컴파일 카나리: 게임 패치 후 client-direct PatchNote 경로(db.teams → Team.news → NewsType::PatchNote{champion_patch_data,is_buff})가 새 SDK에서 여전히 컴파일되는지 -CheckOnly로 확인. 로드용 모드 아님(declare_mod 없음)."`
- [ ] `build_patch_probe.ps1` 삭제 (로드 가능 모드가 아니므로 DLL 빌드는 무의미; check는 P2-1 `-CheckOnly`로)
- [ ] lib.rs 헤더 주석을 카나리 용도로 갱신
- 검증: `build_mod.ps1 -Crate ...\tfm2_patch_probe -CheckOnly` → 에러 0. 패치 의식에 편입: SDK 갱신 후 카나리 먼저 → 초록이면 db_export 빌드.

### P2-3 (#8 CONFIRMED) CLAUDE.md 정합화 — **P2-1/P2-2 이후에** (문서는 마지막)
현재 모순: §0:19 "Game + SDK version: **0.4.6**", §0:20+§3:210-211+§5:317 "nightly-2026-05-29 (cced03bfd) 필수" ↔ 현행 0.4.14/`nightly-2026-05-24`/`23a3312d9`(그리고 SDK마다 변동). §4:255 끝 "Bridge proven too … (single-player server ext fires; verified)"는 커밋된 아티팩트 없음(probe는 round-8 클라 프로브만 커밋됨).
- [ ] §0: 버전/툴체인을 하드코딩에서 **절차 서술**로 전환 — "게임+SDK는 패치마다 갱신; 정확 nightly는 SDK `toolchain_version.txt`에서 도출(채널=rustc날짜+1), 빌드는 `build_mod.ps1`이 자동 적용. 2026-07-04 기준 0.4.14 / nightly-2026-05-24 (23a3312d9)."
- [ ] §3 명령 예시를 build_mod.ps1 기준으로 교체; §5 툴체인 gotcha를 "env var 우선순위 + 자동도출" 서술로
- [ ] §4:255 "Bridge proven too" → "bridge는 당시 세션에서 관찰됐으나 코드는 커밋되지 않음(UNVERIFIABLE-FROM-TREE); 현행 경로는 클라 직접 read"로 완화
- [ ] §7 체크리스트에 P1/P2 완료 반영
- 함정: CLAUDE.md는 세션 지시문 — 고치기 전까지 새 세션이 §3을 따르면 E0514. **이 문서(A-2)가 그동안의 진실.**

### P2-4 잔손질 (컷 3건 수용 + 저우선)
- [ ] (#15 CONFIRMED) `engine.rs:783-784` 리바이벌 트랩: 재작성된 두 줄에 주석 조각 복원 —
  `let total = base + syn + cnt + comp + fam - dup; // (deny 복원 시: + deny)` / dominant 목록에 `/* ("디나이", deny) */`
- [ ] PLAN.md:141 "의도 표시/디나이 — 이미 배포" → "2026-06-20 비활성(훅 무귀속 — CLAUDE.md §2c); P1-2로 intent 표시도 제거" 반영
- [ ] (C19 PLAUSIBLE) 통합 스크립트 Pick에서 Select-String 에러를 삼키지 말고 잠금/읽기실패를 별도 경고로 표면화 (해시 불일치 throw와 구분)
- [ ] (선택, 리포 외부) `c:\tmp\tfm2-overlay\HOW_TO_USE.md` inject-overlay 안내에 은퇴 명시

---

## 재론 금지 (기각 확정 — 증거 요지)

| 후보 | 판정 | 증거 |
|---|---|---|
| 동일-버전 중복 노트 경쟁/비결정성 | REFUTED | `C:\tmp\patch_notes_dump.txt`: teams=120 news_total=1313 **patchnotes=7 distinct_versions=7** — 버전당 단일 인스턴스. `is_empty()` 가드는 미발화 방어구 |
| vkey 비숫자 세그먼트 붕괴 | REFUTED | 신규 노출 아님(선재: recommender.rs:74/242, lib.rs:366) + 관측 8버전 전부 `YYYY.M.P` 숫자형 + "…b" 접미는 sort-below가 아니라 동일키 충돌 |
| 오버레이 vs in-mod chg split-brain | REFUTED | v3.0 파이프라인에 외부 오버레이/파일 IPC 소비자 없음(lib.rs:3,:11-12; 게임 로그 `active=[base, tfm2_db_export]`) |
| champ_changes_update 무조건 실행 낭비 | 유지 타당 | CHAMP_ORDER/스냅샷 부수효과 소유; 비용 ~0.25MB = 인접 덤프의 <1% — 진짜 비용은 P1-1이 해결. **P1-3e가 chg_tsv를 Some 경로에서도 소비하므로 무조건 실행은 이제 필요조건** |

## 보고된 15건 → 작업 매핑

| # | 요지 | 앵커 | 상태 |
|---|---|---|---|
| 1 | stale $RustcHash → 이전 세대 rlib 무경고 링크 | build_db_export.ps1:8 | ✅ 완료 |
| 2 | ~125MB {:#?} 덤프/리빌드 + 첫프레임 스로틀 우회 | lib.rs:1447 | P1-1 |
| 3 | EAP=Stop 제거 → 가짜 "Build successful" | build_db_export.ps1:11 | ✅ 완료 |
| 4 | AI_EVAL/enemy_intent 오귀속 잔존 | lib.rs:1305 / recommender.rs:758 | P1-2 |
| 5 | 'R' 리워크 채널 사멸 (preferred 경로) | lib.rs:490 → recommender.rs:349 | P1-3e |
| 6 | anchor=None/too-old → banrate·lane_dom stale-drop 스킵 | lib.rs:486 | P1-3a |
| 7 | chg_authored.txt 거짓 잔존 + 셔플 재기록 | lib.rs:1471 | P1-3f |
| 8 | CLAUDE.md §0/§3/§5 모순 + 재현불가 "proven" | CLAUDE.md:20 | P2-3 |
| 9 | 프로브 아티팩트 모순 클러스터 (E0609 포함) | tfm2_patch_probe/src/lib.rs:15 | P2-2 |
| 10 | currency 미검사 (PLAUSIBLE) | lib.rs:1457 | P1-3b |
| 11 | 빈-노트 conflation (PLAUSIBLE) | lib.rs:494 | P1-3c |
| 12 | 빈 변경Vec → '~' (PLAUSIBLE) | lib.rs:467 | P1-3d |
| 13 | 스크립트 3중복 + 수동 핀 | build_patch_probe.ps1:12 | P2-1 |
| 14 | RUSTUP_TOOLCHAIN 누수 | build_patch_probe.ps1:10 | ✅ 완료 |
| 15 | 9-영역 비활성화 + 재작성 줄 트랩 | engine.rs:783 | P2-4 |
