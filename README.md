# TFM2 Draft Coach

*Teamfight Manager 2*용 **밴픽(드래프트) 추천 모드** — 게임의 밴픽 화면 안에서 실시간으로 픽/밴을 추천합니다.

> English: an in-game ban/pick recommender mod for *Teamfight Manager 2*. It reads the live draft and
> stamps tier labels, pick/ban highlights and a recommendation panel onto the game's own ban/pick screen —
> no external window, no launcher.

> ⚠️ **상태 (2026-10-26): 개발 중단 후 재개 준비 중.**
> 현재 코드는 게임 **0.4.14** 기준 **classic 모드 API**로 작성되어 있어 **게임 0.6+에서는 로드되지 않습니다**
> (게임사가 0.6부터 classic SDK 배포·지원을 종료). stable API로 이식할지 결정하는 중입니다.

## 무엇을 하나요 (마지막 작동 버전: 게임 0.4.14)

- **밴픽 보드 자동 읽기** — 밴/픽/순서/진영을 밴픽 화면에서 실시간으로 읽습니다.
- **게임 화면에 직접 표시** — 챔피언 칸마다 티어 글자, 추천 테두리(청록 = 픽 / 보라 = 밴),
  상단 추천·스왑 줄, "AI 추천기" 패널. 외부 실행 파일·창 없이 모드 하나(`tfm2_db_export`)로 동작합니다.
- **점수 모델**
  - 현재 패치 위주의 챔피언 승률(베타 사후평균 + 수축), 이전 패치는 감쇠 반영
  - 패치 노트 버프/너프 — 게임 자체 `PatchNote`의 `is_buff`를 직접 읽어 반영
  - 선수의 챔피언 숙련(약하게), 챔피언 라인 배정, 선픽 감지·픽 순서별 밴 가중, 피어리스 대응

## 검증

- 시간순(rolling-origin) 홀드아웃 백테스트 + 부트스트랩 신뢰구간으로 모든 점수 항을 채택/기각합니다.
- 5,274경기 기준 "점수 높은 팀이 이긴다" 예측 정확도 **약 60%**.
- 검증에서 **효과 없음**이 확인된 항(챔피언 시너지/카운터, 동라인 매치업, 상대 팀 성향, 포켓픽 밴 등)은 가중치 0으로 제거했습니다.
- 상세: [`ALGO-FIXES.md`](ALGO-FIXES.md), [`analysis/recommender-algorithm-review.md`](analysis/recommender-algorithm-review.md),
  [`analysis/harness-causal-findings.md`](analysis/harness-causal-findings.md)

## 저장소 구조

| 폴더 | 내용 |
|---|---|
| `tfm2_db_export/` | **제품** — 추천기 모드 본체(v3.0): 보드 읽기 + 통계 수집 + 추천 + 인게임 표시 |
| `draft-overlay/` | 점수 엔진 `src/engine.rs`(`tfm2_db_export`가 `#[path]`로 공유) + 검증 하네스(`src/bin/validate-terms.rs`, `backtest.rs`) + 구 hudhook 오버레이 |
| `draft-weights-gen/` | 구 가중치 생성기(구 오버레이용) |
| `tfm2_bp_reader/` | 라이브 밴픽 읽기 프로토타입 — `tfm2_db_export`에 통합됨 |
| `tfm2_patch_probe/` | 게임 패치 후 컴파일 카나리 |
| `bp_probe/`, `tfm2_bp2/`, `tfm2_ui_probe/`, `tfm2_aiscore/`, `tfm2_pool_reader/`, `draftwatch/`, `q/` | API 조사용 프로브 |
| `tfm2_draft_ai/` | 은퇴한 구 모드(드래프트 점수 후크) |
| `analysis/` | 알고리즘 심사·하네스 결과·바이너리 문자열 덤프 |

## 문서

- [`CLAUDE.md`](CLAUDE.md) — 모딩 API·빌드 파이프라인·검증된 사실 레퍼런스(classic API 시절)
- [`PLAN.md`](PLAN.md) — 마스터 플랜(검증 인프라 → 게임 AI 신호 → 선수 축 → 픽 순서 → 정리 → 배포)
- [`ALGO-FIXES.md`](ALGO-FIXES.md) — 알고리즘 개선 스펙과 "재론 금지" 판정표
- [`REVIEW-FIXES.md`](REVIEW-FIXES.md) — 2026-07-04 코드 리뷰 후속 기록

## 빌드 (classic, 게임 0.4.14까지)

```powershell
& .\build_mod.ps1 -Crate .\tfm2_db_export   # classic SDK가 C:\tmp\mod-sdk 에 있어야 함(저장소 미포함)
```

classic SDK는 게임사 바이너리라 저장소에 포함하지 않으며, 게임 0.6+용으로는 더 이상 배포되지 않습니다.

## 관련

- 챔피언 생성기(분리됨): [`tfm2-champion-creator`](https://github.com/seyelive5/tfm2-champion-creator)
- 공식 모드 문서: [`teamsamoyed/TeamfightManager2Mod`](https://github.com/teamsamoyed/TeamfightManager2Mod)
