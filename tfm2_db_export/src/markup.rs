//! The game's inline label markup (0.6.5 text bundle — skill descriptions use it):
//! `<#rrggbbaa>text<>` colors a span, `<i#asset/path:tag>` places an inline icon.
//! Commentary is built with it so it reads like the game's own descriptions;
//! logs and width budgets use the plain text.

/// One switch for every span we emit: if the host does not parse markup in text
/// a mod sets, the codes would show as literal `<#…>` — flip this and rebuild.
pub const ON: bool = true;

/// champion names — the game's accent teal
pub const NAME: &str = "37d5b3";
/// scores / counts — the game's amber (durations, ranges)
pub const NUM: &str = "ffb900";
/// warnings — the game's crowd-control red
pub const WARN: &str = "ef5350";
/// secondary lines (data status, legends)
pub const DIM: &str = "7d8290";

pub fn paint(rgb: &str, s: &str) -> String {
    if ON {
        format!("<#{}ff>{}<>", rgb, s)
    } else {
        s.to_string()
    }
}

pub fn name(s: &str) -> String {
    paint(NAME, s)
}

pub fn num(s: &str) -> String {
    paint(NUM, s)
}

pub fn warn(s: &str) -> String {
    paint(WARN, s)
}

pub fn dim(s: &str) -> String {
    paint(DIM, s)
}

/// The text as displayed, markup removed.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let b = tail.as_bytes();
        let skip = if tail.starts_with("<>") {
            2
        } else if b.len() >= 11 && b[1] == b'#' && b[2..10].iter().all(u8::is_ascii_hexdigit) && b[10] == b'>' {
            11
        } else if tail.starts_with("<i#") {
            tail.find('>').map_or(0, |j| j + 1)
        } else {
            0
        };
        if skip == 0 {
            out.push('<');
            rest = &tail[1..];
        } else {
            rest = &tail[skip..];
        }
    }
    out.push_str(rest);
    out
}

/// Display width of the visible text in half-em units (ASCII 1, other 2).
pub fn width(s: &str) -> usize {
    plain(s).chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_wraps_span_in_opaque_color_while_on() {
        let want = if ON { "<#37d5b3ff>격투가<>" } else { "격투가" };
        assert_eq!(paint("37d5b3", "격투가"), want);
    }

    #[test]
    fn plain_strips_color_spans() {
        assert_eq!(plain("1순위 <#37d5b3ff>격투가<>, 평점 <#ffb900ff>+8<>."), "1순위 격투가, 평점 +8.");
    }

    #[test]
    fn plain_strips_inline_icons() {
        assert_eq!(plain("0 + <i#asset/base/ui/banpick/champion_stat_icon:ad_0>공격력"), "0 + 공격력");
    }

    #[test]
    fn plain_keeps_a_lone_angle_bracket() {
        assert_eq!(plain("a < b, c <d"), "a < b, c <d");
    }

    #[test]
    fn width_counts_visible_text_only() {
        assert_eq!(width("<#37d5b3ff>격투가<> +8"), 6 + 3);
    }
}
