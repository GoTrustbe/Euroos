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

/// J1: take the list-heap lock, counting how long we waited (the heap is the
/// innermost leaf of the lock hierarchy and the first lock every core shares
/// once kernel work runs on the APs, so its contention is measured). Callers
/// run with interrupts off (IRQSAVE).
#[inline]
fn with_heap<R>(h: &LockedHeap, f: impl FnOnce(&mut linked_list_allocator::Heap) -> R) -> R {
    if let Some(mut g) = h.try_lock() {
        crate::klock::SITE_HEAP.stats.record_acquire(0);
        return f(&mut g);
    }
    let mut spins = 0u64;
    loop {
        while h.is_locked() {
            spins += 1;
            core::hint::spin_loop();
        }
        if let Some(mut g) = h.try_lock() {
            crate::klock::SITE_HEAP.stats.record_acquire(spins.max(1));
            return f(&mut g);
        }
    }
}

// ── J1: per-CPU small-object caches ─────────────────────────────────────────
// Measured first (`[lk-bench]`, lockbench image, -smp 4): with the subsystem
// locks split per mount / channel / connection, the ONE list-heap lock became
// the bottleneck: 58 % of its acquisitions contended in the fs workload, 24 % in
// ipc, and 4 cores reached only x1.25-x1.5 of one core. Every Vec, String and Box
// takes that lock twice (alloc + free).
//
// Small blocks (size <= 2048, align <= 16) are now served from a per-CPU cache
// per size class: a CPU pops/pushes its own free list with interrupts disabled,
// no lock at all. The shared heap lock is taken only to refill a class in a batch
// or to give a batch back when a list grows too long. The idea is the classic
// per-CPU magazine (Bonwick's slab magazines; Linux SLUB per-CPU slabs); this is
// a small re-implementation, not derived from other code.
//
// Invariant that keeps this sound: the class decision depends only on the
// Layout, so `dealloc` reaches the same class as `alloc` did, and every block
// that ever enters a class list was obtained from the list heap as exactly
// `Layout(class_size, 16)`, which is what it is returned as.

const CLASS_SIZES: [usize; 14] = [16, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048];
const NCLASS_H: usize = CLASS_SIZES.len();
const SMALL_MAX: usize = 2048;
const SMALL_ALIGN: usize = 16;
const PCPU: usize = 8;
/// Blocks fetched per refill / returned per flush.
const BATCH: usize = 16;
/// A class list longer than this gives BATCH blocks back to the shared heap.
const HIGH_WATER: usize = 64;

#[inline]
fn class_of(layout: &Layout) -> Option<usize> {
    if layout.size() > SMALL_MAX || layout.align() > SMALL_ALIGN {
        return None;
    }
    let need = layout.size().max(1);
    CLASS_SIZES.iter().position(|&c| c >= need)
}

#[derive(Clone, Copy)]
struct FreeList {
    head: *mut u8,
    count: usize,
}

struct PerCpuCaches(core::cell::UnsafeCell<[[FreeList; NCLASS_H]; PCPU]>);
// SAFETY: slot [cpu] is only touched by core `cpu` (index = its own Local APIC
// id) and only with interrupts disabled, so neither another core nor an
// interrupt handler on the same core can interleave with an update.
unsafe impl Sync for PerCpuCaches {}
static CACHES: PerCpuCaches =
    PerCpuCaches(core::cell::UnsafeCell::new([[FreeList { head: core::ptr::null_mut(), count: 0 }; NCLASS_H]; PCPU]));

/// Set once the Local APIC answers (klock::init); before that every allocation
/// runs on the BSP and uses slot 0.
static PCPU_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Cache hits per core (one cache line each: a shared hit counter bumped on
/// every allocation would itself be a cross-core hot spot), refills, flushes.
#[repr(align(64))]
pub struct PaddedCount(pub core::sync::atomic::AtomicU64);
pub static PCPU_HITS: [PaddedCount; PCPU] = [const { PaddedCount(core::sync::atomic::AtomicU64::new(0)) }; PCPU];
pub static PCPU_REFILLS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static PCPU_FLUSHES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn percpu_ready() {
    PCPU_READY.store(true, core::sync::atomic::Ordering::Release);
}

#[inline]
fn cpu_idx() -> usize {
    if PCPU_READY.load(core::sync::atomic::Ordering::Acquire) {
        crate::percpu::cpu() & (PCPU - 1)
    } else {
        0
    }
}

/// Small-block alloc. Caller: interrupts disabled.
#[inline]
unsafe fn small_alloc(heap: &LockedHeap, class: usize) -> *mut u8 {
    let cpu = cpu_idx();
    // SAFETY: own slot, interrupts off (see PerCpuCaches).
    let fl = unsafe { &mut (*CACHES.0.get())[cpu][class] };
    if fl.head.is_null() {
        // Refill a batch under ONE acquisition of the shared lock.
        let lay = unsafe { Layout::from_size_align_unchecked(CLASS_SIZES[class], SMALL_ALIGN) };
        with_heap(heap, |h| {
            for _ in 0..BATCH {
                match h.allocate_first_fit(lay) {
                    Ok(p) => {
                        let p = p.as_ptr();
                        unsafe { (p as *mut *mut u8).write(fl.head) };
                        fl.head = p;
                        fl.count += 1;
                    }
                    Err(_) => break,
                }
            }
        });
        PCPU_REFILLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if fl.head.is_null() {
            return core::ptr::null_mut(); // heap exhausted
        }
    } else {
        PCPU_HITS[cpu].0.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
    let p = fl.head;
    fl.head = unsafe { (p as *const *mut u8).read() };
    fl.count -= 1;
    p
}

/// Small-block free. Caller: interrupts disabled.
#[inline]
unsafe fn small_free(heap: &LockedHeap, class: usize, ptr: *mut u8) {
    let cpu = cpu_idx();
    // SAFETY: own slot, interrupts off.
    let fl = unsafe { &mut (*CACHES.0.get())[cpu][class] };
    unsafe { (ptr as *mut *mut u8).write(fl.head) };
    fl.head = ptr;
    fl.count += 1;
    if fl.count > HIGH_WATER {
        let lay = unsafe { Layout::from_size_align_unchecked(CLASS_SIZES[class], SMALL_ALIGN) };
        with_heap(heap, |h| {
            for _ in 0..BATCH {
                let p = fl.head;
                fl.head = unsafe { (p as *const *mut u8).read() };
                fl.count -= 1;
                unsafe { h.deallocate(core::ptr::NonNull::new_unchecked(p), lay) };
            }
        });
        PCPU_FLUSHES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Bytes parked in the per-CPU caches (they count as "used" in `stats()`).
pub fn percpu_cached_bytes() -> usize {
    without_interrupts(|| {
        let mut n = 0;
        // Reading other cores' counters without their cooperation is racy but
        // only feeds a statistic.
        let c = unsafe { &*CACHES.0.get() };
        for cpu in c.iter() {
            for (k, fl) in cpu.iter().enumerate() {
                n += fl.count * CLASS_SIZES[k];
            }
        }
        n
    })
}

unsafe impl GlobalAlloc for IrqSafeHeap {
    // `heaptrace` (BUG-027): every block carries a site header in front of it;
    // the inner allocator below sees the enlarged layout on alloc AND dealloc,
    // so its class/pool decision stays consistent. See kernel/src/heaptrace.rs.
    #[cfg(feature = "heaptrace")]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let (outer, off) = crate::heaptrace::outer(layout);
        let raw = unsafe { self.alloc_inner(outer) };
        if raw.is_null() {
            return raw;
        }
        crate::heaptrace::on_alloc(raw, off, layout.size())
    }
    #[cfg(feature = "heaptrace")]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let raw = crate::heaptrace::on_free(ptr, layout);
        let (outer, _) = crate::heaptrace::outer(layout);
        unsafe { self.dealloc_inner(raw, outer) }
    }
    #[cfg(not(feature = "heaptrace"))]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { self.alloc_inner(layout) }
    }
    #[cfg(not(feature = "heaptrace"))]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.dealloc_inner(ptr, layout) }
    }
}

impl IrqSafeHeap {
    #[inline(always)]
    unsafe fn alloc_inner(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= BIG_MIN && layout.align() <= 4096 {
            if let Some(p) = big_alloc(layout.size()) {
                return p;
            }
        }
        if let Some(c) = class_of(&layout) {
            // SAFETY: interrupts are off for the per-CPU cache access.
            return without_interrupts(|| unsafe { small_alloc(&self.0, c) });
        }
        without_interrupts(|| {
            with_heap(&self.0, |h| h.allocate_first_fit(layout).ok().map_or(core::ptr::null_mut(), |p| p.as_ptr()))
        })
    }
    #[inline(always)]
    unsafe fn dealloc_inner(&self, ptr: *mut u8, layout: Layout) {
        if big_owns(ptr) {
            big_free(ptr, layout.size());
            return;
        }
        if let Some(c) = class_of(&layout) {
            // SAFETY: same class as at alloc time (decided by the layout alone);
            // interrupts are off for the per-CPU cache access.
            without_interrupts(|| unsafe { small_free(&self.0, c, ptr) });
            return;
        }
        // SAFETY: GlobalAlloc contract: ptr came from `alloc` with this layout.
        without_interrupts(|| with_heap(&self.0, |h| unsafe { h.deallocate(core::ptr::NonNull::new_unchecked(ptr), layout) }))
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
        BIG_BASE.store(base, core::sync::atomic::Ordering::Release);
        BIG_END.store(base + (b.pages as u64) * 4096, core::sync::atomic::Ordering::Release);
    });
}

/// J1: the pool's address range, published once at install. `big_owns` runs on
/// EVERY free (to route it to the right allocator) and used to take the pool
/// lock just to read these two words: a global lock on every small dealloc.
static BIG_BASE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static BIG_END: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn big_owns(ptr: *mut u8) -> bool {
    let p = ptr as u64;
    p >= BIG_BASE.load(core::sync::atomic::Ordering::Acquire) && p < BIG_END.load(core::sync::atomic::Ordering::Acquire)
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
/// The heap used to be a 512 MiB static array: 512 MiB of .bss that made the
/// kernel's load image 610 MB, so UEFI LoadImage needed that much contiguous
/// RAM and the public image could not boot below 1 GiB (night plan N2.2).
/// It is now UEFI-allocated at boot, sized from installed RAM (`init_at`); this
/// small static heap is only the fallback when that allocation fails.
const HEAP_MAX: usize = 512 * 1024 * 1024;
const FALLBACK_SIZE: usize = 32 * 1024 * 1024;
static mut FALLBACK: [u8; FALLBACK_SIZE] = [0u8; FALLBACK_SIZE];
static HEAP_SIZE_DYN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// The heap size to ask UEFI for: a quarter of installed RAM, 64 MiB to 512 MiB.
pub fn wanted_size(total_ram: u64) -> usize {
    ((total_ram / 4) as usize).clamp(64 * 1024 * 1024, HEAP_MAX)
}

/// Initialize the heap on a UEFI-allocated region. Must be the VERY FIRST action
/// in the kernel, before any `alloc` use (Vec/String/format!).
pub fn init_at(base: *mut u8, size: usize) {
    unsafe {
        ALLOCATOR.0.lock().init(base, size);
    }
    HEAP_SIZE_DYN.store(size, core::sync::atomic::Ordering::Relaxed);
}

/// Fallback: the small static heap (when UEFI could not hand out a region).
pub fn init() {
    unsafe {
        ALLOCATOR
            .0
            .lock()
            .init(core::ptr::addr_of_mut!(FALLBACK) as *mut u8, FALLBACK_SIZE);
    }
    HEAP_SIZE_DYN.store(FALLBACK_SIZE, core::sync::atomic::Ordering::Relaxed);
}

pub fn stats() -> (usize, usize) {
    without_interrupts(|| {
        let h = ALLOCATOR.0.lock();
        (h.used(), h.free())
    })
}

pub fn size() -> usize {
    HEAP_SIZE_DYN.load(core::sync::atomic::Ordering::Relaxed)
}
