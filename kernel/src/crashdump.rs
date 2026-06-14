//! Kernel side of **EuroCrash** (plan Y): on a fatal exception/panic, write
//! a minidump to a reserved disk block, and read it back on the next boot
//! (recovery mode). Builds on G1 (the PF/DF already run on their own IST stacks, so
//! we can still write a dump even on stack exhaustion).

use core::sync::atomic::{AtomicU64, Ordering};

use eurocrash::CrashDump;

/// Reserved disk block for the minidump (GPT gap, after j2-spares/j3-swap/j3-fault).
const CRASH_LBA: u64 = 300;
/// Sentinel vector for the boot self-test dump (distinguishes it from a real crash).
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

/// Write a dump to the crash block (best-effort; fails silently if there is no disk).
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

/// Capture a fatal exception: record the register state + write the dump. Called by
/// the fault handlers just before `halt`.
pub fn capture(vector: u8, error_code: u64, rip: u64, rsp: u64, rflags: u64) {
    let mut d = CrashDump::new(vector, error_code, rip, rsp, rflags);
    d.cr2 = read_cr2();
    write(d);
}

/// Read the last written dump (recovery). None = no valid dump.
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

/// Boot self-test + recovery: show any dump from the previous boot, and prove the
/// dump write/read cycle (cross-boot persistent).
pub fn selftest() {
    if !crate::virtio_blk::present() {
        return;
    }
    // (1) Recovery: was there a dump from the previous boot?
    let prev = read_last();
    if let Some(d) = prev {
        NEXT_SEQ.store(d.seq + 1, Ordering::Relaxed);
        if d.vector == TEST_VECTOR {
            crate::serial_println!(
                "[y] EuroCrash recovery: previous-boot test dump found (seq {}, rip {:#x}) — dump trace survives reboots ✓",
                d.seq, d.rip
            );
        } else {
            crate::serial_println!(
                "[y] EuroCrash recovery: ⚠ previous boot ended with a REAL crash — {} @ rip {:#x}, error {:#x}, cr2 {:#x}",
                d.vector_name(), d.rip, d.error_code, d.cr2
            );
        }
    }
    // (2) Write a fresh test dump (synthetic #PF-like state) + read it back.
    let mut t = CrashDump::new(TEST_VECTOR, 0x2, 0x1_4000_ABCD, 0xFFFF_8000_0000_1000, 0x202);
    t.cr2 = 0xCAFE_BABE;
    t.regs[0] = 0xA11CE;
    t.build_hash = 0xEED0_BEEF;
    write(t);
    let back = read_last();
    let ok = back.map(|b| b.vector == TEST_VECTOR && b.cr2 == 0xCAFE_BABE && b.regs[0] == 0xA11CE).unwrap_or(false);
    crate::serial_println!(
        "[y] EuroCrash: minidump write→disk(LBA {})→read, round-trip-intact={} → {}",
        CRASH_LBA, ok,
        if ok { "OK (crash dumps + recovery boot work) ✓" } else { "FAILED" }
    );
}
