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
//! ## What it does NOT do yet (the follow-on, tracked on the roadmap)
//! It does not yet *program* translation — root/context tables + second-level page
//! tables that confine each device's DMA to only its own buffers. Until that lands,
//! detecting an IOMMU does **not** by itself isolate DMA; this module is honest about
//! that and never reports "protected" merely because a unit exists.

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
            "[iommu] hardware IOMMU present but EuroOS has not programmed translation yet: \
             device DMA remains unrestricted until per-device remapping lands (roadmap). \
             Detection + boot policy is the first step of GitHub #11."
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
