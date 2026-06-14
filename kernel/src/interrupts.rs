//! Interrupt Descriptor Table + exception handlers (Track 3.3).
//!
//! We run (for now) with hardware interrupts OFF and poll the keyboard;
//! only CPU exceptions (non-maskable) arrive here. That keeps the
//! first kernel mode simple and robust — APIC timer + scheduler is a
//! later phase.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pic8259::ChainedPics;
use spin::{Lazy, Mutex};
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::gdt::{DOUBLE_FAULT_IST_INDEX, PAGE_FAULT_IST_INDEX};
use crate::serial_println;

/// Set by the breakpoint handler — proof that the IDT works.
pub static BREAKPOINT_HIT: AtomicBool = AtomicBool::new(false);

// ── Hardware interrupts (PIC remap to 0x20+) ────────────────────────────
const PIC1_OFFSET: u8 = 0x20;
const PIC2_OFFSET: u8 = 0x28;
const TIMER_VECTOR: u8 = PIC1_OFFSET; // IRQ0
const KEYBOARD_VECTOR: u8 = PIC1_OFFSET + 1; // IRQ1
const MOUSE_VECTOR: u8 = PIC2_OFFSET + 4; // IRQ12 (on the slave PIC)

static PICS: Mutex<ChainedPics> = Mutex::new(unsafe { ChainedPics::new(PIC1_OFFSET, PIC2_OFFSET) });

/// Timer ticks since interrupts are enabled (~100 Hz).
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// Separate vector for the per-CPU AP timer (apart from the BSP scheduler stub on 0x20).
const AP_TIMER_VECTOR: u8 = 0x41;
// Cross-CPU IPI vectors (Run 2).
pub const IPI_PING_VECTOR: u8 = 0x43; // generic ping / wakeup
pub const IPI_HALT_VECTOR: u8 = 0x44; // stop a core (cli; hlt)
pub const IPI_TLB_VECTOR: u8 = 0x45; // TLB shootdown (reload CR3)
pub const XHCI_MSIX_VECTOR: u8 = 0x46; // xHCI event ring via MSI-X (J2)
/// Number of received xHCI MSI-X interrupts (proves MSI-X delivery).
pub static XHCI_MSIX_COUNT: AtomicU64 = AtomicU64::new(0);
pub const VIRTIO_BLK_MSIX_VECTOR: u8 = 0x47; // virtio-blk completion via MSI-X (J2)
/// Number of received virtio-blk completion MSI-X interrupts.
pub static BLK_MSIX_COUNT: AtomicU64 = AtomicU64::new(0);
/// Per-CPU counter of received ping IPIs (proves cross-CPU signaling).
pub static IPI_COUNT: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
/// Per-CPU counter of handled TLB shootdowns.
pub static TLB_COUNT: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Initialize the timer chain: 8259 PIC for keyboard/mouse, and the **Local
/// APIC timer** (instead of the PIT) as scheduler tick at `hz` Hz. The PIT IRQ0 is
/// masked; the APIC timer fires on the same vector (0x20 → scheduler stub).
/// Interrupts are NOT yet enabled here (the caller calls `enable()`).
pub fn init_timer(hz: u32) {
    unsafe {
        PICS.lock().initialize();
        // Master: mask IRQ0 (PIT timer, replaced by the APIC timer); keep
        // IRQ1 (keyboard) + IRQ2 (cascade) enabled. Slave: IRQ12 (mouse, bit 4).
        PICS.lock().write_masks(0xF9, 0xEF);
    }
    // Start the Local APIC timer; it now provides the scheduler tick.
    let count = crate::apic::init(hz, TIMER_VECTOR);
    // J1: from now on the lock-free kmsg tee may read `lapic_id()` (LAPIC mapped).
    crate::klog::mark_apic_ready();
    serial_println!(
        "[apic] Local APIC #{} on — timer {hz} Hz (calibrated: {count} ticks/period)",
        crate::apic::lapic_id()
    );
}

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
    idt.general_protection_fault.set_handler_fn(gp_handler);
    unsafe {
        // G1: the page-fault handler on its own IST stack, so that a kernel stack
        // overflow (fault on the guard page) is handled instead of a double fault.
        idt.page_fault
            .set_handler_fn(page_fault_handler)
            .set_stack_index(PAGE_FAULT_IST_INDEX);
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(DOUBLE_FAULT_IST_INDEX);
    }
    // The timer vector points to the context-switch stub (sched.rs), not to
    // an ordinary handler — that one must preserve the full register state.
    // SAFETY: stub_addr() is a valid, present interrupt handler in our CS.
    unsafe {
        idt[TIMER_VECTOR].set_handler_addr(x86_64::VirtAddr::new(crate::sched::stub_addr()));
    }
    idt[KEYBOARD_VECTOR].set_handler_fn(keyboard_handler);
    idt[MOUSE_VECTOR].set_handler_fn(mouse_handler);
    // Per-CPU AP timer → the AP context-switch stub (per-CPU scheduler, sched.rs).
    // SAFETY: ap_stub_addr() is a valid interrupt handler in our (shared) CS.
    unsafe {
        idt[AP_TIMER_VECTOR].set_handler_addr(x86_64::VirtAddr::new(crate::sched::ap_stub_addr()));
    }
    // Cross-CPU IPI handlers (Run 2).
    idt[IPI_PING_VECTOR].set_handler_fn(ipi_ping_handler);
    idt[IPI_HALT_VECTOR].set_handler_fn(ipi_halt_handler);
    idt[IPI_TLB_VECTOR].set_handler_fn(ipi_tlb_handler);
    idt[XHCI_MSIX_VECTOR].set_handler_fn(xhci_msix_handler);
    idt[VIRTIO_BLK_MSIX_VECTOR].set_handler_fn(blk_msix_handler);
    // Harmless spurious handler (LAPIC vector 0xFF).
    idt[0xFF].set_handler_fn(spurious_handler);
    idt
});

/// Ping/wakeup IPI: count up (proof of cross-CPU signaling) + EOI.
extern "x86-interrupt" fn ipi_ping_handler(_frame: InterruptStackFrame) {
    IPI_COUNT[(crate::apic::lapic_id() & 7) as usize].fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Halt IPI: stop this core for good (e.g. on shutdown).
extern "x86-interrupt" fn ipi_halt_handler(_frame: InterruptStackFrame) {
    crate::apic::eoi();
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}

/// TLB-shootdown IPI: reload CR3 so this core discards stale TLB entries
/// (needed when the kernel modifies shared page tables on another core).
extern "x86-interrupt" fn ipi_tlb_handler(_frame: InterruptStackFrame) {
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
    TLB_COUNT[(crate::apic::lapic_id() & 7) as usize].fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Spurious interrupt (LAPIC vector 0xFF): no EOI needed, just ignore.
extern "x86-interrupt" fn spurious_handler(_frame: InterruptStackFrame) {}

/// xHCI event-ring interrupt via MSI-X (J2): **harvest the USB events right away in
/// interrupt context** (instead of waiting until the desktop loop polls). This way USB
/// input also works with HLT-idle/preemption: a key wakes the CPU, this handler harvests
/// the report and buffers the scancode — independent of whether task 0 happens to be running.
/// The `POLLING` flag in `xhci::poll` prevents a race with a possible desktop poll.
extern "x86-interrupt" fn xhci_msix_handler(_frame: InterruptStackFrame) {
    XHCI_MSIX_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::xhci::poll(); // harvest + re-arm endpoint + clear interrupter-pending
    crate::apic::eoi();
}

/// virtio-blk completion via MSI-X (J2): the controller signals a finished
/// block request with a message instead of a shared INTx. We count it (proof of
/// interrupt-driven storage completion on the data path); the used-ring poll in the
/// driver confirms the actual completion (additive, no regression risk).
extern "x86-interrupt" fn blk_msix_handler(_frame: InterruptStackFrame) {
    BLK_MSIX_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Number of handled keyboard IRQs (verification of the IO-APIC routing).
pub static KBD_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

/// IRQ12: read a mouse byte and pass it to the mouse driver. The interrupt
/// now comes via the IO-APIC -> Local APIC, so we EOI to the LAPIC.
extern "x86-interrupt" fn mouse_handler(_frame: InterruptStackFrame) {
    let byte = unsafe { Port::<u8>::new(0x60).read() };
    crate::mouse::push_byte(byte);
    crate::apic::eoi();
}

/// Send End-Of-Interrupt for the timer (called from the scheduler).
/// The timer tick now comes from the Local APIC, so we EOI to the LAPIC.
pub fn send_timer_eoi() {
    crate::apic::eoi();
}

/// IRQ1: read the scancode and buffer it; the shell decodes it later. Via the IO-APIC
/// -> Local APIC, so EOI to the LAPIC.
extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    let sc = unsafe { Port::<u8>::new(0x60).read() };
    crate::ps2::push_scancode(sc);
    KBD_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Switch the IRQ routing from the 8259 PIC to the **IO-APIC** (full-fledged
/// APIC system, SMP-ready). Fully masks the PIC and routes keyboard
/// (IRQ1) + mouse (IRQ12) via the IO-APIC to the BSP. Call after `init_timer`.
pub fn route_io_apic(madt: &crate::acpi::Madt) {
    if madt.ioapic_addr == 0 {
        serial_println!("[ioapic] no IO-APIC — 8259 virtual-wire stays active");
        return;
    }
    // Fully mask the 8259: all IRQs now run via the IO-APIC/LAPIC.
    unsafe {
        PICS.lock().write_masks(0xFF, 0xFF);
    }
    let dest = crate::apic::lapic_id() as u8; // BSP
    let kbd_gsi = madt.gsi_for(1);
    let mouse_gsi = madt.gsi_for(12);
    crate::apic::ioapic_route(madt.ioapic_addr, kbd_gsi, KEYBOARD_VECTOR, dest);
    crate::apic::ioapic_route(madt.ioapic_addr, mouse_gsi, MOUSE_VECTOR, dest);
    serial_println!(
        "[ioapic] @ {:#x}: kbd IRQ1->GSI{} vec {:#x}, mouse IRQ12->GSI{} vec {:#x} -> BSP #{} (8259 masked)",
        madt.ioapic_addr, kbd_gsi, KEYBOARD_VECTOR, mouse_gsi, MOUSE_VECTOR, dest
    );
}

pub fn init() {
    IDT.load();
}

extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    serial_println!("[idt] BREAKPOINT @ {:#x}", frame.instruction_pointer.as_u64());
    BREAKPOINT_HIT.store(true, Ordering::SeqCst);
}

extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) {
    serial_println!("[idt] INVALID OPCODE @ {:#x}", frame.instruction_pointer.as_u64());
    halt();
}

extern "x86-interrupt" fn gp_handler(frame: InterruptStackFrame, code: u64) {
    let ip = frame.instruction_pointer.as_u64();
    let cs = frame.code_segment.0;
    // From RING 3 (CS.RPL=3): terminate only that task/exec, don't halt the whole
    // system (same policy as the page-fault handler).
    if cs & 3 == 3 {
        let cur = crate::sched::current();
        serial_println!("[idt] ring-3 GP FAULT code={code:#x} @ {ip:#x} (task {cur}) -> process terminated");
        if crate::ring3::fg_active() {
            crate::ring3::fg_force_exit(ip);
        }
        let idx = crate::sched::mark_current_dead();
        crate::ring3::note_isolation_kill(idx, ip);
        x86_64::instructions::interrupts::enable();
        loop {
            x86_64::instructions::hlt();
        }
    }
    let rsp = frame.stack_pointer.as_u64();
    let ss = frame.stack_segment.0;
    serial_println!("[idt] GENERAL PROTECTION FAULT code={code:#x} @ {ip:#x} cs={cs:#x} ss={ss:#x} rsp={rsp:#x}");
    // Y: capture a crash dump before we halt (recovery boot reads it).
    crate::crashdump::capture(13, code, ip, rsp, frame.cpu_flags.bits());
    // Dump the top stack words to see how the RIP ended up there.
    if rsp >= 0x1000 {
        for k in 0..5u64 {
            let w = unsafe { ((rsp + k * 8) as *const u64).read_volatile() };
            serial_println!("[idt]   [rsp+{}] = {:#x}", k * 8, w);
        }
    }
    halt();
}

extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    let addr = x86_64::registers::control::Cr2::read_raw();
    // A fault from RING 3 = a process reaching outside its own address space
    // (memory isolation). Terminate ONLY that process and give the CPU back
    // to the scheduler — the rest of the system (desktop, other processes)
    // keeps running. A fault from ring 0 is a real kernel bug: halt.
    if code.contains(PageFaultErrorCode::USER_MODE) {
        // A SYNCHRONOUS foreground exec (own PML4): abort only that exec and
        // return cleanly into run_args — task 0/the shell stays alive.
        if crate::ring3::fg_active() {
            crate::ring3::fg_force_exit(addr); // does not return
        }
        // A PREEMPTIVE background process: terminate that task; the rest keeps running.
        let idx = crate::sched::mark_current_dead();
        let pid = crate::ring3::note_isolation_kill(idx, addr);
        serial_println!(
            "[isolation] ring-3 page fault addr={addr:#x} code={code:?} -> process pid {pid} (task {idx}) TERMINATED"
        );
        x86_64::instructions::interrupts::enable();
        loop {
            x86_64::instructions::hlt(); // the timer switches to another task
        }
    }
    // G1: a ring-0 fault in a guard page below a kernel stack = stack overflow.
    // Detected immediately + deterministically (instead of silent corruption or only on
    // the canary check at the next switch). We run on the OWN PF-IST stack, so
    // the exception frame fit despite the exhausted task stack. RECOVERY: was a
    // regular scheduler task running (current != 0)? Terminate ONLY that task and give the CPU
    // back to the scheduler — the kernel/desktop keeps running. Only an overflow on the
    // boot/main stack (current == 0, not guarded → does not get here) would be fatal.
    if crate::paging::is_stack_guard(addr) {
        let cur = crate::sched::current();
        if cur != 0 {
            let idx = crate::sched::mark_current_dead();
            serial_println!(
                "[g1] KERNEL STACK OVERFLOW: task {} hit guard page {:#x} @ {:#x} → task TERMINATED, kernel keeps running ✓",
                idx,
                addr,
                frame.instruction_pointer.as_u64()
            );
            x86_64::instructions::interrupts::enable();
            loop {
                x86_64::instructions::hlt(); // the timer switches to another task
            }
        }
        serial_println!(
            "[g1] KERNEL STACK OVERFLOW on the boot stack — guard {:#x} @ {:#x} (not recoverable)",
            addr,
            frame.instruction_pointer.as_u64()
        );
        halt();
    }
    // J3: transparent fault-driven swap — is this a swapped-out page? Then read it
    // back from disk, make the PTE present again and RESUME the instruction (return).
    // Non-swap pages fall through to the real fault handling below.
    if crate::swapmgr::try_swap_in(addr) {
        return;
    }
    serial_println!(
        "[idt] PAGE FAULT addr={:#x} code={:?} @ {:#x}",
        addr,
        code,
        frame.instruction_pointer.as_u64()
    );
    crate::crashdump::capture(14, code.bits(), frame.instruction_pointer.as_u64(), frame.stack_pointer.as_u64(), frame.cpu_flags.bits());
    halt();
}

extern "x86-interrupt" fn double_fault_handler(frame: InterruptStackFrame, _code: u64) -> ! {
    serial_println!("[idt] DOUBLE FAULT @ {:#x}", frame.instruction_pointer.as_u64());
    crate::crashdump::capture(8, 0, frame.instruction_pointer.as_u64(), frame.stack_pointer.as_u64(), frame.cpu_flags.bits());
    halt()
}

fn halt() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
