//! 发物品 / 删物品 + 从背包里查「真实 gaitem 句柄」

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use eldenring::cs::{CSFeManImp, EquipInventoryData, GameDataMan, MapItemMan};
use fromsoftware_shared::FromStatic;
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::offsets;
use crate::snap;

type ItemGiveFn =
    unsafe extern "C" fn(map_item_man: usize, items: *const u32, data: *mut u8, unk: u32) -> usize;

/// `RemoveItem(EquipInventoryData*, abs_idx, count)`
///
/// `abs_idx` 是跨列表的绝对下标：`[0, key_items_capacity)` 是钥匙物品，
/// 之后才是普通物品。所以普通物品第 i 格要传 `key_items_capacity + i`。
type RemoveItemFn = unsafe extern "C" fn(
    equip_inventory: *mut EquipInventoryData,
    abs_idx: u32,
    count: u32,
) -> usize;

static ITEM_GIVE: OnceLock<Option<ItemGiveFn>> = OnceLock::new();
static REMOVE_ITEM: OnceLock<Option<RemoveItemFn>> = OnceLock::new();
/// TGA ItemGive 持久缓冲（data + table@+32）
static GIVE_BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn looks_like_fn(addr: usize) -> bool {
    if addr < 0x1_0000 {
        return false;
    }
    let first = unsafe { *(addr as *const u8) };
    matches!(
        first,
        0x40 | 0x41 | 0x44 | 0x45 | 0x48 | 0x4C | 0x50 | 0x51 | 0x53 | 0x55 | 0x56 | 0x57
    )
}

fn item_give() -> Option<ItemGiveFn> {
    *ITEM_GIVE.get_or_init(|| {
        let addr = offsets::resolve_item_give()?;
        if !looks_like_fn(addr) {
            return None;
        }
        Some(unsafe { std::mem::transmute::<usize, ItemGiveFn>(addr) })
    })
}

pub fn item_give_ready() -> bool {
    item_give().is_some()
}

// ---------------------------------------------------------------- 获得提示 / 弹窗屏蔽
//
// ItemGive 会：
//   1. 调 ItemPopup（入队右侧获得日志）
//   2. 再调 0x55AC70（另一条写入 FE / 弹窗的路径）
// 只 NOP ItemPopup 不够。两处一起打成 `ret`，并用嵌套计数避免穿戴阶段误恢复。

/// MapItemMan ItemPopup 队列计数。绝不能写成 ≥10 来“跳过”——显示端会按计数刷弹窗。
const MAP_ITEM_POPUP_COUNT_OFF: usize = 0x148;

struct PatchedFn {
    addr: usize,
    orig: u8,
}

static SUPPRESS_DEPTH: AtomicU32 = AtomicU32::new(0);
static SUPPRESS_PATCHES: Mutex<Vec<PatchedFn>> = Mutex::new(Vec::new());

fn patch_byte(addr: usize, byte: u8) -> bool {
    unsafe {
        let mut old = PAGE_EXECUTE_READWRITE;
        if VirtualProtect(addr as *const _, 1, PAGE_EXECUTE_READWRITE, &mut old).is_err() {
            return false;
        }
        *(addr as *mut u8) = byte;
        let _ = VirtualProtect(addr as *const _, 1, old, &mut old);
        let _ = FlushInstructionCache(GetCurrentProcess(), Some(addr as *const _), 1);
        true
    }
}

fn clear_item_popup_queue() {
    let Ok(man) = (unsafe { MapItemMan::instance() }) else {
        return;
    };
    let base = man as *const MapItemMan as *mut u8;
    unsafe {
        *(base.add(MAP_ITEM_POPUP_COUNT_OFF) as *mut u64) = 0;
    }
}

/// 尽量清掉 FE 获得日志缓冲（布局不完整时只清前缀，避免把整个 view model 抹掉）。
fn clear_fe_item_log() {
    let Ok(fe) = (unsafe { CSFeManImp::instance_mut() }) else {
        return;
    };
    // get_item_log_view_model 开头通常是计数 / 写指针；清前 0x40 字节足以丢掉待显示项。
    fe.get_item_log_view_model[..0x40].fill(0);
}

fn apply_suppress_patches() {
    let Ok(mut guard) = SUPPRESS_PATCHES.lock() else {
        return;
    };
    if !guard.is_empty() {
        return;
    }

    let mut targets = Vec::new();
    if let Some(addr) = offsets::resolve_item_popup() {
        targets.push(addr);
    }
    if let Some(addr) = offsets::resolve_item_get_ui() {
        targets.push(addr);
    }

    for addr in targets {
        let orig = unsafe { *(addr as *const u8) };
        if orig == 0xC3 {
            continue;
        }
        if patch_byte(addr, 0xC3) {
            guard.push(PatchedFn { addr, orig });
        }
    }
}

fn restore_suppress_patches() {
    let Ok(mut guard) = SUPPRESS_PATCHES.lock() else {
        return;
    };
    while let Some(p) = guard.pop() {
        let _ = patch_byte(p.addr, p.orig);
    }
}

/// 压入一层屏蔽（可嵌套）。第一次压入时打补丁。
pub fn suppress_push() {
    clear_item_popup_queue();
    clear_fe_item_log();
    if SUPPRESS_DEPTH.fetch_add(1, Ordering::SeqCst) == 0 {
        apply_suppress_patches();
    }
}

/// 弹出一层屏蔽。归零时还原补丁。
pub fn suppress_pop() {
    clear_item_popup_queue();
    clear_fe_item_log();
    let prev = SUPPRESS_DEPTH.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        restore_suppress_patches();
    } else if prev == 0 {
        // 防御：多余 pop
        SUPPRESS_DEPTH.store(0, Ordering::SeqCst);
    }
}

/// 兼容旧调用：true=压到至少 1 层，false=强制清零并还原。
pub fn set_item_popup_suppressed(suppressed: bool) {
    if suppressed {
        if SUPPRESS_DEPTH.load(Ordering::SeqCst) == 0 {
            suppress_push();
        } else {
            clear_item_popup_queue();
            clear_fe_item_log();
        }
    } else {
        // 强制解除（同步结束 / 出错）
        SUPPRESS_DEPTH.store(0, Ordering::SeqCst);
        restore_suppress_patches();
        clear_item_popup_queue();
        clear_fe_item_log();
    }
}

/// TGA 发包：物品表必须在 `data + 32`（与 CE 同布局）。
/// 整块缓冲也做成持久的，避免游戏异步再读。
pub fn give_tga_packet(entries: &[(u32, u32, u32)]) -> Result<(), String> {
    if entries.is_empty() || entries.len() > 10 {
        return Err(format!("TGA 批次长度必须 1..=10，收到 {}", entries.len()));
    }
    let func = item_give().ok_or("ItemGive 未解析")?;
    let man = unsafe { MapItemMan::instance() }.map_err(|_| "MapItemMan 未就绪")?;
    let man_ptr = man as *const MapItemMan as usize;

    // 持久缓冲：对齐 TGA allocateMemory；table 在 +32
    let mut guard = GIVE_BUF.lock().map_err(|_| "give buffer lock")?;
    if guard.len() < 256 {
        *guard = vec![0u8; 256];
    }
    guard.fill(0);
    let data = guard.as_mut_ptr();
    let table = unsafe { data.add(32) as *mut u32 };

    unsafe {
        *table = entries.len() as u32;
        for (i, &(id, qty, gem)) in entries.iter().enumerate() {
            let base = 1 + i * 4;
            *table.add(base) = id;
            *table.add(base + 1) = qty.max(1);
            *table.add(base + 2) = 0xFFFF_FFFF;
            *table.add(base + 3) = gem;
        }
        // 第 4 参非 0：ItemGive 写入队条目静默位；再配合函数级 ret 补丁双保险。
        let _ = func(man_ptr, table as *const u32, data, 1);
    }
    clear_item_popup_queue();
    clear_fe_item_log();
    Ok(())
}

/// 只发一件；重复件必须各调一次（护符/武器不能靠 qty 堆）。
pub fn give_tga_one(id: u32, quantity: u32, gem: u32) -> Result<(), String> {
    give_tga_packet(&[(id, quantity.max(1), gem)])
}

// ---------------------------------------------------------------- RemoveItem

fn remove_item() -> Option<RemoveItemFn> {
    *REMOVE_ITEM.get_or_init(|| {
        let addr = offsets::resolve_remove_item()?;
        if !looks_like_fn(addr) {
            return None;
        }
        Some(unsafe { std::mem::transmute::<usize, RemoveItemFn>(addr) })
    })
}

pub fn remove_item_ready() -> bool {
    remove_item().is_some()
}

/// 探测报告：不调用，只说解析到了什么。
pub fn remove_item_report() -> String {
    match offsets::resolve_remove_item() {
        None => "RemoveItem：解析失败（AOB 非唯一命中或序言不符），已拒绝调用".to_string(),
        Some(addr) => {
            let bytes = offsets::read_bytes(addr, 16);
            let hex = bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "RemoveItem：RVA 0x{:x}\n序言 {}\n序言校验 {}",
                offsets::to_rva(addr),
                hex,
                if remove_item().is_some() {
                    "通过"
                } else {
                    "未通过"
                }
            )
        }
    }
}

/// 玩家背包的可变裸指针。拿到之后不要再持有任何 `&`/`&mut` 引用就调原生函数。
pub fn inv_ptr() -> Result<*mut EquipInventoryData, String> {
    let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
    Ok(&mut gdm
        .main_player_game_data
        .as_mut()
        .equipment
        .equip_inventory_data as *mut _)
}

/// 普通物品第 `normal_idx` 格对应的绝对索引。
///
/// 绝对索引空间前半段是钥匙物品（长度 = key_items_capacity），普通物品接在后面。
/// 这与 `ItemIdMapping::item_slot` 的文档一致，也与 CT 里 `idx + tailDataIdx` 一致。
fn abs_index(inv: &EquipInventoryData, normal_idx: usize) -> u32 {
    inv.items_data.key_items_capacity + normal_idx as u32
}

/// 找普通列表里第一件我们管理的物品（武器 / 护甲 / 护符，排除弹药）。
/// 返回（绝对索引, item_id, 数量）。
pub fn first_managed(inv: &EquipInventoryData) -> Option<(u32, u32, u32)> {
    for (i, slot) in inv.items_data.normal_entries().iter().enumerate() {
        let Some(entry) = slot.as_option() else {
            continue;
        };
        if !snap::is_managed(entry.item_id) {
            continue;
        }
        return Some((
            abs_index(inv, i),
            entry.item_id.into_inner(),
            entry.quantity.max(1),
        ));
    }
    None
}

/// 普通列表里还有多少件待删物品（按条目数，不按数量）
pub fn managed_entry_count(inv: &EquipInventoryData) -> u32 {
    inv.items_data
        .normal_entries()
        .iter()
        .filter_map(|s| s.as_option())
        .filter(|e| snap::is_managed(e.item_id))
        .count() as u32
}

/// 删掉绝对索引处的物品。调用前必须先卸装，避免 ChrAsm still 引用它。
pub fn remove_at(abs_idx: u32, count: u32) -> Result<(), String> {
    let func = remove_item().ok_or("RemoveItem 未解析")?;
    let inv = inv_ptr()?;
    let _ = unsafe { func(inv, abs_idx, count.max(1)) };
    Ok(())
}
