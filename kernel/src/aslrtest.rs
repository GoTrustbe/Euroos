//! `[aslr]` — how many address bits of a Linux program change from one start
//! to the next (BUG-088), and the checks of ASLR step S0 (docs/design/ASLR.md).
//!
//! Starts `userland/glibc/gaslrx` (a glibc PIE; `gaslr` plus the vDSO and a
//! fork/exec mode) STARTS times with demand paging on, as a program from a
//! pack runs, and reads per start where its executable, ld.so, libc, stack,
//! brk heap, two fresh anonymous mappings (4 KiB and 1 MiB) and the vDSO lie.
//! For each region the varying bits are the bits that differ from the first
//! start in at least one start: an upper bound of the entropy of the slide,
//! and the figure paxtest and the like give. The verdict compares with what
//! Linux gives a 64-bit process (mmap base 28 bits, stack 22, PIE base 28;
//! `/proc/sys/vm/mmap_rnd_bits`).
//!
//! The same starts give:
//! - `[aslr-phys]`: an address of the process that is its own physical
//!   address (the identity-mapped arena; looked up in the page tables of the
//!   process by `run_glibc`). Until step S4 the arena regions are.
//! - `[aslr-ind]`: the bits that vary in the distance between two regions
//!   (stack - mmap, exe - stack, exe - mmap; since S6 also stack - ld.so,
//!   exe - brk and brk - stack: every pair of the three arena segments).
//!   Regions that move together give 0; the target is 20 (step S6 gives the
//!   arena regions their own bases).
//! - `[aslr-fork]`: a fork child sees the layout of its parent bit for bit.
//! - `[aslr-exec]`: an exec in a fork child gets a new draw in every region.

use crate::ring3;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use euromm::layout::Mode;
use euromm::FrameAllocator;

static GASLR_ELF: &[u8] = include_bytes!("../../userland/glibc/gaslrx");

const STARTS: usize = 64;
const FORKS: usize = 8;
const KEYS: [&str; 8] = ["exe", "ldso", "libc", "stack", "brk", "mmap", "mmapbig", "vdso"];
const EXE: usize = 0;
const LDSO: usize = 1;
const STACK: usize = 3;
const BRK: usize = 4;
const MMAP: usize = 5;
const MMAPBIG: usize = 6;
const VDSO: usize = 7;
/// Linux x86-64: mmap_rnd_bits 28 (mmap base and PIE base), stack 22.
const LINUX_MMAP: u32 = 28;
const LINUX_STACK: u32 = 22;
const LINUX_PIE: u32 = 28;
/// `[aslr-ind]`: the distance between two regions must vary in this many bits.
const IND_BITS: u32 = 20;

fn parse_tag(out: &str, tag: &str) -> Option<[u64; 8]> {
    let head = alloc::format!("{tag}: ");
    let l = out.lines().find(|l| l.starts_with(&head))?;
    let mut v = [0u64; 8];
    for (i, k) in KEYS.iter().enumerate() {
        let tag = alloc::format!(" {k}=0x");
        let at = l.find(&tag)? + tag.len();
        let hex: String = l[at..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        v[i] = u64::from_str_radix(&hex, 16).ok()?;
    }
    Some(v)
}

fn run(falloc: &mut FrameAllocator, arg: Option<&[u8]>) -> (String, u64) {
    let caps = ring3::CAP_CONSOLE | ring3::CAP_FILE | ring3::CAP_PROC_INFO;
    let mut argv: Vec<&[u8]> = alloc::vec![b"/bin/gaslrx"];
    if let Some(a) = arg {
        argv.push(a);
    }
    ring3::run_glibc(falloc, GASLR_ELF, ring3::ldlinux_bytes(), &argv, &[b"PATH=/bin"], caps)
}

/// Bits that differ from the first value in at least one value.
fn varying(xs: &[u64]) -> u32 {
    xs.iter().fold(0u64, |a, &x| a | (x ^ xs[0])).count_ones()
}

pub fn selftest(falloc: &mut FrameAllocator) {
    let mode = ring3::aslr_mode();
    let prev_demand = ring3::DEMAND_ENABLED.swap(true, Ordering::Relaxed);
    let prev_file = ring3::DEMAND_FILE_ENABLED.swap(true, Ordering::Relaxed);
    let mut seen: Vec<[u64; 8]> = Vec::new();
    let mut bad = 0usize;
    for i in 0..STARTS {
        ring3::PHYS_PROBE.store(i == 0, Ordering::Relaxed);
        let (out, exit) = run(falloc, None);
        match parse_tag(&out, "gaslr") {
            Some(v) if exit == 0 => seen.push(v),
            _ => {
                bad += 1;
                if bad == 1 {
                    crate::serial_println!("[aslr]   a start gave no addresses: exit {} {:?}", exit as i64, out.trim());
                }
                if out.starts_with('(') {
                    break; // no arena at this memory size: not a verdict about the layout
                }
            }
        }
    }
    ring3::PHYS_PROBE.store(false, Ordering::Relaxed);
    let phys: Vec<(u64, u64)> = core::mem::take(&mut *ring3::PHYS_SEEN.lock());
    let vdso_ptes = *ring3::PHYS_VDSO_PTES.lock();
    // Fork and exec: a few starts each.
    let mut forks: Vec<([u64; 8], [u64; 8])> = Vec::new();
    let mut fork_dead = false;
    for _ in 0..FORKS {
        let (out, _) = run(falloc, Some(b"fork"));
        match (parse_tag(&out, "gaslr-parent"), parse_tag(&out, "gaslr-fork")) {
            (Some(p), Some(c)) => forks.push((p, c)),
            (Some(_), None) => {
                // The child printed nothing: it died (and the parent waited for
                // it until wait4 gave up). One is the verdict; no more of them.
                crate::serial_println!("[aslr-fork]   the fork child gave no line: {:?}", out.trim());
                fork_dead = true;
                break;
            }
            _ => crate::serial_println!("[aslr-fork]   a start gave no line: {:?}", out.trim()),
        }
    }
    let (exec_out, _) = run(falloc, Some(b"exec"));
    ring3::DEMAND_FILE_ENABLED.store(prev_file, Ordering::Relaxed);
    ring3::DEMAND_ENABLED.store(prev_demand, Ordering::Relaxed);
    ring3::clear_demand_file_maps();

    report_aslr(mode, &seen);
    report_phys(&seen, &phys);
    report_vdso_ro(&vdso_ptes);
    report_ind(mode, &seen);
    report_fork(&forks, fork_dead);
    report_exec(mode, &exec_out);
}

fn report_aslr(mode: Mode, seen: &[[u64; 8]]) {
    if seen.len() < 50 {
        crate::serial_println!("[aslr] only {} of {STARTS} starts gave their addresses: not measured ⬜", seen.len());
        return;
    }
    let mut bits = [0u32; 8];
    for i in 0..8 {
        let col: Vec<u64> = seen.iter().map(|v| v[i]).collect();
        bits[i] = varying(&col);
        let mut d = col.clone();
        d.sort_unstable();
        d.dedup();
        crate::serial_println!(
            "[aslr]   {:8} first {:#014x}  varying bits {:2}  distinct {}/{}",
            KEYS[i], seen[0][i], bits[i], d.len(), seen.len()
        );
    }
    let mmap = bits[MMAP].min(bits[MMAPBIG]);
    let ok = mmap >= LINUX_MMAP && bits[STACK] >= LINUX_STACK && bits[EXE] >= LINUX_PIE;
    let verdict = if mode == Mode::Off {
        "off (boot option aslr=off: fixed addresses on purpose) ⬜"
    } else if ok {
        "OK ✓"
    } else {
        "FAILED (below the Linux level: BUG-088)"
    };
    crate::serial_println!(
        "[aslr] {} starts of a glibc PIE (aslr={}): varying bits mmap {} (4 KiB {}, 1 MiB {}), stack {}, PIE base {}, brk {}, ld.so {}, libc {}, vDSO {}; Linux x86-64 gives mmap {LINUX_MMAP}, stack {LINUX_STACK}, PIE {LINUX_PIE} → {verdict}",
        seen.len(), mode.name(), mmap, bits[MMAP], bits[MMAPBIG], bits[STACK], bits[EXE], bits[4], bits[1], bits[2], bits[VDSO]
    );
}

/// `[aslr-phys]`: which regions of the first start lie at their own physical
/// address (looked up in the page tables of the process before they were freed).
fn report_phys(seen: &[[u64; 8]], phys: &[(u64, u64)]) {
    let Some(first) = seen.first() else {
        crate::serial_println!("[aslr-phys] no start gave its addresses: not measured ⬜");
        return;
    };
    if phys.is_empty() {
        crate::serial_println!("[aslr-phys] the addresses of the first start were not looked up: not measured ⬜");
        return;
    }
    let mut same = String::new();
    let mut looked = 0;
    for (i, k) in KEYS.iter().enumerate() {
        if let Some(&(va, pa)) = phys.iter().find(|&&(va, _)| va == first[i]) {
            if pa == u64::MAX {
                continue;
            }
            looked += 1;
            if pa == va {
                same.push(' ');
                same.push_str(k);
            }
        }
    }
    if same.is_empty() {
        crate::serial_println!("[aslr-phys] {looked} addresses of a glibc process looked up in its page tables: none is its own physical address → OK ✓");
    } else {
        crate::serial_println!(
            "[aslr-phys] {looked} addresses of a glibc process looked up in its page tables; at their own physical address (identity-mapped arena):{same} → FAILED (the identity arena: boot option aslr.arena=identity or aslr=legacy)"
        );
    }
}

/// `[aslr-ind]`: bits that vary in the distance between two regions.
fn report_ind(mode: Mode, seen: &[[u64; 8]]) {
    if seen.len() < 50 {
        crate::serial_println!("[aslr-ind] only {} starts: not measured ⬜", seen.len());
        return;
    }
    let pair = |a: usize, b: usize| -> u32 {
        let d: Vec<u64> = seen.iter().map(|v| v[a].wrapping_sub(v[b])).collect();
        varying(&d)
    };
    let (sm, es, em) = (pair(STACK, MMAP), pair(EXE, STACK), pair(EXE, MMAP));
    // ASLR S6 (design S6 exit criterion): stack - ld.so, and the brk heap
    // against the image and the stack segment.
    let (sl, eb, bs) = (pair(STACK, LDSO), pair(EXE, BRK), pair(BRK, STACK));
    let (lb, vb) = (pair(2, EXE), pair(VDSO, MMAPBIG));
    // Shown, not judged: the S1 regions against the executable, and the two
    // pairs that share a segment (only their page slides differ).
    let (eb1, ev, el, bm) = (pair(MMAPBIG, EXE), pair(VDSO, EXE), pair(LDSO, EXE), pair(MMAP, BRK));
    let low: Vec<&str> = [("stack-mmap", sm), ("exe-stack", es), ("exe-mmap", em), ("stack-ldso", sl), ("exe-brk", eb), ("brk-stack", bs)]
        .iter()
        .filter(|&&(_, b)| b < IND_BITS)
        .map(|&(n, _)| n)
        .collect();
    let verdict = if mode == Mode::Off {
        String::from("off (boot option aslr=off) ⬜")
    } else if low.is_empty() {
        String::from("OK ✓")
    } else {
        alloc::format!("FAILED (not yet: {} move with one arena base until step S6)", low.join(", "))
    };
    crate::serial_println!(
        "[aslr-ind] {} starts: varying bits of the distance stack-mmap {sm}, exe-stack {es}, exe-mmap {em}, stack-ldso {sl}, exe-brk {eb}, brk-stack {bs} (want >= {IND_BITS}); libc-exe {lb}, vDSO-mmap1MiB {vb}, mmap1MiB-exe {eb1}, vDSO-exe {ev}; same segment: ldso-exe {el}, mmap-brk {bm} → {verdict}",
        seen.len()
    );
}

/// `[aslr-fork]`: the addresses of the fork child equal those of its parent in every region.
fn report_fork(forks: &[([u64; 8], [u64; 8])], dead: bool) {
    if dead {
        crate::serial_println!("[aslr-fork] a fork child of the probe died before it printed its addresses (after {} that did) → FAILED", forks.len());
        return;
    }
    if forks.len() < FORKS / 2 {
        crate::serial_println!("[aslr-fork] only {} of {FORKS} fork starts gave both lines: not measured ⬜", forks.len());
        return;
    }
    let diff: Vec<&str> = (0..8).filter(|&i| forks.iter().any(|(p, c)| p[i] != c[i])).map(|i| KEYS[i]).collect();
    if diff.is_empty() {
        crate::serial_println!("[aslr-fork] {} fork children: every region (exe, ld.so, libc, stack, brk, mmap 4 KiB and 1 MiB, vDSO) at the address of the parent → OK ✓", forks.len());
    } else {
        crate::serial_println!("[aslr-fork] {} fork children: not at the address of the parent: {} → FAILED", forks.len(), diff.join(", "));
    }
}

/// `[aslr-exec]`: the probe execs itself in a fork child.
fn report_exec(_mode: Mode, out: &str) {
    if let Some(l) = out.lines().find(|l| l.starts_with("gaslr-exec: refused")) {
        // The kernel re-executes only the persistent program from disk
        // (do_child_execve), so the probe cannot exec itself. Kernel side
        // instead: the placement a launch from disk makes against eight execs
        // in a fork child, with the draw do_child_execve makes.
        let kept = ring3::aslr_exec_check();
        let mut not_yet: Vec<&str> = Vec::new();
        let mut line = String::new();
        for &(k, n) in kept.iter() {
            line.push_str(&alloc::format!(" {k} {n}/8"));
            if n >= 4 {
                not_yet.push(match k {
                    "ldso" => "ld.so (the arena base stays until S5)",
                    "vdso" => "vDSO (an exec'd child has none)",
                    "exe" => "exe",
                    "floor" => "mmap floor",
                    other => other,
                });
            }
        }
        let verdict = if _mode == Mode::Off {
            String::from("off (boot option aslr=off: no draw on purpose) ⬜")
        } else if not_yet.is_empty() {
            String::from("OK ✓")
        } else {
            alloc::format!("FAILED (not yet: {})", not_yet.join(", "))
        };
        crate::serial_println!(
            "[aslr-exec] the probe exec was refused ({}: only the persistent program from disk re-executes); kernel side, 8 execs after a launch from disk (aslr={}), kept the address of the parent:{line} → {verdict}",
            l.trim_start_matches("gaslr-exec: refused ").trim(),
            _mode.name()
        );
        return;
    }
    match (parse_tag(out, "gaslr"), parse_tag(out, "gaslr-exec")) {
        (Some(p), Some(c)) => {
            let same: Vec<&str> = (0..8).filter(|&i| p[i] == c[i] && p[i] != 0).map(|i| KEYS[i]).collect();
            if same.is_empty() {
                crate::serial_println!("[aslr-exec] the exec'd child differs from its parent in every region → OK ✓");
            } else {
                crate::serial_println!("[aslr-exec] the exec'd child keeps the address of the parent in: {} → FAILED", same.join(", "));
            }
        }
        _ => crate::serial_println!("[aslr-exec] the exec start gave no pair: {:?} ⬜", out.trim()),
    }
}

/// `[vdso-ro]` (BUG-353): the vDSO image and its clock page are one set of
/// frames shared by every glibc process; a process must not be able to write
/// them. Their leaf entries in the first start must be present, user, not writable.
fn report_vdso_ro(ptes: &[u64; 2]) {
    const W: u64 = 1 << 1;
    const U: u64 = 1 << 2;
    if ptes.iter().any(|&e| e & 1 == 0) {
        crate::serial_println!("[vdso-ro] the vDSO pages of the first start were not found in its page tables: not measured ⬜");
        return;
    }
    let w: Vec<&str> = [("image", ptes[0]), ("clock page", ptes[1])].iter().filter(|&&(_, e)| e & W != 0).map(|&(n, _)| n).collect();
    if w.is_empty() && ptes.iter().all(|&e| e & U != 0) {
        crate::serial_println!("[vdso-ro] the vDSO image and clock page (shared by every glibc process) are mapped user read-only → OK ✓");
    } else {
        crate::serial_println!("[vdso-ro] writable for the process: {} (the frames are shared by every glibc process: BUG-353) → FAILED", w.join(", "));
    }
}
