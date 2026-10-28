# TFM2 Draft Coach

> ⚠️ **프리뷰 알파 — 아직 완성된 모드가 아닙니다.** 이 깃허브 저장소 말고는 어디에도(스팀 창작마당 포함) 올리지 않았습니다.
> 영어 버전은 준비할 예정이지만 시간이 더 필요합니다.
>
> *Preview alpha — not a finished mod. It is not released anywhere except this GitHub repository (no Steam Workshop
> release). An English version is planned but needs more time.*

*Teamfight Manager 2*용 **밴픽(드래프트) 추천 모드** — 게임의 밴픽 화면 안에서 실시간으로 픽/밴을 추천합니다.

> English: an in-game ban/pick recommender mod for *Teamfight Manager 2*. It reads the live draft and
> draws tier tags, pick/ban highlights and two game-style recommendation cards onto the game's own
> ban/pick screen — no external window, no launcher.

> **상태 (2026-10-28): v4.3.1 — 게임 카드 UI(인게임 확인) + 상대 다음 수 후보(F2·버튼 인게임 확인)**
> (게임 0.7.0 확인, 클래식 UI. 새 UI 스타일은 아직 확인하지 않았습니다).
> 게임 0.6부터 게임사가 classic 모드 SDK 지원을 끝내서, 게임 0.6.5+의 **stable 모드 API**로 다시 작성했습니다.
> 카드 표시, 해설 글자 색, Tab 숨기기를 모두 게임에서 확인했습니다.
> 계획·점검 결과: [`ROADMAP.md`](ROADMAP.md), 다음 단계: [고도화 검토](docs/research/2026-10-28-advanced-directions.md).

## 무엇을 하나요

- **밴픽 보드 자동 읽기** — 밴/픽/순서/진영/피어리스 잠금을 밴픽 화면에서 실시간으로 읽습니다.
- **게임 화면에 직접 표시** — 외부 실행 파일·창 없이 모드 하나(`tfm2_db_export`)로 동작합니다.
  - 챔피언 칸: 왼쪽 위에 추천 순위·티어, 추천 칸 테두리(청록 = 픽 / 보라 = 밴).
  - 챔피언 정보 자리에 게임 스킬 상자 모양의 카드 두 장: 순위 카드(Top 5, 스왑 단계에선 라인 배치)와
    "AI 분석관" 해설 카드. Tab으로 숨기기/보이기.
  - 표본이 적은 티어는 흐리게 그립니다. 세이브 초반처럼 전부 불확실하면 순위 카드에 "티어 잠정"이라고 표시합니다.
- **상대 다음 수 후보 (옵트인, F2 또는 해설 카드의 `상대 수 · F2` 버튼)** — 해설 카드 첫 줄에 상대 AI의 다음 밴·픽
  후보 셋을 보여 줍니다. 게임의 드래프트 AI가 매긴 점수(상대의 직전 점수 + 리그 평균)로 계산하며, 추천 점수에는 넣지 않습니다.
  - 후보 옆 숫자는 **내 경기에서 실제로 맞힌 기록**입니다. 예: `(내 경기 4/10)` = 상대의 결정 10번 중 4번이 후보 셋 안에
    들었다는 뜻입니다. 밴·픽을 따로 세고, 5번 이상 채점된 뒤부터 보입니다.
  - AI끼리 붙은 드래프트에서 3위 안 적중은 게임 0.6.5에서 밴 약 93%·픽 약 70%, 0.7.0에서 밴 약 82%·픽 약 70%였습니다.
    사람을 상대로 한 판에서는 더 낮았습니다(0.7.0 내 경기 29번 중 15번).
    근거: [고도화 검토](docs/research/2026-10-28-advanced-directions.md) E1b·E1c.
- **게임 화면은 건드리지 않음** — 표시는 전부 모드가 만든 노드이고 클릭은 게임으로 그대로 통과합니다.
  LT AI Coach 등 다른 모드와 함께 켤 수 있습니다.
- **점수 모델** (v3.0과 동일)
  - 현재 패치 위주의 챔피언 승률(베타 사후평균 + 수축), 이전 패치는 감쇠 반영 — 세이브의 경기 기록에서 직접 집계
  - 패치 노트 버프/너프 — 게임 자체 `PatchNote`의 `is_buff`를 직접 읽어 반영
  - 선수의 챔피언 숙련(약하게), 챔피언 라인 배정, 선픽 감지·픽 순서별 밴 가중, 피어리스 대응

## 검증

- 시간순(rolling-origin) 홀드아웃 백테스트 + 부트스트랩 신뢰구간으로 모든 점수 항을 채택/기각합니다.
- 5,274경기 기준 "점수 높은 팀이 이긴다" 예측 정확도 **약 60%** (게임 0.4.x 데이터; 0.6.5 데이터 재측정은 ROADMAP Phase 2).
- 세이브 초반(콜드스타트) 구간과 확률 보정은 아직 측정하지 않았습니다 — [고도화 검토](docs/research/2026-10-28-advanced-directions.md) 참조.
- 검증에서 **효과 없음**이 확인된 항(챔피언 시너지/카운터, 동라인 매치업, 상대 팀 성향, 포켓픽 밴 등)은 가중치 0으로 제거했습니다.
- 상세: [`ALGO-FIXES.md`](ALGO-FIXES.md), [`analysis/recommender-algorithm-review.md`](analysis/recommender-algorithm-review.md),
  [`analysis/harness-causal-findings.md`](analysis/harness-causal-findings.md)

## 저장소 구조

| 폴더 | 내용 |
|---|---|
| `tfm2_db_export/` | **제품** — 추천기 모드 본체(v4.3, stable API): `db`(경기 기록 → 통계) · `board`(밴픽 읽기) · `recommender` · `prior`(게임 AI 사전) · `foresee`(상대 다음 수 후보) · `ui`(게임 카드·칸 태그) · `markup`(게임 글자 색 문법) · `diag`·`probe`(진단 파일·덤프) |
| `draft-overlay/` | 점수 엔진 `src/engine.rs`(`tfm2_db_export`가 `#[path]`로 공유) + 검증 하네스(`src/bin/validate-terms.rs`, `backtest.rs`) + 구 hudhook 오버레이 |
| `draft-weights-gen/` | 구 가중치 생성기(구 오버레이용) |
| `tfm2_bp_reader/` | 라이브 밴픽 읽기 프로토타입(classic) — `tfm2_db_export`에 통합됨 |
| `tfm2_patch_probe/` | classic 시절 패치 후 컴파일 카나리 |
| `bp_probe/`, `tfm2_bp2/`, `tfm2_ui_probe/`, `tfm2_aiscore/`, `tfm2_pool_reader/`, `draftwatch/`, `q/` | classic API 조사용 프로브 |
| `tfm2_draft_ai/` | 은퇴한 구 모드(드래프트 점수 후크) |
| `analysis/` | 알고리즘 심사·하네스 결과·바이너리 문자열 덤프 |

## 문서

- [`ROADMAP.md`](ROADMAP.md) — **stable API 이식 기준점**: 확인된 사실, v4.0 초안 점검, 결정 사항, 인게임 검증 절차, 이후 단계
- [`docs/research/2026-10-28-advanced-directions.md`](docs/research/2026-10-28-advanced-directions.md) — v4.2 이후 고도화 검토와 작업 순서
- [`docs/specs/2026-10-27-ingame-cards-design.md`](docs/specs/2026-10-27-ingame-cards-design.md) — 게임 카드 UI 설계
- [`CLAUDE.md`](CLAUDE.md) — 모딩 API·빌드 파이프라인·검증된 사실 레퍼런스(classic API 시절)
- [`PLAN.md`](PLAN.md) — 마스터 플랜(검증 인프라 → 게임 AI 신호 → 선수 축 → 픽 순서 → 정리 → 배포)
- [`ALGO-FIXES.md`](ALGO-FIXES.md) — 알고리즘 개선 스펙과 "재론 금지" 판정표
- [`REVIEW-FIXES.md`](REVIEW-FIXES.md) — 2026-07-04 코드 리뷰 후속 기록

## 빌드 (stable API, 게임 0.6.5+)

필요: Rust(툴체인 무관) + MSVC C++ 빌드 도구(link.exe), 게임 설치 폴더(stable SDK `mod-sdk-stable\`이 동봉됨).

```powershell
& .\build_db_export.ps1            # 빌드만 → tfm2_db_export\tfm2_db_export.dll
& .\build_db_export.ps1 -Deploy    # 빌드 + 게임 mods\tfm2_db_export\ 에 DLL·mod.mod_info 복사
& .\build_db_export.ps1 -CheckOnly # 타입 체크만
```

스크립트가 게임 폴더의 `mod-sdk-stable`을 저장소의 `sdk\mod-sdk-stable`(gitignore)로 복사하고, 게임 패치로 SDK가 바뀌면
자동으로 다시 복사합니다. 게임 경로가 기본 Steam 경로가 아니면 `-GameDir <경로>`를 붙이세요.
배포 후 게임을 재시작하고 모드 메뉴에서 "TFM2 드래프트 추천기"를 켜면 됩니다.

## 관련

- 챔피언 생성기(분리됨): [`tfm2-champion-creator`](https://github.com/seyelive5/tfm2-champion-creator)
- 공식 모드 문서: [`teamsamoyed/TeamfightManager2Mod`](https://github.com/teamsamoyed/TeamfightManager2Mod)
