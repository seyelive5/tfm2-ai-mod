//! backtest — does the recommender's winrate signal actually predict match winners?
//! For each match, score each team = Σ its champs' winrate deltas, predict the higher
//! score wins, measure accuracy. Compares:
//!   - champ-OVERALL vs per-LANE winrate   (does B1 / per-lane help?)
//!   - per-lane LATEST-only vs BLENDED      (does mixing the prev patch help thin data?)
//!   - HELD-OUT: prev-patch (0.0) winrates → predict current-patch (0.1) matches
//!     (the honest test that justifies blending — no circularity).
//! Reads champion_patch_statistics.tsv + match_replays.debug.txt.
//!
//! Usage: backtest <patch.tsv> <match_replays.debug.txt>

use std::collections::HashMap;

const ALPHA: f64 = 0.35; // prev-patch blend weight (must match the launcher)

// ---------- balanced-scan helpers ----------
fn read_balanced(b: &[u8], open_idx: usize, open: u8, close: u8) -> Option<(usize, usize)> {
    if open_idx >= b.len() || b[open_idx] != open {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open_idx;
    while i < b.len() {
        if b[i] == open {
            depth += 1;
        } else if b[i] == close {
            depth -= 1;
            if depth == 0 {
                return Some((open_idx, i));
            }
        }
        i += 1;
    }
    None
}
fn find_from(h: &[u8], n: &[u8], from: usize) -> Option<usize> {
    if n.is_empty() || from >= h.len() {
        return None;
    }
    let last = h.len().checked_sub(n.len())?;
    (from..=last).find(|&i| &h[i..i + n.len()] == n)
}
fn parse_bool(block: &str, name: &str) -> Option<bool> {
    let key = format!("{}: ", name);
    let rest = &block[block.find(&key)? + key.len()..];
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}
fn quoted(block: &str, field: &str) -> Option<String> {
    let key = format!("{}: \"", field);
    let rest = &block[block.find(&key)? + key.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
/// `field: Ident` (an enum variant, unquoted).
fn enum_field(block: &str, field: &str) -> Option<String> {
    let key = format!("{}: ", field);
    let rest = &block[block.find(&key)? + key.len()..];
    let end = rest.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(rest.len());
    Some(rest[..end].to_string())
}
fn extract_array<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{}: [", name);
    let kidx = block.find(&key)?;
    let bracket = kidx + key.len() - 1;
    let (s, e) = read_balanced(block.as_bytes(), bracket, b'[', b']')?;
    Some(&block[s..=e])
}
fn lane_idx(p: &str) -> Option<u8> {
    match p {
        "Top" => Some(0),
        "Jungle" => Some(1),
        "Mid" => Some(2),
        "Bottom" => Some(3),
        "Support" => Some(4),
        _ => None,
    }
}
fn shrink(w: f64, m: f64) -> f64 {
    (w + 4.0) / (m + 8.0) - 0.5
}

// ---------- winrate tables ----------
struct Tables {
    overall: HashMap<String, f64>,          // champ -> blended delta
    lane_latest: HashMap<(String, u8), f64>, // current patch only
    lane_blend: HashMap<(String, u8), f64>,  // current + α·prev
    lane_prev: HashMap<(String, u8), f64>,   // prev patch only (held-out predictor)
    latest: String,
}

fn build_tables(tsv: &str) -> Tables {
    let rows: Vec<Vec<&str>> =
        tsv.lines().skip(1).map(|l| l.split('\t').collect::<Vec<_>>()).filter(|f| f.len() >= 7).collect();
    let mut vers: Vec<&str> = rows.iter().map(|f| f[0]).collect();
    // NUMERIC version sort — lexical breaks at two-digit components ("2026.10.0" < "2026.2.0")
    let vk = |v: &str| v.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect::<Vec<u32>>();
    vers.sort_unstable_by(|a, b| vk(a).cmp(&vk(b)));
    vers.dedup();
    let latest = vers.last().copied().unwrap_or("").to_string();
    let prev = if vers.len() >= 2 { Some(vers[vers.len() - 2].to_string()) } else { None };

    // (champ,lane) -> (w,m) per regime
    let mut lat: HashMap<(String, u8), (f64, f64)> = HashMap::new();
    let mut prv: HashMap<(String, u8), (f64, f64)> = HashMap::new();
    let mut ov: HashMap<String, (f64, f64)> = HashMap::new(); // blended overall
    for f in &rows {
        let li = match lane_idx(f[3]) {
            Some(l) => l,
            None => continue,
        };
        let w: f64 = f[5].parse().unwrap_or(0.0);
        let m: f64 = f[6].parse().unwrap_or(0.0);
        let is_latest = f[0] == latest;
        let is_prev = Some(f[0].to_string()) == prev;
        if is_latest {
            let e = lat.entry((f[2].into(), li)).or_insert((0.0, 0.0));
            e.0 += w;
            e.1 += m;
        }
        if is_prev {
            let e = prv.entry((f[2].into(), li)).or_insert((0.0, 0.0));
            e.0 += w;
            e.1 += m;
        }
        let wgt = if is_latest {
            1.0
        } else if is_prev {
            ALPHA
        } else {
            0.0
        };
        if wgt > 0.0 {
            let e = ov.entry(f[2].into()).or_insert((0.0, 0.0));
            e.0 += wgt * w;
            e.1 += wgt * m;
        }
    }
    let lane_latest = lat.iter().map(|(k, (w, m))| (k.clone(), shrink(*w, *m))).collect();
    let lane_prev = prv.iter().map(|(k, (w, m))| (k.clone(), shrink(*w, *m))).collect();
    let overall = ov.iter().map(|(k, (w, m))| (k.clone(), shrink(*w, *m))).collect();
    // blended per-lane = latest + α·prev
    let mut lb: HashMap<(String, u8), (f64, f64)> = HashMap::new();
    for (k, (w, m)) in &lat {
        let e = lb.entry(k.clone()).or_insert((0.0, 0.0));
        e.0 += w;
        e.1 += m;
    }
    for (k, (w, m)) in &prv {
        let e = lb.entry(k.clone()).or_insert((0.0, 0.0));
        e.0 += ALPHA * w;
        e.1 += ALPHA * m;
    }
    let lane_blend = lb.iter().map(|(k, (w, m))| (k.clone(), shrink(*w, *m))).collect();
    Tables { overall, lane_latest, lane_blend, lane_prev, latest }
}

// ---------- matches ----------
struct Match {
    blue: Vec<(String, u8)>,
    red: Vec<(String, u8)>,
    blue_win: bool,
    ver: String,
}

fn parse_team(arr: &str) -> Vec<(String, u8)> {
    let b = arr.as_bytes();
    let needle = b"MatchReplayAthlete {";
    let mut out = Vec::new();
    let mut off = 0;
    while let Some(start) = find_from(b, needle, off) {
        let brace = start + needle.len() - 1;
        let (s, e) = match read_balanced(b, brace, b'{', b'}') {
            Some(v) => v,
            None => break,
        };
        let blk = &arr[s..=e];
        off = e + 1;
        if let (Some(ch), Some(pos)) = (quoted(blk, "champion"), enum_field(blk, "position")) {
            if let Some(li) = lane_idx(&pos) {
                out.push((ch, li));
            }
        }
    }
    out
}

fn parse_matches(text: &str) -> Vec<Match> {
    let b = text.as_bytes();
    let needle = b"MatchReplayData {";
    let mut out = Vec::new();
    let mut off = 0;
    while let Some(start) = find_from(b, needle, off) {
        let brace = start + needle.len() - 1;
        let (s, e) = match read_balanced(b, brace, b'{', b'}') {
            Some(v) => v,
            None => break,
        };
        let block = &text[s..=e];
        off = e + 1;
        let blue_win = match parse_bool(block, "blue_team_win") {
            Some(v) => v,
            None => continue,
        };
        let ver = quoted(block, "version").unwrap_or_default();
        let blue = extract_array(block, "blue_team").map(parse_team).unwrap_or_default();
        let red = extract_array(block, "red_team").map(parse_team).unwrap_or_default();
        if blue.is_empty() || red.is_empty() {
            continue;
        }
        out.push(Match { blue, red, blue_win, ver });
    }
    out
}

// ---------- scoring ----------
fn team_score(team: &[(String, u8)], lane: &HashMap<(String, u8), f64>, overall: &HashMap<String, f64>) -> f64 {
    team.iter()
        .map(|(c, li)| lane.get(&(c.clone(), *li)).copied().or_else(|| overall.get(c).copied()).unwrap_or(0.0))
        .sum()
}

/// accuracy of "higher team score wins" over the given matches.
fn accuracy(matches: &[&Match], lane: &HashMap<(String, u8), f64>, overall: &HashMap<String, f64>) -> (f64, usize) {
    let mut correct = 0.0;
    for m in matches {
        let bs = team_score(&m.blue, lane, overall);
        let rs = team_score(&m.red, lane, overall);
        let pred_blue = bs > rs;
        if (bs - rs).abs() < 1e-9 {
            correct += 0.5;
        } else if pred_blue == m.blue_win {
            correct += 1.0;
        }
    }
    (correct / matches.len().max(1) as f64, matches.len())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: backtest <patch.tsv> <match_replays.debug.txt>");
        std::process::exit(2);
    }
    let t = build_tables(&std::fs::read_to_string(&args[1]).unwrap_or_default());
    let matches = parse_matches(&std::fs::read_to_string(&args[2]).unwrap_or_default());
    let empty: HashMap<(String, u8), f64> = HashMap::new();
    let refs: Vec<&Match> = matches.iter().collect();
    let cur: Vec<&Match> = matches.iter().filter(|m| m.ver == t.latest).collect();
    let base_blue = matches.iter().filter(|m| m.blue_win).count() as f64 / matches.len().max(1) as f64;

    println!("=== backtest ===");
    println!("matches: {} total · {} current-patch ({})", matches.len(), cur.len(), t.latest);
    println!("baseline (always-blue): {:.1}%   (so >50.x% only is meaningful)\n", base_blue * 100.0);

    println!("ALL matches — does per-lane help? (in-sample, directional)");
    let (a_ov, n) = accuracy(&refs, &empty, &t.overall);
    let (a_ll, _) = accuracy(&refs, &t.lane_latest, &t.overall);
    let (a_lb, _) = accuracy(&refs, &t.lane_blend, &t.overall);
    println!("  champ-overall : {:.1}%  (n={})", a_ov * 100.0, n);
    println!("  per-lane(now) : {:.1}%", a_ll * 100.0);
    println!("  per-lane+blend: {:.1}%", a_lb * 100.0);

    println!("\nCURRENT-patch matches — does blending the prev patch help thin data?");
    let (c_ll, cn) = accuracy(&cur, &t.lane_latest, &t.overall);
    let (c_lb, _) = accuracy(&cur, &t.lane_blend, &t.overall);
    // HELD-OUT: predict current matches with PREV-patch winrates only (no circularity)
    let (c_prev, _) = accuracy(&cur, &t.lane_prev, &t.overall);
    println!("  per-lane latest-only : {:.1}%  (n={}, thin/in-sample)", c_ll * 100.0, cn);
    println!("  per-lane blended     : {:.1}%", c_lb * 100.0);
    println!("  prev-patch ONLY (held-out, honest): {:.1}%  <- >50% ⇒ old data predicts new ⇒ blending justified", c_prev * 100.0);
}
