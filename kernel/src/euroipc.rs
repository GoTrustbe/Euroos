//! EuroIPC — a simple message bus between processes (Horizon A).
//!
//! Processes claim a port (an integer endpoint), send messages
//! to a port, and receive from their own port. Each message carries the
//! **app identity** of the sender (pid) and is **audited**. The
//! permission check is an open hook (everything allowed) for ordinary ports;
//! a port claimed with [`register_restricted`] accepts exactly one sender pid
//! (the userspace-driver ports: the driver's port takes only the kernel, the
//! netstack port only the pid holding the device grant). Queues are bounded.
//! no_std + alloc, kernel-internal.
//!
//! Since the J1 merge the port table is the host-tested per-channel core
//! `crates/euroipc` (`ChannelTable`): the channel table is a per-CPU replicated
//! read-mostly lock, every port has its own lock, and the audit trail is a
//! lock-free per-CPU ring. All of its locks are EuroLock `Mode::Irq` (interrupts
//! off while held or waited for), which is what the driver branch's
//! `without_interrupts` wrappers did by hand: the driver's receive and send run
//! in syscalls on the same CPU as task 0, which uses these ports with
//! interrupts on.
//!
//! Interrupts reach a port through a **notification slot** ([`bind_notify`],
//! [`notify_from_irq`]): the interrupt handler only bumps an atomic counter (no
//! lock, no allocation), and the next receive on the bound port delivers it as
//! an ordinary message from the kernel, ahead of queued data.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use ::euroipc::ChannelTable;

/// Messages one port may hold; further sends are refused (`-EAGAIN`) and counted.
#[allow(dead_code)]
pub const MAX_QUEUE: usize = ::euroipc::MAX_QUEUE;
/// The pid the kernel itself sends as.
pub const KERNEL_PID: u64 = 0;
/// Audit lines shown in the system window (the old six-line ring).
const AUDIT_LINES: usize = 6;

fn tsc() -> u64 {
    // SAFETY: rdtsc has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Every port, its queue and the audit ring (per-channel locks, J1).
static TABLE: ChannelTable = ChannelTable::with_clock(tsc);

/// Notification slots: (bound port, pending count). Port 0 = unbound.
pub const NOTIFY_SLOTS: usize = 4;
static NOTIFY_PORT: [AtomicU32; NOTIFY_SLOTS] = [const { AtomicU32::new(0) }; NOTIFY_SLOTS];
static NOTIFY_COUNT: [AtomicU32; NOTIFY_SLOTS] = [const { AtomicU32::new(0) }; NOTIFY_SLOTS];

/// Claim a port for `pid`. 1 = ok, 0 = already in use.
pub fn register(pid: u64, port: u32) -> i64 {
    TABLE.register(pid, port) as i64
}

/// Claim a port for `pid` that accepts messages only from `only_from`.
pub fn register_restricted(pid: u64, port: u32, only_from: u64) -> i64 {
    TABLE.register_restricted(pid, port, only_from) as i64
}

/// Change which sender a restricted port accepts (a restarted driver has a new pid).
pub fn set_only_from(port: u32, only_from: u64) {
    TABLE.set_only_from(port, only_from)
}

/// Release a port and drop its queue (its owner died).
pub fn unregister(port: u32) {
    TABLE.unregister(port);
    for i in 0..NOTIFY_SLOTS {
        if NOTIFY_PORT[i].load(Ordering::Relaxed) == port {
            NOTIFY_PORT[i].store(0, Ordering::Relaxed);
            NOTIFY_COUNT[i].store(0, Ordering::Relaxed);
        }
    }
}

/// Send `data` to `port`. Returns the number of bytes, or -ESRCH (-3) if the
/// port does not exist, -EPERM (-1) if the port does not accept this sender,
/// -EAGAIN (-11) if its queue is full. Tags the message with the sender-pid + audit.
pub fn send(sender_pid: u64, port: u32, data: &[u8]) -> i64 {
    send_with(sender_pid, port, data, true)
}

/// Like [`send`]; `audit_it = false` for the data path (every network frame
/// would otherwise push the control messages out of the audit ring).
pub fn send_with(sender_pid: u64, port: u32, data: &[u8], audit_it: bool) -> i64 {
    match TABLE.send_with(sender_pid, port, data, audit_it) {
        Ok(n) => n as i64,
        Err(e) => e,
    }
}

/// Receive one message from the port of `pid` into `buf` (max `max` bytes).
/// Returns the number of bytes, -EAGAIN (-11) if there is nothing, or -ESRCH (-3)
/// if `pid` has no port. The copy into user memory happens after the channel
/// lock is released.
pub fn recv(pid: u64, buf: u64, max: usize) -> i64 {
    match TABLE.recv(pid) {
        Ok((_, data)) => {
            let n = data.len().min(max);
            // SAFETY: buf lies in the USER arena of the receiving process.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf as *mut u8, n) };
            n as i64
        }
        Err(e) => e.errno(),
    }
}

/// Take the next message of `port` as (sender, data): a pending interrupt
/// notification first (sender = kernel, data = `irq_msg(count)`), then the
/// queue. For kernel-side receivers and for syscalls that copy out themselves.
pub fn take(port: u32, irq_msg: impl Fn(u32) -> Vec<u8>) -> Option<(u64, Vec<u8>)> {
    for i in 0..NOTIFY_SLOTS {
        if NOTIFY_PORT[i].load(Ordering::Relaxed) == port {
            let n = NOTIFY_COUNT[i].swap(0, Ordering::AcqRel);
            if n > 0 {
                return Some((KERNEL_PID, irq_msg(n)));
            }
        }
    }
    TABLE.take(port)
}

/// Bind notification slot `slot` to `port` (from task context).
pub fn bind_notify(slot: usize, port: u32) {
    if slot < NOTIFY_SLOTS {
        NOTIFY_COUNT[slot].store(0, Ordering::Relaxed);
        NOTIFY_PORT[slot].store(port, Ordering::Release);
    }
}

/// From an interrupt handler: one more event for the port bound to `slot`.
/// Lock-free and allocation-free by construction.
pub fn notify_from_irq(slot: usize) {
    if slot < NOTIFY_SLOTS && NOTIFY_PORT[slot].load(Ordering::Acquire) != 0 {
        NOTIFY_COUNT[slot].fetch_add(1, Ordering::AcqRel);
    }
}

/// Sends refused because a queue was full / a restricted port refused the sender.
#[allow(dead_code)]
pub fn refused_counts() -> (u64, u64) {
    (TABLE.dropped_full.load(Ordering::Relaxed), TABLE.refused_sender.load(Ordering::Relaxed))
}

/// The recent IPC audit lines (for the system window).
pub fn audit_lines() -> Vec<String> {
    TABLE.audit_lines(AUDIT_LINES)
}
