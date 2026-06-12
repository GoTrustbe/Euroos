//! Kernel-observability (Sprint S1 / Missing §1): in-memory **kmsg-ringbuffer** +
//! niveau-logging + rijke panic-context (registers + backtrace + recente historie).
//!
//! ALLE seriële output wordt mee in de ring gecaptured (een tee in `serial::_print`),
//! zodat `dmesg` en de panic-handler de recente kernelhistorie tonen zónder dat je
//! de seriële log hoeft te lezen. De ring is een vaste array (géén alloc in het
//! log-pad), dus veilig aan te roepen vanuit een IRQ of de panic-handler.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};

pub const LINES: usize = 512; // aantal bewaarde regels (ringgrootte)
pub const LINE_LEN: usize = 160; // max bytes per regel (afgekapt)
const MAX_CPU: usize = 8; // per-CPU partiële-regel-buffers (J1)

/// Bekende grenzen van het kernel-.text-segment (UEFI image base 0x1_4000_0000).
/// Gebruikt om bij de stack-scan/backtrace echte code-returnadressen te herkennen.
pub const KCODE_LO: u64 = 0x1_4000_0000;
pub const KCODE_HI: u64 = 0x1_4080_0000; // ruim boven de ~1.7 MiB image

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DBG",
            Level::Info => "INF",
            Level::Warn => "WRN",
            Level::Error => "ERR",
        }
    }
}

// ── J1: LOCK-VRIJE kmsg-ring ────────────────────────────────────────────────
// Geen globale Mutex meer op het log-pad (dat werd op ELKE seriële regel genomen,
// ook vanuit IRQ's en op meerdere cores → contentie + deadlock-risico in de panic-
// handler). In plaats daarvan:
//   • De committed-regels-ring is een MPSC-ring: een schrijver claimt een slot met
//     `HEAD.fetch_add(1)` (atomair, wacht-vrij) en schrijft in `LBUF[idx % LINES]`;
//     `LLEN[idx]` wordt met Release gepubliceerd. Verschillende cores claimen
//     verschillende slots → geen content-race binnen het ring-venster.
//   • De partiële (nog niet afgesloten) regel staat PER-CPU (`PCUR`/`PLEN`), zodat
//     elke core z'n eigen regel opbouwt zonder enige lock of cross-core-deling.
// Lezers (dmesg/panic) lezen lock-vrij → de panic-handler kan NOOIT blokkeren.
static HEAD: AtomicUsize = AtomicUsize::new(0); // totaal aantal ooit geschreven regels
static mut LBUF: [[u8; LINE_LEN]; LINES] = [[0; LINE_LEN]; LINES];
static LLEN: [AtomicU16; LINES] = [const { AtomicU16::new(0) }; LINES];
static mut PCUR: [[u8; LINE_LEN]; MAX_CPU] = [[0; LINE_LEN]; MAX_CPU];
static mut PLEN: [usize; MAX_CPU] = [0; MAX_CPU];
/// Pas ná `apic::init` mag de tee `lapic_id()` lezen (LAPIC-MMIO gemapt + cores live).
/// Daarvóór is alles single-core BSP → CPU-index 0.
static APIC_READY: AtomicBool = AtomicBool::new(false);

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Markeer dat de Local-APIC klaar is (door `interrupts::init_timer` aangeroepen).
pub fn mark_apic_ready() {
    APIC_READY.store(true, Ordering::Release);
}

/// De CPU-index voor de per-CPU partiële-regel-buffer (veilig vóór APIC-init: 0).
#[inline]
fn cpu_slot() -> usize {
    if APIC_READY.load(Ordering::Acquire) {
        (crate::apic::lapic_id() & (MAX_CPU as u32 - 1)) as usize
    } else {
        0
    }
}

/// Commit één volledige regel naar de lock-vrije ring (claim slot + publiceer len).
fn commit_line(bytes: &[u8]) {
    let n = bytes.len().min(LINE_LEN);
    let idx = HEAD.fetch_add(1, Ordering::Relaxed) % LINES;
    unsafe {
        let row = (core::ptr::addr_of_mut!(LBUF) as *mut u8).add(idx * LINE_LEN);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), row, n);
    }
    LLEN[idx].store(n as u16, Ordering::Release);
}

/// Een (raw) pointer naar de inhoud van ring-slot `i`.
#[inline]
fn line_ptr(i: usize) -> *const u8 {
    (core::ptr::addr_of!(LBUF) as *const u8).wrapping_add(i * LINE_LEN)
}

/// Aantal geldige regels in de ring + de start-index (oudste regel).
fn ring_view() -> (usize, usize) {
    let total = HEAD.load(Ordering::Acquire);
    let count = total.min(LINES);
    let start = (total - count) % LINES;
    (count, start)
}

/// Tee: elke seriële byte stroomt hier ook doorheen. We line-bufferen PER-CPU tot
/// '\n' en committen de regel dan lock-vrij naar de ring. '\r' wordt genegeerd.
pub fn tee(s: &str) {
    let cpu = cpu_slot();
    unsafe {
        // Raw pointers naar de per-CPU partiële regel (geen autoref op statics).
        let cur = (core::ptr::addr_of_mut!(PCUR) as *mut u8).add(cpu * LINE_LEN);
        let plen = (core::ptr::addr_of_mut!(PLEN) as *mut usize).add(cpu);
        for &b in s.as_bytes() {
            let l = *plen;
            if b == b'\n' {
                let mut tmp = [0u8; LINE_LEN];
                core::ptr::copy_nonoverlapping(cur, tmp.as_mut_ptr(), l);
                commit_line(&tmp[..l]);
                *plen = 0;
            } else if b != b'\r' && l < LINE_LEN {
                *cur.add(l) = b;
                *plen = l + 1;
            }
        }
    }
}

/// Structurele log-regel met niveau + uptime-tijdstempel. De tee in `serial::_print`
/// vangt deze automatisch in de ring; we hoeven hier dus niet apart te pushen.
pub fn record(level: Level, args: core::fmt::Arguments) {
    let _ = SEQ.fetch_add(1, Ordering::Relaxed);
    let t = crate::interrupts::ticks();
    crate::serial::_print(format_args!("[{:>5}.{:02} {}] {}\n", t / 100, t % 100, level.tag(), args));
}

/// Snapshot van de hele ring (oudste -> nieuwste) als losse strings. Alloc — alleen
/// vanuit een normale context (bv. het `dmesg`-shellcommando) aanroepen.
pub fn snapshot() -> alloc::vec::Vec<alloc::string::String> {
    let (count, start) = ring_view();
    let mut out = alloc::vec::Vec::with_capacity(count);
    for k in 0..count {
        let i = (start + k) % LINES;
        let n = LLEN[i].load(Ordering::Acquire) as usize;
        let line = unsafe { core::slice::from_raw_parts(line_ptr(i), n) };
        out.push(alloc::string::String::from_utf8_lossy(line).into_owned());
    }
    out
}

/// Roep `f` aan voor de laatste `n` ringregels (oudste -> nieuwste). Lock-vrij →
/// blokkeert NOOIT (cruciaal voor de panic-handler).
pub fn with_recent(n: usize, mut f: impl FnMut(&[u8])) {
    let (count, start) = ring_view();
    let cnt = count.min(n);
    let skip = count - cnt;
    for k in 0..cnt {
        let i = (start + skip + k) % LINES;
        let len = LLEN[i].load(Ordering::Acquire) as usize;
        let line = unsafe { core::slice::from_raw_parts(line_ptr(i), len) };
        f(line);
    }
}

/// J1-zelftest: bewijs de lock-vrije ring. Schrijf een burst regels (zoals vanuit
/// meerdere bronnen), en verifieer dat de HEAD-claim ze allemaal opnam en dat de
/// inhoud intact terug te lezen is. (De APs loggen bij boot óók via dit lock-vrije
/// pad — "core APIC-id N online" — dus echte cross-core-concurrency wordt al gedekt.)
pub fn lockfree_selftest() -> bool {
    let before = HEAD.load(Ordering::Acquire);
    for i in 0..64u32 {
        crate::serial_println!("[j1-kmsg] lock-vrije-ring-test-regel {i}");
    }
    let after = HEAD.load(Ordering::Acquire);
    // Tel hoeveel van onze test-regels intact in de ring staan.
    let mut found = 0;
    let snap = snapshot();
    for line in &snap {
        if line.starts_with("[j1-kmsg] lock-vrije-ring-test-regel ") {
            found += 1;
        }
    }
    let ok = after - before >= 64 && found >= 64;
    crate::serial_println!(
        "[j1] lock-vrije kmsg-ring: {} regels geclaimd (HEAD {}→{}), {} intact teruggelezen → {}",
        after - before, before, after, found,
        if ok { "OK (geen Mutex op het log-pad) ✓" } else { "MISLUKT" }
    );
    ok
}

/// Dump CPU-registers + een stack-backtrace naar de seriële poort. Wordt door de
/// panic-handler aangeroepen. Loopt eerst de RBP-keten af (force-frame-pointers is
/// aan), en valt terug op een stack-scan als de keten breekt.
pub fn dump_registers_and_backtrace() {
    let (rsp, rbp, cr2, cr3, rflags): (u64, u64, u64, u64, u64);
    unsafe {
        core::arch::asm!(
            "mov {0}, rsp",
            "mov {1}, rbp",
            "mov {2}, cr2",
            "mov {3}, cr3",
            "pushfq",
            "pop {4}",
            out(reg) rsp, out(reg) rbp, out(reg) cr2, out(reg) cr3, out(reg) rflags,
        );
    }
    crate::serial::_print(format_args!(
        "[panic] RSP={rsp:#018x} RBP={rbp:#018x} RFLAGS={rflags:#x}\n[panic] CR2={cr2:#018x} CR3={cr3:#018x}\n"
    ));

    // UEFI verplaatst de PE-image naar een RUNTIME-base (≠ link-base 0x1_4000_0000).
    // Leid het echte .text-bereik af uit het adres van deze functie zelf, anders
    // verwerpt de filter elk geldig returnadres. Het ANCHOR-adres (deze functie)
    // is het ijkpunt voor offline symbolisatie: `scripts/symbolize.sh`.
    let anchor = dump_registers_and_backtrace as usize as u64;
    let code_lo = anchor & !0x3F_FFFF; // 4 MiB omlaag uitgelijnd
    let code_hi = code_lo + 0x80_0000; // 8 MiB venster — dekt de hele kernel-.text
    let in_code = |a: u64| a >= code_lo && a < code_hi;
    crate::serial::_print(format_args!("[panic] anchor dump_registers_and_backtrace @ {anchor:#018x}\n"));

    // Backtrace: probeer eerst de RBP-keten ([rbp]=vorige rbp, [rbp+8]=returnadres).
    // Bij een paniek breekt die vaak op core::panicking-frames (zonder frame pointer),
    // dus vallen we terug op een stack-scan. Symboliseer ruwe adressen offline met
    // `scripts/symbolize.sh target/kernel.map <anchor> <addr...>`.
    crate::serial::_print(format_args!("[panic] backtrace (ruwe returnadressen):\n"));
    let mut bp = rbp;
    let mut frames = 0;
    while frames < 32 && bp >= rsp && bp < rsp + 0x20000 && bp & 0x7 == 0 {
        let ret = unsafe { ((bp + 8) as *const u64).read_volatile() };
        let next = unsafe { (bp as *const u64).read_volatile() };
        if in_code(ret) {
            crate::serial::_print(format_args!("  #{frames:<2} {ret:#018x}\n"));
            frames += 1;
        }
        if next <= bp {
            break; // keten loopt niet meer omhoog -> stop
        }
        bp = next;
    }
    if frames == 0 {
        // Terugval: scan de stack op code-adressen (frame pointers ontbraken).
        crate::serial::_print(format_args!("[panic] (RBP-keten leeg; stack-scan)\n"));
        let mut p = rsp;
        let mut shown = 0;
        let mut last = 0u64;
        while p < rsp + 0x4000 && shown < 24 {
            let v = unsafe { (p as *const u64).read_volatile() };
            if in_code(v) && v != last {
                crate::serial::_print(format_args!("  ? {v:#018x}\n"));
                shown += 1;
                last = v;
            }
            p += 8;
        }
    }
}

#[macro_export]
macro_rules! kinfo {
    ($($a:tt)*) => ($crate::klog::record($crate::klog::Level::Info, format_args!($($a)*)));
}
#[macro_export]
macro_rules! kwarn {
    ($($a:tt)*) => ($crate::klog::record($crate::klog::Level::Warn, format_args!($($a)*)));
}
#[macro_export]
macro_rules! kerr {
    ($($a:tt)*) => ($crate::klog::record($crate::klog::Level::Error, format_args!($($a)*)));
}
#[macro_export]
macro_rules! kdebug {
    ($($a:tt)*) => ($crate::klog::record($crate::klog::Level::Debug, format_args!($($a)*)));
}
