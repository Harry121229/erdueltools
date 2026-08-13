//! F1–F4 加载绑定；按住 Ctrl 左右移鼠标选 BD；F7 覆盖；Shift+F7 新建；F5 面板；F6 战绩条；F8 清目标；F9 只清物品。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7,
    VK_F8, VK_F9, VK_SHIFT,
};

use crate::{combat, i18n, library, native_ui, notify, panel, paths, snap, sync};

static PREV: AtomicU32 = AtomicU32::new(0);
static CTRL_PICKER_HELD: AtomicBool = AtomicBool::new(false);

fn down(key: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(key.0 as i32) as u16 & 0x8000) != 0 }
}

fn edge(key: VIRTUAL_KEY, bit: u32) -> bool {
    let now = down(key);
    let prev = PREV.load(Ordering::Relaxed);
    let was = prev & bit != 0;
    PREV.store(
        if now { prev | bit } else { prev & !bit },
        Ordering::Relaxed,
    );
    now && !was
}

pub fn poll() {
    poll_ctrl_picker();
    if CTRL_PICKER_HELD.load(Ordering::SeqCst) {
        return;
    }

    if edge(VK_F5, 1 << 4) {
        if !native_ui::toggle_panel() {
            panel::toggle_async();
        }
        return;
    }
    if edge(VK_F6, 1 << 5) {
        let on = native_ui::toggle_scoreboard();
        notify::say(if on {
            i18n::t().scoreboard_on
        } else {
            i18n::t().scoreboard_off
        });
        return;
    }
    if edge(VK_F8, 1 << 7) {
        combat::clear_tracked_target();
        notify::say(i18n::t().target_cleared);
        return;
    }
    if !snap::game_ready() {
        return;
    }
    if edge(VK_F9, 1 << 8) {
        match sync::start_purge_only() {
            Ok(message) => notify::say(&message),
            Err(error) => notify::say(&i18n::fmt(i18n::t().purge_failed, [&error])),
        }
        return;
    }
    if edge(VK_F7, 1 << 3) {
        if down(VK_SHIFT) {
            save_new();
        } else {
            save_current();
        }
        return;
    }
    for (index, (key, bit)) in [
        (VK_F1, 1 << 0),
        (VK_F2, 1 << 1),
        (VK_F3, 1 << 2),
        (VK_F4, 1 << 6),
    ]
    .into_iter()
    .enumerate()
    {
        if edge(key, bit) {
            load_binding(index);
            return;
        }
    }
}

fn poll_ctrl_picker() {
    if !library::ctrl_picker_enabled() {
        if CTRL_PICKER_HELD.swap(false, Ordering::SeqCst) {
            native_ui::picker_cancel();
        }
        return;
    }
    let held = down(VK_CONTROL);
    let was = CTRL_PICKER_HELD.load(Ordering::SeqCst);
    if held && !was {
        CTRL_PICKER_HELD.store(true, Ordering::SeqCst);
        if snap::game_ready() {
            let _ = native_ui::picker_begin();
        }
        return;
    }
    if held && was {
        native_ui::picker_tick();
        return;
    }
    if !held && was {
        CTRL_PICKER_HELD.store(false, Ordering::SeqCst);
        if let Some(id) = native_ui::picker_end() {
            load_id(&id);
        }
    }
}

fn save_new() {
    paths::stage("save_new");
    match snap::capture().and_then(|snapshot| library::create(&snapshot)) {
        Ok(entry) => notify::say(&i18n::fmt(i18n::t().created_build, [&entry.name])),
        Err(error) => notify::say(&i18n::fmt(i18n::t().save_failed, [&error])),
    }
}

fn save_current() {
    let Some(id) = library::active() else {
        notify::say(i18n::t().no_active_overwrite);
        return;
    };
    let result = snap::capture().and_then(|snapshot| {
        paths::stage("save_overwrite");
        library::overwrite(&id, &snapshot)
            .map(|entry| i18n::fmt(i18n::t().overwritten_build, [&entry.name]))
    });
    match result {
        Ok(message) => notify::say(&message),
        Err(error) => notify::say(&i18n::fmt(i18n::t().save_failed, [&error])),
    }
}

fn load_binding(index: usize) {
    let Some(id) = library::binding(index) else {
        notify::say(&i18n::fmt(i18n::t().binding_empty, [index + 1]));
        return;
    };
    paths::stage(&format!("load_binding_{}", index + 1));
    load_id(&id);
}

fn load_id(id: &str) {
    if sync::is_busy() {
        notify::say(i18n::t().sync_busy);
        return;
    }
    match library::load(id) {
        Ok(snapshot) => {
            if let Err(error) = sync::start(snapshot) {
                notify::say(&i18n::fmt(i18n::t().sync_failed, [&error]));
            } else {
                let _ = library::set_active(id);
                panel::select_build(id);
            }
        }
        Err(error) => notify::say(&i18n::fmt(i18n::t().read_failed, [&error])),
    }
}
