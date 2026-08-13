//! 游戏内法环风格存档面板（D3D12 ImGui overlay）。F5 开关。

use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use eldenring::cs::CSMenuManImp;
use fromsoftware_shared::FromStatic;
use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::imgui::{
    Condition, Context, FontConfig, FontGlyphRanges, FontSource, Io, StyleColor, Ui, WindowFlags,
};
use hudhook::windows::Win32::Foundation::HINSTANCE;
use hudhook::{Hudhook, ImguiRenderLoop, MessageFilter, RenderContext};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{ClipCursor, GetCursorPos};

use crate::i18n::{self, Lang};
use crate::library::BuildEntry;
use crate::paths;
use crate::{library, notify, panel};

/// Practice tool `cursor_show`：Esc 暂停菜单置位后解锁鼠标、不再锁视角。
const MENU_CURSOR_SHOW: usize = 0xAC;

const GOLD: [f32; 4] = [0.90, 0.78, 0.38, 1.0];
const GOLD_DIM: [f32; 4] = [0.62, 0.54, 0.30, 1.0];
const CREAM: [f32; 4] = [0.90, 0.84, 0.70, 1.0];
const MUTED: [f32; 4] = [0.58, 0.54, 0.44, 1.0];

/// 力 / 敏 / 智 / 信 / 感 → 红 / 绿 / 蓝 / 黄 / 紫
const COLOR_STR: [f32; 4] = [0.82, 0.22, 0.20, 1.0];
const COLOR_DEX: [f32; 4] = [0.28, 0.68, 0.32, 1.0];
const COLOR_INT: [f32; 4] = [0.28, 0.48, 0.88, 1.0];
const COLOR_FAI: [f32; 4] = [0.90, 0.78, 0.22, 1.0];
const COLOR_ARC: [f32; 4] = [0.62, 0.28, 0.78, 1.0];
const COLOR_STAT_FALLBACK: [f32; 4] = [0.45, 0.42, 0.38, 1.0];

#[derive(Clone, Copy)]
enum DomStat {
    Str,
    Dex,
    Int,
    Fai,
    Arc,
    None,
}

/// 取 BD 力敏智信感中最高一项；并列时优先靠前（力→感）。
fn tile_from_stats(stats: Option<&library::StatsPreview>) -> ([f32; 4], DomStat) {
    let Some(s) = stats else {
        return (COLOR_STAT_FALLBACK, DomStat::None);
    };
    let mut best = s.strength;
    let mut color = COLOR_STR;
    let mut dom = DomStat::Str;
    if s.dexterity > best {
        best = s.dexterity;
        color = COLOR_DEX;
        dom = DomStat::Dex;
    }
    if s.intelligence > best {
        best = s.intelligence;
        color = COLOR_INT;
        dom = DomStat::Int;
    }
    if s.faith > best {
        best = s.faith;
        color = COLOR_FAI;
        dom = DomStat::Fai;
    }
    if s.arcane > best {
        color = COLOR_ARC;
        dom = DomStat::Arc;
    }
    (color, dom)
}

/// 当前语言下属性名的首字/首字母。
fn dom_letter(dom: DomStat) -> &'static str {
    let s = i18n::t();
    let name = match dom {
        DomStat::Str => s.stat_strength,
        DomStat::Dex => s.stat_dexterity,
        DomStat::Int => s.stat_intelligence,
        DomStat::Fai => s.stat_faith,
        DomStat::Arc => s.stat_arcane,
        DomStat::None => return "?",
    };
    let Some(ch) = name.chars().next() else {
        return "?";
    };
    &name[..ch.len_utf8()]
}

enum Confirm {
    Delete { id: String, name: String },
    ClearCombat { id: String, name: String },
}

struct OverlayUi {
    selected_id: Option<String>,
    name_buf: String,
    confirm: Option<Confirm>,
}

impl Default for OverlayUi {
    fn default() -> Self {
        Self {
            selected_id: None,
            name_buf: String::new(),
            confirm: None,
        }
    }
}

static VISIBLE: AtomicBool = AtomicBool::new(false);
/// F6：顶部广播上方的分数/胜率条
static SCOREBOARD: AtomicBool = AtomicBool::new(false);
static HOOK_READY: AtomicBool = AtomicBool::new(false);
static FRAME: AtomicU32 = AtomicU32::new(0);
static CURSOR_HELD: AtomicBool = AtomicBool::new(false);
static FONT_BYTES: OnceLock<Vec<u8>> = OnceLock::new();
static PICKER: Mutex<Option<PickerState>> = Mutex::new(None);

/// 按住 Ctrl 时的 BD 选择条。
struct PickerState {
    ids: Vec<String>,
    names: Vec<String>,
    colors: Vec<[f32; 4]>,
    doms: Vec<DomStat>,
    index: usize,
    last_x: i32,
    accum: i32,
}

fn picker_active() -> bool {
    PICKER.lock().ok().is_some_and(|g| g.is_some())
}

fn overlay_wants_cursor() -> bool {
    VISIBLE.load(Ordering::SeqCst) || picker_active()
}

/// 打开面板时解开游戏鼠标锁定（与 Esc 同旗标）；关闭时还原。
fn apply_menu_cursor(free: bool) {
    if let Ok(menu) = unsafe { CSMenuManImp::instance_mut() } {
        unsafe {
            let byte = (menu as *mut CSMenuManImp as *mut u8).add(MENU_CURSOR_SHOW);
            if free {
                *byte |= 0x01;
                CURSOR_HELD.store(true, Ordering::SeqCst);
            } else if CURSOR_HELD.swap(false, Ordering::SeqCst) {
                *byte &= !0x01;
            }
        }
    }
    if free {
        unsafe {
            let _ = ClipCursor(None);
        }
    }
}

fn close_panel() {
    VISIBLE.store(false, Ordering::SeqCst);
    apply_menu_cursor(false);
    paths::stage("native_ui_overlay_close");
}

fn font_bytes() -> Option<&'static [u8]> {
    let bytes = FONT_BYTES.get_or_init(|| {
        const CANDIDATES: &[&str] = &[
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\msyh.ttf",
            r"C:\Windows\Fonts\simhei.ttf",
            r"C:\Windows\Fonts\msyhbd.ttc",
            r"C:\Windows\Fonts\simsun.ttc",
            r"C:\Windows\Fonts\YuGothM.ttc",
            r"C:\Windows\Fonts\malgun.ttf",
        ];
        for path in CANDIDATES {
            if let Ok(data) = std::fs::read(path) {
                if !data.is_empty() {
                    return data;
                }
            }
        }
        Vec::new()
    });
    if bytes.is_empty() {
        None
    } else {
        Some(bytes.as_slice())
    }
}

fn apply_er_style(ctx: &mut Context) {
    let style = ctx.style_mut();
    style.window_rounding = 0.0;
    style.child_rounding = 0.0;
    style.frame_rounding = 0.0;
    style.popup_rounding = 0.0;
    style.scrollbar_rounding = 0.0;
    style.grab_rounding = 0.0;
    style.tab_rounding = 0.0;
    style.window_border_size = 1.6;
    style.child_border_size = 1.2;
    style.frame_border_size = 1.0;
    style.popup_border_size = 1.4;
    style.window_padding = [18.0, 16.0];
    style.frame_padding = [10.0, 6.0];
    style.item_spacing = [10.0, 8.0];
    style.item_inner_spacing = [8.0, 6.0];
    style.scrollbar_size = 12.0;
    style.window_title_align = [0.5, 0.5];

    style[StyleColor::WindowBg] = [0.055, 0.045, 0.032, 0.96];
    style[StyleColor::ChildBg] = [0.04, 0.032, 0.022, 0.88];
    style[StyleColor::PopupBg] = [0.07, 0.055, 0.038, 0.98];
    style[StyleColor::Border] = [0.72, 0.55, 0.22, 0.95];
    style[StyleColor::BorderShadow] = [0.0, 0.0, 0.0, 0.0];
    style[StyleColor::Text] = CREAM;
    style[StyleColor::TextDisabled] = MUTED;
    style[StyleColor::TitleBg] = [0.10, 0.07, 0.04, 1.0];
    style[StyleColor::TitleBgActive] = [0.18, 0.12, 0.05, 1.0];
    style[StyleColor::TitleBgCollapsed] = [0.10, 0.07, 0.04, 1.0];
    style[StyleColor::FrameBg] = [0.10, 0.08, 0.05, 1.0];
    style[StyleColor::FrameBgHovered] = [0.18, 0.14, 0.08, 1.0];
    style[StyleColor::FrameBgActive] = [0.24, 0.18, 0.08, 1.0];
    style[StyleColor::Button] = [0.14, 0.10, 0.05, 1.0];
    style[StyleColor::ButtonHovered] = [0.30, 0.22, 0.08, 1.0];
    style[StyleColor::ButtonActive] = [0.42, 0.30, 0.10, 1.0];
    style[StyleColor::Header] = [0.28, 0.20, 0.08, 0.95];
    style[StyleColor::HeaderHovered] = [0.36, 0.26, 0.10, 1.0];
    style[StyleColor::HeaderActive] = [0.46, 0.34, 0.12, 1.0];
    style[StyleColor::Separator] = [0.72, 0.55, 0.22, 0.70];
    style[StyleColor::ScrollbarBg] = [0.04, 0.03, 0.02, 0.80];
    style[StyleColor::ScrollbarGrab] = [0.42, 0.32, 0.12, 0.90];
    style[StyleColor::ScrollbarGrabHovered] = [0.58, 0.44, 0.16, 1.0];
    style[StyleColor::ScrollbarGrabActive] = [0.72, 0.55, 0.22, 1.0];
    style[StyleColor::CheckMark] = GOLD;
    style[StyleColor::SliderGrab] = GOLD_DIM;
    style[StyleColor::SliderGrabActive] = GOLD;
    style[StyleColor::ResizeGrip] = [0.72, 0.55, 0.22, 0.40];
    style[StyleColor::ResizeGripHovered] = [0.72, 0.55, 0.22, 0.70];
    style[StyleColor::ResizeGripActive] = GOLD;
}

fn yes_no() -> (&'static str, &'static str) {
    match i18n::language() {
        Lang::Zh => ("确定", "取消"),
        Lang::Ja => ("確定", "キャンセル"),
        Lang::Fr => ("OK", "Annuler"),
        Lang::En => ("OK", "Cancel"),
    }
}

fn selected_entry<'a>(view: &'a panel::ViewState, selected_id: &Option<String>) -> Option<&'a BuildEntry> {
    let id = selected_id
        .as_deref()
        .or(view.active.as_deref())?;
    view.entries.iter().find(|entry| entry.id == id)
}

fn sync_name_if_needed(ui_state: &mut OverlayUi, entry: Option<&BuildEntry>) {
    let id = entry.map(|e| e.id.as_str());
    if ui_state.selected_id.as_deref() != id {
        ui_state.selected_id = id.map(ToOwned::to_owned);
        ui_state.name_buf = entry.map(|e| e.name.clone()).unwrap_or_default();
    }
}

fn gold_rule(ui: &Ui) {
    ui.separator();
}

fn draw_close_x(ui: &Ui) {
    let size = 28.0;
    ui.same_line_with_pos((ui.window_size()[0] - size - 10.0).max(8.0));
    let bg = ui.push_style_color(StyleColor::Button, [0.48, 0.08, 0.08, 1.0]);
    let hover = ui.push_style_color(StyleColor::ButtonHovered, [0.78, 0.12, 0.12, 1.0]);
    let active = ui.push_style_color(StyleColor::ButtonActive, [0.92, 0.18, 0.18, 1.0]);
    let text = ui.push_style_color(StyleColor::Text, [1.0, 0.94, 0.94, 1.0]);
    let clicked = ui.button_with_size("×", [size, size]);
    text.pop();
    active.pop();
    hover.pop();
    bg.pop();
    if clicked {
        close_panel();
    }
}

fn draw_builds(ui: &Ui, state: &mut OverlayUi, view: &panel::ViewState) {
    let s = i18n::t();
    let list_h = ui.content_region_avail()[1] - 150.0;
    let list_h = list_h.max(180.0);

    ui.child_window("er_builds_list")
        .size([300.0, list_h])
        .border(true)
        .build(|| {
            ui.text_colored(GOLD_DIM, s.section_builds);
            gold_rule(ui);
            if view.entries.is_empty() {
                ui.text_colored(MUTED, s.backpack_empty);
                return;
            }
            for entry in &view.entries {
                let marker = (0..4)
                    .filter(|binding| view.bindings[*binding].as_deref() == Some(entry.id.as_str()))
                    .map(|binding| format!("F{}", binding + 1))
                    .collect::<Vec<_>>()
                    .join("/");
                let title = if marker.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}  [{}]", entry.name, marker)
                };
                let selected = state.selected_id.as_deref() == Some(entry.id.as_str())
                    || (state.selected_id.is_none() && view.active.as_deref() == Some(entry.id.as_str()));
                let label = format!("{title}##{}", entry.id);
                if ui
                    .selectable_config(&label)
                    .selected(selected)
                    .size([0.0, 28.0])
                    .build()
                {
                    state.selected_id = Some(entry.id.clone());
                    state.name_buf = entry.name.clone();
                    let _ = library::set_active(&entry.id);
                }
            }
        });
    ui.same_line();
    ui.child_window("er_builds_detail")
        .size([0.0, list_h])
        .border(true)
        .build(|| {
            ui.text_colored(GOLD_DIM, s.section_name);
            gold_rule(ui);
            ui.set_next_item_width(-1.0);
            ui.input_text("##build_name", &mut state.name_buf).build();
            ui.dummy([0.0, 4.0]);
            ui.text_colored(GOLD_DIM, s.section_stats);
            gold_rule(ui);
            let labels = [
                s.stat_level,
                s.stat_vigor,
                s.stat_mind,
                s.stat_endurance,
                s.stat_strength,
                s.stat_dexterity,
                s.stat_intelligence,
                s.stat_faith,
                s.stat_arcane,
            ];
            if let Some(stats) = selected_entry(view, &state.selected_id).and_then(|e| e.stats.as_ref())
            {
                let values = [
                    stats.level,
                    stats.vigor,
                    stats.mind,
                    stats.endurance,
                    stats.strength,
                    stats.dexterity,
                    stats.intelligence,
                    stats.faith,
                    stats.arcane,
                ];
                ui.columns(3, "er_stats", false);
                for (label, value) in labels.iter().zip(values) {
                    ui.text(format!("{label}  {value}"));
                    ui.next_column();
                }
                ui.columns(1, "er_stats_reset", false);
            } else {
                ui.text_colored(MUTED, s.no_stats);
            }
            ui.dummy([0.0, 6.0]);
            ui.text_colored(GOLD_DIM, s.section_combat);
            gold_rule(ui);
            if let Some(entry) = selected_entry(view, &state.selected_id) {
                let total = entry.combat.kills.saturating_add(entry.combat.deaths);
                let rate = if total == 0 {
                    0.0
                } else {
                    entry.combat.kills as f64 * 100.0 / total as f64
                };
                ui.text(format!(
                    "{}  {}    {}  {}    {}  {rate:.1}%",
                    s.combat_wins,
                    entry.combat.kills,
                    s.combat_losses,
                    entry.combat.deaths,
                    s.combat_winrate
                ));
            } else {
                ui.text_colored(MUTED, "—");
            }
            ui.dummy([0.0, 6.0]);
            let describe = |index: usize| {
                let name = view.bindings[index]
                    .as_ref()
                    .map(|id| {
                        view.entries
                            .iter()
                            .find(|entry| &entry.id == id)
                            .map(|entry| entry.name.clone())
                            .unwrap_or_else(|| format!("{} {id}", s.missing))
                    })
                    .unwrap_or_else(|| s.unbound.to_string());
                format!("F{}  {name}", index + 1)
            };
            ui.text_colored(MUTED, &format!(
                "{}    {}    {}    {}",
                describe(0),
                describe(1),
                describe(2),
                describe(3)
            ));
            if view.busy {
                ui.text_colored(GOLD, s.syncing);
            }
            if let Some(error) = &view.error {
                ui.text_colored([0.85, 0.35, 0.25, 1.0], &format!("{}{error}", s.refresh_failed));
            }
        });

    gold_rule(ui);
    if ui.button(s.btn_new) {
        match panel::cached_snapshot()
            .ok_or_else(|| s.capture_failed.to_string())
            .and_then(|snapshot| library::create(&snapshot))
        {
            Ok(entry) => {
                state.selected_id = Some(entry.id.clone());
                state.name_buf = entry.name.clone();
                panel::mark_selected(entry.id);
                panel::publish_library();
                panel::notice(s.created_selected.to_owned());
            }
            Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
        }
    }
    ui.same_line();
    if ui.button(s.btn_overwrite) {
        if let Some(entry) = selected_entry(view, &state.selected_id) {
            let id = entry.id.clone();
            match panel::cached_snapshot()
                .ok_or_else(|| s.capture_failed.to_string())
                .and_then(|snapshot| library::overwrite(&id, &snapshot))
            {
                Ok(entry) => {
                    state.selected_id = Some(entry.id.clone());
                    state.name_buf = entry.name.clone();
                    panel::mark_selected(entry.id);
                    panel::publish_library();
                    panel::notice(s.overwritten_selected.to_owned());
                }
                Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
            }
        } else {
            panel::notice(s.no_selection.to_owned());
        }
    }
    ui.same_line();
    if ui.button(s.btn_load) {
        if let Some(entry) = selected_entry(view, &state.selected_id) {
            panel::queue_load(entry.id.clone());
            close_panel();
        } else {
            panel::notice(s.no_selection.to_owned());
        }
    }
    ui.same_line();
    if ui.button(s.btn_rename) {
        if let Some(entry) = selected_entry(view, &state.selected_id) {
            let name = state.name_buf.trim();
            if name.is_empty() {
                panel::notice(s.name_empty.to_owned());
            } else {
                match library::rename(&entry.id, name) {
                    Ok(()) => {
                        panel::publish_library();
                        panel::notice(s.renamed.to_owned());
                    }
                    Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
                }
            }
        } else {
            panel::notice(s.no_selection.to_owned());
        }
    }
    ui.same_line();
    if ui.button(s.btn_delete) {
        if let Some(entry) = selected_entry(view, &state.selected_id) {
            state.confirm = Some(Confirm::Delete {
                id: entry.id.clone(),
                name: entry.name.clone(),
            });
            ui.open_popup("er_confirm");
        } else {
            panel::notice(s.no_selection.to_owned());
        }
    }

    if ui.button(s.btn_bind_f1) {
        bind_selected(state, view, 0);
    }
    ui.same_line();
    if ui.button(s.btn_bind_f2) {
        bind_selected(state, view, 1);
    }
    ui.same_line();
    if ui.button(s.btn_bind_f3) {
        bind_selected(state, view, 2);
    }
    ui.same_line();
    if ui.button(s.btn_bind_f4) {
        bind_selected(state, view, 3);
    }
    ui.same_line();
    if ui.button(if view.care { s.care_on } else { s.care_off }) {
        let enabled = !library::care_enabled();
        match library::set_care_enabled(enabled) {
            Ok(()) => {
                panel::publish_library();
                panel::notice(if enabled {
                    s.care_enabled_msg.to_owned()
                } else {
                    s.care_disabled_msg.to_owned()
                });
            }
            Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
        }
    }
    ui.same_line();
    if ui.button(if view.ctrl_picker {
        s.ctrl_picker_on
    } else {
        s.ctrl_picker_off
    }) {
        let enabled = !library::ctrl_picker_enabled();
        match library::set_ctrl_picker_enabled(enabled) {
            Ok(()) => {
                if !enabled {
                    picker_cancel();
                }
                panel::publish_library();
                panel::notice(if enabled {
                    s.ctrl_picker_enabled_msg.to_owned()
                } else {
                    s.ctrl_picker_disabled_msg.to_owned()
                });
            }
            Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
        }
    }

    if ui.button(s.btn_clear_combat) {
        if let Some(entry) = selected_entry(view, &state.selected_id) {
            state.confirm = Some(Confirm::ClearCombat {
                id: entry.id.clone(),
                name: entry.name.clone(),
            });
            ui.open_popup("er_confirm");
        } else {
            panel::notice(s.no_selection.to_owned());
        }
    }
    ui.same_line();
    if ui.button(s.btn_refresh) {
        panel::refresh_snapshot();
        panel::publish_library();
        panel::notice(s.list_refreshed.to_owned());
    }
    ui.same_line();
    if ui.button(s.lang_button) {
        match library::cycle_language() {
            Ok(_) => panel::publish_library(),
            Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
        }
    }
    ui.same_line();
    if ui.button(s.btn_close) {
        close_panel();
    }
}

fn bind_selected(state: &OverlayUi, view: &panel::ViewState, index: usize) {
    let s = i18n::t();
    if let Some(entry) = selected_entry(view, &state.selected_id) {
        match library::bind(index, Some(&entry.id)) {
            Ok(()) => {
                panel::publish_library();
                panel::notice(i18n::fmt(s.bound_fn, [index + 1]));
            }
            Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
        }
    } else {
        panel::notice(s.no_selection.to_owned());
    }
}

fn draw_confirm(ui: &Ui, state: &mut OverlayUi) {
    let s = i18n::t();
    let (yes, no) = yes_no();
    ui.modal_popup_config("er_confirm")
        .always_auto_resize(true)
        .build(|| {
            match &state.confirm {
                Some(Confirm::Delete { name, .. }) => {
                    ui.text(i18n::fmt(s.confirm_delete, [name]));
                }
                Some(Confirm::ClearCombat { name, .. }) => {
                    ui.text(i18n::fmt(s.confirm_clear_combat, [name]));
                }
                None => ui.text(""),
            }
            ui.dummy([0.0, 8.0]);
            if ui.button_with_size(yes, [96.0, 32.0]) {
                match state.confirm.take() {
                    Some(Confirm::Delete { id, .. }) => match library::delete(&id) {
                        Ok(()) => {
                            state.selected_id = None;
                            state.name_buf.clear();
                            panel::publish_library();
                            panel::notice(s.deleted.to_owned());
                        }
                        Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
                    },
                    Some(Confirm::ClearCombat { id, .. }) => match library::clear_combat(&id) {
                        Ok(()) => {
                            panel::publish_library();
                            panel::notice(s.combat_cleared.to_owned());
                        }
                        Err(error) => panel::notice(i18n::fmt(s.op_failed, [&error])),
                    },
                    None => {}
                }
                ui.close_current_popup();
            }
            ui.same_line();
            if ui.button_with_size(no, [96.0, 32.0]) {
                state.confirm = None;
                ui.close_current_popup();
            }
        });
}

impl ImguiRenderLoop for OverlayUi {
    fn initialize(&mut self, ctx: &mut Context, _render_context: &mut dyn RenderContext) {
        apply_er_style(ctx);
        if let Some(bytes) = font_bytes() {
            ctx.fonts().add_font(&[FontSource::TtfData {
                data: bytes,
                size_pixels: 18.0,
                config: Some(FontConfig {
                    glyph_ranges: FontGlyphRanges::chinese_simplified_common(),
                    rasterizer_multiply: 1.15,
                    pixel_snap_h: true,
                    ..FontConfig::default()
                }),
            }]);
        }
    }

    fn before_render(&mut self, ctx: &mut Context, _render_context: &mut dyn RenderContext) {
        ctx.io_mut().mouse_draw_cursor = VISIBLE.load(Ordering::SeqCst);
    }

    fn message_filter(&self, _io: &Io) -> MessageFilter {
        if overlay_wants_cursor() {
            MessageFilter::InputAll
        } else {
            MessageFilter::empty()
        }
    }

    fn render(&mut self, ui: &mut Ui) {
        draw_scoreboard(ui);
        draw_picker(ui);
        if !VISIBLE.load(Ordering::SeqCst) {
            return;
        }
        let s = i18n::t();
        let view = panel::view_snapshot();
        let selected_now = self.selected_id.clone();
        let fallback = selected_entry(&view, &selected_now).or_else(|| {
            view.active
                .as_ref()
                .and_then(|id| view.entries.iter().find(|e| &e.id == id))
                .or_else(|| view.entries.first())
        });
        sync_name_if_needed(self, fallback);

        let [dw, dh] = ui.io().display_size;
        let width = (dw * 0.60).clamp(780.0, 1080.0);
        let height = (dh * 0.70).clamp(540.0, 760.0);

        ui.window("##erdueltools_panel")
            .size([width, height], Condition::FirstUseEver)
            .position([dw * 0.08, dh * 0.10], Condition::FirstUseEver)
            .title_bar(false)
            .collapsible(false)
            .build(|| {
                ui.text_colored(GOLD, s.panel_title);
                draw_close_x(ui);
                gold_rule(ui);
                draw_builds(ui, self, &view);
                draw_confirm(ui, self);
            });
    }
}

/// 右上角：当前存档胜负与胜率。
fn draw_scoreboard(ui: &mut Ui) {
    if !SCOREBOARD.load(Ordering::SeqCst) {
        return;
    }
    let s = i18n::t();
    let [dw, _dh] = ui.io().display_size;
    let width = (dw * 0.28).clamp(260.0_f32, 400.0_f32);
    let height = 64.0_f32;
    let margin = 16.0_f32;
    let x = dw - width - margin;
    let y = margin;
    // 文字右侧内边距：躲开盖在框右边的 scroll，框本身仍贴右上角
    let text_right_pad = 96.0_f32;
    let text_left_pad = 12.0_f32;

    let (title, line) = match library::active_combat() {
        Some((name, record)) => {
            let total = record.kills.saturating_add(record.deaths);
            let wr = if total == 0 {
                0.0
            } else {
                record.kills as f64 * 100.0 / total as f64
            };
            (
                name,
                format!(
                    "{} {}  ·  {} {}  ·  {} {:.1}%",
                    s.combat_wins,
                    record.kills,
                    s.combat_losses,
                    record.deaths,
                    s.combat_winrate,
                    wr
                ),
            )
        }
        None => (s.no_active_combat.to_owned(), "—".to_owned()),
    };

    ui.window("##erdueltools_scoreboard")
        .size([width, height], Condition::Always)
        .position([x, y], Condition::Always)
        .flags(WindowFlags::NO_INPUTS | WindowFlags::NO_NAV | WindowFlags::NO_FOCUS_ON_APPEARING)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .movable(false)
        .bg_alpha(0.72)
        .build(|| {
            let max_w = (width - text_left_pad - text_right_pad).max(40.0);
            let title = fit_text(ui, &title, max_w);
            let line = fit_text(ui, &line, max_w);
            ui.set_cursor_pos([text_left_pad, ui.cursor_pos()[1] + 4.0]);
            ui.text_colored(GOLD, &title);
            ui.set_cursor_pos([text_left_pad, ui.cursor_pos()[1]]);
            ui.text_colored(CREAM, &line);
        });
}

fn fit_text(ui: &Ui, text: &str, max_w: f32) -> String {
    if ui.calc_text_size(text)[0] <= max_w {
        return text.to_owned();
    }
    let ellipsis = "…";
    let ew = ui.calc_text_size(ellipsis)[0];
    let mut out = String::new();
    for ch in text.chars() {
        let mut trial = out.clone();
        trial.push(ch);
        if ui.calc_text_size(&trial)[0] + ew > max_w {
            out.push_str(ellipsis);
            return out;
        }
        out.push(ch);
    }
    out
}

/// F6 开关顶部分数/胜率条。返回开启后的状态。
pub fn toggle_scoreboard() -> bool {
    let next = !SCOREBOARD.load(Ordering::SeqCst);
    SCOREBOARD.store(next, Ordering::SeqCst);
    paths::stage(if next {
        "native_ui_scoreboard_on"
    } else {
        "native_ui_scoreboard_off"
    });
    next
}

pub fn scoreboard_visible() -> bool {
    SCOREBOARD.load(Ordering::SeqCst)
}

/// 滑动窗口起点：始终露出最多 4 个方块，选中项尽量居中。
fn picker_window_start(idx: usize, n: usize, win: usize) -> usize {
    if n <= win {
        return 0;
    }
    let half = win / 2;
    if idx <= half {
        0
    } else if idx + win - half >= n {
        n - win
    } else {
        idx - half
    }
}

fn draw_picker(ui: &mut Ui) {
    let Ok(guard) = PICKER.lock() else {
        return;
    };
    let Some(picker) = guard.as_ref() else {
        return;
    };
    if picker.ids.is_empty() {
        return;
    }
    let s = i18n::t();
    let [dw, dh] = ui.io().display_size;
    let n = picker.ids.len();
    let idx = picker.index.min(n - 1);
    let win = n.min(4);
    let start = picker_window_start(idx, n, win);

    const TILE: f32 = 78.0;
    const GAP: f32 = 18.0;
    const PAD: f32 = 20.0;
    let row_w = win as f32 * TILE + (win.saturating_sub(1) as f32) * GAP;
    let width = row_w + PAD * 2.0;
    let height = 168.0;
    let x = (dw - width) * 0.5;
    // 再往上一点
    let y = dh * 0.22 - height * 0.5;

    ui.window("##erdueltools_ctrl_picker")
        .size([width, height], Condition::Always)
        .position([x, y], Condition::Always)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .movable(false)
        .bg_alpha(0.82)
        .build(|| {
            ui.text_colored(GOLD_DIM, s.picker_hint);
            ui.spacing();
            let origin = ui.cursor_screen_pos();
            let mut glyphs: Vec<(usize, [f32; 4], &'static str, f32, f32, [f32; 2])> =
                Vec::with_capacity(win);
            for i in 0..win {
                let bi = start + i;
                let color = picker
                    .colors
                    .get(bi)
                    .copied()
                    .unwrap_or(COLOR_STAT_FALLBACK);
                let letter = picker
                    .doms
                    .get(bi)
                    .copied()
                    .map(dom_letter)
                    .unwrap_or("?");
                let px = origin[0] + i as f32 * (TILE + GAP);
                let py = origin[1];
                let size = ui.calc_text_size(letter);
                glyphs.push((bi, color, letter, px, py, size));
            }
            {
                let draw = ui.get_window_draw_list();
                for &(bi, color, letter, px, py, size) in &glyphs {
                    draw.add_rect([px, py], [px + TILE, py + TILE], color)
                        .filled(true)
                        .rounding(3.0)
                        .build();
                    if bi == idx {
                        draw.add_rect(
                            [px - 3.0, py - 3.0],
                            [px + TILE + 3.0, py + TILE + 3.0],
                            GOLD,
                        )
                        .thickness(3.0)
                        .rounding(4.0)
                        .build();
                    } else {
                        draw.add_rect([px, py], [px + TILE, py + TILE], GOLD_DIM)
                            .thickness(1.0)
                            .rounding(3.0)
                            .build();
                    }
                    let tx = px + (TILE - size[0]) * 0.5;
                    let ty = py + (TILE - size[1]) * 0.5;
                    draw.add_text([tx, ty], CREAM, letter);
                }
            }
            ui.dummy([row_w, TILE + 10.0]);
            let name = &picker.names[idx];
            let name_w = ui.calc_text_size(name)[0];
            let indent = ((row_w - name_w) * 0.5).max(0.0);
            if indent > 0.0 {
                ui.set_cursor_pos([ui.cursor_pos()[0] + indent, ui.cursor_pos()[1]]);
            }
            ui.text_colored(CREAM, name);
        });
}

/// 按住 Ctrl：打开选择条（左右移鼠标切换）。
pub fn picker_begin() -> bool {
    if !HOOK_READY.load(Ordering::SeqCst) {
        return false;
    }
    let Ok(entries) = library::list() else {
        return false;
    };
    if entries.is_empty() {
        notify::say(i18n::t().picker_empty);
        return false;
    }
    let active = library::active();
    let index = active
        .as_ref()
        .and_then(|id| entries.iter().position(|e| &e.id == id))
        .unwrap_or(0);
    let mut last_x = 0i32;
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        last_x = pt.x;
    }
    let mut colors = Vec::with_capacity(entries.len());
    let mut doms = Vec::with_capacity(entries.len());
    for e in &entries {
        let (color, dom) = tile_from_stats(e.stats.as_ref());
        colors.push(color);
        doms.push(dom);
    }
    let state = PickerState {
        ids: entries.iter().map(|e| e.id.clone()).collect(),
        names: entries.iter().map(|e| e.name.clone()).collect(),
        colors,
        doms,
        index,
        last_x,
        accum: 0,
    };
    if let Ok(mut guard) = PICKER.lock() {
        *guard = Some(state);
    }
    apply_menu_cursor(true);
    paths::stage("native_ui_picker_begin");
    true
}

/// 按住 Ctrl 期间：根据鼠标水平位移切换选中项。
pub fn picker_tick() {
    let Ok(mut guard) = PICKER.lock() else {
        return;
    };
    let Some(picker) = guard.as_mut() else {
        return;
    };
    if picker.ids.is_empty() {
        return;
    }
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let dx = pt.x - picker.last_x;
    picker.last_x = pt.x;
    picker.accum += dx;
    // 灵敏度略低：需要更大水平位移才切档
    const STEP: i32 = 96;
    while picker.accum <= -STEP {
        picker.accum += STEP;
        if picker.index == 0 {
            picker.accum = 0;
            break;
        }
        picker.index -= 1;
    }
    while picker.accum >= STEP {
        picker.accum -= STEP;
        if picker.index + 1 >= picker.ids.len() {
            picker.accum = 0;
            break;
        }
        picker.index += 1;
    }
}

/// 松开 Ctrl：关闭选择条，返回选中存档 id。
pub fn picker_end() -> Option<String> {
    let state = PICKER.lock().ok()?.take()?;
    apply_menu_cursor(VISIBLE.load(Ordering::SeqCst));
    paths::stage("native_ui_picker_end");
    if state.ids.is_empty() {
        return None;
    }
    let idx = state.index.min(state.ids.len() - 1);
    Some(state.ids[idx].clone())
}

pub fn picker_cancel() {
    if let Ok(mut guard) = PICKER.lock() {
        *guard = None;
    }
    apply_menu_cursor(VISIBLE.load(Ordering::SeqCst));
}

/// 在 D3D12 Present 上安装游戏内 overlay。应在游戏窗口起来后调用一次。
pub fn install_overlay(hmodule: usize) {
    if HOOK_READY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        paths::stage("native_ui_overlay_install");
        match Hudhook::builder()
            .with::<ImguiDx12Hooks>(OverlayUi::default())
            .with_hmodule(HINSTANCE(hmodule as *mut _))
            .build()
            .apply()
        {
            Ok(()) => paths::stage("native_ui_overlay_ready"),
            Err(error) => {
                HOOK_READY.store(false, Ordering::SeqCst);
                paths::stage(&format!("native_ui_overlay_fail_{error:?}"));
            }
        }
    });
}

/// 打开游戏内存档面板（已打开则保持）。hook 未就绪时返回 false。
pub fn open_panel() -> bool {
    if !HOOK_READY.load(Ordering::SeqCst) {
        return false;
    }
    if VISIBLE.load(Ordering::SeqCst) {
        return true;
    }
    panel::refresh_snapshot();
    panel::publish_library();
    VISIBLE.store(true, Ordering::SeqCst);
    apply_menu_cursor(true);
    notify::say(i18n::t().panel_title);
    paths::stage("native_ui_overlay_panel");
    true
}

/// 打开/切换游戏内存档面板。hook 未就绪时返回 false，调用方可回退 Win32。
pub fn toggle_panel() -> bool {
    if !HOOK_READY.load(Ordering::SeqCst) {
        return false;
    }
    if VISIBLE.load(Ordering::SeqCst) {
        close_panel();
        return true;
    }
    open_panel()
}

/// 游戏线程：维持鼠标解锁；面板开着时刷新快照。
pub fn poll() {
    let want = overlay_wants_cursor();
    apply_menu_cursor(want);
    if !VISIBLE.load(Ordering::SeqCst) {
        return;
    }
    let tick = FRAME.fetch_add(1, Ordering::Relaxed);
    if tick % 20 == 0 {
        panel::refresh_snapshot();
        panel::publish_library();
    }
}
