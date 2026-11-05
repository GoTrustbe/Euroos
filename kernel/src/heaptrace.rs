//! Heap allocation tracing per call site (BUG-027 instrumentation).
//!
//! Built only with `--features heaptrace` (`scripts/build.sh heaptrace`); a
//! normal image carries none of this. Every heap block then gets a 16-byte
//! header in front of it with the index of its allocation site; a site is the
//! chain of return addresses found on the stack above the allocator (see
//! `walk`). Per site the table keeps live bytes,
//! live blocks and the number of allocations ever made, without allocating
//! itself (fixed static table, atomics only).
//!
//! Shell commands (kernel/src/shell.rs), for the serial harness:
//! - `heapsnap`: remember every site's live bytes and blocks;
//! - `heapdiff [n]`: the n sites whose live bytes grew most since the snapshot;
//! - `heaptop [n]`: the n sites holding the most live bytes.
//! Each site line carries its raw return addresses and the runtime address of
//! `klog::dump_registers_and_backtrace` as anchor; `scripts/heaptrace-sym.py`
//! turns them into function names with the link map (target/kernel.map).

use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

/// Header in front of every block: site index, a magic word, the requested size.
pub const HDR: usize = 16;
const MAGIC: u32 = 0x4854_5243; // "HTRC"
/// Return addresses kept per site.
pub const DEPTH: usize = 12;
/// Frames that decide which site an allocation belongs to.
const HASHED: usize = 6;
/// Sites in the table (open addressing on a hash of the return addresses).
const NSITES: usize = 32768;

struct Site {
    key: AtomicU64,
    frames: [AtomicU64; DEPTH],
    live_bytes: AtomicI64,
    live_count: AtomicI64,
    total: AtomicU64,
}

#[allow(clippy::declare_interior_mutable_const)]
const SITE0: Site = Site {
    key: AtomicU64::new(0),
    frames: [const { AtomicU64::new(0) }; DEPTH],
    live_bytes: AtomicI64::new(0),
    live_count: AtomicI64::new(0),
    total: AtomicU64::new(0),
};
static SITES: [Site; NSITES] = [SITE0; NSITES];
static SNAP_BYTES: [AtomicI64; NSITES] = [const { AtomicI64::new(0) }; NSITES];
static SNAP_COUNT: [AtomicI64; NSITES] = [const { AtomicI64::new(0) }; NSITES];
/// Allocations that found the table full (counted on site 0).
pub static OVERFLOW: AtomicU64 = AtomicU64::new(0);
/// Frees whose header did not carry the magic (would mean a foreign pointer).
pub static BAD_FREES: AtomicU32 = AtomicU32::new(0);

#[inline(always)]
fn read_rsp() -> u64 {
    let rsp: u64;
    // SAFETY: reads a register.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags)) };
    rsp
}

/// What kind of call ends right before `ret`: Some(target) for `call rel32`
/// (E8), Some(0) for an indirect call (`call reg`, FF D0+r with or without a
/// REX prefix; `call [reg+disp8]`, FF 50+r; `call [rip+disp32]` /
/// `call [reg+disp32]`, FF 15 / FF 90+r), None when no call does.
/// Only called for addresses inside the kernel's code window.
fn call_before(ret: u64) -> Option<u64> {
    // SAFETY: ret - 7 .. ret lies in the kernel image (checked by the caller).
    let b = |k: u64| unsafe { ((ret - k) as *const u8).read_volatile() };
    if b(5) == 0xE8 {
        // SAFETY: as above.
        let rel = unsafe { ((ret - 4) as *const i32).read_unaligned() };
        return Some(ret.wrapping_add(rel as i64 as u64));
    }
    if (b(2) == 0xFF && (0xD0..=0xD7).contains(&b(1)))
        || (b(3) == 0xFF && (0x50..=0x57).contains(&b(2)))
        || (b(6) == 0xFF && (b(5) == 0x15 || (0x90..=0x97).contains(&b(5))))
    {
        return Some(0);
    }
    None
}

/// Is `va` mapped present and supervisor-only in the current address space?
/// Walks the 4-level tables from CR3 (page tables are identity-mapped).
fn kernel_mapped(va: u64) -> bool {
    const ADDR: u64 = 0x000F_FFFF_FFFF_F000;
    let cr3: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
    let mut table = cr3 & ADDR;
    let mut user = true; // U must be set at every level for a user page
    for shift in [39u32, 30, 21, 12] {
        let idx = (va >> shift) & 0x1FF;
        // SAFETY: a present table frame, identity-mapped.
        let e = unsafe { ((table + idx * 8) as *const u64).read_volatile() };
        if e & 1 == 0 {
            return false;
        }
        user &= e & 4 != 0;
        if (shift == 30 || shift == 21) && e & 0x80 != 0 {
            break;
        }
        table = e & ADDR;
    }
    !user // a user page: SMAP would fault the read
}

/// The allocation's call chain, found on the stack. The UEFI target uses the
/// Windows x64 frame layout (rbp is set after the callee-saved pushes, so
/// [rbp] is not the caller's rbp) and the image carries no unwind tables, so
/// there is no frame chain to walk. Instead: scan the kernel stack upward
/// from rsp for words that point into the kernel code right after a call,
/// and keep only a CHAIN: a direct call must target the function that holds
/// the previous return address (target <= previous, within 1 MiB), which
/// drops the stale return addresses that dead stack slots are full of. The
/// chain starts at the return into site() (a direct call of walk). Indirect
/// calls cannot be checked and are taken as they come; a tail call ends the
/// chain early. Only kernel stack pages are read.
#[inline(never)]
fn walk(out: &mut [u64; DEPTH]) {
    let anchor = crate::klog::dump_registers_and_backtrace as *const () as usize as u64;
    // .text is ~3 MiB with the anchor ~0.6 MiB into it (target/kernel.map).
    let code_lo = anchor.saturating_sub(0x20_0000);
    let code_hi = anchor + 0x60_0000;
    let me = walk as *const () as usize as u64;
    let rsp = read_rsp();
    let mut p = rsp;
    let mut prev = 0u64; // the last return address of the chain (0: not started)
    let mut i = 0usize;
    while p < rsp + 8192 && i < DEPTH {
        // A stack may end at a page boundary with a guard page or a user page
        // above it: cross into the next page only if it is a present kernel page.
        if p & 0xFFF == 0 && !kernel_mapped(p) {
            break;
        }
        // SAFETY: p lies on the current kernel stack, in a checked page.
        let v = unsafe { (p as *const u64).read_volatile() };
        p += 8;
        if v < code_lo + 8 || v >= code_hi {
            continue;
        }
        let Some(target) = call_before(v) else { continue };
        if prev == 0 {
            if target == me {
                prev = v; // the return into site(): the chain starts here
            }
            continue;
        }
        if target != 0 && !(target <= prev && prev - target < 0x10_0000) {
            continue; // a direct call of something else: a stale slot
        }
        if v == prev || out[..i].contains(&v) {
            continue; // a stale copy of a frame already taken
        }
        out[i] = v;
        i += 1;
        prev = v;
    }
}

fn hash(fr: &[u64; DEPTH]) -> u64 {
    // The first HASHED frames name a site: deeper ones are where stale slots
    // and the caller's own callers would split one site into many.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &f in &fr[..HASHED] {
        h ^= f;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        h ^= h >> 29;
    }
    h | 1
}

/// The site index for the current call chain (0 when the table is full).
#[inline(never)]
fn site() -> u32 {
    let mut fr = [0u64; DEPTH];
    walk(&mut fr);
    let h = hash(&fr);
    let mut i = (h as usize) % NSITES;
    for _ in 0..64 {
        if i == 0 {
            i = 1;
        }
        let s = &SITES[i];
        let k = s.key.load(Ordering::Acquire);
        if k == h {
            return i as u32;
        }
        if k == 0 && s.key.compare_exchange(0, h, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            for (d, &f) in fr.iter().enumerate() {
                s.frames[d].store(f, Ordering::Relaxed);
            }
            return i as u32;
        }
        if s.key.load(Ordering::Acquire) == h {
            return i as u32;
        }
        i = (i + 1) % NSITES;
    }
    OVERFLOW.fetch_add(1, Ordering::Relaxed);
    0
}

/// The layout the inner allocator must serve for a traced `layout`, and the
/// offset of the user block in it.
#[inline]
pub fn outer(layout: core::alloc::Layout) -> (core::alloc::Layout, usize) {
    let off = layout.align().max(HDR);
    // SAFETY: align unchanged (a power of two); size grows by at most one align.
    let l = unsafe { core::alloc::Layout::from_size_align_unchecked(layout.size() + off, layout.align()) };
    (l, off)
}

/// Record a fresh block: `raw` is what the inner allocator returned, `off`
/// from `outer`. Returns the user pointer.
#[inline]
pub fn on_alloc(raw: *mut u8, off: usize, size: usize) -> *mut u8 {
    let idx = site();
    let user = unsafe { raw.add(off) };
    // SAFETY: the header lies inside the block the inner allocator handed out.
    unsafe {
        (user.sub(HDR) as *mut u32).write(idx);
        (user.sub(HDR - 4) as *mut u32).write(MAGIC);
        (user.sub(HDR - 8) as *mut u64).write(size as u64);
    }
    let s = &SITES[idx as usize];
    s.live_bytes.fetch_add(size as i64, Ordering::Relaxed);
    s.live_count.fetch_add(1, Ordering::Relaxed);
    s.total.fetch_add(1, Ordering::Relaxed);
    user
}

/// Forget a block before it is freed; returns the inner allocator's pointer.
#[inline]
pub fn on_free(user: *mut u8, layout: core::alloc::Layout) -> *mut u8 {
    let off = layout.align().max(HDR);
    // SAFETY: `user` came from on_alloc with this layout.
    let (idx, magic, size) = unsafe {
        ((user.sub(HDR) as *const u32).read(), (user.sub(HDR - 4) as *const u32).read(), (user.sub(HDR - 8) as *const u64).read())
    };
    if magic == MAGIC && (idx as usize) < NSITES {
        let s = &SITES[idx as usize];
        s.live_bytes.fetch_sub(size as i64, Ordering::Relaxed);
        s.live_count.fetch_sub(1, Ordering::Relaxed);
        // Poison the magic: a second free of the same block is then visible.
        unsafe { (user.sub(HDR - 4) as *mut u32).write(0) };
    } else {
        BAD_FREES.fetch_add(1, Ordering::Relaxed);
    }
    unsafe { user.sub(off) }
}

pub fn snapshot() -> alloc::vec::Vec<alloc::string::String> {
    let mut sites = 0usize;
    let mut bytes = 0i64;
    for (i, s) in SITES.iter().enumerate() {
        let b = s.live_bytes.load(Ordering::Relaxed);
        SNAP_BYTES[i].store(b, Ordering::Relaxed);
        SNAP_COUNT[i].store(s.live_count.load(Ordering::Relaxed), Ordering::Relaxed);
        if s.key.load(Ordering::Relaxed) != 0 {
            sites += 1;
        }
        bytes += b;
    }
    alloc::vec![alloc::format!("heapsnap: {sites} sites, {bytes} live bytes traced, overflow {}, bad frees {}",
        OVERFLOW.load(Ordering::Relaxed), BAD_FREES.load(Ordering::Relaxed))]
}

fn site_line(tag: &str, i: usize, db: i64, dc: i64) -> alloc::string::String {
    let s = &SITES[i];
    let mut fr = alloc::string::String::new();
    for f in s.frames.iter() {
        let v = f.load(Ordering::Relaxed);
        if v == 0 {
            break;
        }
        if !fr.is_empty() {
            fr.push(',');
        }
        fr.push_str(&alloc::format!("{v:#x}"));
    }
    alloc::format!("[heaptrace] {tag} site={i} bytes={db:+} blocks={dc:+} live={} total={} frames={fr}",
        s.live_bytes.load(Ordering::Relaxed), s.total.load(Ordering::Relaxed))
}

/// The `n` sites with the largest growth since `snapshot` (diff) or the
/// largest live bytes (top). Lines also go to the serial console.
pub fn report(diff: bool, n: usize) -> alloc::vec::Vec<alloc::string::String> {
    let n = n.clamp(1, 200);
    // Pick the top n without allocating per site: n passes over the table.
    let mut chosen: alloc::vec::Vec<(usize, i64, i64)> = alloc::vec::Vec::with_capacity(n);
    let val = |i: usize| -> (i64, i64) {
        let s = &SITES[i];
        let (b, c) = (s.live_bytes.load(Ordering::Relaxed), s.live_count.load(Ordering::Relaxed));
        if diff {
            (b - SNAP_BYTES[i].load(Ordering::Relaxed), c - SNAP_COUNT[i].load(Ordering::Relaxed))
        } else {
            (b, c)
        }
    };
    let mut total = 0i64;
    for i in 0..NSITES {
        total += val(i).0;
    }
    for _ in 0..n {
        let mut best: Option<(usize, i64, i64)> = None;
        for i in 0..NSITES {
            if SITES[i].key.load(Ordering::Relaxed) == 0 && i != 0 {
                continue;
            }
            if chosen.iter().any(|c| c.0 == i) {
                continue;
            }
            let (b, c) = val(i);
            if b <= 0 && diff {
                continue;
            }
            if best.map_or(true, |x| b > x.1) {
                best = Some((i, b, c));
            }
        }
        match best {
            Some(b) => chosen.push(b),
            None => break,
        }
    }
    let anchor = crate::klog::dump_registers_and_backtrace as *const () as usize as u64;
    let mut out = alloc::vec![alloc::format!(
        "[heaptrace] {} anchor={anchor:#x} sum={total:+} overflow={} bad_frees={}",
        if diff { "diff since heapsnap" } else { "top live" },
        OVERFLOW.load(Ordering::Relaxed), BAD_FREES.load(Ordering::Relaxed))];
    for &(i, b, c) in &chosen {
        out.push(site_line(if diff { "grew" } else { "live" }, i, b, c));
    }
    for l in &out {
        crate::serial_println!("{l}");
    }
    out
}
