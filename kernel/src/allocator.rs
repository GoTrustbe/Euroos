//! Kernel-heap: een global allocator over een statische heap-regio.
//!
//! We installeren onze EIGEN allocator (niet die van de uefi-crate) zodat `alloc`
//! werkt in beide fases: tijdens UEFI Boot Services én erna (na ExitBootServices
//! bestaat de UEFI-allocator niet meer). De heap is een statische `.bss`-regio,
//! dus hij is meteen geldig en hangt niet van UEFI af.
//!
//! `linked_list_allocator` is hier de motor; EuroMM's eigen slab-allocator
//! (Track 3.4) vervangt dit later.

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// 96 MiB kernel-heap. Ruim voor EuroFS-volume, console-history, packets én de
/// browser-engine: een echte webpagina (~140 KB, ~3000 elementen) bouwt een grote
/// DOM + per-knoop computed-style; 32 MiB liep daarop vol → OOM-panic. De VM heeft
/// 256 MiB, dus 96 MiB heap is veilig.
const HEAP_SIZE: usize = 96 * 1024 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

/// Initialiseer de heap. Moet de ALLEREERSTE actie in de kernel zijn,
/// vóór enige `alloc`-gebruik (Vec/String/format!).
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
