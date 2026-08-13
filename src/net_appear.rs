//! 单端原生装备同步。
//!
//! 菜单装备调用 `0xCA11C0`，由游戏自己读取当前 ChrAsm、构造完整
//! packet 12 并广播。这里直接复用该入口，不再手工猜测 192 字节布局。

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use retour::RawDetour;

use crate::{offsets, paths};

type BroadCastFn =
    unsafe extern "C" fn(session: usize, packet_type: u64, buf: *const u8, len: u32) -> usize;
type TryDequeueFn = unsafe extern "C" fn(
    session: usize,
    peer: u64,
    packet_id: u32,
    out_buf: *mut u8,
    max_len: u32,
    control: u8,
) -> i32;
type SendEquipmentSnapshotFn = unsafe extern "C" fn();
type Packet24OwnerOuterFn = unsafe extern "C" fn(sync: usize) -> usize;

const EQUIPMENT_PACKET_TYPE: u64 = 12;
const EQUIPMENT_PACKET_LEN: usize = 192;

static BROADCAST_ORIG: AtomicUsize = AtomicUsize::new(0);
static BROADCAST_HOOK: OnceLock<RawDetour> = OnceLock::new();
static DEQUEUE_ORIG: AtomicUsize = AtomicUsize::new(0);
static DEQUEUE_HOOK: OnceLock<RawDetour> = OnceLock::new();
static OWNER_LOOP_ORIG: AtomicUsize = AtomicUsize::new(0);
static OWNER_LOOP_HOOK: OnceLock<RawDetour> = OnceLock::new();
static OWNER_ONCE_ORIG: AtomicUsize = AtomicUsize::new(0);
static OWNER_ONCE_HOOK: OnceLock<RawDetour> = OnceLock::new();
static SEND_EQUIPMENT: OnceLock<Option<SendEquipmentSnapshotFn>> = OnceLock::new();
static NET_EVENTS: Mutex<Vec<TraceEvent>> = Mutex::new(Vec::new());
static RECENT_PACKETS: Mutex<VecDeque<PacketEvent>> = Mutex::new(VecDeque::new());
static CAPTURE_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static REMOTE_BRAID_TRIGGERS: Mutex<Vec<(u32, u64, usize)>> = Mutex::new(Vec::new());

thread_local! {
    static PACKET24_OWNER: Cell<usize> = const { Cell::new(0) };
}

const RECENT_LIMIT: usize = 128;
const EVENT_LIMIT: usize = 512;
const PAYLOAD_LIMIT: usize = 512;
const CAPTURE_AFTER: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct PacketEvent {
    source: &'static str,
    packet_type: u64,
    peer: u64,
    actual_len: usize,
    payload: Vec<u8>,
}

enum TraceEvent {
    Marker(String),
    Packet(PacketEvent),
}

fn send_equipment_fn() -> Option<SendEquipmentSnapshotFn> {
    *SEND_EQUIPMENT.get_or_init(|| {
        offsets::resolve_send_equipment_snapshot()
            .map(|addr| unsafe { std::mem::transmute::<usize, SendEquipmentSnapshotFn>(addr) })
    })
}

fn capture_active() -> bool {
    CAPTURE_UNTIL
        .lock()
        .ok()
        .and_then(|until| *until)
        .is_some_and(|until| Instant::now() <= until)
}

fn remember_packet(event: PacketEvent, always_log: bool) {
    if let Ok(mut recent) = RECENT_PACKETS.lock() {
        if recent.len() >= RECENT_LIMIT {
            recent.pop_front();
        }
        recent.push_back(event.clone());
    }
    if (always_log || capture_active())
        && let Ok(mut events) = NET_EVENTS.lock()
        && events.len() < EVENT_LIMIT
    {
        events.push(TraceEvent::Packet(event));
    }
}

fn packet_event(source: &'static str, packet_type: u64, peer: u64, data: &[u8]) -> PacketEvent {
    PacketEvent {
        source,
        packet_type,
        peer,
        actual_len: data.len(),
        payload: data[..data.len().min(PAYLOAD_LIMIT)].to_vec(),
    }
}

unsafe extern "C" fn broadcast_hook(
    session: usize,
    packet_type: u64,
    buf: *const u8,
    len: u32,
) -> usize {
    if packet_type == 38 {
        crate::combat::note_local_damage_packet_target();
    }
    let always_log = (packet_type == EQUIPMENT_PACKET_TYPE && len as usize == EQUIPMENT_PACKET_LEN)
        || ([8, 60, 61].contains(&packet_type) && len > 0 && len <= 4096);
    if !buf.is_null() && len > 0 && len <= 4096 {
        let data = unsafe { std::slice::from_raw_parts(buf, len as usize) };
        remember_packet(packet_event("send", packet_type, 0, data), always_log);
    }

    let orig_addr = BROADCAST_ORIG.load(Ordering::SeqCst);
    if orig_addr == 0 {
        return 0;
    }
    let orig: BroadCastFn = unsafe { std::mem::transmute(orig_addr) };
    unsafe { orig(session, packet_type, buf, len) }
}

unsafe fn packet24_owner_outer(sync: usize, orig_addr: usize) -> usize {
    if orig_addr == 0 {
        return 0;
    }
    let owner = if sync >= 0x10000 {
        unsafe { *((sync + 0xA8) as *const usize) }
    } else {
        0
    };
    let orig: Packet24OwnerOuterFn = unsafe { std::mem::transmute(orig_addr) };
    PACKET24_OWNER.with(|slot| {
        let previous = slot.replace(owner);
        let result = unsafe { orig(sync) };
        slot.set(previous);
        result
    })
}

unsafe extern "C" fn packet24_owner_loop_hook(sync: usize) -> usize {
    unsafe { packet24_owner_outer(sync, OWNER_LOOP_ORIG.load(Ordering::SeqCst)) }
}

unsafe extern "C" fn packet24_owner_once_hook(sync: usize) -> usize {
    unsafe { packet24_owner_outer(sync, OWNER_ONCE_ORIG.load(Ordering::SeqCst)) }
}

/// 取出远端“玛莉卡的发辫”事件的 owner selector、连接和长度。
pub fn take_remote_braid_triggers() -> Vec<(u32, u64, usize)> {
    REMOTE_BRAID_TRIGGERS
        .lock()
        .map(|mut peers| std::mem::take(&mut *peers))
        .unwrap_or_default()
}

unsafe extern "C" fn try_dequeue_hook(
    session: usize,
    peer: u64,
    packet_id: u32,
    out_buf: *mut u8,
    max_len: u32,
    control: u8,
) -> i32 {
    let orig_addr = DEQUEUE_ORIG.load(Ordering::SeqCst);
    if orig_addr == 0 {
        return 0;
    }
    let orig: TryDequeueFn = unsafe { std::mem::transmute(orig_addr) };
    let result = unsafe { orig(session, peer, packet_id, out_buf, max_len, control) };
    if result > 0 && !out_buf.is_null() {
        let len = (result as usize).min(max_len as usize).min(4096);
        let data = unsafe { std::slice::from_raw_parts(out_buf, len) };
        if packet_id == 24 {
            let legacy_braid = data.len() >= 9
                && data[0..5] == [0x0D, 0x01, 0x58, 0x02, 0x00]
                && data[6..9] == [0x0C, 0x01, 0x25];
            // 实际联机中还会以批量 SpEffect 状态格式发送：
            // 0D 02 08 00 00 00 | 58 02 00 00，其中 0x0258 就是发辫事件。
            let batched_braid = data.len() >= 10
                && data[0..6] == [0x0D, 0x02, 0x08, 0x00, 0x00, 0x00]
                && data[6..10] == [0x58, 0x02, 0x00, 0x00];
            let braid_trigger = legacy_braid || batched_braid;
            if braid_trigger {
                let selector = PACKET24_OWNER.with(|slot| {
                    let owner = slot.get();
                    if owner < 0x10000 {
                        return 0;
                    }
                    let selector = unsafe { *((owner + 0x8) as *const u32) };
                    if selector >> 28 == 1 { selector } else { 0 }
                });
                if let Ok(mut peers) = REMOTE_BRAID_TRIGGERS.lock() {
                    peers.push((selector, peer, data.len()));
                }
            }
        }
        remember_packet(packet_event("recv", packet_id as u64, peer, data), false);
    }
    result
}

/// 同步完成后调用菜单同源的原生装备快照发送入口。
pub fn request_after_sync() {
    let Some(send) = send_equipment_fn() else {
        paths::stage("equipment_native_send_resolve_failed");
        return;
    };
    unsafe {
        send();
    }
    paths::stage("equipment_native_snapshot_sent");
}

fn install_packet24_owner_hook(
    addr: usize,
    detour_fn: *const (),
    orig: &AtomicUsize,
    slot: &OnceLock<RawDetour>,
    label: &str,
) {
    unsafe {
        match RawDetour::new(addr as *const (), detour_fn) {
            Ok(hook) => {
                orig.store(
                    std::mem::transmute::<*const (), usize>(hook.trampoline()),
                    Ordering::SeqCst,
                );
                if let Err(error) = hook.enable() {
                    orig.store(0, Ordering::SeqCst);
                    paths::stage(&format!("{label}_enable_failed_{error:?}"));
                    return;
                }
                let _ = slot.set(hook);
                paths::stage(&format!("{label}_ok"));
            }
            Err(error) => paths::stage(&format!("{label}_create_failed_{error:?}")),
        }
    }
}

/// 安装只读收发包 Hook；不修改任何原生载荷或返回值。
pub fn install_network_trace_hook() {
    let Some(addr) = offsets::resolve_broadcast() else {
        paths::stage("network_trace_resolve_failed");
        return;
    };
    paths::reset_network_trace();
    unsafe {
        match RawDetour::new(addr as *const (), broadcast_hook as *const ()) {
            Ok(hook) => {
                BROADCAST_ORIG.store(
                    std::mem::transmute::<*const (), usize>(hook.trampoline()),
                    Ordering::SeqCst,
                );
                if let Err(e) = hook.enable() {
                    BROADCAST_ORIG.store(0, Ordering::SeqCst);
                    paths::stage(&format!("network_trace_hook_enable_failed_{e:?}"));
                    return;
                }
                let _ = BROADCAST_HOOK.set(hook);
                paths::stage("network_trace_hook_ok");
            }
            Err(e) => paths::stage(&format!("network_trace_hook_create_failed_{e:?}")),
        }
    }
    let Some(addr) = offsets::resolve_try_dequeue() else {
        paths::stage("network_recv_trace_resolve_failed");
        return;
    };
    unsafe {
        match RawDetour::new(addr as *const (), try_dequeue_hook as *const ()) {
            Ok(hook) => {
                DEQUEUE_ORIG.store(
                    std::mem::transmute::<*const (), usize>(hook.trampoline()),
                    Ordering::SeqCst,
                );
                if let Err(error) = hook.enable() {
                    DEQUEUE_ORIG.store(0, Ordering::SeqCst);
                    paths::stage(&format!("network_recv_trace_enable_failed_{error:?}"));
                    return;
                }
                let _ = DEQUEUE_HOOK.set(hook);
                paths::stage("network_send_recv_trace_ok");
            }
            Err(error) => paths::stage(&format!("network_recv_trace_create_failed_{error:?}")),
        }
    }
    if let Some(addr) = offsets::resolve_packet24_owner_outer_loop() {
        install_packet24_owner_hook(
            addr,
            packet24_owner_loop_hook as *const (),
            &OWNER_LOOP_ORIG,
            &OWNER_LOOP_HOOK,
            "packet24_owner_loop",
        );
    } else {
        paths::stage("packet24_owner_loop_resolve_failed");
    }
    if let Some(addr) = offsets::resolve_packet24_owner_outer_once() {
        install_packet24_owner_hook(
            addr,
            packet24_owner_once_hook as *const (),
            &OWNER_ONCE_ORIG,
            &OWNER_ONCE_HOOK,
            "packet24_owner_once",
        );
    } else {
        paths::stage("packet24_owner_once_resolve_failed");
    }
}

/// 死亡边沿触发：写入此前最近 128 个收发包，并继续记录之后 3 秒。
pub fn request_combat_capture(label: &str) {
    let recent = RECENT_PACKETS
        .lock()
        .map(|packets| packets.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if let Ok(mut events) = NET_EVENTS.lock() {
        events.push(TraceEvent::Marker(format!(
            "combat_capture_begin event={label}"
        )));
        for packet in recent {
            if events.len() >= EVENT_LIMIT {
                break;
            }
            events.push(TraceEvent::Packet(packet));
        }
        events.push(TraceEvent::Marker(format!(
            "combat_capture_edge event={label}"
        )));
    }
    if let Ok(mut until) = CAPTURE_UNTIL.lock() {
        *until = Some(Instant::now() + CAPTURE_AFTER);
    }
}

pub fn poll() {
    if let Ok(mut events) = NET_EVENTS.lock() {
        for event in events.drain(..) {
            match event {
                TraceEvent::Marker(line) => paths::append_network_trace(&line),
                TraceEvent::Packet(packet) => {
                    let hex = packet
                        .payload
                        .iter()
                        .map(|byte| format!("{byte:02X}"))
                        .collect::<Vec<_>>()
                        .join("");
                    paths::append_network_trace(&format!(
                        "source={} type={} peer={:#x} len={} captured={} payload={hex}",
                        packet.source,
                        packet.packet_type,
                        packet.peer,
                        packet.actual_len,
                        packet.payload.len(),
                    ));
                }
            }
        }
    }
}
