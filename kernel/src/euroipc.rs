//! EuroIPC — a simple message bus between processes (Horizon A).
//!
//! Processes claim a port (an integer endpoint), send messages
//! to a port, and receive from their own port. Each message carries the
//! **app identity** of the sender (pid) and is **audited**. The
//! permission check is currently an open hook (everything allowed) — the wiring with the
//! EuroGuard policy is a next step. no_std + alloc, kernel-internal.

use alloc::string::String;
use alloc::vec::Vec;

use spin::Mutex;

struct Port {
    port: u32,
    owner_pid: u64,
    queue: Vec<(u64, Vec<u8>)>, // (sender-pid, data)
}

static PORTS: Mutex<Vec<Port>> = Mutex::new(Vec::new());
static AUDIT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn audit(line: String) {
    let mut a = AUDIT.lock();
    a.push(line);
    let n = a.len();
    if n > 6 {
        a.drain(0..n - 6);
    }
}

/// Claim a port for `pid`. 1 = ok, 0 = already in use.
pub fn register(pid: u64, port: u32) -> i64 {
    let mut ps = PORTS.lock();
    if ps.iter().any(|p| p.port == port) {
        return 0;
    }
    ps.push(Port { port, owner_pid: pid, queue: Vec::new() });
    drop(ps);
    audit(alloc::format!("pid {pid} claimed port {port}"));
    1
}

/// Send `data` to `port`. Returns the number of bytes, or -ESRCH (-3) if the
/// port does not exist. Tags the message with the sender-pid + audit.
pub fn send(sender_pid: u64, port: u32, data: &[u8]) -> i64 {
    let mut ps = PORTS.lock();
    let owner = match ps.iter_mut().find(|p| p.port == port) {
        Some(p) => {
            // permission-check hook (now: allowed) — later EuroGuard policy.
            p.queue.push((sender_pid, data.to_vec()));
            p.owner_pid
        }
        None => return -3,
    };
    drop(ps);
    audit(alloc::format!("pid {sender_pid} -> port {port} (pid {owner}): {} bytes", data.len()));
    data.len() as i64
}

/// Receive one message from the port of `pid` into `buf` (max `max` bytes).
/// Returns the number of bytes, -EAGAIN (-11) if there is nothing, or -ESRCH (-3)
/// if `pid` has no port.
pub fn recv(pid: u64, buf: u64, max: usize) -> i64 {
    let mut ps = PORTS.lock();
    match ps.iter_mut().find(|p| p.owner_pid == pid) {
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
    }
}

/// The recent IPC audit lines (for the system window).
pub fn audit_lines() -> Vec<String> {
    AUDIT.lock().clone()
}
