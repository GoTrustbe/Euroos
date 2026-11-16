//! `[aslr]` — how many address bits of a Linux program change from one start
//! to the next (BUG-088).
//!
//! Starts `userland/glibc/gaslr` (a glibc PIE) STARTS times with demand
//! paging on, as a program from a pack runs, and reads per start where its
//! executable, ld.so, libc, stack, brk heap and two fresh anonymous mappings
//! (4 KiB and 1 MiB) lie. For each region the varying bits are the bits that
//! differ from the first start in at least one start: an upper bound of the
//! entropy of the slide, and the figure paxtest and the like give. The verdict
//! compares with what Linux gives a 64-bit process (mmap base 28 bits, stack
//! 22, PIE base 28; `/proc/sys/vm/mmap_rnd_bits`).

use crate::ring3;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use euromm::FrameAllocator;

static GASLR_ELF: &[u8] = include_bytes!("../../userland/glibc/gaslr");

const STARTS: usize = 64;
const KEYS: [&str; 7] = ["exe", "ldso", "libc", "stack", "brk", "mmap", "mmapbig"];
/// Linux x86-64: mmap_rnd_bits 28 (mmap base and PIE base), stack 22.
const LINUX_MMAP: u32 = 28;
const LINUX_STACK: u32 = 22;
const LINUX_PIE: u32 = 28;

fn parse(out: &str) -> Option<[u64; 7]> {
    let l = out.lines().find(|l| l.starts_with("gaslr: "))?;
    let mut v = [0u64; 7];
    for (i, k) in KEYS.iter().enumerate() {
        let tag = alloc::format!(" {k}=0x");
        let at = l.find(&tag)? + tag.len();
        let hex: String = l[at..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        v[i] = u64::from_str_radix(&hex, 16).ok()?;
    }
    Some(v)
}

pub fn selftest(falloc: &mut FrameAllocator) {
    let caps = ring3::CAP_CONSOLE | ring3::CAP_FILE | ring3::CAP_PROC_INFO;
    let prev_demand = ring3::DEMAND_ENABLED.swap(true, Ordering::Relaxed);
    let prev_file = ring3::DEMAND_FILE_ENABLED.swap(true, Ordering::Relaxed);
    let mut seen: Vec<[u64; 7]> = Vec::new();
    let mut bad = 0usize;
    for _ in 0..STARTS {
        let (out, exit) = ring3::run_glibc(falloc, GASLR_ELF, ring3::ldlinux_bytes(), &[b"/bin/gaslr"], &[b"PATH=/bin"], caps);
        match parse(&out) {
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
    ring3::DEMAND_FILE_ENABLED.store(prev_file, Ordering::Relaxed);
    ring3::DEMAND_ENABLED.store(prev_demand, Ordering::Relaxed);
    ring3::clear_demand_file_maps();
    if seen.len() < 50 {
        crate::serial_println!("[aslr] only {} of {STARTS} starts gave their addresses: not measured ⬜", seen.len());
        return;
    }
    let mut bits = [0u32; 7];
    let mut distinct = [0usize; 7];
    for i in 0..7 {
        let x = seen.iter().fold(0u64, |acc, v| acc | (v[i] ^ seen[0][i]));
        bits[i] = x.count_ones();
        let mut vals: Vec<u64> = seen.iter().map(|v| v[i]).collect();
        vals.sort_unstable();
        vals.dedup();
        distinct[i] = vals.len();
    }
    for i in 0..7 {
        crate::serial_println!(
            "[aslr]   {:8} first {:#014x}  varying bits {:2}  distinct {}/{}",
            KEYS[i],
            seen[0][i],
            bits[i],
            distinct[i],
            seen.len()
        );
    }
    let mmap = bits[5].min(bits[6]);
    let ok = mmap >= LINUX_MMAP && bits[3] >= LINUX_STACK && bits[0] >= LINUX_PIE;
    crate::serial_println!(
        "[aslr] {} starts of a glibc PIE: varying bits mmap {} (4 KiB {}, 1 MiB {}), stack {}, PIE base {}, brk {}, ld.so {}, libc {}; Linux x86-64 gives mmap {LINUX_MMAP}, stack {LINUX_STACK}, PIE {LINUX_PIE} → {}",
        seen.len(),
        mmap,
        bits[5],
        bits[6],
        bits[3],
        bits[0],
        bits[4],
        bits[1],
        bits[2],
        if ok { "OK ✓" } else { "FAILED (below the Linux level: BUG-088)" }
    );
}
