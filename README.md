# TFM2 Champion Creator

*Teamfight Manager 2*용 **챔피언 창작 도구** — 코딩 없이 새 챔피언을 만들어 게임에 추가하고 Steam 창작마당에 공유합니다.

> 영어 요약: a standalone desktop tool to create custom *Teamfight Manager 2* champions
> (role, stats, growth, tags, skills, sprite/icon) and emit a ready-to-load mod folder.

---

## 무엇인가요?

- **데스크톱 앱**(`creator-app`)에서 챔피언을 마우스로 만듭니다 → 게임이 읽는 **모드 폴더**가 생성됩니다.
- 게임을 재시작하면 만든 챔피언이 **밴픽 풀에 등장**하고 AI가 플레이합니다.
- 만든 챔피언은 게임에 동봉된 `TFM2ModUploader.exe`로 **창작마당에 업로드**할 수 있습니다.
- 공식 모드 업로더처럼 **별도 실행 프로그램**입니다(창작마당이 아니라 직접 받아 실행).

## 빠른 사용법

1. `champion-creator.exe`(릴리스 빌드: `creator-app/target/release/creator-app.exe`)를 실행합니다.
   - 게임 번들(`bundle.game_data`)을 자동으로 읽어 실제 챔피언 데이터를 참고로 불러옵니다.
2. 왼쪽 **추가**로 챔피언을 만들고 가운데서 편집:
   - **이름(한/영), 역할, 태그**
   - **능력치 Lv.1 + 레벨당 성장치** — 슬라이더 옆 괄호는 *실제 챔피언들의 범위*(감 잡기용)
   - **스프라이트 / 스킬 아이콘**: `랜덤` / `선택`(게임 기본 그림 재활용) / `업로드`(내 PNG)
   - **스킬 3개**: 템플릿(투사체·버프·돌진·힐·광역스턴·실드·스턴샷·넉백·독 등) + 수치 조정
3. 아래 **생성 (Generate)** → 출력 폴더에 모드가 만들어집니다(기본: 게임 `mods\tfm2_champion_creator`).
4. **게임 재시작** → 타이틀 화면 **Mods** 메뉴에서 활성화 → 연습 경기/새 게임에서 확인.
5. 공유하려면 `TFM2ModUploader.exe`로 그 모드 폴더를 창작마당에 업로드.

## 그림(에셋)은 어디서?

앱 상단 **"그림 구하기 (도움말)"** 버튼에 안내가 있습니다. 요점:
- **CC0** 라고 적힌 무료 그림이면 마음 편하게 씁니다(공유 OK, 출처표기 불필요).
- 캐릭터: [itch.io CC0 픽셀아트](https://itch.io/game-assets/assets-cc0/tag-pixel-art), [OpenGameArt CC0](https://opengameart.org/content/cc0-resources)
- 스킬 아이콘: [game-icons.net](https://game-icons.net/) (출처 한 줄 표기 조건)

## 현재 한계 (알아두기)

- **업로드 스프라이트는 정적 1프레임**입니다. *캐릭터 1명이 그려진 한 장* PNG(투명배경)만 제대로 들어갑니다.
  여러 동작이 격자로 든 **스프라이트시트는 아직 통째로 뭉개져** 보입니다(애니메이션 업로드는 추후).
- 가장 확실한 그림은 **`선택`(기존 챔피언 스프라이트)** — 애니메이션까지 완벽 동작.
- 스킬은 **정해진 템플릿 조합**입니다(임의 스크립팅 아님). 수치는 자유 조정.
- 어떤 그림/수치든 최종 확인은 **생성 → 게임에서 직접 확인**.

## 안전장치

- 생성 시 **자동 검증**: 잘못된 값/형식을 게임에 띄우기 전에 잡아냅니다(게임 재시작 낭비 방지).
- 능력치/성장치/쿨타임 기본값은 **게임 실제 데이터**에서 뽑은 값으로 시드됩니다.

---

## 개발자용 / 저장소 구조

| 폴더 | 내용 |
|---|---|
| `creator-app/` | 제품: egui 데스크톱 GUI (이걸 빌드/배포) |
| `champion-gen/` | 엔진: 번들 파싱 + 챔피언 생성 + 검증 (라이브러리 + `champion-gen` CLI). creator-app이 재사용 |
| `tfm2_combat_ai/` | 실험: 네이티브 Rust 챔피언(처형자) — 코드 모드 빌드 파이프라인 검증용 |
| `tfm2_champion_creator/` | Phase 0 수동 증명용 데이터 챔피언(엠벌링) |
| `sdk/mod-sdk/` | TFM2 Mod SDK 0.4.6 (네이티브 빌드용) |
| `analysis/` | 리버스 엔지니어링/설계 문서(내장 AI 구조, 행동 공간, 완성도 감사, 베이스 챔피언 데이터) |

### 빌드 (소스에서)

```powershell
# Rust nightly-2026-05-29 + MSVC 빌드도구 필요 (네이티브 코드 모드 빌드용; creator-app은 일반 toolchain로도 됨)
cd creator-app
cargo build --release      # -> target/release/creator-app.exe

cd ../champion-gen
cargo build --release      # CLI: champion-gen <spec.json> --out <mod_dir>  /  --check <file.data_champion>
```

CLI 생성기는 JSON 스펙으로도 챔피언을 찍어낼 수 있습니다(앱이 내부적으로 쓰는 형식). 예: `champion-gen/specs/champions.json`.

### 핵심 사실 (모딩)

- 게임 엔진은 **Rust**. 모드: 데이터(`.data_champion` JSON) 또는 네이티브 코드(`mod_api` cdylib).
- 챔피언 이름/설명 i18n은 base 텍스트에 `merge`가 **동작하지 않아**, 베이스 스냅샷+우리 키 합본을 `override`-교체합니다.
- 인게임 UI 모드 API는 매우 제한적(읽기+이벤트만, 노드 변경/그리기 불가) → 그래서 창작 도구는 **독립 데스크톱 앱**.
- 자세한 내용은 `analysis/` 문서 참고.
