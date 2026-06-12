//! P3: **append-only audit-log** — tamper-evident vastlegging van veiligheids-events.
//!
//! GDPR/NIS2 vragen een betrouwbaar, niet-vervalsbaar logboek van wie wat deed. We
//! houden de events in een in-memory ring én persisteren ze naar
//! `/var/log/audit.log`, die met de L1-`FLAG_APPEND_ONLY`-vlag is gemarkeerd: het
//! filesysteem laat dan ALLEEN uitbreiding toe — eerdere regels kunnen niet gewist
//! of gewijzigd worden, zelfs niet door root. Het wissen van die vlag vereist
//! `CAP_IMMUTABLE_ADMIN` (L2). Zo is het audit-spoor structureel onomkeerbaar.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use eurofs::{FileSystem, FLAG_APPEND_ONLY};

/// Een veiligheids-event-categorie.
#[derive(Clone, Copy)]
pub enum Event {
    ImmutableSet,
    ImmutableCleared,
    ImmutableDenied,
    CapDenied,
    Login,
    Logout,
    Boot,
    /// Eén EuroAgent MCP-tool-aanroep (toegestaan of geweigerd). Het persistente
    /// spoor van wat een agent deed — overleeft een herstart (P0.3 / audit #7).
    AgentTool,
}

impl Event {
    fn tag(self) -> &'static str {
        match self {
            Event::ImmutableSet => "IMMUTABLE_SET",
            Event::ImmutableCleared => "IMMUTABLE_CLEARED",
            Event::ImmutableDenied => "IMMUTABLE_DENIED",
            Event::CapDenied => "CAP_DENIED",
            Event::Login => "LOGIN",
            Event::Logout => "LOGOUT",
            Event::Boot => "BOOT",
            Event::AgentTool => "AGENT_TOOL",
        }
    }
}

const LOG_PATH: &str = "/var/log/audit.log";

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Aantal in-memory events dat al naar schijf is geschreven (zodat we enkel de
/// NIEUWE events APPENDEN — de on-disk log groeit monotoon over boots heen).
static PERSISTED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Leg een event vast in de in-memory audit-ring (lock-beschermd; veilig vanuit elke
/// kernelcontext). Persisteren naar schijf doet [`persist`] later.
pub fn record(event: Event, detail: &str) {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let t = crate::interrupts::ticks();
    let line = format!("{seq:>6} t={t:>8} {} {detail}", event.tag());
    LOG.lock().push(line);
}

/// Aantal vastgelegde events.
pub fn count() -> usize {
    LOG.lock().len()
}

/// De laatste `n` audit-regels (voor een shell-`audit`-commando).
pub fn recent(n: usize) -> Vec<String> {
    let log = LOG.lock();
    let start = log.len().saturating_sub(n);
    log[start..].to_vec()
}

/// Persisteer de NIEUWE (nog niet weggeschreven) events naar de append-only
/// `/var/log/audit.log`: lees de bestaande inhoud (vorige boots + eerdere persists)
/// en APPEND enkel de nieuwe regels — zo breidt de write altijd uit (slaagt de
/// append-only-FS-controle) en groeit het spoor monotoon. Zet eenmalig de
/// `FLAG_APPEND_ONLY`-vlag (cap-gated via L2). Geeft true bij succes.
pub fn persist(fs: &mut dyn FileSystem, caps: u64) -> bool {
    use core::sync::atomic::Ordering;
    let _ = fs.create_dir("/var");
    let _ = fs.create_dir("/var/log");

    let (new_lines, total) = {
        let log = LOG.lock();
        let already = PERSISTED.load(Ordering::Relaxed).min(log.len());
        let mut s = Vec::new();
        for l in &log[already..] {
            s.extend_from_slice(l.as_bytes());
            s.push(b'\n');
        }
        (s, log.len())
    };
    if new_lines.is_empty() && fs.exists(LOG_PATH) {
        return true; // niets nieuws + bestand bestaat al
    }
    // Bestaande on-disk inhoud + de nieuwe events → strikt uitbreidende write.
    let mut body = fs.read_file(LOG_PATH).unwrap_or_default();
    body.extend_from_slice(&new_lines);
    if fs.write_file(LOG_PATH, &body).is_err() {
        return false;
    }
    PERSISTED.store(total, Ordering::Relaxed);
    if fs.get_flags(LOG_PATH).unwrap_or(0) & FLAG_APPEND_ONLY == 0 {
        let _ = crate::immutable::set_protected(fs, LOG_PATH, FLAG_APPEND_ONLY, caps);
    }
    true
}

/// P3-boot-zelftest: bewijs dat het audit-spoor onomkeerbaar is — events worden
/// vastgelegd, gepersisteerd naar een append-only bestand, en een poging het te
/// vervalsen (inkorten/overschrijven) wordt door de FS geweigerd.
pub fn selftest(fs: &mut dyn FileSystem, caps: u64) {
    let nl = |fs: &mut dyn FileSystem| fs.read_file(LOG_PATH).map(|d| d.iter().filter(|&&b| b == b'\n').count()).unwrap_or(0);
    record(Event::Boot, "kernel-start");
    record(Event::Login, "user=root tty=console");
    let persisted = persist(fs, caps);
    let append_only = fs.get_flags(LOG_PATH).unwrap_or(0) & FLAG_APPEND_ONLY != 0;
    let lines_before = nl(fs);

    // Tamper-poging: het log inkorten of overschrijven → de append-only-FS weigert.
    let tamper_blocked = fs.write_file(LOG_PATH, b"X").is_err();

    // Een nieuw event + her-persist MOET wél slagen (het breidt enkel uit) en de
    // on-disk log groeit (werkt ook over reboots, want we appenden i.p.v. herschrijven).
    record(Event::ImmutableSet, "/bin/hello");
    let extend_ok = persist(fs, caps);
    let lines_after = nl(fs);

    let ok = persisted && append_only && tamper_blocked && extend_ok && lines_after > lines_before;
    crate::serial_println!(
        "[p3] append-only audit-log: {} events, gepersisteerd={}, append-only-vlag={}, vervalsing-geblokkeerd={}, uitbreiden-OK={}, regels-op-schijf {}→{} → {}",
        count(), persisted, append_only, tamper_blocked, extend_ok, lines_before, lines_after,
        if ok { "OK (tamper-evident audit-spoor) ✓" } else { "MISLUKT" }
    );
}
