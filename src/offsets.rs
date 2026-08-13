//! 函数地址解析。
//!
//! 都已对磁盘 eldenring.exe 做过静态扫描，确认**全模块唯一命中**：
//!   ItemGive    `8b 02 83 f8 0a` −0x52 → RVA 0x5605b0
//!   RemoveItem  `?? 83 ec ?? 8b f2 ?? 8b e9 ?? 85 c0 74` −0x10 → RVA 0x24d1e0
//!   EquipGear   `?? 8b f1 ?? 8b d8 ?? 63 ea ?? 8b f9` −0x17 → RVA 0x249160
//!
//! CE 用 AOBScanModuleUnique；命中数 ≠ 1 时一律拒绝。

use std::sync::OnceLock;

use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::core::w;

static MODULE: OnceLock<Option<(usize, usize)>> = OnceLock::new();

pub(crate) fn module_image() -> Option<(usize, usize)> {
    *MODULE.get_or_init(|| {
        let module = unsafe {
            GetModuleHandleW(w!("eldenring.exe"))
                .ok()
                .or_else(|| GetModuleHandleW(None).ok())
        }?;
        let base = module.0 as usize;
        unsafe {
            if *(base as *const u16) != 0x5A4D {
                return None;
            }
            let e_lfanew = *((base + 0x3C) as *const i32) as usize;
            let nt = base + e_lfanew;
            if *(nt as *const u32) != 0x0000_4550 {
                return None;
            }
            let size = *((nt + 0x50) as *const u32) as usize;
            Some((base, size))
        }
    })
}

pub fn module_base() -> Option<usize> {
    module_image().map(|(base, _)| base)
}

fn find_aob(base: usize, size: usize, needle: &[u8]) -> Option<usize> {
    let hay = unsafe { std::slice::from_raw_parts(base as *const u8, size) };
    hay.windows(needle.len())
        .position(|w| w == needle)
        .map(|off| base + off)
}

fn find_aob_unique(base: usize, size: usize, needle: &[u8], mask: &[u8]) -> Option<usize> {
    debug_assert_eq!(needle.len(), mask.len());
    let hay = unsafe { std::slice::from_raw_parts(base as *const u8, size) };
    let n = needle.len();
    if hay.len() < n {
        return None;
    }
    let mut found = None;
    for off in 0..=(hay.len() - n) {
        let window = &hay[off..off + n];
        if window
            .iter()
            .zip(needle.iter().zip(mask.iter()))
            .all(|(got, (want, m))| *m == 0 || got == want)
        {
            if found.is_some() {
                return None;
            }
            found = Some(base + off);
        }
    }
    found
}

pub fn read_bytes(addr: usize, len: usize) -> Vec<u8> {
    let (base, size) = match module_image() {
        Some(v) => v,
        None => return Vec::new(),
    };
    if addr < base || addr + len > base + size {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(addr as *const u8, len) }.to_vec()
}

pub fn to_rva(addr: usize) -> usize {
    module_image()
        .map(|(b, _)| addr.wrapping_sub(b))
        .unwrap_or(0)
}

pub fn resolve_item_give() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0x560_5b0;
    const INNER: [u8; 5] = [0x8B, 0x02, 0x83, 0xF8, 0x0A];
    const INNER_OFF: usize = 0x52;

    let probe = base + RVA + INNER_OFF;
    if probe + INNER.len() < base + size
        && unsafe { std::slice::from_raw_parts(probe as *const u8, INNER.len()) } == INNER
    {
        return Some(base + RVA);
    }
    let hit = find_aob(base, size, &INNER)?;
    Some(hit.checked_sub(INNER_OFF)?)
}

pub fn resolve_remove_item() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0x24_d1e0;
    const PROLOGUE: [u8; 16] = [
        0x48, 0x89, 0x5C, 0x24, 0x20, 0x89, 0x54, 0x24, 0x10, 0x55, 0x56, 0x57, 0x41, 0x56, 0x41,
        0x57,
    ];

    let by_rva = base + RVA;
    if by_rva + PROLOGUE.len() < base + size
        && unsafe { std::slice::from_raw_parts(by_rva as *const u8, PROLOGUE.len()) } == PROLOGUE
    {
        return Some(by_rva);
    }

    const NEEDLE: [u8; 13] = [
        0x00, 0x83, 0xEC, 0x00, 0x8B, 0xF2, 0x00, 0x8B, 0xE9, 0x00, 0x85, 0xC0, 0x74,
    ];
    const MASK: [u8; 13] = [
        0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF, 0xFF,
    ];
    let hit = find_aob_unique(base, size, &NEEDLE, &MASK)?;
    let func = hit.checked_sub(0x10)?;
    if unsafe { std::slice::from_raw_parts(func as *const u8, PROLOGUE.len()) } != PROLOGUE {
        return None;
    }
    Some(func)
}

/// TGA `ItemPopup`：物品获得提示入队。
/// AOB `48 8b fa 48 8b d9 48 8b 81 a8 00 00 00` − 0x14 → RVA 0x5640f0
pub fn resolve_item_popup() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0x564_0f0;
    const INNER: [u8; 13] = [
        0x48, 0x8B, 0xFA, 0x48, 0x8B, 0xD9, 0x48, 0x8B, 0x81, 0xA8, 0x00, 0x00, 0x00,
    ];
    const INNER_OFF: usize = 0x14;
    const PROLOGUE: [u8; 4] = [0x40, 0x57, 0x48, 0x83];

    let by_rva = base + RVA;
    let probe = by_rva + INNER_OFF;
    if probe + INNER.len() < base + size
        && unsafe { std::slice::from_raw_parts(probe as *const u8, INNER.len()) } == INNER
        && unsafe { std::slice::from_raw_parts(by_rva as *const u8, PROLOGUE.len()) } == PROLOGUE
    {
        return Some(by_rva);
    }

    let hit = find_aob(base, size, &INNER)?;
    let func = hit.checked_sub(INNER_OFF)?;
    if unsafe { std::slice::from_raw_parts(func as *const u8, PROLOGUE.len()) } != PROLOGUE {
        return None;
    }
    Some(func)
}

/// ItemGive 内部第二条获得 UI 路径（写 FE / 弹窗），RVA 0x55ac70。
/// 只 NOP ItemPopup 时这条仍会刷提示。
pub fn resolve_item_get_ui() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0x55a_c70;
    const PROLOGUE: [u8; 8] = [0x48, 0x8B, 0xC4, 0x55, 0x56, 0x57, 0x41, 0x54];

    let by_rva = base + RVA;
    if by_rva + PROLOGUE.len() < base + size
        && unsafe { std::slice::from_raw_parts(by_rva as *const u8, PROLOGUE.len()) } == PROLOGUE
    {
        return Some(by_rva);
    }
    None
}

/// TGA `equipItem` → EquipGear
/// AOB `?? 8b f1 ?? 8b d8 ?? 63 ea ?? 8b f9` − 0x17 → RVA 0x249160
pub fn resolve_equip_gear() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0x249_160;
    /// `push rbx,rbp,rsi,rdi,r14` / `sub rsp,90h`
    const PROLOGUE: [u8; 16] = [
        0x40, 0x53, 0x55, 0x56, 0x57, 0x41, 0x56, 0x48, 0x81, 0xEC, 0x90, 0x00, 0x00, 0x00, 0x48,
        0xC7,
    ];

    let by_rva = base + RVA;
    if by_rva + PROLOGUE.len() < base + size
        && unsafe { std::slice::from_raw_parts(by_rva as *const u8, PROLOGUE.len()) } == PROLOGUE
    {
        return Some(by_rva);
    }

    const NEEDLE: [u8; 12] = [
        0x00, 0x8B, 0xF1, 0x00, 0x8B, 0xD8, 0x00, 0x63, 0xEA, 0x00, 0x8B, 0xF9,
    ];
    const MASK: [u8; 12] = [
        0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0xFF, 0xFF,
    ];
    let hit = find_aob_unique(base, size, &NEEDLE, &MASK)?;
    let func = hit.checked_sub(0x17)?;
    if unsafe { std::slice::from_raw_parts(func as *const u8, PROLOGUE.len()) } != PROLOGUE {
        return None;
    }
    Some(func)
}

/// 会话发送入口：用于发送并记录原生装备快照包。
pub fn resolve_broadcast() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0xcaf_620;
    const AOB: [u8; 31] = [
        0x48, 0x89, 0x5C, 0x24, 0x08, 0x48, 0x89, 0x6C, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18,
        0x57, 0x41, 0x56, 0x41, 0x57, 0x48, 0x83, 0xEC, 0x30, 0x48, 0x8B, 0xB9, 0x80, 0x00, 0x00,
        0x00,
    ];
    let addr = base + RVA;
    if addr + AOB.len() < base + size
        && unsafe { std::slice::from_raw_parts(addr as *const u8, AOB.len()) } == AOB
    {
        Some(addr)
    } else {
        find_aob(base, size, &AOB)
    }
}

/// 会话收包队列入口；用于只读记录原生死亡相关包。
pub fn resolve_try_dequeue() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0xcb4_9f0;
    const AOB: [u8; 26] = [
        0x48, 0x89, 0x6C, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18, 0x48, 0x89, 0x7C, 0x24, 0x20,
        0x41, 0x56, 0x48, 0x83, 0xEC, 0x40, 0x80, 0x7C, 0x24, 0x78, 0x00,
    ];
    let addr = base + RVA;
    if addr + AOB.len() < base + size
        && unsafe { std::slice::from_raw_parts(addr as *const u8, AOB.len()) } == AOB
    {
        Some(addr)
    } else {
        find_aob(base, size, &AOB)
    }
}

fn resolve_packet24_owner_outer(rva: usize, prologue: &[u8]) -> Option<usize> {
    let (base, size) = module_image()?;
    let addr = base + rva;
    if addr + prologue.len() < base + size
        && unsafe { std::slice::from_raw_parts(addr as *const u8, prologue.len()) } == prologue
    {
        Some(addr)
    } else {
        find_aob_unique(base, size, prologue, &vec![0xFF; prologue.len()])
    }
}

/// packet 24 循环消费外层；`this + 0xA8` 是正在应用状态的 ChrIns。
pub fn resolve_packet24_owner_outer_loop() -> Option<usize> {
    const PROLOGUE: [u8; 32] = [
        0x48, 0x8B, 0xC4, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x48, 0x83, 0xEC,
        0x70, 0x48, 0xC7, 0x40, 0xA0, 0xFE, 0xFF, 0xFF, 0xFF, 0x48, 0x89, 0x58, 0x10, 0x48, 0x89,
        0x68, 0x18,
    ];
    resolve_packet24_owner_outer(0x3d_6930, &PROLOGUE)
}

/// packet 24 单次消费外层；`this + 0xA8` 是正在应用状态的 ChrIns。
pub fn resolve_packet24_owner_outer_once() -> Option<usize> {
    const PROLOGUE: [u8; 32] = [
        0x48, 0x8B, 0xC4, 0x56, 0x57, 0x41, 0x54, 0x41, 0x56, 0x41, 0x57, 0x48, 0x83, 0xEC, 0x70,
        0x48, 0xC7, 0x40, 0xA8, 0xFE, 0xFF, 0xFF, 0xFF, 0x48, 0x89, 0x58, 0x10, 0x48, 0x89, 0x68,
        0x18, 0x48,
    ];
    resolve_packet24_owner_outer(0x3d_6db0, &PROLOGUE)
}

/// Esc 暂停菜单「讯息 / Messages」：打开 `BloodMessageTopDialog` 的包装函数。
///
/// RVA `0x898D00`。全模块唯一内点 `b9 68 15 00 00`（分配 0x1568）在 +0x3D。
pub fn resolve_open_blood_message_top() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0x898_d00;
    const PROLOGUE: [u8; 14] = [
        0x48, 0x8B, 0xC4, 0x56, 0x57, 0x41, 0x56, 0x48, 0x81, 0xEC, 0xA0, 0x00, 0x00, 0x00,
    ];
    const INNER: [u8; 5] = [0xB9, 0x68, 0x15, 0x00, 0x00];
    const INNER_OFF: usize = 0x3D;

    let by_rva = base + RVA;
    if by_rva + INNER_OFF + INNER.len() <= base + size
        && unsafe { std::slice::from_raw_parts(by_rva as *const u8, PROLOGUE.len()) } == PROLOGUE
        && unsafe { std::slice::from_raw_parts((by_rva + INNER_OFF) as *const u8, INNER.len()) }
            == INNER
    {
        return Some(by_rva);
    }

    let hit = find_aob_unique(base, size, &INNER, &[0xFF; 5])?;
    let func = hit.checked_sub(INNER_OFF)?;
    if unsafe { std::slice::from_raw_parts(func as *const u8, PROLOGUE.len()) } != PROLOGUE {
        return None;
    }
    Some(func)
}

/// `CSMenuManImp` 打开暂停菜单（创建 `CSPopupMenu`，等价按 Esc）。
///
/// RVA `0x7660F0`。唯一内点 `48 83 bb 80 00 00 00 00 75 4c`（cmp popup_menu）在 +0x80。
pub fn resolve_open_pause() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0x766_0f0;
    const PROLOGUE: [u8; 6] = [0x40, 0x57, 0x48, 0x83, 0xEC, 0x30];
    const INNER: [u8; 10] = [
        0x48, 0x83, 0xBB, 0x80, 0x00, 0x00, 0x00, 0x00, 0x75, 0x4C,
    ];
    const INNER_OFF: usize = 0x80;

    let by_rva = base + RVA;
    if by_rva + INNER_OFF + INNER.len() <= base + size
        && unsafe { std::slice::from_raw_parts(by_rva as *const u8, PROLOGUE.len()) } == PROLOGUE
        && unsafe { std::slice::from_raw_parts((by_rva + INNER_OFF) as *const u8, INNER.len()) }
            == INNER
    {
        return Some(by_rva);
    }

    let hit = find_aob_unique(base, size, &INNER, &[0xFF; 10])?;
    let func = hit.checked_sub(INNER_OFF)?;
    if unsafe { std::slice::from_raw_parts(func as *const u8, PROLOGUE.len()) } != PROLOGUE {
        return None;
    }
    Some(func)
}

/// 菜单装备路径使用的原生“构造并广播当前装备快照”函数。
/// 调用栈实测：`0x658C90 -> 0xCA11C0 -> BroadCast(type 12)`。
pub fn resolve_send_equipment_snapshot() -> Option<usize> {
    let (base, size) = module_image()?;
    const RVA: usize = 0xca1_1c0;
    const PROLOGUE: [u8; 18] = [
        0x40, 0x53, 0x48, 0x81, 0xEC, 0x00, 0x01, 0x00, 0x00, 0x48, 0xC7, 0x44, 0x24, 0x20, 0xFE,
        0xFF, 0xFF, 0xFF,
    ];
    let addr = base + RVA;
    if addr + PROLOGUE.len() < base + size
        && unsafe { std::slice::from_raw_parts(addr as *const u8, PROLOGUE.len()) } == PROLOGUE
    {
        Some(addr)
    } else {
        find_aob(base, size, &PROLOGUE)
    }
}
