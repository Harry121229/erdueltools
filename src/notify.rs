//! 启动用 MessageBox；游戏内一律系统滚动广播（ServerMessage）

use std::sync::OnceLock;

use eldenring::cs::{CSMenuManImp, MenuCommonParam, SoloParamRepository};
use fromsoftware_shared::FromStatic;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
};
use windows::core::HSTRING;

const SERVER_MESSAGE_RVA: usize = 0x841_b60;
const SERVER_MESSAGE_AOB: [u8; 20] = [
    0x48, 0x89, 0x54, 0x24, 0x10, 0x48, 0x83, 0xEC, 0x28, 0x48, 0x8B, 0x41, 0x38, 0x48, 0xFF, 0xC0,
    0x48, 0x83, 0xF8, 0x0A,
];
const SYSTEM_ANNOUNCE_VM_OFFSET: usize = 0x860;
/// ServerMessage 入口首先递增此字段并以 10 为上限；发送前清零即可丢弃旧队列。
const ANNOUNCE_QUEUE_COUNT_OFFSET: usize = 0x38;
/// 短提示停留时间
const ANNOUNCE_DISPLAY_SECS: f32 = 0.2;
const LONG_ANNOUNCE_DISPLAY_SECS: f32 = 4.0;

type ServerMessageFn = unsafe extern "C" fn(view_model: usize, message: *const u16) -> u8;

static SERVER_MSG: OnceLock<Option<ServerMessageFn>> = OnceLock::new();

/// 仅启动时用：挡操作的弹窗
pub fn toast(text: &str) {
    let text = text.to_owned();
    std::thread::spawn(move || {
        let body = HSTRING::from(text.as_str());
        let title = HSTRING::from("erdueltools");
        unsafe {
            let _ = MessageBoxW(
                None,
                &body,
                &title,
                MB_OK | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND,
            );
        }
    });
}

fn resolve_server_message() -> Option<ServerMessageFn> {
    *SERVER_MSG.get_or_init(|| {
        let module = unsafe { GetModuleHandleW(None) }.ok()?;
        let addr = (module.0 as usize).wrapping_add(SERVER_MESSAGE_RVA);
        let bytes =
            unsafe { std::slice::from_raw_parts(addr as *const u8, SERVER_MESSAGE_AOB.len()) };
        if bytes != SERVER_MESSAGE_AOB {
            return None;
        }
        Some(unsafe { std::mem::transmute::<usize, ServerMessageFn>(addr) })
    })
}

fn patch_announce_duration(seconds: f32) {
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let Some(row) = repo.get_mut::<MenuCommonParam>(0) else {
        return;
    };
    row.set_system_announce_no_scroll_wait_time(seconds);
    row.set_system_announce_scroll_buffer_time(seconds);
    row.set_system_announce_scroll_count(1);
}

/// 游戏内系统滚动字（不挡操作），约 0.5s 消失
pub fn say(text: &str) {
    say_with_duration(text, ANNOUNCE_DISPLAY_SECS);
}

/// 需要阅读数值的长公告，约 4 秒消失。
pub fn say_long(text: &str) {
    say_with_duration(text, LONG_ANNOUNCE_DISPLAY_SECS);
}

fn say_with_duration(text: &str, seconds: f32) {
    patch_announce_duration(seconds);
    let Some(func) = resolve_server_message() else {
        return;
    };
    let Ok(menu) = (unsafe { CSMenuManImp::instance() }) else {
        return;
    };
    let vm = unsafe {
        let base = menu as *const CSMenuManImp as *const u8;
        *(base.add(SYSTEM_ANNOUNCE_VM_OFFSET) as *const usize)
    };
    if vm == 0 {
        return;
    }
    // 每条新广播都替换等待中的旧广播，不让快速连续提示排成队列。
    unsafe {
        *((vm + ANNOUNCE_QUEUE_COUNT_OFFSET) as *mut usize) = 0;
    }
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let flat = if flat.chars().count() > 96 {
        format!("{}…", flat.chars().take(95).collect::<String>())
    } else {
        flat
    };
    let mut wide: Vec<u16> = flat.encode_utf16().collect();
    wide.push(0);
    let _ = unsafe { func(vm, wide.as_ptr()) };
}
