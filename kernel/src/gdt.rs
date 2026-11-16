//! Global Descriptor Table + Task State Segment (Track 3.3).
//!
//! In long mode segmentation is mostly off, but the GDT is still needed for
//! privilege levels and the TSS — which provides separate interrupt stacks (IST) for
//! critical exceptions (double fault), so that a broken stack does not cause a triple
//! fault.

use spin::Lazy;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
/// G1: the page-fault handler runs on its OWN IST stack. This way a kernel
/// stack overflow (which faults on the guard page) can be handled — the CPU
/// pushes the exception frame onto this fresh stack instead of onto the just-
/// exhausted task stack (which would otherwise immediately cause a double fault).
pub const PAGE_FAULT_IST_INDEX: u16 = 1;
/// NMI runs on its own IST stack: an NMI is delivered even with IF=0, so it fires
/// during an IF=0 wedge (a spinlock/loop that killed the timer). Its handler dumps
/// the interrupted RIP to name the spinning code.
pub const NMI_IST_INDEX: u16 = 2;
const IST_STACK_SIZE: usize = 4096 * 5;

static mut DF_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];
static mut RSP0_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];
static mut PF_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];
static mut NMI_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        let start = VirtAddr::from_ptr(core::ptr::addr_of!(DF_STACK));
        start + IST_STACK_SIZE as u64 // top (stack grows downward)
    };
    tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = {
        let start = VirtAddr::from_ptr(core::ptr::addr_of!(PF_STACK));
        start + IST_STACK_SIZE as u64
    };
    tss.interrupt_stack_table[NMI_IST_INDEX as usize] = {
        let start = VirtAddr::from_ptr(core::ptr::addr_of!(NMI_STACK));
        start + IST_STACK_SIZE as u64
    };
    // Kernel stack for ring3->ring0 transitions (privilege change via interrupt).
    tss.privilege_stack_table[0] = {
        let start = VirtAddr::from_ptr(core::ptr::addr_of!(RSP0_STACK));
        start + IST_STACK_SIZE as u64
    };
    tss
});

pub struct Selectors {
    pub code: SegmentSelector,
    pub data: SegmentSelector,
    pub user_data: SegmentSelector,
    pub user_code: SegmentSelector,
    pub tss: SegmentSelector,
}

// The order is deliberate: kernel_code, kernel_data, user_data, user_code — required
// for the SYSCALL/SYSRET selector layout (user_data = kernel_data+8, user_code
// = kernel_data+16).
static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    let user_data = gdt.append(Descriptor::user_data_segment());
    let user_code = gdt.append(Descriptor::user_code_segment());
    let tss = gdt.append(Descriptor::tss_segment(&TSS));
    (
        gdt,
        Selectors {
            code,
            data,
            user_data,
            user_code,
            tss,
        },
    )
});

/// The segment selectors (for SYSCALL/SYSRET configuration and ring-3 entry).
pub fn selectors() -> &'static Selectors {
    &GDT.1
}

/// Top of the kernel stack that the CPU uses on an interrupt from ring 3
/// (= TSS.rsp0). A ring-3 scheduler task uses this for its interrupt frames.
pub fn rsp0_top() -> u64 {
    (core::ptr::addr_of!(RSP0_STACK) as u64 + IST_STACK_SIZE as u64) & !0xF
}

/// Set TSS.rsp0 to the kernel stack of the current ring-3 task. The CPU reads
/// this on every ring3->ring0 interrupt; the scheduler updates it per task,
/// so that MULTIPLE ring-3 processes each have their own interrupt stack.
pub fn set_rsp0(addr: u64) {
    let tss: &TaskStateSegment = &TSS;
    let p = tss as *const TaskStateSegment as *mut TaskStateSegment;
    // SAFETY: single-core; we only write the rsp0 field that the CPU reads out.
    unsafe {
        core::ptr::addr_of_mut!((*p).privilege_stack_table[0]).write_volatile(VirtAddr::new(addr));
    }
}

pub fn init() {
    use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
    use x86_64::instructions::tables::load_tss;
    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code);
        // Reload SS/DS/ES to our data segment: the old UEFI selectors are not
        // in our GDT and would cause a #GP on the first iretq.
        SS::set_reg(GDT.1.data);
        DS::set_reg(GDT.1.data);
        ES::set_reg(GDT.1.data);
        load_tss(GDT.1.tss);
    }
}

// ── Per-AP GDT + TSS ────────────────────────────────────────────────────────
// The IDT is shared by every core and its double-fault, page-fault and NMI
// gates name IST stacks. A core without a loaded TSS takes the IST pointer from
// wherever TR points after INIT: base 0, i.e. the stack pointer is read from
// linear address 0x24.. (the real-mode vector table) and the exception frame
// is written there. The APs loaded no TSS, so a page fault, a double fault or
// an NMI on an AP (the `nmi` of `--nmi-on-hang` reaches every core) ran on a
// garbage stack. Each AP now has its own GDT (same selector layout as the
// BSP's, so the shared IDT's CS stays valid) with its own TSS and its own
// three IST stacks.
const MAX_CPU: usize = 8;
const AP_IST_COUNT: usize = 3; // double fault, page fault, NMI (the indexes above)
static mut AP_IST: [[[u8; IST_STACK_SIZE]; AP_IST_COUNT]; MAX_CPU] = [[[0; IST_STACK_SIZE]; AP_IST_COUNT]; MAX_CPU];
static mut AP_TSS: [TaskStateSegment; MAX_CPU] = [const { TaskStateSegment::new() }; MAX_CPU];
static mut AP_GDT: [GlobalDescriptorTable; MAX_CPU] = [const { GlobalDescriptorTable::new() }; MAX_CPU];

/// `[bottom, top)` of IST stack `ist` (one of the `*_IST_INDEX` values) of AP `cpu`.
pub fn ap_ist_range(cpu: usize, ist: u16) -> (u64, u64) {
    let base = unsafe { core::ptr::addr_of!(AP_IST[cpu % MAX_CPU][ist as usize % AP_IST_COUNT]) as u64 };
    (base, base + IST_STACK_SIZE as u64)
}

/// Bring-up of application processor `cpu` (its LAPIC id & 7): its own GDT with
/// the BSP's selector layout and its own TSS with three IST stacks. Call once,
/// on that core.
pub fn init_ap(cpu: usize) {
    use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
    use x86_64::instructions::tables::load_tss;
    let cpu = cpu % MAX_CPU;
    // SAFETY: slot `cpu` belongs to this core alone and is written once, here,
    // before the core takes its first interrupt; the tables live for ever.
    unsafe {
        let tss: &'static mut TaskStateSegment = &mut *core::ptr::addr_of_mut!(AP_TSS[cpu]);
        for ist in [DOUBLE_FAULT_IST_INDEX, PAGE_FAULT_IST_INDEX, NMI_IST_INDEX] {
            tss.interrupt_stack_table[ist as usize] = VirtAddr::new(ap_ist_range(cpu, ist).1 & !0xF);
        }
        let gdt: &'static mut GlobalDescriptorTable = &mut *core::ptr::addr_of_mut!(AP_GDT[cpu]);
        *gdt = GlobalDescriptorTable::new();
        let code = gdt.append(Descriptor::kernel_code_segment());
        let data = gdt.append(Descriptor::kernel_data_segment());
        let _user_data = gdt.append(Descriptor::user_data_segment());
        let _user_code = gdt.append(Descriptor::user_code_segment());
        let tss_sel = gdt.append(Descriptor::tss_segment(tss));
        debug_assert!(code == GDT.1.code && data == GDT.1.data);
        gdt.load();
        CS::set_reg(code);
        SS::set_reg(data);
        DS::set_reg(data);
        ES::set_reg(data);
        load_tss(tss_sel);
    }
}

/// S3: TSS.rsp0 of AP `cpu`: where an interrupt from ring 3 lands on that core.
pub fn set_ap_rsp0(cpu: usize, top: u64) {
    // SAFETY: slot `cpu` belongs to that core; written on that core before it
    // runs ring 3.
    unsafe {
        let tss = core::ptr::addr_of_mut!(AP_TSS[cpu % MAX_CPU]);
        core::ptr::addr_of_mut!((*tss).privilege_stack_table[0]).write_volatile(VirtAddr::new(top));
    }
}

/// The task register of this core (0 = no TSS loaded).
pub fn task_register() -> u16 {
    let tr: u16;
    unsafe { core::arch::asm!("str {0:x}", out(reg) tr, options(nomem, nostack, preserves_flags)) };
    tr
}
