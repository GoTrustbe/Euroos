//! Global Descriptor Table + Task State Segment (Track 3.3).
//!
//! In long mode is segmentatie grotendeels uit, maar de GDT is nog nodig voor
//! privilege levels en de TSS — die levert aparte interrupt-stacks (IST) voor
//! kritieke excepties (double fault), zodat een kapotte stack geen triple fault
//! veroorzaakt.

use spin::Lazy;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
/// G1: de page-fault-handler draait op een EIGEN IST-stack. Zo kan een kernel-
/// stack-overflow (die op de guard-pagina faultt) afgehandeld worden — de CPU
/// pusht het exceptie-frame op deze verse stack i.p.v. op de net-uitgeputte
/// taak-stack (wat anders meteen een double fault zou geven).
pub const PAGE_FAULT_IST_INDEX: u16 = 1;
const IST_STACK_SIZE: usize = 4096 * 5;

static mut DF_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];
static mut RSP0_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];
static mut PF_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        let start = VirtAddr::from_ptr(core::ptr::addr_of!(DF_STACK));
        start + IST_STACK_SIZE as u64 // top (stack groeit naar beneden)
    };
    tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = {
        let start = VirtAddr::from_ptr(core::ptr::addr_of!(PF_STACK));
        start + IST_STACK_SIZE as u64
    };
    // Kernel-stack voor ring3->ring0 overgangen (privilege change via interrupt).
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

// Volgorde is bewust: kernel_code, kernel_data, user_data, user_code — vereist
// voor de SYSCALL/SYSRET-selector-layout (user_data = kernel_data+8, user_code
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

/// De segment-selectoren (voor SYSCALL/SYSRET-configuratie en ring-3-entry).
pub fn selectors() -> &'static Selectors {
    &GDT.1
}

/// Top van de kernel-stack die de CPU gebruikt bij een interrupt vanuit ring 3
/// (= TSS.rsp0). Een ring-3 scheduler-taak gebruikt deze voor z'n interrupt-frames.
pub fn rsp0_top() -> u64 {
    (core::ptr::addr_of!(RSP0_STACK) as u64 + IST_STACK_SIZE as u64) & !0xF
}

/// Stel TSS.rsp0 in op de kernel-stack van de huidige ring-3 taak. De CPU leest
/// dit bij elke ring3->ring0 interrupt; de scheduler werkt het bij per taak,
/// zodat MEERDERE ring-3 processen elk hun eigen interrupt-stack hebben.
pub fn set_rsp0(addr: u64) {
    let tss: &TaskStateSegment = &TSS;
    let p = tss as *const TaskStateSegment as *mut TaskStateSegment;
    // SAFETY: single-core; we schrijven enkel het rsp0-veld dat de CPU uitleest.
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
        // Herlaad SS/DS/ES naar ons data-segment: de oude UEFI-selectoren staan
        // niet in onze GDT en zouden bij de eerste iretq een #GP geven.
        SS::set_reg(GDT.1.data);
        DS::set_reg(GDT.1.data);
        ES::set_reg(GDT.1.data);
        load_tss(GDT.1.tss);
    }
}

/// Bring-up van een application-processor: laad de **gedeelde** GDT en zet de
/// kernel-segmenten, zodat de CS-selector in de (gedeelde) IDT-entries geldig is
/// en de AP ring-0 interrupts kan afhandelen. We laden GEEN TSS: de enige TSS is
/// die van de BSP (busy-bit), en een geparkeerde/timer-only AP doet geen ring-3
/// of IST. (Per-CPU TSS is de volgende stap richting AP-taken in ring 3.)
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
