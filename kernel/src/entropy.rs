//! 3D-8 — the kernel early-boot CSPRNG. Gathers real noise (CPU timing **jitter**,
//! RDSEED and RDRAND when the CPU has them, the TPM RNG when there is one),
//! seeds an HMAC-DRBG ([`euroentropy`]), and hands out randomness **only** once
//! a security threshold of real entropy is reached. This closes the "weak
//! randomness at the worst moment" gap for FDE, TPM sealing, ML-KEM key
//! generation, the VPN and EuroCA — all of which draw key material very early.
//!
//! Seeding (BUG-065): [`init`] gathers until the pool has its 256 bits, with a
//! bound ([`SeedPolicy::BOOT`]); the sources are mixed, none replaces another;
//! the result is the `[entropy]` line every image prints. A pool that did not
//! get there stays closed: [`getrandom`] and [`draw`] refuse (after one more
//! bounded attempt to seed), and every caller has to deal with that refusal.
//!
//! One seeding, early (BUG-080, BUG-081): `main.rs` calls [`init`] as soon as
//! there is a heap and a clock, before the identity store, the network and
//! the first process. A request that comes before that call does the same
//! seeding itself and says so; either way there is one `[entropy]` status
//! line. The pool is taken with interrupts off, so it can be asked from any
//! context (a port is chosen with interrupts off, a SYN is answered from
//! the receive path). Callers that cannot refuse (the `AT_RANDOM` bytes of a
//! process start, a TCP sequence number, a salt) use [`fill_wait`] and
//! [`draw_wait`]: they keep gathering noise until the pool opens and never
//! return other bytes.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use euroentropy::{estimate_jitter_bits, EntropyPool, NoiseSources, SeedPolicy, SeedReport};
use spin::Mutex;

static POOL: Mutex<Option<EntropyPool>> = Mutex::new(None);
/// Requests refused because the pool was not seeded.
static REFUSED: AtomicU64 = AtomicU64::new(0);
/// Timer tick of the last seeding attempt made on behalf of a request.
static LAST_RETRY_TICK: AtomicU64 = AtomicU64::new(0);
/// [`init`] has been entered: there is one boot seeding.
static INIT_DONE: AtomicBool = AtomicBool::new(false);
/// A seeding is gathering noise right now (the pool is out of [`POOL`]).
static SEEDING: AtomicBool = AtomicBool::new(false);
/// Kernel callers that had to wait for the seed ([`fill_wait`]).
static WAITED: AtomicU64 = AtomicU64::new(0);

/// The pool, with interrupts off while it is held. The holder is never
/// parked by the timer and an interrupt handler never finds its own core
/// holding the lock; what runs under it is short (a few HMACs).
fn with_pool<R>(f: impl FnOnce(&mut Option<EntropyPool>) -> R) -> R {
    let _if_off = crate::sched::IfOffGuard::new();
    f(&mut POOL.lock())
}

#[inline]
fn rdtsc() -> u64 {
    // SAFETY: RDTSC is always available on x86-64 and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// The block the jitter sample walks: larger than a core's first- and
/// second-level cache, so where an access is served from differs from access
/// to access.
const WALK_BYTES: usize = 512 * 1024;
/// Memory accesses between the two timestamps of one sample.
const WALK_STEPS: usize = 32;

/// The noise sources of this machine.
struct Platform {
    walk: Vec<u8>,
    acc: u64,
    rdseed: bool,
    rdrand: bool,
    tpm: bool,
}

impl Platform {
    /// Every source the machine has.
    fn all() -> Self {
        let (rdrand, rdseed) = cpu_rng_features();
        Platform { walk: alloc::vec![0u8; WALK_BYTES], acc: rdtsc() | 1, rdseed, rdrand, tpm: true }
    }
    /// The jitter source alone (the self-test's "no device, no CPU generator").
    fn jitter_only() -> Self {
        Platform { walk: alloc::vec![0u8; WALK_BYTES], acc: rdtsc() | 1, rdseed: false, rdrand: false, tpm: false }
    }
}

/// (RDRAND, RDSEED) as CPUID reports them. Without the bit the instruction is
/// an invalid opcode (qemu64 under TCG, `-cpu …,-rdrand,-rdseed`).
fn cpu_rng_features() -> (bool, bool) {
    // SAFETY: CPUID is available on every x86-64 CPU.
    unsafe {
        let max = core::arch::x86_64::__cpuid(0).eax;
        let rdrand = core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0;
        let rdseed = max >= 7 && core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 18) != 0;
        (rdrand, rdseed)
    }
}

impl NoiseSources for Platform {
    fn tpm(&mut self) -> Option<Vec<u8>> {
        if self.tpm {
            crate::tpm::get_random(32)
        } else {
            None
        }
    }

    fn rdseed(&mut self) -> Option<u64> {
        if !self.rdseed {
            return None;
        }
        // RDSEED may have nothing ready (CF = 0): try again a few times.
        for _ in 0..64 {
            let v: u64;
            let ok: u8;
            // SAFETY: the CPU reports RDSEED (checked in `cpu_rng_features`).
            unsafe {
                core::arch::asm!("rdseed {0}", "setc {1}", out(reg) v, out(reg_byte) ok, options(nomem, nostack));
            }
            if ok == 1 {
                return Some(v);
            }
            core::hint::spin_loop();
        }
        None
    }

    fn rdrand(&mut self) -> Option<u64> {
        if !self.rdrand {
            return None;
        }
        for _ in 0..16 {
            let v: u64;
            let ok: u8;
            // SAFETY: the CPU reports RDRAND (checked in `cpu_rng_features`).
            unsafe {
                core::arch::asm!("rdrand {0}", "setc {1}", out(reg) v, out(reg_byte) ok, options(nomem, nostack));
            }
            if ok == 1 {
                return Some(v);
            }
        }
        None
    }

    /// `n` timing samples: the TSC around [`WALK_STEPS`] read-modify-write
    /// accesses at pseudo-random places of a block that does not fit the
    /// near caches. How long that takes depends on where each line is at that
    /// moment (L1, L2, L3, the TLB), on the prefetchers and on what else the
    /// core and its sibling are doing. The duration of each sample is fed
    /// back into the walk, so the places depend on the timing before.
    ///
    /// Before BUG-065 a sample timed one multiplication between two RDTSCs:
    /// about 20 cycles. What varied there was the beat between the core's
    /// clock and the TSC's: a short repeating pattern in most P-states, and a
    /// constant in the P-states where the span is a whole number of TSC ticks
    /// (measured on the R630: 24, 24, 24, … for whole batches). The estimator
    /// rightly credited such batches nothing, and the old `init` then gave up.
    fn jitter(&mut self, n: usize) -> Vec<u64> {
        let mut s = Vec::with_capacity(n);
        let mask = self.walk.len() - 1; // WALK_BYTES is a power of two
        for _ in 0..n {
            let t0 = rdtsc();
            for _ in 0..WALK_STEPS {
                self.acc = self.acc.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let i = ((self.acc >> 33) as usize) & mask;
                self.walk[i] = self.walk[i].wrapping_add(self.acc as u8);
            }
            core::hint::black_box(&self.walk);
            let d = rdtsc().wrapping_sub(t0);
            self.acc ^= d;
            s.push(d);
        }
        s
    }
}

fn now_us() -> u64 {
    if crate::hpet::present() {
        crate::hpet::us()
    } else {
        crate::interrupts::ticks() * 10_000
    }
}

fn status_line(r: &SeedReport, us: u64) -> alloc::string::String {
    let tpm = if r.tpm_bytes > 0 { alloc::format!("{} bytes", r.tpm_bytes) } else { alloc::string::String::from("none") };
    let cpu = |have: bool, words: usize| -> alloc::string::String {
        if !have {
            alloc::string::String::from("not in this CPU")
        } else {
            alloc::format!("{words} words")
        }
    };
    let (have_rdrand, have_rdseed) = cpu_rng_features();
    let sources = alloc::format!(
        "tpm={tpm}, rdseed={}, rdrand={} (CPU credit {} bits, at most half the threshold), jitter={} batches of 512 samples ({} bits)",
        cpu(have_rdseed, r.rdseed_words),
        cpu(have_rdrand, r.rdrand_words),
        r.cpu_bits,
        r.jitter_batches,
        r.jitter_bits
    );
    if r.ready {
        alloc::format!(
            "[entropy] CSPRNG seeded: {} of {} bits · sources mixed: {sources} · {}.{:03} ms → OK",
            r.bits,
            r.threshold,
            us / 1000,
            us % 1000
        )
    } else {
        alloc::format!(
            "[entropy] CSPRNG NOT SEEDED: {} of {} bits · sources: {sources} · {}.{:03} ms → FAILED (keys and nonces are refused; seeding is tried again when one is asked for)",
            r.bits,
            r.threshold,
            us / 1000,
            us % 1000
        )
    }
}

/// Seed the global pool: TPM, RDSEED, RDRAND, jitter, mixed, until the
/// threshold is met or the bound of [`SeedPolicy::BOOT`] is reached. Prints
/// the `[entropy]` status line (every image). The first call does the work;
/// a later call returns at once, so there is one seeding and one line.
pub fn init() {
    if INIT_DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    SEEDING.store(true, Ordering::SeqCst);
    let t0 = now_us();
    let mut pool = EntropyPool::new(256);
    let mut plat = Platform::all();
    let report = pool.seed_from(&mut plat, &SeedPolicy::BOOT);
    with_pool(|p| *p = Some(pool));
    SEEDING.store(false, Ordering::SeqCst);
    LAST_RETRY_TICK.store(crate::interrupts::ticks(), Ordering::Relaxed);
    crate::serial_println!("{}", status_line(&report, now_us().saturating_sub(t0)));
    report_health(0, &report);
    restore_watch_init();
}

/// Whether the pool has gathered enough real entropy to hand out randomness.
pub fn ready() -> bool {
    with_pool(|p| p.as_ref().map(|p| p.ready()).unwrap_or(false))
}

/// What the seeding gathered so far (`None` before [`init`]).
pub fn report() -> Option<SeedReport> {
    with_pool(|p| p.as_ref().map(|p| p.seed_report()))
}

/// Requests refused so far because the pool was not seeded.
pub fn refused() -> u64 {
    REFUSED.load(Ordering::Relaxed)
}

/// One more bounded attempt to seed a pool that is not ready. On behalf of a
/// request that can be refused: at most once a second (the request waits for
/// it; without noise it costs a few milliseconds and changes nothing). With
/// `force`, for a caller that waits: now. The noise is gathered with the pool
/// out of its lock; a request that comes meanwhile finds no pool and is
/// refused or waits, as it would have been.
fn seed_again(force: bool) {
    let now = crate::interrupts::ticks();
    if !force {
        let last = LAST_RETRY_TICK.load(Ordering::Relaxed);
        if now.saturating_sub(last) < 100 && last != 0 {
            return;
        }
    }
    if SEEDING.swap(true, Ordering::SeqCst) {
        return; // one seeding at a time
    }
    LAST_RETRY_TICK.store(now.max(1), Ordering::Relaxed);
    // Only a pool that is not ready leaves the lock.
    let taken = with_pool(|p| match p {
        Some(pool) if pool.ready() => None,
        _ => Some(p.take()),
    });
    if let Some(taken) = taken {
        let mut pool = taken.unwrap_or_else(|| EntropyPool::new(256));
        let mut plat = Platform::all();
        let t0 = now_us();
        let failures = pool.seed_report().health_failures;
        let report = pool.seed_from(&mut plat, &SeedPolicy::RETRY);
        with_pool(|p| *p = Some(pool));
        report_health(failures, &report);
        if report.ready {
            crate::serial_println!("{}", status_line(&report, now_us().saturating_sub(t0)));
        }
    }
    SEEDING.store(false, Ordering::SeqCst);
}

/// The boot seeding, for a request that comes before `main.rs` asked for it.
/// Not expected: it is reported, so an ordering mistake in the boot shows.
fn init_on_demand() {
    if !INIT_DONE.load(Ordering::SeqCst) {
        crate::serial_println!("[entropy] random bytes were asked for before the boot seeded the CSPRNG: seeding now");
        init();
    }
}

fn try_fill(buf: &mut [u8]) -> bool {
    // BUG-089: a restored or cloned guest reseeds before it hands out a byte.
    restore_check_fast();
    if PENDING.load(Ordering::Relaxed) != 0 {
        reseed_pending();
    }
    with_pool(|p| p.as_mut().map(|p| p.fill(buf)).unwrap_or(false))
}

/// Fill `buf` with CSPRNG output. Returns `false` (and writes nothing) if the
/// pool is not seeded with enough entropy — the hard blocking guarantee. A
/// request that finds the pool closed first waits for one more seeding
/// attempt. The caller must act on `false`: the buffer is as it was.
#[must_use = "false means the CSPRNG is not seeded and the buffer was not written"]
pub fn getrandom(buf: &mut [u8]) -> bool {
    init_on_demand();
    if try_fill(buf) {
        return true;
    }
    seed_again(false);
    if try_fill(buf) {
        return true;
    }
    if REFUSED.fetch_add(1, Ordering::Relaxed) == 0 {
        crate::serial_println!("[entropy] a request for random bytes was refused: the CSPRNG is not seeded");
    }
    false
}

/// One seeding attempt now if the pool is closed; whether it is open after.
/// For a waiter that gives the CPU away between attempts (the `getrandom`
/// syscall without `GRND_NONBLOCK`).
pub fn seed_step() -> bool {
    init_on_demand();
    if ready() {
        return true;
    }
    seed_again(true);
    ready()
}

/// Fill `buf` with CSPRNG output, waiting for the seed if need be: the form
/// for kernel callers that can neither refuse nor go on without the bytes
/// (the `AT_RANDOM` bytes and the layout of a new process, a TCP sequence
/// number, a port, a salt, a session id). Waiting is work: each round gathers
/// more noise into the pool (64 batches), with or without a running clock or
/// scheduler. It never returns bytes from anything but the seeded generator.
pub fn fill_wait(buf: &mut [u8]) {
    init_on_demand();
    let mut waiting = false;
    while !try_fill(buf) {
        if !waiting {
            waiting = true;
            if WAITED.fetch_add(1, Ordering::Relaxed) == 0 {
                crate::serial_println!("[entropy] a kernel caller waits for the CSPRNG to be seeded (gathering more noise)");
            }
        }
        seed_again(true);
        core::hint::spin_loop();
    }
}

/// `N` bytes from the CSPRNG, waiting for the seed if need be ([`fill_wait`]).
pub fn draw_wait<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    fill_wait(&mut b);
    b
}

/// `N` bytes from the CSPRNG, or `None` when it is not seeded. The form for
/// keys, seeds and nonces: there is no buffer left to use by mistake.
#[must_use]
pub fn draw<const N: usize>() -> Option<[u8; N]> {
    let mut b = [0u8; N];
    if getrandom(&mut b) {
        Some(b)
    } else {
        None
    }
}

/// `[3d8]` boot self-test: prove the entropy gate refuses before it is seeded,
/// that CPU jitter alone reaches the threshold (works with no TPM and no CPU
/// generator), and that the seeded pool yields non-zero, non-repeating output.
pub fn selftest() {
    // (1) A fresh pool refuses to hand out randomness and writes nothing.
    let mut probe = EntropyPool::new(256);
    let empty_refused = {
        let mut b = [0xA7u8; 16];
        !probe.fill(&mut b) && b == [0xA7u8; 16]
    };
    // (2) CPU jitter ALONE can reach readiness (no device dependency), in the
    // 16 batches this test has always allowed.
    let mut jbits = 0usize;
    {
        let mut plat = Platform::jitter_only();
        for _ in 0..16 {
            let s = plat.jitter(512);
            jbits += estimate_jitter_bits(&s);
            probe.add_jitter(&s);
        }
    }
    let jitter_ready = probe.ready();

    // (3) The real global pool (all sources) hands out distinct, non-zero bytes.
    // It was seeded early in the boot; `init` here is a no-op unless it was not.
    init();
    let from_tpm = crate::tpm::present();
    let is_ready = ready();
    let mixed = report().map(|r| r.jitter_batches >= SeedPolicy::BOOT.min_jitter_batches && r.jitter_bits > 0).unwrap_or(false);
    let a = draw::<32>();
    let b = draw::<32>();
    let distinct = match (a, b) {
        (Some(a), Some(b)) => a != b && a.iter().any(|&x| x != 0),
        _ => false,
    };

    health_selftest();
    let ok = empty_refused && jitter_ready && is_ready && mixed && distinct;
    crate::serial_println!(
        "[3d8] EuroEntropy CSPRNG (HMAC-DRBG SP800-90A, NIST-KAT-verified): empty-pool-refused={empty_refused}, jitter-alone≈{jbits}bits→ready={jitter_ready}, seeded(jitter+TPM,from-tpm={from_tpm})-ready={is_ready}, jitter-in-the-seed={mixed}, output-nonzero-distinct={distinct} → {}",
        if ok { "OK (getrandom gated on real entropy — no low-entropy output at the worst moment) ✓" } else { "FAILED" }
    );
}

// ── BUG-089: a reseed after a restore, in a clone, and from time to time ───
//
// The DRBG state is in RAM: two guests resumed from one snapshot, or two
// clones of one VM, continue the same state and hand out the same bytes until
// something reseeds them. Three triggers:
// - the VM generation ID (ACPI `VM_Gen_Counter`): the hypervisor gives it a
//   new value in a restored or cloned guest. It is read before every request
//   (16 bytes of memory), so the first bytes after a restore are new ones;
// - the wall clock against the HPET: both run in a guest, but after a restore
//   the HPET continues from the snapshot and the RTC (the host's clock) does
//   not. Looked at once a second from the desktop loop (task context: the CMOS
//   index port is not shared with an interrupt), a jump of more than
//   CLOCK_JUMP_S marks the pool for a reseed at the next request;
// - time: every RESEED_PERIOD_US, as recovery from a state that leaked.
// A reseed mixes fresh noise (RDSEED, RDRAND, a jitter batch; health-tested)
// and what tells this instance apart (the generation ID, the TSC, the HPET).

const CLOCK_JUMP_S: i64 = 10;
const RESEED_PERIOD_US: u64 = 300_000_000;

/// Physical address of the VM generation ID, 0 = no such device.
static VMGENID: AtomicU64 = AtomicU64::new(0);
static VMGENID_SEEN: Mutex<[u8; 16]> = Mutex::new([0; 16]);
/// Why the next request reseeds first: 0 none, else a [`Why`] as u8.
static PENDING: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
static LAST_RESEED_US: AtomicU64 = AtomicU64::new(0);
static CLOCK_CHECK_US: AtomicU64 = AtomicU64::new(0);
/// RTC seconds minus HPET seconds at the last clock check (i64::MIN: none yet).
static CLOCK_OFFSET: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(i64::MIN);
static RESEEDS: AtomicU64 = AtomicU64::new(0);
static RESTORES: AtomicU64 = AtomicU64::new(0);
/// The noise sources of a reseed, made at `init` (a reseed may come in any
/// context: no allocation then). No TPM: it was read once per pool.
static RESEED_PLAT: Mutex<Option<Platform>> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Why {
    VmGenId = 1,
    ClockJump = 2,
    Period = 3,
    SelfTest = 4,
}

impl Why {
    fn from(v: u8) -> Option<Why> {
        match v {
            1 => Some(Why::VmGenId),
            2 => Some(Why::ClockJump),
            3 => Some(Why::Period),
            4 => Some(Why::SelfTest),
            _ => None,
        }
    }
    fn text(self) -> &'static str {
        match self {
            Why::VmGenId => "the VM generation ID changed (restored from a snapshot, or a clone)",
            Why::ClockJump => "the wall clock jumped against the HPET (restored from a snapshot, or resumed)",
            Why::Period => "periodic",
            Why::SelfTest => "self-test",
        }
    }
}

fn vmgenid_read(addr: u64) -> [u8; 16] {
    let mut g = [0u8; 16];
    for (i, b) in g.iter_mut().enumerate() {
        // SAFETY: firmware-reserved RAM below 4 GiB named by the ACPI tables,
        // identity-mapped like the tables themselves; a byte read.
        *b = unsafe { core::ptr::read_volatile((addr + i as u64) as *const u8) };
    }
    g
}

/// Look for the VM generation ID and remember its value (from `init`).
fn restore_watch_init() {
    *RESEED_PLAT.lock() = Some(Platform { tpm: false, ..Platform::all() });
    LAST_RESEED_US.store(now_us(), Ordering::Relaxed);
    if let Some(a) = crate::acpi::vmgenid_addr().filter(|&a| a < 1 << 32) {
        let g = vmgenid_read(a);
        *VMGENID_SEEN.lock() = g;
        VMGENID.store(a, Ordering::Relaxed);
        crate::serial_println!(
            "[entropy] VM generation ID at {a:#x} ({:02x}{:02x}{:02x}{:02x}…): a restore or a clone reseeds the CSPRNG",
            g[0], g[1], g[2], g[3]
        );
    }
}

/// Before every request: has the generation ID changed, is a reseed due?
fn restore_check_fast() {
    let a = VMGENID.load(Ordering::Relaxed);
    if a != 0 {
        let g = vmgenid_read(a);
        let _if_off = crate::sched::IfOffGuard::new();
        let mut seen = VMGENID_SEEN.lock();
        if g != *seen {
            *seen = g;
            PENDING.store(Why::VmGenId as u8, Ordering::SeqCst);
        }
    }
    if crate::hpet::present() && PENDING.load(Ordering::Relaxed) == 0 {
        let last = LAST_RESEED_US.load(Ordering::Relaxed);
        if last != 0 && now_us().saturating_sub(last) >= RESEED_PERIOD_US {
            let _ = PENDING.compare_exchange(0, Why::Period as u8, Ordering::SeqCst, Ordering::Relaxed);
        }
    }
}

/// From the desktop loop (task context), at most once a second: the wall
/// clock against the HPET.
pub fn poll_clock() {
    if !crate::hpet::present() || !INIT_DONE.load(Ordering::Relaxed) {
        return;
    }
    let now = now_us();
    let last = CLOCK_CHECK_US.load(Ordering::Relaxed);
    if now.saturating_sub(last) < 1_000_000 && last != 0 {
        return;
    }
    CLOCK_CHECK_US.store(now.max(1), Ordering::Relaxed);
    let Some(e) = crate::rtc::epoch_nowait() else { return };
    let off = e as i64 - (now / 1_000_000) as i64;
    let prev = CLOCK_OFFSET.swap(off, Ordering::Relaxed);
    if prev != i64::MIN && (off - prev).abs() > CLOCK_JUMP_S {
        let _ = PENDING.compare_exchange(0, Why::ClockJump as u8, Ordering::SeqCst, Ordering::Relaxed);
    }
}

/// (reseeds, of them after a restore or in a clone, a generation ID is watched).
pub fn reseed_stats() -> (u64, u64, bool) {
    (RESEEDS.load(Ordering::Relaxed), RESTORES.load(Ordering::Relaxed), VMGENID.load(Ordering::Relaxed) != 0)
}

/// BUG-082: one line for every new health test failure in `r` (there were
/// `before` failures). The pool itself switched the source off or discarded
/// the batch; this is the report, never a panic.
fn report_health(before: usize, r: &SeedReport) {
    if r.health_failures <= before {
        return;
    }
    if let Some((src, f)) = r.last_failure {
        let what = match src {
            euroentropy::HealthSource::Jitter => "batch discarded (not mixed, not credited); the next batch is tested afresh",
            _ => "source disabled for this boot",
        };
        crate::serial_println!(
            "[entropy] health test FAILED on {}: {} ({} failure(s) in all, {} jitter batch(es) discarded) → {what}",
            src.name(),
            f.describe(),
            r.health_failures,
            r.jitter_discarded
        );
    }
}

/// A reseed that is pending: fresh noise into the open pool, before the
/// request that found it is served.
fn reseed_pending() {
    let why = PENDING.swap(0, Ordering::SeqCst);
    let Some(why) = Why::from(why) else { return };
    let _if_off = crate::sched::IfOffGuard::new();
    let mut plat = RESEED_PLAT.lock();
    let Some(plat) = plat.as_mut() else { return };
    let mut extra = [0u8; 48];
    extra[0] = why as u8;
    let a = VMGENID.load(Ordering::Relaxed);
    if a != 0 {
        extra[1..17].copy_from_slice(&vmgenid_read(a));
    }
    extra[17..25].copy_from_slice(&rdtsc().to_le_bytes());
    if crate::hpet::present() {
        extra[25..33].copy_from_slice(&crate::hpet::counter().to_le_bytes());
    }
    extra[33..41].copy_from_slice(&RESEEDS.load(Ordering::Relaxed).to_le_bytes());
    let t0 = now_us();
    let done = with_pool(|p| match p {
        Some(pool) if pool.ready() => {
            let before = pool.seed_report();
            let r = pool.reseed_from(plat, 1, &extra);
            Some((before, r))
        }
        _ => None,
    });
    LAST_RESEED_US.store(now_us().max(1), Ordering::Relaxed);
    let Some((before, r)) = done else { return };
    RESEEDS.fetch_add(1, Ordering::Relaxed);
    report_health(before.health_failures, &r);
    if why == Why::Period {
        return;
    }
    if why != Why::SelfTest {
        RESTORES.fetch_add(1, Ordering::Relaxed);
    }
    let us = now_us().saturating_sub(t0);
    crate::serial_println!(
        "[entropy] reseeded: {} · fresh noise: rdseed {} words, rdrand {} words, jitter {} batch ({} bits), generation id and clocks mixed in · {}.{:03} ms → OK",
        why.text(),
        r.rdseed_words - before.rdseed_words,
        r.rdrand_words - before.rdrand_words,
        r.jitter_batches - before.jitter_batches,
        r.jitter_bits - before.jitter_bits,
        us / 1000,
        us % 1000
    );
}

/// `[reseed]` boot self-test: a reseed mixes fresh noise into the open pool
/// and the stream goes on from another state.
pub fn reseed_selftest() {
    let (n0, _, watched) = reseed_stats();
    let rs0 = with_pool(|p| p.as_ref().map(|p| p.reseed_count()).unwrap_or(0));
    PENDING.store(Why::SelfTest as u8, Ordering::SeqCst);
    let a = draw::<16>();
    let (n1, _, _) = reseed_stats();
    let rs1 = with_pool(|p| p.as_ref().map(|p| p.reseed_count()).unwrap_or(0));
    let ok = a.is_some() && n1 == n0 + 1 && rs1 > rs0;
    crate::serial_println!(
        "[reseed] a requested reseed mixed fresh noise into the open CSPRNG before the next request (reseeds {n0}→{n1}, DRBG reseeds {rs0}→{rs1}); VM generation ID {} → {}",
        if watched { "watched" } else { "not present (no restore detection by ID; the clock check and the 5-minute reseed remain)" },
        if ok { "OK ✓" } else { "FAILED" }
    );
}

/// `[entropy-health]` (BUG-082): the SP 800-90B health tests in the seeding
/// of this kernel. A platform whose RDRAND is stuck at all ones and whose
/// first two jitter batches are a clock beat: the words are not used and the
/// generator is switched off, the two batches are discarded, the real jitter
/// after them still seeds the pool. And the boot seeding of this machine had
/// no failure.
fn health_selftest() {
    struct Broken {
        real: Platform,
        batches: usize,
    }
    impl NoiseSources for Broken {
        fn rdrand(&mut self) -> Option<u64> {
            Some(u64::MAX)
        }
        fn jitter(&mut self, n: usize) -> Vec<u64> {
            self.batches += 1;
            if self.batches <= 2 {
                let pat = [12u64, 12, 12, 12, 38, 12, 12, 12, 14, 38];
                (0..n).map(|i| pat[i % pat.len()]).collect()
            } else {
                self.real.jitter(n)
            }
        }
    }
    let mut src = Broken { real: Platform::jitter_only(), batches: 0 };
    let mut pool = EntropyPool::new(256);
    let r = pool.seed_from(&mut src, &SeedPolicy::BOOT);
    let caught = r.rdrand_off && r.rdrand_words == 0 && r.jitter_discarded == 2 && r.health_failures == 3;
    let boot = report();
    let boot_clean = boot.map(|b| b.health_failures == 0).unwrap_or(false);
    let ok = caught && r.ready && boot_clean;
    crate::serial_println!(
        "[entropy-health] SP 800-90B repetition count + adaptive proportion on the raw samples: stuck RDRAND switched off={} (words used {}), clock-beat jitter batches discarded {}/2, pool still seeded from the rest={}; boot seeding of this machine: {} health failure(s), {} batch(es) discarded → {}",
        r.rdrand_off,
        r.rdrand_words,
        r.jitter_discarded,
        r.ready,
        boot.map(|b| b.health_failures).unwrap_or(usize::MAX),
        boot.map(|b| b.jitter_discarded).unwrap_or(usize::MAX),
        if ok { "OK ✓" } else { "FAILED" }
    );
}
