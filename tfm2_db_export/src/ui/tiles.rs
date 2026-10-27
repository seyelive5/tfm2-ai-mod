//! Per champion tile, in its free top-left corner (the game's ban / pick-order /
//! fearless marks sit top-right, its own `#tier` label at x6 y68): a rank badge
//! in the turn color with a tier chip beside it, and an outline ring.

use super::cards::{mode, tier_rgb, turn_rgb};
use super::{UiState, View};
use crate::recommender::Output;
use mod_api_stable::*;
use std::collections::HashSet;

pub const BADGE_X: i32 = 6;
pub const CHIP_BESIDE_X: i32 = 30;

#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    /// tier letter + change mark
    pub text: String,
    /// rgba hex
    pub ink: String,
    pub x: i32,
    pub w: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TileLook {
    /// outline ring color (rgb), None = off
    pub ring: Option<&'static str>,
    /// rank badge: (digit, fill rgb)
    pub badge: Option<(String, &'static str)>,
    pub chip: Option<Chip>,
}

/// `dim` = draw unsure letters dimmed (only while some letters are sure).
pub fn desired(champ: &str, out: Option<&Output>, taken: &HashSet<String>, dim: bool) -> TileLook {
    let Some(o) = out.filter(|_| !taken.contains(champ)) else {
        return TileLook::default();
    };
    let turn = turn_rgb(mode(Some(o)));
    let rank = o.recs.iter().position(|(id, _, _)| id == champ);
    let chip = o.tiers.get(champ).and_then(|m| {
        let text = format!("{}{}", m.letter, m.chg);
        (!text.is_empty()).then(|| Chip {
            ink: format!("{}{}", tier_rgb(m.letter), if dim && m.unsure { "80" } else { "ff" }),
            x: if rank.is_some() { CHIP_BESIDE_X } else { BADGE_X },
            w: 8 + 12 * text.chars().count() as i32,
            text,
        })
    });
    TileLook { ring: rank.map(|_| turn), badge: rank.map(|r| ((r + 1).to_string(), turn)), chip }
}

// ── nodes

const RING: &str = "tfm2dc_ring";
const BADGE: &str = "tfm2dc_rk";
const CHIP: &str = "tfm2dc_tier";

const RING_SRC: &str = r#"tfm2dc_ring:color {
  width: 100%;
  height: 100%;
  stroke: 3;
  color: #37d5b3ff;
  back_color: #00000000;
  rounding: Uniform {
    rounding: 12;
  }
  visible: false;
  ignore_event: true;
}"#;

/// dark digits: white on the bright turn colors reads at ~2:1
const BADGE_SRC: &str = r#"tfm2dc_rk:color {
  x: 6px;
  y: 6px;
  width: 22px;
  height: 22px;
  color: #37d5b3ff;
  rounding: Uniform {
    rounding: 6;
  }
  visible: false;
  ignore_event: true;
  #t:label {
    @"asset/base/style/main#bold_label";
    width: 100%;
    height: 100%;
    size: 14;
    align_x: Center;
    align_y: Center;
    color: #0f1016ff;
    ignore_event: true;
    text: "";
  }
}"#;

const CHIP_SRC: &str = r#"tfm2dc_tier:color {
  x: 6px;
  y: 6px;
  width: 20px;
  height: 22px;
  color: #0f1016eb;
  rounding: Uniform {
    rounding: 6;
  }
  visible: false;
  ignore_event: true;
  #t:label {
    @"asset/base/style/main#bold_label";
    width: 100%;
    height: 100%;
    size: 13;
    align_x: Center;
    align_y: Center;
    color: #e8e8e8ff;
    ignore_event: true;
    text: "";
  }
}"#;

/// Every on-screen tile: spawn our three nodes when missing, write what changed.
pub(super) fn stamp(ctx: &mut StableClient, st: &mut UiState, view: &View, dim: bool) {
    for champ in &view.tiles {
        let base = format!("main.champions.contents.{}", champ);
        let [ring, badge, chip] = [RING, BADGE, CHIP].map(|n| format!("{}.{}", base, n));
        if !ctx.ui_exists(&chip) {
            // new screen or a rebuilt tile: (re)spawn, forget what we applied
            st.tiles.remove(champ);
            if let Some(at) = st.tile_fail.get(champ) {
                if st.frame < at + 120 {
                    continue;
                }
            }
            st.spawns += 1;
            let ok = [(&ring, RING_SRC), (&badge, BADGE_SRC), (&chip, CHIP_SRC)]
                .iter()
                .all(|(p, src)| ctx.ui_exists(p) || ctx.ui_spawn_source(&base, src));
            if !ok {
                st.spawn_fails += 1;
                st.tile_fail.insert(champ.clone(), st.frame);
                continue;
            }
            st.tile_fail.remove(champ);
        }
        if !st.tiles.contains_key(champ) {
            // fresh nodes, or ours from a screen the game kept alive: their state
            // is unknown to this cache → all off, then apply below
            for p in [&ring, &badge, &chip] {
                st.ap.put_vis(ctx, p, false);
            }
            st.tiles.insert(champ.clone(), TileLook::default());
        }
        let want = desired(champ, view.out.as_deref(), &view.taken, dim);
        let Some(have) = st.tiles.get(champ).cloned() else { continue };
        if have == want {
            continue;
        }
        let ap = &mut st.ap;
        if have.ring != want.ring {
            match want.ring {
                None => {
                    ap.put_vis(ctx, &ring, false);
                }
                Some(c) => {
                    ap.put_props(ctx, &ring, &format!("color: #{}ff;", c));
                    ap.put_vis(ctx, &ring, true);
                }
            }
        }
        if have.badge != want.badge {
            match &want.badge {
                None => {
                    ap.put_vis(ctx, &badge, false);
                }
                Some((digit, c)) => {
                    ap.put_props(ctx, &badge, &format!("color: #{}ff;", c));
                    ap.put_text(ctx, &format!("{}.t", badge), digit);
                    ap.put_vis(ctx, &badge, true);
                }
            }
        }
        if have.chip != want.chip {
            match &want.chip {
                None => {
                    ap.put_vis(ctx, &chip, false);
                }
                Some(c) => {
                    ap.put_props(ctx, &chip, &format!("x: {}px; width: {}px;", c.x, c.w));
                    let label = format!("{}.t", chip);
                    ap.put_props(ctx, &label, &format!("color: #{};", c.ink));
                    ap.put_text(ctx, &label, &c.text);
                    ap.put_vis(ctx, &chip, true);
                }
            }
        }
        st.tiles.insert(champ.clone(), want);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommender::TierMark;

    fn out(rec_ban: bool, ranked: &[&str], tiers: &[(&str, &'static str, bool, &'static str)]) -> Output {
        Output {
            rec_ban,
            recs: ranked.iter().map(|id| (id.to_string(), String::new(), "+1".to_string())).collect(),
            tiers: tiers
                .iter()
                .map(|(id, letter, unsure, chg)| (id.to_string(), TierMark { letter, unsure: *unsure, chg }))
                .collect(),
            ..Default::default()
        }
    }

    fn free() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn taken_or_unread_tiles_stay_bare() {
        let o = out(false, &["monk"], &[("monk", "S", false, "")]);
        let taken: HashSet<String> = ["monk".to_string()].into();
        assert_eq!(desired("monk", Some(&o), &taken, false), TileLook::default());
        assert_eq!(desired("monk", None, &free(), false), TileLook::default());
    }

    #[test]
    fn ranked_tile_gets_ring_badge_and_the_chip_beside_it() {
        let o = out(false, &["ogre", "monk"], &[("monk", "S", false, "▲")]);
        let want = TileLook {
            ring: Some("37d5b3"),
            badge: Some(("2".to_string(), "37d5b3")),
            chip: Some(Chip { text: "S▲".into(), ink: "fad33eff".into(), x: CHIP_BESIDE_X, w: 32 }),
        };
        assert_eq!(desired("monk", Some(&o), &free(), false), want);
    }

    #[test]
    fn ban_turn_marks_in_the_ban_color() {
        let o = out(true, &["monk"], &[]);
        let l = desired("monk", Some(&o), &free(), false);
        assert_eq!((l.ring, l.badge), (Some("a974ff"), Some(("1".to_string(), "a974ff"))));
        assert_eq!(l.chip, None);
    }

    #[test]
    fn unranked_tile_keeps_its_chip_in_the_corner() {
        let o = out(false, &["ogre"], &[("monk", "B", false, "")]);
        let l = desired("monk", Some(&o), &free(), false);
        assert_eq!((l.ring, l.badge), (None, None));
        assert_eq!(l.chip, Some(Chip { text: "B".into(), ink: "e8e8e8ff".into(), x: BADGE_X, w: 20 }));
    }

    #[test]
    fn unsure_letter_dims_only_while_dim() {
        let o = out(false, &[], &[("monk", "A", true, "")]);
        assert_eq!(desired("monk", Some(&o), &free(), true).chip.unwrap().ink, "5ce0e680");
        assert_eq!(desired("monk", Some(&o), &free(), false).chip.unwrap().ink, "5ce0e6ff");
    }

    #[test]
    fn a_change_mark_alone_still_tags_the_tile() {
        let o = out(false, &[], &[("monk", "", false, "▼")]);
        let c = desired("monk", Some(&o), &free(), false).chip.unwrap();
        assert_eq!((c.text.as_str(), c.w), ("▼", 20));
    }
}
