//! EuroIPC — een eenvoudige message-bus tussen processen (Horizon A).
//!
//! Processen claimen een poort (een geheeltallige endpoint), sturen berichten
//! naar een poort, en ontvangen van hun eigen poort. Elk bericht draagt de
//! **app-identiteit** van de afzender (pid) en wordt **geaudit**. De
//! permissie-check is nu een open hook (alles toegestaan) — de koppeling met de
//! EuroGuard-policy is een volgende stap. no_std + alloc, kernel-intern.

use alloc::string::String;
use alloc::vec::Vec;

use spin::Mutex;

struct Port {
    port: u32,
    owner_pid: u64,
    queue: Vec<(u64, Vec<u8>)>, // (afzender-pid, data)
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

/// Claim een poort voor `pid`. 1 = ok, 0 = al in gebruik.
pub fn register(pid: u64, port: u32) -> i64 {
    let mut ps = PORTS.lock();
    if ps.iter().any(|p| p.port == port) {
        return 0;
    }
    ps.push(Port { port, owner_pid: pid, queue: Vec::new() });
    drop(ps);
    audit(alloc::format!("pid {pid} claimde port {port}"));
    1
}

/// Stuur `data` naar `port`. Geeft het aantal bytes terug, of -ESRCH (-3) als de
/// poort niet bestaat. Tagt het bericht met de afzender-pid + audit.
pub fn send(sender_pid: u64, port: u32, data: &[u8]) -> i64 {
    let mut ps = PORTS.lock();
    let owner = match ps.iter_mut().find(|p| p.port == port) {
        Some(p) => {
            // permissie-check-hook (nu: toegestaan) — later EuroGuard-policy.
            p.queue.push((sender_pid, data.to_vec()));
            p.owner_pid
        }
        None => return -3,
    };
    drop(ps);
    audit(alloc::format!("pid {sender_pid} -> port {port} (pid {owner}): {} bytes", data.len()));
    data.len() as i64
}

/// Ontvang één bericht van de poort van `pid` naar `buf` (max `max` bytes).
/// Geeft het aantal bytes terug, -EAGAIN (-11) als er niets is, of -ESRCH (-3)
/// als `pid` geen poort heeft.
pub fn recv(pid: u64, buf: u64, max: usize) -> i64 {
    let mut ps = PORTS.lock();
    match ps.iter_mut().find(|p| p.owner_pid == pid) {
        Some(p) => {
            if p.queue.is_empty() {
                return -11;
            }
            let (_, data) = p.queue.remove(0);
            let n = data.len().min(max);
            // SAFETY: buf ligt in de USER-arena van het ontvangende proces.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf as *mut u8, n) };
            n as i64
        }
        None => -3,
    }
}

/// De recente IPC-audit-regels (voor het systeemvenster).
pub fn audit_lines() -> Vec<String> {
    AUDIT.lock().clone()
}
