//! F5 存档管理面板。
//!
//! 窗口和消息循环运行在独立 UI 线程；按钮只向 `PENDING` 投递动作，
//! 由游戏线程每帧调用 [`poll`] 执行。尤其是 `sync::start` 绝不会在 UI
//! 线程调用。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DEFAULT_PITCH, DeleteObject, FF_DONTCARE, HBRUSH, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
    SetBkColor, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, BN_CLICKED, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, GetMessageW, GetSystemMetrics,
    GetWindowTextLengthW, GetWindowTextW, HCURSOR, HICON, IDC_ARROW, IDYES, LB_ADDSTRING,
    LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, LB_SETHORIZONTALEXTENT, LB_SETITEMHEIGHT,
    LBN_SELCHANGE, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LoadCursorW, MB_ICONWARNING, MB_YESNO, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowTextW, ShowWindow, TranslateMessage, WINDOW_STYLE,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_SETFONT, WM_TIMER, WNDCLASSW,
    WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_TOPMOST, WS_HSCROLL, WS_MINIMIZEBOX,
    WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::library::BuildEntry;

#[derive(Clone)]
enum Action {
    Load(String),
}

#[derive(Clone, Default)]
pub(crate) struct ViewState {
    pub entries: Vec<BuildEntry>,
    pub bindings: [Option<String>; 4],
    pub active: Option<String>,
    pub care: bool,
    pub ctrl_picker: bool,
    pub busy: bool,
    pub error: Option<String>,
    pub revision: u64,
}

const ID_LIST: i32 = 101;
const ID_NAME: i32 = 102;
const ID_SAVE: i32 = 110;
const ID_LOAD: i32 = 111;
const ID_RENAME: i32 = 112;
const ID_DELETE: i32 = 113;
const ID_OVERWRITE: i32 = 114;
const ID_BIND_F1: i32 = 121;
const ID_BIND_F2: i32 = 122;
const ID_BIND_F3: i32 = 123;
const ID_BIND_F4: i32 = 124;
const ID_CARE: i32 = 131;
const ID_CTRL_PICKER: i32 = 132;
const ID_REFRESH: i32 = 141;
const ID_CLOSE: i32 = 142;
const ID_CLEAR_COMBAT: i32 = 143;
const ID_LANG: i32 = 144;
const ID_TITLE_LABEL: i32 = 201;
const ID_SECTION_BUILDS: i32 = 202;
const ID_SECTION_STATS: i32 = 203;
const ID_SECTION_COMBAT: i32 = 204;
const ID_SECTION_NAME: i32 = 205;

const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 350;
const PUBLISH_EVERY_FRAMES: u32 = 30;
const CLASS_NAME: PCWSTR = w!("erdueltools_library_panel");
const BG: COLORREF = COLORREF(0x00f7_f7f7);
const FG: COLORREF = COLORREF(0x0020_2020);

const CLIENT_W: i32 = 700;
const CLIENT_H: i32 = 738;
const MARGIN: i32 = 18;
const CONTENT_W: i32 = CLIENT_W - MARGIN * 2;
const GAP: i32 = 8;
const BTN_H: i32 = 34;
const LIST_H: i32 = 215;
const LIST_ITEM_H: i32 = 30;
const UI_FONT_PT: i32 = 14;
const TITLE_FONT_PT: i32 = 17;
static PANEL_HWND: AtomicIsize = AtomicIsize::new(0);
static LIST_HWND: AtomicIsize = AtomicIsize::new(0);
static NAME_HWND: AtomicIsize = AtomicIsize::new(0);
static BINDINGS_HWND: AtomicIsize = AtomicIsize::new(0);
static CARE_HWND: AtomicIsize = AtomicIsize::new(0);
static CTRL_PICKER_HWND: AtomicIsize = AtomicIsize::new(0);
static LANG_HWND: AtomicIsize = AtomicIsize::new(0);
static STAT_HWNDS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static COMBAT_HWNDS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static STATIC_LABELS: Mutex<Vec<(i32, isize)>> = Mutex::new(Vec::new());
static BUTTON_HWNDS: Mutex<Vec<(i32, isize)>> = Mutex::new(Vec::new());
static UI_FONT: AtomicIsize = AtomicIsize::new(0);
static TITLE_FONT: AtomicIsize = AtomicIsize::new(0);
static PANEL_DPI: AtomicU32 = AtomicU32::new(96);
static BG_BRUSH: AtomicIsize = AtomicIsize::new(0);
static CLASS_READY: AtomicBool = AtomicBool::new(false);
static UI_RUNNING: AtomicBool = AtomicBool::new(false);
static FRAME: AtomicU32 = AtomicU32::new(0);

static PENDING: Mutex<Vec<Action>> = Mutex::new(Vec::new());
static PENDING_NOTICES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PANEL_SNAPSHOT: Mutex<Option<crate::snap::BuildSnapshot>> = Mutex::new(None);
static VIEW: Mutex<ViewState> = Mutex::new(ViewState {
    entries: Vec::new(),
    bindings: [None, None, None, None],
    active: None,
    care: false,
    ctrl_picker: false,
    busy: false,
    error: None,
    revision: 0,
});
/// UI 线程最后一次真正画进 ListBox 的条目，用于稳定地把选中行映射到 id。
static DISPLAYED: Mutex<Vec<BuildEntry>> = Mutex::new(Vec::new());
static SHOWN_REVISION: Mutex<u64> = Mutex::new(u64::MAX);
static SELECT_AFTER_REFRESH: Mutex<Option<String>> = Mutex::new(None);
static LAST_DETAIL_ID: Mutex<Option<String>> = Mutex::new(None);

fn push_action(action: Action) {
    if let Ok(mut queue) = PENDING.lock() {
        if queue.len() < 32 {
            queue.push(action);
        }
    }
}

fn push_notice(message: String) {
    if let Ok(mut notices) = PENDING_NOTICES.lock() {
        notices.push(message);
    }
}

pub(crate) fn queue_load(id: String) {
    push_action(Action::Load(id));
}

pub(crate) fn notice(message: String) {
    push_notice(message);
}

pub(crate) fn refresh_snapshot() {
    if let Ok(mut cached) = PANEL_SNAPSHOT.lock() {
        *cached = crate::snap::capture().ok();
    }
}

pub(crate) fn cached_snapshot() -> Option<crate::snap::BuildSnapshot> {
    PANEL_SNAPSHOT.lock().ok().and_then(|cached| cached.clone())
}

pub(crate) fn view_snapshot() -> ViewState {
    VIEW.lock().ok().map(|view| view.clone()).unwrap_or_default()
}

pub(crate) fn publish_library() {
    publish_state();
}

pub(crate) fn mark_selected(id: String) {
    if let Ok(mut selected) = SELECT_AFTER_REFRESH.lock() {
        *selected = Some(id);
    }
}

/// F5：面板未打开时启动独立 UI 线程，已打开时异步关闭。
pub fn toggle_async() {
    let hwnd = PANEL_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        return;
    }
    if UI_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Ok(mut cached) = PANEL_SNAPSHOT.lock() {
        *cached = crate::snap::capture().ok();
    }
    std::thread::spawn(|| {
        unsafe { ui_thread() };
        UI_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// 同步/加载成功后立即让面板选中对应存档，避免旧列表选中项覆盖 active。
pub fn select_build(id: &str) {
    if let Ok(mut selected) = SELECT_AFTER_REFRESH.lock() {
        *selected = Some(id.to_owned());
    }
    publish_state();
    let hwnd = PANEL_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut _)),
                WM_TIMER,
                WPARAM(TIMER_ID),
                LPARAM(0),
            );
        }
    }
}

/// 游戏 `FrameBegin` 调用：执行 UI 动作，并定期发布存档、绑定及忙碌状态。
pub fn poll() {
    let tick = FRAME.fetch_add(1, Ordering::Relaxed);
    let periodic = PANEL_HWND.load(Ordering::SeqCst) != 0 && tick % PUBLISH_EVERY_FRAMES == 0;
    let actions = match PENDING.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(_) => Vec::new(),
    };

    let changed = !actions.is_empty();
    for action in actions {
        run(action);
    }
    let notices = PENDING_NOTICES
        .lock()
        .map(|mut notices| std::mem::take(&mut *notices))
        .unwrap_or_default();
    for message in notices {
        crate::notify::say(&message);
    }
    if periodic || changed {
        publish_state();
    }
}

fn run(action: Action) {
    let result = match action {
        Action::Load(id) => {
            if crate::sync::is_busy() {
                Err(crate::i18n::t().sync_busy.to_string())
            } else {
                crate::library::load(&id)
                    .and_then(|snapshot| {
                        crate::sync::start(snapshot).and_then(|summary| {
                            crate::library::set_active(&id)?;
                            select_build(&id);
                            Ok(summary)
                        })
                    })
                    .map(|summary| crate::i18n::fmt(crate::i18n::t().start_sync, [summary]))
            }
        }
    };

    match result {
        Ok(message) => crate::notify::say(&message),
        Err(error) => crate::notify::say(&crate::i18n::fmt(crate::i18n::t().op_failed, [&error])),
    }
}

fn publish_state() {
    let (entries, error) = match crate::library::list() {
        Ok(entries) => (entries, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let bindings = [
        crate::library::binding(0),
        crate::library::binding(1),
        crate::library::binding(2),
        crate::library::binding(3),
    ];
    if let Ok(mut view) = VIEW.lock() {
        view.entries = entries;
        view.bindings = bindings;
        view.active = crate::library::active();
        view.care = crate::library::care_enabled();
        view.ctrl_picker = crate::library::ctrl_picker_enabled();
        view.busy = crate::sync::is_busy();
        view.error = error;
        view.revision = view.revision.wrapping_add(1);
    }
}

fn publish_library_state_from_ui(error: Option<String>) {
    let entries = crate::library::list().unwrap_or_default();
    let bindings = [
        crate::library::binding(0),
        crate::library::binding(1),
        crate::library::binding(2),
        crate::library::binding(3),
    ];
    if let Ok(mut view) = VIEW.lock() {
        view.entries = entries;
        view.bindings = bindings;
        view.active = crate::library::active();
        view.care = crate::library::care_enabled();
        view.ctrl_picker = crate::library::ctrl_picker_enabled();
        view.error = error;
        view.revision = view.revision.wrapping_add(1);
    }
}

fn run_library_action(result: Result<(), String>, success: &str) {
    match result {
        Ok(()) => {
            push_notice(success.to_owned());
            publish_library_state_from_ui(None);
        }
        Err(error) => {
            push_notice(crate::i18n::fmt(crate::i18n::t().op_failed, [&error]));
            publish_library_state_from_ui(Some(error));
        }
    }
}

fn brush() -> HBRUSH {
    let cached = BG_BRUSH.load(Ordering::SeqCst);
    if cached != 0 {
        return HBRUSH(cached as *mut _);
    }
    let value = unsafe { CreateSolidBrush(BG) };
    BG_BRUSH.store(value.0 as isize, Ordering::SeqCst);
    value
}

unsafe fn make_font(dpi: u32, points: i32, bold: bool, face: &str) -> HFONT {
    let face = HSTRING::from(face);
    unsafe {
        CreateFontW(
            -(points * dpi as i32 / 96),
            0,
            0,
            0,
            if bold { 700 } else { 400 },
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 as u32) | (FF_DONTCARE.0 as u32),
            PCWSTR(face.as_ptr()),
        )
    }
}

fn set_control_font(hwnd: isize, font: HFONT) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        SendMessageW(
            HWND(hwnd as *mut _),
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}

unsafe fn apply_list_item_height(dpi: u32) {
    let list = LIST_HWND.load(Ordering::SeqCst);
    if list == 0 {
        return;
    }
    let height = (LIST_ITEM_H * dpi as i32 / 96).max(22);
    unsafe {
        SendMessageW(
            HWND(list as *mut _),
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(height as isize)),
        );
    }
}

unsafe fn recreate_ui_fonts(dpi: u32) {
    let face = crate::i18n::t().ui_font;
    let ui_font = unsafe { make_font(dpi, UI_FONT_PT, false, face) };
    let title_font = unsafe { make_font(dpi, TITLE_FONT_PT, true, face) };
    let old_ui = UI_FONT.swap(ui_font.0 as isize, Ordering::SeqCst);
    let old_title = TITLE_FONT.swap(title_font.0 as isize, Ordering::SeqCst);
    if old_ui != 0 {
        let _ = unsafe { DeleteObject(HGDIOBJ(old_ui as *mut _)) };
    }
    if old_title != 0 {
        let _ = unsafe { DeleteObject(HGDIOBJ(old_title as *mut _)) };
    }
    let ui = HFONT(UI_FONT.load(Ordering::SeqCst) as *mut _);
    let title = HFONT(TITLE_FONT.load(Ordering::SeqCst) as *mut _);
    if let Ok(labels) = STATIC_LABELS.lock() {
        for (id, handle) in labels.iter().copied() {
            set_control_font(
                handle,
                if id == ID_TITLE_LABEL { title } else { ui },
            );
        }
    }
    if let Ok(buttons) = BUTTON_HWNDS.lock() {
        for (_, handle) in buttons.iter().copied() {
            set_control_font(handle, ui);
        }
    }
    set_control_font(LIST_HWND.load(Ordering::SeqCst), ui);
    set_control_font(NAME_HWND.load(Ordering::SeqCst), ui);
    set_control_font(BINDINGS_HWND.load(Ordering::SeqCst), ui);
    set_control_font(CARE_HWND.load(Ordering::SeqCst), ui);
    set_control_font(CTRL_PICKER_HWND.load(Ordering::SeqCst), ui);
    set_control_font(LANG_HWND.load(Ordering::SeqCst), ui);
    if let Ok(stats) = STAT_HWNDS.lock() {
        for handle in stats.iter().copied() {
            set_control_font(handle, ui);
        }
    }
    if let Ok(combat) = COMBAT_HWNDS.lock() {
        for handle in combat.iter().copied() {
            set_control_font(handle, ui);
        }
    }
    unsafe { apply_list_item_height(dpi) };
}

fn set_text(hwnd: isize, text: &str) {
    if hwnd == 0 {
        return;
    }
    let _ = unsafe { SetWindowTextW(HWND(hwnd as *mut _), &HSTRING::from(text)) };
}

unsafe fn apply_locale_texts(hwnd: HWND) {
    let s = crate::i18n::t();
    let dpi = PANEL_DPI.load(Ordering::SeqCst).max(96);
    unsafe { recreate_ui_fonts(dpi) };
    set_text(hwnd.0 as isize, s.panel_title);
    if let Ok(labels) = STATIC_LABELS.lock() {
        for (id, handle) in labels.iter().copied() {
            let text = match id {
                ID_TITLE_LABEL => s.panel_title,
                ID_SECTION_BUILDS => s.section_builds,
                ID_SECTION_STATS => s.section_stats,
                ID_SECTION_COMBAT => s.section_combat,
                ID_SECTION_NAME => s.section_name,
                _ => continue,
            };
            set_text(handle, text);
        }
    }
    if let Ok(buttons) = BUTTON_HWNDS.lock() {
        for (id, handle) in buttons.iter().copied() {
            let text = match id {
                ID_SAVE => s.btn_new,
                ID_OVERWRITE => s.btn_overwrite,
                ID_LOAD => s.btn_load,
                ID_RENAME => s.btn_rename,
                ID_DELETE => s.btn_delete,
                ID_BIND_F1 => s.btn_bind_f1,
                ID_BIND_F2 => s.btn_bind_f2,
                ID_BIND_F3 => s.btn_bind_f3,
                ID_BIND_F4 => s.btn_bind_f4,
                ID_CLEAR_COMBAT => s.btn_clear_combat,
                ID_REFRESH => s.btn_refresh,
                ID_CLOSE => s.btn_close,
                ID_LANG => s.lang_button,
                ID_CARE => {
                    if crate::library::care_enabled() {
                        s.care_on
                    } else {
                        s.care_off
                    }
                }
                ID_CTRL_PICKER => {
                    if crate::library::ctrl_picker_enabled() {
                        s.ctrl_picker_on
                    } else {
                        s.ctrl_picker_off
                    }
                }
                _ => continue,
            };
            set_text(handle, text);
        }
    }
    set_text(LANG_HWND.load(Ordering::SeqCst), s.lang_button);
    unsafe {
        sync_stats_from_selection();
        sync_combat_from_selection();
    }
    if let Ok(mut shown) = SHOWN_REVISION.lock() {
        *shown = u64::MAX;
    }
    publish_library_state_from_ui(None);
}

unsafe fn ensure_class(instance: HINSTANCE) {
    if CLASS_READY.swap(true, Ordering::SeqCst) {
        return;
    }
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or(HCURSOR(std::ptr::null_mut()));
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: HICON(std::ptr::null_mut()),
        hCursor: cursor,
        hbrBackground: brush(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: CLASS_NAME,
    };
    unsafe {
        RegisterClassW(&class);
    }
}

unsafe fn ui_thread() {
    let instance = unsafe { GetModuleHandleW(None) }
        .map(|module| HINSTANCE(module.0))
        .unwrap_or(HINSTANCE(std::ptr::null_mut()));
    unsafe { ensure_class(instance) };

    let dpi = unsafe { GetDpiForSystem() }.max(96);
    PANEL_DPI.store(dpi, Ordering::SeqCst);
    let scale = |value: i32| value * dpi as i32 / 96;
    let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(CLIENT_W),
        bottom: scale(CLIENT_H),
    };
    let _ = unsafe { AdjustWindowRect(&mut rect, style, false) };
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let x = (unsafe { GetSystemMetrics(SM_CXSCREEN) } - width).max(0) / 2;
    let y = (unsafe { GetSystemMetrics(SM_CYSCREEN) } - height).max(0) / 2;

    let title = HSTRING::from(crate::i18n::t().panel_title);
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            CLASS_NAME,
            &title,
            style,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        )
    }) else {
        return;
    };

    PANEL_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    if let Ok(mut revision) = SHOWN_REVISION.lock() {
        *revision = u64::MAX;
    }
    unsafe { build_controls(hwnd, instance, dpi) };
    publish_library_state_from_ui(None);
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    let _ = unsafe { SetForegroundWindow(hwnd) };
    unsafe {
        SetTimer(Some(hwnd), TIMER_ID, TIMER_MS, None);
    }

    let mut message = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if got <= 0 {
            break;
        }
        let _ = unsafe { TranslateMessage(&message) };
        unsafe { DispatchMessageW(&message) };
    }

    for slot in [&UI_FONT, &TITLE_FONT] {
        let font = slot.swap(0, Ordering::SeqCst);
        if font != 0 {
            let _ = unsafe { DeleteObject(HGDIOBJ(font as *mut _)) };
        }
    }
}

unsafe fn build_controls(hwnd: HWND, instance: HINSTANCE, dpi: u32) {
    PANEL_DPI.store(dpi, Ordering::SeqCst);
    unsafe { recreate_ui_fonts(dpi) };
    let ui_font = HFONT(UI_FONT.load(Ordering::SeqCst) as *mut _);
    let title_font = HFONT(TITLE_FONT.load(Ordering::SeqCst) as *mut _);
    if let Ok(mut labels) = STATIC_LABELS.lock() {
        labels.clear();
    }
    if let Ok(mut buttons) = BUTTON_HWNDS.lock() {
        buttons.clear();
    }
    let scale = |value: i32| value * dpi as i32 / 96;
    let s = crate::i18n::t();

    let control = |class: PCWSTR,
                   text: &str,
                   style,
                   ex_style,
                   id: Option<i32>,
                   x: i32,
                   y: i32,
                   width: i32,
                   height: i32,
                   font: HFONT|
     -> Option<HWND> {
        let child = unsafe {
            CreateWindowExW(
                ex_style,
                class,
                &HSTRING::from(text),
                style,
                scale(x),
                scale(y),
                scale(width),
                scale(height),
                Some(hwnd),
                id.map(|value| {
                    windows::Win32::UI::WindowsAndMessaging::HMENU(value as isize as *mut _)
                }),
                Some(instance),
                None,
            )
        };
        if let Ok(child) = child {
            unsafe {
                SendMessageW(
                    child,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            Some(child)
        } else {
            None
        }
    };
    let remember_label = |id: i32, handle: Option<HWND>| {
        if let (Some(child), Ok(mut labels)) = (handle, STATIC_LABELS.lock()) {
            labels.push((id, child.0 as isize));
        }
    };
    let remember_button = |id: i32, handle: Option<HWND>| {
        if let (Some(child), Ok(mut buttons)) = (handle, BUTTON_HWNDS.lock()) {
            buttons.push((id, child.0 as isize));
        }
    };
    let label = |text: &str, id: Option<i32>, x, y, width, height, font| {
        let child = control(
            w!("STATIC"),
            text,
            WS_CHILD | WS_VISIBLE,
            Default::default(),
            id,
            x,
            y,
            width,
            height,
            font,
        );
        if let Some(label_id) = id {
            remember_label(label_id, child);
        }
        child
    };
    let button = |text: &str, id, x, y, width| {
        let child = control(
            w!("BUTTON"),
            text,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            Default::default(),
            Some(id),
            x,
            y,
            width,
            BTN_H,
            ui_font,
        );
        remember_button(id, child);
        child
    };

    label(
        s.panel_title,
        Some(ID_TITLE_LABEL),
        MARGIN,
        12,
        CONTENT_W - 170,
        28,
        title_font,
    );
    let lang = button(s.lang_button, ID_LANG, CLIENT_W - MARGIN - 160, 10, 160);
    LANG_HWND.store(
        lang.map(|value| value.0 as isize).unwrap_or(0),
        Ordering::SeqCst,
    );
    label(
        s.section_builds,
        Some(ID_SECTION_BUILDS),
        MARGIN,
        46,
        CONTENT_W,
        20,
        ui_font,
    );
    let list = control(
        w!("LISTBOX"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WS_BORDER
            | WS_HSCROLL
            | WS_VSCROLL
            | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
        WS_EX_CLIENTEDGE,
        Some(ID_LIST),
        MARGIN,
        68,
        CONTENT_W,
        LIST_H,
        ui_font,
    );
    LIST_HWND.store(
        list.map(|value| value.0 as isize).unwrap_or(0),
        Ordering::SeqCst,
    );
    unsafe { apply_list_item_height(dpi) };

    label(
        s.section_stats,
        Some(ID_SECTION_STATS),
        MARGIN,
        296,
        CONTENT_W,
        20,
        ui_font,
    );
    if let Ok(mut stats) = STAT_HWNDS.lock() {
        stats.clear();
        let card_w = (CONTENT_W - GAP * 2) / 3;
        for index in 0..9 {
            let row = index / 3;
            let column = index % 3;
            let card = control(
                w!("STATIC"),
                "",
                WS_CHILD | WS_VISIBLE | WS_BORDER,
                WS_EX_CLIENTEDGE,
                None,
                MARGIN + column * (card_w + GAP),
                318 + row * 46,
                card_w,
                40,
                ui_font,
            );
            stats.push(card.map(|value| value.0 as isize).unwrap_or(0));
        }
    }

    label(
        s.section_combat,
        Some(ID_SECTION_COMBAT),
        MARGIN,
        458,
        CONTENT_W,
        20,
        ui_font,
    );
    if let Ok(mut combat) = COMBAT_HWNDS.lock() {
        combat.clear();
        let card_w = (CONTENT_W - GAP * 2) / 3;
        for index in 0..3 {
            let card = control(
                w!("STATIC"),
                "",
                WS_CHILD | WS_VISIBLE | WS_BORDER,
                WS_EX_CLIENTEDGE,
                None,
                MARGIN + index * (card_w + GAP),
                480,
                card_w,
                40,
                ui_font,
            );
            combat.push(card.map(|value| value.0 as isize).unwrap_or(0));
        }
    }

    label(
        s.section_name,
        Some(ID_SECTION_NAME),
        MARGIN,
        532,
        54,
        24,
        ui_font,
    );
    let name = control(
        w!("EDIT"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
        WS_EX_CLIENTEDGE,
        Some(ID_NAME),
        MARGIN + 56,
        528,
        CONTENT_W - 56,
        30,
        ui_font,
    );
    NAME_HWND.store(
        name.map(|value| value.0 as isize).unwrap_or(0),
        Ordering::SeqCst,
    );

    let fifth = (CONTENT_W - GAP * 4) / 5;
    button(s.btn_new, ID_SAVE, MARGIN, 568, fifth);
    button(s.btn_overwrite, ID_OVERWRITE, MARGIN + fifth + GAP, 568, fifth);
    button(s.btn_load, ID_LOAD, MARGIN + (fifth + GAP) * 2, 568, fifth);
    button(s.btn_rename, ID_RENAME, MARGIN + (fifth + GAP) * 3, 568, fifth);
    button(s.btn_delete, ID_DELETE, MARGIN + (fifth + GAP) * 4, 568, fifth);

    let quarter = (CONTENT_W - GAP * 3) / 4;
    button(s.btn_bind_f1, ID_BIND_F1, MARGIN, 610, quarter);
    button(s.btn_bind_f2, ID_BIND_F2, MARGIN + quarter + GAP, 610, quarter);
    button(
        s.btn_bind_f3,
        ID_BIND_F3,
        MARGIN + (quarter + GAP) * 2,
        610,
        quarter,
    );
    button(
        s.btn_bind_f4,
        ID_BIND_F4,
        MARGIN + (quarter + GAP) * 3,
        610,
        quarter,
    );

    let bindings = label("", None, MARGIN, 650, CONTENT_W, 24, ui_font);
    BINDINGS_HWND.store(
        bindings.map(|value| value.0 as isize).unwrap_or(0),
        Ordering::SeqCst,
    );

    let care = button(s.care_off, ID_CARE, MARGIN, 680, 180);
    CARE_HWND.store(
        care.map(|value| value.0 as isize).unwrap_or(0),
        Ordering::SeqCst,
    );
    let ctrl = button(s.ctrl_picker_off, ID_CTRL_PICKER, MARGIN + 188, 680, 180);
    CTRL_PICKER_HWND.store(
        ctrl.map(|value| value.0 as isize).unwrap_or(0),
        Ordering::SeqCst,
    );
    button(s.btn_clear_combat, ID_CLEAR_COMBAT, MARGIN + 376, 680, 150);
    let footer_w = 110;
    button(
        s.btn_refresh,
        ID_REFRESH,
        CLIENT_W - MARGIN - footer_w * 2 - GAP,
        680,
        footer_w,
    );
    button(
        s.btn_close,
        ID_CLOSE,
        CLIENT_W - MARGIN - footer_w,
        680,
        footer_w,
    );
}

fn selected_entry() -> Option<BuildEntry> {
    let list = LIST_HWND.load(Ordering::SeqCst);
    if list == 0 {
        return None;
    }
    let selected = unsafe {
        SendMessageW(
            HWND(list as *mut _),
            LB_GETCURSEL,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        )
        .0
    };
    if selected < 0 {
        return None;
    }
    DISPLAYED
        .lock()
        .ok()
        .and_then(|entries| entries.get(selected as usize).cloned())
}

fn edit_text() -> String {
    let edit = NAME_HWND.load(Ordering::SeqCst);
    if edit == 0 {
        return String::new();
    }
    let hwnd = HWND(edit as *mut _);
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    let mut buffer = vec![0u16; length.max(0) as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

unsafe fn sync_name_from_selection() {
    let edit = NAME_HWND.load(Ordering::SeqCst);
    if edit != 0 {
        let text = selected_entry().map(|entry| entry.name).unwrap_or_default();
        let _ = unsafe { SetWindowTextW(HWND(edit as *mut _), &HSTRING::from(text)) };
    }
}

unsafe fn sync_stats_from_selection() {
    let s = crate::i18n::t();
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
    let values = selected_entry().and_then(|entry| entry.stats).map(|stats| {
        [
            format!("{}    {}", labels[0], stats.level),
            format!("{}    {}", labels[1], stats.vigor),
            format!("{}    {}", labels[2], stats.mind),
            format!("{}    {}", labels[3], stats.endurance),
            format!("{}    {}", labels[4], stats.strength),
            format!("{}    {}", labels[5], stats.dexterity),
            format!("{}    {}", labels[6], stats.intelligence),
            format!("{}    {}", labels[7], stats.faith),
            format!("{}    {}", labels[8], stats.arcane),
        ]
    });
    let empty = [
        format!("{}    -", labels[0]),
        format!("{}    -", labels[1]),
        format!("{}    -", labels[2]),
        format!("{}    -", labels[3]),
        format!("{}    -", labels[4]),
        format!("{}    -", labels[5]),
        format!("{}    -", labels[6]),
        format!("{}    -", labels[7]),
        format!("{}    -", labels[8]),
    ];
    let Ok(controls) = STAT_HWNDS.lock() else {
        return;
    };
    for (index, raw) in controls.iter().copied().enumerate() {
        if raw == 0 {
            continue;
        }
        let text = values
            .as_ref()
            .map(|items| items[index].as_str())
            .unwrap_or(empty[index].as_str());
        let _ = unsafe { SetWindowTextW(HWND(raw as *mut _), &HSTRING::from(text)) };
    }
}

unsafe fn sync_combat_from_selection() {
    let s = crate::i18n::t();
    let record = selected_entry().map(|entry| entry.combat);
    let texts = record
        .map(|record| {
            let total = record.kills.saturating_add(record.deaths);
            let win_rate = if total == 0 {
                0.0
            } else {
                record.kills as f64 * 100.0 / total as f64
            };
            [
                format!("{}    {}", s.combat_wins, record.kills),
                format!("{}    {}", s.combat_losses, record.deaths),
                format!("{}    {win_rate:.1}%", s.combat_winrate),
            ]
        })
        .unwrap_or_else(|| {
            [
                format!("{}    -", s.combat_wins),
                format!("{}    -", s.combat_losses),
                format!("{}    -", s.combat_winrate),
            ]
        });
    let Ok(controls) = COMBAT_HWNDS.lock() else {
        return;
    };
    for (index, raw) in controls.iter().copied().enumerate() {
        if raw != 0 {
            let _ = unsafe {
                SetWindowTextW(HWND(raw as *mut _), &HSTRING::from(texts[index].as_str()))
            };
        }
    }
}

unsafe fn sync_details_if_selection_changed() {
    let selected = selected_entry().map(|entry| entry.id);
    let Ok(mut previous) = LAST_DETAIL_ID.lock() else {
        return;
    };
    if *previous == selected {
        return;
    }
    *previous = selected.clone();
    drop(previous);
    if let Some(id) = selected {
        let _ = crate::library::set_active(&id);
    }
    unsafe { sync_name_from_selection() };
    unsafe { sync_stats_from_selection() };
    unsafe { sync_combat_from_selection() };
}

fn binding_text(view: &ViewState) -> String {
    let s = crate::i18n::t();
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
        format!("F{}：{name}", index + 1)
    };
    let mut text = format!(
        "{}    {}    {}    {}",
        describe(0),
        describe(1),
        describe(2),
        describe(3)
    );
    if view.busy {
        text.push_str("    ");
        text.push_str(s.syncing);
    }
    if let Some(error) = &view.error {
        text.push_str("    ");
        text.push_str(s.refresh_failed);
        text.push_str(error);
    }
    text
}

/// UI 定时器调用；只有 revision 变化时重建列表，避免打断用户选择和编辑。
unsafe fn refresh_ui() {
    let view = match VIEW.lock() {
        Ok(view) => view.clone(),
        Err(_) => return,
    };
    let mut shown = match SHOWN_REVISION.lock() {
        Ok(shown) => shown,
        Err(_) => return,
    };
    if *shown == view.revision {
        return;
    }
    *shown = view.revision;

    let list_raw = LIST_HWND.load(Ordering::SeqCst);
    let preferred = SELECT_AFTER_REFRESH
        .lock()
        .ok()
        .and_then(|mut selected| selected.take());
    let force_selection = preferred.is_some();
    let selected_id = preferred
        .or_else(|| selected_entry().map(|entry| entry.id))
        .or_else(|| view.active.clone());
    if list_raw != 0 {
        let list = HWND(list_raw as *mut _);
        unsafe {
            SendMessageW(list, LB_RESETCONTENT, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        let mut selected_index = None;
        for (index, entry) in view.entries.iter().enumerate() {
            let marker = (0..4)
                .filter(|binding| view.bindings[*binding].as_deref() == Some(entry.id.as_str()))
                .map(|binding| format!("F{}", binding + 1))
                .collect::<Vec<_>>()
                .join("/");
            let title = if marker.is_empty() {
                entry.name.clone()
            } else {
                format!("{}    [{}]", entry.name, marker)
            };
            let wide = HSTRING::from(title);
            unsafe {
                SendMessageW(
                    list,
                    LB_ADDSTRING,
                    Some(WPARAM(0)),
                    Some(LPARAM(wide.as_ptr() as isize)),
                );
            }
            if selected_id.as_deref() == Some(entry.id.as_str()) {
                selected_index = Some(index);
            }
        }
        if let Some(index) = selected_index {
            unsafe {
                SendMessageW(list, LB_SETCURSEL, Some(WPARAM(index)), Some(LPARAM(0)));
            }
        }
        unsafe {
            SendMessageW(
                list,
                LB_SETHORIZONTALEXTENT,
                Some(WPARAM(1500)),
                Some(LPARAM(0)),
            );
        }
    }
    if let Ok(mut displayed) = DISPLAYED.lock() {
        *displayed = view.entries.clone();
    }
    if let Some(entry) = selected_entry() {
        let _ = crate::library::set_active(&entry.id);
    }
    unsafe { sync_stats_from_selection() };
    unsafe { sync_combat_from_selection() };
    if force_selection || selected_id.is_none() {
        unsafe { sync_name_from_selection() };
    }

    let bindings = BINDINGS_HWND.load(Ordering::SeqCst);
    if bindings != 0 {
        let _ = unsafe {
            SetWindowTextW(
                HWND(bindings as *mut _),
                &HSTRING::from(binding_text(&view)),
            )
        };
    }
    let care = CARE_HWND.load(Ordering::SeqCst);
    if care != 0 {
        let s = crate::i18n::t();
        let text = if view.care { s.care_on } else { s.care_off };
        let _ = unsafe { SetWindowTextW(HWND(care as *mut _), &HSTRING::from(text)) };
    }
    let ctrl = CTRL_PICKER_HWND.load(Ordering::SeqCst);
    if ctrl != 0 {
        let s = crate::i18n::t();
        let text = if view.ctrl_picker {
            s.ctrl_picker_on
        } else {
            s.ctrl_picker_off
        };
        let _ = unsafe { SetWindowTextW(HWND(ctrl as *mut _), &HSTRING::from(text)) };
    }
    set_text(LANG_HWND.load(Ordering::SeqCst), crate::i18n::t().lang_button);
}

unsafe fn on_command(hwnd: HWND, id: i32, notification: u16) {
    if id == ID_LIST && notification == LBN_SELCHANGE as u16 {
        unsafe { sync_details_if_selection_changed() };
        return;
    }
    if notification != BN_CLICKED as u16 {
        return;
    }

    let s = crate::i18n::t();
    match id {
        ID_SAVE => {
            let snapshot = PANEL_SNAPSHOT.lock().ok().and_then(|cached| cached.clone());
            let result = snapshot
                .ok_or_else(|| s.capture_failed.to_string())
                .and_then(|snapshot| crate::library::create(&snapshot));
            match result {
                Ok(entry) => {
                    if let Ok(mut selected) = SELECT_AFTER_REFRESH.lock() {
                        *selected = Some(entry.id);
                    }
                    run_library_action(Ok(()), s.created_selected);
                }
                Err(error) => run_library_action(Err(error), s.created_selected),
            }
        }
        ID_OVERWRITE => {
            if let Some(entry) = selected_entry() {
                let snapshot = PANEL_SNAPSHOT.lock().ok().and_then(|cached| cached.clone());
                let result = snapshot
                    .ok_or_else(|| s.capture_failed.to_string())
                    .and_then(|snapshot| crate::library::overwrite(&entry.id, &snapshot));
                match result {
                    Ok(entry) => {
                        if let Ok(mut selected) = SELECT_AFTER_REFRESH.lock() {
                            *selected = Some(entry.id);
                        }
                        run_library_action(Ok(()), s.overwritten_selected);
                    }
                    Err(error) => run_library_action(Err(error), s.overwritten_selected),
                }
            } else {
                push_notice(s.no_selection.to_owned());
            }
        }
        ID_LOAD => {
            if let Some(entry) = selected_entry() {
                push_action(Action::Load(entry.id));
                // 同步必须回到游戏帧线程执行；关闭面板后游戏立即恢复处理。
                let _ = unsafe { DestroyWindow(hwnd) };
            } else {
                push_notice(s.no_selection.to_owned());
            }
        }
        ID_RENAME => {
            if let Some(entry) = selected_entry() {
                let name = edit_text();
                if !name.trim().is_empty() {
                    run_library_action(
                        crate::library::rename(&entry.id, name.trim()),
                        s.renamed,
                    );
                } else {
                    push_notice(s.name_empty.to_owned());
                }
            } else {
                push_notice(s.no_selection.to_owned());
            }
        }
        ID_DELETE => {
            if let Some(entry) = selected_entry() {
                let body = HSTRING::from(crate::i18n::fmt(s.confirm_delete, [&entry.name]));
                let answer = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                        Some(hwnd),
                        &body,
                        w!("erdueltools"),
                        MB_YESNO | MB_ICONWARNING,
                    )
                };
                if answer == IDYES {
                    run_library_action(crate::library::delete(&entry.id), s.deleted);
                }
            } else {
                push_notice(s.no_selection.to_owned());
            }
        }
        ID_BIND_F1 | ID_BIND_F2 | ID_BIND_F3 | ID_BIND_F4 => {
            if let Some(entry) = selected_entry() {
                run_library_action(
                    crate::library::bind((id - ID_BIND_F1) as usize, Some(&entry.id)),
                    &crate::i18n::fmt(s.bound_fn, [id - ID_BIND_F1 + 1]),
                );
            } else {
                push_notice(s.no_selection.to_owned());
            }
        }
        ID_CARE => {
            let enabled = !crate::library::care_enabled();
            run_library_action(
                crate::library::set_care_enabled(enabled),
                if enabled {
                    s.care_enabled_msg
                } else {
                    s.care_disabled_msg
                },
            );
        }
        ID_CTRL_PICKER => {
            let enabled = !crate::library::ctrl_picker_enabled();
            if !enabled {
                crate::native_ui::picker_cancel();
            }
            run_library_action(
                crate::library::set_ctrl_picker_enabled(enabled),
                if enabled {
                    s.ctrl_picker_enabled_msg
                } else {
                    s.ctrl_picker_disabled_msg
                },
            );
        }
        ID_LANG => match crate::library::cycle_language() {
            Ok(_) => unsafe { apply_locale_texts(hwnd) },
            Err(error) => push_notice(crate::i18n::fmt(crate::i18n::t().op_failed, [&error])),
        },
        ID_CLEAR_COMBAT => {
            if let Some(entry) = selected_entry() {
                let body = HSTRING::from(crate::i18n::fmt(s.confirm_clear_combat, [&entry.name]));
                let answer = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                        Some(hwnd),
                        &body,
                        w!("erdueltools"),
                        MB_YESNO | MB_ICONWARNING,
                    )
                };
                if answer == IDYES {
                    run_library_action(crate::library::clear_combat(&entry.id), s.combat_cleared);
                }
            } else {
                push_notice(s.no_selection.to_owned());
            }
        }
        ID_REFRESH => {
            publish_library_state_from_ui(None);
            push_notice(s.list_refreshed.to_owned());
        }
        ID_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        _ => {}
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            let notification = ((wparam.0 >> 16) & 0xffff) as u16;
            unsafe { on_command(hwnd, id, notification) };
            LRESULT(0)
        }
        WM_TIMER => {
            unsafe { refresh_ui() };
            unsafe { sync_details_if_selection_changed() };
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let dc = HDC(wparam.0 as *mut _);
            unsafe {
                SetBkColor(dc, BG);
                SetTextColor(dc, FG);
            }
            LRESULT(brush().0 as isize)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            PANEL_HWND.store(0, Ordering::SeqCst);
            LIST_HWND.store(0, Ordering::SeqCst);
            NAME_HWND.store(0, Ordering::SeqCst);
            BINDINGS_HWND.store(0, Ordering::SeqCst);
            CARE_HWND.store(0, Ordering::SeqCst);
            CTRL_PICKER_HWND.store(0, Ordering::SeqCst);
            if let Ok(mut displayed) = DISPLAYED.lock() {
                displayed.clear();
            }
            if let Ok(mut selected) = LAST_DETAIL_ID.lock() {
                *selected = None;
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
