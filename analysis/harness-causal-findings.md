# 하네스 인과화(Step 0-1) 결과 — 프로젝트 #1 신호가 누출 아티팩트였다 (2026-07-05)

`ALGO-FIXES.md` Step 0-1 실행 결과. `validate-terms`의 폴드를 비인과 `i%5` 인터리브에서 **인과적 rolling-origin**(버전 v를 테스트할 때 v 이전 패치로만 훈련)으로 바꾸고 동일 데이터(4,488 리플레이, 8 패치)에서 재실행했다.

## 리팩터 신뢰성: `--legacy`가 구 수치를 비트 단위 재현
`validate-terms --legacy`(옛 `i%5`)가 리팩터 전 베이스라인과 **정확히 일치**(base_lane 59.4%, famg sweep 62.8/64.8/66.3/67.2, base_lane+0.08famg 66.3%). 따라서 rolling-origin(기본) 수치는 리팩터 버그가 아니라 실측이다.

## 핵심 결과 — 표 (i%5 비인과 → rolling-origin 인과; noise ±0.8%p)

| 항 | i%5 (구) | rolling-origin | 판정 |
|---|---|---|---|
| base_lane 단독 | 59.4% | **59.0%** | 견고 — 진짜 신호 (−0.4는 origin당 훈련량 감소분) |
| **famg** 단독 (games 숙련) | 64.8% | **52.4%** | **붕괴 −12.4%p** |
| base_lane + 0.08·**famg** | **66.3%** | **57.8%** | **+6.9%p 리프트 → −1.2%p 해악** (부호 반전) |
| famg sweep (0.02→0.16) | 62.8→**67.2** 상승 | 59.1→**56.6** 하강 | 단조 반전 — 많이 쓸수록 나빠짐 |
| famgc | 62.9 / 66.0 | 51.6 / 57.2 | 동반 붕괴 |
| **fam_c** 단독 (중심화 승률) | 57.7% | **56.3%** | 견고 |
| base_lane + 0.25·**fam_c** | 61.4% | **60.0%** | **여전히 +1.0%p — 유일하게 살아남는 fam 변종** |
| syn / cnt / mu | 60.x 증분 | ~59.x (평탄) | 무증분 재확인 |
| lp | 59.9 | 59.0 | 무증분 (누출 캐비어트 있던 항 — 사멸 확정) |
| aip | 50.0 | 50.0 | 불변 무용 (S1-1과 일관) |
| comp / fit | 무증분 | 무증분 | 불변 |

## 해석 — 왜 famg만 붕괴하나 (스모킹건)
`famg = ln(1 + 그 선수의 그 챔프 게임수)`. `i%5`는 홀드아웃 매치의 **미래** 폴드로 테이블을 만든다 → 한 선수가 커리어 전체에 걸쳐 그 챔프를 **주력으로 삼는다는 사실**(미래 게임 포함)이 초기 매치 시점의 "게임수"에 누출된다. "이 선수는 이 챔프를 결국 많이 한다 → 잘한다"를 사실상 컨닝. rolling-origin은 매치 **이전** 게임만 세므로 진짜 사전 경험만 남고, 그건 승리를 예측하지 못한다.

**차등이 결정적**: 부피 신호(famg, 미래 누출 가능) 붕괴 vs 성능 신호(fam_c = 챔프 승률 − 선수 전체 승률, 누출 내성) 견고. 같은 "숙련" 축인데 누출 가능한 것만 무너졌다. 이것이 노이즈/훈련량이 아니라 **누출**임을 증명한다 (base_lane 일반 하락은 −0.4%p인데 famg 증분은 −8%p 스윙).

## 프로젝트에 대한 함의 (중대)
- **PLAN.md와 코드가 "검증된 최강 신호(famg, +7.9%p 증분)"로 취급하고 `af()` 프로듀서(0.020·famg + 0.15·famc)의 지배 성분으로 출하한 신호가 인과적으로는 무가치(또는 해악)다.** 07-04 리뷰(F10)가 "숙련은 검증된 SIGNAL"이라 한 것도 이 누출 위에 있었다 — 부분 정정 필요.
- **살아남는 그림**: `base_lane`가 신호, `fam_c`가 소폭 실재 리프트(+1.0%p), 나머지(famg/syn/cnt/mu/lp/comp/aip/fit) 전부 무증분. 훨씬 겸손하고 정직한 모델.
- **Step 0가 게이트인 이유를 증명**: 하네스가 거짓말하고 있었고, 가장 중요한 것에 대해 거짓말했다. 이 수치 없이 가중치를 만졌으면 famg를 더 키웠을 것.

## 새 백로그 항목 → **F15: fam 재가중** (ALGO-FIXES Step 2로)
`af()` 프로듀서(tfm2_db_export/src/lib.rs:102-146)를 인과 증거에 맞춰 재가중:
- **famg 성분(0.020·ln games)을 ~0으로** — 인과적으로 리프트 없음/해악.
- **famc 성분(0.15·centered-wr) 유지/소폭 상향** — 유일한 인과 생존 fam 신호(base_lane+0.25famc = +1.0%p 인과).
- 캡·정확 계수는 S0-4(사전등록 6모델 + 부트스트랩 CI) + rolling-origin sweep으로 확정. **famc 스윕을 rolling-origin에서 재실행**해 인과 최적 스케일을 잡을 것(현재 표는 0.25 한 점만).
- engine.rs af() doc + score_pick_in "validated strongest signal" 주석을 다시 정정(현재 S1-6이 "SIGNAL은 검증"이라 했으나, 그 검증도 누출 → "games 성분은 누출 아티팩트, 인과 생존은 centered-wr만").
- PLAN.md의 fam 판정(66% 콤보, "familiarity 최강")을 인과 결과로 갱신.

## 남은 Step 0
- **S0-2 재캡처**(게임 세션 필요 — 사용자): 07-04 이후 debug 덤프 게이트로 최신 2패치 누락. `probe\DEBUG_DUMPS` 마커 후 1세션 → ~5,000판/9버전. rolling-origin은 origin이 많을수록·초기 훈련량이 클수록 정확해짐(현재 첫 origin은 훈련량 적어 노이즈↑).
- **S0-3 프로덕션 수학 이식**: 하네스 base가 아직 κ8. 프로덕션 κ40/κ20/0.35블렌드/저작리셋 이식 후 재평가(그래야 하네스가 출하 모델을 검증). famc 재가중 확정 전 필수.
- **S0-4 사전등록 6모델 + 부트스트랩 CI**: 채택 게이트.

## 재현
```
cd draft-overlay; cargo run --release --bin validate-terms -- <match_replays.debug.txt> src/draft_weights.tsv <champion_patch_statistics.tsv>
#   --legacy 붙이면 구 i%5. 전체 출력: analysis/harness-rolling-origin.txt, analysis/harness-legacy-i5.txt
```
캐비어트: rolling-origin은 origin당 훈련이 과거로 한정돼 초기 origin이 얇다(전반적 −0.4%p). 8버전은 성긴 인과 분할 — 날짜별/leave-past-out으로 더 데이터 효율적으로 만들 수 있으나 방향(famg 누출)은 명확·강건.
