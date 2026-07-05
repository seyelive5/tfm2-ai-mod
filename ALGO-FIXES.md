# ALGO-FIXES — 추천기 알고리즘 개선 상세 구현 스펙 (Opus 세션 핸드오프)

근거: `analysis/recommender-algorithm-review.md` (2026-07-05 건전성 심사, F1–F14). **왜**는 그 문서, **무엇을 어떻게**는 이 문서.
전제: 07-04 구현버그 리뷰(REVIEW-FIXES.md)는 완료 상태. 이 문서는 그 위의 **설계 개선**이다.
라인 번호는 2026-07-05 트리 기준(±수 줄 — 항상 주변 코드로 재확인). 모든 인용 코드는 원문 검증 완료.

> **집행 규율**: Step 1은 증거가 이미 있으므로(검증 실패 기록·실측 반증·표시 정직성) 하네스 없이 집행 가능 — 단 항목별 개별 커밋 + 빌드 + 인게임 스모크. Step 2는 **Step 0 하네스 수치 동반 필수**(PLAN.md:99 규율). Step 3은 룰 확인 테스트(T1–T3) 선행.

---

## A. 세션 부트스트랩

### A-1. 파일/심볼 맵 (오해 금지)
- **`draft-overlay/src/engine.rs`** = Tables/스코어링 (W_* 상수 :53-106, `tier_of` :514-534, `counters` :597-603, `comp_need` :605-641, `overlap` :643-650, `score_pick_in` :712-789, `score_ban` :797-861, `dominant` :869-875, `enemy_strong` 게이트 :992, `assign_core` :1111-1157, `recommend_lanes`/플렉스 보너스 :1267-1292, ★ :1320-1340, `ai_prior_term` :438-446, `matchup` :463-476, `low_conf` :343, `chg_nudge` :360-371). **4개 소비처에 #[path] 공유**: tfm2_db_export(recommender.rs:11), draft-overlay lib, bin/meta-report, (리포 외) overlay-selftest — 삭제성 변경 전 4곳 grep 필수.
- **`tfm2_db_export/src/recommender.rs`** = 파이프라인 (`beta_post` :31-37, `patch_current` :63-150, `compute_pos/roles/banrate/lane_dom` :154-333, `ChgInfo` :337, `build_tables` :347-393, `update`/Series :400-540, 코멘터리 :600-830).
- **`tfm2_db_export/src/lib.rs`** = 프로듀서 (`patch_tsv` :64-96, fam/pocket :102-166, `replay_rows` :173-241, `authored_chg` :455-520, AI_PRIOR 피드 :1283-1362, 리빌드 블록 :1400-1530).
- **하네스**: `draft-overlay/src/bin/validate-terms.rs`(5-fold CV, 폴드 할당 :332-334), `draft-overlay/src/bin/backtest.rs`(prev→current 홀드아웃).
- 상수 현황표(출처 감사)는 리뷰 문서 §5. **단위**: delta 1티어밴드 = 0.04 (`tier_of` 컷 ±0.02/±0.06).

### A-2. 빌드/검증 의식
```powershell
& C:\tmp\tfm2-ai-mod\build_mod.ps1 -Crate C:\tmp\tfm2-ai-mod\tfm2_db_export      # ≈4s, 핀 자동
Copy-Item C:\tmp\tfm2-ai-mod\tfm2_db_export\tfm2_db_export.dll `
  "C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2\mods\tfm2_db_export\" -Force
# 하네스(오프라인, 게임 불필요):
cd C:\tmp\tfm2-ai-mod\draft-overlay; cargo run --release --bin validate-terms   # (기존 방식 확인: bin 이름/입력 경로는 파일 헤더 참조)
```
- 데이터: `%LOCALAPPDATA%\tfm2-overlay\probe\` (champion_patch_statistics.tsv, chg_authored.txt, match_replays.debug.txt — **07-04부터 `probe\DEBUG_DUMPS` 마커 있어야 갱신**), `C:\tmp\patch_notes_dump.txt`(노트 7개 전문), `C:\tmp\aiscore\aiscore.txt`(AI 밴/픽 스코어 20만 콜).
- 실측 기준치(2026-07-05): 현 패치 312판, 챔프 games q1/med/q3 = 24/71/111; EB κ\* 현=37/성숙=151; MU 629/990쌍 med 19게임; BT τ≤1.5%p; 프론트라인 층화 잔차≈0; 힐러 +2.3%p; 스택 +2%p.

### A-3. 금지/주의
- **MU direct·데이터 시너지/카운터 부활 금지** (BT τ≈0 — 표본 문제 아님 존재 문제. 리뷰 §4).
- deny/steal ENEMY_VAL 부활 금지 (귀속 무효 — 종결).
- **대칭 메타강함 deny 항 금지** (greedy가 이미 포함 — 이중계상 증명, 리뷰 F9).
- 픽순서 램프류(cnt_ramp/power_ramp/base_mult)는 리플레이로 검증 불가(픽 순서 미저장) — 키우려면 T4 드래프트-리플레이 하네스 선행.
- engine.rs 수정 시 draft-overlay 크레이트도 컴파일되는지 확인(`cargo check` in draft-overlay).

---

## Step 0 — 하네스 인과화 (Step 2의 게이트)

### S0-1. rolling-origin 폴드 — ✅ 완료 (2026-07-05) — ⚠️ 결과가 프로젝트 #1 신호를 뒤엎음
구현·실행·검증 완료. `--legacy`가 구 i%5를 비트단위 재현 → 리팩터 무결. **rolling-origin이 famg("숙련 games") 검증을 누출 아티팩트로 폭로**: base_lane+0.08famg 66.3%→**57.8%** (리프트 → 해악), famg 단독 64.8→52.4. base_lane 견고(59.4→59.0), fam_c만 생존(+1.0%p 인과). → **`analysis/harness-causal-findings.md`** (전문·표·해석·신규 **F15 fam 재가중**) + `analysis/harness-rolling-origin.txt`·`harness-legacy-i5.txt`. **다음: S0-3(프로덕션 수학 이식) 후 famc 재가중 확정 — 그 전엔 fam 가중 변경 금지.**
**S0-1b (per-origin 판별, 07-05): 누출 확정, 굶주림(초기 훈련량 부족) 기각** — Δfamg가 훈련량 증가와 함께 악화(train 535→3832에서 +0.7→−4.8/−2.4; 후기 famg 단독 47~49% <50%), 동일 훈련량(~3590)의 legacy 폴드는 전부 +5.4~+10.8 → 리프트는 미래 데이터가 훈련에 들어갈 때만 존재. 부수: **famc +1.0%p는 early-집중(late 풀 −1.0) → famc 생존 잠정**, S0-3 게이트 근거 데이터로 재확인. 표·해석은 findings 문서 §판별 실험.

<details><summary>원 스펙 (구현 기록)</summary>

현재 `validate-terms.rs:332-334`:
```rust
for (i, m) in matches.iter().enumerate() {
    if i % 5 == fold {
        continue; // held out
    }
```
문제: 인터리브 = 미래로 과거 예측(비인과) → 전 수치 낙관 상한.
**변경**: 버전 경계 rolling-origin — origin v에 대해 train = version < v 전체, test = version == v (8 origins; 매치의 version 필드는 리플레이에 존재). 매치 벡터가 시간순인지 먼저 확인(아니면 (version, index) 정렬). 풀드 정확도 + origin별 표. 기존 i%5 모드는 `--legacy` 플래그로 보존(수치 비교용).
</details>

### S0-2. 데이터 재캡처
```powershell
New-Item "$env:LOCALAPPDATA\tfm2-overlay\probe\DEBUG_DUMPS" -ItemType File
# 게임 1세션(커리어 로드 + 경기 1회) → match_replays.debug.txt 갱신(≈5,000판, 9버전) → 마커 삭제
```

### S0-3. 프로덕션 수학 이식 — ✅ 완료 (2026-07-05)
하네스의 base 계산을 출하 수학으로: `beta_post` κ40(champ, prior 0.5)/κ20(lane, prior champ-mean), prev 블렌드 `0.35·(1−cm/40)` power-prior, 변경 챔프 리셋(버전별 changed-set은 `patch_notes_dump.txt`의 7개 노트에서 재구성 — authored_chg와 동일 규칙). 현행 하네스 κ8과의 차이를 한 번 리포트(κ8 검증이 κ40 출하를 대변하는지가 F7/F11의 미해결점).

**결과** (validate-terms v3.2, `--notes=` 플래그, 현패치 id-순 확장 — id↔version 단조 0위반 검증): base_prod **60.0%** / base_lane_prod **59.5%** vs pooled κ8 58.8/59.0·κ40 58.6/58.9 → **출하 패치-로컬 메모리(현패치 확장+0.35·prev+변경챔프 리셋)가 풀드 히스토리보다 우수(+0.6~+1.4%p) — 설계 검증**. κ8-vs-κ40 풀드 차이 무시 가능(**F7/F11 해소** — κ8 하네스가 결론을 왜곡하지 않았음). famg는 출하 앵커에서도 단조 악화(F15 3중 확인); **famc는 출하 앵커 +1.6%p(s=0.25 최적)이나 여전히 early-집중(late −0.8~−2.2)** — fam 테이블엔 리셋 개념이 없으므로 신호의 국면 의존성(커리어 초반 강함) 가능성, **상향은 S0-4 CI 후**. 상세: `analysis/harness-causal-findings.md` §S0-3.

### S0-4. 사전등록 6모델 + CI — ✅ 완료 (2026-07-05)
비교군(사전 고정, 추가 금지): ① always-first-side ② base_lane ③ base_lane+출하fam ④ 출하 픽스코어 전체(보드 무관 항만) ⑤ ④−comp ⑥ ④−LP. 매치 리샘플 부트스트랩 1000× 페어드 CI. **채택 기준: 풀드 CI 하한 > 0.** ±0.5–1%p급 효과는 이 규모에서 측정 불가 — "미입증/코스메틱 예산"으로 문서화(키우기 금지).

완료 판정: rolling-origin 표 1장 + ①–⑥ CI 표 1장이 `analysis/`에 저장되고, 기존 판정 중 뒤집힌 것 목록화.

**결과** (validate-terms v3.3, 산출물 `analysis/harness-s04-ci.txt` + `harness-rolling-origin.txt`): 유일한 PASS = ②−① +7.6 [+5.3,+9.7] — **base가 유일한 입증 신호**. 출하 af −0.1 [−1.0,+0.8]·번들 +0.7 [−1.2,+2.5]·comp +0.1 [−0.4,+0.5]·LP −0.1 [−0.8,+0.7] 전부 미입증 → **예산 항목 확정, 키우기 금지**. 신규 발견: **af 캡(0.05)이 famg만으로 g≈11에서 포화 → famc 질식**(③ −0.1의 구조 원인) ⇒ **F15 실행형 = af에서 famg 성분 제거(0.15·famc, 캡 유지), famc 상향은 기각**. 뒤집힌 판정: W_MU_LP=0.008의 "스윕 최적" 근거(비인과), lib.rs:113 "+2.5%p 신호 보존" 주석. aip 인과 단독 48.4%(S1-1 게이트 정합). 상세: findings §S0-4.

---

## Step 1 — 즉시 집행 (증거 완비; 항목별 커밋+스모크) — ✅ 코드 완료 (2026-07-05)

> S1-1~S1-6 전부 구현·빌드·배포됨 (커밋 `7bfc340`·`252d434`·`794200f`·`fa6abbb`·`dcbf913`·`7da11fc`). 각 항목 독립 빌드 그린, 양 크레이트(tfm2_db_export + draft-overlay) 컴파일 확인.
> **인게임 스모크 대기(사용자)**: ① ai_prior 축소 후 신규/시드 챔프가 실전 기록을 뒤엎지 않는지 ② 노프론트 상황서 프론트 후보 1티어 점프 소멸 ③ 티어에 "?" 표기 — **`TIER_UNSTABLE=0.10` 밀도 확인**(현 패치 대부분 "?"면 상향, 잘 표집된 챔프도 "?"면 그대로가 정직): engine.rs 한 상수 ④ 코멘터리 "패치/사전" 라벨·"라인전 지표상"(전적 주장 제거)·피어리스 세트배분 문구.
> **의도적 defer**: S1-5 플렉스 귀속(LaneCand에 flex_bonus 배선 필요, 저가치) — 향후 항목으로 남김. **S1-4 note 정정**: matchup_games는 배선 대신 counter_of 문구를 직접 수정(dir 데이터가 weight-0라 게이트 무의미) — pub이라 무해하게 잔존.

### S1-1 (F1) ai_prior 재게이트 — engine.rs:438-446
**현재** (원문):
```rust
fn ai_prior_term(&self, c: &str) -> f32 {
    let p = match self.ai_prior.get(c) { Some(v) => *v, None => return 0.0 };
    let g = self.games(c);
    let fresh = if g == u32::MAX { 1.0 } else { 10.0 / (g as f32 + 10.0) };
    (0.4 * p * fresh).clamp(-0.05, 0.05)
}
```
**교체** (승인 스코프 PLAN.md:144-145 "통계 0 챔프만, 15판 페이드" 복원 + 캡 0.6티어):
```rust
/// cold-start ONLY prior from the game AI's intrinsic valuation. That signal
/// FAILED outcome validation for established champs (45.4%, 5σ below chance —
/// PLAN.md 2026-06-13), so it must never outvote authored patch direction
/// (±0.02) or real evidence: linear fade to ZERO at 15 games (the approved
/// scope), cap 0.024 (< nudge+evidence at any g≥5). Zero-data champs
/// (g==u32::MAX, e.g. release day) keep the full prior — the one regime where
/// dev-intent seeds are plausibly informative.
fn ai_prior_term(&self, c: &str) -> f32 {
    let p = match self.ai_prior.get(c) { Some(v) => *v, None => return 0.0 };
    let g = self.games(c);
    let fresh = if g == u32::MAX { 1.0 } else { (1.0 - g as f32 / 15.0).max(0.0) };
    (0.4 * p * fresh).clamp(-0.024, 0.024)
}
```
- **부수 1 — AI_PRIOR 패치 리셋**(lib.rs): 리빌드 블록에서 authored anchor가 전진 감지될 때(`chg_info.anchor` 변화 — LAST_ANCHOR static 하나 추가) `if let Ok(mut p) = AI_PRIOR.lock() { *p = None; }`. 사유: 패치 후 AI 온라인 탤리가 스코어 기준선을 옮기는데 현재 평균이 패치 전후 혼합(F1 부수).
- **부수 2 — p 중심화(선택, 같은 커밋 가능)**: `set_ai_prior` 시점에 스캔된 p들의 평균을 빼서 저장(lib.rs :893-905 소비부). 현재는 분포가 우연히 near-centered — 명시화로 미래 드리프트 방어.
- **검증**: 산술 스모크 — ④의 부호반전 사례 재계산: 시드 0.1215 챔프, 5게임 2승3패, 너프 노트 → base = −0.0111 −0.0167 + 0.024·(1−5/15)=+0.016 → **−0.0118 (너프 방향 보존)**. 리뷰 문서 F1 사례와 대조. whip_master류(u32::MAX)는 +0.024 유지 확인.

### S1-2 (F3) comp 재캘리브레이션 — engine.rs:605-641, 767-775
**현재 실효 크기**(W_COMP 0.2 × COMP_UNIT 0.12 × 배수 × ramp): tank 0.024 / front 0.012 / CC 0.0168 / heal 0.0144 / backline 0.0144, + **경계 부스트 +0.05**.
**실측**: 프론트라인 층화 잔차 ≈ 0 (n=3077); 힐러만 +2.3%p (2.1σ); "AD-only wins more" 판정은 이미 :636-638에 기록됨.
**변경**:
1. `:770-775` 경계 부스트 **삭제** (또는 0.01 이하로) — 주석에 반증 수치 기록:
```rust
// no-frontline boundary boost REMOVED (2026-07-05 review): measured across
// 4,488 replays, the more-frontline side wins 50.0% and strength-stratified
// residual ≈ 0 (n=3077) — the "autobattle punishes it hardest" premise is
// false. Healer is the only comp dimension that measurably matters (+2.3%p).
```
2. `comp_need` 배수 재조정(:620-635): tank `COMP_UNIT` → `COMP_UNIT*0.2` (실효 0.005), front `*0.5`→`*0.2`, heal `*0.6`→`*0.85` (실효 ≈0.0204 = 측정 효과의 절반-수축), CC `*0.7`→`*0.3`(미검증 — heal에 준해 보수화), backline `*0.6`→`*0.3`.
3. `W_COMP=0.2` 선형항 자체는 기록된 판정("❌ 선형항 무가치")과 모순 — **유지하되** 위 재조정으로 실질 축소. 완전 제거는 Step 2에서 comp 분리 검증(front/heal/CC/backline 개별 항) 후 결정.
4. 코멘터리("앞라인이 없습니다" 경고)는 이번엔 불변(advice-only) — 단, 문구에 "(참고)" 수위 조정은 선택.
**검증**: 빌드 + 드래프트에서 3픽 노프론트 상황 만들어 프론트 후보가 1티어 점프하지 않는지; 힐러 갭 보너스가 dominant 라벨 "조합"으로 잡히는지.

### S1-3 (F4) W_DUP = 0 — engine.rs:57, 776
```rust
const W_DUP: f32 = 0.0; // was 0.4 — NEVER validated, and measurement says the
// SIGN is wrong: the side with the bigger same-category stack wins 53.2% raw,
// ~+2%p strength-stratified residual in both strata (4,488 replays, 2026-07-05).
// Lane overlap is already handled structurally by recommend_lanes/assign_lanes.
// If a controlled harness test confirms the +2%p, consider a small POSITIVE
// stack term instead (Step 2).
```
`overlap()`/호출부는 유지(가중 0). dominant 목록엔 dup가 원래 없음 — 변경 불요.

### S1-4 (F8) 좀비/사망 코드 정리 — 보수적 절차
**먼저 검증** (삭제 전 필수):
```
grep -rn "^S\t\|^K\t" draft-overlay/  # WEIGHTS_TSV·draft_weights.tsv에 S/K 행 존재 여부
grep -rn "matchup_games" --include=*.rs  # 호출자 0 재확인(정의 제외)
```
- draft_weights.tsv에 구세대 S/K 행이 **있고** meta-report 등 legacy 소비처가 로드한다면: 상수 삭제 대신 **주석 스코핑**(각 상수에 `// ZOMBIE in v3.0 in-mod build (no S/K rows); legacy overlay TSVs only`) + in-mod 영향 0 문서화.
- 없다면: `W_SYN/W_CNT/W_WEAK/W_BAN_SYN` 상수·`syn()`·`cnt()`·score 내 데이터 항·ban `적시너지` 라벨 삭제, `W_BAN_CNT`는 live 절반(0.5·matchup)만 남기고 개명(`W_BAN_THREAT`).
- `matchup_games()`: S1-5의 counter_of 게이트로 **배선**(삭제 아님 — 용도가 생김).
- tag 이론항(tag_synergy/tag_counter)은 콜드스타트 전용으로 **유지** — 단 주석에 "confidence=1(성숙 커리어)에서 정확히 0" 명시.
**검증**: 4개 #[path] 소비처 전부 `cargo check` 통과.

### S1-5 (F2/F10) 표시 정직화 — 5개 소편집
1. **posterior sd 방출**: recommender.rs:125 `beta_post(...).0`의 sd(`.1`)를 C행 5필드로:
   `format!("C\t{}\t{:.3}\t{}\t{:.4}", c, mean-0.5, m.round() as u32, sd)` — CP행(:130)도 동일 6필드. engine.rs의 C/CP 파서(`parse_tables_from_str`, C행 파싱부 — `champ_delta`/`champ_games` 채우는 곳)에 옵션 필드로 수용(구 4필드 TSV와 호환: `.get(4)` 옵션 파싱). `champ_sd: HashMap<String,f32>` 추가.
2. **티어 sd 게이트** (`tier_of` :514-534): 레터 산출 후 `if 1.96*sd > 0.04 { return "S?"-스타일 }` — 반환형이 `&'static str`이므로 **방식 선택**: (a) 불안정 시 `""` 반환(가장 단순, 티어 숨김) or (b) 반환형을 `(tier, stable: bool)`로 바꾸고 소비처(recommender.rs Row/LaneCand)에서 `"S?"` 문자열 조립. **(b) 권장** — 정보 보존. 실측 기대: 현 패치에선 대부분 "?" — 그게 정직한 상태다(0/41 안정). UI 과밀하면 게이트를 `2.5·sd>0.06`로 완화하되 근거 주석.
3. **제로데이터 low_conf** (:343): `fn low_conf(&self, c) -> bool { let g = self.games(c); (g == u32::MAX && self.matches > 0) || g < LOW_CONF_GAMES }` — matches>0 조건이 진짜 콜드스타트 커리어(전챔 무데이터)의 전면 플래그를 방지. whip_master류가 "?"+표본경고 코멘터리를 받게 됨.
4. **counter_of 게이트/문구** (recommender.rs:669-676): 현재 "라인전 지표·상대 전적상 우위" — dir 가중치 0이라 '상대 전적' 근거 0. 수정:
```rust
if let Some(cof) = &lane.counter_of {
    let head2head = tables.matchup_games(&lane.id, /*cof의 id*/) >= 20; // S1-4에서 배선
    parts.push(format!(
        "1순위 픽은 상대 {}에게 {} 우위가 있습니다.",
        cof, if head2head { "라인전 지표·상대 전적상" } else { "라인전 지표상" }
    ));
}
```
(counter_of가 ko명만 보유하면 id도 같이 담게 구조 소폭 변경 — LaneCand 생성부 :1294-1305 참조.)
5. **base 라벨 분기 + 플렉스 귀속** (engine.rs :734/:787, :1271-1279):
   - score_pick_in에서 `let prior_part = self.chg_nudge(cand) + self.ai_prior_term(cand);`를 분리 보관, dominant 목록에 `("강챔", power_ramp*delta_for)`와 `("패치/사전", prior_part)`를 **별개 엔트리**로.
   - 플렉스: recommend_lanes가 보너스를 score에 더할 때 label을 덮어쓸 수단이 없음 → LaneCand에 `flex_bonus: f32` 저장, 코멘터리에서 `flex_bonus > |score−flex_bonus 대비 유의|`면 "(플렉스 가치 포함)" 병기. 최소구현: 피어리스에서 flex_bonus>0.015면 헤드라인에 "· 플렉스" 추가.
6. **피어리스 메시지 분리** (recommender.rs :807-829 근방): ★("얕은 라인 확보")와 고갈("아껴두세요")이 동시 표출 — 문구를 "이번 세트: ★ 라인 우선 확보 / 다음 세트 대비: X·Y는 아직 아낄 가치"로 한 문장 통합해 모순 제거.
**검증**: 인게임 코멘터리 캡처로 4개 문구 변화 확인; 구 TSV(4필드) 로드 호환.

### S1-6 (F10) 거짓/스테일 주석 3건
- engine.rs:412 `(0.08·famg+0.25·famc)` → `(producer ships 0.020·ln-games + 0.15·centered-wr, cap 0.05 — deliberately DETUNED from the validated 0.08/0.25 for endogeneity; see lib.rs:102-114)`.
- engine.rs:462-463 `~2 games/pair is typical` → `median 19 games/pair on the 2026-07-05 career (629/990 pairs populated); direct term is weight-0 (BT residual τ≈0 — pair counters don't exist; see analysis/recommender-algorithm-review.md §4)`.
- engine.rs:779 `validated strongest signal; weights baked by the producer` → `validated strongest SIGNAL (famg 66.0% standalone); the shipped weights are deliberately detuned from the validated fit (lib.rs:102-114)`.

---

## Step 2 — 하네스 통과 조건부 (S0 완료 후; 각 항 = 하네스 수치 첨부 커밋)

| # | 변경 | 스펙 | 판정 테스트 |
|---|---|---|---|
| S2-1 (F5) | 카운트-스케일 이월 | `ChgInfo.dirs: Vec<(String,char)>` → `(String,char,u32)` (n=b+n′; authored_chg :472-478에서 폐기 중단, TSV 폴백은 n=4 취급). patch_current의 변경챔프 `continue`(:91-94)를 `w = 0.35·max(0,1−n/4)·(1−cm/40)`로 — n=1→0.26이월, n≥4/'~'대형/'R'→0(현행). banrate/lane_dom은 전량 드롭 유지(저가중) | T-amnesia: 과거 패치 경계들에서 첫 20게임 승률 예측 MAE — (0) vs (0.35·prev) vs (스케일) 비교, n≤2에서 (스케일)<(0) 이면 채택 |
| S2-2 (F6) | 카운트-스케일 넛지 | `chg_nudge`: 크기 `0.01+0.005·min(n,4)`; '~'는 `0.02·(b−n′)/(b+n′)·min(1,n/4)` (5버프1너프 → +0.0133) | T-nudge: 7개 노트의 signed count vs 실현 delta 변화 회귀 — 기울기>0이면 채택, 평탄하면 균일 ±0.02 유지 |
| S2-3 (F11) | 적응형 EB κ | patch_current에서 `κ̂ = 0.25 / max(1e-4, var(d_i) − mean(0.25/n_i))` clamp [30,160] (~10줄). KAPPA_CHAMP=40 상수는 폴백 | T1-κ: rolling-origin에서 κ∈{20,40,80,150,EB} 정확도 + top-10 Kendall-τ 안정성 |
| S2-4 (F7) | LP 재구축-or-0 | 셀 평균을 라인 평균으로 수축(`(n·m+30·lane_mean)/(n+30)`), n-가중 sd, 소스를 latest+prev(1.0/0.35)로 — 이후 인과 재스윕 s∈{0.004,0.008,0.016}. **CI 하한 ≤0이면 W_MU_LP=0** (W_MU_DIRECT 전례) | S0 하네스 |
| S2-5 (F13) | solo-rank power prior | C행 구축에 solo_rank 1,091판을 w_solo∈{0.1,0.25}로 합류(승·표본 동시 할인). 가설: games<30 stratum만 개선 | T-solo: 층화 rolling-origin |
| S2-6 (F13) | BT θ 항 | 리플레이 joint 로지스틱 `P(win)=σ(Σθ_b−Σθ_r)` + L2, fam 공변량 옵션 — validate-terms에 `θ_bt` 컬럼, base 대체/보조 비교 | T-BT |
| S2-7 (F4 후속) | 스택 양수 항? | S1-3 후 하네스에 stack 항(최대 동일카테고리 수 차) 추가 — CI 하한>0일 때만 소형(≤0.01) 채택 | S0 하네스 |
| S2-8 (F14) | comp 분리 검증 | comp를 front/heal/CC/backline 개별 항으로 하네스에 — heal만 생존 예상. 결과로 S1-2 배수 확정 or W_COMP 완전 제거 | S0 하네스 |

**집행 결과 (2026-07-05, validate-terms v3.4 진단 — 커밋 `3648700`/`0b3ae2a`/`70340e5`/`641f7dd`, 산출물 `analysis/harness-rolling-origin.txt`):**
- **F15 ✅ 출하** (`3648700`): af에서 famg 성분 제거 → `min(0.05, 0.15·famc)`. 검증 af_new: base_lane +0.3 [−0.5,+1.1] / prod 앵커 +0.1 [−0.7,+0.8] — 구 af(−0.1)보다 개선, 예산 크기.
- **S2-4 ✅ 출하** (`0b3ae2a`): W_MU_LP=0(근거 스윕이 비인과; 인과 기여 −0.1 [−0.8,+0.7], 누출-우위 소스 상한) + power_ramp=1.0 복원(대체할 카운터 항 부재). matchup() 축 전체 weight-0.
- **S2-2 ✅ 출하** (`641f7dd`): T-nudge slope **+0.0125/chg, r +0.66, n=46** → 카운트-스케일 넛지 `0.01+0.005·min(n,4)`; '~'는 `0.02·(b−n′)/n·min(1,n/4)`; 카운트 부재 = n=2(구 ±0.02 재현). CHG 행 5필드(하위호환). ⚠️ 스모크 노트: 1줄 넛지(0.015) < ai_prior 캡(0.024) — S1-1 불변식은 nudge+evidence 기준이라 유지, 인게임 확인 항목.
- **S2-1 기각(모집단 부재)**: T-amnesia n≤2 층 표본 1개(이 게임 노트는 1-2줄 수정을 안 함); n≥3에서 reset 0.112/full 0.115/scaled 0.111 동률 — 전량 리셋 유지, ChgInfo 이월 없음.
- **S2-3 종결(κ40 확정)**: 인과 κ 스윕 20/40/80/150 → 60.1/60.0/60.1/59.9% 평탄 — 적응형 EB 불채택 (recommender.rs KAPPA_CHAMP 주석 기록).
- **S2-7 기각**: 0.01·stack Δ−0.1 [−0.5,+0.3] — 원시 +2%p 스택 연관은 base가 이미 반영한 챔프 강도 (W_DUP 주석 기록).
- **S2-8 종결(가산 가치 0)**: heal −0.2 [−0.4,+0.1] · cc 무분산 · front ±0.0 · back −0.1 — heal 포함 전 성분 미입증. W_COMP는 예측항이 아닌 UX 가드레일로 명시 유지(배수 불변, 키우기 금지; comp_need 주석 기록).
- **S2-5 예비 기각** (v3.5, 07-05): prev-버전 솔로 블렌드(w∈{0.1,0.25}, 챔프 레벨; 동버전 솔로는 크로스파일 순서 불명 → 보수적 제외) Δ+0.0 [−0.1,+0.2], thin 층 +0.1 — 미입증. 솔로 1,091판이 2026.3.0/3.1에 쏠려(893/1091) 현 데이터의 prev-경로가 얇음 — **S0-2 후 재확인**.
- **S2-6 예비 기각** (v3.5, 07-05): BT θ(pooled full-batch 로지스틱+L2, 결정적) 단독 **58.8%** vs base_prod 60.0 (Δ−1.1 [−3.0,+0.7]); thin 층 **−2.7** — κ-수축 없는 joint fit은 얇은 챔프에서 과적합. base 대체 탈락; 재캡처 후 재확인 여지만.
- **(v3.5 부수) parse_lane_dom 비결정성 수정**: HashMap 순회 순서 × f64 합산 비결합성으로 lp 표시 끝자리가 런마다 흔들림 — `--legacy` 바이트 회귀가 검출(49.4↔49.3). BTreeMap으로 결정화, 회귀 기준 legacy lp = 49.3%.
- 배포: tfm2_db_export.dll 재빌드·게임 반영(백업 `.bak-0620` 유지). ⏳ 인게임 스모크: ▲▼ 넛지 크기 분화(1줄 vs 4줄+), '~' 혼합챔 방향 넛지 등장, 카운터 제안/『카운터』 라벨 소멸(노이즈였음 — 정직한 후퇴), af 상수-포화 해제(숙련 선수 간 차등 복원).

## Step 3 — 룰 테스트 후 구조 개선

### 선행 룰 테스트 (bp_live.txt 관찰, 각 5–30분)
- **T1 피어리스 밴 지속**: 1세트에 밴한 X가 2세트에 픽 가능한가? (락=픽 전용 가설 확인 → S3-3 배율 결정)
- **T2 선후공 신호**: ~10경기에서 밴 리스트 성장 순서·`team1_order/team2_order`·픽 선후 기록 — "선밴측==선픽측" 또는 order 필드 인코딩 가설 확정
- **T3 호버 semantics**: 내 픽 턴에 확정 없이 호버 → `team*_pick` 선반영 여부. 반영되면 confirmed-only 필터/디바운스 추가

→ **패시브 수집으로 전환 (2026-07-05 17:45 배포)**: bp_live의 승인된 보드 변화마다 `%LOCALAPPDATA%/tfm2-overlay/bp_history.log`에 1줄 append(ts + 전 필드, '|' 구분, 5MB→.old 로테이션). T1은 `set=` 필드로 세트간 밴/fearless 지속 추적, T2는 밴 리스트 성장 순서, T3는 타임스탬프 간 픽 리스트 진동으로 판독 — **수동 관찰 세션 불필요, 몇 시리즈 플레이 후 로그 분석으로 판정.**

### 구현 (T 결과 조건부)
| # | 변경 | 스펙 |
|---|---|---|
| S3-1 (F9) | first_pick 밴페이즈 감지 | T2 규칙으로 Series에 조기 학습 경로 추가(recommender.rs :494-496 유지 + 밴 성장 순서 관찰 경로). 실패 시 현행 유지(0=자동 폴백) |
| S3-2 (F9) | 스나이프 게이트 해제 | engine.rs:992 `phase_ban && !s.enemy_locked.is_empty()` → `phase_ban` (락 비면 적 풀 = pool−taken). ≤2-strong 임계가 오발 자체 억제 — 배포 후 1세트 밴 목록 눈검사 |
| S3-3 (F9) | 픽측 pocket steal | score_pick_in에 `+ W_PICK_DENY·self.enemy_pocket.get(cand)…` (W≈0.35, 실효 ≤0.016; T1이 "밴은 세트한정, 픽은 시리즈락" 확인 시 피어리스 ×1.5). dominant에 ("저격픽", …) 엔트리. **주의: enemy_pocket이 Tables에 있고 픽 경로에서 접근 가능한지 확인**(현재 밴 전용 소비 :843) |
| S3-4 (F9) | order-conditional 밴 | S3-1 후: order==1이면 글로벌 top-1 delta 챔프의 base를 하드 할인(내가 집을 것), order==2면 base_mult 1.0→1.1. T4(드래프트-리플레이 하네스, 밴 채택률) 없이는 소폭만 |
| S3-5 (F12) | enemy-lane 단일화 | `assign_core(enemies)` 결과 `enemy_at`을 mu 항(:756-766)·counters(:598)·ban threat에 공통 주입, `primary(e)==lane` 필터·mu_ramp 삭제. 5픽 스왑에선 소거 증명 있으므로 동작 불변 확인이 곧 테스트 |
| S3-6 (F9, 선택) | replacement-level 밴 EV | S3-2/4 효과 확인 후에만: assign_core 재사용해 `적 최적조합 − 후보 제외 시 최적조합` 마진. ~100줄, O(60·assign) — 과엔지니어링 경계 |
| S3-7 (F14) | POS soft eligibility | :1267-1269 하드 절벽 → `≥10게임이면 후보 인정 + 오프메타 −0.01` or 절벽 10%로 인하 |
| S3-8 (F9 low) | banrate→objective proxy / OPP usage 수축 | `B` 대체: `pickrate·max(0,shrunk_delta)` 같은 0.05 가중; OPP `usage=(g+1)/(tg+3)` |

**집행 결과 (2026-07-05, 커밋 `8fdf9d9`/`c9e87b3`/`d440f96`/`bb2aac5` — T1–T3 없이 집행 가능분, DLL 배포됨):**
- **S3-2 ✅ 출하** (`8fdf9d9`): 스나이프 게이트 해제 — 락이 없으면 적 풀 = pool−taken으로 그대로 계산(필터가 자연 축퇴; ≤2-strong 임계가 오발 억제). 1세트 밴 목록 눈검사 대기.
- **S3-3 ✅ 출하(기본형)** (`d440f96`): `steal = 0.35·enemy_pocket[cand]` (실효 ≤0.016, ~0.4티어) + dominant "저격픽". enemy_pocket은 Tables 필드라 픽 경로 접근 문제 없음(스펙 주의사항 해소). **피어리스 ×1.5는 T1 확인 후.**
- **S3-7 ✅ 출하** (`bb2aac5`): `lane_eligible`(POS 셋 ∨ CP≥10게임) + `off_meta_penalty` −0.01 — 라인 그리드 후보 필터·assign_core 절벽 양쪽 소프트화. flex()/strong_per_lane은 POS 셋 유지(플렉스 보너스·저격 깊이 인플레 방지).
- **S3-8 ✅ 부분 출하** (`c9e87b3`): OPP `usage=(g+1)/(tg+3)` 수축. banrate 대체 프록시는 **보류** — 스펙이 크기를 열어둠('같은') + W_BAN_META=0.05가 이미 순환 신호를 1티어 미만으로 캡 + 정직한 채점기는 T-ban(재캡처 후).
- **S3-5 무의미화(S2-4 여파)**: 단일화 대상 소비처(mu 항·counters 데이터부·밴 threat의 matchup)가 전부 weight-0이 됨 — enemy-lane 통일은 죽은 경로 리팩터. matchup 축이 부활하지 않는 한 착수하지 않음.
- **잔여**: T1–T3(사용자, bp_live.txt 관찰 각 5–30분) → S3-1(T2 조건부)·S3-3 피어리스 배율(T1)·S3-4(S3-1 후)·S3-6(S3-2/4 효과 확인 후). **T-ban·T4**(밴/픽 행동 채점기, 유일한 정직한 밴 레이어 검증)는 S0-2 재캡처 후 구축.
- ⏳ 스모크: 1세트 밴에 "저격" 등장, 픽 추천 "저격픽" 라벨, 오프메타 라인 후보 등장(−0.01 확인), 넛지 크기 분화(S2-2).

### 밴 레이어 검증 (결과 하네스 구조적 불가 — 별도 목적함수)
- **T-ban 행동예측**: 팀별 rolling-origin — t 이전 매치로 OPP/pocket 구축, t 매치의 적 챔프셋 top-k 적중률 vs 글로벌 픽률 베이스라인. `W_BAN_OPP`/pocket/snipe의 유일한 정직한 채점.
- **T4 드래프트-리플레이 어드바이스 벤치**(PLAN Phase 0b): 실픽의 우리-랭킹 백분위(승팀 실픽이 패팀보다 상위 백분위인가) — 픽 랭킹 자체의 오프라인 검증(~150줄 신규 bin).

---

## 재론 금지 (리뷰 §4 + 기존)

| 질문 | 판정 | 증거 |
|---|---|---|
| MU direct/페어 카운터 부활 | 불가 — 추정 대상 부재 | BT 잔차 χ²/df≈1.0 전층, τ≤1.5%p |
| 데이터 시너지/카운터 재투자 | 불가 | 3회 홀드아웃 실패 + p(a>b)≈강도차 기전 |
| "노프론트=최악" 전제 | 반증 | 층화 잔차≈0 (n=3077) |
| 스택 벌점 | 부호 반대 | +2%p 잔차 양층 |
| 5픽 스왑 적-매치업 항 | no-op | 열상수 소거+클램프 비활성 증명 |
| 0.35 블렌딩 "고정" 비판 | 전제 오류 | 이미 per-champ power prior |
| deny/steal·enemy_intent (훅 소스) | 종결 | 귀속 무효 (CLAUDE.md §2c) |

## F → 작업 매핑

| F | 결함 | 작업 |
|---|---|---|
| F1 | ai_prior 서열 역전 | **S1-1** |
| F2 | 티어 허위 확신 | **S1-5**(1,2,3) |
| F3 | comp 반증 | **S1-2**, S2-8 |
| F4 | dup 부호 | **S1-3**, S2-7 |
| F5 | 망각 절벽 | S2-1 |
| F6 | 넛지 손실 | S2-2 |
| F7 | 방법론 | **S0**, S2-4 |
| F8 | 좀비 | **S1-4** |
| F9 | 밴 구조 | S3-1..4, S3-6, S3-8, T-ban |
| F10 | 코멘터리 | **S1-5**(4,5,6), **S1-6** |
| F11 | κ 고정 | S2-3 |
| F12 | enemy-lane 3중 | S3-5 |
| F13 | 방치 데이터 | S2-5, S2-6 |
| F14 | 잡다 | S1-2(4), S2-8, S3-7, S3-8 |

**권장 세션 플랜**: 세션 1 = Step 1 전체(S1-1…S1-6, 각각 커밋) + 인게임 스모크. 세션 2 = Step 0(하네스) + Step 2 중 T-amnesia/T-nudge/κ(오프라인으로 완결 가능). 세션 3 = T1–T3 룰 테스트(게임 필요) → Step 3. 진행할 때마다 이 문서 체크박스 갱신 + CLAUDE.md §7 반영.
