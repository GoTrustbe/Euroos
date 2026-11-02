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
//! Interrupts reach a port through a **notification slot** ([`bind_notify`],
//! [`notify_from_irq`]): the interrupt handler only bumps an atomic counter (no
//! lock, no allocation), and the next receive on the bound port delivers it as
//! an ordinary message from the kernel, ahead of queued data.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use spin::Mutex;

/// Messages one port may hold; further sends are refused (`-EAGAIN`) and counted.
pub const MAX_QUEUE: usize = 256;
/// The pid the kernel itself sends as.
pub const KERNEL_PID: u64 = 0;

struct Port {
    port: u32,
    owner_pid: u64,
    /// `Some(pid)`: only this sender is accepted.
    only_from: Option<u64>,
    queue: Vec<(u64, Vec<u8>)>, // (sender-pid, data)
}

static PORTS: Mutex<Vec<Port>> = Mutex::new(Vec::new());
static AUDIT: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Sends refused: full queue, or a sender the port does not accept.
pub static DROPPED_FULL: AtomicU64 = AtomicU64::new(0);
pub static REFUSED_SENDER: AtomicU64 = AtomicU64::new(0);

/// Notification slots: (bound port, pending count). Port 0 = unbound.
const NOTIFY_SLOTS: usize = 4;
static NOTIFY_PORT: [AtomicU32; NOTIFY_SLOTS] = [const { AtomicU32::new(0) }; NOTIFY_SLOTS];
static NOTIFY_COUNT: [AtomicU32; NOTIFY_SLOTS] = [const { AtomicU32::new(0) }; NOTIFY_SLOTS];

/// Every lock here is taken with interrupts off: the driver's receive and send
/// run in syscalls (interrupts off) on the same CPU as task 0, which uses these
/// ports with interrupts on. A preempted holder would otherwise spin the
/// syscall forever (the BUG-007 class).
fn ports<R>(f: impl FnOnce(&mut Vec<Port>) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut PORTS.lock()))
}

fn audit(line: String) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut a = AUDIT.lock();
        a.push(line);
        let n = a.len();
        if n > 6 {
            a.drain(0..n - 6);
        }
    })
}

/// Claim a port for `pid`. 1 = ok, 0 = already in use.
pub fn register(pid: u64, port: u32) -> i64 {
    claim(pid, port, None)
}

/// Claim a port for `pid` that accepts messages only from `only_from`.
pub fn register_restricted(pid: u64, port: u32, only_from: u64) -> i64 {
    claim(pid, port, Some(only_from))
}

fn claim(pid: u64, port: u32, only_from: Option<u64>) -> i64 {
    let fresh = ports(|ps| {
        if ps.iter().any(|p| p.port == port) {
            return false;
        }
        ps.push(Port { port, owner_pid: pid, only_from, queue: Vec::new() });
        true
    });
    if !fresh {
        return 0;
    }
    audit(alloc::format!("pid {pid} claimed port {port}{}", if only_from.is_some() { " (restricted)" } else { "" }));
    1
}

/// Change which sender a restricted port accepts (a restarted driver has a new pid).
pub fn set_only_from(port: u32, only_from: u64) {
    ports(|ps| {
        if let Some(p) = ps.iter_mut().find(|p| p.port == port) {
            p.only_from = Some(only_from);
        }
    })
}

/// Release a port and drop its queue (its owner died).
pub fn unregister(port: u32) {
    ports(|ps| ps.retain(|p| p.port != port));
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
/// would otherwise push the control messages out of the 6-line audit ring).
pub fn send_with(sender_pid: u64, port: u32, data: &[u8], audit_it: bool) -> i64 {
    let owner = ports(|ps| match ps.iter_mut().find(|p| p.port == port) {
        Some(p) => {
            if let Some(only) = p.only_from {
                if only != sender_pid {
                    return Err(-1);
                }
            }
            // permission-check hook for unrestricted ports (now: allowed) — later EuroGuard policy.
            if p.queue.len() >= MAX_QUEUE {
                return Err(-11);
            }
            p.queue.push((sender_pid, data.to_vec()));
            Ok(p.owner_pid)
        }
        None => Err(-3),
    });
    let owner = match owner {
        Ok(o) => o,
        Err(-1) => {
            REFUSED_SENDER.fetch_add(1, Ordering::Relaxed);
            audit(alloc::format!("pid {sender_pid} -> port {port}: REFUSED (restricted port)"));
            return -1;
        }
        Err(-11) => {
            DROPPED_FULL.fetch_add(1, Ordering::Relaxed);
            return -11;
        }
        Err(e) => return e,
    };
    if audit_it {
        audit(alloc::format!("pid {sender_pid} -> port {port} (pid {owner}): {} bytes", data.len()));
    }
    data.len() as i64
}

/// Receive one message from the port of `pid` into `buf` (max `max` bytes).
/// Returns the number of bytes, -EAGAIN (-11) if there is nothing, or -ESRCH (-3)
/// if `pid` has no port.
pub fn recv(pid: u64, buf: u64, max: usize) -> i64 {
    ports(|ps| match ps.iter_mut().find(|p| p.owner_pid == pid) {
        Some(p) => {
            if p.queue.is_empty() {
                return -11;
            }
            let (_, data) = p.queue.remove(0);
            let n = data.len().min(max);
            // SAFETY: buf lies in the USER arena of the receiving process.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf as *mut u8, n) };
            n as i64
        }
        None => -3,
    })
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
    ports(|ps| {
        let p = ps.iter_mut().find(|p| p.port == port)?;
        if p.queue.is_empty() {
            return None;
        }
        Some(p.queue.remove(0))
    })
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

/// The recent IPC audit lines (for the system window).
pub fn audit_lines() -> Vec<String> {
    x86_64::instructions::interrupts::without_interrupts(|| AUDIT.lock().clone())
}
