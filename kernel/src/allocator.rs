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
        if layout.size() >= BIG_MIN && layout.align() <= 4096 {
            if let Some(p) = big_alloc(layout.size()) {
                return p;
            }
        }
        without_interrupts(|| unsafe { self.0.alloc(layout) })
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if big_owns(ptr) {
            big_free(ptr, layout.size());
            return;
        }
        without_interrupts(|| unsafe { self.0.dealloc(ptr, layout) })
    }
}

// ── Big-block pool ──────────────────────────────────────────────────────────
// Allocations of BIG_MIN and up (file buffers, socket receive queues, window
// bitmaps, growing Vecs of every kind) come from a page-granular pool of their
// own instead of the first-fit list heap. Runs 63, 65 and 75 on the NUC died on
// "memory allocation of 2 or 4 MiB failed" with 170 to 220 MiB of the list heap
// nominally free: the heap had no hole that size left between thousands of
// small long-lived allocations. A bitmap over whole pages has no such holes,
// and the list heap only ever sees small blocks now. The pool is a run of RAM
// handed over at boot (identity-mapped, like every frame the kernel touches);
// until it is installed, or when it is full, the list heap serves as before.
pub const BIG_MIN: usize = 128 * 1024;
const BIG_MAX_PAGES: usize = 131_072; // 512 MiB at most
static BIG: spin::Mutex<BigPool> = spin::Mutex::new(BigPool { base: 0, pages: 0, bits: [0; BIG_MAX_PAGES / 64], used: 0, peak: 0 });

struct BigPool {
    base: u64,
    pages: usize,
    bits: [u64; BIG_MAX_PAGES / 64],
    used: usize,
    peak: usize,
}

/// Hand the pool `pages` frames starting at `base` (a contiguous run from the
/// main frame allocator). Call once at boot after the frame allocator exists.
pub fn install_big_pool(base: u64, pages: usize) {
    without_interrupts(|| {
        let mut b = BIG.lock();
        b.base = base;
        b.pages = pages.min(BIG_MAX_PAGES);
        b.bits = [0; BIG_MAX_PAGES / 64];
        b.used = 0;
        b.peak = 0;
    });
}

fn big_owns(ptr: *mut u8) -> bool {
    without_interrupts(|| {
        let b = BIG.lock();
        b.pages > 0 && (ptr as u64) >= b.base && (ptr as u64) < b.base + (b.pages as u64) * 4096
    })
}

fn big_alloc(size: usize) -> Option<*mut u8> {
    let need = size.div_ceil(4096);
    without_interrupts(|| {
        let mut b = BIG.lock();
        if b.pages == 0 || need > b.pages {
            return None;
        }
        // First fit over the bitmap, skipping whole full words.
        let mut i = 0usize;
        while i + need <= b.pages {
            if i % 64 == 0 && b.bits[i / 64] == u64::MAX {
                i += 64;
                continue;
            }
            if b.bits[i / 64] & (1u64 << (i % 64)) != 0 {
                i += 1;
                continue;
            }
            let mut run = 1;
            while run < need && b.bits[(i + run) / 64] & (1u64 << ((i + run) % 64)) == 0 {
                run += 1;
            }
            if run == need {
                for j in i..i + need {
                    b.bits[j / 64] |= 1u64 << (j % 64);
                }
                b.used += need;
                if b.used > b.peak {
                    b.peak = b.used;
                }
                return Some((b.base + (i as u64) * 4096) as *mut u8);
            }
            i += run + 1;
        }
        None
    })
}

fn big_free(ptr: *mut u8, size: usize) {
    let need = size.div_ceil(4096);
    without_interrupts(|| {
        let mut b = BIG.lock();
        let first = ((ptr as u64 - b.base) / 4096) as usize;
        for j in first..(first + need).min(b.pages) {
            b.bits[j / 64] &= !(1u64 << (j % 64));
        }
        b.used = b.used.saturating_sub(need);
    });
}

/// (used MiB, free MiB, peak MiB) of the big-block pool, for the [cpu] ledger.
pub fn big_stats() -> (usize, usize, usize) {
    without_interrupts(|| {
        let b = BIG.lock();
        (b.used / 256, (b.pages - b.used) / 256, b.peak / 256)
    })
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
///
/// Bumped to 256 MiB: the desktop-graphics stack (glibc + Cairo + FreeType +
/// Pango/HarfBuzz + the X11 client libs) is served through the VFS, and
/// register_file COPIES each library's bytes into a heap Vec. That library set
/// is now ~30 MiB resident; combined with the EuroFS volume and a late 16 MiB
/// selftest allocation, a 128 MiB heap had no contiguous block left and OOM'd.
/// 384 MiB since 2026-09-04: full desktop Chromium in MULTI-PROCESS mode ran
/// the 256 MiB heap dry (a 512 KiB allocation failed while a child spawned its
/// thread pool). The browser writes its profile - GPU cache, cookie DBs, code
/// cache - into the in-RAM VFS, every child adds its own tracking state, and
/// the X stack holds window buffers; all of that lives here. The guest runs
/// with 3.5 GiB, so the extra 128 MiB is the cheap end of that budget.
/// 512 MiB since 2026-09-25: the desktop browser's profile (a working Simple
/// Cache since the ENOENT fix), its session files and a first-fit list heap
/// that fragments under hundreds of growing file buffers; runs 63 and 65
/// failed 64 KiB and 2 MiB allocations with 169 MiB nominally free. The extra
/// 128 MiB comes out of the demand pool (2.5 GiB on the 5632M guest).
const HEAP_SIZE: usize = 512 * 1024 * 1024;
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
