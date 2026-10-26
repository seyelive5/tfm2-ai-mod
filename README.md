# TFM2 Draft Coach

*Teamfight Manager 2*용 **밴픽(드래프트) 추천 모드** — 게임의 밴픽 화면 안에서 실시간으로 픽/밴을 추천합니다.

> English: an in-game ban/pick recommender mod for *Teamfight Manager 2*. It reads the live draft and
> draws tier tags, pick/ban highlights and a recommendation panel onto the game's own ban/pick screen —
> no external window, no launcher.

> ⚠️ **상태 (2026-10-27): stable API 이식판 v4.1.0 — 빌드 완료, 인게임 검증 전** (`stable-port` 브랜치).
> 게임 0.6부터 게임사가 classic 모드 SDK 지원을 끝내서, 게임 0.6.5+의 **stable 모드 API**로 다시 작성했습니다.
> 계획·점검 결과·검증 절차: [`ROADMAP.md`](ROADMAP.md). 마지막으로 실전에서 검증된 버전은 v3.0(classic, 게임 0.4.14)입니다.

## 무엇을 하나요

- **밴픽 보드 자동 읽기** — 밴/픽/순서/진영/피어리스 잠금을 밴픽 화면에서 실시간으로 읽습니다.
- **게임 화면에 직접 표시** — 챔피언 칸마다 티어 태그, 추천 칸 테두리(청록 = 픽 / 보라 = 밴, 1위는 굵게),
  "AI 추천기" 창(Top-5 + 해설, 드래그 가능, Tab 접기, X 닫기). 외부 실행 파일·창 없이 모드 하나(`tfm2_db_export`)로 동작합니다.
- **게임 화면은 건드리지 않음** — 표시는 전부 모드가 만든 노드이고 클릭은 게임으로 그대로 통과합니다.
  LT AI Coach 등 다른 모드와 함께 켤 수 있습니다.
- **점수 모델** (v3.0과 동일)
  - 현재 패치 위주의 챔피언 승률(베타 사후평균 + 수축), 이전 패치는 감쇠 반영 — 세이브의 경기 기록에서 직접 집계
  - 패치 노트 버프/너프 — 게임 자체 `PatchNote`의 `is_buff`를 직접 읽어 반영
  - 선수의 챔피언 숙련(약하게), 챔피언 라인 배정, 선픽 감지·픽 순서별 밴 가중, 피어리스 대응

## 검증

- 시간순(rolling-origin) 홀드아웃 백테스트 + 부트스트랩 신뢰구간으로 모든 점수 항을 채택/기각합니다.
- 5,274경기 기준 "점수 높은 팀이 이긴다" 예측 정확도 **약 60%** (게임 0.4.x 데이터; 0.6.5 데이터 재측정은 ROADMAP Phase 2).
- 검증에서 **효과 없음**이 확인된 항(챔피언 시너지/카운터, 동라인 매치업, 상대 팀 성향, 포켓픽 밴 등)은 가중치 0으로 제거했습니다.
- 상세: [`ALGO-FIXES.md`](ALGO-FIXES.md), [`analysis/recommender-algorithm-review.md`](analysis/recommender-algorithm-review.md),
  [`analysis/harness-causal-findings.md`](analysis/harness-causal-findings.md)

## 저장소 구조

| 폴더 | 내용 |
|---|---|
| `tfm2_db_export/` | **제품** — 추천기 모드 본체(v4.1, stable API): `db`(경기 기록 → 통계) · `board`(밴픽 읽기) · `recommender` · `prior`(게임 AI 사전) · `ui`(표시) · `diag`(진단 파일) |
| `draft-overlay/` | 점수 엔진 `src/engine.rs`(`tfm2_db_export`가 `#[path]`로 공유) + 검증 하네스(`src/bin/validate-terms.rs`, `backtest.rs`) + 구 hudhook 오버레이 |
| `draft-weights-gen/` | 구 가중치 생성기(구 오버레이용) |
| `tfm2_bp_reader/` | 라이브 밴픽 읽기 프로토타입(classic) — `tfm2_db_export`에 통합됨 |
| `tfm2_patch_probe/` | classic 시절 패치 후 컴파일 카나리 |
| `bp_probe/`, `tfm2_bp2/`, `tfm2_ui_probe/`, `tfm2_aiscore/`, `tfm2_pool_reader/`, `draftwatch/`, `q/` | classic API 조사용 프로브 |
| `tfm2_draft_ai/` | 은퇴한 구 모드(드래프트 점수 후크) |
| `analysis/` | 알고리즘 심사·하네스 결과·바이너리 문자열 덤프 |

## 문서

- [`ROADMAP.md`](ROADMAP.md) — **stable API 이식 기준점**: 확인된 사실, v4.0 초안 점검, 결정 사항, 인게임 검증 절차, 이후 단계
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
