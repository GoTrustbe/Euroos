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
pub const NVME_MSIX_VECTOR: u8 = 0x48; // NVMe I/O completion via MSI-X (Metal M2-1)
pub const VIRTIO_NET_MSIX_VECTOR: u8 = 0x4B; // virtio-net receive via MSI-X
pub const SCI_VECTOR: u8 = 0x49; // ACPI SCI (power button etc.) — Metal M5-2
/// Cooperative-yield software interrupt: a blocked/sleeping task triggers this
/// (`int YIELD_VECTOR`) to switch away immediately (sched::yield_now). Not a
/// hardware IRQ — its handler sends no EOI.
pub const YIELD_VECTOR: u8 = 0x4A;
/// Number of received virtio-blk completion MSI-X interrupts.
pub static BLK_MSIX_COUNT: AtomicU64 = AtomicU64::new(0);
/// Number of NVMe completion interrupts received via MSI-X (M2-1: proof of
/// interrupt-driven NVMe completion; the data path polls, additive only).
pub static NVME_MSIX_COUNT: AtomicU64 = AtomicU64::new(0);
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
        // NMI: fires even with IF=0 → a live probe of an IF=0 wedge. Injected via QMP
        // when chrome --headless freezes; the handler prints the interrupted RIP.
        idt.non_maskable_interrupt
            .set_handler_fn(nmi_handler)
            .set_stack_index(crate::gdt::NMI_IST_INDEX);
    }
    // The timer vector points to the context-switch stub (sched.rs), not to
    // an ordinary handler — that one must preserve the full register state.
    // SAFETY: stub_addr() is a valid, present interrupt handler in our CS.
    unsafe {
        idt[TIMER_VECTOR].set_handler_addr(x86_64::VirtAddr::new(crate::sched::stub_addr()));
    }
    // Cooperative-yield vector → the yield context-switch stub (sched.rs). Like
    // the timer stub it must preserve the full register state, so it is set by
    // raw address. DPL stays 0: only kernel code (a syscall that just blocked)
    // triggers it, never ring 3.
    // SAFETY: yield_stub_addr() is a valid, present interrupt handler in our CS.
    unsafe {
        idt[YIELD_VECTOR].set_handler_addr(x86_64::VirtAddr::new(crate::sched::yield_stub_addr()));
    }
    idt[KEYBOARD_VECTOR].set_handler_fn(keyboard_handler);
    idt[MOUSE_VECTOR].set_handler_fn(mouse_handler);
    idt[SCI_VECTOR].set_handler_fn(sci_handler);
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
    idt[NVME_MSIX_VECTOR].set_handler_fn(nvme_msix_handler);
    idt[VIRTIO_NET_MSIX_VECTOR].set_handler_fn(net_msix_handler);
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

/// NVMe completion via MSI-X (M2-1): counted as delivery proof; the driver's
/// polling `wait()` consumes the actual completion entries.
extern "x86-interrupt" fn nvme_msix_handler(_frame: InterruptStackFrame) {
    NVME_MSIX_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Receive interrupts taken from the network card.
pub static NET_MSIX_COUNT: AtomicU64 = AtomicU64::new(0);

/// The card has frames for us: drain the ring NOW, rather than when a task next
/// happens to ask. Unlike the storage handlers this one does real work, because
/// the driver has no poll loop of its own that runs often enough - a page load
/// lost 89 segments per load waiting for one.
extern "x86-interrupt" fn net_msix_handler(_frame: InterruptStackFrame) {
    NET_MSIX_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::net::rx_route_irq();
    crate::apic::eoi();
}

/// Number of handled keyboard IRQs (verification of the IO-APIC routing).
pub static KBD_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

/// IRQ12: read a mouse byte and pass it to the mouse driver. The interrupt
/// now comes via the IO-APIC -> Local APIC, so we EOI to the LAPIC.
pub static MOUSE_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
extern "x86-interrupt" fn mouse_handler(_frame: InterruptStackFrame) {
    // Route by the STATUS register, not by which IRQ fired: keyboard and mouse
    // share data port 0x60, and under a shared-buffer race the byte sitting there
    // may belong to the other device. Bit 0 = output buffer full, bit 5 = the byte
    // is AUX (mouse) data. Reading 0x60 blindly here stole keyboard scancodes into
    // the mouse stream and vice versa (measured: usb-tablet moves surfaced as
    // KeyPress 130/38 noise in the X event stream).
    let status = unsafe { Port::<u8>::new(0x64).read() };
    if status & 1 != 0 {
        let byte = unsafe { Port::<u8>::new(0x60).read() };
        if status & 0x20 != 0 {
            crate::mouse::push_byte(byte);
        } else {
            crate::ps2::push_scancode(byte);
        }
    }
    MOUSE_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// ACPI SCI (M5-2): a fixed/GPE ACPI event. Today we act on the power button —
/// a press performs a clean OS-controlled shutdown (ACPI S5) instead of a hard
/// cut. Other SCI sources are acknowledged (status cleared by the source check)
/// and ignored for now.
extern "x86-interrupt" fn sci_handler(_frame: InterruptStackFrame) {
    let power_button = crate::power::sci_is_power_button();
    crate::apic::eoi();
    if power_button {
        crate::serial_println!("[acpi] power button pressed → clean ACPI S5 shutdown");
        crate::power::shutdown();
    }
}

/// Send End-Of-Interrupt for the timer (called from the scheduler).
/// The timer tick now comes from the Local APIC, so we EOI to the LAPIC.
pub fn send_timer_eoi() {
    crate::apic::eoi();
}

/// IRQ1: read the scancode and buffer it; the shell decodes it later. Via the IO-APIC
/// -> Local APIC, so EOI to the LAPIC.
extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    // Same status-based routing as the mouse handler (see there): bit 5 says the
    // pending byte is AUX data even when IRQ1 fired.
    let status = unsafe { Port::<u8>::new(0x64).read() };
    if status & 1 != 0 {
        let sc = unsafe { Port::<u8>::new(0x60).read() };
        if status & 0x20 != 0 {
            crate::mouse::push_byte(sc);
        } else {
            crate::ps2::push_scancode(sc);
        }
    }
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
    // Virtual-wire ExtINT on LINT0 was only needed while the 8259 still delivered
    // interrupts. Now that the PIC is masked and the IO-APIC routes kbd/mouse, mask
    // LINT0 so a stray ExtINT can't make the CPU fetch a spurious vector from the
    // inert PIC (harmless under TCG, wedges input under KVM). See apic::mask_lint0.
    crate::apic::mask_lint0();
    let dest = crate::apic::lapic_id() as u8; // BSP
    let kbd_gsi = madt.gsi_for(1);
    let mouse_gsi = madt.gsi_for(12);
    crate::apic::ioapic_route(madt.ioapic_addr, kbd_gsi, KEYBOARD_VECTOR, dest);
    crate::apic::ioapic_route(madt.ioapic_addr, mouse_gsi, MOUSE_VECTOR, dest);
    // M5-2: the ACPI SCI (power button) — level-triggered, active-low. Enable
    // the fixed power-button event, then route its GSI to the SCI handler.
    if let Some(sci_int) = crate::power::enable_power_button() {
        // SCI_INT from the FADT is a GSI; apply an ISA override if one exists.
        let sci_gsi = if sci_int < 16 { madt.gsi_for(sci_int as u8) } else { sci_int as u32 };
        crate::apic::ioapic_route_level_low(madt.ioapic_addr, sci_gsi, SCI_VECTOR, dest);
        serial_println!("[acpi] power button armed → SCI GSI{} vec {:#x} (press = clean S5)", sci_gsi, SCI_VECTOR);
    }
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

/// NMI probe: fires even under IF=0, so it captures where the CPU is spinning during
/// an IF=0 wedge. Dumps the interrupted RIP + a raw scan of the interrupted stack for
/// return addresses (symbolize offline against the kernel .efi). Then returns (iret)
/// so the guest keeps running — this is a probe, not a fatal.
extern "x86-interrupt" fn nmi_handler(frame: InterruptStackFrame) {
    let rip = frame.instruction_pointer.as_u64();
    let rsp = frame.stack_pointer.as_u64();
    let cs = frame.code_segment.0;
    let rflags = frame.cpu_flags;
    serial_println!("========== NMI PROBE (wedge RIP capture) ==========");
    serial_println!("[nmi] interrupted RIP={rip:#018x} CS={cs:#x} RSP={rsp:#018x} RFLAGS={:#x}", rflags.bits());
    serial_println!("[nmi] anchor kernel_base ~ nmi_handler @ {:#018x}", nmi_handler as usize as u64);
    // The INSTRUCTIONS at the wedge: a lock spin (pause;jmp / cmpxchg) and a
    // poll loop (cmp mem;jne) look identical from the outside; 32 bytes of code
    // tell them apart at a glance.
    {
        // The loop body usually lies BEFORE the sampled rip (a trailing
        // `jmp -N` is where the NMI lands): dump rip-16..rip+16, and decode a
        // `mov r64, [rip+disp32]` (48 8b /r with mod=00 rm=101) anywhere in the
        // window - the absolute address of the polled static identifies WHICH
        // flag/lock the wedge waits on.
        let base = rip.wrapping_sub(16);
        let mut b = [0u8; 32];
        for (i, bi) in b.iter_mut().enumerate() {
            *bi = unsafe { ((base + i as u64) as *const u8).read_volatile() };
        }
        serial_println!("[nmi] code rip-16..: {:02x?}", &b[..16]);
        serial_println!("[nmi] code rip..   : {:02x?}", &b[16..]);
        let mut i = 0usize;
        while i + 7 <= 32 {
            if b[i] == 0x48 && b[i + 1] == 0x8b && (b[i + 2] & 0xC7) == 0x05 {
                let disp = i32::from_le_bytes([b[i + 3], b[i + 4], b[i + 5], b[i + 6]]);
                let insn_end = base + i as u64 + 7;
                let tgt = insn_end.wrapping_add(disp as i64 as u64);
                serial_println!("[nmi] polled static @ {tgt:#018x} (rip-relative mov at rip{:+})", i as i64 - 16);
            }
            i += 1;
        }
    }
    // WHO is wedged: the running task, its name, its last syscall, and whose
    // per-process state is loaded — the holder of whatever lock the spin waits
    // on is almost always identified by these four.
    {
        let cur = crate::sched::current();
        let (sn, sa, sr) = crate::ring3::last_syscall(cur);
        serial_println!("[nmi] current task {cur} {:?} last-syscall={sn}(a1={sa:#x})->{sr:#x} globals-owner={}",
            crate::ring3::thread_name_pub(cur), crate::ring3::globals_owner_now());
    }
    // Scan the interrupted stack for plausible kernel code return addresses (RBP
    // chains are unreliable in release). Kernel code lives high (>= 0x2000_0000).
    serial_println!("[nmi] stack scan (return addresses):");
    let mut printed = 0;
    let mut p = rsp & !0x7;
    let end = (rsp + 0x800) & !0x7; // scan 2 KiB of the interrupted stack
    while p < end && printed < 24 {
        let v = unsafe { (p as *const u64).read_volatile() };
        if (0x2000_0000..0x8000_0000).contains(&v) {
            serial_println!("[nmi]   {v:#018x}");
            printed += 1;
        }
        p += 8;
    }
    // The FULL task census from the same interrupt context. The wedge RIP only
    // describes the task the NMI happened to interrupt - usually the idle loop -
    // while the question is nearly always "what is task N doing". census_trylock
    // is built for interrupt context (try-locks, no allocation).
    crate::sched::census_trylock();
    serial_println!("========== END NMI PROBE ==========");
}

extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) {
    let ip = frame.instruction_pointer.as_u64();
    let cs = frame.code_segment.0;
    // From RING 3 (CS.RPL=3): terminate ONLY that task, never halt the whole system
    // (same policy as the #GP and #PF handlers). A ring-3 #UD is almost always a
    // userland binary hitting an instruction this CPU/OS config does not provide —
    // classically SwiftShader's AVX2 (VEX-encoded, c4/c5 prefix) on a qemu64/non-AVX
    // boot. Before this, ANY such #UD froze the entire VM: a single chrome worker
    // executing one VEX op hung the machine, which is exactly the kind of dead wait
    // that wastes hours. Now it fails fast and readable, and the desktop lives on.
    if cs & 3 == 3 {
        let cur = crate::sched::current();
        let in_code = ip >= 0x1000
            && (ip < 0x1_0000_0000 || (0x100_0000_0000..0x1_0100_0000_0000).contains(&ip));
        let mut b = [0u8; 16];
        if in_code {
            unsafe {
                core::arch::asm!("stac", options(nomem, nostack, preserves_flags));
                for (i, bi) in b.iter_mut().enumerate() {
                    *bi = ((ip + i as u64) as *const u8).read_volatile();
                }
                core::arch::asm!("clac", options(nomem, nostack, preserves_flags));
            }
        }
        // A VEX prefix (0xC4 = 3-byte, 0xC5 = 2-byte) means an AVX/AVX2 instruction the
        // running config can't execute — the one #UD we expect from real userland here.
        let is_vex = in_code && (b[0] == 0xC4 || b[0] == 0xC5);
        let (sn, sa1, sr) = crate::ring3::last_syscall(cur);
        serial_println!(
            "[idt] ring-3 INVALID OPCODE @ {ip:#x} (task {cur}) insn={b:02x?}{} | last-syscall={sn}(a1={sa1:#x})->{:#x} -> process terminated",
            if is_vex { " = VEX/AVX (unsupported on this CPU/boot: enable AVX via an AVX-capable -cpu, e.g. Haswell)" } else { "" },
            sr
        );
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
    // Ring-0 #UD is a genuine kernel bug: halt as before.
    serial_println!("[idt] INVALID OPCODE @ {ip:#x} (ring 0) -> halt");
    halt();
}

extern "x86-interrupt" fn gp_handler(frame: InterruptStackFrame, code: u64) {
    let ip = frame.instruction_pointer.as_u64();
    let cs = frame.code_segment.0;
    // From RING 3 (CS.RPL=3): terminate only that task/exec, don't halt the whole
    // system (same policy as the page-fault handler).
    if cs & 3 == 3 {
        let cur = crate::sched::current();
        // Dump the faulting instruction bytes when RIP is in a plausible, mapped user
        // code range (the identity arena low, or the demand region), read through a
        // brief SMAP AC window. This turns an opaque ring-3 #GP into a decodable
        // instruction — e.g. it identified chrome's IMMEDIATE_CRASH (`cc 0f 0b` =
        // int3;ud2), and a ring-3 int3 hitting the DPL-0 #BP gate is exactly a #GP with
        // error code (3<<3)|2 = 0x1a. Guarded so a wild/unmapped RIP can't nest-fault
        // the handler into a double fault.
        let in_code = ip >= 0x1000
            && (ip < 0x1_0000_0000 || (0x100_0000_0000..0x1_0100_0000_0000).contains(&ip));
        if in_code {
            let mut b = [0u8; 16];
            unsafe {
                core::arch::asm!("stac", options(nomem, nostack, preserves_flags));
                for (i, bi) in b.iter_mut().enumerate() {
                    *bi = ((ip + i as u64) as *const u8).read_volatile();
                }
                core::arch::asm!("clac", options(nomem, nostack, preserves_flags));
            }
            let imm_crash = b[0] == 0xcc && b[1] == 0x0f && b[2] == 0x0b; // int3;ud2
            let (sn, sa1, sr) = crate::ring3::last_syscall(cur);
            serial_println!(
                "[idt] ring-3 GP FAULT code={code:#x} @ {ip:#x} (task {cur}) insn={b:02x?}{} | last-syscall={sn}(a1={sa1:#x}=fd:{})->{:#x} ({}) -> process terminated",
                if imm_crash { " = IMMEDIATE_CRASH (deliberate CHECK abort)" } else { "" },
                crate::ring3::fd_kind(sa1),
                sr,
                if (sr as i64) < 0 { "ERROR" } else { "ok" }
            );
        } else {
            serial_println!("[idt] ring-3 GP FAULT code={code:#x} @ {ip:#x} (task {cur}) -> process terminated");
        }
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

/// Read a u64 from a user virtual address by manually walking the current CR3
/// page tables. Returns None if any level is not present (so a corrupt pointer
/// can never fault us). Only used for post-mortem fault diagnostics.
fn read_user_qword(vaddr: u64) -> Option<u64> {
    // The kernel identity-maps physical memory (virtual == physical), so a physical
    // table/frame address can be dereferenced directly.
    if vaddr & 7 != 0 {
        return None;
    }
    let (cr3, _) = x86_64::registers::control::Cr3::read();
    let mut table_phys = cr3.start_address().as_u64();
    let idx = [
        (vaddr >> 39) & 0x1ff,
        (vaddr >> 30) & 0x1ff,
        (vaddr >> 21) & 0x1ff,
        (vaddr >> 12) & 0x1ff,
    ];
    for (level, &i) in idx.iter().enumerate() {
        let entry_ptr = (table_phys + i * 8) as *const u64;
        let entry = unsafe { core::ptr::read_volatile(entry_ptr) };
        if entry & 1 == 0 {
            return None; // not present
        }
        let next = entry & 0x000f_ffff_ffff_f000;
        // A huge page (bit 7) at level 1 (2 MiB) or 2 (1 GiB) ends the walk.
        if level >= 1 && entry & 0x80 != 0 {
            let page_off = vaddr & ((1u64 << (12 + (3 - level as u32) * 9)) - 1);
            return Some(read_phys_u64(next + page_off));
        }
        table_phys = next;
    }
    Some(read_phys_u64(table_phys + (vaddr & 0xfff)))
}

/// Read a u64 from an identity-mapped physical address, permitting supervisor
/// access to a user-accessible page (SMAP) via STAC/CLAC around the load.
#[inline(never)]
fn read_phys_u64(pa: u64) -> u64 {
    // No `nomem`: the asm must act as a compiler barrier so the volatile read is not
    // hoisted before STAC (which is what re-enabled SMAP and re-faulted us).
    unsafe {
        core::arch::asm!("stac", options(nostack));
        let v = core::ptr::read_volatile(pa as *const u64);
        core::arch::asm!("clac", options(nostack));
        v
    }
}

extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    let addr = x86_64::registers::control::Cr2::read_raw();
    // DEMAND PAGING (opt-in): a fault in the running glibc process's sparse mmap
    // region is committed here (a fresh zeroed frame mapped on the spot) and the
    // instruction retried. No-op unless enabled + in-range, so the normal fault
    // handling below is untouched for every other case (incl. ring 0 kernel copies
    // that touch a not-yet-committed demand page during a syscall).
    if crate::ring3::handle_demand_fault(
        addr,
        code.contains(PageFaultErrorCode::CAUSED_BY_WRITE),
        code.contains(PageFaultErrorCode::PROTECTION_VIOLATION),
    ) {
        return;
    }
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
        {
            let cr3 = x86_64::registers::control::Cr3::read().0.start_address().as_u64();
            crate::paging::dump_walk(cr3, addr);
        }
        serial_println!(
            "[isolation] ring-3 page fault addr={addr:#x} code={code:?} -> process pid {pid} (task {idx}) TERMINATED"
        );
        // Always name WHERE: for an exe mapped at the demand base, rip - base is the
        // objdump offset, which turns "it crashed" into a named function. Cheap, and
        // reading the frame is safe (it is our own interrupt frame, not user memory).
        serial_println!("[isolation]   rip={:#x} rsp={:#x}",
            frame.instruction_pointer.as_u64(), frame.stack_pointer.as_u64());
        // Diagnostic: for an instruction-fetch fault (a bad jump/call target), dump
        // the faulting thread's RIP/RSP and the top of its stack so we can see which
        // library made the bad call. Reads are guarded by a manual CR3 page-walk so a
        // corrupt RSP can never double-fault us. Gated on INSTRUCTION_FETCH so the
        // intentional data-fault isolation selftests early in boot stay quiet.
        // Not only for bad jumps: a DATA fault's caller chain names the function
        // that consumed a bad pointer, which three blind fixes in a row failed to
        // guess for the fontconfig null-page crash. The reads stay page-walk-guarded.
        if code.contains(PageFaultErrorCode::INSTRUCTION_FETCH)
            || (code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) && addr < 0x1000)
        {
            let rip = frame.instruction_pointer.as_u64();
            let rsp = frame.stack_pointer.as_u64();
            serial_println!("[pf-diag] task={idx} rip={rip:#x} rsp={rsp:#x}");
            // Scan the stack upward for return addresses into user code (chrome's libs
            // live in the demand region 0x100_0000_0000.. ; the arena is lower). These
            // name the call chain that reached the null/wild jump — map each against the
            // [mmaplib] ranges to identify the crashing library.
            let mut printed = 0;
            for i in 0..96u64 {
                let a = rsp.wrapping_add(i * 8);
                if let Some(v) = read_user_qword(a) {
                    // Plausible code pointer: in the demand region (libs/code) or arena.
                    if (v >= 0x100_0000_0000 && v < 0x140_0000_0000) || (v >= 0x0100_0000 && v < 0x1000_0000) {
                        serial_println!("[pf-diag]   ret[{:#05x}] -> {v:#x}", i * 8);
                        printed += 1;
                        if printed >= 12 { break; }
                    }
                }
            }
        }
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
