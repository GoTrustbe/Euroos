//! Kernel heap: a global allocator over a static heap region.
//!
//! We install our OWN allocator (not the one from the uefi crate) so that `alloc`
//! works in both phases: during UEFI Boot Services AND afterwards (after ExitBootServices
//! the UEFI allocator no longer exists). The heap is a static `.bss` region,
//! so it is immediately valid and does not depend on UEFI.
//!
//! `linked_list_allocator` is the engine here; EuroMM's own slab allocator
//! (Track 3.4) replaces this later.

use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;
use x86_64::instructions::interrupts::without_interrupts;

/// Interrupt-safe wrapper around the heap lock (BUG-007 class, root cause of the
/// flaky boot hang): interrupt handlers allocate too (the xHCI MSI-X harvest
/// builds key-event `Vec`s), and `LockedHeap` is a plain spinlock. If an IRQ
/// fires while the interrupted task holds that lock, the handler spins forever
/// with interrupts off — a silent 100%-CPU hang at whatever the task happened
/// to be doing. Holding the lock only with interrupts disabled makes that
/// preemption impossible, so an IRQ-context alloc always finds the lock free.
struct IrqSafeHeap(LockedHeap);

unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| unsafe { self.0.alloc(layout) })
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| unsafe { self.0.dealloc(ptr, layout) })
    }
}

#[global_allocator]
static ALLOCATOR: IrqSafeHeap = IrqSafeHeap(LockedHeap::empty());

/// 128 MiB kernel heap. Plenty for the EuroFS volume, console history, packets,
/// the browser engine's DOM (~140 KB page → large DOM + computed style; 32 MiB
/// OOM'd on that), AND the installer: writing a bootable disk builds the whole
/// ~40 MiB ESP (loader + two kernel copies) in RAM in one allocation — with the
/// captured media also resident, a 96 MiB heap had no contiguous 40 MiB block
/// left for the NVMe/AHCI install path (Metal M2-3). Safe on the 256 MiB
/// screenshot VM (no install there) and the 512 MiB matrix/DOOM VMs.
const HEAP_SIZE: usize = 128 * 1024 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

/// Initialize the heap. Must be the VERY FIRST action in the kernel,
/// before any `alloc` use (Vec/String/format!).
pub fn init() {
    unsafe {
        ALLOCATOR
            .0
            .lock()
            .init(core::ptr::addr_of_mut!(HEAP) as *mut u8, HEAP_SIZE);
    }
}

pub fn stats() -> (usize, usize) {
    without_interrupts(|| {
        let h = ALLOCATOR.0.lock();
        (h.used(), h.free())
    })
}

pub fn size() -> usize {
    HEAP_SIZE
}
