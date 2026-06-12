//! Interrupt Descriptor Table + exception-handlers (Track 3.3).
//!
//! We draaien (voorlopig) met hardware-interrupts UIT en pollen het toetsenbord;
//! alleen CPU-excepties (niet-maskeerbaar) komen hier binnen. Dat houdt de
//! eerste kernelmodus simpel en robuust — APIC-timer + scheduler is een
//! volgende fase.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pic8259::ChainedPics;
use spin::{Lazy, Mutex};
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::gdt::{DOUBLE_FAULT_IST_INDEX, PAGE_FAULT_IST_INDEX};
use crate::serial_println;

/// Wordt door de breakpoint-handler gezet — bewijs dat de IDT werkt.
pub static BREAKPOINT_HIT: AtomicBool = AtomicBool::new(false);

// ── Hardware-interrupts (PIC remap naar 0x20+) ────────────────────────────
const PIC1_OFFSET: u8 = 0x20;
const PIC2_OFFSET: u8 = 0x28;
const TIMER_VECTOR: u8 = PIC1_OFFSET; // IRQ0
const KEYBOARD_VECTOR: u8 = PIC1_OFFSET + 1; // IRQ1
const MOUSE_VECTOR: u8 = PIC2_OFFSET + 4; // IRQ12 (op de slave-PIC)

static PICS: Mutex<ChainedPics> = Mutex::new(unsafe { ChainedPics::new(PIC1_OFFSET, PIC2_OFFSET) });

/// Timer-ticks sinds de interrupts aan staan (~100 Hz).
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// Aparte vector voor de per-CPU AP-timer (los van de BSP-scheduler-stub op 0x20).
const AP_TIMER_VECTOR: u8 = 0x41;
// Cross-CPU IPI-vectoren (Run 2).
pub const IPI_PING_VECTOR: u8 = 0x43; // generieke ping / wakeup
pub const IPI_HALT_VECTOR: u8 = 0x44; // stop een core (cli; hlt)
pub const IPI_TLB_VECTOR: u8 = 0x45; // TLB-shootdown (CR3 herladen)
pub const XHCI_MSIX_VECTOR: u8 = 0x46; // xHCI-event-ring via MSI-X (J2)
/// Aantal ontvangen xHCI-MSI-X-interrupts (bewijst MSI-X-levering).
pub static XHCI_MSIX_COUNT: AtomicU64 = AtomicU64::new(0);
pub const VIRTIO_BLK_MSIX_VECTOR: u8 = 0x47; // virtio-blk completion via MSI-X (J2)
/// Aantal ontvangen virtio-blk-completion-MSI-X-interrupts.
pub static BLK_MSIX_COUNT: AtomicU64 = AtomicU64::new(0);
/// Per-CPU teller van ontvangen ping-IPIs (bewijst cross-CPU-signalering).
pub static IPI_COUNT: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
/// Per-CPU teller van afgehandelde TLB-shootdowns.
pub static TLB_COUNT: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Initialiseer de timer-keten: 8259-PIC voor toetsenbord/muis, en de **Local
/// APIC-timer** (i.p.v. de PIT) als scheduler-tick op `hz` Hz. De PIT-IRQ0 wordt
/// gemaskeerd; de APIC-timer vuurt op dezelfde vector (0x20 → scheduler-stub).
/// Interrupts worden hier NOG niet aangezet (caller doet `enable()`).
pub fn init_timer(hz: u32) {
    unsafe {
        PICS.lock().initialize();
        // Master: maskeer IRQ0 (PIT-timer, vervangen door de APIC-timer); houd
        // IRQ1 (toetsenbord) + IRQ2 (cascade) aan. Slave: IRQ12 (muis, bit 4).
        PICS.lock().write_masks(0xF9, 0xEF);
    }
    // Start de Local-APIC-timer; die levert voortaan de scheduler-tick.
    let count = crate::apic::init(hz, TIMER_VECTOR);
    // J1: vanaf nu mag de lock-vrije kmsg-tee `lapic_id()` lezen (LAPIC gemapt).
    crate::klog::mark_apic_ready();
    serial_println!(
        "[apic] Local APIC #{} aan — timer {hz} Hz (gekalibreerd: {count} ticks/periode)",
        crate::apic::lapic_id()
    );
}

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
    idt.general_protection_fault.set_handler_fn(gp_handler);
    unsafe {
        // G1: de page-fault-handler op een eigen IST-stack, zodat een kernel-stack-
        // overflow (fault op de guard-pagina) afgehandeld wordt i.p.v. een double fault.
        idt.page_fault
            .set_handler_fn(page_fault_handler)
            .set_stack_index(PAGE_FAULT_IST_INDEX);
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(DOUBLE_FAULT_IST_INDEX);
    }
    // De timer-vector wijst naar de context-switch-stub (sched.rs), niet naar
    // een gewone handler — die moet de volledige registerstaat bewaren.
    // SAFETY: stub_addr() is een geldige, present interrupt-handler in onze CS.
    unsafe {
        idt[TIMER_VECTOR].set_handler_addr(x86_64::VirtAddr::new(crate::sched::stub_addr()));
    }
    idt[KEYBOARD_VECTOR].set_handler_fn(keyboard_handler);
    idt[MOUSE_VECTOR].set_handler_fn(mouse_handler);
    // Per-CPU AP-timer → de AP-context-switch-stub (per-CPU scheduler, sched.rs).
    // SAFETY: ap_stub_addr() is een geldige interrupt-handler in onze (gedeelde) CS.
    unsafe {
        idt[AP_TIMER_VECTOR].set_handler_addr(x86_64::VirtAddr::new(crate::sched::ap_stub_addr()));
    }
    // Cross-CPU IPI-handlers (Run 2).
    idt[IPI_PING_VECTOR].set_handler_fn(ipi_ping_handler);
    idt[IPI_HALT_VECTOR].set_handler_fn(ipi_halt_handler);
    idt[IPI_TLB_VECTOR].set_handler_fn(ipi_tlb_handler);
    idt[XHCI_MSIX_VECTOR].set_handler_fn(xhci_msix_handler);
    idt[VIRTIO_BLK_MSIX_VECTOR].set_handler_fn(blk_msix_handler);
    // Onschuldige spurious-handler (LAPIC vector 0xFF).
    idt[0xFF].set_handler_fn(spurious_handler);
    idt
});

/// Ping/wakeup-IPI: tel op (bewijs van cross-CPU-signalering) + EOI.
extern "x86-interrupt" fn ipi_ping_handler(_frame: InterruptStackFrame) {
    IPI_COUNT[(crate::apic::lapic_id() & 7) as usize].fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Halt-IPI: stop deze core definitief (bv. bij shutdown).
extern "x86-interrupt" fn ipi_halt_handler(_frame: InterruptStackFrame) {
    crate::apic::eoi();
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}

/// TLB-shootdown-IPI: herlaad CR3 zodat deze core stale TLB-entries weggooit
/// (nodig wanneer de kernel gedeelde page-tables wijzigt op een andere core).
extern "x86-interrupt" fn ipi_tlb_handler(_frame: InterruptStackFrame) {
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
    TLB_COUNT[(crate::apic::lapic_id() & 7) as usize].fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Spurious-interrupt (LAPIC vector 0xFF): geen EOI nodig, gewoon negeren.
extern "x86-interrupt" fn spurious_handler(_frame: InterruptStackFrame) {}

/// xHCI-event-ring-interrupt via MSI-X (J2): **harvest de USB-events meteen in
/// interrupt-context** (i.p.v. te wachten tot de desktop-loop pollt). Zo werkt USB-
/// invoer ook met HLT-idle/preemptie: een toets wekt de CPU, deze handler harvest
/// het rapport en buffert de scancode — onafhankelijk van of taak 0 net draait. De
/// `POLLING`-vlag in `xhci::poll` voorkomt een race met een eventuele desktop-poll.
extern "x86-interrupt" fn xhci_msix_handler(_frame: InterruptStackFrame) {
    XHCI_MSIX_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::xhci::poll(); // harvest + re-arm endpoint + clear interrupter-pending
    crate::apic::eoi();
}

/// virtio-blk-completion via MSI-X (J2): de controller signaleert een afgeronde
/// blok-request met een bericht i.p.v. een gedeelde INTx. We tellen 'm (bewijs van
/// interrupt-gedreven storage-completion op de datapad); de used-ring-poll in de
/// driver bevestigt de eigenlijke voltooiing (additief, géén regressie-risico).
extern "x86-interrupt" fn blk_msix_handler(_frame: InterruptStackFrame) {
    BLK_MSIX_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Aantal afgehandelde toetsenbord-IRQs (verificatie van de IO-APIC-routering).
pub static KBD_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

/// IRQ12: lees een muis-byte en geef 'm door aan de muis-driver. De interrupt
/// komt nu via de IO-APIC -> Local APIC, dus EOI'en we naar de LAPIC.
extern "x86-interrupt" fn mouse_handler(_frame: InterruptStackFrame) {
    let byte = unsafe { Port::<u8>::new(0x60).read() };
    crate::mouse::push_byte(byte);
    crate::apic::eoi();
}

/// Stuur End-Of-Interrupt voor de timer (aangeroepen vanuit de scheduler).
/// De timer-tick komt nu van de Local APIC, dus EOI'en we naar de LAPIC.
pub fn send_timer_eoi() {
    crate::apic::eoi();
}

/// IRQ1: lees de scancode en buffer 'm; de shell decodeert later. Via de IO-APIC
/// -> Local APIC, dus EOI naar de LAPIC.
extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    let sc = unsafe { Port::<u8>::new(0x60).read() };
    crate::ps2::push_scancode(sc);
    KBD_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
}

/// Schakel de IRQ-routering om van de 8259-PIC naar de **IO-APIC** (volwaardig
/// APIC-systeem, SMP-klaar). Maskeert de PIC volledig en routeert toetsenbord
/// (IRQ1) + muis (IRQ12) via de IO-APIC naar de BSP. Aanroepen ná `init_timer`.
pub fn route_io_apic(madt: &crate::acpi::Madt) {
    if madt.ioapic_addr == 0 {
        serial_println!("[ioapic] geen IO-APIC — 8259 virtual-wire blijft actief");
        return;
    }
    // 8259 volledig maskeren: alle IRQs lopen voortaan via de IO-APIC/LAPIC.
    unsafe {
        PICS.lock().write_masks(0xFF, 0xFF);
    }
    let dest = crate::apic::lapic_id() as u8; // BSP
    let kbd_gsi = madt.gsi_for(1);
    let mouse_gsi = madt.gsi_for(12);
    crate::apic::ioapic_route(madt.ioapic_addr, kbd_gsi, KEYBOARD_VECTOR, dest);
    crate::apic::ioapic_route(madt.ioapic_addr, mouse_gsi, MOUSE_VECTOR, dest);
    serial_println!(
        "[ioapic] @ {:#x}: kbd IRQ1->GSI{} vec {:#x}, muis IRQ12->GSI{} vec {:#x} -> BSP #{} (8259 gemaskeerd)",
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
    // Vanuit RING 3 (CS.RPL=3): beëindig alleen die taak/exec, halt niet het hele
    // systeem (zelfde beleid als de page-fault-handler).
    if cs & 3 == 3 {
        let cur = crate::sched::current();
        serial_println!("[idt] ring-3 GP FAULT code={code:#x} @ {ip:#x} (taak {cur}) -> proces beëindigd");
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
    // Y: leg een crash-dump vast vóór we halten (recovery-boot leest 'm).
    crate::crashdump::capture(13, code, ip, rsp, frame.cpu_flags.bits());
    // Dump de bovenste stackwoorden om te zien hoe de RIP daar belandde.
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
    // Een fout vanuit RING 3 = een proces dat buiten zijn eigen adresruimte
    // grijpt (geheugenisolatie). Beëindig ALLEEN dat proces en geef de CPU terug
    // aan de scheduler — de rest van het systeem (desktop, andere processen)
    // draait door. Een fout vanuit ring 0 is een echte kernelbug: halt.
    if code.contains(PageFaultErrorCode::USER_MODE) {
        // Een SYNCHRONE voorgrond-exec (eigen PML4): breek alleen die exec af en
        // keer netjes terug in run_args — taak 0/de shell blijft leven.
        if crate::ring3::fg_active() {
            crate::ring3::fg_force_exit(addr); // keert niet terug
        }
        // Een PREEMPTIEF achtergrondproces: beëindig die taak; de rest draait door.
        let idx = crate::sched::mark_current_dead();
        let pid = crate::ring3::note_isolation_kill(idx, addr);
        serial_println!(
            "[isolatie] ring-3 page fault addr={addr:#x} code={code:?} -> proces pid {pid} (taak {idx}) BEËINDIGD"
        );
        x86_64::instructions::interrupts::enable();
        loop {
            x86_64::instructions::hlt(); // de timer schakelt naar een andere taak
        }
    }
    // G1: een ring-0-fout in een guard-pagina onder een kernel-stack = stack-overflow.
    // Onmiddellijk + deterministisch gedetecteerd (i.p.v. stille corruptie of pas bij
    // de canary-check op de volgende switch). We draaien op de EIGEN PF-IST-stack, dus
    // het exceptie-frame paste ondanks de uitgeputte taak-stack. RECOVERY: draaide er
    // een gewone scheduler-taak (current != 0)? Beëindig ALLEEN die taak en geef de CPU
    // terug aan de scheduler — de kernel/desktop draait door. Alleen een overflow op de
    // boot-/main-stack (current == 0, niet-guarded → komt hier niet) zou fataal zijn.
    if crate::paging::is_stack_guard(addr) {
        let cur = crate::sched::current();
        if cur != 0 {
            let idx = crate::sched::mark_current_dead();
            serial_println!(
                "[g1] KERNEL STACK OVERFLOW: taak {} raakte guard-pagina {:#x} @ {:#x} → taak BEËINDIGD, kernel draait door ✓",
                idx,
                addr,
                frame.instruction_pointer.as_u64()
            );
            x86_64::instructions::interrupts::enable();
            loop {
                x86_64::instructions::hlt(); // de timer schakelt naar een andere taak
            }
        }
        serial_println!(
            "[g1] KERNEL STACK OVERFLOW op de boot-stack — guard {:#x} @ {:#x} (niet recoverbaar)",
            addr,
            frame.instruction_pointer.as_u64()
        );
        halt();
    }
    // J3: transparante fault-gedreven swap — is dit een uitgeswapte pagina? Lees 'm
    // dan terug van schijf, maak de PTE weer present en HERVAT de instructie (return).
    // Niet-swap-pagina's vallen door naar de echte-fault-afhandeling hieronder.
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
