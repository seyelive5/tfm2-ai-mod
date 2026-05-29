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
const TEMPLATES: &[&str] = &["skillshot_ap", "skillshot_ad", "self_buff", "dash", "heal", "aoe_stun",
    "shield_self", "stun_shot", "knockback_shot", "dot_poison"];
const TEMPLATES_KO: &[&str] = &["투사체(마법)", "투사체(물리)", "자기강화", "돌진", "치유", "광역기절",
    "자기보호막", "기절투사체", "넉백투사체", "독도트"];
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
struct SkillDraft { template: usize, params: Vec<(String, i64)> }

impl SkillDraft {
    fn new(template: usize) -> Self {
        let params = template_params(TEMPLATES[template]).iter()
            .map(|(k, _, d, _)| (k.to_string(), *d)).collect();
        Self { template, params }
    }
}

/// Per-template tunable params: (json_key, korean_label, default, max). Mirrors champion-gen build_skill.
fn template_params(t: &str) -> &'static [(&'static str, &'static str, i64, i64)] {
    match t {
        "skillshot_ap" => &[("ap_ratio","주문력계수%",80,400),("damage","고정피해",50,1000),("range","사거리",65000,90000),("radius","반경",8000,40000),("cooltime","쿨타임(틱)",360,3600)],
        "skillshot_ad" => &[("ad_ratio","공격력계수%",110,400),("damage","고정피해",40,1000),("range","사거리",60000,90000),("radius","반경",8000,40000),("cooltime","쿨타임(틱)",340,3600)],
        "self_buff" => &[("magic_power","주문력",0,200),("attack","공격력",0,200),("attack_speed_mult","공속%",0,200),("move_speed_mult","이속%",0,100),("skill_cooldown_mult","쿨감%",0,100),("vamp","흡혈",0,100),("tick","지속(틱)",180,1200),("cooltime","쿨타임(틱)",600,3600)],
        "dash" => &[("ad_ratio","공격력계수%",60,400),("damage","고정피해",30,1000),("range","사거리",40000,90000),("speed","속도",3800,9000),("cooltime","쿨타임(틱)",420,3600)],
        "heal" => &[("amount","회복량",80,1000),("ap_ratio","주문력계수%",40,400),("range","사거리",55000,90000),("cooltime","쿨타임(틱)",540,3600)],
        "aoe_stun" => &[("ap_ratio","주문력계수%",100,400),("damage","고정피해",120,1000),("radius","반경",42000,90000),("stun","기절(틱)",45,300),("cooltime","쿨타임(틱)",1200,3600)],
        "shield_self" => &[("amount","보호막량",120,1000),("ap_ratio","주문력계수%",50,400),("tick","지속(틱)",300,1200),("cooltime","쿨타임(틱)",540,3600)],
        "stun_shot" => &[("ap_ratio","주문력계수%",60,400),("damage","고정피해",40,1000),("range","사거리",60000,90000),("radius","반경",8000,40000),("stun","기절(틱)",45,300),("cooltime","쿨타임(틱)",480,3600)],
        "knockback_shot" => &[("ad_ratio","공격력계수%",90,400),("damage","고정피해",30,1000),("range","사거리",55000,90000),("radius","반경",9000,40000),("knockback_speed","넉백속도",2200,6000),("knockback_tick","넉백(틱)",10,40),("cooltime","쿨타임(틱)",420,3600)],
        "dot_poison" => &[("ap_ratio","계수%",20,400),("dot_damage","틱당피해",20,500),("dot_duration","지속(틱)",180,600),("dot_period","주기(틱)",30,120),("range","사거리",62000,90000),("radius","반경",8000,40000),("cooltime","쿨타임(틱)",420,3600)],
        _ => &[],
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
            skills: vec![SkillDraft::new(0), SkillDraft::new(2), SkillDraft::new(5)],
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
        let skills: Vec<Value> = self.skills.iter().enumerate().map(|(i, s)| {
            let mut p = serde_json::Map::new();
            for (k, v) in &s.params { p.insert(k.clone(), json!(v)); }
            json!({ "slot": SLOTS[i], "template": TEMPLATES[s.template], "params": p })
        }).collect();
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
            let c = &mut self.champions[i];

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
                section(ui, "스킬", |ui| {
                    for si in 0..3 {
                        let s = &mut c.skills[si];
                        egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(8.0)).rounding(5.0).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(SLOTS_KO[si]).strong());
                                let old = s.template;
                                egui::ComboBox::from_id_source(format!("tmpl{si}")).width(160.0)
                                    .selected_text(TEMPLATES_KO[s.template]).show_ui(ui, |ui| {
                                    for ti in 0..TEMPLATES.len() { ui.selectable_value(&mut s.template, ti, TEMPLATES_KO[ti]); }
                                });
                                if s.template != old { *s = SkillDraft::new(s.template); }
                            });
                            ui.add_space(4.0);
                            let specs = template_params(TEMPLATES[s.template]);
                            egui::Grid::new(format!("p{si}")).num_columns(4).spacing([12.0, 5.0]).show(ui, |ui| {
                                for (idx, (k, v)) in s.params.iter_mut().enumerate() {
                                    let label = specs.iter().find(|(pk, ..)| pk == k).map(|(_, l, ..)| *l).unwrap_or(k.as_str());
                                    let max = specs.iter().find(|(pk, ..)| pk == k).map(|(.., m)| *m).unwrap_or(1000);
                                    ui.label(label);
                                    ui.add(egui::DragValue::new(v).clamp_range(0..=max));
                                    if idx % 2 == 1 { ui.end_row(); }
                                }
                            });
                        });
                        ui.add_space(6.0);
                    }
                });

                ui.add_space(8.0);
            });
        });
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
