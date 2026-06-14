//! EuroInit (Sprint S4 / Missing §11): the service supervisor (PID-1 role). Starts
//! declared services, KEEPS AN EYE ON THEM and restarts them according to policy; the
//! `flush_log` function (eurologd) writes the kmsg ring periodically and persistently to
//! /var/log/messages. The supervision tick runs in the desktop loop, where the frame
//! allocator + the filesystem are available.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use eurofs::FileSystem;
use euromm::FrameAllocator;
use spin::Mutex;

/// Restart policy of a service.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// One-shot (no restart).
    Never,
    /// Restart on every exit (within the ceiling).
    Always,
}

pub struct Service {
    pub name: &'static str,
    pub bin: &'static str,   // path in the VFS (/bin/...)
    pub restart: Restart,
    pub pid: u64,            // current pid (0 = no longer tracked)
    pub starts: u32,         // number of times started
    pub max_starts: u32,     // restart ceiling (anti-storm)
}

static SERVICES: Mutex<Vec<Service>> = Mutex::new(Vec::new());
static SVC_PID: AtomicU64 = AtomicU64::new(100); // service pids 100+
static SUP_TICK: AtomicU64 = AtomicU64::new(0);

/// Register the default services (the declarative definition; later /etc/services).
fn register_defaults() {
    let mut s = SERVICES.lock();
    if !s.is_empty() {
        return;
    }
    // ticker: proves the SUPERVISION — it exits itself and EuroInit restarts it
    // up to the ceiling, with log lines per restart.
    s.push(Service { name: "ticker", bin: "/bin/ticker", restart: Restart::Always, pid: 0, starts: 0, max_starts: 3 });
}

fn spawn_service(falloc: &mut FrameAllocator, fs: &mut dyn FileSystem, svc: &mut Service) {
    let bytes = fs.read_file(svc.bin).unwrap_or_default();
    if bytes.is_empty() || !crate::ring3::verify_program(svc.bin, &bytes) {
        crate::kwarn!("[init] service {} cannot start ({} missing/invalid)", svc.name, svc.bin);
        svc.pid = 0;
        return;
    }
    let pid = SVC_PID.fetch_add(1, Ordering::Relaxed);
    crate::ring3::spawn_bg_musl(falloc, &bytes, pid, svc.name.as_bytes());
    svc.pid = pid;
    svc.starts += 1;
    crate::kinfo!("[init] service {} started (pid {}, start #{})", svc.name, pid, svc.starts);
}

/// Start all declared services at boot.
pub fn start_all(falloc: &mut FrameAllocator, fs: &mut dyn FileSystem) {
    register_defaults();
    let mut svcs = SERVICES.lock();
    for svc in svcs.iter_mut() {
        spawn_service(falloc, fs, svc);
    }
    crate::kinfo!("[init] EuroInit active — {} service(s) under supervision", svcs.len());
}

/// Supervision tick: restart stopped services according to policy. Called by the
/// desktop loop after `reap_dead`.
pub fn supervise(falloc: &mut FrameAllocator, fs: &mut dyn FileSystem) {
    let mut svcs = SERVICES.lock();
    for svc in svcs.iter_mut() {
        if svc.pid != 0 && !crate::ring3::is_pid_alive(svc.pid) {
            let may_restart = svc.restart == Restart::Always && svc.starts < svc.max_starts;
            if may_restart {
                crate::kinfo!("[init] service {} stopped -> restart ({}/{})", svc.name, svc.starts, svc.max_starts);
                spawn_service(falloc, fs, svc);
            } else {
                crate::kinfo!("[init] service {} stopped — no restart (ceiling/policy)", svc.name);
                svc.pid = 0; // no longer track
            }
        }
    }
}

/// eurologd: write the kmsg ring periodically to /var/log/messages (persistent).
/// Not every frame — about every ~256 supervision ticks.
pub fn flush_log(fs: &mut dyn FileSystem) {
    if SUP_TICK.fetch_add(1, Ordering::Relaxed) % 256 != 0 {
        return;
    }
    let mut blob = String::new();
    for l in crate::klog::snapshot() {
        blob.push_str(&l);
        blob.push('\n');
    }
    let _ = fs.write_file("/var/log/messages", blob.as_bytes());
}

/// Status lines for the `services` shell command (`euroctl status` equivalent).
pub fn status_lines() -> Vec<String> {
    let svcs = SERVICES.lock();
    let mut out = Vec::with_capacity(svcs.len() + 1);
    out.push(String::from("SERVICE     STATUS    PID    STARTS  POLICY"));
    for s in svcs.iter() {
        let st = if s.pid != 0 && crate::ring3::is_pid_alive(s.pid) { "running" } else { "stopped" };
        let pol = if s.restart == Restart::Always { "always" } else { "never" };
        out.push(alloc::format!("{:<11} {:<9} {:<6} {:<7} {}", s.name, st, s.pid, s.starts, pol));
    }
    out
}
