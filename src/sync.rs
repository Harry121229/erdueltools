//! 同步流水线（阶段严格分开）：
//!   1. 加点 —— TGA 写 PlayerGameData
//!   2. 卸装 —— TGA EquipGear（徒手/裸装）
//!   3. 清栏 —— RemoveItem
//!   4. 发放 —— TGA ItemGive（含缺的法术 Goods）
//!   5. 短暂 settle
//!   6. 穿戴 —— EquipGear
//!   7. 法术轮盘 —— EquipMagicData
//!   8. 共持 + 单端发送原生装备快照刷新对端外观

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use eldenring::cs::GameDataMan;
use fromsoftware_shared::FromStatic;

use crate::equip::LoadoutSlot;
use crate::snap::{BagItem, BuildSnapshot};
use crate::{equip, game, magic, net_appear, notify, paths, snap, stats, tga};

/// 每帧吞吐（RemoveItem / ItemGive / EquipGear 均为同步调用，可同帧连跑）
const REMOVE_PER_FRAME: u32 = 96;
const GIVE_PER_FRAME: u32 = 48;
const EQUIP_PER_FRAME: u32 = 32;
/// 每批 ItemGive 上限与 TGA 一致
const GIVE_BATCH: usize = 10;
const SETTLE_FRAMES: u32 = 3;
const STALL_LIMIT: u32 = 8;

enum Phase {
    Purge {
        stall: u32,
        last_remaining: u32,
        pending_give: VecDeque<BagItem>,
        loadout: Vec<LoadoutSlot>,
    },
    Give {
        queue: VecDeque<BagItem>,
        placed: Vec<(u32, i64, u32)>,
        loadout: Vec<LoadoutSlot>,
    },
    Settle {
        left: u32,
        loadout: Vec<LoadoutSlot>,
    },
    Equip {
        queue: VecDeque<LoadoutSlot>,
        used: HashSet<usize>,
        done: u32,
        finalize_loadout: Vec<LoadoutSlot>,
    },
}

struct Job {
    phase: Phase,
    purge_only: bool,
    removed: u32,
    given: u32,
    equipped: u32,
    magic: Vec<i32>,
    arm_style: u8,
}

static JOB: Mutex<Option<Job>> = Mutex::new(None);

pub fn is_busy() -> bool {
    JOB.lock().map(|g| g.is_some()).unwrap_or(false)
}

fn remaining() -> u32 {
    let Ok(gdm) = (unsafe { GameDataMan::instance_mut() }) else {
        return 0;
    };
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;
    game::managed_entry_count(inv)
}

fn next_target() -> Option<(u32, u32, u32)> {
    let gdm = unsafe { GameDataMan::instance_mut() }.ok()?;
    let inv = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .equip_inventory_data;
    game::first_managed(inv)
}

pub fn start(snap_data: BuildSnapshot) -> Result<String, String> {
    if is_busy() {
        return Err(crate::i18n::t().sync_still_running.into());
    }
    if !game::remove_item_ready() {
        return Err(crate::i18n::t().remove_item_missing.into());
    }
    if !game::item_give_ready() {
        return Err(crate::i18n::t().item_give_missing.into());
    }
    if !equip::equip_gear_ready() {
        return Err(crate::i18n::t().equip_gear_missing.into());
    }
    game::set_item_popup_suppressed(false);

    match snap_data.stats.as_ref() {
        Some(s) => {
            paths::stage("sync_stats");
            stats::apply(s)?;
        }
        None => {
            return Err(crate::i18n::t().no_stats.into());
        }
    }

    if snap_data.loadout.is_empty() {
        return Err(crate::i18n::t().no_loadout.into());
    }

    paths::stage("sync_unequip");
    equip::unequip_all()?;

    let expanded = snap::expand_copies(&snap_data.bag);
    let magic_slots = snap_data.magic;
    let arm_style = snap_data.arm_style;
    let mut pending: Vec<BagItem> = magic::missing_goods(&magic_slots);
    pending.extend(expanded);
    let loadout = snap_data.loadout;
    let pending_give: VecDeque<BagItem> = pending.into_iter().collect();
    let left = remaining();

    paths::stage(&format!("sync_purge_start_{}", left));

    if let Ok(mut g) = JOB.lock() {
        *g = Some(Job {
            phase: Phase::Purge {
                stall: 0,
                last_remaining: left + 1,
                pending_give,
                loadout,
            },
            purge_only: false,
            removed: 0,
            given: 0,
            equipped: 0,
            magic: magic_slots,
            arm_style,
        });
    }

    Ok(crate::i18n::fmt(crate::i18n::t().sync_plan, [left]))
}

/// F9：安全卸装后只清空武器、护甲和护符，不进入发放/穿戴阶段。
pub fn start_purge_only() -> Result<String, String> {
    if is_busy() {
        return Err(crate::i18n::t().sync_busy.into());
    }
    if !game::remove_item_ready() {
        return Err(crate::i18n::t().remove_item_missing.into());
    }
    if !game::item_give_ready() {
        return Err(crate::i18n::t().item_give_missing.into());
    }
    if !equip::equip_gear_ready() {
        return Err(crate::i18n::t().equip_gear_missing.into());
    }

    game::set_item_popup_suppressed(false);
    paths::stage("purge_only_unequip");
    equip::unequip_all()?;
    let left = remaining();
    if let Ok(mut guard) = JOB.lock() {
        *guard = Some(Job {
            phase: Phase::Purge {
                stall: 0,
                last_remaining: left + 1,
                pending_give: VecDeque::new(),
                loadout: Vec::new(),
            },
            purge_only: true,
            removed: 0,
            given: 0,
            equipped: 0,
            magic: Vec::new(),
            arm_style: 1,
        });
    }
    Ok(crate::i18n::fmt(crate::i18n::t().purge_start, [left]))
}

fn finish(job: &Job, placed: &[(u32, i64, u32)]) {
    let _ = (job, placed);
    game::set_item_popup_suppressed(false);
    stats::after_sync_care();
    paths::stage("sync_done");
}

pub fn poll() {
    stats::poll_care();
    net_appear::poll();

    let Ok(mut guard) = JOB.lock() else {
        return;
    };

    // 同帧可跨阶段推进（结算等待除外）
    for _ in 0..4 {
        let Some(job) = guard.as_mut() else {
            return;
        };

        match &mut job.phase {
            Phase::Purge {
                stall,
                last_remaining,
                pending_give,
                loadout,
            } => {
                let mut advance = false;
                for _ in 0..REMOVE_PER_FRAME {
                    let left = remaining();
                    if left == 0 {
                        advance = true;
                        break;
                    }

                    if left >= *last_remaining {
                        *stall += 1;
                        if *stall >= STALL_LIMIT {
                            let removed = job.removed;
                            let stuck = next_target()
                                .map(|(_, id, _)| format!("0x{:08X}", id))
                                .unwrap_or_else(|| crate::i18n::t().unknown.into());
                            game::set_item_popup_suppressed(false);
                            *guard = None;
                            drop(guard);
                            paths::stage("sync_stalled");
                            notify::say(&crate::i18n::fmt(
                                crate::i18n::t().sync_stalled,
                                [removed.to_string(), left.to_string(), stuck],
                            ));
                            return;
                        }
                    } else {
                        *stall = 0;
                    }
                    *last_remaining = left;

                    let Some((abs, _id, qty)) = next_target() else {
                        continue;
                    };
                    if game::remove_at(abs, qty).is_err() {
                        let removed = job.removed;
                        game::set_item_popup_suppressed(false);
                        *guard = None;
                        drop(guard);
                        notify::say(&crate::i18n::fmt(crate::i18n::t().delete_failed, [removed]));
                        return;
                    }
                    job.removed += 1;
                }

                if advance || remaining() == 0 {
                    if job.purge_only {
                        let removed = job.removed;
                        game::set_item_popup_suppressed(false);
                        *guard = None;
                        drop(guard);
                        paths::stage("purge_only_done");
                        notify::say(&crate::i18n::fmt(crate::i18n::t().purge_done, [removed]));
                        return;
                    }
                    paths::stage("sync_give_start");
                    game::set_item_popup_suppressed(true);
                    let queue = std::mem::take(pending_give);
                    let loadout = std::mem::take(loadout);
                    job.phase = Phase::Give {
                        queue,
                        placed: Vec::new(),
                        loadout,
                    };
                    continue;
                }
                return;
            }

            Phase::Give {
                queue,
                placed,
                loadout,
            } => {
                let mut done_give = false;
                let mut given_this_frame = 0u32;
                while given_this_frame < GIVE_PER_FRAME {
                    if queue.is_empty() {
                        done_give = true;
                        break;
                    }
                    let take = (GIVE_BATCH as u32)
                        .min(GIVE_PER_FRAME - given_this_frame)
                        .min(queue.len() as u32) as usize;
                    let batch: Vec<BagItem> = queue.drain(..take).collect();
                    match tga::give_batch(&batch) {
                        Ok(results) => {
                            for (item, (final_id, gem)) in batch.iter().zip(results.into_iter()) {
                                let gem_i = if gem == 0xFFFF_FFFF { -1 } else { gem as i64 };
                                placed.push((final_id, gem_i, item.sort_id));
                                job.given += 1;
                                given_this_frame += 1;
                            }
                        }
                        Err(e) => {
                            let (removed, given) = (job.removed, job.given);
                            game::set_item_popup_suppressed(false);
                            *guard = None;
                            drop(guard);
                            notify::say(&crate::i18n::fmt(
                                crate::i18n::t().give_failed,
                                [e, removed.to_string(), given.to_string()],
                            ));
                            return;
                        }
                    }
                }

                if done_give || queue.is_empty() {
                    paths::stage("sync_settle");
                    let loadout = std::mem::take(loadout);
                    let placed = std::mem::take(placed);
                    snap::stamp_sort_ids(&placed);
                    job.phase = Phase::Settle {
                        left: SETTLE_FRAMES,
                        loadout,
                    };
                    // 结算必须跨帧，不能同帧接着穿
                    return;
                }
                return;
            }

            Phase::Settle { left, loadout } => {
                if *left > 0 {
                    *left -= 1;
                    return;
                }
                paths::stage("sync_equip_start");
                let loadout = std::mem::take(loadout);
                let finalize_loadout = loadout.clone();
                let ordered = equip::sort_loadout_for_apply(&loadout);
                let queue: VecDeque<LoadoutSlot> = ordered.into_iter().collect();
                job.phase = Phase::Equip {
                    queue,
                    used: HashSet::new(),
                    done: 0,
                    finalize_loadout,
                };
                continue;
            }

            Phase::Equip {
                queue,
                used,
                done,
                finalize_loadout,
            } => {
                for _ in 0..EQUIP_PER_FRAME {
                    let Some(slot) = queue.pop_front() else {
                        job.equipped = *done;
                        let cleaned = equip::purge_placeholders();
                        paths::stage(&format!("sync_clean_fist_{}", cleaned));
                        if !job.magic.is_empty() {
                            paths::stage("sync_magic");
                            let _ = magic::apply(&job.magic);
                        }
                        equip::reset_arm_style_one_handed();
                        equip::apply_arm_style(job.arm_style);
                        let _ = equip::apply_saved_accessory_ordinals(finalize_loadout);
                        paths::stage("sync_weapon_refresh");
                        net_appear::request_after_sync();
                        let summary = Job {
                            phase: Phase::Settle {
                                left: 0,
                                loadout: Vec::new(),
                            },
                            purge_only: false,
                            removed: job.removed,
                            given: job.given,
                            equipped: job.equipped,
                            magic: Vec::new(),
                            arm_style: job.arm_style,
                        };
                        *guard = None;
                        drop(guard);
                        finish(&summary, &[]);
                        return;
                    };

                    match equip::apply_one(&slot, used) {
                        Ok(_) => {
                            *done += 1;
                        }
                        Err(e) => {
                            paths::stage(&format!("sync_equip_skip_{}_{}", slot.slot, e));
                        }
                    }
                }
                return;
            }
        }
    }
}
