//! TGA `equipItem` / 穿戴方案。
//!
//! 关键规则：
//! - EquipGear 第 3 参 = **gaitem 句柄**（TGA `getItemByIdx`），第 4 参 = abs_idx
//! - 空槽 unequip：第 3 参 = **item_id**（徒手/裸装），与 TGA unequipItem 一致
//! - 穿戴后**不要**再手写 ChrAsm——会破坏 EquipGear 写入的库存索引，卸装时武器消失
//! - 清栏前卸装也必须走 EquipGear，禁止 soft_strip 写 handle=0
//! - 保留未装备的徒手；只清未装备的裸装占位

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use eldenring::cs::{
    ChrAsmArmStyle, EquipGameData, GaitemHandle, GameDataMan, ItemCategory, ItemId, PlayerGameData,
    PlayerIns,
};
use fromsoftware_shared::FromStatic;
use serde::{Deserialize, Serialize};

use crate::offsets;
use crate::snap;

pub const FIST: u32 = 110_000;
pub const BARE_ARMOR: [u32; 4] = [0x1000_2710, 0x1000_2774, 0x1000_27D8, 0x1000_283C];

const EQUIP_GAME_DATA_OFF: usize = 0x2B0;

pub const LOADOUT_SLOTS: [u8; 14] = [0, 1, 2, 3, 4, 5, 12, 13, 14, 15, 17, 18, 19, 20];

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LoadoutSlot {
    pub slot: u8,
    pub id: u32,
    #[serde(default = "default_gem")]
    pub gem: i64,
    /// 重复武器使用保存时的 sort_id 精确定位；与护符位次系统分离。
    #[serde(default)]
    pub weapon_sort_id: Option<u32>,
    /// 护符在同 ID 实例中的保存顺序（按 sort_id、再按库存下标排序，0 起始）。
    /// 仅重复护符写入；旧存档及非重复物品保持 None。
    #[serde(default)]
    pub accessory_ordinal: Option<u32>,
}

fn default_gem() -> i64 {
    -1
}

type EquipGearFn = unsafe extern "C" fn(
    equip_game_data: *mut EquipGameData,
    slot: u32,
    item_id: *const u32,
    abs_idx: u32,
    a: u32,
    b: u32,
    c: u32,
) -> usize;

static EQUIP_GEAR: OnceLock<Option<EquipGearFn>> = OnceLock::new();
static EQUIP_DATA: Mutex<[u8; 0x20]> = Mutex::new([0u8; 0x20]);

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

fn equip_gear() -> Option<EquipGearFn> {
    *EQUIP_GEAR.get_or_init(|| {
        let addr = offsets::resolve_equip_gear()?;
        if !looks_like_fn(addr) {
            return None;
        }
        Some(unsafe { std::mem::transmute::<usize, EquipGearFn>(addr) })
    })
}

pub fn equip_gear_ready() -> bool {
    equip_gear().is_some()
}

pub fn equip_gear_report() -> String {
    match offsets::resolve_equip_gear() {
        None => "EquipGear：解析失败".into(),
        Some(addr) => format!(
            "EquipGear：RVA 0x{:x} / {}",
            offsets::to_rva(addr),
            if equip_gear().is_some() {
                "可用"
            } else {
                "序言不符"
            }
        ),
    }
}

fn equip_game_data_ptr() -> Result<*mut EquipGameData, String> {
    let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
    let player = gdm.main_player_game_data.as_mut() as *mut PlayerGameData;
    let by_field = &mut gdm.main_player_game_data.as_mut().equipment as *mut EquipGameData;
    let by_off = unsafe { (player as *mut u8).add(EQUIP_GAME_DATA_OFF) as *mut EquipGameData };
    Ok(if by_field == by_off { by_field } else { by_off })
}

pub fn is_empty_loadout(slot: u8, id: u32) -> bool {
    match slot {
        0..=5 => id == FIST || id == 0,
        12..=15 => BARE_ARMOR.contains(&id) || id == 0,
        17..=20 => id == 0xFFFF_FFFF || id == 0,
        _ => true,
    }
}

pub fn is_placeholder_item(id: ItemId) -> bool {
    let raw = id.into_inner();
    if id.category() == ItemCategory::Weapon && id.param_id() == FIST {
        return true;
    }
    if id.category() == ItemCategory::Protector && BARE_ARMOR.contains(&raw) {
        return true;
    }
    false
}

/// EquipGear：第 3 参必须是物品栏条目的 **gaitem 句柄**（TGA `getItemByIdx`），
/// 不是 item_id。传错会导致装备记账坏掉——脱武器时物品从背包消失、空槽错乱。
fn equip_gear_call(slot: u8, gaitem_or_id: u32, abs_idx: u32) -> Result<(), String> {
    if !LOADOUT_SLOTS.contains(&slot) {
        return Err(format!("非法穿戴槽 {}", slot));
    }
    let func = equip_gear().ok_or("EquipGear 未解析")?;
    let eq = equip_game_data_ptr()?;

    let mut buf = EQUIP_DATA.lock().map_err(|_| "equip buffer lock")?;
    buf.fill(0);
    let id_ptr = unsafe {
        let p = buf.as_mut_ptr().add(0x10) as *mut u32;
        *p = gaitem_or_id;
        p as *const u32
    };
    let _ = unsafe { func(eq, slot as u32, id_ptr, abs_idx, 1, 1, 0) };
    Ok(())
}

fn entry_handle_raw(entry: &eldenring::cs::EquipInventoryDataListEntry) -> u32 {
    unsafe { *(&entry.gaitem_handle as *const GaitemHandle as *const u32) }
}

/// 返回同 ID 实例，顺序与存档定义一致：sort_id 升序，相同则按库存下标。
/// 元组为（普通库存下标、绝对库存下标、gaitem handle、sort_id）。
fn inventory_instances(item_id: u32) -> Result<Vec<(usize, u32, u32, u32)>, String> {
    let gdm = unsafe { GameDataMan::instance() }.map_err(|_| "GameDataMan 未就绪")?;
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;
    let key_cap = inv.items_data.key_items_capacity;
    let mut matches: Vec<_> = inv
        .items_data
        .normal_entries()
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            let entry = slot.as_option()?;
            (entry.item_id.into_inner() == item_id).then_some((
                index,
                key_cap + index as u32,
                entry_handle_raw(entry),
                entry.sort_id,
            ))
        })
        .collect();
    matches.sort_by_key(|entry| (entry.3, entry.0));
    Ok(matches)
}

fn accessory_ordinal_of_slot(slot: usize, item_id: u32) -> Option<u32> {
    let instances = inventory_instances(item_id).ok()?;
    if instances.len() < 2 {
        return None;
    }
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let equipment = &gdm.main_player_game_data.as_ref().equipment;
    if let Some(equipped_index) = equipment.inventory_index(slot)
        && let Some(index) = instances
            .iter()
            .position(|entry| entry.1 == equipped_index || entry.0 as u32 == equipped_index)
    {
        return Some(index as u32);
    }

    // 仅兼容旧状态：库存索引无效时才退回句柄匹配。
    let equipped_handle = equipment.chr_asm.gaitem_handles.get(slot)?.0;
    instances
        .iter()
        .position(|entry| entry.2 == equipped_handle)
        .map(|index| index as u32)
}

fn weapon_sort_id_of_slot(slot: usize, item_id: u32) -> Option<u32> {
    let instances = inventory_instances(item_id).ok()?;
    if instances.len() < 2 {
        return None;
    }
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let equipped_handle = gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .chr_asm
        .gaitem_handles
        .get(slot)?
        .0;
    instances
        .iter()
        .find(|entry| entry.2 == equipped_handle)
        .map(|entry| entry.3)
}

/// 在普通物品栏里找指定 id（可跳过已占用下标）
fn find_inv_id(item_id: u32, skip: &HashSet<usize>) -> Option<(u32, u32, usize)> {
    let gdm = unsafe { GameDataMan::instance_mut() }.ok()?;
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;
    let key_cap = inv.items_data.key_items_capacity;
    for (i, slot) in inv.items_data.normal_entries().iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        let Some(entry) = slot.as_option() else {
            continue;
        };
        if entry.item_id.into_inner() != item_id {
            continue;
        }
        return Some((key_cap + i as u32, entry_handle_raw(entry), i));
    }
    None
}

/// TGA unequip：空槽要 EquipGear 徒手/裸装（缺则 ItemGive），护符槽传 0xFFFFFFFF。
fn ensure_placeholder(item_id: u32) -> Result<(u32, u32, usize), String> {
    let empty = HashSet::new();
    if let Some(found) = find_inv_id(item_id, &empty) {
        return Ok(found);
    }
    crate::game::suppress_push();
    let give = crate::game::give_tga_one(item_id, 1, 0xFFFF_FFFF);
    crate::game::suppress_pop();
    give?;
    find_inv_id(item_id, &empty).ok_or_else(|| format!("占位物 0x{:08X} 发放后仍找不到", item_id))
}

/// 清栏前卸装：必须走 TGA EquipGear，禁止 soft_strip 写 handle=0。
/// 手写清空句柄会破坏库存索引，之后在游戏里脱武器时物品会从背包消失。
pub fn unequip_all() -> Result<(), String> {
    crate::game::suppress_push();
    let result = (|| {
        for &slot in &LOADOUT_SLOTS {
            unequip_slot_tga(slot)?;
        }
        Ok(())
    })();
    crate::game::suppress_pop();
    result
}

fn equipped_handles() -> HashSet<u32> {
    let mut out = HashSet::new();
    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return out;
    };
    for h in gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .chr_asm
        .gaitem_handles
    {
        let raw = h.0;
        if raw != 0 && raw != u32::MAX {
            out.insert(raw);
        }
    }
    if let Ok(player) = unsafe { PlayerIns::local_player() } {
        for h in player.chr_asm.gaitem_handles {
            let raw = h.0;
            if raw != 0 && raw != u32::MAX {
                out.insert(raw);
            }
        }
    }
    out
}

/// TGA `unequipItem(slot)`：空槽只走 EquipGear；护符槽传 0xFFFFFFFF。
/// 与 TGA 一致：第 3 参写 **item_id**（徒手/裸装），不是 gaitem 句柄。
fn unequip_slot_tga(slot: u8) -> Result<(), String> {
    match slot {
        0..=5 => {
            let (abs, _handle, _idx) = ensure_placeholder(FIST)?;
            equip_gear_call(slot, FIST, abs)?;
        }
        12..=15 => {
            let bare = BARE_ARMOR[(slot - 12) as usize];
            let (abs, _handle, _idx) = ensure_placeholder(bare)?;
            equip_gear_call(slot, bare, abs)?;
        }
        17..=20 => {
            equip_gear_call(slot, 0xFFFF_FFFF, 0xFFFF_FFFF)?;
        }
        _ => {}
    }
    Ok(())
}

fn find_entry(
    item_id: u32,
    gem: Option<i64>,
    weapon_sort_id: Option<u32>,
    used: &mut HashSet<usize>,
) -> Option<(u32, u32, usize)> {
    let gdm = unsafe { GameDataMan::instance_mut() }.ok()?;
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;
    let key_cap = inv.items_data.key_items_capacity;

    for (i, slot) in inv.items_data.normal_entries().iter().enumerate() {
        if used.contains(&i) {
            continue;
        }
        let Some(entry) = slot.as_option() else {
            continue;
        };
        if entry.item_id.into_inner() != item_id {
            continue;
        }
        if let Some(want_sort_id) = weapon_sort_id
            && entry.sort_id != want_sort_id
        {
            continue;
        }
        if let Some(want_gem) = gem {
            if want_gem >= 0 {
                let g = snap::gem_of_handle(&entry.gaitem_handle);
                if g != want_gem {
                    continue;
                }
            }
        }
        used.insert(i);
        return Some((key_cap + i as u32, entry_handle_raw(entry), i));
    }
    None
}

fn gem_of_slot(slot: usize) -> i64 {
    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return -1;
    };
    let handle = gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .chr_asm
        .gaitem_handles
        .get(slot)
        .copied();
    match handle {
        Some(h) if h.0 != 0 => snap::gem_of_handle(&h),
        _ => -1,
    }
}

pub fn capture_loadout() -> Result<Vec<LoadoutSlot>, String> {
    let gdm = unsafe { GameDataMan::instance() }.map_err(|_| "GameDataMan 未就绪")?;
    let ent = gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equipment_entries;

    let mut out = Vec::with_capacity(14);

    let weapons: [(u8, ItemId); 6] = [
        (0, ent.weapon_primary_left),
        (1, ent.weapon_primary_right),
        (2, ent.weapon_secondary_left),
        (3, ent.weapon_secondary_right),
        (4, ent.weapon_tertiary_left),
        (5, ent.weapon_tertiary_right),
    ];
    for (slot, id) in weapons {
        out.push(LoadoutSlot {
            slot,
            id: id.into_inner(),
            gem: gem_of_slot(slot as usize),
            weapon_sort_id: weapon_sort_id_of_slot(slot as usize, id.into_inner()),
            accessory_ordinal: None,
        });
    }

    let armor: [(u8, ItemId); 4] = [
        (12, ent.protector_head),
        (13, ent.protector_chest),
        (14, ent.protector_hands),
        (15, ent.protector_legs),
    ];
    for (slot, id) in armor {
        out.push(LoadoutSlot {
            slot,
            id: id.into_inner(),
            gem: -1,
            weapon_sort_id: None,
            accessory_ordinal: None,
        });
    }

    for (i, opt) in ent.accessories.iter().enumerate() {
        let slot = (17 + i) as u8;
        let id = match opt.as_valid() {
            Some(id) => id.into_inner(),
            None => 0xFFFF_FFFF,
        };
        out.push(LoadoutSlot {
            slot,
            id,
            gem: -1,
            weapon_sort_id: None,
            accessory_ordinal: (!is_empty_loadout(slot, id))
                .then(|| accessory_ordinal_of_slot(slot as usize, id))
                .flatten(),
        });
    }

    if out.len() != 14 {
        return Err(format!("穿戴捕获异常：得到 {} 格", out.len()));
    }
    Ok(out)
}

pub fn loadout_describe(slots: &[LoadoutSlot]) -> String {
    if slots.is_empty() {
        return "穿戴：无（请重新保存）".into();
    }
    let mut w = 0u32;
    let mut a = 0u32;
    let mut t = 0u32;
    for s in slots {
        if is_empty_loadout(s.slot, s.id) {
            continue;
        }
        match s.slot {
            0..=5 => w += 1,
            12..=15 => a += 1,
            17..=20 => t += 1,
            _ => {}
        }
    }
    format!("穿戴 武器{}/6 护甲{}/4 护符{}/4", w, a, t)
}

pub fn apply_one(slot: &LoadoutSlot, used: &mut HashSet<usize>) -> Result<bool, String> {
    if is_empty_loadout(slot.slot, slot.id) {
        unequip_slot_tga(slot.slot)?;
        return Ok(true);
    }

    let gem = if slot.gem >= 0 { Some(slot.gem) } else { None };
    let (abs, handle, _normal_idx) = find_entry(slot.id, gem, slot.weapon_sort_id, used)
        .or_else(|| {
            if gem.is_some() {
                find_entry(slot.id, None, slot.weapon_sort_id, used)
            } else {
                None
            }
        })
        .or_else(|| {
            if slot.weapon_sort_id.is_some() {
                find_entry(slot.id, gem, None, used)
            } else {
                None
            }
        })
        .or_else(|| {
            if slot.weapon_sort_id.is_some() && gem.is_some() {
                find_entry(slot.id, None, None, used)
            } else {
                None
            }
        })
        .ok_or_else(|| {
            format!(
                "槽 {} 要装 0x{:08X} gem={}，物品栏里找不到",
                slot.slot, slot.id, slot.gem
            )
        })?;

    let Ok(parsed) = ItemId::try_from(slot.id) else {
        return Err(format!("槽 {} 物品 id 无效", slot.slot));
    };
    let ok = match slot.slot {
        0..=5 => parsed.category() == ItemCategory::Weapon,
        12..=15 => parsed.category() == ItemCategory::Protector,
        17..=20 => parsed.category() == ItemCategory::Accessory,
        _ => false,
    };
    if !ok {
        return Err(format!(
            "槽 {} 与类别 {:?} 不匹配，已拒绝",
            slot.slot,
            parsed.category()
        ));
    }

    // 严格按 TGA：只 EquipGear（第 3 参 = gaitem 句柄，第 4 参 = abs_idx）。
    // 禁止事后手写 ChrAsm——会破坏 unk8 库存索引，导致游戏内卸装时武器消失。
    equip_gear_call(slot.slot, handle, abs)?;
    Ok(true)
}

/// 同步全部阶段结束前，按保存时记录的重复护符序号精确重装。
pub fn apply_saved_accessory_ordinals(slots: &[LoadoutSlot]) -> Result<u32, String> {
    let mut applied = 0u32;
    for slot in slots
        .iter()
        .filter(|slot| (17..=20).contains(&slot.slot) && slot.accessory_ordinal.is_some())
    {
        let ordinal = slot.accessory_ordinal.unwrap() as usize;
        let instances = inventory_instances(slot.id)?;
        let target = instances.get(ordinal).copied().ok_or_else(|| {
            format!(
                "护符槽 {} 保存为同类第 {} 件，当前只有 {} 件",
                slot.slot,
                ordinal + 1,
                instances.len()
            )
        })?;
        equip_gear_call(slot.slot, target.2, target.1)?;
        applied += 1;
    }
    if applied > 0 {
        request_force_update();
    }
    Ok(applied)
}

/// 连打几帧 force_update。
pub fn request_force_update() {
    if let Ok(player) = unsafe { PlayerIns::local_player_mut() } {
        player.chr_ins.chr_flags1c4.set_force_update(true);
    }
}

fn arm_style_from_u8(style: u8) -> ChrAsmArmStyle {
    match style {
        0 => ChrAsmArmStyle::EmptyHanded,
        2 => ChrAsmArmStyle::LeftBothHands,
        3 => ChrAsmArmStyle::RightBothHands,
        _ => ChrAsmArmStyle::OneHanded,
    }
}

fn write_arm_style_field(style: ChrAsmArmStyle) {
    if let Ok(gdm) = unsafe { GameDataMan::instance_mut() } {
        gdm.main_player_game_data
            .as_mut()
            .equipment
            .chr_asm
            .equipment
            .arm_style = style;
    }
    if let Ok(player) = unsafe { PlayerIns::local_player_mut() } {
        player.chr_asm.as_mut().equipment.arm_style = style;
    }
}

/// 先回到单持姿态（只写字段，不请求动作），给 FSM 一帧空隙。
pub fn reset_arm_style_one_handed() {
    write_arm_style_field(ChrAsmArmStyle::OneHanded);
    request_force_update();
}

/// 恢复共持：写字段 + 请求切换动作（应在切武器刷新之后调用）。
pub fn apply_arm_style(style: u8) {
    let style = arm_style_from_u8(style);
    write_arm_style_field(style);

    if let Ok(player) = unsafe { PlayerIns::local_player_mut() } {
        let req = &mut player
            .chr_ins
            .modules
            .action_request
            .as_mut()
            .action_requests;
        match style {
            ChrAsmArmStyle::LeftBothHands => {
                req.set_change_style_l(true);
            }
            ChrAsmArmStyle::RightBothHands => {
                req.set_change_style_r(true);
            }
            _ => {}
        }
    }
    request_force_update();
}

/// 扫掉未装备的裸装占位；徒手全部保留（游戏内卸武器常要回退到徒手）。
pub fn purge_placeholders() -> u32 {
    let mut removed = 0u32;
    for _ in 0..32 {
        let equipped = equipped_handles();
        let Some((abs, qty)) = next_unequipped_bare(&equipped) else {
            break;
        };
        if crate::game::remove_at(abs, qty).is_ok() {
            removed += 1;
        } else {
            break;
        }
    }
    removed
}

/// 只找未装备的裸装（不删徒手）
fn next_unequipped_bare(equipped: &HashSet<u32>) -> Option<(u32, u32)> {
    let gdm = unsafe { GameDataMan::instance_mut() }.ok()?;
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;
    let key_cap = inv.items_data.key_items_capacity;
    for (i, slot) in inv.items_data.normal_entries().iter().enumerate() {
        let Some(entry) = slot.as_option() else {
            continue;
        };
        let raw = entry.item_id.into_inner();
        if !BARE_ARMOR.contains(&raw) {
            continue;
        }
        let handle = entry_handle_raw(entry);
        if equipped.contains(&handle) {
            continue;
        }
        return Some((key_cap + i as u32, entry.quantity.max(1)));
    }
    None
}

/// 先卸空槽，再装有货的槽（避免 EquipGear 顺序把空槽挤乱）
pub fn sort_loadout_for_apply(slots: &[LoadoutSlot]) -> Vec<LoadoutSlot> {
    let mut out = slots.to_vec();
    out.sort_by_key(|s| {
        let empty = is_empty_loadout(s.slot, s.id);
        (if empty { 0u8 } else { 1u8 }, s.slot)
    });
    out
}

pub fn apply_loadout(slots: &[LoadoutSlot]) -> Result<u32, String> {
    if slots.is_empty() {
        return Err("存档没有穿戴数据，请先 Shift+F6 重新保存".into());
    }
    if !equip_gear_ready() {
        return Err("EquipGear 未解析".into());
    }
    let ordered = sort_loadout_for_apply(slots);
    let mut used = HashSet::new();
    let mut ok = 0u32;
    let mut errs = Vec::new();
    for s in &ordered {
        match apply_one(s, &mut used) {
            Ok(_) => ok += 1,
            Err(e) => errs.push(e),
        }
    }
    request_force_update();
    let _ = purge_placeholders();
    if ok == 0 && !errs.is_empty() {
        return Err(errs.join("\n"));
    }
    Ok(ok)
}

pub fn capture_arm_style() -> u8 {
    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return 1;
    };
    gdm.main_player_game_data
        .as_ref()
        .equipment
        .chr_asm
        .equipment
        .arm_style as u8
}
