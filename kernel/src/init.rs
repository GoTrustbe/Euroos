//! EuroInit (Sprint S4 / Missing §11): de service-supervisor (PID-1-rol). Start
//! gedeclareerde services, HOUDT ZE IN DE GATEN en herstart ze volgens beleid; de
//! `flush_log`-functie (eurologd) schrijft de kmsg-ring periodiek persistent naar
//! /var/log/messages. De supervisie-tick draait in de desktop-lus, waar de frame-
//! allocator + het filesysteem beschikbaar zijn.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use eurofs::FileSystem;
use euromm::FrameAllocator;
use spin::Mutex;

/// Herstartbeleid van een service.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// Eénmalig (geen herstart).
    Never,
    /// Herstart bij elke exit (binnen het plafond).
    Always,
}

pub struct Service {
    pub name: &'static str,
    pub bin: &'static str,   // pad in de VFS (/bin/...)
    pub restart: Restart,
    pub pid: u64,            // huidige pid (0 = niet (meer) gevolgd)
    pub starts: u32,         // aantal keren gestart
    pub max_starts: u32,     // herstart-plafond (anti-storm)
}

static SERVICES: Mutex<Vec<Service>> = Mutex::new(Vec::new());
static SVC_PID: AtomicU64 = AtomicU64::new(100); // service-pids 100+
static SUP_TICK: AtomicU64 = AtomicU64::new(0);

/// Registreer de standaard-services (de declaratieve definitie; later /etc/services).
fn register_defaults() {
    let mut s = SERVICES.lock();
    if !s.is_empty() {
        return;
    }
    // ticker: bewijst de SUPERVISIE — het sluit zichzelf af en EuroInit herstart het
    // tot het plafond, met logregels per herstart.
    s.push(Service { name: "ticker", bin: "/bin/ticker", restart: Restart::Always, pid: 0, starts: 0, max_starts: 3 });
}

fn spawn_service(falloc: &mut FrameAllocator, fs: &mut dyn FileSystem, svc: &mut Service) {
    let bytes = fs.read_file(svc.bin).unwrap_or_default();
    if bytes.is_empty() || !crate::ring3::verify_program(svc.bin, &bytes) {
        crate::kwarn!("[init] service {} kan niet starten ({} ontbreekt/ongeldig)", svc.name, svc.bin);
        svc.pid = 0;
        return;
    }
    let pid = SVC_PID.fetch_add(1, Ordering::Relaxed);
    crate::ring3::spawn_bg_musl(falloc, &bytes, pid, svc.name.as_bytes());
    svc.pid = pid;
    svc.starts += 1;
    crate::kinfo!("[init] service {} gestart (pid {}, start #{})", svc.name, pid, svc.starts);
}

/// Start alle gedeclareerde services bij boot.
pub fn start_all(falloc: &mut FrameAllocator, fs: &mut dyn FileSystem) {
    register_defaults();
    let mut svcs = SERVICES.lock();
    for svc in svcs.iter_mut() {
        spawn_service(falloc, fs, svc);
    }
    crate::kinfo!("[init] EuroInit actief — {} service(s) onder supervisie", svcs.len());
}

/// Supervisie-tick: herstart gestopte services volgens beleid. Door de desktop-lus
/// na `reap_dead` aangeroepen.
pub fn supervise(falloc: &mut FrameAllocator, fs: &mut dyn FileSystem) {
    let mut svcs = SERVICES.lock();
    for svc in svcs.iter_mut() {
        if svc.pid != 0 && !crate::ring3::is_pid_alive(svc.pid) {
            let may_restart = svc.restart == Restart::Always && svc.starts < svc.max_starts;
            if may_restart {
                crate::kinfo!("[init] service {} gestopt -> herstart ({}/{})", svc.name, svc.starts, svc.max_starts);
                spawn_service(falloc, fs, svc);
            } else {
                crate::kinfo!("[init] service {} gestopt — geen herstart (plafond/beleid)", svc.name);
                svc.pid = 0; // niet meer volgen
            }
        }
    }
}

/// eurologd: schrijf de kmsg-ring periodiek naar /var/log/messages (persistent).
/// Niet elke frame — om de ~256 supervisie-ticks.
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

/// Statusregels voor het `services`-shellcommando (`euroctl status`-equivalent).
pub fn status_lines() -> Vec<String> {
    let svcs = SERVICES.lock();
    let mut out = Vec::with_capacity(svcs.len() + 1);
    out.push(String::from("SERVICE     STATUS    PID    STARTS  BELEID"));
    for s in svcs.iter() {
        let st = if s.pid != 0 && crate::ring3::is_pid_alive(s.pid) { "draait" } else { "gestopt" };
        let pol = if s.restart == Restart::Always { "always" } else { "never" };
        out.push(alloc::format!("{:<11} {:<9} {:<6} {:<7} {}", s.name, st, s.pid, s.starts, pol));
    }
    out
}
