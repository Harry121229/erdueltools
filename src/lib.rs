//! erdueltools — 决斗 BD 同步
//!
//! 保存 / 还原：物品栏（武器护甲护符）+ 14 格穿戴 + 法术轮盘 + 加点出身。
//! 清栏自研；发放与穿戴与加点均按 TGA CE。

mod combat;
mod equip;
mod game;
mod hotkeys;
mod i18n;
mod library;
mod magic;
mod native_ui;
mod net_appear;
mod notify;
mod offsets;
mod panel;
mod paths;
mod snap;
mod stats;
mod sync;
mod tga;

use std::time::Duration;

use eldenring::cs::{CSTaskGroupIndex, CSTaskImp};
use eldenring::fd4::FD4TaskData;
use fromsoftware_shared::SharedTaskImpExt;

/// # Safety
/// 由 Windows 加载器调用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn DllMain(hmodule: usize, reason: u32) -> bool {
    if reason != 1 {
        return true;
    }

    std::thread::spawn(move || {
        if let Err(error) = library::init() {
            paths::stage(&format!("library_init_failed_{error}"));
        }
        let Ok(cs_task) = CSTaskImp::wait_for_instance(Duration::MAX) else {
            return;
        };

        native_ui::install_overlay(hmodule);
        net_appear::install_network_trace_hook();
        combat::install_death_hook();

        cs_task.run_recurring(
            |_: &FD4TaskData| {
                combat::poll();
                sync::poll();
                panel::poll();
                native_ui::poll();
                hotkeys::poll();
            },
            CSTaskGroupIndex::FrameBegin,
        );

        notify::toast(i18n::t().help);
    });

    true
}
