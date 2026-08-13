//! TGA `ItemGive_code` 的 Rust 复刻。
//!
//! 对照 `ER_TGA_v1.18.0.CT` → Scripts → ItemGib → ItemGive_code：
//! - AOB 同为 `8B 02 83 F8 0A` − 0x52
//! - 发包：`count` + 最多 10 条 `{id, qty, 0xFFFFFFFF, gem}`
//! - 武器先按强化等级加成，再加亲和偏移，再挂灰烬
//!
//! 参数表读取用 `SoloParamRepository`，等价于 TGA 里的 `getParamAddr`。
//! 清空逻辑不在这里，仍走我们自己的 `RemoveItem`。

use eldenring::cs::{GameDataMan, ItemCategory, ItemId, MapItemMan, SoloParamRepository};
use eldenring::param::EQUIP_PARAM_WEAPON_ST;
use fromsoftware_shared::FromStatic;

use crate::game;
use crate::snap::BagItem;

const AFFINITY_OFFSETS: [u32; 13] = [
    1200, 1100, 1000, 900, 800, 700, 600, 500, 400, 300, 200, 100, 0,
];

/// 把背包里的成品武器 id 拆成 TGA / 构筑规划器那种五元组。
///
/// 规划器写法：`{ base, 1, reinforce, upgrade, gem }`，
/// ItemGive 会做 `base + reinforce + upgrade`。
pub fn decode_weapon(full_id: u32, gem: i64) -> BagItem {
    let Ok(id) = ItemId::try_from(full_id) else {
        return BagItem::exact(full_id, 1, gem);
    };
    if id.category() != ItemCategory::Weapon {
        return BagItem::exact(full_id, 1, gem);
    }

    let pid = id.param_id();
    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        return BagItem::exact(full_id, 1, gem);
    };
    let Some(row) = repo.get::<eldenring::cs::EquipParamWeapon>(pid) else {
        return BagItem::exact(full_id, 1, gem);
    };

    let origin = row.origin_equip_wep();
    let reinforce = if origin >= 0 {
        let r = pid as i32 - origin;
        if (0..=25).contains(&r) { r } else { 0 }
    } else {
        0
    };
    let plus0 = if reinforce > 0 {
        (pid as i32 - reinforce) as u32
    } else {
        pid
    };

    for &aff in &AFFINITY_OFFSETS {
        if aff == 0 {
            return BagItem {
                id: plus0,
                qty: 1,
                reinforce,
                upgrade: 0,
                gem,
                sort_id: 0,
            };
        }
        if plus0 >= aff {
            let base = plus0 - aff;
            if repo.get::<eldenring::cs::EquipParamWeapon>(base).is_some()
                && base + aff + reinforce as u32 == pid
            {
                return BagItem {
                    id: base,
                    qty: 1,
                    reinforce,
                    upgrade: aff as i32,
                    gem,
                    sort_id: 0,
                };
            }
        }
    }

    BagItem {
        id: plus0,
        qty: 1,
        reinforce,
        upgrade: 0,
        gem,
        sort_id: 0,
    }
}

/// 对一条五元组做 TGA 同款变换，得到最终 item_id / qty / gem。
///
/// 武器/护甲/护符的 qty 在这里仍按 TGA 写成 1；重复份数由上层拆成多条再调。
pub fn resolve_one(item: &BagItem) -> Option<(u32, u32, u32)> {
    let Ok(parsed) = ItemId::try_from(item.id) else {
        return None;
    };
    let category = parsed.category();
    let param_id = parsed.param_id();

    match category {
        ItemCategory::Weapon => {
            let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
                return Some((item.id, 1, gem_u32(item.gem)));
            };
            let row = repo
                .get::<eldenring::cs::EquipParamWeapon>(param_id)
                .or_else(|| repo.get::<eldenring::cs::EquipParamWeapon>((param_id / 100) * 100));

            let mut out_id = item.id;
            if let Some(row) = row {
                out_id = apply_reinforce(item.id, item.reinforce, row);
                if (0..=1200).contains(&item.upgrade) {
                    out_id = out_id.wrapping_add(item.upgrade as u32);
                }
            } else if (0..=25).contains(&item.reinforce) {
                out_id = out_id.wrapping_add(item.reinforce as u32);
                if (0..=1200).contains(&item.upgrade) {
                    out_id = out_id.wrapping_add(item.upgrade as u32);
                }
            }

            Some((out_id, 1, gem_u32(item.gem)))
        }
        ItemCategory::Protector | ItemCategory::Accessory => Some((item.id, 1, 0xFFFF_FFFF)),
        ItemCategory::Goods | ItemCategory::Gem => Some((item.id, item.qty.max(1), 0xFFFF_FFFF)),
    }
}

fn gem_u32(gem: i64) -> u32 {
    if gem < 0 { 0xFFFF_FFFF } else { gem as u32 }
}

/// TGA `set_reinforceLv`
fn apply_reinforce(weapon_id: u32, input: i32, row: &EQUIP_PARAM_WEAPON_ST) -> u32 {
    let origin1 = row.origin_equip_wep1();
    // 0xFFFFFFFF as i32 == -1：不可强化
    if origin1 == -1 {
        return weapon_id;
    }

    let mut lv = input;
    if !(0..=25).contains(&lv) {
        lv = player_reinforce_lv() as i32;
    }

    let origin16 = row.origin_equip_wep16();
    let result = if origin16 == -1 {
        // 只能到 +10 的那条：25 级输入折到 0..10
        ((lv as f64 + 0.5) / 2.5).floor() as i32
    } else {
        lv
    };

    weapon_id.wrapping_add(result.max(0) as u32)
}

fn player_reinforce_lv() -> u8 {
    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return 25;
    };
    gdm.main_player_game_data.as_ref().matching_weapon_level
}

/// 发一条（内部已 resolve）。返回最终 (id, gem) 便于事后写 sort_id。
pub fn give_one(item: &BagItem) -> Result<(u32, u32), String> {
    let results = give_batch(std::slice::from_ref(item))?;
    results
        .into_iter()
        .next()
        .ok_or_else(|| "发放结果为空".into())
}

/// 批量发放（最多 10，与 TGA 上限一致）。返回每件最终 (id, gem)。
pub fn give_batch(items: &[BagItem]) -> Result<Vec<(u32, u32)>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if items.len() > 10 {
        return Err(format!("批次过长：{}", items.len()));
    }
    let mut entries = Vec::with_capacity(items.len());
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let (id, qty, gem) = resolve_one(item).ok_or("物品无效")?;
        if qty == 0 {
            return Err("数量为 0".into());
        }
        entries.push((id, qty, gem));
        out.push((id, gem));
    }
    game::give_tga_packet(&entries)?;
    Ok(out)
}

/// 探测：ItemGive / MapItemMan 是否就绪
pub fn ready_report() -> String {
    let ig = if game::item_give_ready() {
        "ItemGive 已解析"
    } else {
        "ItemGive 未解析"
    };
    let mm = match unsafe { MapItemMan::instance() } {
        Ok(_) => "MapItemMan 就绪",
        Err(_) => "MapItemMan 未就绪",
    };
    format!("TGA 发放：{ig} / {mm}")
}
