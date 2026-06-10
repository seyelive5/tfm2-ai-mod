//! validate-terms — HONEST holdout test of each scoring term's predictive power.
//!
//! Splits completed matches into TRAIN (80%) / TEST (20%). Builds champ-winrate,
//! synergy, and counter tables from TRAIN only. On TEST, scores each team by each
//! term and predicts the higher-scoring team wins; reports accuracy. Comp (taxonomy
//! completeness) needs no training. This tells us which terms ACTUALLY predict wins
//! → so weights can be set from data instead of guesses.
//!
//! Usage: validate-terms <match_replays.debug.txt> <draft_weights.tsv>
//!   (draft_weights.tsv is only for T= taxonomy rows → comp score)

use std::collections::HashMap;

// ---------- balanced-scan parse (same as backtest) ----------
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
fn extract_array<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{}: [", name);
    let kidx = block.find(&key)?;
    let bracket = kidx + key.len() - 1;
    let (s, e) = read_balanced(block.as_bytes(), bracket, b'[', b']')?;
    Some(&block[s..=e])
}

struct Match {
    blue: Vec<String>,
    red: Vec<String>,
    blue_win: bool,
}

fn parse_team(arr: &str) -> Vec<String> {
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
        if let Some(ch) = quoted(blk, "champion") {
            out.push(ch);
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
        let blue = extract_array(block, "blue_team").map(parse_team).unwrap_or_default();
        let red = extract_array(block, "red_team").map(parse_team).unwrap_or_default();
        if blue.len() < 2 || red.len() < 2 {
            continue;
        }
        out.push(Match { blue, red, blue_win });
    }
    out
}

// ---------- taxonomy (T rows) for comp ----------
struct Taxon {
    category: String,
    damage: String,
    tags: Vec<String>,
}
fn parse_taxonomy(tsv: &str) -> HashMap<String, Taxon> {
    let mut m = HashMap::new();
    for line in tsv.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.first() == Some(&"T") && f.len() >= 5 {
            m.insert(
                f[1].to_string(),
                Taxon {
                    category: f[2].to_string(),
                    damage: f[3].to_string(),
                    tags: f[4].split(',').map(|s| s.to_string()).collect(),
                },
            );
        }
    }
    m
}

// ---------- WL accumulator ----------
#[derive(Default, Clone, Copy)]
struct WL {
    w: f64,
    g: f64,
}
impl WL {
    fn rec(&mut self, won: bool) {
        self.g += 1.0;
        if won {
            self.w += 1.0;
        }
    }
    fn delta(&self, k: f64) -> f64 {
        (self.w + k * 0.5) / (self.g + k) - 0.5
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: validate-terms <match_replays.debug.txt> [draft_weights.tsv]");
        std::process::exit(2);
    }
    let matches = parse_matches(&std::fs::read_to_string(&args[1]).unwrap_or_default());
    let taxon = if args.len() >= 3 {
        parse_taxonomy(&std::fs::read_to_string(&args[2]).unwrap_or_default())
    } else {
        HashMap::new()
    };

    // deterministic 80/20 split (every 5th match → test).
    let train: Vec<&Match> = matches.iter().enumerate().filter(|(i, _)| i % 5 != 0).map(|(_, m)| m).collect();
    let test: Vec<&Match> = matches.iter().enumerate().filter(|(i, _)| i % 5 == 0).map(|(_, m)| m).collect();

    // ---- build TRAIN tables ----
    let mut champ: HashMap<String, WL> = HashMap::new();
    let mut syn: HashMap<(String, String), WL> = HashMap::new();
    let mut cnt: HashMap<(String, String), WL> = HashMap::new();
    let pair = |a: &str, b: &str| {
        if a < b {
            (a.to_string(), b.to_string())
        } else {
            (b.to_string(), a.to_string())
        }
    };
    for m in &train {
        for c in &m.blue {
            champ.entry(c.clone()).or_default().rec(m.blue_win);
        }
        for c in &m.red {
            champ.entry(c.clone()).or_default().rec(!m.blue_win);
        }
        // synergy: unordered same-team pairs
        for i in 0..m.blue.len() {
            for j in (i + 1)..m.blue.len() {
                syn.entry(pair(&m.blue[i], &m.blue[j])).or_default().rec(m.blue_win);
            }
        }
        for i in 0..m.red.len() {
            for j in (i + 1)..m.red.len() {
                syn.entry(pair(&m.red[i], &m.red[j])).or_default().rec(!m.blue_win);
            }
        }
        // counter: ordered (mine, theirs)
        for b in &m.blue {
            for r in &m.red {
                cnt.entry((b.clone(), r.clone())).or_default().rec(m.blue_win);
                cnt.entry((r.clone(), b.clone())).or_default().rec(!m.blue_win);
            }
        }
    }

    // ---- comp completeness from tags ----
    let has = |team: &[String], tag: &str| team.iter().any(|c| taxon.get(c).map_or(false, |t| t.tags.iter().any(|x| x == tag)));
    let has_cat = |team: &[String], cat: &str| team.iter().any(|c| taxon.get(c).map_or(false, |t| t.category == cat));
    let has_dmg = |team: &[String], d: &str| team.iter().any(|c| taxon.get(c).map_or(false, |t| t.damage.contains(d)));
    let comp_score = |team: &[String]| -> f64 {
        let mut s = 0.0;
        if has(team, "Tank") {
            s += 1.0;
        } else if has(team, "Frontline") {
            s += 0.5;
        }
        if has(team, "CC") {
            s += 0.7;
        }
        if has(team, "Heal") || has(team, "Shield") {
            s += 0.6;
        }
        if has(team, "Backline") || has_cat(team, "Range") {
            s += 0.6;
        }
        if has_dmg(team, "AP") {
            s += 0.5;
        }
        if has_dmg(team, "AD") {
            s += 0.5;
        }
        s
    };

    // ---- TEST scorers ----
    let k_c = 8.0;
    let k_p = 6.0;
    let base = |team: &[String]| -> f64 { team.iter().map(|c| champ.get(c).map_or(0.0, |w| w.delta(k_c))).sum() };
    let syn_s = |team: &[String]| -> f64 {
        let mut s = 0.0;
        for i in 0..team.len() {
            for j in (i + 1)..team.len() {
                s += syn.get(&pair(&team[i], &team[j])).map_or(0.0, |w| w.delta(k_p));
            }
        }
        s
    };
    let cnt_s = |me: &[String], them: &[String]| -> f64 {
        let mut s = 0.0;
        for a in me {
            for b in them {
                s += cnt.get(&(a.clone(), b.clone())).map_or(0.0, |w| w.delta(k_p));
            }
        }
        s
    };

    // ---- evaluate ----
    // returns accuracy of "blue_term > red_term predicts blue_win"
    let eval = |f: &dyn Fn(&Match) -> (f64, f64)| -> f64 {
        let mut correct = 0.0;
        for m in &test {
            let (bs, rs) = f(m);
            if (bs - rs).abs() < 1e-12 {
                correct += 0.5;
            } else if (bs > rs) == m.blue_win {
                correct += 1.0;
            }
        }
        correct / test.len().max(1) as f64 * 100.0
    };

    let baseline = test.iter().filter(|m| m.blue_win).count() as f64 / test.len().max(1) as f64 * 100.0;

    println!("=== term validation (holdout 80/20) ===");
    println!("matches: {} total → {} train · {} test", matches.len(), train.len(), test.len());
    println!("baseline (always-blue): {:.1}%   (a term is only real if clearly > 50%)\n", baseline);

    let acc_base = eval(&|m| (base(&m.blue), base(&m.red)));
    let acc_syn = eval(&|m| (syn_s(&m.blue), syn_s(&m.red)));
    let acc_cnt = eval(&|m| (cnt_s(&m.blue, &m.red), cnt_s(&m.red, &m.blue)));
    let acc_comp = eval(&|m| (comp_score(&m.blue), comp_score(&m.red)));

    println!("STANDALONE (each term alone predicts the winner):");
    println!("  base  (champ winrate) : {:.1}%", acc_base);
    println!("  syn   (synergy)       : {:.1}%", acc_syn);
    println!("  cnt   (counter)       : {:.1}%", acc_cnt);
    println!("  comp  (comp balance)  : {:.1}%   (taxon champs: {})", acc_comp, taxon.len());

    // incremental: does adding a term to base help? (same unit: all winrate-deltas)
    let acc_bs = eval(&|m| (base(&m.blue) + syn_s(&m.blue), base(&m.red) + syn_s(&m.red)));
    let acc_bc = eval(&|m| {
        (base(&m.blue) + cnt_s(&m.blue, &m.red), base(&m.red) + cnt_s(&m.red, &m.blue))
    });
    let acc_all = eval(&|m| {
        (
            base(&m.blue) + syn_s(&m.blue) + cnt_s(&m.blue, &m.red),
            base(&m.red) + syn_s(&m.red) + cnt_s(&m.red, &m.blue),
        )
    });
    println!("\nINCREMENTAL (does adding a term to base improve it?):");
    println!("  base            : {:.1}%", acc_base);
    println!("  base + syn      : {:.1}%", acc_bs);
    println!("  base + cnt      : {:.1}%", acc_bc);
    println!("  base + syn + cnt: {:.1}%", acc_all);
    // comp is a different unit (0..~3.6) → scale to winrate units to test incrementally.
    println!("\nCOMP incremental (does comp add value ON TOP of base?):");
    for scale in [0.02_f64, 0.04, 0.06, 0.10] {
        let a = eval(&|m| (base(&m.blue) + scale * comp_score(&m.blue), base(&m.red) + scale * comp_score(&m.red)));
        println!("  base + {:.2}*comp : {:.1}%", scale, a);
    }
    println!("\nNOTE: 744 test matches → accuracy noise is ~±1.8% (1 sd). Differences under ~2% are noise.");
    println!(">> A term EARNS weight only if it LIFTS base incrementally beyond noise.");

    // ── AD/AP damage-balance analysis (does mixing AD+AP correlate with winning?) ──
    // Uses ALL matches (damage type is a fixed champ property → no circularity).
    let dmg = |c: &str| -> (bool, bool) {
        taxon.get(c).map(|t| (t.damage.contains("AD"), t.damage.contains("AP"))).unwrap_or((false, false))
    };
    let team_type = |team: &[String]| -> &'static str {
        let ad = team.iter().any(|c| dmg(c).0);
        let ap = team.iter().any(|c| dmg(c).1);
        match (ad, ap) {
            (true, true) => "balanced",
            (true, false) => "AD only",
            (false, true) => "AP only",
            _ => "none",
        }
    };
    use std::collections::HashMap as Map;
    let mut cat: Map<&str, (f64, f64)> = Map::new(); // cat -> (wins, games)
    let (mut h2h_w, mut h2h_g) = (0.0f64, 0.0f64); // balanced vs single-type
    for m in &matches {
        let (bt, rt) = (team_type(&m.blue), team_type(&m.red));
        let eb = cat.entry(bt).or_insert((0.0, 0.0));
        eb.1 += 1.0;
        if m.blue_win {
            eb.0 += 1.0;
        }
        let er = cat.entry(rt).or_insert((0.0, 0.0));
        er.1 += 1.0;
        if !m.blue_win {
            er.0 += 1.0;
        }
        let single = |t: &str| t == "AD only" || t == "AP only";
        if bt == "balanced" && single(rt) {
            h2h_g += 1.0;
            if m.blue_win {
                h2h_w += 1.0;
            }
        } else if rt == "balanced" && single(bt) {
            h2h_g += 1.0;
            if !m.blue_win {
                h2h_w += 1.0;
            }
        }
    }
    println!("\n=== AD/AP 균형 분석 (전체 {} 경기) — 균형이 승리와 상관있나? ===", matches.len());
    for c in ["balanced", "AD only", "AP only", "none"] {
        if let Some((w, g)) = cat.get(c) {
            if *g > 0.0 {
                println!("  {:<9}: 승률 {:.1}%  (해당 팀 {}개)", c, w / g * 100.0, *g as u64);
            }
        }
    }
    if h2h_g > 0.0 {
        println!(
            "  >> 균형팀 vs 단일타입팀 직접대결: 균형팀 승률 {:.1}%  (n={})  ← 50%면 균형은 무의미",
            h2h_w / h2h_g * 100.0,
            h2h_g as u64
        );
    } else {
        println!("  >> 단일타입(AD only/AP only) 팀이 거의 없음 → 사실상 모든 팀이 이미 균형 (균형 강제 불필요)");
    }
}
