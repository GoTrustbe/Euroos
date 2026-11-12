//! `[linux-rand]` — what a Linux-ABI program gets when it asks the kernel for
//! random bytes (BUG-080): the `getrandom` syscall, `/dev/urandom`,
//! `/dev/random` and the 16 `AT_RANDOM` bytes of its start (the stack guard and
//! the pointer guard of glibc and musl are made of those).
//!
//! The self-test runs `userland/randprobe.c` as a musl static-PIE and as a
//! glibc program, several processes of each, and compares what they print:
//! no two processes may see the same `AT_RANDOM`, guard, `getrandom` or device
//! bytes, and none may see the pattern the kernel wrote before the fix. One run
//! of each libc does the statistics over a stream; one run meets a pool that
//! is reported closed (`ring3::RANDOM_ST_CLOSED`) and must get `EAGAIN` with
//! `GRND_NONBLOCK` and bytes, after a wait, without it.
//!
//! The lines of every run are printed, so `scripts/rand-boots.py` can compare
//! them between boots.

use crate::ring3;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use euromm::FrameAllocator;

static RANDPROBE_ELF: &[u8] = include_bytes!("../../userland/randprobe.elf");
static GRANDPROBE_ELF: &[u8] = include_bytes!("../../userland/glibc/grandprobe");

/// The 16 bytes every process found at `AT_RANDOM` before BUG-080 was fixed
/// (`0x5A ^ i * 31`).
fn old_at_random() -> String {
    let mut s = String::new();
    for i in 0..16u8 {
        s.push_str(&alloc::format!("{:02x}", 0x5Au8 ^ i.wrapping_mul(31)));
    }
    s
}

struct Run {
    name: &'static str,
    exit: u64,
    out: String,
}

impl Run {
    /// The program did not start (no arena at this memory size, a chrome
    /// iteration boot): not a verdict about randomness.
    fn not_run(&self) -> bool {
        self.exit == u64::MAX && self.out.starts_with('(')
    }
    fn passed(&self) -> bool {
        self.exit == 0 && self.out.lines().any(|l| l.trim() == "randprobe: PASS")
    }
    /// The value after `randprobe: <key>=` up to the next blank.
    fn field(&self, key: &str) -> Option<String> {
        let prefix = alloc::format!("randprobe: {key}=");
        self.out
            .lines()
            .find_map(|l| l.trim().strip_prefix(prefix.as_str()))
            .map(|rest| String::from(rest.split(' ').next().unwrap_or("")))
            .filter(|v| !v.is_empty())
    }
}

/// True when every run has the field and no two runs have the same value.
fn all_distinct(runs: &[&Run], key: &str) -> bool {
    let vals: Vec<String> = runs.iter().filter_map(|r| r.field(key)).collect();
    if vals.len() != runs.len() {
        return false;
    }
    for i in 0..vals.len() {
        for j in 0..i {
            if vals[i] == vals[j] {
                return false;
            }
        }
    }
    true
}

pub fn selftest(falloc: &mut FrameAllocator) {
    let caps = ring3::CAP_CONSOLE | ring3::CAP_FILE | ring3::CAP_PROC_INFO;
    ring3::register_device_files();
    // Verify before execute, as every launcher does.
    if !ring3::verify_program("/bin/randprobe", RANDPROBE_ELF) {
        crate::serial_println!("[linux-rand] /bin/randprobe: signature INVALID or unknown, program NOT run → FAILED");
        return;
    }
    let musl = |a: &mut FrameAllocator, name: &'static str, args: &[&[u8]]| -> Run {
        let (exit, out) = x86_64::instructions::interrupts::without_interrupts(|| ring3::run_args(a, RANDPROBE_ELF, args, caps, true));
        Run { name, exit, out }
    };
    let glibc = |a: &mut FrameAllocator, name: &'static str, args: &[&[u8]]| -> Run {
        let (out, exit) = ring3::run_glibc(a, GRANDPROBE_ELF, ring3::ldlinux_bytes(), args, &[b"PATH=/bin"], caps);
        Run { name, exit, out }
    };

    let m1 = musl(falloc, "musl#1", &[b"/bin/randprobe"]);
    // The musl runs are foreground launches with interrupts off: a quarter of
    // the stream there, the whole MiB in the glibc run below.
    let m2 = musl(falloc, "musl#2", &[b"/bin/randprobe", b"stat", b"256"]);
    let g1 = glibc(falloc, "glibc#1", &[b"/bin/grandprobe"]);
    let g2 = glibc(falloc, "glibc#2", &[b"/bin/grandprobe", b"stat", b"1024"]);
    // A pool that is reported closed to the syscall layer for this one run.
    let waits0 = ring3::RANDOM_WAITS.load(Ordering::Relaxed);
    ring3::RANDOM_ST_CLOSED.store(true, Ordering::Relaxed);
    let mc = musl(falloc, "musl#closed", &[b"/bin/randprobe", b"closed"]);
    ring3::RANDOM_ST_CLOSED.store(false, Ordering::Relaxed);
    let waited = ring3::RANDOM_WAITS.load(Ordering::Relaxed).saturating_sub(waits0);

    for r in [&m1, &m2, &g1, &g2, &mc] {
        crate::serial_println!("[linux-rand] {} exit={}", r.name, r.exit as i64);
        for l in r.out.lines().filter(|l| !l.trim().is_empty()) {
            crate::serial_println!("[linux-rand]   {} {}", r.name, l.trim());
        }
    }

    let glibc_ran = !g1.not_run() && !g2.not_run();
    let mut runs: Vec<&Run> = alloc::vec![&m1, &m2];
    if glibc_ran {
        runs.push(&g1);
        runs.push(&g2);
    }
    let passed = runs.iter().all(|r| r.passed());
    let old = old_at_random();
    let at_random = all_distinct(&runs, "at_random") && runs.iter().all(|r| r.field("at_random").as_deref() != Some(old.as_str()));
    let guard = all_distinct(&runs, "stack_guard");
    let getrandom = all_distinct(&runs, "getrandom");
    let devices = all_distinct(&runs, "urandom") && all_distinct(&runs, "random");
    let stat_line = |r: &Run, what: &str| r.out.lines().any(|l| l.contains(what) && l.trim().ends_with(" ok"));
    let stats = stat_line(&m2, "stat getrandom bytes=262144")
        && stat_line(&m2, "stat urandom bytes=262144")
        && (!glibc_ran || (stat_line(&g2, "stat getrandom bytes=1048576") && stat_line(&g2, "stat urandom bytes=1048576")));
    let closed = mc.passed() && mc.out.contains("closed nonblock ret=-1 errno=11") && waited >= 3;
    let glibc_note = if glibc_ran {
        String::from("2 glibc processes")
    } else {
        alloc::format!("glibc not run: {}", g1.out.trim())
    };
    let ok = passed && at_random && guard && getrandom && devices && stats && closed;
    crate::serial_println!(
        "[linux-rand] getrandom, /dev/urandom, /dev/random and AT_RANDOM of Linux-ABI programs (2 musl processes, {glibc_note}): every check of every run passed={passed}, AT_RANDOM differs per process and is not the old constant={at_random}, stack guard differs={guard}, getrandom differs={getrandom}, device bytes differ={devices}, stream statistics (bit balance, chi-square, repeated blocks)={stats}, closed pool: GRND_NONBLOCK is EAGAIN and a blocking call waited {waited} rounds={closed} → {}",
        if ok { "OK ✓" } else { "FAILED" }
    );
}
