//! CLI: champion-gen <spec.json> --out <mod_dir> [--bundle <bundle.game_data>]

use std::path::PathBuf;
use anyhow::{anyhow, Context, Result};
use champion_gen::{generate, load_catalog, validate, ModSpec};

const DEFAULT_BUNDLE: &str =
    r"C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2\bundle.game_data";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut spec_path: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut bundle = PathBuf::from(DEFAULT_BUNDLE);

    let mut check_files: Vec<PathBuf> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out_dir = Some(args.next().ok_or_else(|| anyhow!("--out needs a path"))?.into()),
            "--bundle" => bundle = args.next().ok_or_else(|| anyhow!("--bundle needs a path"))?.into(),
            "--check" => check_files.push(args.next().ok_or_else(|| anyhow!("--check needs a file"))?.into()),
            _ => spec_path = Some(a.into()),
        }
    }

    // --check: validate existing .data_champion file(s) offline; no game restart needed.
    if !check_files.is_empty() {
        let mut bad = 0;
        for f in &check_files {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(f)
                .with_context(|| format!("reading {}", f.display()))?)
                .with_context(|| format!("parsing {}", f.display()))?;
            let problems = validate::validate_champion(&v);
            if problems.is_empty() {
                println!("OK   {}", f.display());
            } else {
                bad += 1;
                println!("FAIL {}", f.display());
                for p in problems { println!("       - {p}"); }
            }
        }
        std::process::exit(if bad == 0 { 0 } else { 1 });
    }
    let spec_path = spec_path.ok_or_else(|| anyhow!("usage: champion-gen <spec.json> --out <mod_dir> [--bundle <path>]"))?;
    let out_dir = out_dir.ok_or_else(|| anyhow!("--out <mod_dir> is required"))?;

    let spec_txt = std::fs::read_to_string(&spec_path)
        .with_context(|| format!("reading spec {}", spec_path.display()))?;
    let spec: ModSpec = serde_json::from_str(&spec_txt).context("parsing spec json")?;

    eprintln!("Loading catalog from {} ...", bundle.display());
    let cat = load_catalog(&bundle)?;
    eprintln!(
        "catalog: {} sprites, {} skill-icon tags ({} champions)",
        cat.champion_sprites.len(),
        cat.skill_icon_tags.len(),
        cat.skill_icon_champions().len()
    );

    let written = generate(&spec, &cat, &out_dir)?;
    println!("Generated {} champion(s) into {}", spec.champions.len(), out_dir.display());
    for w in &written {
        println!("  {}", w.display());
    }
    Ok(())
}
