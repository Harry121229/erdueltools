//! F1/F2/F3/F4 = 加载绑定；F7 = 覆盖；Shift+F7 = 新建；F5 = 面板；F6 = 战绩；F8 = 清空击杀目标；F9 = 只清物品。

use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9,
    VK_SHIFT,
};

use crate::{combat, i18n, library, notify, panel, paths, snap, sync};

static PREV: AtomicU32 = AtomicU32::new(0);

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
    if edge(VK_F5, 1 << 4) {
        panel::toggle_async();
        return;
    }
    if edge(VK_F6, 1 << 5) {
        show_combat();
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

fn show_combat() {
    let Some((name, record)) = library::active_combat() else {
        notify::say_long(i18n::t().no_active_combat);
        return;
    };
    let total = record.kills.saturating_add(record.deaths);
    let win_rate = if total == 0 {
        0.0
    } else {
        record.kills as f64 * 100.0 / total as f64
    };
    notify::say_long(&i18n::fmt(
        i18n::t().combat_detail,
        [
            name,
            record.kills.to_string(),
            record.deaths.to_string(),
            format!("{win_rate:.1}"),
        ],
    ));
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
    if sync::is_busy() {
        return;
    }
    let Some(id) = library::binding(index) else {
        notify::say(&i18n::fmt(i18n::t().binding_empty, [index + 1]));
        return;
    };
    paths::stage(&format!("load_binding_{}", index + 1));
    match library::load(&id) {
        Ok(snapshot) => {
            if let Err(error) = sync::start(snapshot) {
                notify::say(&i18n::fmt(i18n::t().sync_failed, [&error]));
            } else {
                let _ = library::set_active(&id);
                panel::select_build(&id);
            }
        }
        Err(error) => notify::say(&i18n::fmt(i18n::t().read_failed, [&error])),
    }
}
