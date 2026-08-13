//! 法术轮盘：读/写 `EquipMagicData`（对齐 TGA changeMagic 的效果）
//!
//! 存的是 Magic param id（如 6800）；空槽为 -1。
//! 应用前若背包没有对应 Goods（`0x40000000 + id`）会先 ItemGive。

use eldenring::cs::{EquipInventoryData, GameDataMan, Magic, SoloParamRepository};
use fromsoftware_shared::FromStatic;

use crate::game;
use crate::snap::BagItem;
use crate::tga;

pub const MAGIC_SLOTS: usize = 14;

fn goods_id(magic_param: i32) -> u32 {
    0x4000_0000u32.wrapping_add(magic_param as u32)
}

fn owns_goods(inv: &EquipInventoryData, goods: u32) -> bool {
    inv.items_data
        .items()
        .any(|e| e.item_id.into_inner() == goods)
}

fn charges_of(magic_param: i32) -> i32 {
    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        return 1;
    };
    repo.get::<Magic>(magic_param as u32)
        .map(|r| r.max_quantity().max(1) as i32)
        .unwrap_or(1)
}

/// 捕获当前 14 格法术轮盘（param id；空 = -1）
pub fn capture() -> Result<Vec<i32>, String> {
    let gdm = unsafe { GameDataMan::instance() }.map_err(|_| "GameDataMan 未就绪")?;
    let magic = gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_magic_data
        .as_ref();
    let mut out = Vec::with_capacity(MAGIC_SLOTS);
    for e in &magic.entries {
        let id = e.param_id;
        out.push(if id <= 0 { -1 } else { id });
    }
    Ok(out)
}

pub fn describe(slots: &[i32]) -> String {
    if slots.is_empty() {
        return "法术：无（请重新保存）".into();
    }
    let n = slots.iter().filter(|&&id| id > 0).count();
    format!("法术 {}/{}", n, MAGIC_SLOTS)
}

/// 缺的法术 Goods 做成发放队列条目
pub fn missing_goods(slots: &[i32]) -> Vec<BagItem> {
    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return Vec::new();
    };
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &id in slots {
        if id <= 0 || !seen.insert(id) {
            continue;
        }
        let gid = goods_id(id);
        if owns_goods(inv, gid) {
            continue;
        }
        out.push(BagItem::exact(gid, 1, -1));
    }
    out
}

/// 写入轮盘；必要时先发缺的法术。返回成功写入的非空格数。
pub fn apply(slots: &[i32]) -> Result<u32, String> {
    if slots.is_empty() {
        return Ok(0);
    }

    // 先补齐 Goods，避免只写 param 但未解锁
    for it in missing_goods(slots) {
        let (id, qty, gem) = tga::resolve_one(&it).ok_or("法术物品无效")?;
        game::give_tga_one(id, qty, gem)?;
    }

    let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
    let p = gdm.main_player_game_data.as_mut();

    // 解锁格数至少盖住最高有用法术槽
    let mut need = 0u8;
    for (i, &id) in slots.iter().take(MAGIC_SLOTS).enumerate() {
        if id > 0 {
            need = (i as u8).saturating_add(1);
        }
    }
    if need > 0 && p.unlocked_magic_slots < need {
        p.unlocked_magic_slots = need;
    }

    let magic = p.equipment.equip_magic_data.as_mut();
    let mut wrote = 0u32;
    for i in 0..MAGIC_SLOTS {
        let id = slots.get(i).copied().unwrap_or(-1);
        if id > 0 {
            magic.entries[i].param_id = id;
            magic.entries[i].charges = charges_of(id);
            wrote += 1;
        } else {
            magic.entries[i].param_id = -1;
            magic.entries[i].charges = 0;
        }
    }
    if magic.selected_slot < 0 || magic.selected_slot as usize >= MAGIC_SLOTS {
        magic.selected_slot = 0;
    }
    Ok(wrote)
}
