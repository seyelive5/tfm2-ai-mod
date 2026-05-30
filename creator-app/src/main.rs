//! TFM2 Champion Creator — standalone desktop GUI (egui).
//! Reuses `champion-gen` (bundle catalog parse + generate + validate). Produces a mod folder
//! the user then uploads to Steam Workshop via TFM2ModUploader.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use champion_gen::{generate, load_catalog, power_score_pairs, role_base_growth, role_base_stats, role_base_tags, Catalog, ModSpec};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

const DEFAULT_BUNDLE: &str =
    r"C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2\bundle.game_data";
const DEFAULT_OUT: &str =
    r"C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2\mods\tfm2_champion_creator";

const ROLES: &[&str] = &["mage", "ad_carry", "assassin", "tank", "support"];
const ROLES_KO: &[&str] = &["메이지", "원거리 딜러", "암살자", "탱커", "서포터"];
const DELIVERY: &[&str] = &["linear_projectile", "homing_projectile", "area_self", "area_target", "dash", "support"];
const DELIVERY_KO: &[&str] = &["직선 투사체", "유도 투사체", "주변 광역", "지정 광역", "돌진", "지원(힐·실드·버프)"];
const DAMAGE: &[&str] = &["none", "ad", "ap", "fixed"];
const DAMAGE_KO: &[&str] = &["없음", "물리(공격력)", "마법(주문력)", "고정"];
const STATUS: &[&str] = &["stun", "bind", "fear", "knockback", "slow", "poison_dot", "fire_dot"];
const STATUS_KO: &[&str] = &["기절", "속박", "공포", "넉백", "슬로우", "독 도트", "화상 도트"];
const SLOTS: &[&str] = &["skill", "skill2", "ult"];
const SLOTS_KO: &[&str] = &["스킬", "스킬2", "궁극기"];
const ALL_TAGS: &[&str] = &["AD", "AP", "Heal", "Shield", "Dot", "CC", "Range", "Melee", "Tank", "Magic"];
const MODES: &[&str] = &["랜덤", "선택", "업로드"];

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Small thumbnail of a local PNG (egui_extras file:// loader).
fn png_thumb(ui: &mut egui::Ui, path: &str, sz: f32) {
    if !path.is_empty() {
        ui.add(egui::Image::new(format!("file://{path}")).max_width(sz).max_height(sz));
    }
}

/// A titled "card" section (bordered, padded) for a clean grouped layout.
fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::same(10.0))
        .outer_margin(egui::Margin { bottom: 10.0, ..Default::default() })
        .rounding(6.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).strong().size(15.0));
            ui.add_space(6.0);
            add(ui);
        });
}

/// One-time theme/spacing setup for a tidier, professional look.
fn setup_style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.slider_width = 200.0;
    ctx.set_style(style);
}

// ============================================================================
// Drafts
// ============================================================================

#[derive(Clone)]
struct StatusDraft { kind: usize, v: i64, v2: i64 }
impl StatusDraft {
    fn new(kind: usize) -> Self {
        let (v, v2) = match STATUS[kind] {
            "knockback" => (10, 2200), "slow" => (120, 30),
            "poison_dot" | "fire_dot" => (180, 25), _ => (45, 0),
        };
        Self { kind, v, v2 }
    }
}

#[derive(Clone)]
struct SkillDraft {
    delivery: usize,         // index into DELIVERY
    damage: usize,           // index into DAMAGE
    damage_ratio: i64,
    damage_flat: i64,
    statuses: Vec<StatusDraft>,
    heal: i64,
    shield: i64,
    b_attack: i64, b_mp: i64, b_aspd: i64, b_mspd: i64, b_vamp: i64, b_cdr: i64, b_tick: i64,
    ally_target: bool,
    range: i64, radius: i64, speed: i64, cooltime: i64, penetrate: bool,
    projectile: String, // projectile sprite/effect name ("" = default arrow)
    impact: String,     // impact effect anim name ("" = none)
}
impl SkillDraft {
    fn offensive(delivery: usize, damage: usize, ratio: i64, statuses: Vec<StatusDraft>, cooltime: i64) -> Self {
        Self {
            delivery, damage, damage_ratio: ratio, damage_flat: 0, statuses,
            heal: 0, shield: 0, b_attack: 0, b_mp: 0, b_aspd: 0, b_mspd: 0, b_vamp: 0, b_cdr: 0, b_tick: 180,
            ally_target: false, range: 60000, radius: 13000, speed: 4200, cooltime, penetrate: false,
            projectile: String::new(), impact: String::new(),
        }
    }
    fn support(shield: i64, cooltime: i64) -> Self {
        let mut s = Self::offensive(5, 0, 0, vec![], cooltime); // 5 = support
        s.shield = shield; s
    }
    fn to_value(&self, slot: &str) -> Value {
        let statuses: Vec<Value> = self.statuses.iter()
            .map(|st| json!({"kind": STATUS[st.kind], "v": st.v, "v2": st.v2})).collect();
        json!({
            "slot": slot, "delivery": DELIVERY[self.delivery], "damage": DAMAGE[self.damage],
            "damage_ratio": self.damage_ratio, "damage_flat": self.damage_flat, "statuses": statuses,
            "heal": self.heal, "shield": self.shield,
            "buff": {"attack": self.b_attack, "magic_power": self.b_mp, "attack_speed_mult": self.b_aspd,
                "move_speed_mult": self.b_mspd, "vamp": self.b_vamp, "skill_cooldown_mult": self.b_cdr, "tick": self.b_tick},
            "ally_target": self.ally_target, "range": self.range, "radius": self.radius,
            "speed": self.speed, "cooltime": self.cooltime, "penetrate": self.penetrate,
            "projectile": self.projectile, "impact": self.impact,
        })
    }
}

/// One-line Korean summary of a skill (live feedback under the editor).
fn skill_summary(s: &SkillDraft) -> String {
    let d = DELIVERY_KO[s.delivery];
    let mut parts: Vec<String> = Vec::new();
    if DELIVERY[s.delivery] == "support" {
        if s.heal > 0 { parts.push(format!("치유 {}", s.heal)); }
        if s.shield > 0 { parts.push(format!("보호막 {}", s.shield)); }
        if s.b_attack != 0 || s.b_mp != 0 || s.b_aspd != 0 || s.b_mspd != 0 || s.b_vamp != 0 || s.b_cdr != 0 {
            parts.push("강화".to_string());
        }
    } else {
        match DAMAGE[s.damage] {
            "ad" => parts.push(format!("공격력 {}% 피해", s.damage_ratio)),
            "ap" => parts.push(format!("주문력 {}% 피해", s.damage_ratio)),
            "fixed" => parts.push(format!("고정 {} 피해", s.damage_flat)),
            _ => {}
        }
        for st in &s.statuses { parts.push(STATUS_KO[st.kind].to_string()); }
    }
    if parts.is_empty() { d.to_string() } else { format!("{d} — {}", parts.join(", ")) }
}

/// Korean label + max for a status's secondary fields, by status kind.
fn status_fields(kind: usize) -> (&'static str, i64, &'static str, i64) {
    // (v_label, v_max, v2_label, v2_max) ; v2_label empty = hide v2
    match STATUS[kind] {
        "stun" | "bind" | "fear" => ("지속(틱)", 300, "", 0),
        "knockback" => ("지속(틱)", 40, "속도", 6000),
        "slow" => ("지속(틱)", 600, "감소%", 90),
        "poison_dot" | "fire_dot" => ("지속(틱)", 600, "틱당피해", 500),
        _ => ("값", 600, "", 0),
    }
}

#[derive(Clone)]
struct ChampionDraft {
    id: String,
    name_ko: String,
    name_en: String,
    role: usize,
    sprite_mode: u8, // 0 random, 1 select, 2 upload
    sprite: String,
    sprite_path: String,
    icon_mode: u8,
    icon: String,
    icon_paths: [String; 3],
    skills: Vec<SkillDraft>,
    stats: Vec<(String, i64)>,
    growth: Vec<(String, i64)>,
    tags: Vec<String>,
}

impl ChampionDraft {
    fn new(n: usize) -> Self {
        Self {
            id: format!("tfm2_cc_champ{n}"),
            name_ko: format!("새 챔피언 {n}"),
            name_en: format!("New Champion {n}"),
            role: 0,
            sprite_mode: 0,
            sprite: String::new(),
            sprite_path: String::new(),
            icon_mode: 0,
            icon: String::new(),
            icon_paths: [String::new(), String::new(), String::new()],
            skills: vec![
                SkillDraft::offensive(0, 2, 80, vec![], 480),                          // 직선 투사체 + 마법 80%
                SkillDraft::support(120, 540),                                         // 지원 + 실드 120
                SkillDraft::offensive(2, 2, 120, vec![StatusDraft::new(0)], 1200),     // 주변 광역 + 마법 120% + 기절
            ],
            stats: role_base_stats(ROLES[0]),
            growth: role_base_growth(ROLES[0]),
            tags: role_base_tags(ROLES[0]),
        }
    }
    fn to_value(&self) -> Value {
        let pick = |mode: u8, sel: &str, up: &str| match mode {
            1 => json!({ "mode": "select", "value": sel }),
            2 => json!({ "mode": "upload", "path": up }),
            _ => json!({ "mode": "random" }),
        };
        let skills: Vec<Value> = self.skills.iter().enumerate()
            .map(|(i, s)| s.to_value(SLOTS[i])).collect();
        let mut overrides = serde_json::Map::new();
        for (k, v) in &self.stats { overrides.insert(k.clone(), json!(v)); }
        let mut growth_ov = serde_json::Map::new();
        for (k, v) in &self.growth { growth_ov.insert(k.clone(), json!(v)); }
        json!({
            "id": self.id,
            "name": { "ko": self.name_ko, "en": self.name_en },
            "role": ROLES[self.role],
            "tags": self.tags,
            "stat_overrides": overrides,
            "growth_overrides": growth_ov,
            "sprite": pick(self.sprite_mode, &self.sprite, &self.sprite_path),
            "skill_icon": if self.icon_mode == 2 { json!({"mode":"random"}) } else { pick(self.icon_mode, &self.icon, "") },
            "skill_icon_uploads": if self.icon_mode == 2 { json!(self.icon_paths.to_vec()) } else { json!([]) },
            "skills": skills,
        })
    }
}

fn stat_label(key: &str) -> &str {
    match key {
        "attack" => "공격력", "magic_power" => "주문력", "hp" => "체력", "defence" => "방어력",
        "magic_resistance" => "마법저항", "move_speed" => "이동속도", "hp_regen" => "체력재생",
        "crit_chance" => "치명타%", other => other,
    }
}
fn stat_max(key: &str) -> i64 {
    match key { "hp" => 2000, "move_speed" => 1600, "crit_chance" => 100, _ => 200 }
}
fn growth_max(key: &str) -> i64 {
    match key { "hp" => 300, "move_speed" => 50, "crit_chance" => 10, _ => 40 }
}
fn ref_str(ranges: &BTreeMap<String, (i64, i64, i64)>, k: &str) -> String {
    ranges.get(k).map(|(mn, _, mx)| format!("({mn}~{mx})")).unwrap_or_default()
}

// ============================================================================
// App
// ============================================================================

struct App {
    bundle_path: String,
    out_dir: String,
    mod_id: String,
    author: String,
    catalog: Option<Catalog>,
    sprite_list: Vec<String>,
    icon_list: Vec<String>,
    status: String,
    champions: Vec<ChampionDraft>,
    selected: usize,
    next_n: usize,
    stat_ranges: BTreeMap<String, (i64, i64, i64)>,
    growth_ranges: BTreeMap<String, (i64, i64, i64)>,
    show_assets: bool,
    icon_sheet_png: Vec<u8>,
    icon_rects: BTreeMap<String, [f32; 4]>,
    icon_texture: Option<egui::TextureHandle>,
    /// raw unified visuals from the catalog (id -> VisualAsset)
    visuals: BTreeMap<String, champion_gen::VisualAsset>,
    /// per-visual decoded UI state (id -> VisualUi), built lazily on first frame after load
    vis_ui: BTreeMap<String, VisualUi>,
    vis_built: bool,
    /// which gallery picker is open: Some((skill_index, "projectile"|"impact")) or None
    gallery: Option<(usize, &'static str)>,
    gallery_filter: String,
}

/// Decoded preview for one visual: texture + normalized animation frames + kind.
struct VisualUi {
    tex: Option<egui::TextureHandle>,
    frames: Vec<egui::Rect>, // normalized UV rects
    kind: &'static str,      // "projectile" | "impact"
    label: String,           // human label e.g. "archer · ult_projectile"
}

/// Decode PNG bytes into an egui texture.
fn decode_texture(ctx: &egui::Context, name: &str, png: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

/// Show one icon by cropping the atlas via its normalized UV rect.
fn icon_crop(ui: &mut egui::Ui, tex: &egui::TextureHandle, rects: &BTreeMap<String, [f32; 4]>, tag: &str, sz: f32) {
    if let Some(r) = rects.get(tag) {
        let uv = egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]));
        ui.add(egui::Image::new(egui::load::SizedTexture::new(tex.id(), egui::vec2(sz, sz))).uv(uv));
    }
}

impl Default for App {
    fn default() -> Self {
        let mut app = Self {
            bundle_path: DEFAULT_BUNDLE.into(),
            out_dir: DEFAULT_OUT.into(),
            mod_id: "tfm2_champion_creator".into(),
            author: "you".into(),
            catalog: None,
            sprite_list: Vec::new(),
            icon_list: Vec::new(),
            status: "번들 로딩 중…".into(),
            champions: vec![ChampionDraft::new(1)],
            selected: 0,
            next_n: 2,
            stat_ranges: BTreeMap::new(),
            growth_ranges: BTreeMap::new(),
            show_assets: false,
            icon_sheet_png: Vec::new(),
            icon_rects: BTreeMap::new(),
            icon_texture: None,
            visuals: BTreeMap::new(),
            vis_ui: BTreeMap::new(),
            vis_built: false,
            gallery: None,
            gallery_filter: String::new(),
        };
        app.load_catalog();
        app
    }
}

impl App {
    fn seed(&self, role_idx: usize) -> (Vec<(String, i64)>, Vec<(String, i64)>) {
        let role = ROLES[role_idx];
        match &self.catalog {
            Some(c) => (c.role_stat_seed(role), c.role_growth_seed(role)),
            None => (role_base_stats(role), role_base_growth(role)),
        }
    }

    fn load_catalog(&mut self) {
        match load_catalog(&PathBuf::from(&self.bundle_path)) {
            Ok(cat) => {
                self.sprite_list = cat.champion_sprites.clone();
                self.icon_list = cat.skill_icon_champions();
                self.stat_ranges = cat.stat_ranges.clone();
                self.growth_ranges = cat.growth_ranges.clone();
                self.icon_sheet_png = cat.icon_sheet_png.clone();
                self.icon_rects = cat.icon_rects.clone();
                self.icon_texture = None; // rebuilt lazily in update()
                self.visuals = cat.visuals.clone();
                self.vis_ui.clear();
                self.vis_built = false;
                self.status = format!("준비 완료 — 스프라이트 {}종, 아이콘 {}종 (실측 데이터 반영)",
                    self.sprite_list.len(), self.icon_list.len());
                let first_sprite = self.sprite_list.first().cloned().unwrap_or_default();
                let first_icon = self.icon_list.first().cloned().unwrap_or_default();
                self.catalog = Some(cat);
                for i in 0..self.champions.len() {
                    let (s, g) = self.seed(self.champions[i].role);
                    let c = &mut self.champions[i];
                    c.stats = s; c.growth = g;
                    if c.sprite.is_empty() { c.sprite = first_sprite.clone(); }
                    if c.icon.is_empty() { c.icon = first_icon.clone(); }
                }
            }
            Err(e) => self.status = format!("번들 로드 실패 (경로 확인): {e:#}"),
        }
    }

    fn generate(&mut self) {
        let Some(cat) = &self.catalog else { self.status = "먼저 번들을 로드하세요.".into(); return; };
        let spec_val = json!({
            "mod_id": self.mod_id, "author": self.author, "version": "0.1.0",
            "champions": self.champions.iter().map(|c| c.to_value()).collect::<Vec<_>>(),
        });
        let spec: ModSpec = match serde_json::from_value(spec_val) {
            Ok(s) => s, Err(e) => { self.status = format!("스펙 오류: {e}"); return; }
        };
        match generate(&spec, cat, &PathBuf::from(&self.out_dir)) {
            Ok(files) => self.status = format!("[성공] {}개 챔피언 생성 완료 → {} ({}개 파일). 게임 재시작 후 Mods에서 켜세요.",
                self.champions.len(), self.out_dir, files.len()),
            Err(e) => self.status = format!("[실패] {e:#}"),
        }
    }
}

fn assets_window(ctx: &egui::Context, open: &mut bool) {
    let green = egui::Color32::from_rgb(120, 200, 120);
    let amber = egui::Color32::from_rgb(220, 180, 90);
    egui::Window::new("그림 쉽게 구하기").open(open).resizable(true).default_width(460.0).show(ctx, |ui| {
        ui.colored_label(green, "한 줄 요약: 무료이고 'CC0'라고 적힌 그림만 받으면 안전해요.");
        ui.add_space(4.0);
        ui.label("왜냐면: 내가 만든 챔피언을 남들과 나누려면(창작마당 업로드) 그림도 '나눠도 되는' 것이어야 해요.");
        ui.label("'CC0' = 공짜 + 출처 안 적어도 됨 + 마음껏 공유 OK.");
        ui.separator();
        ui.heading("이렇게 따라하세요");
        ui.label("1. 아래 버튼을 누르면 그림 사이트가 열려요.");
        ui.label("2. '작은 픽셀 캐릭터' 그림을 고르세요 (이 게임이랑 잘 어울려요).");
        ui.label("3. 그림 설명에 'CC0' 가 적혀 있는지만 확인하세요.");
        ui.label("4. PNG 파일로 내려받아요 (배경이 투명한 게 제일 좋아요).");
        ui.label("5. 이 앱에서 '스프라이트'를 [업로드]로 바꾸고 받은 PNG를 고르면 끝!");
        ui.add_space(6.0);
        ui.hyperlink_to("▶ CC0 캐릭터 그림 (itch.io)", "https://itch.io/game-assets/assets-cc0/tag-pixel-art");
        ui.hyperlink_to("▶ CC0 그림 더 보기 (OpenGameArt)", "https://opengameart.org/content/cc0-resources");
        ui.separator();
        ui.heading("스킬 아이콘");
        ui.hyperlink_to("▶ 스킬 아이콘 (game-icons.net)", "https://game-icons.net/");
        ui.label("거의 다 쓸 수 있어요. 대신 모드 설명칸에 한 줄만 →  Icons: game-icons.net");
        ui.add_space(6.0);
        ui.colored_label(amber, "잘 모르겠으면 'CC0'라고 적힌 것만 쓰면 신경 쓸 게 없어요.");
        ui.label("참고: 창작마당엔 팬 창작도 많아요. 비영리면 보통 문제없지만, 원작자가 내려달라 하면 내려야 할 수도 있어요.");
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut show = self.show_assets;
        assets_window(ctx, &mut show);
        self.show_assets = show;

        // lazily build the skill-icon atlas texture (once per catalog load)
        if self.icon_texture.is_none() && !self.icon_sheet_png.is_empty() {
            self.icon_texture = decode_texture(ctx, "skill_icons", &self.icon_sheet_png);
        }
        // lazily decode all skill-visual previews once per catalog. Many visuals share the same
        // champion #sheet (only the tag/frames differ), so decode each sheet PNG only once.
        if !self.vis_built && !self.visuals.is_empty() {
            let mut sheet_cache: std::collections::HashMap<String, Option<egui::TextureHandle>> = std::collections::HashMap::new();
            for (id, v) in &self.visuals {
                let tex = sheet_cache.entry(v.anim_asset.clone())
                    .or_insert_with(|| decode_texture(ctx, &v.anim_asset, &v.sheet_png)).clone();
                let frames = match &tex {
                    Some(t) => {
                        let sz = t.size_vec2();
                        if sz.x > 0.0 && sz.y > 0.0 {
                            v.frames.iter().map(|r| egui::Rect::from_min_size(
                                egui::pos2(r[0] / sz.x, r[1] / sz.y), egui::vec2(r[2] / sz.x, r[3] / sz.y))).collect()
                        } else { Vec::new() }
                    }
                    None => Vec::new(),
                };
                // label: "champ · tag" for ch:..., "tag" for fx:...
                let label = id.strip_prefix("ch:").map(|r| r.replacen(':', " · ", 1))
                    .unwrap_or_else(|| v.tag.clone());
                self.vis_ui.insert(id.clone(), VisualUi { tex, frames, kind: v.kind, label });
            }
            self.vis_built = true;
        }

        // ---------- Top bar ----------
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("TFM2 챔피언 창작 도구");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("그림 구하기 (도움말)").clicked() { self.show_assets = true; }
                    if ui.button("번들 다시 로드").clicked() { self.load_catalog(); }
                });
            });
            ui.add_space(6.0);
            egui::Grid::new("topcfg").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("출력 폴더");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.out_dir).desired_width(560.0));
                    if ui.button("찾기…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() { self.out_dir = d.display().to_string(); }
                    }
                });
                ui.end_row();
                ui.label("모드 ID");
                ui.add(egui::TextEdit::singleline(&mut self.mod_id).desired_width(280.0));
                ui.end_row();
            });
            ui.add_space(6.0);
        });

        // ---------- Bottom bar ----------
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let btn = egui::Button::new(egui::RichText::new("챔피언 생성").size(16.0).strong())
                    .min_size(egui::vec2(150.0, 34.0));
                if ui.add(btn).clicked() { self.generate(); }
                ui.separator();
                ui.label(&self.status);
            });
            ui.add_space(6.0);
        });

        // ---------- Left: champion list ----------
        egui::SidePanel::left("list").resizable(true).default_width(200.0).show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("챔피언 목록");
            ui.add_space(6.0);
            if ui.add(egui::Button::new("+ 새 챔피언").min_size(egui::vec2(ui.available_width(), 30.0))).clicked() {
                let mut d = ChampionDraft::new(self.next_n);
                let (s, g) = self.seed(d.role);
                d.stats = s; d.growth = g;
                d.sprite = self.sprite_list.first().cloned().unwrap_or_default();
                d.icon = self.icon_list.first().cloned().unwrap_or_default();
                self.champions.push(d);
                self.selected = self.champions.len() - 1;
                self.next_n += 1;
            }
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
            let mut to_remove = None;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for i in 0..self.champions.len() {
                    ui.horizontal(|ui| {
                        let txt = format!("{}  · {}", self.champions[i].name_ko, ROLES_KO[self.champions[i].role]);
                        if ui.selectable_label(self.selected == i, txt).clicked() { self.selected = i; }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("삭제").clicked() { to_remove = Some(i); }
                        });
                    });
                }
            });
            if let Some(i) = to_remove {
                if self.champions.len() > 1 { self.champions.remove(i); self.selected = 0; }
            }
        });

        // ---------- Center: editor ----------
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.champions.is_empty() { return; }
            let i = self.selected.min(self.champions.len() - 1);
            let sprites = self.sprite_list.clone();
            let icons = self.icon_list.clone();
            let stat_ranges = self.stat_ranges.clone();
            let growth_ranges = self.growth_ranges.clone();
            let role_seeds: Vec<(Vec<(String, i64)>, Vec<(String, i64)>)> =
                (0..ROLES.len()).map(|r| self.seed(r)).collect();
            let base_power = self.catalog.as_ref().map(|c| c.base_power_avg).filter(|v| *v > 0.0).unwrap_or(1500.0);
            let icon_tex = self.icon_texture.clone();
            let icon_rects_uv = self.icon_rects.clone();
            // snapshot minimal preview data (id -> texture + animation frames) + labels
            let vis_prev: BTreeMap<String, (Option<egui::TextureHandle>, Vec<egui::Rect>)> =
                self.vis_ui.iter().map(|(k, u)| (k.clone(), (u.tex.clone(), u.frames.clone()))).collect();
            let vis_labels: BTreeMap<String, String> =
                self.vis_ui.iter().map(|(k, u)| (k.clone(), u.label.clone())).collect();
            let anim_time = ctx.input(|inp| inp.time);
            if !vis_prev.is_empty() { ctx.request_repaint(); } // keep animations playing
            let mut gallery_open = self.gallery;
            let c = &mut self.champions[i];
            // preview a chosen skill visual (by id) at `sz`px, animated.
            let show_vis = |ui: &mut egui::Ui, id: &str, sz: f32| {
                if id.is_empty() { return; }
                let Some((Some(tex), frames)) = vis_prev.get(id) else { return };
                if !frames.is_empty() {
                    let idx = ((anim_time * 12.0) as usize) % frames.len(); // ~12 fps
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(tex.id(), egui::vec2(sz, sz))).uv(frames[idx]));
                } else {
                    ui.add(egui::Image::new(tex).max_width(sz).max_height(sz));
                }
            };
            let vis_label = |id: &str| -> String { vis_labels.get(id).cloned().unwrap_or_default() };

            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(8.0);

                // --- 기본 정보 ---
                section(ui, "기본 정보", |ui| {
                    egui::Grid::new("basic").num_columns(2).spacing([12.0, 8.0]).min_col_width(90.0).show(ui, |ui| {
                        ui.label("ID"); ui.add(egui::TextEdit::singleline(&mut c.id).desired_width(300.0)); ui.end_row();
                        ui.label("이름 (한글)"); ui.add(egui::TextEdit::singleline(&mut c.name_ko).desired_width(300.0)); ui.end_row();
                        ui.label("이름 (영문)"); ui.add(egui::TextEdit::singleline(&mut c.name_en).desired_width(300.0)); ui.end_row();
                        ui.label("역할");
                        let old = c.role;
                        egui::ComboBox::from_id_source("role").selected_text(ROLES_KO[c.role]).show_ui(ui, |ui| {
                            for ri in 0..ROLES.len() { ui.selectable_value(&mut c.role, ri, ROLES_KO[ri]); }
                        });
                        if c.role != old {
                            let (s, g) = role_seeds[c.role].clone(); c.stats = s; c.growth = g;
                            c.tags = role_base_tags(ROLES[c.role]);
                        }
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    ui.label("태그");
                    ui.horizontal_wrapped(|ui| {
                        for t in ALL_TAGS {
                            let mut on = c.tags.iter().any(|x| x == t);
                            if ui.checkbox(&mut on, *t).changed() {
                                if on { if !c.tags.iter().any(|x| x == t) { c.tags.push(t.to_string()); } }
                                else { c.tags.retain(|x| x != t); }
                            }
                        }
                    });
                });

                // --- 외형 ---
                section(ui, "외형 (스프라이트 · 스킬 아이콘)", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("스프라이트");
                        egui::ComboBox::from_id_source("sprmode").width(76.0).selected_text(MODES[c.sprite_mode as usize]).show_ui(ui, |ui| {
                            for (mi, mn) in MODES.iter().enumerate() { ui.selectable_value(&mut c.sprite_mode, mi as u8, *mn); }
                        });
                        match c.sprite_mode {
                            1 => { egui::ComboBox::from_id_source("sprite").width(180.0).selected_text(&c.sprite).show_ui(ui, |ui| {
                                    for s in &sprites { ui.selectable_value(&mut c.sprite, s.clone(), s); } }); }
                            2 => {
                                if ui.button("PNG 선택…").clicked() {
                                    if let Some(p) = rfd::FileDialog::new().add_filter("PNG", &["png"]).pick_file() { c.sprite_path = p.display().to_string(); }
                                }
                                ui.weak(file_name_of(&c.sprite_path));
                            }
                            _ => { ui.weak("(매 생성 시 무작위)"); }
                        }
                    });
                    if c.sprite_mode == 2 && !c.sprite_path.is_empty() {
                        ui.add_space(4.0);
                        png_thumb(ui, &c.sprite_path, 128.0);
                        ui.colored_label(egui::Color32::from_rgb(220, 180, 90),
                            "※ '캐릭터 1명이 그려진 한 장' PNG여야 합니다. 격자형 스프라이트시트는 통째로 뭉개져 보여요.");
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("스킬 아이콘");
                        egui::ComboBox::from_id_source("iconmode").width(76.0).selected_text(MODES[c.icon_mode as usize]).show_ui(ui, |ui| {
                            for (mi, mn) in MODES.iter().enumerate() { ui.selectable_value(&mut c.icon_mode, mi as u8, *mn); }
                        });
                        match c.icon_mode {
                            1 => { egui::ComboBox::from_id_source("icon").width(180.0).selected_text(&c.icon).show_ui(ui, |ui| {
                                    for s in &icons { ui.selectable_value(&mut c.icon, s.clone(), s); } }); }
                            2 => { ui.weak("아래에서 슬롯별로 지정"); }
                            _ => { ui.weak("(매 생성 시 무작위)"); }
                        }
                    });
                    // preview the chosen base icons (skill / skill2 / ult)
                    if c.icon_mode == 1 && !c.icon.is_empty() {
                        if let Some(tex) = &icon_tex {
                            ui.horizontal(|ui| {
                                for k in 0..3 { icon_crop(ui, tex, &icon_rects_uv, &format!("{}_{}", c.icon, k), 40.0); }
                            });
                        }
                    }
                    if c.icon_mode == 2 {
                        for si in 0..3 {
                            ui.horizontal(|ui| {
                                if ui.button(format!("{} PNG…", SLOTS_KO[si])).clicked() {
                                    if let Some(p) = rfd::FileDialog::new().add_filter("PNG", &["png"]).pick_file() { c.icon_paths[si] = p.display().to_string(); }
                                }
                                png_thumb(ui, &c.icon_paths[si], 28.0);
                                ui.weak(file_name_of(&c.icon_paths[si]));
                            });
                        }
                    }
                });

                // --- 능력치 ---
                section(ui, "능력치 (Lv.1)", |ui| {
                    ui.horizontal(|ui| {
                        ui.weak("괄호 = 실제 챔피언들의 범위");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("실측 기본값으로 리셋").clicked() { c.stats = role_seeds[c.role].0.clone(); }
                        });
                    });
                    ui.add_space(2.0);
                    for (k, v) in c.stats.iter_mut() {
                        ui.add(egui::Slider::new(v, 0..=stat_max(k)).text(format!("{} {}", stat_label(k), ref_str(&stat_ranges, k))));
                    }
                });

                // --- 성장치 ---
                section(ui, "성장치 (레벨당 증가)", |ui| {
                    ui.horizontal(|ui| {
                        ui.weak("괄호 = 실제 범위");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("실측 기본값으로 리셋").clicked() { c.growth = role_seeds[c.role].1.clone(); }
                        });
                    });
                    ui.add_space(2.0);
                    for (k, v) in c.growth.iter_mut() {
                        ui.add(egui::Slider::new(v, 0..=growth_max(k)).text(format!("{} {}", stat_label(k), ref_str(&growth_ranges, k))));
                    }
                });

                // --- 밸런스 ---
                section(ui, "밸런스 점검 (능력치 기준)", |ui| {
                    let score = power_score_pairs(&c.stats, &c.growth);
                    let ratio = (score / base_power).max(0.0);
                    let (color, verdict) = if ratio < 0.85 {
                        (egui::Color32::from_rgb(110, 170, 230), "약함 — 기준보다 낮음")
                    } else if ratio <= 1.20 {
                        (egui::Color32::from_rgb(120, 200, 120), "균형 — 기준과 비슷")
                    } else if ratio <= 1.50 {
                        (egui::Color32::from_rgb(220, 180, 90), "강함 — 기준보다 셈")
                    } else {
                        (egui::Color32::from_rgb(230, 110, 110), "과함 — 너무 셈 (사기 주의)")
                    };
                    ui.add(egui::ProgressBar::new((ratio / 2.0).min(1.0) as f32)
                        .fill(color).text(format!("기준 대비 {:.0}%", ratio * 100.0)));
                    ui.colored_label(color, verdict);
                    ui.weak("※ 능력치/성장치 기반 추정입니다(스킬 위력은 미포함). '균형' 근처를 권장합니다.");
                });

                // --- 스킬 ---
                section(ui, "스킬 (효과 빌더)", |ui| {
                    for si in 0..3 {
                        let s = &mut c.skills[si];
                        egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(8.0)).rounding(5.0).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(SLOTS_KO[si]).strong());
                                ui.label("전달방식");
                                egui::ComboBox::from_id_source(format!("dlv{si}")).width(150.0)
                                    .selected_text(DELIVERY_KO[s.delivery]).show_ui(ui, |ui| {
                                    for di in 0..DELIVERY.len() { ui.selectable_value(&mut s.delivery, di, DELIVERY_KO[di]); }
                                });
                            });
                            ui.add_space(4.0);
                            let is_support = DELIVERY[s.delivery] == "support";

                            // skill VISUALS (offensive only): pick via gallery, show animated preview
                            if !is_support {
                                let dl = DELIVERY[s.delivery];
                                if matches!(dl, "linear_projectile" | "homing_projectile" | "area_target") {
                                    ui.horizontal(|ui| {
                                        ui.label("투사체 그림");
                                        show_vis(ui, &s.projectile, 48.0);
                                        let lbl = if s.projectile.is_empty() { "기본(화살)".to_string() } else { vis_label(&s.projectile) };
                                        ui.label(lbl);
                                        if ui.button("그림 선택…").clicked() { gallery_open = Some((si, "projectile")); }
                                    });
                                }
                                ui.horizontal(|ui| {
                                    ui.label("적중 효과");
                                    show_vis(ui, &s.impact, 48.0);
                                    let lbl = if s.impact.is_empty() { "없음".to_string() } else { vis_label(&s.impact) };
                                    ui.label(lbl);
                                    if ui.button("효과 선택…").clicked() { gallery_open = Some((si, "impact")); }
                                });
                                ui.add_space(4.0);
                            }

                            if is_support {
                                ui.checkbox(&mut s.ally_target, "아군 대상 (해제 시 자신)");
                                egui::Grid::new(format!("sup{si}")).num_columns(4).spacing([10.0, 5.0]).show(ui, |ui| {
                                    ui.label("치유"); ui.add(egui::DragValue::new(&mut s.heal).clamp_range(0..=2000));
                                    ui.label("보호막"); ui.add(egui::DragValue::new(&mut s.shield).clamp_range(0..=2000)); ui.end_row();
                                    ui.label("공격력+"); ui.add(egui::DragValue::new(&mut s.b_attack).clamp_range(0..=200));
                                    ui.label("주문력+"); ui.add(egui::DragValue::new(&mut s.b_mp).clamp_range(0..=200)); ui.end_row();
                                    ui.label("공속%+"); ui.add(egui::DragValue::new(&mut s.b_aspd).clamp_range(0..=200));
                                    ui.label("이속%+"); ui.add(egui::DragValue::new(&mut s.b_mspd).clamp_range(0..=100)); ui.end_row();
                                    ui.label("흡혈+"); ui.add(egui::DragValue::new(&mut s.b_vamp).clamp_range(0..=100));
                                    ui.label("쿨감%+"); ui.add(egui::DragValue::new(&mut s.b_cdr).clamp_range(0..=100)); ui.end_row();
                                    ui.label("버프지속(틱)"); ui.add(egui::DragValue::new(&mut s.b_tick).clamp_range(0..=1200)); ui.end_row();
                                });
                            } else {
                                ui.horizontal(|ui| {
                                    ui.label("피해");
                                    egui::ComboBox::from_id_source(format!("dmg{si}")).width(120.0)
                                        .selected_text(DAMAGE_KO[s.damage]).show_ui(ui, |ui| {
                                        for di in 0..DAMAGE.len() { ui.selectable_value(&mut s.damage, di, DAMAGE_KO[di]); }
                                    });
                                    match DAMAGE[s.damage] {
                                        "none" => {}
                                        "fixed" => { ui.label("고정값"); ui.add(egui::DragValue::new(&mut s.damage_flat).clamp_range(0..=2000)); }
                                        _ => {
                                            ui.label("계수%"); ui.add(egui::DragValue::new(&mut s.damage_ratio).clamp_range(0..=500));
                                            ui.label("+고정"); ui.add(egui::DragValue::new(&mut s.damage_flat).clamp_range(0..=2000));
                                        }
                                    }
                                });
                                ui.add_space(2.0);
                                ui.horizontal(|ui| {
                                    ui.label("상태이상");
                                    if ui.small_button("+ 추가").clicked() { s.statuses.push(StatusDraft::new(0)); }
                                });
                                let mut remove = None;
                                for (sti, st) in s.statuses.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        egui::ComboBox::from_id_source(format!("st{si}_{sti}")).width(90.0)
                                            .selected_text(STATUS_KO[st.kind]).show_ui(ui, |ui| {
                                            for ki in 0..STATUS.len() { ui.selectable_value(&mut st.kind, ki, STATUS_KO[ki]); }
                                        });
                                        let (vl, vmax, v2l, v2max) = status_fields(st.kind);
                                        ui.label(vl); ui.add(egui::DragValue::new(&mut st.v).clamp_range(0..=vmax));
                                        if !v2l.is_empty() { ui.label(v2l); ui.add(egui::DragValue::new(&mut st.v2).clamp_range(0..=v2max)); }
                                        if ui.small_button("삭제").clicked() { remove = Some(sti); }
                                    });
                                }
                                if let Some(r) = remove { s.statuses.remove(r); }
                            }

                            ui.add_space(4.0);
                            let dl = DELIVERY[s.delivery];
                            egui::Grid::new(format!("num{si}")).num_columns(4).spacing([10.0, 5.0]).show(ui, |ui| {
                                ui.label("쿨타임(틱)"); ui.add(egui::DragValue::new(&mut s.cooltime).clamp_range(0..=3600));
                                if matches!(dl, "linear_projectile" | "homing_projectile" | "area_target" | "dash") {
                                    ui.label("사거리"); ui.add(egui::DragValue::new(&mut s.range).clamp_range(0..=120000));
                                }
                                ui.end_row();
                                if matches!(dl, "linear_projectile" | "homing_projectile" | "area_self" | "area_target") {
                                    ui.label("반경/폭"); ui.add(egui::DragValue::new(&mut s.radius).clamp_range(0..=90000));
                                }
                                if matches!(dl, "linear_projectile" | "homing_projectile" | "area_target" | "dash") {
                                    ui.label("속도"); ui.add(egui::DragValue::new(&mut s.speed).clamp_range(0..=9000));
                                }
                                ui.end_row();
                            });
                            if matches!(dl, "linear_projectile" | "dash") {
                                ui.checkbox(&mut s.penetrate, "관통");
                            }
                            ui.add_space(2.0);
                            ui.weak(skill_summary(s));
                        });
                        ui.add_space(8.0);
                    }
                });

                ui.add_space(8.0);
            });
            self.gallery = gallery_open; // commit any "그림 선택…" button press
        });

        // ---------- Visual gallery picker (modal-ish window) ----------
        self.draw_gallery(ctx);
    }
}

impl App {
    /// Image gallery to pick a projectile or impact visual. Big animated thumbnails, search, click to select.
    fn draw_gallery(&mut self, ctx: &egui::Context) {
        let Some((si, kind)) = self.gallery else { return };
        let title = if kind == "projectile" { "투사체 그림 선택" } else { "적중 효과 선택" };
        // candidate visuals of this kind, matching the search filter
        let filt = self.gallery_filter.to_lowercase();
        let items: Vec<(String, Vec<egui::Rect>, Option<egui::TextureHandle>, String)> = self.vis_ui.iter()
            .filter(|(_, u)| u.kind == kind)
            .filter(|(_, u)| filt.is_empty() || u.label.to_lowercase().contains(&filt))
            .map(|(id, u)| (id.clone(), u.frames.clone(), u.tex.clone(), u.label.clone()))
            .collect();
        let anim_time = ctx.input(|inp| inp.time);
        ctx.request_repaint();
        let mut open = true;
        let mut chosen: Option<String> = None;
        let mut filter = self.gallery_filter.clone();

        const CELL: f32 = 156.0;
        const IMG: f32 = 120.0;
        egui::Window::new(title).open(&mut open).resizable(true).default_width(820.0).default_height(620.0).collapsible(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("검색");
                ui.add(egui::TextEdit::singleline(&mut filter).hint_text("챔피언/태그 이름…").desired_width(260.0));
                ui.weak(format!("{}개", items.len()));
            });
            ui.weak("그림을 클릭해서 선택하세요. (움직이는 미리보기)");
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.allocate_ui(egui::vec2(CELL, CELL + 24.0), |ui| {
                        ui.vertical_centered(|ui| {
                            if ui.add_sized(egui::vec2(IMG, IMG), egui::Button::new("✕\n해제(기본)")).clicked() { chosen = Some(String::new()); }
                        });
                    });
                    for (id, frames, tex, label) in &items {
                        ui.allocate_ui(egui::vec2(CELL, CELL + 24.0), |ui| {
                            ui.vertical_centered(|ui| {
                                let resp = egui::Frame::group(ui.style()).inner_margin(6.0).show(ui, |ui| {
                                    match (tex, frames.is_empty()) {
                                        (Some(t), false) => {
                                            let idx = ((anim_time * 12.0) as usize) % frames.len();
                                            ui.add(egui::Image::new(egui::load::SizedTexture::new(t.id(), egui::vec2(IMG, IMG))).uv(frames[idx]));
                                        }
                                        (Some(t), true) => { ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(IMG, IMG))); }
                                        _ => { ui.add_sized(egui::vec2(IMG, IMG), egui::Label::new("(미리보기 없음)")); }
                                    }
                                }).response;
                                if resp.interact(egui::Sense::click()).clicked() { chosen = Some(id.clone()); }
                                ui.small(label.as_str());
                            });
                        });
                    }
                });
            });
        });

        self.gallery_filter = filter;
        if let Some(id) = chosen {
            let idx = self.selected.min(self.champions.len().saturating_sub(1));
            if let Some(c) = self.champions.get_mut(idx) {
                if let Some(sk) = c.skills.get_mut(si) {
                    if kind == "projectile" { sk.projectile = id; } else { sk.impact = id; }
                }
            }
            self.gallery = None;
            self.gallery_filter.clear();
        } else if !open {
            self.gallery = None;
            self.gallery_filter.clear();
        }
    }
}

/// Load a Korean-capable TTF (Latin too). Windows Malgun first, then game's NotoSansKR.
fn load_korean_font() -> Option<Vec<u8>> {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\malgun.ttf",
        r"C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2\mods\base_unpacked\font\NotoSansKR\NotoSansKR-Medium.ttf",
        r"C:\Program Files (x86)\Steam\steamapps\common\Teamfight Manager2\mods\base_unpacked\font\NotoSansKR\NotoSansKR-Light.ttf",
        r"C:\Windows\Fonts\gulim.ttc",
    ];
    CANDIDATES.iter().find_map(|p| std::fs::read(p).ok())
}

fn setup_fonts(ctx: &egui::Context) {
    let Some(bytes) = load_korean_font() else { return };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("kr".to_owned(), egui::FontData::from_owned(bytes));
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "kr".to_owned());
    fonts.families.entry(egui::FontFamily::Monospace).or_default().push("kr".to_owned());
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 880.0])
            .with_min_inner_size([760.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "TFM2 Champion Creator",
        opts,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Box::new(App::default()) as Box<dyn eframe::App>
        }),
    )
}
