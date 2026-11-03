//! Multi-core lock stress + throughput benchmark (J1 fine-grained locking).
//!
//! Runs the same workload on 1 core and on every core (BSP + the APs through
//! `kwork`), once with the J1 fine-grained locks and once with ONE global lock
//! around the very same operations (the pre-J1 shape: one mutex per subsystem).
//! Every operation is checked (read-back, sequence numbers, byte order), so a run
//! proves "no lost update, no torn data" as well as measuring throughput.
//!
//! Workloads (all in-kernel, all real kernel code paths):
//! - **fs**: EuroFS on RAM block devices, one mount per worker, behind a
//!   `SharedVfs` (per-mount locks); write 512 B + read back.
//! - **fs-1mount**: every worker on the same mount (shows that the per-mount
//!   lock still serialises one filesystem, as it must).
//! - **ipc**: one EuroIPC channel per worker (`euroipc::ChannelTable`); send +
//!   recv with a sequence number.
//! - **unix**: one AF_UNIX connection per worker (`SharedSwitchboard`); 64 B
//!   through the stream + read back.
//!
//! Honesty: QEMU without KVM runs multi-threaded TCG, so the cores really run in
//! parallel on host threads, but absolute numbers and ratios are TCG numbers,
//! not hardware numbers; under KVM they are virtualised numbers (the start
//! line says which, from CPUID). Memory allocation goes through the one global heap
//! lock in every variant; its contention is reported separately.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use eurofs::{EuroFs, FileSystem, MemoryBlockDevice, SharedVfs};
use eurolock::sync::Mutex;
use eurolock::{LockClass, LockSite, Mode};
use euroipc::ChannelTable;
use euronet::unix::SharedSwitchboard;

type BVfs = SharedVfs<dyn FileSystem + Send>;

/// The "one global lock" baseline. Class `Process`: ranked above every
/// subsystem lock it wraps, so wrapping is order-correct.
static SITE_GLOBAL_TASK: LockSite = LockSite::new("bench.global-lock (task)", LockClass::Process);
static SITE_GLOBAL_IRQ: LockSite = LockSite::new("bench.global-lock (irq)", LockClass::Process);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Fs,
    FsOneMount,
    Ipc,
    Unix,
}

impl Work {
    fn name(self) -> &'static str {
        match self {
            Work::Fs => "fs (mount per worker)",
            Work::FsOneMount => "fs (all on one mount)",
            Work::Ipc => "ipc (channel per worker)",
            Work::Unix => "unix (connection per worker)",
        }
    }
}

struct Bench {
    vfs: BVfs,
    ipc: ChannelTable,
    unix: SharedSwitchboard,
    pairs: Vec<(euronet::unix::Endpoint, euronet::unix::Endpoint)>,
    global_task: Mutex<()>,
    global_irq: Mutex<()>,
    ops: [AtomicU64; 8],
    errors: AtomicU64,
}

const MAXW: usize = 8;

fn tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// What the numbers were measured on, from CPUID (leaf 1 ECX bit 31 = running
/// under a hypervisor; leaf 0x4000_0000 = its vendor id). The `[lk-bench]`
/// line used to say "QEMU TCG" unconditionally, also under KVM
/// (LOADTEST-R630 BUG-013).
fn platform_label() -> String {
    let l1 = unsafe { core::arch::x86_64::__cpuid(1) };
    if l1.ecx & (1 << 31) == 0 {
        return String::from("no hypervisor bit in CPUID: bare metal (or an emulator that hides itself)");
    }
    let h = unsafe { core::arch::x86_64::__cpuid(0x4000_0000) };
    let mut v = [0u8; 12];
    v[0..4].copy_from_slice(&h.ebx.to_le_bytes());
    v[4..8].copy_from_slice(&h.ecx.to_le_bytes());
    v[8..12].copy_from_slice(&h.edx.to_le_bytes());
    let vendor: String = v.iter().take_while(|&&b| b != 0).map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect();
    match vendor.as_str() {
        "KVMKVMKVM" => String::from("KVM guest: hardware-virtualised vCPUs, NOT bare-metal numbers"),
        "TCGTCGTCGTCG" => String::from("QEMU TCG: parallel host threads, NOT hardware numbers"),
        _ => format!("hypervisor '{}': virtualised, NOT bare-metal numbers", vendor),
    }
}

fn tsc_per_sec() -> u64 {
    crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1) * 100
}

impl Bench {
    fn new() -> Bench {
        let vfs: BVfs = SharedVfs::new(Box::new(eurofs::RamDisk::new(1 << 20)));
        for w in 0..MAXW {
            let fs = EuroFs::format(MemoryBlockDevice::new(256, 4096), [0x4C; 16], 1).expect("bench EuroFS format");
            vfs.mount(&format!("/lk{w}"), Box::new(fs));
        }
        let ipc = ChannelTable::new();
        for w in 0..MAXW {
            ipc.register(9_000 + w as u64, 9_000 + w as u32);
        }
        let unix = SharedSwitchboard::new();
        unix.bind_listen("/run/lkbench.sock", 64).expect("bench unix bind");
        let pairs = (0..MAXW)
            .map(|_| {
                let c = unix.connect("/run/lkbench.sock").expect("bench connect");
                let s = unix.accept("/run/lkbench.sock").expect("bench accept");
                (c, s)
            })
            .collect();
        Bench {
            vfs,
            ipc,
            unix,
            pairs,
            global_task: Mutex::new_site(&SITE_GLOBAL_TASK, Mode::Task, ()),
            global_irq: Mutex::new_site(&SITE_GLOBAL_IRQ, Mode::Irq, ()),
            ops: [const { AtomicU64::new(0) }; 8],
            errors: AtomicU64::new(0),
        }
    }

    /// One operation of `work` by worker `w`, round `n`. Returns false on a
    /// correctness failure.
    fn op(&self, work: Work, global: bool, w: usize, n: u64) -> bool {
        match work {
            Work::Fs | Work::FsOneMount => {
                let mount = if work == Work::Fs { w } else { 0 };
                let path = format!("/lk{mount}/w{w}-{}", n % 8);
                let mut data = [0u8; 512];
                for (k, b) in data.iter_mut().enumerate() {
                    *b = (k as u64 ^ n ^ (w as u64) << 4) as u8;
                }
                let mut v = self.vfs.clone();
                let _g = if global { Some(self.global_task.lock()) } else { None };
                v.write_file(&path, &data).is_ok() && v.read_file(&path).map(|d| d[..] == data[..]).unwrap_or(false)
            }
            Work::Ipc => {
                let port = 9_000 + w as u32;
                let _g = if global { Some(self.global_irq.lock()) } else { None };
                if self.ipc.send(w as u64, port, &n.to_le_bytes()).is_err() {
                    return false;
                }
                match self.ipc.recv(port as u64) {
                    Ok((s, d)) => s == w as u64 && d[..] == n.to_le_bytes(),
                    Err(_) => false,
                }
            }
            Work::Unix => {
                let (c, s) = self.pairs[w];
                let mut msg = [0u8; 64];
                msg[..8].copy_from_slice(&n.to_le_bytes());
                msg[8] = w as u8;
                let _g = if global { Some(self.global_irq.lock()) } else { None };
                if self.unix.send(c, &msg).is_err() {
                    return false;
                }
                match self.unix.recv(s, 64) {
                    Ok(d) => d[..] == msg[..],
                    Err(_) => false,
                }
            }
        }
    }
}

/// One timed run: `n` workers for `ms` milliseconds. Returns (workers, ops/s,
/// total ops, errors).
fn run(b: &Arc<Bench>, work: Work, global: bool, n: usize, ms: u64) -> (usize, u64, u64, u64) {
    for o in b.ops.iter() {
        o.store(0, Ordering::Relaxed);
    }
    b.errors.store(0, Ordering::Relaxed);
    let span = tsc_per_sec() / 1000 * ms;
    let start = tsc() + tsc_per_sec() / 200; // 5 ms for the APs to pick the job up
    let deadline = start + span;
    let bb = b.clone();
    let workers = crate::kwork::parallel(
        n,
        Arc::new(move |w: usize| {
            while tsc() < start {
                core::hint::spin_loop();
            }
            let mut k = 0u64;
            while tsc() < deadline {
                if !bb.op(work, global, w, k) {
                    bb.errors.fetch_add(1, Ordering::Relaxed);
                }
                k += 1;
            }
            bb.ops[w].store(k, Ordering::Relaxed);
        }),
    );
    let total: u64 = b.ops.iter().map(|o| o.load(Ordering::Relaxed)).sum();
    let rate = total * 1000 / ms.max(1);
    (workers, rate, total, b.errors.load(Ordering::Relaxed))
}

fn heap_spins() -> (u64, u64) {
    let s = crate::klock::SITE_HEAP.stats.snapshot();
    (s.contended, s.spins)
}

/// The full benchmark (feature `lockbench`): every workload, fine vs global,
/// 1 worker vs all cores, `rounds` rounds. Prints `[lk-bench]` lines.
pub fn bench(rounds: usize, ms: u64) -> u64 {
    let b = Arc::new(Bench::new());
    let ncpu = 1 + crate::kwork::online_aps().len();
    crate::serial_println!(
        "[lk-bench] start: {} core(s) online, {} round(s) x {} ms per run, TSC {} MHz ({})",
        ncpu,
        rounds,
        ms,
        tsc_per_sec() / 1_000_000,
        platform_label()
    );
    if ncpu > 1 && !crate::kwork::accepting() {
        // `kwork::parallel` would then run every worker inline on the BSP, one
        // after the other: the "N cores" figures would not be parallel at all.
        crate::serial_println!("[lk-bench] WARNING: APs online but kwork does not accept jobs: multi-core figures below are NOT parallel");
    }
    let mut errors = 0u64;
    for work in [Work::Fs, Work::FsOneMount, Work::Ipc, Work::Unix] {
        for global in [false, true] {
            if work == Work::FsOneMount && global {
                continue; // one mount: the per-mount lock IS the global lock
            }
            let mut ones = Vec::new();
            let mut alls = Vec::new();
            for _ in 0..rounds {
                let (_, r1, _, e1) = run(&b, work, global, 1, ms);
                let before = crate::klock::snapshot_all();
                let (h0, hs0) = heap_spins();
                let (wn, rn, _, en) = run(&b, work, global, ncpu, ms);
                let (h1, hs1) = heap_spins();
                errors += e1 + en;
                ones.push(r1);
                alls.push((wn, rn, h1 - h0, hs1 - hs0, before));
            }
            ones.sort_unstable();
            let mut rs: Vec<u64> = alls.iter().map(|a| a.1).collect();
            rs.sort_unstable();
            let med1 = ones[ones.len() / 2];
            let medn = rs[rs.len() / 2];
            let wn = alls[0].0;
            let (hc, hsp) = (alls.iter().map(|a| a.2).sum::<u64>(), alls.iter().map(|a| a.3).sum::<u64>());
            crate::serial_println!(
                "[lk-bench] {:<30} {:<6} 1 core: {:>8} ops/s | {} cores: {:>8} ops/s | scaling x{}.{:02} | heap contended {} (spins {})",
                work.name(),
                if global { "GLOBAL" } else { "J1" },
                med1,
                wn,
                medn,
                medn / med1.max(1),
                (medn * 100 / med1.max(1)) % 100,
                hc,
                hsp
            );
            // Which locks were contended in the last multi-core round.
            if let Some(last) = alls.last() {
                crate::klock::report_delta(&format!("  {} {}:", work.name(), if global { "GLOBAL" } else { "J1" }), &last.4, 4);
            }
        }
    }
    crate::serial_println!(
        "[lk-bench] done: correctness errors={} order-inversions={} task-locks-with-IF0={} -> {}",
        errors,
        crate::klock::inversions(),
        crate::klock::if0_violations(),
        if errors == 0 && crate::klock::inversions() == 0 { "OK ✓" } else { "FAILED" }
    );
    errors
}

/// `[lk-stress]` boot self-test: every workload with the J1 locks on all cores
/// at once (a mixed load: worker w runs workload w % 4), `rounds` times, plus
/// the root VFS read from every core. Checks: no correctness error, no order
/// inversion, no task lock under IF=0, every round finishes (no deadlock).
pub fn selftest(rounds: usize, ms: u64) {
    let b = Arc::new(Bench::new());
    let ncpu = 1 + crate::kwork::online_aps().len();
    let mut total_ops = 0u64;
    let mut errors = 0u64;
    let t0 = crate::interrupts::ticks();
    for r in 0..rounds {
        let span = tsc_per_sec() / 1000 * ms;
        let start = tsc() + tsc_per_sec() / 200;
        let deadline = start + span;
        let bb = b.clone();
        let root_ok = Arc::new(AtomicU64::new(0));
        let ro = root_ok.clone();
        for o in b.ops.iter() {
            o.store(0, Ordering::Relaxed);
        }
        crate::kwork::parallel(
            ncpu,
            Arc::new(move |w: usize| {
                // The real root filesystem, from this core, through the shared VFS.
                if let Some(v) = crate::kvfs::root() {
                    if v.read_file("/etc/os-release").map(|d| d.starts_with(b"NAME=") || !d.is_empty()).unwrap_or(false) {
                        ro.fetch_add(1, Ordering::Relaxed);
                    }
                }
                while tsc() < start {
                    core::hint::spin_loop();
                }
                let work = [Work::Fs, Work::Ipc, Work::Unix, Work::FsOneMount][(w + r) % 4];
                let mut k = 0u64;
                while tsc() < deadline {
                    if !bb.op(work, false, w, k) {
                        bb.errors.fetch_add(1, Ordering::Relaxed);
                    }
                    k += 1;
                }
                bb.ops[w].store(k, Ordering::Relaxed);
            }),
        );
        total_ops += b.ops.iter().map(|o| o.load(Ordering::Relaxed)).sum::<u64>();
        errors += b.errors.load(Ordering::Relaxed);
        if crate::kvfs::root().is_some() && root_ok.load(Ordering::Relaxed) as usize != ncpu {
            errors += 1;
        }
    }
    let secs10 = (crate::interrupts::ticks() - t0) / 10;
    let inv = crate::klock::inversions();
    let if0 = crate::klock::if0_violations();
    let ok = errors == 0 && inv == 0 && if0 == 0;
    crate::serial_println!(
        "[lk-stress] {} round(s) x {} core(s), mixed fs/ipc/unix + root VFS from every core: {} ops, correctness errors={}, order inversions={}, task-lock-with-IF0={}, all rounds completed in {}.{} s (no deadlock) -> {}",
        rounds,
        ncpu,
        total_ops,
        errors,
        inv,
        if0,
        secs10 / 10,
        secs10 % 10,
        if ok { "OK ✓" } else { "FAILED" }
    );
    crate::klock::report("after-stress", 8);
}

/// Shell: `lockbench [rounds] [ms]`.
pub fn shell(args: &str) -> Vec<String> {
    let mut it = args.split_whitespace();
    let rounds = it.next().and_then(|x| x.parse().ok()).unwrap_or(1usize).clamp(1, 5);
    let ms = it.next().and_then(|x| x.parse().ok()).unwrap_or(300u64).clamp(50, 3000);
    let e = bench(rounds, ms);
    alloc::vec![format!("lockbench finished (errors={e}); results on the serial console ([lk-bench])")]
}
