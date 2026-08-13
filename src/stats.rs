//! 加点读写 + 同步后护理（严格清异常 / 回血蓝精 / 大卢恩，F9 开关）

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use eldenring::cs::{
    ChrInsExt, EquipParamGoods, GameDataMan, PlayerIns, SoloParamRepository, SpEffectParam,
};
use fromsoftware_shared::FromStatic;
use serde::{Deserialize, Serialize};

use crate::library;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct StatsSnapshot {
    pub level: u32,
    pub vigor: u32,
    pub mind: u32,
    pub endurance: u32,
    pub strength: u32,
    pub dexterity: u32,
    pub intelligence: u32,
    pub faith: u32,
    pub arcane: u32,
    pub archetype: u8,
}

impl StatsSnapshot {
    pub fn describe(&self) -> String {
        format!(
            "Lv{}  Vig{} Min{} End{} Str{} Dex{} Int{} Fai{} Arc{}  出身{}",
            self.level,
            self.vigor,
            self.mind,
            self.endurance,
            self.strength,
            self.dexterity,
            self.intelligence,
            self.faith,
            self.arcane,
            self.archetype
        )
    }
}

pub fn post_sync_care_enabled() -> bool {
    library::care_enabled()
}

/// 返回切换后的状态，并持久化到本地配置。
pub fn toggle_post_sync_care() -> bool {
    let next = !post_sync_care_enabled();
    let _ = library::set_care_enabled(next);
    if !next {
        CLEAR_LEFT.store(0, Ordering::Relaxed);
        VITALS_LEFT.store(0, Ordering::Relaxed);
    }
    next
}

pub fn capture() -> Result<StatsSnapshot, String> {
    let gdm = unsafe { GameDataMan::instance() }.map_err(|_| "GameDataMan 未就绪")?;
    let p = gdm.main_player_game_data.as_ref();
    Ok(StatsSnapshot {
        level: p.level,
        vigor: p.vigor,
        mind: p.mind,
        endurance: p.endurance,
        strength: p.strength,
        dexterity: p.dexterity,
        intelligence: p.intelligence,
        faith: p.faith,
        arcane: p.arcane,
        archetype: p.archetype,
    })
}

pub fn apply(s: &StatsSnapshot) -> Result<(), String> {
    let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
    let p = gdm.main_player_game_data.as_mut();

    p.level = s.level;
    p.vigor = s.vigor;
    p.mind = s.mind;
    p.endurance = s.endurance;
    p.strength = s.strength;
    p.dexterity = s.dexterity;
    p.intelligence = s.intelligence;
    p.faith = s.faith;
    p.arcane = s.arcane;
    p.archetype = s.archetype;

    p.effective_vigor = s.vigor;
    p.effective_mind = s.mind;
    p.effective_endurance = s.endurance;
    p.effective_strength = s.strength;
    p.effective_dexterity = s.dexterity;
    p.effective_intelligence = s.intelligence;
    p.effective_faith = s.faith;
    p.effective_arcane = s.arcane;

    Ok(())
}

const BOLUS_GOODS: [u32; 7] = [900, 910, 920, 930, 940, 950, 960];
/// 0毒 1红腐 2出血 3咒死 4霜 5眠 6狂
const DEATH_GAUGE: usize = 3;
/// 严格清异常：连续多帧防 Speffect 同帧回写
const CLEAR_HOLD_FRAMES: u32 = 90;
/// 回血蓝精：30 帧内持续写满，满了提前停
const VITALS_HOLD_FRAMES: u32 = 30;

static CLEAR_LEFT: AtomicU32 = AtomicU32::new(0);
static VITALS_LEFT: AtomicU32 = AtomicU32::new(0);
static CURSE_CURE_IDS: OnceLock<Vec<i32>> = OnceLock::new();

/// 同步完成后若护理开启：启动严格清异常 + 30 帧回血 + 开大卢恩
pub fn after_sync_care() {
    if !post_sync_care_enabled() {
        return;
    }
    let _ = clear_status_buildup();
    VITALS_LEFT.store(VITALS_HOLD_FRAMES, Ordering::Relaxed);
    let _ = restore_vitals();
    activate_great_rune();
}

/// 每帧：清异常剩余帧 + 回血剩余帧
pub fn poll_care() {
    if !post_sync_care_enabled() {
        return;
    }
    poll_status_clear();
    poll_vitals();
}

/// 换 BD 后调用：立刻清一次，并挂 90 帧持续清
pub fn clear_status_buildup() -> Result<(), String> {
    CLEAR_LEFT.store(CLEAR_HOLD_FRAMES, Ordering::Relaxed);
    clear_status_buildup_once()
}

fn poll_status_clear() {
    let left = CLEAR_LEFT.load(Ordering::Relaxed);
    if left == 0 {
        return;
    }
    let _ = clear_status_buildup_once();
    CLEAR_LEFT.store(left.saturating_sub(1), Ordering::Relaxed);
}

fn poll_vitals() {
    let left = VITALS_LEFT.load(Ordering::Relaxed);
    if left == 0 {
        return;
    }
    let _ = restore_vitals();
    if vitals_are_full() {
        VITALS_LEFT.store(0, Ordering::Relaxed);
        return;
    }
    VITALS_LEFT.store(left.saturating_sub(1), Ordering::Relaxed);
}

/// 激活大卢恩（卢恩弯弧）
pub fn activate_great_rune() {
    let Ok(gdm) = (unsafe { GameDataMan::instance_mut() }) else {
        return;
    };
    gdm.award_phantom_great_rune_requested = false;
    let pgd = gdm.main_player_game_data.as_mut();
    pgd.rune_arc_active = true;
    pgd.frontend_flags.set_rune_arc_active(true);
}

fn clear_status_buildup_once() -> Result<(), String> {
    if let Ok(player) = unsafe { PlayerIns::local_player_mut() } {
        expire_status_speffects(player);
        reset_resist_module(player);
    }
    reset_player_game_data_gauges()?;
    Ok(())
}

fn curse_cure_ids() -> &'static [i32] {
    CURSE_CURE_IDS.get_or_init(|| {
        let mut ids = Vec::new();
        let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
            return ids;
        };
        for (id, row) in repo.rows::<SpEffectParam>() {
            if row.change_curse_resist_point() <= 0 || row.effect_endurance() > 1.0 {
                continue;
            }
            if row.change_poison_resist_point() == 0
                && row.change_disease_resist_point() == 0
                && row.change_blood_resist_point() == 0
            {
                ids.push(id as i32);
            }
        }
        ids
    })
}

fn apply_bolus_speffects(player: &mut PlayerIns) {
    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        return;
    };
    for gid in BOLUS_GOODS {
        if let Some(g) = repo.get::<EquipParamGoods>(gid) {
            for sp in [g.ref_id_default(), g.ref_id_1()] {
                if sp > 0 {
                    player.apply_speffect(sp, false);
                }
            }
        }
    }
    for &id in curse_cure_ids() {
        if id > 0 {
            player.apply_speffect(id, false);
        }
    }
}

fn expire_status_speffects(player: &mut PlayerIns) {
    let ptrs: Vec<*mut eldenring::cs::SpecialEffectEntry> = player
        .chr_ins
        .special_effect
        .as_ref()
        .entries()
        .map(|e| e as *const _ as *mut _)
        .collect();

    let mut remove_ids = Vec::new();
    for ptr in &ptrs {
        if ptr.is_null() {
            continue;
        }
        unsafe {
            let e = &mut **ptr;
            let Some(row) = e.param_data.as_ref().map(|p| p.as_ref()) else {
                continue;
            };
            let status = row.curse_attack_power() > 0
                || row.poizon_attack_power() > 0
                || row.disease_attack_power() > 0
                || row.blood_attack_power() > 0
                || row.freeze_attack_power() > 0
                || row.sleep_attack_power() > 0
                || row.madness_attack_power() > 0;
            if status {
                e.removal_timer = 0.0;
                e.duration = 0.0;
                remove_ids.push(e.param_id);
            }
        }
    }
    for id in remove_ids {
        player.remove_speffect(id);
    }
}

/// CSChrResistModule：u32 remaining@+0x10 / max@+0x2C，float 累计@+0x48
fn reset_resist_module(player: &mut PlayerIns) {
    let modules = player.chr_ins.modules.as_ptr() as *mut u8;
    let resist = unsafe { *(modules.add(0x20) as *const usize) };
    if resist == 0 {
        return;
    }
    unsafe {
        let cur = std::slice::from_raw_parts_mut((resist + 0x10) as *mut u32, 7);
        let max = std::slice::from_raw_parts((resist + 0x2C) as *const u32, 7);
        for i in 0..7 {
            cur[i] = if max[i] > 0 { max[i] } else { 9999 };
        }
        let buildup = std::slice::from_raw_parts_mut((resist + 0x48) as *mut f32, 7);
        for b in buildup.iter_mut() {
            *b = 0.0;
        }
        cur[DEATH_GAUGE] = max[DEATH_GAUGE].max(9999);
        buildup[DEATH_GAUGE] = 0.0;
    }
}

fn reset_player_game_data_gauges() -> Result<(), String> {
    let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
    let p = gdm.main_player_game_data.as_mut();
    for i in 0..7 {
        let m = p.resistance_gauge_max[i];
        p.resistance_gauges[i] = if m > 0 { m } else { 9999 };
    }
    p.resistance_gauges[DEATH_GAUGE] = p.resistance_gauge_max[DEATH_GAUGE].max(9999);
    p.proc_status_timers.fill(0.0);
    p.proc_status_timer_max.fill(0.0);

    unsafe {
        let max_ptr = p.resistance_gauge_max.as_ptr() as *mut u8;
        let floats = max_ptr.add(7 * 4) as *mut f32;
        for i in 0..7 {
            *floats.add(i) = 0.0;
        }
    }
    Ok(())
}

/// 回满血 / 蓝 / 精力（PlayerGameData + ChrData）
pub fn restore_vitals() -> Result<(), String> {
    {
        let gdm = unsafe { GameDataMan::instance_mut() }.map_err(|_| "GameDataMan 未就绪")?;
        let p = gdm.main_player_game_data.as_mut();
        if p.current_max_hp > 0 {
            p.current_hp = p.current_max_hp;
        }
        if p.current_max_fp > 0 {
            p.current_fp = p.current_max_fp;
        }
        if p.current_max_stamina > 0 {
            p.current_stamina = p.current_max_stamina;
        }
    }
    if let Ok(player) = unsafe { PlayerIns::local_player_mut() } {
        let data = player.chr_ins.modules.data.as_mut();
        if data.max_hp > 0 {
            data.hp = data.max_hp;
            data.recoverable_hp = data.max_hp as f32;
        }
        if data.max_fp > 0 {
            data.fp = data.max_fp;
        }
        if data.max_stamina > 0 {
            data.stamina = data.max_stamina;
        }
    }
    Ok(())
}

fn vitals_are_full() -> bool {
    if let Ok(player) = unsafe { PlayerIns::local_player_mut() } {
        let data = player.chr_ins.modules.data.as_ref();
        if data.max_hp > 0 && data.hp < data.max_hp {
            return false;
        }
        if data.max_fp > 0 && data.fp < data.max_fp {
            return false;
        }
        if data.max_stamina > 0 && data.stamina < data.max_stamina {
            return false;
        }
        return true;
    }
    if let Ok(gdm) = unsafe { GameDataMan::instance() } {
        let p = gdm.main_player_game_data.as_ref();
        return (p.current_max_hp == 0 || p.current_hp >= p.current_max_hp)
            && (p.current_max_fp == 0 || p.current_fp >= p.current_max_fp)
            && (p.current_max_stamina == 0 || p.current_stamina >= p.current_max_stamina);
    }
    false
}
