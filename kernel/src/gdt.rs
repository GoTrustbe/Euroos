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

/// Bring-up of an application processor: load the **shared** GDT and set the
/// kernel segments, so that the CS selector in the (shared) IDT entries is valid
/// and the AP can handle ring-0 interrupts. We load NO TSS: the only TSS is
/// that of the BSP (busy bit), and a parked/timer-only AP does no ring-3
/// or IST. (Per-CPU TSS is the next step towards AP tasks in ring 3.)
pub fn init_ap() {
    use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code);
        SS::set_reg(GDT.1.data);
        DS::set_reg(GDT.1.data);
        ES::set_reg(GDT.1.data);
    }
}
