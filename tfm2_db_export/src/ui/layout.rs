//! Where the two cards sit (0.6.5 banpick layout, 1920×1080) — see the ui module
//! doc for why these spots.

pub const CARD_W: i32 = 427;
pub const CARD_H: i32 = 200;
/// the "hidden — Tab" hint, bottom-right inside the rank card's spot
pub const HINT_W: i32 = 124;
pub const HINT_H: i32 = 22;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spots {
    pub rank: (i32, i32),
    pub note: (i32, i32),
}

/// champion_info (x310–1610, y690–970): its three skill boxes start at these x, y770
const LEFT_X: i32 = 310;
const MID_X: i32 = 747;
const RIGHT_X: i32 = 1183;
const SKILL_Y: i32 = 770;
/// swap screen (x310–1610): the blue table spans y60–471.5, the red one y481.5–893
const BLUE_TABLE_TOP: i32 = 62;
const RED_TABLE_TOP: i32 = 484;
const GAP: i32 = 8;

/// `swap`: the swap screen is up; `red`: my side is red (unresolved = blue).
pub fn spots(swap: bool, red: bool) -> Spots {
    if swap {
        let top = if red { BLUE_TABLE_TOP } else { RED_TABLE_TOP };
        Spots { rank: (RIGHT_X, top), note: (RIGHT_X, top + CARD_H + GAP) }
    } else {
        Spots { rank: (MID_X, SKILL_Y), note: (if red { LEFT_X } else { RIGHT_X }, SKILL_Y) }
    }
}

pub fn hint(s: &Spots) -> (i32, i32) {
    (s.rank.0 + CARD_W - HINT_W - GAP, s.rank.1 + CARD_H - HINT_H - GAP)
}

/// The spots relative to the node the cards hang from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub parent: &'static str,
    pub rank: (i32, i32),
    pub note: (i32, i32),
    pub hint: (i32, i32),
}

/// champion_info's top-left on screen (x centred: 1920/2 − 1300/2)
const INFO_ORIGIN: (i32, i32) = (310, 690);

/// Draft: under champion_info, a plain absolute-layout `empty`, so the game's
/// later `main` children (tooltips, popups, the phase banner) still draw over the
/// cards. Swap: `main.swap` and its tables stack their children (TopToBottom),
/// so the cards hang from `main` itself.
pub fn place(swap: bool, red: bool) -> Place {
    let s = spots(swap, red);
    let (parent, (ox, oy)) = if swap { ("main", (0, 0)) } else { ("main.champion_info", INFO_ORIGIN) };
    let rel = |(x, y): (i32, i32)| (x - ox, y - oy);
    Place { parent, rank: rel(s.rank), note: rel(s.note), hint: rel(hint(&s)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlap(a: (i32, i32), b: (i32, i32)) -> bool {
        a.0 < b.0 + CARD_W && b.0 < a.0 + CARD_W && a.1 < b.1 + CARD_H && b.1 < a.1 + CARD_H
    }

    #[test]
    fn draft_rank_card_takes_the_middle_skill_box_on_both_sides() {
        assert_eq!(spots(false, true).rank, (747, 770));
        assert_eq!(spots(false, false).rank, (747, 770));
    }

    #[test]
    fn draft_note_card_sits_at_the_end_away_from_my_coach() {
        assert_eq!(spots(false, true).note, (310, 770));
        assert_eq!(spots(false, false).note, (1183, 770));
    }

    #[test]
    fn draft_note_card_clears_my_coach_toasts() {
        // toasts stack 520px wide from my side's bottom corner (red x≥1388, blue x≤532)
        assert!(spots(false, true).note.0 + CARD_W <= 1388);
        assert!(spots(false, false).note.0 >= 532);
    }

    #[test]
    fn swap_cards_stack_on_the_opponent_tables_right_end() {
        // red → the blue table on top (y60–471.5); blue → the red table (y481.5–893)
        assert_eq!(spots(true, true), Spots { rank: (1183, 62), note: (1183, 270) });
        assert_eq!(spots(true, false), Spots { rank: (1183, 484), note: (1183, 692) });
    }

    #[test]
    fn swap_cards_stay_inside_the_opponent_table() {
        let s = spots(true, true);
        assert!(s.rank.1 >= 60 && s.note.1 + CARD_H <= 472);
        let s = spots(true, false);
        assert!(s.rank.1 >= 481 && s.note.1 + CARD_H <= 893);
    }

    #[test]
    fn cards_never_overlap_each_other() {
        for swap in [false, true] {
            for red in [false, true] {
                let s = spots(swap, red);
                assert!(!overlap(s.rank, s.note), "swap={} red={}", swap, red);
            }
        }
    }

    #[test]
    fn draft_cards_take_the_skill_box_slots_inside_champion_info() {
        // champion_info's own children (0.6.5): skill1 x0, skill2 x437, ult x873 — all y80
        let p = place(false, true);
        assert_eq!(p.parent, "main.champion_info");
        assert_eq!((p.rank, p.note), ((437, 80), (0, 80)));
        assert_eq!(place(false, false).note, (873, 80));
    }

    #[test]
    fn swap_cards_hang_from_main_at_their_screen_spots() {
        for red in [false, true] {
            let (p, s) = (place(true, red), spots(true, red));
            assert_eq!((p.parent, p.rank, p.note, p.hint), ("main", s.rank, s.note, hint(&s)));
        }
    }

    #[test]
    fn hint_keeps_its_spot_relative_to_the_rank_card() {
        for swap in [false, true] {
            for red in [false, true] {
                let (p, s) = (place(swap, red), spots(swap, red));
                let h = hint(&s);
                assert_eq!((p.hint.0 - p.rank.0, p.hint.1 - p.rank.1), (h.0 - s.rank.0, h.1 - s.rank.1));
            }
        }
    }

    #[test]
    fn hint_sits_inside_the_rank_spot() {
        for swap in [false, true] {
            for red in [false, true] {
                let s = spots(swap, red);
                let (x, y) = hint(&s);
                assert!(x >= s.rank.0 && x + HINT_W <= s.rank.0 + CARD_W);
                assert!(y >= s.rank.1 && y + HINT_H <= s.rank.1 + CARD_H);
            }
        }
    }
}
