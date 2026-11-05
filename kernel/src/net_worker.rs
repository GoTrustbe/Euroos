//! The network worker: inbound TCP in its own kernel task (net worker, 2026-11).
//!
//! Inbound connections used to move only when the desktop loop called
//! `service()` / `pump_all()`. The loop runs the shell, and a long synchronous
//! shell or file-system command (`churn fs`, `truncate` or `sha256sum` of
//! 8 MiB: 20-30 s) held it: in the 1 h soak with 4 httpd clients 4 570
//! requests were answered and 652 failed, p99 9.5 s, all of the failures inside
//! such commands. Since BUG-019 nothing on the inbound path waits, so it does
//! not need the loop, only a context of its own.
//!
//! That context is a kernel task on the BSP (`sched::spawn_kernel`), next to
//! task 0 under the same scheduler, so every assumption of the netstack holds
//! (BSP only, the userspace driver e1000d runs on the same core, the same
//! locks). It sleeps when there is nothing to do and is woken:
//!
//! - by e1000d (`net.driver=user`): every EuroIPC delivery of a frame already
//!   calls `wake_rx_waiters` from the driver's syscall;
//! - by the NIC's receive interrupt (kernel drivers: the 82574L and virtio-net
//!   raise MSI-X; `rx_route_irq` drains the ring and wakes the worker without
//!   waiting for the scheduler lock);
//! - every tick while inbound work is in flight (half-open entries, httpd
//!   connections: SYN-ACK retransmission, linger, expiry), and on a card that
//!   cannot interrupt (82540EM) while the httpd or a listener is on.
//!
//! A pass takes from the legacy queue only what belongs to inbound TCP (SYNs
//! for the httpd or a listener, segments of a connection that registered its
//! queue late) and ARP requests for our address; everything else stays there
//! for `service()` and the blocking readers. Then `passive::step` (handshakes,
//! httpd answers, accept queues) and, once a tick, `pump_all` (ACKs and the
//! retransmit clock of every socket connection). The desktop loop keeps calling
//! `service()` and `pump_all()` as before; both are safe to run from either
//! context (the passive table is try-locked, every net-global lock is held with
//! IF=0, `lk`).
//!
//! A pass runs with interrupts off. That is safe only because no lock it takes
//! can be held by a task the timer parked: the net globals, the firewall and
//! the NIC rings are IF=0 holders now, socket and EuroIPC locks are `Mode::Irq`,
//! and the scheduler lock is never held across a switch (a switch needs it).
//!
//! The design is the usual one of a network bottom half / a netstack daemon
//! (Redox runs its netstack as the `smolnetd` scheme daemon, woken by the
//! driver's events; see docs/REDOX-REFERENCE.md). No code is taken from it.

use super::*;
use core::sync::atomic::AtomicUsize;

const NO_TASK: usize = usize::MAX;
/// Wait channel of the worker when it has nothing to do.
const CHAN: u64 = 0x4E45_5457_4F52_4B52; // "NETWORKR"

/// Scheduler slot of the worker (NO_TASK until started).
static TASK: AtomicUsize = AtomicUsize::new(NO_TASK);
/// Work was announced since the worker's last pass began.
static PENDING: AtomicBool = AtomicBool::new(false);
/// The worker is parked (blocked or sleeping) or about to be.
static IDLE: AtomicBool = AtomicBool::new(false);
/// Passes suspended (the `[net-async]` self-test counts what one service()
/// pass does and must not have a second context doing it meanwhile).
static PAUSED: AtomicBool = AtomicBool::new(false);
/// Tick of the worker's last `pump_all`.
static LAST_PUMP: AtomicU64 = AtomicU64::new(u64::MAX);

pub(super) static PASSES: AtomicU64 = AtomicU64::new(0);
pub(super) static FRAMES: AtomicU64 = AtomicU64::new(0);
pub(super) static KICKS: AtomicU64 = AtomicU64::new(0);
pub(super) static KICKS_IRQ: AtomicU64 = AtomicU64::new(0);
pub(super) static KICKS_IRQ_BUSY: AtomicU64 = AtomicU64::new(0);
/// Longest pass in microseconds (HPET).
pub(super) static PASS_MAX_US: AtomicU64 = AtomicU64::new(0);

/// The worker's scheduler slot, if it runs.
pub fn task() -> Option<usize> {
    match TASK.load(Ordering::Acquire) {
        NO_TASK => None,
        t => Some(t),
    }
}

/// Start the worker (once the scheduler runs and the network is configured).
pub fn start() -> Option<usize> {
    if let Some(t) = task() {
        return Some(t);
    }
    get()?;
    // nice -5: when it is woken it should get the CPU at the next tick, ahead of
    // a busy shell (task 0) and of compute-bound processes. A pass is short.
    let t = crate::sched::spawn_kernel(worker_main, -5)?;
    TASK.store(t, Ordering::Release);
    crate::serial_println!(
        "[net-worker] kernel task {t} started: inbound TCP (SYN queue, httpd, accept queues) runs here, woken by {}",
        if crate::nic::frames_from_process() {
            "e1000d's frame deliveries (EuroIPC)"
        } else if crate::nic::rx_interrupts() {
            "the NIC's receive interrupt (MSI-X)"
        } else {
            "the tick (this NIC does not interrupt)"
        }
    );
    Some(t)
}

pub(super) fn pause(on: bool) {
    PAUSED.store(on, Ordering::Release);
}

/// Something arrived for the netstack (task or syscall context).
pub(super) fn kick() {
    let Some(t) = task() else { return };
    KICKS.fetch_add(1, Ordering::Relaxed);
    PENDING.store(true, Ordering::Release);
    if IDLE.load(Ordering::Acquire) && crate::sched::current_lockfree() != t {
        crate::sched::unblock_any(t);
    }
}

/// The same from an interrupt handler: never waits for the scheduler lock. If
/// it is busy, PENDING stays set and the next kick (or the desktop loop's
/// `poke`) wakes the worker.
pub(super) fn kick_irq() {
    let Some(t) = task() else { return };
    KICKS_IRQ.fetch_add(1, Ordering::Relaxed);
    PENDING.store(true, Ordering::Release);
    if IDLE.load(Ordering::Acquire) && !crate::sched::try_unblock_any(t) {
        KICKS_IRQ_BUSY.fetch_add(1, Ordering::Relaxed);
    }
}

/// Desktop loop: wake the worker if work is pending and it still sleeps (an
/// interrupt-time kick that found the scheduler lock busy).
pub fn poke() {
    if PENDING.load(Ordering::Acquire) && IDLE.load(Ordering::Acquire) {
        if let Some(t) = task() {
            crate::sched::unblock_any(t);
        }
    }
}

/// Should the worker look again at the next tick even without a kick?
fn needs_tick() -> bool {
    if passive::busy() {
        return true; // retransmissions, linger and expiry run on the tick
    }
    // A card without a receive interrupt is polled while someone can be
    // reached from outside.
    !crate::nic::frames_from_process()
        && !crate::nic::rx_interrupts()
        && (SERVER_ON.load(Ordering::Relaxed) || LISTENERS.load(Ordering::Relaxed) > 0)
}

extern "C" fn worker_main() -> ! {
    let me = crate::sched::current_lockfree();
    loop {
        PENDING.store(false, Ordering::Release);
        if !PAUSED.load(Ordering::Acquire) {
            pass();
        }
        {
            let _g = crate::sched::IfOffGuard::new();
            IDLE.store(true, Ordering::Release);
            if !PAUSED.load(Ordering::Acquire) && needs_tick() {
                crate::sched::sleep_ticks(1);
            } else {
                crate::sched::block_on(CHAN);
            }
            // A kick between the end of the pass and the line above found the
            // worker running and did not wake it: wake ourselves.
            if PENDING.load(Ordering::Acquire) {
                crate::sched::unblock_any(me);
            }
        }
        crate::sched::yield_now();
        IDLE.store(false, Ordering::Release);
    }
}

/// One pass. Never waits; interrupts off throughout (see the module docs).
fn pass() {
    let Some(cfg) = get() else { return };
    let t0 = time_us();
    let _g = crate::sched::IfOffGuard::new();
    rx_route();
    let n = inbound(&cfg);
    let now = crate::interrupts::ticks();
    if LAST_PUMP.swap(now, Ordering::Relaxed) != now {
        pump_all(); // the socket connections' ACKs + clock, then passive::step
    } else {
        passive::step();
    }
    PASSES.fetch_add(1, Ordering::Relaxed);
    FRAMES.fetch_add(n as u64, Ordering::Relaxed);
    PASS_MAX_US.fetch_max(time_us().saturating_sub(t0), Ordering::Relaxed);
}

/// Is this a frame the worker may want (TCP to anyone, or ARP)? Only a cheap
/// look at the headers; `handle_legacy` decides.
fn candidate(f: &[u8]) -> bool {
    match EthernetHeader::parse(f) {
        Ok((h, p)) => match h.ethertype {
            EtherType::Arp => true,
            EtherType::Ipv4 => matches!(Ipv4Header::parse(p), Ok((ih, _)) if ih.protocol == Protocol::Tcp),
            _ => false,
        },
        Err(_) => false,
    }
}

/// Inbound frames from the legacy queue; the rest goes back where it was.
fn inbound(cfg: &NetCfg) -> usize {
    let mut mine = alloc::vec::Vec::new();
    {
        let mut l = lk(&NET_LEGACY);
        let mut i = 0;
        while i < l.len() && mine.len() < 64 {
            if candidate(&l[i]) {
                if let Some(f) = l.remove(i) {
                    mine.push(f);
                }
            } else {
                i += 1;
            }
        }
    }
    if mine.is_empty() {
        return 0;
    }
    let mut used = 0;
    let mut back = alloc::vec::Vec::new();
    for f in mine {
        if handle_legacy(&f, cfg, true) {
            used += 1;
        } else {
            back.push(f);
        }
    }
    if !back.is_empty() {
        let mut l = lk(&NET_LEGACY);
        for f in back.into_iter().rev() {
            l.push_front(f);
        }
    }
    used
}

/// One line for `netstat` / the boot report.
pub fn report() -> String {
    match task() {
        None => "net worker: not running".into(),
        Some(t) => alloc::format!(
            "net worker: task {t}, {} passes, {} inbound frames, longest pass {} us; kicks {} (e1000d/task) + {} (interrupt, {} found the scheduler busy)",
            PASSES.load(Ordering::Relaxed),
            FRAMES.load(Ordering::Relaxed),
            PASS_MAX_US.load(Ordering::Relaxed),
            KICKS.load(Ordering::Relaxed),
            KICKS_IRQ.load(Ordering::Relaxed),
            KICKS_IRQ_BUSY.load(Ordering::Relaxed)
        ),
    }
}
