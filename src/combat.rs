//! 按当前选中存档累计决斗击杀 / 阵亡，不依赖幻影颜色或队伍。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use eldenring::cs::{
    CSBulletManager, CSChrDataModule, ChrIns, FieldInsHandle, FieldInsType, PlayerIns, WorldChrMan,
};
use fromsoftware_shared::FromStatic;
use retour::RawDetour;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::core::s;

#[derive(Clone, Copy, PartialEq)]
enum Attribution {
    Local,
    Other,
    Unknown,
}

struct PeerTrack {
    alive: bool,
    hp: i32,
}

#[derive(Clone, Copy)]
struct DamageOwner {
    selector: u32,
    chr: usize,
    data: usize,
}

/// 手动丢弃当前个人击杀归属目标。
pub fn clear_tracked_target() -> bool {
    let Ok(mut guard) = TRACK.lock() else {
        return false;
    };
    let Some(track) = guard.as_mut() else {
        return false;
    };
    track.tracked_target.take().is_some()
}

fn player_display_name(player: &PlayerIns) -> String {
    let character_name = unsafe { player.player_game_data.as_ref() }.character_name();
    if !character_name.trim().is_empty() {
        return character_name;
    }

    let entry = player.session_manager_player_entry.as_ptr();
    if !entry.is_null()
        && let Ok(steam_name) = unsafe { &*entry }.steam_name.to_string()
        && !steam_name.trim().is_empty()
    {
        return steam_name;
    }
    "未知玩家".to_owned()
}

#[derive(Default)]
struct CombatTrack {
    local_alive: Option<bool>,
    local_hp: Option<i32>,
    local_death_credited: bool,
    /// 本机最后一次实际打中的远程玩家；始终只保存一个。
    tracked_target: Option<u32>,
    peers: HashMap<u32, PeerTrack>,
    hp_credited: HashSet<u32>,
}

static TRACK: Mutex<Option<CombatTrack>> = Mutex::new(None);
static DEATH_EVENTS: Mutex<Vec<(u32, FieldInsHandle)>> = Mutex::new(Vec::new());
static DEATH_ORIG: AtomicUsize = AtomicUsize::new(0);
static DEATH_INSTALL_TICK: AtomicUsize = AtomicUsize::new(0);
static DEATH_HOOK: OnceLock<RawDetour> = OnceLock::new();
static IS_DEAD_ORIG: AtomicUsize = AtomicUsize::new(0);
static IS_DEAD_HOOK: OnceLock<RawDetour> = OnceLock::new();
static IS_DEAD_STATE: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
static DAMAGE_ORIG: AtomicUsize = AtomicUsize::new(0);
static DAMAGE_HOOK: OnceLock<RawDetour> = OnceLock::new();
static DAMAGE_OWNERS: Mutex<Option<HashMap<usize, DamageOwner>>> = Mutex::new(None);
static LOCAL_SELECTOR: AtomicUsize = AtomicUsize::new(0);
/// 当前 DamageChr 调用正在处理的受害者；BroadCast(type 38) 在该调用栈内读取。
static DAMAGE_CONTEXT_VICTIM: AtomicUsize = AtomicUsize::new(0);
static LOCAL_DAMAGE_TARGETS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

type DeathFn = unsafe extern "C" fn(*mut ChrIns);
type IsDeadFn = unsafe extern "C" fn(*mut ChrIns) -> u32;
type DamageFn = unsafe extern "C" fn(*mut (), *mut ChrIns, u64, u8);

unsafe extern "C" fn death_hook(chr: *mut ChrIns) {
    if let Some(chr) = unsafe { chr.as_ref() }
        && let Ok(mut events) = DEATH_EVENTS.lock()
    {
        events.push((chr.field_ins_handle.selector.0, chr.last_hit_by));
    }
    let original = DEATH_ORIG.load(Ordering::SeqCst);
    if original != 0 {
        let original: DeathFn = unsafe { std::mem::transmute(original) };
        unsafe { original(chr) };
    }
}

unsafe extern "C" fn is_dead_hook(chr: *mut ChrIns) -> u32 {
    let original = IS_DEAD_ORIG.load(Ordering::SeqCst);
    if original == 0 {
        return 0;
    }
    let original: IsDeadFn = unsafe { std::mem::transmute(original) };
    let result = unsafe { original(chr) };
    if let Some(chr) = unsafe { chr.as_ref() }
        && let Ok(mut guard) = IS_DEAD_STATE.lock()
    {
        let dead = guard.get_or_insert_with(HashSet::new);
        let selector = chr.field_ins_handle.selector.0;
        if result != 0 {
            if dead.insert(selector)
                && let Ok(mut events) = DEATH_EVENTS.lock()
            {
                events.push((selector, chr.last_hit_by));
            }
        } else {
            dead.remove(&selector);
        }
    }
    result
}

unsafe extern "C" fn damage_hook(module: *mut (), attacker: *mut ChrIns, damage: u64, flags: u8) {
    let owner = DAMAGE_OWNERS
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref()?.get(&(module as usize)).copied());
    let before = owner
        .and_then(|owner| unsafe { (owner.data as *const CSChrDataModule).as_ref() })
        .map(|data| data.hp);

    let original = DAMAGE_ORIG.load(Ordering::SeqCst);
    if original == 0 {
        return;
    }
    let original: DamageFn = unsafe { std::mem::transmute(original) };
    let previous_context = owner
        .map(|owner| DAMAGE_CONTEXT_VICTIM.swap(owner.selector as usize, Ordering::SeqCst))
        .unwrap_or(0);
    unsafe { original(module, attacker, damage, flags) };
    if owner.is_some() {
        DAMAGE_CONTEXT_VICTIM.store(previous_context, Ordering::SeqCst);
    }

    let Some(owner) = owner else {
        return;
    };
    let Some(before) = before else {
        return;
    };
    let Some(data) = (unsafe { (owner.data as *const CSChrDataModule).as_ref() }) else {
        return;
    };
    // 每次本机真正让某个远程玩家掉血，覆盖唯一的“当前目标”候选。
    let attacker_selector = unsafe {
        attacker
            .as_ref()
            .map(|chr| chr.field_ins_handle.selector.0)
            .unwrap_or(0)
    };
    if data.hp < before && attacker_selector == LOCAL_SELECTOR.load(Ordering::SeqCst) as u32 {
        if let Ok(mut targets) = LOCAL_DAMAGE_TARGETS.lock() {
            targets.push(owner.selector);
        }
    }
    // DamageChr 这里只认真实 HP<=0；发辫只由其专用网络触发包检测，
    // 禁止再用任何回血/HP 上升现象猜测。
    if before > 0 && data.hp <= 0 {
        let last_hit = unsafe {
            attacker
                .as_ref()
                .map(|chr| chr.field_ins_handle)
                .or_else(|| {
                    (owner.chr as *const ChrIns)
                        .as_ref()
                        .map(|chr| chr.last_hit_by)
                })
        };
        if let Some(last_hit) = last_hit
            && let Ok(mut events) = DEATH_EVENTS.lock()
        {
            events.push((owner.selector, last_hit));
        }
    }
}

/// 由本机 BroadCast(type 38) Hook 调用。只有发送发生在 DamageChr 调用栈内才会记录，
/// 因而能精确绑定受害者，不依赖轮询时间或房间人数。
pub fn note_local_damage_packet_target() {
    let selector = DAMAGE_CONTEXT_VICTIM.load(Ordering::SeqCst) as u32;
    if selector == 0 {
        return;
    }
    if let Ok(mut targets) = LOCAL_DAMAGE_TARGETS.lock() {
        targets.push(selector);
    }
}

/// 优先从 ERSE 导出的已解析函数槽安装死亡事件 Hook。
/// DEN Maps 的发辫也经过此原生入口，因此同帧复活不会漏记。
pub fn install_death_hook() {
    if DEATH_HOOK.get().is_some() {
        return;
    }
    const SYMBOL: windows::core::PCSTR = s!("?DoDeathStuffs@ChrIns@game@erse@@2P6AXPEAU123@@ZEA");
    let Ok(module) = (unsafe { GetModuleHandleA(s!("EldenRingScriptExtender.dll")) }) else {
        crate::paths::stage("combat_death_hook_erse_missing");
        return;
    };
    let Some(export) = (unsafe { GetProcAddress(module, SYMBOL) }) else {
        crate::paths::stage("combat_death_hook_export_missing");
        return;
    };
    let slot = export as *const () as *const usize;
    let target = unsafe { slot.read() };
    if target == 0 {
        crate::paths::stage("combat_death_hook_target_missing");
        return;
    }
    let Ok(detour) = (unsafe { RawDetour::new(target as *const (), death_hook as *const ()) })
    else {
        crate::paths::stage("combat_death_hook_create_failed");
        return;
    };
    DEATH_ORIG.store(detour.trampoline() as *const () as usize, Ordering::SeqCst);
    if unsafe { detour.enable() }.is_err() {
        DEATH_ORIG.store(0, Ordering::SeqCst);
        crate::paths::stage("combat_death_hook_enable_failed");
        return;
    }
    if DEATH_HOOK.set(detour).is_err() {
        crate::paths::stage("combat_death_hook_duplicate");
        return;
    }
    crate::paths::stage("combat_death_hook_ready");
}

/// `IsDead` 会在发辫同帧复活期间仍短暂返回 true，可捕获帧轮询看不到的死亡边沿。
pub fn install_is_dead_hook() {
    if IS_DEAD_HOOK.get().is_some() {
        return;
    }
    const SYMBOL: windows::core::PCSTR = s!("?IsDead@ChrIns@game@erse@@2P6AIPEAU123@@ZEA");
    let Ok(module) = (unsafe { GetModuleHandleA(s!("EldenRingScriptExtender.dll")) }) else {
        return;
    };
    let Some(export) = (unsafe { GetProcAddress(module, SYMBOL) }) else {
        return;
    };
    let target = unsafe { (export as *const () as *const usize).read() };
    if target == 0 {
        return;
    }
    let Ok(detour) = (unsafe { RawDetour::new(target as *const (), is_dead_hook as *const ()) })
    else {
        crate::paths::stage("combat_is_dead_hook_create_failed");
        return;
    };
    IS_DEAD_ORIG.store(detour.trampoline() as *const () as usize, Ordering::SeqCst);
    if unsafe { detour.enable() }.is_err() {
        IS_DEAD_ORIG.store(0, Ordering::SeqCst);
        crate::paths::stage("combat_is_dead_hook_enable_failed");
        return;
    }
    if IS_DEAD_HOOK.set(detour).is_err() {
        return;
    }
    crate::paths::stage("combat_is_dead_hook_ready");
}

/// 在 DEN Maps 的发辫处理路径外层观察伤害前后 HP，捕获被同帧复活吞掉的死亡。
pub fn install_damage_hook() {
    if DAMAGE_HOOK.get().is_some() {
        return;
    }
    const SYMBOL: windows::core::PCSTR =
        s!("?DamageChr@CSChrDamageModule@game@erse@@2P6AXPEAU123@PEAUChrIns@23@_KC@ZEA");
    let Ok(module) = (unsafe { GetModuleHandleA(s!("EldenRingScriptExtender.dll")) }) else {
        return;
    };
    let Some(export) = (unsafe { GetProcAddress(module, SYMBOL) }) else {
        return;
    };
    let target = unsafe { (export as *const () as *const usize).read() };
    if target == 0 {
        return;
    }
    let Ok(detour) = (unsafe { RawDetour::new(target as *const (), damage_hook as *const ()) })
    else {
        crate::paths::stage("combat_damage_hook_create_failed");
        return;
    };
    DAMAGE_ORIG.store(detour.trampoline() as *const () as usize, Ordering::SeqCst);
    if unsafe { detour.enable() }.is_err() {
        DAMAGE_ORIG.store(0, Ordering::SeqCst);
        crate::paths::stage("combat_damage_hook_enable_failed");
        return;
    }
    if DAMAGE_HOOK.set(detour).is_err() {
        return;
    }
    crate::paths::stage("combat_damage_hook_ready");
}

fn player_steam_lo(player: &eldenring::cs::PlayerIns) -> u32 {
    let entry = player.session_manager_player_entry.as_ptr();
    if entry.is_null() {
        return 0;
    }
    unsafe { (*entry).steam_id as u32 }
}

/// 子弹 last_hit 还原为发射者角色；子弹已销毁时保留原句柄供缓存兜底。
fn resolve_attacker_handle(handle: FieldInsHandle) -> FieldInsHandle {
    if handle.is_empty() || handle.selector.field_ins_type() != Some(FieldInsType::Bullet) {
        return handle;
    }
    let Ok(bullets) = (unsafe { CSBulletManager::instance_mut() }) else {
        return handle;
    };
    bullets
        .bullet_ins_by_handle(&handle)
        .map(|bullet| bullet.targeting_owner.owner_chr_handle)
        .unwrap_or(handle)
}

fn record(kills: u32, deaths: u32) {
    match crate::library::add_active_combat(kills, deaths) {
        Ok(Some(stats)) => crate::notify::say_long(&crate::i18n::fmt(
            crate::i18n::t().duel_record,
            [stats.kills, stats.deaths],
        )),
        Ok(None) => {}
        Err(error) => {
            crate::notify::say(&crate::i18n::fmt(crate::i18n::t().combat_save_failed, [&error]))
        }
    }
}

/// 游戏 `FrameBegin` 调用；读取 HP 边沿和 last_hit 归属。
pub fn poll() {
    // ERSE 在本 DLL 之后才会填入导出的函数槽；首次失败后按约每秒重试，
    // 直到真正装好 Hook，不能永久退回容易漏掉发辫同帧复活的 HP 采样。
    if (DEATH_HOOK.get().is_none() || IS_DEAD_HOOK.get().is_none() || DAMAGE_HOOK.get().is_none())
        && DEATH_INSTALL_TICK.fetch_add(1, Ordering::Relaxed) % 60 == 0
    {
        install_death_hook();
        install_is_dead_hook();
        install_damage_hook();
    }
    let Ok(world) = (unsafe { WorldChrMan::instance() }) else {
        return;
    };
    let Some(main) = world.main_player.as_ref() else {
        return;
    };
    let local_sel = main.chr_ins.field_ins_handle.selector.0;
    LOCAL_SELECTOR.store(local_sel as usize, Ordering::SeqCst);
    let local_hp = main.chr_ins.modules.data.hp;
    let local_alive = local_hp > 0;
    let mut damage_owners = HashMap::new();
    damage_owners.insert(
        main.chr_ins.modules.damage,
        DamageOwner {
            selector: local_sel,
            chr: &main.chr_ins as *const ChrIns as usize,
            data: main.chr_ins.modules.data.as_ptr() as usize,
        },
    );
    let mut death_events = DEATH_EVENTS
        .lock()
        .map(|mut events| std::mem::take(&mut *events))
        .unwrap_or_default();
    let mut packet_deaths = HashSet::new();
    for (selector, _, _) in crate::net_appear::take_remote_braid_triggers() {
        if selector != 0
            && let Some(player) = world
                .player_chr_set
                .characters()
                .find(|player| player.chr_ins.field_ins_handle.selector.0 == selector)
        {
            packet_deaths.insert(selector);
            death_events.push((selector, player.chr_ins.last_hit_by));
        }
    }

    // 先复制远程玩家帧快照，避免持有 TRACK 时遍历游戏容器。
    let mut snapshot = Vec::new();
    for player in world.player_chr_set.characters() {
        let sel = player.chr_ins.field_ins_handle.selector.0;
        if sel == local_sel {
            continue;
        }
        damage_owners.insert(
            player.chr_ins.modules.damage,
            DamageOwner {
                selector: sel,
                chr: &player.chr_ins as *const ChrIns as usize,
                data: player.chr_ins.modules.data.as_ptr() as usize,
            },
        );
        snapshot.push((
            sel,
            player_steam_lo(player),
            player.chr_ins.modules.data.hp,
            player.chr_ins.last_hit_by,
        ));
    }
    if let Ok(mut guard) = DAMAGE_OWNERS.lock() {
        *guard = Some(damage_owners);
    }

    let resolved: Vec<(u32, u32, i32, Attribution, u32, u32)> = snapshot
        .into_iter()
        .map(|(sel, steam, hp, last_hit)| {
            let attacker = resolve_attacker_handle(last_hit);
            let attribution = if attacker.is_empty() {
                Attribution::Unknown
            } else if attacker.selector.0 == local_sel {
                Attribution::Local
            } else if attacker.selector.field_ins_type() == Some(FieldInsType::Bullet) {
                Attribution::Unknown
            } else {
                Attribution::Other
            };
            (
                sel,
                steam,
                hp,
                attribution,
                last_hit.selector.0,
                attacker.selector.0,
            )
        })
        .collect();
    let latest_local_target = LOCAL_DAMAGE_TARGETS
        .lock()
        .ok()
        .and_then(|mut targets| std::mem::take(&mut *targets).into_iter().last());

    let Ok(mut guard) = TRACK.lock() else {
        return;
    };
    let track = guard.get_or_insert_with(CombatTrack::default);
    if latest_local_target
        .is_some_and(|target| target != local_sel && resolved.iter().any(|entry| entry.0 == target))
    {
        if track.tracked_target != latest_local_target {
            if let Some(target) = latest_local_target {
                let name = world
                    .player_chr_set
                    .characters()
                    .find(|player| player.chr_ins.field_ins_handle.selector.0 == target)
                    .map(|player| player_display_name(player))
                    .unwrap_or_else(|| crate::i18n::t().unknown_player.to_owned());
                crate::notify::say(&crate::i18n::fmt(crate::i18n::t().target_changed, [&name]));
            }
        }
        track.tracked_target = latest_local_target;
    }
    let mut deaths = 0;
    let mut credited_victims = HashSet::new();

    if track
        .local_hp
        .is_some_and(|previous| track.local_alive == Some(true) && local_hp < previous)
    {
        track.local_death_credited = false;
    }
    if let Some(was_alive) = track.local_alive {
        if was_alive && !local_alive {
            crate::net_appear::request_combat_capture("local_death");
            if !track.local_death_credited {
                deaths += 1;
                track.local_death_credited = true;
            }
        }
    }
    track.local_alive = Some(local_alive);
    track.local_hp = Some(local_hp);

    let mut kills = 0;
    for &(sel, steam, hp, _attribution, last_hit_raw, attacker_raw) in &resolved {
        let alive = hp > 0;
        // 复活后的下一次受伤代表新一轮交战，此时才重新武装 HP 死亡计分。
        // 这样晚到的发辫状态包仍能消耗上一轮标记，不依赖等待时间。
        if track
            .peers
            .get(&sel)
            .is_some_and(|peer| peer.alive && hp < peer.hp)
        {
            track.hp_credited.remove(&sel);
        }
        let peer = track.peers.entry(sel).or_insert(PeerTrack { alive, hp });
        peer.hp = hp;

        let was_alive = peer.alive;
        peer.alive = alive;
        if was_alive && !alive {
            let mine = track.tracked_target == Some(sel);
            crate::net_appear::request_combat_capture(&if mine {
                format!(
                    "remote_death_mine local={local_sel:#x} last_hit={last_hit_raw:#x} attacker={attacker_raw:#x}"
                )
            } else {
                format!(
                    "remote_death_other local={local_sel:#x} last_hit={last_hit_raw:#x} attacker={attacker_raw:#x}"
                )
            });
            if mine {
                let victim = if steam != 0 { steam } else { sel };
                if credited_victims.insert(victim) {
                    kills += 1;
                }
                track.hp_credited.insert(sel);
                track.tracked_target = None;
            }
        }
    }
    for (victim_sel, last_hit) in death_events {
        if victim_sel == local_sel {
            if !track.local_death_credited {
                crate::net_appear::request_combat_capture("local_death_native");
                deaths += 1;
                track.local_death_credited = true;
            }
            continue;
        }
        let Some(&(_, steam, _, _, _, _)) = resolved.iter().find(|entry| entry.0 == victim_sel)
        else {
            continue;
        };
        let attacker = resolve_attacker_handle(last_hit);
        let attribution = if attacker.is_empty() {
            Attribution::Unknown
        } else if attacker.selector.0 == local_sel {
            Attribution::Local
        } else if attacker.selector.field_ins_type() == Some(FieldInsType::Bullet) {
            Attribution::Unknown
        } else {
            Attribution::Other
        };
        // 保留旧统计路径，只强化死亡身份：任何证据只能处理它实际携带的
        // victim_sel，且最终只有与当前目标完全相同的 selector 才能加分。
        let direct_local_damage = attribution == Attribution::Local;
        let is_tracked_target = track.tracked_target == Some(victim_sel);
        if !direct_local_damage && !is_tracked_target && !packet_deaths.contains(&victim_sel) {
            continue;
        }
        // 同一轮若 HP 边沿或另一条原生路径已结算，后到事件只确认，不重复加分。
        if track.hp_credited.contains(&victim_sel) {
            continue;
        }
        let mine = is_tracked_target;
        let victim = if steam != 0 { steam } else { victim_sel };
        crate::net_appear::request_combat_capture(&if mine {
            format!(
                "remote_death_native_mine local={local_sel:#x} victim={victim_sel:#x} last_hit={:#x} attacker={:#x}",
                last_hit.selector.0, attacker.selector.0
            )
        } else {
            format!(
                "remote_death_native_other local={local_sel:#x} victim={victim_sel:#x} last_hit={:#x} attacker={:#x}",
                last_hit.selector.0, attacker.selector.0
            )
        });
        if mine {
            if credited_victims.insert(victim) {
                kills += 1;
            }
            // 已确认当前目标死亡后，无论本帧是否被另一来源先去重，都结束本轮目标。
            track.hp_credited.insert(victim_sel);
            track.tracked_target = None;
        }
    }
    let live: HashSet<u32> = resolved.iter().map(|entry| entry.0).collect();
    track.peers.retain(|sel, _| live.contains(sel));
    if track
        .tracked_target
        .is_some_and(|target| !live.contains(&target))
    {
        track.tracked_target = None;
    }
    // 最终保险：只要本轮实际增加了个人击杀分，离开统计区前必定清空目标。
    if kills > 0 {
        track.tracked_target = None;
    }
    // 本机被击杀后，上一轮交战目标不得带入复活后的下一轮。
    if deaths > 0 {
        track.tracked_target = None;
    }
    drop(guard);

    if kills != 0 || deaths != 0 {
        record(kills, deaths);
    }
}
