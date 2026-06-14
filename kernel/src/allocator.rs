//! Kernel heap: a global allocator over a static heap region.
//!
//! We install our OWN allocator (not the one from the uefi crate) so that `alloc`
//! works in both phases: during UEFI Boot Services AND afterwards (after ExitBootServices
//! the UEFI allocator no longer exists). The heap is a static `.bss` region,
//! so it is immediately valid and does not depend on UEFI.
//!
//! `linked_list_allocator` is the engine here; EuroMM's own slab allocator
//! (Track 3.4) replaces this later.

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// 96 MiB kernel heap. Plenty for the EuroFS volume, console history, packets AND the
/// browser engine: a real web page (~140 KB, ~3000 elements) builds a large
/// DOM + per-node computed style; 32 MiB ran out on that → OOM panic. The VM has
/// 256 MiB, so a 96 MiB heap is safe.
const HEAP_SIZE: usize = 96 * 1024 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

/// Initialize the heap. Must be the VERY FIRST action in the kernel,
/// before any `alloc` use (Vec/String/format!).
pub fn init() {
    unsafe {
        ALLOCATOR
            .lock()
            .init(core::ptr::addr_of_mut!(HEAP) as *mut u8, HEAP_SIZE);
    }
}

pub fn stats() -> (usize, usize) {
    let h = ALLOCATOR.lock();
    (h.used(), h.free())
}

pub fn size() -> usize {
    HEAP_SIZE
}
