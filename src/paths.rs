//! 存档 / 日志目录：跟随 erdueltools.dll，不依赖游戏工作目录

use std::path::PathBuf;
use std::sync::OnceLock;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::core::PCWSTR;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

fn dll_dir() -> PathBuf {
    unsafe {
        let mut module = HMODULE::default();
        let ok = GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(dll_dir as *const () as *const u16),
            &mut module,
        );
        if ok.is_ok() {
            let mut buf = [0u16; 520];
            let n = GetModuleFileNameW(Some(module), &mut buf);
            if n > 0 {
                let path = String::from_utf16_lossy(&buf[..n as usize]);
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    return parent.to_path_buf();
                }
            }
        }
    }
    PathBuf::from("mod")
}

pub fn data_dir() -> &'static PathBuf {
    DATA_DIR.get_or_init(|| {
        let dir = dll_dir().join("erdueltools_bds");
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

pub fn stage(msg: &str) {
    let _ = std::fs::write(data_dir().join("last_stage.txt"), msg);
}

pub fn reset_network_trace() {
    let _ = std::fs::write(data_dir().join("network_trace.txt"), "");
}

pub fn append_network_trace(line: &str) {
    use std::io::Write;
    let path = data_dir().join("network_trace.txt");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}
