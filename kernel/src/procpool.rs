//! Globale PROCES-FRAME-POOL (Sprint S3 fork/exec).
//!
//! De hoofd-frame-allocator wordt door `main`/de desktop-lus bezeten en is dus niet
//! bereikbaar vanuit de SYSCALL-laag. fork()/execve() moeten echter frames kunnen
//! alloceren (een arena + page tables + kernel-stack voor het kind) terwijl ze in
//! een syscall draaien. Daarom reserveren we bij boot één groot, aaneengesloten
//! stuk RAM uit de hoofd-allocator en beheren dat hier via een EIGEN, globale
//! `FrameAllocator` achter een Mutex — los van de hoofd-allocator, geen contentie.

use euromm::{FrameAllocator, MemoryRegion};
use spin::Mutex;

static POOL: Mutex<Option<FrameAllocator>> = Mutex::new(None);

/// Installeer de pool: het bereik `base .. base + frames*4096` (door `main` uit de
/// hoofd-allocator gereserveerd en dus daar als 'in gebruik' gemarkeerd) wordt
/// voortaan door deze globale allocator als vrij beheerd.
pub fn install(base: u64, frames: usize) {
    let region = MemoryRegion { start: base, len: (frames as u64) * 4096, usable: true };
    *POOL.lock() = Some(FrameAllocator::from_regions(&[region], 0));
}

/// Alloceer `count` aaneengesloten frames uit de pool (None = pool vol / niet geïnit).
pub fn alloc_contiguous(count: usize) -> Option<u64> {
    POOL.lock().as_mut()?.allocate_contiguous(count).ok()
}

/// Alloceer één frame uit de pool.
pub fn alloc() -> Option<u64> {
    POOL.lock().as_mut()?.allocate().ok()
}

/// Geef een eerder gealloceerd frame terug aan de pool.
pub fn free(addr: u64) {
    if let Some(p) = POOL.lock().as_mut() {
        let _ = p.free(addr);
    }
}

/// Vrije frames in de pool (voor diagnostiek / `dmesg`).
pub fn free_frames() -> usize {
    POOL.lock().as_ref().map(|p| p.free_frames()).unwrap_or(0)
}
