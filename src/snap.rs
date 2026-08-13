//! 物品栏快照：武器 / 护甲 / 护符，存成 TGA / 构筑规划器同款五元组。
//!
//! ```text
//! { id, qty, reinforce, upgrade, gem }
//! ```
//! 见 `398802fd1d4c67.CT` 与 `ER_TGA_v1.18.0.CT` ItemGive。

use eldenring::cs::{
    CSGaitemImp, CSGaitemInsSubclass, EquipParamWeapon, GameDataMan, ItemCategory, ItemId,
    SoloParamRepository,
};
use fromsoftware_shared::FromStatic;
use serde::{Deserialize, Serialize};

use crate::equip::LoadoutSlot;
use crate::paths;
use crate::stats::StatsSnapshot;
use crate::tga;

pub const SLOT_COUNT: usize = 3;

const AMMO_WEP_TYPES: std::ops::RangeInclusive<u16> = 81..=86;
const AMMO_PARAM_ID_FLOOR: u32 = 50_000_000;

pub fn is_ammo(id: ItemId) -> bool {
    if id.category() != ItemCategory::Weapon {
        return false;
    }
    if let Ok(repo) = unsafe { SoloParamRepository::instance() } {
        let base = (id.param_id() / 100) * 100;
        if let Some(row) = repo.get::<EquipParamWeapon>(base) {
            return AMMO_WEP_TYPES.contains(&row.wep_type());
        }
    }
    id.param_id() >= AMMO_PARAM_ID_FLOOR
}

pub fn is_managed(id: ItemId) -> bool {
    if !matches!(
        id.category(),
        ItemCategory::Weapon | ItemCategory::Protector | ItemCategory::Accessory
    ) {
        return false;
    }
    if id.param_id() == 0 {
        return false;
    }
    !is_ammo(id)
}

/// 与 TGA `ItemGive({ id, qty, reinforce, upgrade, gem })` 同形。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BagItem {
    /// 带类别前缀。武器是 **未强化、未加亲和** 的基础 id（规划器写法）。
    pub id: u32,
    /// 份数。旧存档会把多份合成一条 qty>1；发放时必须拆开。
    pub qty: u32,
    /// 强化等级 0..=25；−1 = 用角色当前匹配武器等级（TGA 行为）
    #[serde(default)]
    pub reinforce: i32,
    /// 亲和偏移 0/100/…/1200；−1 = 不加
    #[serde(default)]
    pub upgrade: i32,
    /// 灰烬的完整 item id（`0x8.......`）；−1 = 无
    #[serde(default = "default_gem")]
    pub gem: i64,
    /// 物品栏获得顺序（UI「按获得顺序」靠它排）。0 表示未记录。
    #[serde(default)]
    pub sort_id: u32,
}

fn default_gem() -> i64 {
    -1
}

impl BagItem {
    /// 成品 id 原样发放（强化/亲和已烘焙进 id），只挂灰烬。
    pub fn exact(id: u32, qty: u32, gem: i64) -> Self {
        Self {
            id,
            qty: qty.max(1),
            reinforce: 0,
            upgrade: 0,
            gem,
            sort_id: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildSnapshot {
    pub version: u32,
    #[serde(default)]
    pub bag: Vec<BagItem>,
    /// 14 格穿戴，与 bag 分开存、分开装
    #[serde(default)]
    pub loadout: Vec<LoadoutSlot>,
    /// 法术轮盘 14 格（Magic param id；空 = -1）。旧存档缺省为空。
    #[serde(default)]
    pub magic: Vec<i32>,
    /// 共持：0 空手 / 1 单持 / 2 左手共持 / 3 右手共持（ChrAsmArmStyle）
    #[serde(default = "default_arm_style")]
    pub arm_style: u8,
    /// TGA Attributes 同款
    #[serde(default)]
    pub stats: Option<StatsSnapshot>,
}

fn default_arm_style() -> u8 {
    1 // OneHanded
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BagCounts {
    pub weapons: u32,
    pub armor: u32,
    pub talismans: u32,
}

impl BagCounts {
    pub fn describe(&self) -> String {
        format!(
            "武器 {} / 护甲 {} / 护符 {}",
            self.weapons, self.armor, self.talismans
        )
    }
}

pub fn game_ready() -> bool {
    unsafe { GameDataMan::instance() }.is_ok()
}

/// 从 gaitem 句柄读出挂着的灰烬 item id；没有则 −1。
pub fn gem_of_handle(handle: &eldenring::cs::GaitemHandle) -> i64 {
    let Ok(gaitems) = (unsafe { CSGaitemImp::instance() }) else {
        return -1;
    };
    let Some(ins) = gaitems.gaitem_ins_by_handle(handle) else {
        return -1;
    };
    match ins.into() {
        CSGaitemInsSubclass::CSWepGaitemIns(wep) => {
            let gh = wep.gem_slot_table.gem_slots[0].gaitem_handle;
            if gh.0 == 0 {
                return -1;
            }
            let Some(gem_ins) = gaitems.gaitem_ins_by_handle(&gh) else {
                return -1;
            };
            match gem_ins.item_id.as_valid() {
                Some(id) => id.into_inner() as i64,
                None => -1,
            }
        }
        _ => -1,
    }
}

/// 徒手（不算装备）
const FIST_PARAM: u32 = 110_000;

pub fn capture() -> Result<BuildSnapshot, String> {
    let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;

    // 只扫普通物品栏，按 sort_id 排成「从上到下」（获得顺序）
    let mut bag: Vec<BagItem> = Vec::new();
    for entry in inv
        .items_data
        .normal_entries()
        .iter()
        .filter_map(|s| s.as_option())
    {
        if !is_managed(entry.item_id) {
            continue;
        }
        // 徒手实例会混在物品栏里，不同步
        if entry.item_id.category() == ItemCategory::Weapon
            && entry.item_id.param_id() == FIST_PARAM
        {
            continue;
        }

        let full = entry.item_id.into_inner();
        let gem = gem_of_handle(&entry.gaitem_handle);
        let sort_id = entry.sort_id;

        // 一格多少份就拆成多少条，每条 qty=1 —— 和规划器 CT 写法一致
        let copies = entry.quantity.max(1);
        for _ in 0..copies {
            let mut item = match entry.item_id.category() {
                ItemCategory::Weapon => tga::decode_weapon(full, gem),
                ItemCategory::Protector | ItemCategory::Accessory => BagItem::exact(full, 1, -1),
                _ => continue,
            };
            item.qty = 1;
            item.sort_id = sort_id;
            bag.push(item);
        }
    }

    bag.sort_by(|a, b| a.sort_id.cmp(&b.sort_id));

    // 穿戴 / 法术 / 加点必须写进存档，静默失败会导致「同步了但没换衣服没加点」
    let loadout = crate::equip::capture_loadout()?;
    let magic = crate::magic::capture()?;
    let arm_style = crate::equip::capture_arm_style();
    let stats = crate::stats::capture()?;

    Ok(BuildSnapshot {
        version: 11,
        bag,
        loadout,
        magic,
        arm_style,
        stats: Some(stats),
    })
}

/// 发放前把旧存档的 `qty>1` 拆成多条 qty=1。
///
/// TGA / 游戏对武器护甲护符都是「一次一例」；把 qty 写进发包只会得到一件。
pub fn expand_copies(bag: &[BagItem]) -> Vec<BagItem> {
    let mut out = Vec::new();
    for (i, it) in bag.iter().enumerate() {
        let Ok(id) = ItemId::try_from(it.id) else {
            continue;
        };
        if id.category() == ItemCategory::Weapon && id.param_id() == FIST_PARAM {
            continue;
        }
        if is_ammo(id) {
            continue;
        }

        let n = match id.category() {
            ItemCategory::Weapon | ItemCategory::Protector | ItemCategory::Accessory => {
                it.qty.max(1)
            }
            _ => it.qty.max(1),
        };

        for k in 0..n {
            let mut one = *it;
            one.qty = 1;
            // 旧存档 sort_id 全是 0：按展开后的顺序补一份，保留相对顺序
            if one.sort_id == 0 {
                one.sort_id = ((i as u32 + 1) * 1000).saturating_add(k);
            }
            out.push(one);
        }
    }
    out
}

/// 发放完成后，按保存时的顺序写回 `sort_id`，UI「按获得顺序」才会一致。
pub fn stamp_sort_ids(given: &[(u32, i64, u32)]) {
    let Ok(gdm) = (unsafe { GameDataMan::instance_mut() }) else {
        return;
    };
    let inventory = &mut gdm
        .main_player_game_data
        .as_mut()
        .equipment
        .equip_inventory_data;
    let items = &mut inventory.items_data;

    let mut used = vec![false; items.normal_items_capacity as usize];

    for &(want_id, want_gem, sort_id) in given {
        for (i, slot) in items.normal_entries_mut().iter_mut().enumerate() {
            if used.get(i).copied().unwrap_or(true) {
                continue;
            }
            let Some(entry) = slot.as_option_mut() else {
                continue;
            };
            if entry.item_id.into_inner() != want_id {
                continue;
            }
            if want_gem >= 0 && gem_of_handle(&entry.gaitem_handle) != want_gem {
                continue;
            }
            entry.sort_id = sort_id;
            used[i] = true;
            break;
        }
    }

    // 手工恢复 sort_id 后，游戏自己的分配计数器不会自动跟进。
    // 若不修正，从木箱取出或新获得的物品会拿到较小/重复的 ID，
    // 在“按获得顺序”排序下无法出现在第一位。
    let next = items
        .normal_entries()
        .iter()
        .filter_map(|slot| slot.as_option())
        .map(|entry| entry.sort_id)
        .filter(|sort_id| *sort_id != u32::MAX)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    inventory.next_sort_id = next;
}

fn count_bag<'a>(items: impl Iterator<Item = &'a BagItem>) -> BagCounts {
    let mut c = BagCounts::default();
    for b in items {
        let Ok(id) = ItemId::try_from(b.id) else {
            continue;
        };
        if id.category() == ItemCategory::Weapon && is_ammo(id) {
            continue;
        }
        let n = b.qty.max(1);
        match id.category() {
            ItemCategory::Weapon => c.weapons += n,
            ItemCategory::Protector => c.armor += n,
            ItemCategory::Accessory => c.talismans += n,
            _ => {}
        }
    }
    c
}

pub fn snap_counts(snap: &BuildSnapshot) -> BagCounts {
    count_bag(snap.bag.iter())
}

pub fn live_counts() -> Result<BagCounts, String> {
    let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;
    let mut c = BagCounts::default();
    for e in inv.items_data.items() {
        if !is_managed(e.item_id) {
            continue;
        }
        match e.item_id.category() {
            ItemCategory::Weapon => c.weapons += 1,
            ItemCategory::Protector => c.armor += e.quantity.max(1),
            ItemCategory::Accessory => c.talismans += e.quantity.max(1),
            _ => {}
        }
    }
    Ok(c)
}

pub fn dump_inventory() -> Result<std::path::PathBuf, String> {
    let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
    let items = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data
        .items_data;

    let mut out = String::new();
    out.push_str(&format!(
        "普通格容量 {} 已用 {} / 钥匙格容量 {}\n\n",
        items.normal_items_capacity, items.normal_items_len, items.key_items_capacity
    ));
    out.push_str("普通格\t绝对索引\titem_id\t\t类别\t数量\t灰烬\t处理\n");

    for (i, slot) in items.normal_entries().iter().enumerate() {
        let Some(entry) = slot.as_option() else {
            continue;
        };
        let id = entry.item_id;
        let gem = gem_of_handle(&entry.gaitem_handle);
        out.push_str(&format!(
            "{}\t{}\t0x{:08X}\t{:?}\t{}\t{}\t{}\n",
            i,
            items.key_items_capacity as usize + i,
            id.into_inner(),
            id.category(),
            entry.quantity,
            if gem < 0 {
                "-".into()
            } else {
                format!("0x{:08X}", gem)
            },
            if is_managed(id) {
                "同步"
            } else if is_ammo(id) {
                "弹药-跳过"
            } else {
                "跳过"
            }
        ));
    }

    let path = paths::data_dir().join("inventory_dump.txt");
    std::fs::write(&path, out).map_err(|e| e.to_string())?;
    Ok(path)
}

fn slot_path(slot: usize) -> std::path::PathBuf {
    paths::data_dir().join(format!("bd{}.json", slot + 1))
}

pub fn save_slot(slot: usize, snap: &BuildSnapshot) -> Result<(), String> {
    if slot >= SLOT_COUNT {
        return Err("槽位无效".into());
    }
    let text = serde_json::to_string_pretty(snap).map_err(|e| e.to_string())?;
    std::fs::write(slot_path(slot), text).map_err(|e| e.to_string())
}

pub fn load_slot(slot: usize) -> Result<BuildSnapshot, String> {
    if slot >= SLOT_COUNT {
        return Err("槽位无效".into());
    }
    let text = std::fs::read_to_string(slot_path(slot))
        .map_err(|_| format!("槽 {} 还没有存档", slot + 1))?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}
