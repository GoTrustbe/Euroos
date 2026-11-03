//! Userspace drivers: the supervisor, the device grant, the driver syscalls,
//! the IRQ->EuroIPC path and the NIC backend (docs/SPRINT-PLAN-USERSPACE-DRIVERS.md).
//!
//! The first driver out of the kernel is `e1000d` (`userland/e1000d`, logic in
//! `crates/euroe1000`). Selected with `net.driver=user` (bootopt.rs); the
//! in-kernel `e1000.rs` stays the default and the fallback.
//!
//! What the kernel gives the driver, and nothing more (`eurodrv::grant`):
//! - **MMIO**: the pages of BAR 0, mapped `USER|RW|NX|PCD|PWT` at virtual ==
//!   physical in the driver's own address space. Not BAR 3 (the MSI-X table):
//!   the kernel programs where the device's interrupts go.
//! - **DMA**: a zeroed window of `DMA_PAGES` pages, mapped into the driver at
//!   virtual == physical, and under VT-d the device's own IOMMU domain holding
//!   exactly those pages (IOVA == PA). Without an IOMMU the log says the DMA is
//!   not confined.
//! - **IRQ**: an MSI-X vector from the userspace-driver pool (below) on the
//!   BSP; the handler bumps a EuroIPC notification slot and the driver
//!   receives it as an `Irq` message.
//! - **EuroIPC**: its own port (only the kernel may send to it) and the
//!   netstack port (only this driver's pid may send to it).
//!
//! When the driver dies, `supervise` quiesces the device before anything is
//! freed (bus mastering off, IOMMU domain back to the empty table), reaps the
//! process, frees the DMA window and restarts per `eurodrv::restart::Policy`.
//!
//! ## What every userspace driver shares (this module)
//!
//! Four drivers run in ring 3: e1000d (here), nvmed (`udrv_blk.rs`),
//! bcm5720d (`udrv_bcm5720.rs`) and megaraidd (`udrv_mr.rs`). Their
//! per-driver resources come from one place, so two drivers cannot end up on
//! the same one:
//! - [`Drv`] / [`RES`]: EuroIPC ports, notification slot and pid range per
//!   driver, fixed and checked for overlap at compile time;
//! - the **vector pool** `interrupts::UDRV_VECTOR_BASE ..+ UDRV_VECTORS`,
//!   handed out by `eurodrv::vectors::VectorPool` ([`alloc_vector`]): one
//!   owner per vector, a claim on a taken vector is refused, a restarted
//!   driver keeps its vector, an exhausted pool means a polled device. Each
//!   pool vector has a stub handler that notifies its owner's slot;
//! - **one driver-syscall dispatch** ([`syscall`]): the caller's grant (looked
//!   up by task) says which driver it is; there is no "try this module first".
//!
//! The driver syscalls are served WITHOUT the BG lock (ring3 routes the
//! `driver` profile before taking it), so task 0 can hand the CPU to a
//! driver while it waits for it. Everything this module locks is taken with
//! interrupts off.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

use eurodrv::abi::{self, DriverInfo};
use eurodrv::grant::{Bdf, DeviceGrant, Window};
use eurodrv::proto::{self, Msg};
use eurodrv::restart::{Decision, Policy, Tracker};
use eurodrv::vectors::{VecError, VectorPool};
use euromm::FrameAllocator;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::structures::idt::InterruptStackFrame;

use crate::euroipc;
use crate::interrupts::{UDRV_VECTORS, UDRV_VECTOR_BASE};
use crate::pci::{self, PciDevice};

// ── Per-driver resources (every userspace driver) ────────────────────────────

/// The userspace drivers. The discriminant indexes [`RES`], the grant table,
/// the IRQ counters and the vector pool's owners.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Drv {
    E1000d = 0,
    Nvmed = 1,
    Bcm5720d = 2,
    Megaraidd = 3,
}

pub(crate) const DRIVERS: usize = 4;
const ALL: [Drv; DRIVERS] = [Drv::E1000d, Drv::Nvmed, Drv::Bcm5720d, Drv::Megaraidd];

/// What one driver owns besides its grant: its EuroIPC port (only the kernel
/// sends to it), the kernel-side port it sends to (only its pid may), the
/// notification slot its interrupts bump and the pids its instances get
/// (`pid_base + start number`, below `pid_base + PID_SPAN`).
#[derive(Clone, Copy)]
pub(crate) struct DrvRes {
    pub name: &'static str,
    pub driver_port: u32,
    pub stack_port: u32,
    pub notify_slot: usize,
    pub pid_base: u64,
}

pub(crate) const PID_SPAN: u64 = 0x1000;

pub(crate) const RES: [DrvRes; DRIVERS] = [
    DrvRes { name: "e1000d", driver_port: 0x4E00, stack_port: 0x4E10, notify_slot: 0, pid_base: 0x2_0000 },
    DrvRes { name: "nvmed", driver_port: 0x4E20, stack_port: 0x4E30, notify_slot: 1, pid_base: 0x2_1000 },
    DrvRes { name: "bcm5720d", driver_port: 0x4E40, stack_port: 0x4E50, notify_slot: 2, pid_base: 0x2_2000 },
    DrvRes { name: "megaraidd", driver_port: 0x4E60, stack_port: 0x4E70, notify_slot: 3, pid_base: 0x2_3000 },
];

impl Drv {
    pub const fn res(self) -> DrvRes {
        RES[self as usize]
    }
    pub const fn name(self) -> &'static str {
        RES[self as usize].name
    }
}

/// No two drivers share a port, a notification slot or a pid; every slot exists.
const fn resources_distinct() -> bool {
    let mut i = 0;
    while i < DRIVERS {
        let a = RES[i];
        if a.notify_slot >= euroipc::NOTIFY_SLOTS || a.driver_port == a.stack_port {
            return false;
        }
        let mut j = i + 1;
        while j < DRIVERS {
            let b = RES[j];
            let ports_clash = a.driver_port == b.driver_port
                || a.driver_port == b.stack_port
                || a.stack_port == b.driver_port
                || a.stack_port == b.stack_port;
            let pids_clash = a.pid_base < b.pid_base + PID_SPAN && b.pid_base < a.pid_base + PID_SPAN;
            if ports_clash || a.notify_slot == b.notify_slot || pids_clash {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}
const _: () = assert!(resources_distinct(), "two userspace drivers share a port, notification slot or pid range");
const _: () = assert!(ALL.len() == DRIVERS && UDRV_VECTORS >= DRIVERS);

// ── The vector pool and its interrupt stubs ──────────────────────────────────

static VECTORS: Mutex<VectorPool> = Mutex::new(VectorPool::new(UDRV_VECTOR_BASE, UDRV_VECTORS as u8));
/// The driver bound to each pool vector (driver index + 1; 0 = none). Read
/// by the stubs: no lock in the interrupt path.
static VEC_OWNER: [AtomicU8; UDRV_VECTORS] = [const { AtomicU8::new(0) }; UDRV_VECTORS];
static IRQS: [AtomicU64; DRIVERS] = [const { AtomicU64::new(0) }; DRIVERS];
static IRQS_DELIVERED: [AtomicU64; DRIVERS] = [const { AtomicU64::new(0) }; DRIVERS];
/// Interrupts on a pool vector nobody owns (a device left programmed).
static IRQS_UNOWNED: AtomicU64 = AtomicU64::new(0);

/// The MSI-X vector for driver `d`: the one it holds, or the first free one.
/// `None` when the pool is exhausted: the device then runs polled.
pub(crate) fn alloc_vector(d: Drv) -> Option<u8> {
    let r = without_interrupts(|| {
        let r = VECTORS.lock().alloc(d as u8);
        if let Ok(v) = r {
            VEC_OWNER[(v - UDRV_VECTOR_BASE) as usize].store(d as u8 + 1, Ordering::Release);
        }
        r
    });
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            crate::serial_println!("[udrv] no MSI-X vector for {}: {e:?} (pool {:#x}..{:#x}): the device runs polled", d.name(), UDRV_VECTOR_BASE, UDRV_VECTOR_BASE as usize + UDRV_VECTORS - 1);
            None
        }
    }
}

/// Give driver `d`'s vector back (the device is off for good: bus mastering
/// is off, so it cannot raise an MSI any more).
pub(crate) fn release_vector(d: Drv) {
    without_interrupts(|| {
        let mut p = VECTORS.lock();
        if let Some(v) = p.vector_of(d as u8) {
            if p.release(v, d as u8).is_ok() {
                VEC_OWNER[(v - UDRV_VECTOR_BASE) as usize].store(0, Ordering::Release);
            }
        }
    });
}

pub(crate) fn vector_of(d: Drv) -> Option<u8> {
    without_interrupts(|| VECTORS.lock().vector_of(d as u8))
}

/// Interrupts raised for driver `d`, and delivered to it as `Irq` messages.
pub(crate) fn irqs(d: Drv) -> u64 {
    IRQS[d as usize].load(Ordering::Relaxed)
}
pub(crate) fn irqs_delivered(d: Drv) -> u64 {
    IRQS_DELIVERED[d as usize].load(Ordering::Relaxed)
}

/// The pool vector `i` fired: count it for its owner, notify the owner's
/// port, EOI. No lock, no allocation (notify_from_irq is two atomics).
#[inline(always)]
fn irq_pool(i: usize) {
    let o = VEC_OWNER[i].load(Ordering::Acquire);
    if o != 0 && (o as usize) <= DRIVERS {
        let d = o as usize - 1;
        IRQS[d].fetch_add(1, Ordering::Relaxed);
        euroipc::notify_from_irq(RES[d].notify_slot);
    } else {
        IRQS_UNOWNED.fetch_add(1, Ordering::Relaxed);
    }
    crate::apic::eoi();
}

extern "x86-interrupt" fn irq_pool0(_f: InterruptStackFrame) {
    irq_pool(0)
}
extern "x86-interrupt" fn irq_pool1(_f: InterruptStackFrame) {
    irq_pool(1)
}
extern "x86-interrupt" fn irq_pool2(_f: InterruptStackFrame) {
    irq_pool(2)
}
extern "x86-interrupt" fn irq_pool3(_f: InterruptStackFrame) {
    irq_pool(3)
}

/// The IDT entries of the pool, in vector order (interrupts.rs installs them).
pub(crate) const IRQ_STUBS: [extern "x86-interrupt" fn(InterruptStackFrame); UDRV_VECTORS] = [irq_pool0, irq_pool1, irq_pool2, irq_pool3];

/// The `Irq` message driver `d` receives for `n` pending notifications, in
/// its own wire format.
fn irq_msg(d: Drv, n: u32) -> Vec<u8> {
    IRQS_DELIVERED[d as usize].fetch_add(n as u64, Ordering::Relaxed);
    let mut b = [0u8; 16];
    let len = match d {
        Drv::Megaraidd => euromegaraid::blkif::encode(&euromegaraid::blkif::Msg::Irq { count: n }, &mut b).unwrap_or(0),
        _ => proto::encode(&Msg::Irq { count: n }, &mut b).unwrap_or(0),
    };
    b[..len].to_vec()
}

/// [udrv-vec]: every driver holding a vector holds a different one, the
/// stubs point at the right owner, and a second claim on a held vector is
/// refused. Boot self-test, after the drivers started.
pub fn selftest_vectors() {
    let (pool, owners) = without_interrupts(|| {
        let p = *VECTORS.lock();
        let o: [u8; UDRV_VECTORS] = core::array::from_fn(|i| VEC_OWNER[i].load(Ordering::Acquire));
        (p, o)
    });
    let mut held = Vec::new();
    for d in ALL {
        if let Some(v) = pool.vector_of(d as u8) {
            held.push((d, v));
        }
    }
    if held.is_empty() {
        crate::serial_println!("[udrv-vec] no userspace driver holds an MSI-X vector on this machine (pool {:#x}..{:#x}) ⬜", UDRV_VECTOR_BASE, UDRV_VECTOR_BASE as usize + UDRV_VECTORS - 1);
        return;
    }
    let distinct = held.iter().enumerate().all(|(i, a)| held.iter().skip(i + 1).all(|b| a.1 != b.1));
    let stubs_ok = held.iter().all(|(d, v)| owners[(*v - UDRV_VECTOR_BASE) as usize] == *d as u8 + 1)
        && (0..UDRV_VECTORS).all(|i| owners[i] == 0 || held.iter().any(|(d, v)| (*v - UDRV_VECTOR_BASE) as usize == i && owners[i] == *d as u8 + 1));
    // A second claim on every held vector, by every other driver: all refused.
    let mut probe = pool;
    let mut refused = 0;
    let mut tried = 0;
    for &(holder, v) in &held {
        for d in ALL.iter().filter(|d| **d != holder) {
            tried += 1;
            if probe.claim(v, *d as u8) == Err(VecError::Taken { owner: holder as u8 }) {
                refused += 1;
            }
        }
    }
    let list: Vec<String> = held.iter().map(|(d, v)| alloc::format!("{} {v:#x}", d.name())).collect();
    let ok = distinct && stubs_ok && refused == tried;
    crate::serial_println!(
        "[udrv-vec] MSI-X vectors from the pool {:#x}..{:#x}: {}; distinct={distinct}, stubs bound to their owner={stubs_ok}, second claims on a held vector refused {refused}/{tried}; unowned interrupts {} → {}",
        UDRV_VECTOR_BASE, UDRV_VECTOR_BASE as usize + UDRV_VECTORS - 1,
        list.join(", "),
        IRQS_UNOWNED.load(Ordering::Relaxed),
        if ok { "OK ✓" } else { "FAILED" }
    );
}

// ── e1000d ───────────────────────────────────────────────────────────────────

static E1000D_ELF: &[u8] = include_bytes!("../../userland/e1000d.elf");
static E1000D_SIG: &[u8] = include_bytes!("../../userland/e1000d.elf.sig");

/// EuroIPC ports: the driver's own, and the netstack's.
pub const DRIVER_PORT: u32 = Drv::E1000d.res().driver_port;
pub const STACK_PORT: u32 = Drv::E1000d.res().stack_port;
/// The DMA window: two descriptor rings + 2 x 16 buffers of 2 KiB need 18 pages.
const DMA_PAGES: usize = 32;
const DOMAIN: &str = "e1000d";
const ARENA_MIB: u64 = 4;

// ── State read by the data path (atomics: no lock on the frame path) ──────────
static ACTIVE: AtomicBool = AtomicBool::new(false);
static LINK_UP: AtomicBool = AtomicBool::new(false);
static MAC: AtomicU64 = AtomicU64::new(0);
static DRIVER_PID: AtomicU64 = AtomicU64::new(u64::MAX);
static TX_FRAMES: AtomicU64 = AtomicU64::new(0);
static RX_FRAMES: AtomicU64 = AtomicU64::new(0);
static TX_REFUSED: AtomicU64 = AtomicU64::new(0);
static HELLOS: AtomicU32 = AtomicU32::new(0);
/// Set by on_death: a send attempted right after the death was refused.
static SEND_REFUSED_WHILE_DEAD: AtomicBool = AtomicBool::new(false);
/// The last `Report` from the driver: (code, a, b), code 0 = none.
static REPORT: Mutex<(u8, u64, u64)> = Mutex::new((0, 0, 0));

/// Words a driver may get after the standard `DriverInfo` block.
pub(crate) const MAX_EXTRA_WORDS: usize = 4;

/// What a driver syscall needs to know about its caller (looked up by task).
#[derive(Clone, Copy)]
pub(crate) struct SysGrant {
    /// Which driver this is: decides the wire format and who is woken.
    pub drv: Drv,
    pub task: usize,
    pub pid: u64,
    pub pml4: u64,
    pub arena: u64,
    pub arena_bytes: u64,
    pub info: DriverInfo,
    /// Driver-specific words after `info` in SYS_DRV_INFO (bcm5720d: the APE
    /// window and its options), `extra_words` of them.
    pub extra: [u64; MAX_EXTRA_WORDS],
    pub extra_words: usize,
}

impl SysGrant {
    /// A grant with no extra info words.
    pub fn new(drv: Drv, task: usize, pid: u64, pml4: u64, arena: u64, arena_bytes: u64, info: DriverInfo) -> SysGrant {
        SysGrant { drv, task, pid, pml4, arena, arena_bytes, info, extra: [0; MAX_EXTRA_WORDS], extra_words: 0 }
    }
}

/// The running drivers' grants, one slot per [`Drv`].
static SYS: Mutex<[Option<SysGrant>; DRIVERS]> = Mutex::new([None; DRIVERS]);

fn sys_grant(task: usize) -> Option<SysGrant> {
    without_interrupts(|| SYS.lock().iter().flatten().find(|g| g.task == task).copied())
}

/// Drop every grant held by process `pid` (reap_dead, before its task slot can
/// be reused by another process).
pub(crate) fn revoke_pid(pid: u64) {
    without_interrupts(|| {
        for g in SYS.lock().iter_mut() {
            if g.is_some_and(|g| g.pid == pid) {
                *g = None;
            }
        }
    });
}

/// Publish (or clear) the grant of driver `d` (interrupts off).
pub(crate) fn set_grant(d: Drv, g: Option<SysGrant>) {
    debug_assert!(g.is_none_or(|g| g.drv == d));
    without_interrupts(|| SYS.lock()[d as usize] = g);
}

/// One running driver instance (task-0 state).
struct Instance {
    pid: u64,
    task: usize,
    grant: DeviceGrant,
    iommu: bool,
}

struct Supervisor {
    dev: Option<PciDevice>,
    mmio: Window,
    inst: Option<Instance>,
    tracker: Tracker,
    restart_at: Option<u64>,
    gave_up: bool,
    starts: u32,
    msix: bool,
    last_death: Option<String>,
}

static SUP: Mutex<Supervisor> = Mutex::new(Supervisor {
    dev: None,
    mmio: Window { base: 0, len: 0 },
    inst: None,
    tracker: Tracker::new(),
    restart_at: None,
    gave_up: false,
    starts: 0,
    msix: false,
    last_death: None,
});

// ── Driver syscalls (every userspace driver) ────────────────────────────────

/// Is `[ptr, ptr+len)` inside the caller's arena and accessible (writable when
/// `write`) for ring 3 in its own page tables?
fn user_ok(g: &SysGrant, ptr: u64, len: usize, write: bool) -> bool {
    let Some(end) = ptr.checked_add(len as u64) else { return false };
    if len == 0 || ptr < g.arena || end > g.arena + g.arena_bytes {
        return false;
    }
    if write {
        return crate::paging::user_range_writable_in(g.pml4, ptr, len);
    }
    let mut p = ptr & !0xFFF;
    while p < end {
        if !crate::paging::user_accessible_in(g.pml4, p) {
            return false;
        }
        p += 4096;
    }
    true
}

fn err(e: i64) -> Option<u64> {
    Some(e as u64)
}

const _: () = assert!(euromegaraid::blkif::MAX_MSG <= proto::MAX_MSG);

/// Largest message driver `d` sends or receives (its wire format's bound).
fn max_msg(d: Drv) -> usize {
    match d {
        Drv::Megaraidd => euromegaraid::blkif::MAX_MSG,
        _ => proto::MAX_MSG,
    }
}

/// Bulk traffic stays out of the audit ring (frames, block completions).
fn is_bulk(d: Drv, data: &[u8]) -> bool {
    match d {
        Drv::Megaraidd => matches!(euromegaraid::blkif::decode(data), Ok(euromegaraid::blkif::Msg::Done { .. })),
        _ => matches!(proto::decode(data), Ok(Msg::Rx(_))),
    }
}

/// The driver sent the kernel something: wake whoever sleeps waiting for it.
fn after_send(d: Drv) {
    match d {
        // A task sleeping in a network wait (net.rs RxWait) has something to
        // look at now: wake it instead of letting it sleep out its tick.
        Drv::E1000d => crate::net::wake_rx_waiters(),
        Drv::Nvmed => crate::udrv_blk::wake(),
        // Their kernel sides poll per tick (bcm5720d has no netstack route;
        // mrblk polls its completions).
        Drv::Bcm5720d | Drv::Megaraidd => {}
    }
}

/// THE driver-syscall entry: every task under the `driver` profile comes
/// here (ring3.rs), whichever driver it is. The caller's grant, found by its
/// task, says which driver it is and what it may touch; a driver-profile task
/// without a grant gets EPERM for the driver calls. `None`: not a driver call
/// (exit, and anything the profile kills), the ordinary route handles it.
pub fn syscall(task: usize, num: u64, a1: u64, a2: u64, a3: u64) -> Option<u64> {
    if !(abi::is_driver_call(num) || matches!(num, 0x6107 | 0x6108 | 0x6109)) {
        return None;
    }
    let Some(g) = sys_grant(task) else { return err(abi::EPERM) };
    let d = g.drv;
    match num {
        abi::SYS_DRV_INFO => {
            // The standard block, then the driver's extra words.
            let mut b = [0u8; (abi::INFO_WORDS + MAX_EXTRA_WORDS) * 8];
            g.info.write_bytes(&mut b[..abi::INFO_WORDS * 8]);
            let extra = g.extra_words.min(MAX_EXTRA_WORDS);
            for (i, w) in g.extra[..extra].iter().enumerate() {
                let s = (abi::INFO_WORDS + i) * 8;
                b[s..s + 8].copy_from_slice(&w.to_le_bytes());
            }
            let n = (a2 as usize).min((abi::INFO_WORDS + extra) * 8);
            if n == 0 || !user_ok(&g, a1, n, true) {
                return err(abi::EFAULT);
            }
            // SAFETY: checked above: inside the caller's arena, user-writable in its tables.
            unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), a1 as *mut u8, n) };
            Some(0)
        }
        abi::SYS_IPC_RECV => {
            let max = (a2 as usize).min(max_msg(d));
            if max == 0 || !user_ok(&g, a1, max, true) {
                return err(abi::EFAULT);
            }
            for attempt in 0..2 {
                if let Some((_, data)) = euroipc::take(g.info.own_port as u32, |n| irq_msg(d, n)) {
                    if data.len() > max {
                        return err(abi::ENOSPC); // dropped: the driver's buffer is MAX_MSG
                    }
                    // SAFETY: checked above (arena + writable), length bounded by max.
                    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), a1 as *mut u8, data.len()) };
                    return Some(data.len() as u64);
                }
                if attempt == 0 {
                    // Nothing pending: sleep one tick and give the CPU away (no lock held).
                    crate::sched::sleep_ticks(1);
                    crate::sched::yield_now();
                }
            }
            err(abi::EAGAIN)
        }
        abi::SYS_IPC_SEND => {
            let (port, len) = (a1 as u32, a3 as usize);
            if port as u64 != g.info.stack_port {
                return err(abi::EPERM); // the grant names one destination
            }
            if len == 0 || len > max_msg(d) || !user_ok(&g, a2, len, false) {
                return err(abi::EFAULT);
            }
            let mut buf = [0u8; proto::MAX_MSG];
            // SAFETY: checked above (inside the arena, user-accessible), len bounded
            // by max_msg(d) <= proto::MAX_MSG.
            unsafe { core::ptr::copy_nonoverlapping(a2 as *const u8, buf.as_mut_ptr(), len) };
            let data = &buf[..len];
            let r = euroipc::send_with(g.pid, port, data, !is_bulk(d, data));
            if r >= 0 {
                after_send(d);
            }
            Some(r as u64)
        }
        0x6107 => {
            // log(ptr, len): one line to the serial log, tagged with the driver.
            let len = (a2 as usize).min(200);
            if len == 0 || !user_ok(&g, a1, len, false) {
                return err(abi::EFAULT);
            }
            let mut b = [0u8; 200];
            // SAFETY: checked above.
            unsafe { core::ptr::copy_nonoverlapping(a1 as *const u8, b.as_mut_ptr(), len) };
            crate::serial_println!("[{} pid {}] {}", d.name(), g.pid, String::from_utf8_lossy(&b[..len]));
            Some(0)
        }
        0x6108 => Some(crate::interrupts::ticks() * 10),
        0x6109 => {
            crate::sched::sleep_ticks(a1.min(600_000).div_ceil(10).max(1));
            crate::sched::yield_now();
            Some(0)
        }
        _ => None,
    }
}

// ── The NIC backend (nic.rs, KIND_USER) ──────────────────────────────────────

pub fn mac() -> Option<[u8; 6]> {
    let m = MAC.load(Ordering::Relaxed);
    if m == 0 {
        return None;
    }
    let b = m.to_le_bytes();
    Some([b[0], b[1], b[2], b[3], b[4], b[5]])
}

pub fn link_up() -> bool {
    LINK_UP.load(Ordering::Relaxed)
}

/// Hand a frame to the driver. False when the link is down (driver dead or
/// restarting) or its queue is full.
pub fn nic_send(frame: &[u8]) -> bool {
    if !LINK_UP.load(Ordering::Relaxed) {
        TX_REFUSED.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let mut b = [0u8; proto::MAX_MSG];
    let Ok(n) = proto::encode(&Msg::Tx(frame), &mut b) else { return false };
    let ok = euroipc::send_with(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n], false) >= 0;
    if ok {
        TX_FRAMES.fetch_add(1, Ordering::Relaxed);
    }
    ok
}

/// Handle the control messages on the netstack port; return the next frame.
fn take_stack() -> Option<Vec<u8>> {
    for _ in 0..64 {
        let (sender, data) = euroipc::take(STACK_PORT, |_| Vec::new())?;
        if sender != DRIVER_PID.load(Ordering::Relaxed) {
            continue; // the port is restricted; belt and braces
        }
        match proto::decode(&data) {
            Ok(Msg::Rx(f)) => {
                RX_FRAMES.fetch_add(1, Ordering::Relaxed);
                return Some(f.to_vec());
            }
            Ok(Msg::Hello { mac }) => {
                let mut w = [0u8; 8];
                w[..6].copy_from_slice(&mac);
                MAC.store(u64::from_le_bytes(w), Ordering::Relaxed);
                LINK_UP.store(true, Ordering::Relaxed);
                HELLOS.fetch_add(1, Ordering::Relaxed);
                crate::nic::bind_user();
            }
            Ok(Msg::Report { code, a, b }) => {
                without_interrupts(|| *REPORT.lock() = (code, a, b));
            }
            _ => {} // nothing else is valid from a driver: ignore
        }
    }
    None
}

/// Next received frame. When nothing is queued and the caller may give the
/// CPU away (task context with interrupts on: the boot self-tests, the desktop
/// loop), yield so the driver can run. In a syscall (interrupts off) it does
/// not yield: that context may hold the BG lock, and a yield there could run a
/// process that needs it.
pub fn nic_poll() -> Option<Vec<u8>> {
    if let Some(f) = take_stack() {
        return Some(f);
    }
    // At most one yield per tick: the network code polls in tight spin loops
    // with iteration-count timeouts, and with busy processes around a yield can
    // cost a whole round of time slices. The timer preempts the poller anyway;
    // the yield only shortens the wait for the driver.
    if x86_64::instructions::interrupts::are_enabled() {
        static LAST_YIELD: AtomicU64 = AtomicU64::new(u64::MAX);
        let t = crate::interrupts::ticks();
        if LAST_YIELD.swap(t, Ordering::Relaxed) != t {
            crate::sched::yield_now();
        }
    }
    None
}

/// Process control messages (Hello/Report) without consuming frames' order:
/// frames found are pushed back as the netstack's next reads would see them.
fn pump_control() {
    // Frames are rare while we wait for control messages; route them into the
    // netstack's queues exactly like a reader would.
    if net_configured() {
        crate::net::drain_rx();
    } else {
        while take_stack().is_some() {}
    }
}

fn net_configured() -> bool {
    crate::net::get().is_some()
}

// ── Supervisor ───────────────────────────────────────────────────────────────

fn find_device() -> Option<PciDevice> {
    pci::find(|d| d.vendor == 0x8086 && (d.device == 0x100E || d.device == 0x10D3))
}

/// Is there a NIC the userspace driver drives (82540EM / 82574L)? The boot
/// uses this to move its network part behind the scheduler.
pub fn device_present() -> bool {
    find_device().is_some()
}

/// Size of memory BAR 0, probed with decode off (PCI spec 6.2.5.1).
pub(crate) fn bar0_window(dev: &PciDevice) -> Option<Window> {
    let base = dev.bar_addr(0);
    let off = 0x10u8;
    let cmd = pci::cfg_read32(dev.bus, dev.dev, dev.func, 0x04);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, cmd & !0x6);
    let orig = pci::cfg_read32(dev.bus, dev.dev, dev.func, off);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, off, 0xFFFF_FFFF);
    let probe = pci::cfg_read32(dev.bus, dev.dev, dev.func, off);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, off, orig);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, cmd);
    let size = eurodrv::grant::mem_bar_size(probe)?;
    Window::page_round(base, size)
}

pub(crate) fn bus_master(dev: &PciDevice, on: bool) {
    let cmd = pci::cfg_read32(dev.bus, dev.dev, dev.func, 0x04);
    let new = if on { cmd | 0x6 } else { cmd & !0x4 };
    pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, new);
}

/// Start one driver instance. Task 0 only (uses the boot frame allocator).
fn start(sup: &mut Supervisor, falloc: &mut FrameAllocator) -> Result<u64, &'static str> {
    let dev = sup.dev.ok_or("no device")?;
    if !crate::crypto::verify(E1000D_ELF, E1000D_SIG) {
        return Err("e1000d.elf signature invalid: refused (verify-before-execute)");
    }
    let bdf = Bdf { bus: dev.bus, dev: dev.dev, func: dev.func };
    let dma_base = falloc.allocate_aligned(DMA_PAGES, 1).map_err(|_| "no memory for the DMA window")?;
    // SAFETY: fresh frames from the allocator, identity-mapped.
    unsafe { core::ptr::write_bytes(dma_base as *mut u8, 0, DMA_PAGES * 4096) };
    let dma = Window::new(dma_base, (DMA_PAGES * 4096) as u64);
    // The BAR must not be RAM: below the PCI hole (2 GiB on every PC layout we
    // drive) everything the allocator covers is RAM.
    let ram = Window::new(0, falloc.total_bytes().min(0x8000_0000));
    let irq = if sup.msix { alloc_vector(Drv::E1000d) } else { None };
    let grant = match DeviceGrant::new(bdf, sup.mmio, dma, irq, &[ram], &[sup.mmio]) {
        Ok(g) => g,
        Err(_) => {
            free_dma(falloc, dma_base);
            return Err("grant refused by eurodrv::grant");
        }
    };
    // IOMMU first: the device gets its own domain before bus mastering is on.
    let iommu = crate::iommu::attach_runtime(falloc, DOMAIN, (bdf.bus, bdf.dev, bdf.func), dma_base, DMA_PAGES).is_some();
    if crate::iommu::translating() && !iommu {
        free_dma(falloc, dma_base);
        return Err("IOMMU translation is on but the driver domain could not be built: refused");
    }
    let pid = Drv::E1000d.res().pid_base + sup.starts as u64;
    let info = DriverInfo {
        abi: abi::ABI_VERSION,
        vendor: dev.vendor as u64,
        device: dev.device as u64,
        bdf: bdf.sid() as u64,
        mmio_base: grant.mmio.base,
        mmio_len: grant.mmio.len,
        dma_base,
        dma_len: dma.len,
        own_port: DRIVER_PORT as u64,
        stack_port: STACK_PORT as u64,
        irq_vector: irq.unwrap_or(0) as u64,
        flags: if iommu { abi::FLAG_IOMMU } else { 0 } | if irq.is_some() { abi::FLAG_MSIX } else { 0 },
    };
    // Ports before the process exists: its first receive must find them.
    euroipc::unregister(DRIVER_PORT);
    euroipc::register_restricted(pid, DRIVER_PORT, euroipc::KERNEL_PID);
    euroipc::set_only_from(STACK_PORT, pid);
    euroipc::bind_notify(Drv::E1000d.res().notify_slot, DRIVER_PORT);
    DRIVER_PID.store(pid, Ordering::Relaxed);
    // Spawn, map the grant, publish it: all with interrupts off, so the task
    // cannot run before its address space holds exactly its grant.
    let spawned = without_interrupts(|| -> Result<usize, &'static str> {
        let task = crate::ring3::spawn_euro_app(falloc, E1000D_ELF, pid, &[b"e1000d"], ARENA_MIB, crate::ring3::CAP_CONSOLE, eurosandbox::Profile::Driver)
            .ok_or("spawn failed (memory)")?;
        let (t, pml4, arena, arena_bytes) = crate::ring3::bg_layout(pid).ok_or("spawned process not found")?;
        debug_assert_eq!(t, task);
        for pa in grant.mmio.page_addrs() {
            if !crate::paging::map_user_identity_4k(falloc, pml4, pa, true) {
                crate::sched::mark_dead(task);
                return Err("could not map the BAR");
            }
        }
        for pa in grant.dma.page_addrs() {
            if !crate::paging::map_user_identity_4k(falloc, pml4, pa, false) {
                crate::sched::mark_dead(task);
                return Err("could not map the DMA window");
            }
        }
        SYS.lock()[Drv::E1000d as usize] = Some(SysGrant::new(Drv::E1000d, task, pid, pml4, arena, arena_bytes, info));
        // Interrupt routing and bus mastering before the driver can run: its
        // first DMA must find the device enabled (the IOMMU domain is in place).
        if let Some(v) = irq {
            crate::msix::enable(&dev, 0, v, crate::apic::lapic_id() as u8);
        }
        bus_master(&dev, true);
        Ok(task)
    });
    let task = match spawned {
        Ok(t) => t,
        Err(e) => {
            if iommu {
                crate::iommu::detach_runtime(falloc, DOMAIN);
            }
            free_dma(falloc, dma_base);
            return Err(e);
        }
    };
    sup.starts += 1;
    sup.inst = Some(Instance { pid, task, grant, iommu });
    sup.restart_at = None;
    ACTIVE.store(true, Ordering::Relaxed);
    crate::serial_println!(
        "[udrv] e1000d started: pid {pid}, task {task}, profile driver · BAR0 {:#x}+{:#x} ({} pages) · DMA {:#x}+{:#x} · IRQ {} · IOMMU {}",
        grant.mmio.base, grant.mmio.len, grant.mmio.pages(), dma_base, dma.len,
        irq.map(|v| alloc::format!("MSI-X {v:#x} -> EuroIPC")).unwrap_or_else(|| String::from("none (polled)")),
        if iommu { "own domain (DMA confined)" } else { "absent: DMA NOT confined" }
    );
    Ok(pid)
}

fn free_dma(falloc: &mut FrameAllocator, base: u64) {
    for i in 0..DMA_PAGES as u64 {
        let _ = falloc.free(base + i * 4096);
    }
}

/// The driver died: quiesce the device, then free, then decide on a restart.
fn on_death(sup: &mut Supervisor, falloc: &mut FrameAllocator, inst: Instance) {
    let reason = crate::ring3::bg_end_reason(inst.pid).unwrap_or_else(|| String::from("terminated"));
    LINK_UP.store(false, Ordering::Relaxed);
    SEND_REFUSED_WHILE_DEAD.store(!nic_send(&[0u8; 60]), Ordering::Relaxed);
    set_grant(Drv::E1000d, None);
    // 1. No new DMA: bus mastering off.
    if let Some(dev) = sup.dev {
        bus_master(&dev, false);
    }
    // 2. No translation to the window any more: back to the empty table, flushed.
    let revoked = inst.iommu && crate::iommu::detach_runtime(falloc, DOMAIN);
    // 3. Its port and anything queued for it go.
    euroipc::unregister(DRIVER_PORT);
    // 4. The process (arena, page tables with the BAR and DMA mappings), then the window.
    crate::ring3::reap_dead(falloc);
    free_dma(falloc, inst.grant.dma.base);
    let now = crate::interrupts::ticks();
    let decision = sup.tracker.on_death(now, &Policy::NIC);
    crate::serial_println!(
        "[udrv] e1000d pid {} died ({reason}) -> device quiesced: bus-master off, IOMMU domain {}, {DMA_PAGES} DMA pages freed; {}",
        inst.pid,
        if revoked { "revoked" } else { "n/a (no IOMMU)" },
        match decision {
            Decision::RestartAt(t) => alloc::format!("restart at tick {t} (death #{})", sup.tracker.total_deaths()),
            Decision::GiveUp => String::from("GIVING UP (too many deaths in the window; device stays off)"),
        }
    );
    sup.last_death = Some(reason);
    match decision {
        Decision::RestartAt(t) => sup.restart_at = Some(t),
        Decision::GiveUp => {
            sup.gave_up = true;
            ACTIVE.store(false, Ordering::Relaxed);
            release_vector(Drv::E1000d);
        }
    }
}

/// Supervision tick: notice a dead driver, quiesce, restart on schedule.
/// Task 0 only (the desktop loop and the boot self-test).
pub fn supervise(falloc: &mut FrameAllocator) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let mut sup = SUP.lock();
    if let Some(inst) = sup.inst.as_ref() {
        if !crate::ring3::bg_alive(inst.pid) {
            let inst = sup.inst.take().unwrap();
            on_death(&mut sup, falloc, inst);
        }
    }
    if sup.inst.is_none() && !sup.gave_up {
        if let Some(t) = sup.restart_at {
            if crate::interrupts::ticks() >= t {
                if let Err(e) = start(&mut sup, falloc) {
                    crate::serial_println!("[udrv] restart failed: {e}");
                    sup.restart_at = Some(crate::interrupts::ticks() + 100);
                }
            }
        }
    }
    drop(sup);
    // Hello/Report messages arrive on the stack port even when nobody reads frames.
    if !LINK_UP.load(Ordering::Relaxed) {
        pump_control();
    }
}

/// Wait (yielding) until `cond` holds or `ticks` pass. Task 0, interrupts on.
fn wait_for(ticks: u64, falloc: &mut FrameAllocator, mut cond: impl FnMut() -> bool) -> bool {
    let end = crate::interrupts::ticks() + ticks;
    loop {
        supervise(falloc);
        if cond() {
            return true;
        }
        if crate::interrupts::ticks() >= end {
            return false;
        }
        crate::sched::yield_now();
        x86_64::instructions::hlt();
    }
}

// ── Network bring-up over the driver (the same steps as the kernel path) ─────

fn ipfmt(ip: euronet::ipv4::Ipv4Addr) -> String {
    alloc::format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3])
}

/// DHCP (DISCOVER/OFFER/REQUEST/ACK) + ARP + DNS + HTTP GET, all through the
/// userspace driver. Returns the lines for the log and whether it all worked.
fn net_roundtrip(falloc: &mut FrameAllocator, xid: u32, probe: bool) -> (bool, Vec<String>) {
    use euronet::dhcp;
    use euronet::ethernet::{EtherType, EthernetHeader, MacAddr};
    use euronet::ipv4::{Ipv4Addr, Ipv4Header, Protocol};
    use euronet::udp::UdpDatagram;
    let mut out = Vec::new();
    let Some(m) = mac() else { return (false, alloc::vec![String::from("no MAC (driver not up)")]) };
    let my_mac = MacAddr(m);
    let any = Ipv4Addr([0, 0, 0, 0]);
    let bcast = Ipv4Addr([255, 255, 255, 255]);
    let send_dhcp = |mt: u8, req: Option<Ipv4Addr>, sid: Option<Ipv4Addr>| {
        let payload = dhcp::build(mt, xid, my_mac.0, req, sid);
        let seg = UdpDatagram { src_port: 68, dst_port: 67, payload }.build(any, bcast);
        let ipf = Ipv4Header { protocol: Protocol::Udp, ttl: 64, src: any, dst: bcast, total_length: 0, identification: 0x5544 }.build(&seg);
        let frame = EthernetHeader { dst: MacAddr::BROADCAST, src: my_mac, ethertype: EtherType::Ipv4 }.build(&ipf);
        crate::nic::send(&frame)
    };
    // Frames straight from the driver (the netstack is not configured yet).
    let wait_dhcp = |want: u8, falloc: &mut FrameAllocator| -> Option<dhcp::DhcpInfo> {
        let end = crate::interrupts::ticks() + 300;
        while crate::interrupts::ticks() < end {
            supervise(falloc);
            match take_stack() {
                Some(rx) => {
                    if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                        if h.ethertype == EtherType::Ipv4 {
                            if let Ok((iph, ipl)) = Ipv4Header::parse(p) {
                                if iph.protocol == Protocol::Udp && ipl.len() > 8 && u16::from_be_bytes([ipl[2], ipl[3]]) == 68 {
                                    if let Some(info) = dhcp::parse(&ipl[8..]) {
                                        if info.msg_type == want {
                                            return Some(info);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                None => {
                    crate::sched::yield_now();
                    x86_64::instructions::hlt();
                }
            }
        }
        None
    };
    let mut offer = None;
    for _ in 0..4 {
        if !send_dhcp(dhcp::DISCOVER, None, None) {
            out.push(String::from("DISCOVER not sent (link down)"));
            return (false, out);
        }
        offer = wait_dhcp(dhcp::OFFER, falloc);
        if offer.is_some() {
            break;
        }
    }
    let Some(o) = offer else {
        out.push(String::from("no DHCP OFFER"));
        return (false, out);
    };
    send_dhcp(dhcp::REQUEST, Some(o.your_ip), Some(o.server_id));
    let Some(a) = wait_dhcp(dhcp::ACK, falloc) else {
        out.push(alloc::format!("OFFER {} but no ACK", ipfmt(o.your_ip)));
        return (false, out);
    };
    let my_ip = a.your_ip;
    let gw_ip = a.router.unwrap_or(Ipv4Addr::new(10, 0, 2, 2));
    let dns_ip = a.dns.unwrap_or(Ipv4Addr::new(10, 0, 2, 3));
    out.push(alloc::format!("DHCP ACK: lease {} (router {}, dns {}, {}s)", ipfmt(my_ip), ipfmt(gw_ip), ipfmt(dns_ip), a.lease_secs));
    let save = |gw_mac: MacAddr, dns_mac: MacAddr| {
        crate::net::save(crate::net::NetCfg {
            my_mac,
            my_ip,
            gw_ip,
            gw_mac,
            dns_ip,
            dns_mac,
            link_local: euronet::ipv6::Ipv6Addr::link_local_from_mac(my_mac.0),
            router_ll: None,
            router_mac: None,
        })
    };
    // The netstack reads through its demux only once it knows its own address.
    save(MacAddr::ZERO, MacAddr::ZERO);
    let mut gw_mac = None;
    for _ in 0..3 {
        gw_mac = crate::net::arp_resolve(my_mac, my_ip, gw_ip);
        if gw_mac.is_some() {
            break;
        }
    }
    let Some(gwm) = gw_mac else {
        out.push(String::from("no ARP reply from the gateway"));
        return (false, out);
    };
    let dns_mac = crate::net::arp_resolve(my_mac, my_ip, dns_ip).unwrap_or(gwm);
    save(gwm, dns_mac);
    out.push(alloc::format!("ARP: gateway {} is-at {:02x?}", ipfmt(gw_ip), gwm.0));
    if !probe {
        // Configuration only (the public image): no probe of outside hosts.
        return (true, out);
    }
    let host = "example.com";
    let Some(ip) = crate::net::dns_query(my_mac, my_ip, dns_mac, dns_ip, host) else {
        out.push(String::from("no DNS reply"));
        return (false, out);
    };
    out.push(alloc::format!("DNS: {host} = {}", ipfmt(ip)));
    match crate::net::http_get(my_mac, my_ip, gwm, ip, host, "/") {
        Some((status, data)) => {
            let ok = status.contains(" 200") || status.contains(" 30");
            out.push(alloc::format!("HTTP GET http://{host}/ -> {} bytes, {}", data.len(), status.trim()));
            (ok, out)
        }
        None => {
            out.push(String::from("no HTTP response"));
            (false, out)
        }
    }
}

fn wait_link(falloc: &mut FrameAllocator, want_hellos: u32, ticks: u64) -> bool {
    wait_for(ticks, falloc, || HELLOS.load(Ordering::Relaxed) >= want_hellos && LINK_UP.load(Ordering::Relaxed))
}

fn mac_str() -> String {
    mac().map(|m| alloc::format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])).unwrap_or_default()
}

/// Boot: with `net.driver=user`, start e1000d and prove every step.
/// Call from task 0 after the scheduler runs, interrupts are on and
/// `iommu::program` has run.
pub fn boot(falloc: &mut FrameAllocator) {
    if !crate::bootopt::net_user_driver() {
        return;
    }
    crate::serial_println!("[udrv] net.driver=user: the e1000 driver runs in ring 3 (e1000d); the in-kernel e1000 stays unbound");
    if crate::nic::kind() != "none" {
        crate::serial_println!("[udrv] another NIC ({}) is already bound by the kernel: e1000d not started", crate::nic::kind());
        return;
    }
    let Some(dev) = find_device() else {
        crate::serial_println!("[udrv] no e1000/e1000e (8086:100e / 8086:10d3) present: nothing to drive");
        return;
    };
    let Some(mmio) = bar0_window(&dev) else {
        crate::serial_println!("[udrv] e1000 BAR0 not a memory BAR: not started");
        return;
    };
    pci::claim(dev.bus, dev.dev, dev.func, "e1000d (userspace)");
    // The netstack's port: nobody may send until a driver holds the grant.
    euroipc::register_restricted(euroipc::KERNEL_PID, STACK_PORT, u64::MAX);
    let msix = dev.device == 0x10D3 && dev.find_cap(0x11).is_some();
    {
        let mut sup = SUP.lock();
        sup.dev = Some(dev);
        sup.mmio = mmio;
        sup.msix = msix;
    }
    let started = {
        let mut sup = SUP.lock();
        start(&mut sup, falloc)
    };
    let pid = match started {
        Ok(p) => p,
        Err(e) => {
            crate::serial_println!("[udrv-grant] e1000d not started: {e} → FAILED");
            return;
        }
    };
    selftest_grant(pid);
    if !wait_link(falloc, 1, 1000) {
        crate::serial_println!("[udrv-e1000] e1000d pid {pid} did not report link up within 10 s → FAILED");
        return;
    }
    crate::serial_println!(
        "[udrv-e1000] e1000d (pid {pid}, ring 3, profile driver) brought the {} up: MAC {} reported over EuroIPC; nic backend = {} → OK ✓",
        if dev.device == 0x10D3 { "82574L (e1000e)" } else { "82540EM (e1000)" },
        mac_str(),
        crate::nic::kind()
    );
    let (ok, lines) = net_roundtrip(falloc, 0x4555_0001, cfg!(feature = "selftest"));
    for l in &lines {
        crate::serial_println!("[udrv-net]   {l}");
    }
    if !cfg!(feature = "selftest") {
        // The public image: bring the network up, prove nothing destructive
        // (no malicious-DMA probe, no deliberate driver crash at every boot).
        crate::serial_println!(
            "[udrv-net] network over e1000d: {}",
            if ok { "DHCP + ARP done, netstack configured" } else { "NOT configured (no DHCP lease / gateway)" }
        );
        crate::serial_println!("[udrv] {}", status_line());
        return;
    }
    crate::serial_println!(
        "[udrv-net] netstack over EuroIPC through e1000d: DHCP + ARP + DNS + HTTP: tx {} rx {} frames → {}",
        TX_FRAMES.load(Ordering::Relaxed),
        RX_FRAMES.load(Ordering::Relaxed),
        if ok { "OK (the kernel e1000 driver was not used) ✓" } else { "FAILED" }
    );
    selftest_irq(msix);
    selftest_dma(falloc);
    selftest_crash(falloc, pid);
    crate::serial_println!("[udrv] {}", status_line());
}

fn selftest_grant(pid: u64) {
    let Some((_, pml4, _, _)) = crate::ring3::bg_layout(pid) else { return };
    let sup = SUP.lock();
    let Some(inst) = sup.inst.as_ref() else { return };
    let g = inst.grant;
    let bar_first = crate::paging::user_accessible_in(pml4, g.mmio.base);
    let bar_last = crate::paging::user_accessible_in(pml4, g.mmio.base + g.mmio.len - 4096);
    let past_bar = crate::paging::user_accessible_in(pml4, g.mmio.base + g.mmio.len);
    let before_bar = crate::paging::user_accessible_in(pml4, g.mmio.base - 4096);
    let dma_in = crate::paging::user_accessible_in(pml4, g.dma.base) && crate::paging::user_accessible_in(pml4, g.dma.base + g.dma.len - 4096);
    let dma_past = crate::paging::user_accessible_in(pml4, g.dma.base + g.dma.len);
    let lapic = crate::paging::user_accessible_in(pml4, 0xFEE0_0000);
    let kernel_code = crate::paging::user_accessible_in(pml4, (selftest_grant as *const () as usize as u64) & !0xFFF);
    let ok = bar_first && bar_last && !past_bar && !before_bar && dma_in && !dma_past && !lapic && !kernel_code;
    crate::serial_println!(
        "[udrv-grant] e1000d pid {pid}: BAR0 {:#x}..{:#x} mapped for ring 3 (first={bar_first}, last={bar_last}), page before={before_bar}, page after={past_bar}; DMA window {} pages mapped={dma_in}, page after={dma_past}; LAPIC={lapic}, kernel code={kernel_code} → {}",
        g.mmio.base, g.mmio.base + g.mmio.len, g.dma.pages(),
        if ok { "OK (exactly the granted ranges) ✓" } else { "FAILED" }
    );
    let pages = crate::iommu::domain_pages(DOMAIN);
    if inst.iommu {
        crate::serial_println!(
            "[udrv-iommu] device {:02x}:{:02x}.{} has its own VT-d domain '{DOMAIN}' with {pages} page(s) = the DMA window, nothing else → {}",
            g.bdf.bus, g.bdf.dev, g.bdf.func,
            if pages == DMA_PAGES { "OK ✓" } else { "FAILED" }
        );
    } else {
        crate::serial_println!("[udrv-iommu] no IOMMU translation on this machine: e1000d's DMA is NOT confined (boot QEMU with -device intel-iommu,intremap=off) ⬜");
    }
}

fn selftest_irq(msix: bool) {
    let raised = irqs(Drv::E1000d);
    let delivered = irqs_delivered(Drv::E1000d);
    if !msix {
        crate::serial_println!("[udrv-irq] device without MSI-X: e1000d is serviced on its receive timeout (polled), no interrupt path to prove ⬜");
        return;
    }
    crate::serial_println!(
        "[udrv-irq] MSI-X vector 0x{:x}: {raised} interrupt(s) raised, {delivered} delivered to e1000d as EuroIPC Irq messages → {}",
        vector_of(Drv::E1000d).unwrap_or(0),
        if raised > 0 && delivered > 0 { "OK ✓" } else { "FAILED" }
    );
}

/// [udrv-dma]: the driver turns malicious on request and aims the device at
/// two kernel frames: an RX descriptor pointing at a canary (the device would
/// WRITE an incoming frame there) and a TX descriptor reading a secret page
/// (the device would READ it onto the wire). Under VT-d both must fault, the
/// canary must stay intact, and the driver must keep working afterwards.
fn selftest_dma(falloc: &mut FrameAllocator) {
    let iommu = SUP.lock().inst.as_ref().map(|i| i.iommu).unwrap_or(false);
    if !iommu {
        crate::serial_println!("[udrv-dma] no IOMMU translation: skipped (the escape would succeed; that is the unconfined case) ⬜");
        return;
    }
    let (Ok(canary), Ok(secret)) = (falloc.allocate(), falloc.allocate()) else {
        crate::serial_println!("[udrv-dma] no frames for the probe → FAILED");
        return;
    };
    // SAFETY: two fresh, identity-mapped frames of our own.
    unsafe {
        core::ptr::write_bytes(canary as *mut u8, 0xA5, 4096);
        core::ptr::write_bytes(secret as *mut u8, 0x5E, 4096);
    }
    let _ = crate::iommu::poll_faults(); // start from a clean fault log
    let faults_before = crate::iommu::faults_total();
    without_interrupts(|| *REPORT.lock() = (0, 0, 0));
    let mut b = [0u8; 32];
    let n = proto::encode(&Msg::DmaEscape { rx_target: canary, tx_target: secret }, &mut b).unwrap_or(0);
    euroipc::send(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n]);
    let reported = wait_for(500, falloc, || {
        pump_control();
        without_interrupts(|| REPORT.lock().0) == proto::REPORT_DMA_ESCAPE_DONE
    });
    let (_, slot, tx_res) = without_interrupts(|| *REPORT.lock());
    crate::serial_println!("[udrv-dma]   driver report received={reported} (slot {slot}); provoking inbound frames for the aimed slot");
    let _ = wait_for(20, falloc, || false);
    let tx_faults = crate::iommu::poll_faults();
    // Inbound frames for the aimed slot: ask the gateway for its MAC. Tick-based
    // waits, not the netstack's spin loops (those crawl with busy processes around).
    let first = arp_probe(falloc, 200);
    let _ = wait_for(50, falloc, || false);
    let rx_faults = crate::iommu::poll_faults();
    let faults = crate::iommu::faults_total() - faults_before;
    // SAFETY: our own frame.
    let changed = unsafe { core::slice::from_raw_parts(canary as *const u8, 4096) }.iter().any(|&x| x != 0xA5);
    // Still working afterwards: a fresh ARP round trip through the same driver.
    let still = arp_probe(falloc, 300);
    crate::serial_println!("[udrv-dma]   first ARP reply seen={first} (expected false: it went to the aimed slot)");
    let ok = reported && tx_faults >= 1 && rx_faults >= 1 && !changed && still;
    crate::serial_println!(
        "[udrv-dma] malicious e1000d: RX slot {slot} aimed at kernel canary {canary:#x}, TX from kernel page {secret:#x} (driver saw tx {}) · IOMMU fault records: TX read {tx_faults}, RX write {rx_faults} (total {faults}) · canary changed={changed} · driver still works={still} → {}",
        if tx_res == 0 { "done" } else { "error" },
        if ok { "OK (device DMA confined to the driver's own window) ✓" } else { "FAILED" }
    );
    let _ = falloc.free(canary);
    let _ = falloc.free(secret);
}

/// Send an ARP request for the gateway through the driver and wait (in ticks)
/// for the reply, reading the driver's frames directly. True when it came.
fn arp_probe(falloc: &mut FrameAllocator, ticks: u64) -> bool {
    use euronet::arp::{ArpOp, ArpPacket};
    use euronet::ethernet::{EtherType, EthernetHeader, MacAddr};
    let Some(c) = crate::net::get() else { return false };
    let req = ArpPacket { op: ArpOp::Request, sender_mac: c.my_mac, sender_ip: c.my_ip, target_mac: MacAddr::ZERO, target_ip: c.gw_ip };
    let frame = EthernetHeader { dst: MacAddr::BROADCAST, src: c.my_mac, ethertype: EtherType::Arp }.build(&req.build());
    if !crate::nic::send(&frame) {
        return false;
    }
    let end = crate::interrupts::ticks() + ticks;
    while crate::interrupts::ticks() < end {
        supervise(falloc);
        while let Some(rx) = take_stack() {
            if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                if h.ethertype == EtherType::Arp {
                    if let Ok(a) = ArpPacket::parse(p) {
                        if a.op == ArpOp::Reply && a.sender_ip == c.gw_ip {
                            return true;
                        }
                    }
                }
            }
        }
        crate::sched::yield_now();
        x86_64::instructions::hlt();
    }
    false
}

/// [udrv-crash]: a real ring-3 fault in the driver, a restart, the network back.
fn selftest_crash(falloc: &mut FrameAllocator, pid: u64) {
    let hellos = HELLOS.load(Ordering::Relaxed);
    let mut b = [0u8; 4];
    let n = proto::encode(&Msg::Crash, &mut b).unwrap_or(0);
    euroipc::send(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n]);
    // The death is noticed by supervise (inside wait_for), which quiesces and restarts.
    let died = wait_for(500, falloc, || !crate::ring3::bg_alive(pid));
    let link_was_down = SEND_REFUSED_WHILE_DEAD.load(Ordering::Relaxed);
    let back = wait_link(falloc, hellos + 1, 1500);
    let new_pid = DRIVER_PID.load(Ordering::Relaxed);
    if !died || !back || !link_was_down {
        crate::serial_println!("[udrv-crash] driver died={died}, sends refused while dead={link_was_down}, came back={back} → FAILED");
        return;
    }
    let (ok, lines) = net_roundtrip(falloc, 0x4555_0002, true);
    for l in &lines {
        crate::serial_println!("[udrv-crash]   {l}");
    }
    let reason = SUP.lock().last_death.clone().unwrap_or_default();
    crate::serial_println!(
        "[udrv-crash] e1000d pid {pid} killed by a real fault ({reason}); kernel kept running; sends refused while dead={link_was_down}; restarted as pid {new_pid}, MAC {}, DHCP + HTTP again → {}",
        mac_str(),
        if ok { "OK (driver crash = process exit, network back) ✓" } else { "FAILED" }
    );
}

/// One status line for `hwprobe`/the System window.
pub fn status_line() -> String {
    if !ACTIVE.load(Ordering::Relaxed) {
        return String::from("userspace drivers: none");
    }
    let sup = SUP.lock();
    alloc::format!(
        "userspace e1000d: {} (starts {}, deaths {}), link {}, tx {} rx {} irq {}",
        match sup.inst.as_ref() { Some(i) => alloc::format!("pid {} task {}", i.pid, i.task), None => String::from("not running") },
        sup.starts,
        sup.tracker.total_deaths(),
        if link_up() { "up" } else { "down" },
        TX_FRAMES.load(Ordering::Relaxed),
        RX_FRAMES.load(Ordering::Relaxed),
        irqs(Drv::E1000d)
    )
}
