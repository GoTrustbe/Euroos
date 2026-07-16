//! Global PROCESS FRAME POOL (Sprint S3 fork/exec).
//!
//! The main frame allocator is owned by `main`/the desktop loop and is therefore not
//! reachable from the SYSCALL layer. fork()/execve() however must be able to allocate
//! frames (an arena + page tables + kernel stack for the child) while running in
//! a syscall. That is why at boot we reserve one large, contiguous
//! chunk of RAM from the main allocator and manage it here via a SEPARATE, global
//! `FrameAllocator` behind a Mutex — independent of the main allocator, no contention.

use euromm::{FrameAllocator, MemoryRegion};
use spin::Mutex;

static POOL: Mutex<Option<FrameAllocator>> = Mutex::new(None);
/// A SECOND, independent pool dedicated to DEMAND PAGING (ring3::handle_demand_fault
/// commits sparse-mmap pages from here). Kept separate from the fork pool so a large
/// demand working set never starves fork()/exec() and vice-versa.
static DEMAND_POOL: Mutex<Option<FrameAllocator>> = Mutex::new(None);

/// Install the pool: the range `base .. base + frames*4096` (reserved by `main` from the
/// main allocator and therefore marked there as 'in use') is from now on
/// managed as free by this global allocator.
pub fn install(base: u64, frames: usize) {
    let region = MemoryRegion { start: base, len: (frames as u64) * 4096, usable: true };
    *POOL.lock() = Some(FrameAllocator::from_regions(&[region], 0));
}

/// Allocate `count` contiguous frames from the pool (None = pool full / not initialized).
pub fn alloc_contiguous(count: usize) -> Option<u64> {
    POOL.lock().as_mut()?.allocate_contiguous(count).ok()
}

/// Allocate one frame from the pool.
pub fn alloc() -> Option<u64> {
    POOL.lock().as_mut()?.allocate().ok()
}

/// Return a previously allocated frame to the pool.
pub fn free(addr: u64) {
    if let Some(p) = POOL.lock().as_mut() {
        let _ = p.free(addr);
    }
}

/// Free frames in the pool (for diagnostics / `dmesg`).
pub fn free_frames() -> usize {
    POOL.lock().as_ref().map(|p| p.free_frames()).unwrap_or(0)
}

// ── Dedicated DEMAND-PAGING pool (independent of the fork pool above) ─────────
pub fn demand_install(base: u64, frames: usize) {
    let region = MemoryRegion { start: base, len: (frames as u64) * 4096, usable: true };
    *DEMAND_POOL.lock() = Some(FrameAllocator::from_regions(&[region], 0));
}
/// Allocate one frame from the demand pool (None = exhausted / not initialized).
pub fn demand_alloc() -> Option<u64> {
    DEMAND_POOL.lock().as_mut()?.allocate().ok()
}
/// Return a frame to the demand pool.
pub fn demand_free(addr: u64) {
    if let Some(p) = DEMAND_POOL.lock().as_mut() {
        let _ = p.free(addr);
    }
}
pub fn demand_free_frames() -> usize {
    DEMAND_POOL.lock().as_ref().map(|p| p.free_frames()).unwrap_or(0)
}
