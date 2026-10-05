//! IOMMU (Intel VT-d) detection + DMA-isolation boot policy.
//!
//! ## Why this module exists
//! EuroOS enforces capabilities at the **syscall** boundary, but every DMA-capable
//! device (NVMe, AHCI, the e1000 NIC, xHCI USB, all virtio) programs **physical**
//! addresses directly and — with no IOMMU — can read or write *any* physical memory,
//! bypassing every capability, signature check and audit-log entry. A buggy or
//! malicious device (the classic DMA / "Thunderclap" attack class) therefore defeats
//! the entire isolation model. For an OS that claims sovereign isolation this is a
//! real gap (reported as GitHub issue #11).
//!
//! ## What this module does *today* (honest scope)
//! It **detects** the platform IOMMU via the ACPI DMAR table, reads each VT-d
//! remapping unit's capability registers, reports the DMA-exposure state plainly,
//! and enforces a configurable **boot policy** (`Warn` by default, `Required` for
//! high-assurance deployments that must fail-closed when no IOMMU is present).
//!
//! ## Translation (since 2026-10-05)
//! After every DMA driver has initialised, `program` builds root/context tables and
//! one second-level page table per driver domain (identity-mapped, only the frames
//! that driver allocated for DMA) and enables translation on every unit. A PCI
//! function no driver claimed gets an empty table, so its DMA faults. Faults are
//! read back with `poll_faults` and logged. Detection alone still never reports
//! "protected": `dma_unprotected()` only clears once translation is on.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

/// VT-d memory-mapped register offsets from a remapping unit's base.
const REG_VER: u64 = 0x00; // u32: [7:4]=major [3:0]=minor
const REG_CAP: u64 = 0x08; // u64: capabilities
const REG_ECAP: u64 = 0x10; // u64: extended capabilities
const REG_GSTS: u64 = 0x1C; // u32: global status (bit31 TES = translation enabled)

/// Boot policy for the absence of hardware DMA isolation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Ignore: do not even check (not recommended; kept only for parity).
    Off = 0,
    /// Detect + warn loudly, but continue booting. DEFAULT: the public preview image
    /// must still boot under plain QEMU (no `-device intel-iommu`) and on hardware
    /// whose firmware hides the IOMMU, so we cannot hard-fail by default.
    Warn = 1,
    /// Fail-closed: if no usable IOMMU is present, halt rather than run with
    /// unrestricted device DMA. For deployments whose threat model includes a
    /// malicious peripheral (the whole point of issue #11).
    Required = 2,
}

static POLICY: AtomicU8 = AtomicU8::new(Policy::Warn as u8);
/// True once detection has run and found at least one VT-d remapping unit.
static PRESENT: AtomicBool = AtomicBool::new(false);
/// True while device DMA is unrestricted (no IOMMU, OR an IOMMU exists but we have
/// not yet programmed translation). This is the honest security state the System
/// panel / audit should surface: it stays true until real remapping is active.
static DMA_UNPROTECTED: AtomicBool = AtomicBool::new(true);
static UNIT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Set the boot policy (e.g. from a signed system policy / installer choice).
pub fn set_policy(p: Policy) {
    POLICY.store(p as u8, Ordering::Relaxed);
}

fn policy() -> Policy {
    match POLICY.load(Ordering::Relaxed) {
        0 => Policy::Off,
        2 => Policy::Required,
        _ => Policy::Warn,
    }
}

/// Is a hardware IOMMU present (DMAR published + at least one remapping unit)?
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// Is device DMA currently unrestricted? True whenever no IOMMU translation is
/// actively confining devices — the state that makes capability claims incomplete.
pub fn dma_unprotected() -> bool {
    DMA_UNPROTECTED.load(Ordering::Relaxed)
}

/// One-line status for the System window / audit log.
pub fn status_line() -> alloc::string::String {
    use alloc::string::String;
    if !PRESENT.load(Ordering::Relaxed) {
        return String::from("IOMMU: absent — device DMA UNRESTRICTED");
    }
    let n = UNIT_COUNT.load(Ordering::Relaxed);
    if DMA_UNPROTECTED.load(Ordering::Relaxed) {
        alloc::format!("IOMMU: {n} VT-d unit(s) present, translation NOT yet active — DMA unrestricted")
    } else {
        alloc::format!("IOMMU: {n} VT-d unit(s), translation active — DMA confined")
    }
}

/// Read a u32 from a VT-d unit register (the base is identity-mapped, phys=virt).
unsafe fn rd32(base: u64, off: u64) -> u32 {
    core::ptr::read_volatile((base + off) as *const u32)
}
unsafe fn rd64(base: u64, off: u64) -> u64 {
    core::ptr::read_volatile((base + off) as *const u64)
}

/// Detect the platform IOMMU, report the DMA-exposure state, and enforce the boot
/// policy. Call once during hardware bring-up, after ACPI is available. Returns
/// `true` if a hardware IOMMU is present.
pub fn detect_and_enforce() -> bool {
    if policy() == Policy::Off {
        crate::serial_println!("[iommu] policy=Off — DMA-isolation check skipped (device DMA unrestricted)");
        return false;
    }

    let dmar = match crate::acpi::dmar() {
        Some(d) => d,
        None => {
            // No DMAR table: the platform exposes no Intel VT-d IOMMU. (AMD-Vi / IVRS
            // detection is a separate follow-on.) Device DMA is fully unrestricted.
            PRESENT.store(false, Ordering::Relaxed);
            DMA_UNPROTECTED.store(true, Ordering::Relaxed);
            crate::serial_println!(
                "[iommu] no ACPI DMAR — NO hardware IOMMU. Device DMA is UNRESTRICTED: a \
                 malicious/buggy NVMe/USB/NIC can DMA over any physical memory, bypassing \
                 capabilities. (GitHub #11)"
            );
            enforce_absent();
            return false;
        }
    };

    if dmar.units.is_empty() {
        PRESENT.store(false, Ordering::Relaxed);
        DMA_UNPROTECTED.store(true, Ordering::Relaxed);
        crate::serial_println!("[iommu] DMAR present but declares 0 remapping units — DMA UNRESTRICTED");
        enforce_absent();
        return false;
    }

    PRESENT.store(true, Ordering::Relaxed);
    UNIT_COUNT.store(dmar.units.len() as u64, Ordering::Relaxed);
    crate::serial_println!(
        "[iommu] ACPI DMAR: {} VT-d remapping unit(s), host-addr-width={} bits, interrupt-remap={}",
        dmar.units.len(),
        dmar.host_addr_width as u32 + 1,
        dmar.intr_remap
    );

    let mut any_translating = false;
    for (i, u) in dmar.units.iter().enumerate() {
        // The register base is identity-mapped; read version + capabilities + status.
        let (ver, cap, ecap, gsts) = unsafe {
            (rd32(u.base, REG_VER), rd64(u.base, REG_CAP), rd64(u.base, REG_ECAP), rd32(u.base, REG_GSTS))
        };
        let tes = gsts & (1 << 31) != 0; // Translation Enable Status
        if tes {
            any_translating = true;
        }
        // CAP.MGAW[21:16] = max guest address width - 1; ECAP.IR bit3 = interrupt remap.
        let mgaw = ((cap >> 16) & 0x3F) as u32 + 1;
        let ir = ecap & (1 << 3) != 0;
        crate::serial_println!(
            "[iommu]   unit{i} @ {:#x} seg{} ver {}.{} cap={:#x} ecap={:#x} mgaw={}bits ir={} translating={} include_all={}",
            u.base, u.segment, (ver >> 4) & 0xF, ver & 0xF, cap, ecap, mgaw, ir, tes, u.include_all
        );
    }

    // Presence != protection. We have NOT programmed translation yet, so unless the
    // firmware already enabled it (it won't have), DMA is still unrestricted.
    DMA_UNPROTECTED.store(!any_translating, Ordering::Relaxed);
    if any_translating {
        crate::serial_println!("[iommu] a unit reports translation ENABLED — DMA confined by firmware/prior setup");
    } else {
        crate::serial_println!(
            "[iommu] hardware IOMMU present, translation not yet active: EuroOS programs \
             per-driver DMA domains once every DMA driver has initialised (see \
             `translation ENABLED` below; GitHub #11)."
        );
    }
    true
}

/// Policy action when no usable IOMMU is present.
fn enforce_absent() {
    match policy() {
        Policy::Required => {
            crate::serial_println!(
                "[iommu] POLICY=Required and no usable IOMMU — FAILING CLOSED. Refusing to run with \
                 unrestricted device DMA. Enable the platform IOMMU (VT-d/AMD-Vi) in firmware, or boot \
                 QEMU with `-machine q35,kernel-irqchip=split -device intel-iommu`."
            );
            // Fail-closed: a deployment that set Required would rather halt than run a
            // system whose isolation guarantees are void. Park the CPU.
            loop {
                x86_64::instructions::hlt();
            }
        }
        _ => {
            crate::serial_println!(
                "[iommu] policy=Warn — continuing WITHOUT DMA isolation. Set policy=Required on \
                 high-assurance deployments to fail-closed instead."
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Translation (GitHub #11, second step): per-driver DMA domains.
//
// Every DMA buffer a driver owns is allocated through the frame allocator while
// its init runs under `dma_scope`; those frames become that driver's domain, and
// the PCI functions it claims get a context entry pointing at the domain's
// second-level page table (identity-mapped, 4 KiB granules, read+write). A device
// nobody claims gets no context entry at all, so its DMA faults. Everything that
// is not a driver's own buffer (the kernel heap, page tables, process arenas,
// the demand pool, the identity store) is simply absent from every table.
// ═══════════════════════════════════════════════════════════════════════════════

use alloc::vec::Vec;
use spin::Mutex;

const REG_GCMD: u64 = 0x18;
const REG_RTADDR: u64 = 0x20;
const REG_CCMD: u64 = 0x28;
const REG_FSTS: u64 = 0x34;
const GCMD_TE: u32 = 1 << 31;
const GCMD_SRTP: u32 = 1 << 30;
const GSTS_TES: u32 = 1 << 31;
const GSTS_RTPS: u32 = 1 << 30;
const SL_R: u64 = 1;
const SL_W: u64 = 2;

struct Domain {
    name: &'static str,
    bdfs: Vec<(u8, u8, u8)>,
    frames: Vec<(u64, usize)>,
    /// Root of the second-level page table once built (0 before `program`).
    slpt: u64,
    id: u16,
}

struct Tables {
    levels: u8,       // 3 (39-bit) or 4 (48-bit) second-level paging
    root: u64,        // root table physical address
    ctx: [u64; 256],  // context table per bus (0 = none)
    units: Vec<(u64, u64, u64)>, // (base, iotlb-register offset, fault-record offset)
    nfr: u64,         // fault-record count per unit
    table_frames: u64,
}

static DOMAINS: Mutex<Vec<Domain>> = Mutex::new(Vec::new());
static TABLES: Mutex<Option<Tables>> = Mutex::new(None);
static TRANSLATING: AtomicBool = AtomicBool::new(false);
static FAULTS_SEEN: AtomicU64 = AtomicU64::new(0);

unsafe fn wr32(base: u64, off: u64, v: u32) {
    core::ptr::write_volatile((base + off) as *mut u32, v)
}
unsafe fn wr64(base: u64, off: u64, v: u64) {
    core::ptr::write_volatile((base + off) as *mut u64, v)
}

/// Is EuroOS's own translation active (devices confined to their domains)?
pub fn translating() -> bool {
    TRANSLATING.load(Ordering::Relaxed)
}

/// Run a driver's initialisation and record what it allocated and claimed: the
/// frames become the driver's DMA domain, the claimed PCI functions its members.
pub fn dma_scope<R>(falloc: &mut euromm::FrameAllocator, name: &'static str, f: impl FnOnce(&mut euromm::FrameAllocator) -> R) -> R {
    let claims_before = crate::pci::claims().len();
    falloc.trace_start();
    let r = f(falloc);
    let frames = falloc.trace_take();
    let bdfs: Vec<(u8, u8, u8)> = crate::pci::claims().into_iter().skip(claims_before).map(|(b, d, fu, _)| (b, d, fu)).collect();
    let pages: usize = frames.iter().map(|&(_, n)| n).sum();
    let mut doms = DOMAINS.lock();
    match doms.iter_mut().find(|d| d.name == name) {
        Some(d) => {
            d.frames.extend(frames);
            for b in bdfs {
                if !d.bdfs.contains(&b) {
                    d.bdfs.push(b);
                }
            }
        }
        None => {
            let id = doms.len() as u16 + 1;
            doms.push(Domain { name, bdfs, frames, slpt: 0, id });
        }
    }
    if let Some(d) = doms.iter().find(|d| d.name == name) {
        crate::serial_println!("[iommu] domain {name}: {} PCI function(s), {} DMA page(s) registered", d.bdfs.len(), pages);
    }
    r
}

/// A DMA buffer allocated outside a scope (heap-backed virtqueues, a scanout
/// framebuffer, a hot-plugged USB device): add it to the named driver's domain,
/// live if translation already runs.
pub fn dma_add(name: &'static str, pa: u64, len: usize) {
    let start = pa & !0xFFF;
    let pages = ((pa + len as u64 + 0xFFF) / 4096 - start / 4096) as usize;
    let mut doms = DOMAINS.lock();
    let slpt = match doms.iter_mut().find(|d| d.name == name) {
        Some(d) => {
            d.frames.push((start, pages));
            d.slpt
        }
        None => {
            let id = doms.len() as u16 + 1;
            doms.push(Domain { name, bdfs: Vec::new(), frames: alloc::vec![(start, pages)], slpt: 0, id });
            0
        }
    };
    drop(doms);
    if slpt != 0 && TRANSLATING.load(Ordering::Relaxed) {
        let mut t = TABLES.lock();
        if let Some(tb) = t.as_mut() {
            if let Some(fp) = unsafe { crate::DMA_FALLOC } {
                let fa = unsafe { &mut *fp };
                for i in 0..pages as u64 {
                    map_sl(fa, tb, slpt, start + i * 4096);
                }
            }
            for &(base, iro, _) in tb.units.iter() {
                unsafe { invalidate_iotlb(base, iro) };
            }
        }
    }
}

fn zeroed_frame(falloc: &mut euromm::FrameAllocator, tb: &mut Tables) -> Option<u64> {
    let f = falloc.allocate().ok()?;
    unsafe { core::ptr::write_bytes(f as *mut u8, 0, 4096) };
    tb.table_frames += 1;
    Some(f)
}

/// Map one 4 KiB page identity (IOVA == PA, read+write) in a second-level table.
fn map_sl(falloc: &mut euromm::FrameAllocator, tb: &mut Tables, slpt: u64, pa: u64) -> bool {
    let mut table = slpt;
    let shifts: &[u32] = if tb.levels == 4 { &[39, 30, 21] } else { &[30, 21] };
    for &sh in shifts {
        let idx = ((pa >> sh) & 0x1FF) as u64;
        let entry = unsafe { core::ptr::read_volatile((table + idx * 8) as *const u64) };
        let next = if entry & SL_R != 0 {
            entry & 0x000F_FFFF_FFFF_F000
        } else {
            let n = match zeroed_frame(falloc, tb) { Some(n) => n, None => return false };
            unsafe { core::ptr::write_volatile((table + idx * 8) as *mut u64, n | SL_R | SL_W) };
            n
        };
        table = next;
    }
    let idx = ((pa >> 12) & 0x1FF) as u64;
    unsafe { core::ptr::write_volatile((table + idx * 8) as *mut u64, (pa & !0xFFF) | SL_R | SL_W) };
    true
}

unsafe fn invalidate_context_cache(base: u64) {
    wr64(base, REG_CCMD, (1u64 << 63) | (1u64 << 61)); // ICC + global invalidation
    while rd64(base, REG_CCMD) & (1 << 63) != 0 {}
}

unsafe fn invalidate_iotlb(base: u64, iro: u64) {
    wr64(base, iro + 8, (1u64 << 63) | (1u64 << 60)); // IVT + global
    while rd64(base, iro + 8) & (1 << 63) != 0 {}
}

/// Build the tables for every registered domain and switch the remapping units
/// to EuroOS's translation. Call once all DMA drivers have initialised. Returns
/// the number of PCI functions now confined.
pub fn program(falloc: &mut euromm::FrameAllocator) -> usize {
    if !present() || policy() == Policy::Off {
        return 0;
    }
    let dmar = match crate::acpi::dmar() { Some(d) => d, None => return 0 };
    if dmar.units.is_empty() {
        return 0;
    }
    // Capabilities of the first unit decide the paging depth (QEMU: one unit).
    let cap = unsafe { rd64(dmar.units[0].base, REG_CAP) };
    let sagaw = (cap >> 8) & 0x1F;
    let levels: u8 = if sagaw & 0x2 != 0 { 3 } else if sagaw & 0x4 != 0 { 4 } else {
        crate::serial_println!("[iommu] unit supports neither 39- nor 48-bit second-level paging (sagaw={sagaw:#x}) — not programmed");
        return 0;
    };
    let nfr = ((cap >> 40) & 0xFF) + 1;
    let fro = ((cap >> 24) & 0x3FF) * 16;
    let mut tb = Tables { levels, root: 0, ctx: [0; 256], units: Vec::new(), nfr, table_frames: 0 };
    for u in dmar.units.iter() {
        let ecap = unsafe { rd64(u.base, REG_ECAP) };
        let iro = ((ecap >> 8) & 0x3FF) * 16;
        tb.units.push((u.base, iro, fro));
    }
    let root = match zeroed_frame(falloc, &mut tb) { Some(r) => r, None => return 0 };
    tb.root = root;

    // Second-level tables: one per domain; an empty one for unclaimed functions.
    let mut doms = DOMAINS.lock();
    for d in doms.iter_mut() {
        let slpt = match zeroed_frame(falloc, &mut tb) { Some(s) => s, None => return 0 };
        for &(base, n) in d.frames.iter() {
            for i in 0..n as u64 {
                map_sl(falloc, &mut tb, slpt, base + i * 4096);
            }
        }
        d.slpt = slpt;
    }
    let deny_slpt = match zeroed_frame(falloc, &mut tb) { Some(s) => s, None => return 0 };
    let deny_id = doms.len() as u16 + 1;

    // Context entries for every PCI function that exists.
    let aw_code: u64 = if levels == 4 { 2 } else { 1 };
    let mut confined = 0usize;
    let mut unclaimed = 0usize;
    for dev in crate::pci::enumerate() {
        let ctx = if tb.ctx[dev.bus as usize] != 0 {
            tb.ctx[dev.bus as usize]
        } else {
            let c = match zeroed_frame(falloc, &mut tb) { Some(c) => c, None => return 0 };
            tb.ctx[dev.bus as usize] = c;
            unsafe { core::ptr::write_volatile((root + dev.bus as u64 * 16) as *mut u64, c | 1) };
            c
        };
        let (slpt, did) = match doms.iter().find(|d| d.bdfs.contains(&(dev.bus, dev.dev, dev.func))) {
            Some(d) => { confined += 1; (d.slpt, d.id) }
            None => { unclaimed += 1; (deny_slpt, deny_id) }
        };
        let idx = ((dev.dev as u64) << 3 | dev.func as u64) * 16;
        unsafe {
            core::ptr::write_volatile((ctx + idx) as *mut u64, slpt | 1); // present, SL translation
            core::ptr::write_volatile((ctx + idx + 8) as *mut u64, aw_code | ((did as u64) << 8));
        }
    }
    drop(doms);

    // Program every unit: root table, invalidate caches, enable translation.
    for &(base, iro, _) in tb.units.iter() {
        unsafe {
            wr64(base, REG_RTADDR, root);
            wr32(base, REG_GCMD, GCMD_SRTP);
            while rd32(base, REG_GSTS) & GSTS_RTPS == 0 {}
            invalidate_context_cache(base);
            invalidate_iotlb(base, iro);
            wr32(base, REG_GCMD, GCMD_TE);
            while rd32(base, REG_GSTS) & GSTS_TES == 0 {}
        }
    }
    let frames = tb.table_frames;
    *TABLES.lock() = Some(tb);
    TRANSLATING.store(true, Ordering::Relaxed);
    DMA_UNPROTECTED.store(false, Ordering::Relaxed);
    let names: Vec<&str> = DOMAINS.lock().iter().map(|d| d.name).collect();
    crate::serial_println!(
        "[iommu] translation ENABLED: {}-level second-level paging, {} domain(s) [{}], {confined} claimed PCI function(s) confined to their own DMA buffers, {unclaimed} unclaimed function(s) get NO DMA, {frames} table frame(s)",
        levels, names.len(), names.join(", ")
    );
    confined
}

/// Read and clear the fault records of every unit; each one is a DMA the
/// hardware refused. Returns how many were found now.
pub fn poll_faults() -> usize {
    let t = TABLES.lock();
    let tb = match t.as_ref() { Some(t) => t, None => return 0 };
    let mut found = 0;
    for &(base, _, fro) in tb.units.iter() {
        let fsts = unsafe { rd32(base, REG_FSTS) };
        if fsts & 0x2 == 0 {
            continue; // PPF: no pending primary fault
        }
        for i in 0..tb.nfr {
            let off = fro + i * 16;
            let hi = unsafe { rd64(base, off + 8) };
            if hi & (1 << 63) == 0 {
                continue;
            }
            let lo = unsafe { rd64(base, off) };
            let sid = (hi & 0xFFFF) as u16;
            let reason = ((hi >> 32) & 0xFF) as u8;
            let is_read = hi & (1 << 62) != 0;
            found += 1;
            let n = FAULTS_SEEN.fetch_add(1, Ordering::Relaxed);
            if n < 32 {
                crate::serial_println!(
                    "[iommu] DMA FAULT blocked: device {:02x}:{:02x}.{} tried to {} physical {:#x} (reason {reason:#x}) — outside its domain",
                    sid >> 8, (sid >> 3) & 0x1F, sid & 7, if is_read { "read" } else { "write" }, lo & !0xFFF
                );
            }
            unsafe { wr64(base, off + 8, 1 << 63) }; // clear this record
        }
        unsafe { wr32(base, REG_FSTS, fsts & 0x7F) }; // clear the summary bits
    }
    found
}

/// Total faults recorded since boot.
pub fn faults_total() -> u64 {
    FAULTS_SEEN.load(Ordering::Relaxed)
}
