//! Kernel-zijde van **EuroCrash** (plan Y): schrijf bij een fatale exceptie/paniek
//! een minidump naar een gereserveerd schijf-blok, en lees 'm bij de volgende boot
//! terug (recovery-modus). Bouwt op G1 (de PF/DF lopen al op eigen IST-stacks, dus
//! we kunnen ook bij stack-uitputting nog een dump schrijven).

use core::sync::atomic::{AtomicU64, Ordering};

use eurocrash::CrashDump;

/// Gereserveerd schijf-blok voor de minidump (GPT-gat, ná j2-spares/j3-swap/j3-fault).
const CRASH_LBA: u64 = 300;
/// Sentinel-vector voor de boot-zelftest-dump (onderscheidt 'm van een echte crash).
const TEST_VECTOR: u8 = 0xFE;

static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

#[inline]
fn read_cr2() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) v, options(nostack, nomem)) };
    v
}
#[inline]
fn read_cr3() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) v, options(nostack, nomem)) };
    v
}

/// Schrijf een dump naar het crash-blok (best-effort; faalt stil als er geen schijf is).
pub fn write(mut dump: CrashDump) {
    dump.seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
    dump.cr3 = read_cr3();
    dump.uptime_ms = crate::interrupts::ticks() * 10; // 100 Hz → ms
    let enc = dump.encode();
    if crate::virtio_blk::present() {
        crate::virtio_blk::write_sector(CRASH_LBA, &enc);
        crate::virtio_blk::flush();
    }
}

/// Vang een fatale exceptie: leg de registerstaat vast + schrijf de dump. Door de
/// fault-handlers aangeroepen vlak vóór `halt`.
pub fn capture(vector: u8, error_code: u64, rip: u64, rsp: u64, rflags: u64) {
    let mut d = CrashDump::new(vector, error_code, rip, rsp, rflags);
    d.cr2 = read_cr2();
    write(d);
}

/// Lees de laatst geschreven dump (recovery). None = geen geldige dump.
pub fn read_last() -> Option<CrashDump> {
    if !crate::virtio_blk::present() {
        return None;
    }
    let mut b = [0u8; 512];
    if !crate::virtio_blk::read_sector(CRASH_LBA, &mut b) {
        return None;
    }
    CrashDump::decode(&b)
}

/// Boot-zelftest + recovery: toon een eventuele dump van de vorige boot, en bewijs de
/// dump-schrijf/lees-cyclus (cross-boot persistent).
pub fn selftest() {
    if !crate::virtio_blk::present() {
        return;
    }
    // (1) Recovery: stond er een dump van de vorige boot?
    let prev = read_last();
    if let Some(d) = prev {
        NEXT_SEQ.store(d.seq + 1, Ordering::Relaxed);
        if d.vector == TEST_VECTOR {
            crate::serial_println!(
                "[y] EuroCrash recovery: vorige-boot test-dump gevonden (seq {}, rip {:#x}) — dump-spoor overleeft reboots ✓",
                d.seq, d.rip
            );
        } else {
            crate::serial_println!(
                "[y] EuroCrash recovery: ⚠ vorige boot eindigde met een ECHTE crash — {} @ rip {:#x}, error {:#x}, cr2 {:#x}",
                d.vector_name(), d.rip, d.error_code, d.cr2
            );
        }
    }
    // (2) Schrijf een verse test-dump (synthetische #PF-achtige staat) + lees 'm terug.
    let mut t = CrashDump::new(TEST_VECTOR, 0x2, 0x1_4000_ABCD, 0xFFFF_8000_0000_1000, 0x202);
    t.cr2 = 0xCAFE_BABE;
    t.regs[0] = 0xA11CE;
    t.build_hash = 0xEED0_BEEF;
    write(t);
    let back = read_last();
    let ok = back.map(|b| b.vector == TEST_VECTOR && b.cr2 == 0xCAFE_BABE && b.regs[0] == 0xA11CE).unwrap_or(false);
    crate::serial_println!(
        "[y] EuroCrash: minidump schrijf→schijf(LBA {})→lees, round-trip-intact={} → {}",
        CRASH_LBA, ok,
        if ok { "OK (crash-dumps + recovery-boot werken) ✓" } else { "MISLUKT" }
    );
}
