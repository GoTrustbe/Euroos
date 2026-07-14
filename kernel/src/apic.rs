//! Local APIC + APIC timer (Track 3.5) — replaces the 8259-PIT as scheduler tick.
//!
//! The LAPIC MMIO sits at physical `0xFEE00000` (identity-mapped supervisor, in the
//! 3–4 GiB 1-GiB huge page of [`crate::paging`]). We:
//!   1. enable the LAPIC (IA32_APIC_BASE bit 11 + software-enable),
//!   2. keep the 8259 IRQs (keyboard/mouse) alive via LINT0=ExtINT
//!      (virtual-wire), so PS/2 keeps working,
//!   3. calibrate the timer against PIT channel 2 and set it periodic at `hz` Hz.
//!
//! This is the first step toward SMP (multiple cores): the Local APIC is per-CPU
//! and the IO-APIC + AP bring-up build on top of it.

use x86_64::instructions::port::Port;
use x86_64::registers::model_specific::Msr;

const LAPIC_BASE: u64 = 0xFEE0_0000;

// Register offsets (bytes from the base).
const REG_ID: u64 = 0x020;
const REG_EOI: u64 = 0x0B0;
const REG_SPURIOUS: u64 = 0x0F0;
const REG_LVT_LINT0: u64 = 0x350;
const REG_LVT_LINT1: u64 = 0x360;
const REG_LVT_TIMER: u64 = 0x320;
const REG_TIMER_INIT: u64 = 0x380;
const REG_TIMER_CUR: u64 = 0x390;
const REG_TIMER_DIV: u64 = 0x3E0;
const REG_ICR_LOW: u64 = 0x300;
const REG_ICR_HIGH: u64 = 0x310;

const IA32_APIC_BASE: u32 = 0x1B;

const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const APIC_SW_ENABLE: u32 = 1 << 8;
const DELIVERY_EXTINT: u32 = 0b111 << 8;
const DELIVERY_NMI: u32 = 0b100 << 8;

#[inline]
unsafe fn rd(off: u64) -> u32 {
    ((LAPIC_BASE + off) as *const u32).read_volatile()
}
#[inline]
unsafe fn wr(off: u64, v: u32) {
    ((LAPIC_BASE + off) as *mut u32).write_volatile(v);
}

/// Local-APIC-ID of the current CPU (the BSP when single-core).
pub fn lapic_id() -> u32 {
    unsafe { rd(REG_ID) >> 24 }
}

// ── IO-APIC (IRQ routing, replaces the 8259-PIC) ──────────────────────────
unsafe fn ioapic_write(base: u64, reg: u32, val: u32) {
    (base as *mut u32).write_volatile(reg); // IOREGSEL
    ((base + 0x10) as *mut u32).write_volatile(val); // IOWIN
}

/// Route a GSI (global system interrupt) to `vector` on core `dest_apic`.
/// ISA-IRQs are edge-triggered, active-high; we set the redirection entry
/// (low = vector/fixed/physical/unmasked, high = destination APIC id).
pub fn ioapic_route(ioapic_base: u32, gsi: u32, vector: u8, dest_apic: u8) {
    let base = ioapic_base as u64;
    let low = 0x10 + 2 * gsi;
    let high = 0x11 + 2 * gsi;
    unsafe {
        ioapic_write(base, low, 1 << 16); // mask first
        ioapic_write(base, high, (dest_apic as u32) << 24);
        ioapic_write(base, low, vector as u32); // fixed, physical, edge, active-high, unmasked
    }
}

/// Route a GSI as **level-triggered, active-low** (the ACPI SCI convention).
pub fn ioapic_route_level_low(ioapic_base: u32, gsi: u32, vector: u8, dest_apic: u8) {
    let base = ioapic_base as u64;
    let low = 0x10 + 2 * gsi;
    let high = 0x11 + 2 * gsi;
    unsafe {
        ioapic_write(base, low, 1 << 16); // mask first
        ioapic_write(base, high, (dest_apic as u32) << 24);
        // vector | active-low polarity (bit13) | level trigger (bit15), unmasked.
        ioapic_write(base, low, vector as u32 | (1 << 13) | (1 << 15));
    }
}

/// End-Of-Interrupt to the Local APIC (the timer vector EOIs here instead of the PIC).
#[inline]
pub fn eoi() {
    unsafe { wr(REG_EOI, 0) };
}

/// Number of LAPIC timer ticks per `hz` period (result of the calibration).
static mut CAL_COUNT: u32 = 0;

/// Enable the LAPIC and start the periodic timer at `hz` Hz, interrupt `vector`.
/// Returns the calibrated initial count value (diagnostics).
pub fn init(hz: u32, vector: u8) -> u32 {
    unsafe {
        // 1. Global enable via IA32_APIC_BASE (bit 11) — firmware usually sets it already.
        let mut base_msr = Msr::new(IA32_APIC_BASE);
        let base = base_msr.read();
        base_msr.write(base | (1 << 11));

        // 2. Software-enable + spurious vector 0xFF.
        wr(REG_SPURIOUS, APIC_SW_ENABLE | 0xFF);

        // 3. Virtual-wire: LINT0 = ExtINT (let 8259 kbd/mouse through), LINT1 = NMI.
        wr(REG_LVT_LINT0, DELIVERY_EXTINT);
        wr(REG_LVT_LINT1, DELIVERY_NMI);

        // 4. Calibrate against PIT channel 2 and start the periodic timer.
        let count = calibrate(hz);
        CAL_COUNT = count;
        wr(REG_TIMER_DIV, 0x3); // 0b011 = divide by 16
        wr(REG_LVT_TIMER, LVT_PERIODIC | vector as u32);
        wr(REG_TIMER_INIT, count);
        count
    }
}

/// Enable the Local APIC of THIS cpu and start its periodic timer on `vector`,
/// with the count value already calibrated (by the BSP). For the APs.
pub fn start_timer_on_this_cpu(vector: u8) {
    unsafe {
        let mut base = Msr::new(IA32_APIC_BASE);
        let b = base.read();
        base.write(b | (1 << 11));
        wr(REG_SPURIOUS, APIC_SW_ENABLE | 0xFF);
        let count = core::ptr::addr_of!(CAL_COUNT).read();
        wr(REG_TIMER_DIV, 0x3);
        wr(REG_LVT_TIMER, LVT_PERIODIC | vector as u32);
        wr(REG_TIMER_INIT, if count == 0 { 1_000_000 } else { count });
    }
}

/// Send an ordinary (fixed-delivery) inter-processor interrupt to `apic_id` on
/// `vector`. For cross-CPU signaling (reschedule, halt, TLB shootdown).
pub fn send_ipi(apic_id: u8, vector: u8) {
    unsafe {
        wr(REG_ICR_HIGH, (apic_id as u32) << 24);
        wr(REG_ICR_LOW, 0x0000_4000 | vector as u32); // fixed, assert, edge, physical dest
        wait_icr_idle();
    }
}

/// Send an INIT-IPI to core `apic_id` (physical destination mode).
pub fn send_init(apic_id: u8) {
    unsafe {
        wr(REG_ICR_HIGH, (apic_id as u32) << 24);
        wr(REG_ICR_LOW, 0x0000_4500); // INIT, assert, edge
        wait_icr_idle();
    }
}

/// Send a Startup-IPI (SIPI) with trampoline start page `vector` (phys >> 12).
pub fn send_sipi(apic_id: u8, vector: u8) {
    unsafe {
        wr(REG_ICR_HIGH, (apic_id as u32) << 24);
        wr(REG_ICR_LOW, 0x0000_4600 | vector as u32); // Startup
        wait_icr_idle();
    }
}

unsafe fn wait_icr_idle() {
    // ICR bit 12 (delivery status) stays 1 while the IPC is in flight.
    let mut guard = 0u32;
    while rd(REG_ICR_LOW) & (1 << 12) != 0 {
        core::hint::spin_loop();
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
    }
}

/// Busy-wait ~`us` microseconds via the running LAPIC timer (current-count),
/// independent of interrupts (which are still off during SMP bring-up).
pub fn busy_wait_us(us: u32) {
    let cal = unsafe { core::ptr::addr_of!(CAL_COUNT).read() };
    if cal == 0 {
        for _ in 0..(us as u64 * 300) {
            core::hint::spin_loop();
        }
        return;
    }
    // cal ticks = 1 period = 10_000 us (100 Hz). want = cal * us / 10_000.
    let want = (cal as u64 * us as u64) / 10_000;
    unsafe {
        let mut last = rd(REG_TIMER_CUR);
        let mut elapsed = 0u64;
        let mut guard = 0u64;
        while elapsed < want {
            let cur = rd(REG_TIMER_CUR);
            // The counter counts DOWN; on a reload (cur > last) it has wrapped.
            elapsed += if cur <= last {
                (last - cur) as u64
            } else {
                last as u64 + (cal as u64 - cur as u64)
            };
            last = cur;
            core::hint::spin_loop();
            guard += 1;
            if guard > 200_000_000 {
                break;
            }
        }
    }
}

/// Measure how many LAPIC timer ticks fit in one `hz` period via PIT channel 2
/// (mode 0, ~`1/hz` s one-shot, polled on the OUT2 status bit).
unsafe fn calibrate(hz: u32) -> u32 {
    let mut p61 = Port::<u8>::new(0x61);
    // Gate on (bit0), speaker off (bit1=0).
    let v = (p61.read() & 0xFC) | 0x01;
    p61.write(v);

    let pit_count = (1_193_182u32 / hz) as u16;
    let mut cmd = Port::<u8>::new(0x43);
    let mut ch2 = Port::<u8>::new(0x42);
    cmd.write(0xB0); // ch2, lobyte+hibyte, mode 0
    ch2.write((pit_count & 0xFF) as u8);
    ch2.write((pit_count >> 8) as u8);

    // Let the LAPIC timer run to maximum.
    wr(REG_TIMER_DIV, 0x3);
    wr(REG_TIMER_INIT, 0xFFFF_FFFF);

    // Wait until PIT-ch2 reaches its terminal count (OUT2 = 0x61 bit5 goes high).
    let mut guard = 0u32;
    while (p61.read() & 0x20) == 0 {
        guard += 1;
        if guard > 50_000_000 {
            break; // safety valve against a hanging calibration
        }
    }

    wr(REG_LVT_TIMER, LVT_MASKED);
    let elapsed = 0xFFFF_FFFFu32 - rd(REG_TIMER_CUR);
    if elapsed < 1000 {
        1_000_000 // fallback on a failed calibration
    } else {
        elapsed
    }
}
