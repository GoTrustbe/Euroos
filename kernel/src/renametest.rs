//! `[rename]` — a rename over an existing file, an unlink, and what the open
//! descriptors and mappings of the OTHER files see afterwards (BUG-091).
//!
//! An open descriptor and a mapping hold an index into the file table of the
//! Linux ABI. `rename` over an existing file used to remove that file's entry,
//! so every later file moved one slot down under the descriptors that were
//! open on it. The self-test runs `userland/renprobe.c` twice: the freestanding
//! build as a foreground program (file mappings are copies) and the glibc build
//! with demand paging on (a mapping of 1 MiB is filled page by page from the
//! file, so a page touched after the rename shows which file the mapping
//! belongs to). The program prints one line per check; all of them are printed
//! here, and the verdict line counts them.

use crate::ring3;
use alloc::string::String;
use core::sync::atomic::Ordering;
use euromm::FrameAllocator;

static RENPROBE_ELF: &[u8] = include_bytes!("../../userland/renprobe.elf");
static GRENPROBE_ELF: &[u8] = include_bytes!("../../userland/glibc/grenprobe");

/// The ELF bytes of /bin/renprobe (the freestanding build).
pub fn renprobe_bytes() -> &'static [u8] {
    RENPROBE_ELF
}

struct Run {
    name: &'static str,
    exit: u64,
    out: String,
}

impl Run {
    /// The program did not start (no arena at this memory size, a chrome
    /// iteration boot): not a verdict about rename.
    fn not_run(&self) -> bool {
        self.exit == u64::MAX && self.out.starts_with('(')
    }
    fn passed(&self) -> bool {
        self.exit == 0 && self.out.lines().any(|l| l.trim() == "renprobe: PASS")
    }
    fn ok_lines(&self) -> usize {
        self.out.lines().filter(|l| l.starts_with("renprobe: ") && l.trim().ends_with(" ok")).count()
    }
    fn failed_lines(&self) -> usize {
        self.out.lines().filter(|l| l.starts_with("renprobe: ") && l.contains(" FAILED ")).count()
    }
    fn has(&self, what: &str) -> bool {
        self.out.lines().any(|l| l.contains(what))
    }
}

pub fn selftest(falloc: &mut FrameAllocator) {
    let caps = ring3::CAP_CONSOLE | ring3::CAP_FILE | ring3::CAP_PROC_INFO;
    // Verify before execute, as every launcher does.
    if !ring3::verify_program("/bin/renprobe", RENPROBE_ELF) {
        crate::serial_println!("[rename] /bin/renprobe: signature INVALID or unknown, program NOT run → FAILED");
        return;
    }
    let flat = {
        let (exit, out) = x86_64::instructions::interrupts::without_interrupts(|| {
            ring3::run_args(falloc, RENPROBE_ELF, &[b"/bin/renprobe", b"/tmp/renprobe-flat"], caps, true)
        });
        Run { name: "freestanding", exit, out }
    };
    // The glibc run with demand paging, as the file-mapping self-test (gfmmap) and
    // every program started from a pack have it.
    let prev_demand = ring3::DEMAND_ENABLED.swap(true, Ordering::Relaxed);
    let prev_file = ring3::DEMAND_FILE_ENABLED.swap(true, Ordering::Relaxed);
    let glibc = {
        let (out, exit) = ring3::run_glibc(
            falloc,
            GRENPROBE_ELF,
            ring3::ldlinux_bytes(),
            &[b"/bin/grenprobe", b"/tmp/renprobe-glibc"],
            &[b"PATH=/bin"],
            caps,
        );
        Run { name: "glibc", exit, out }
    };
    ring3::DEMAND_FILE_ENABLED.store(prev_file, Ordering::Relaxed);
    ring3::DEMAND_ENABLED.store(prev_demand, Ordering::Relaxed);
    ring3::clear_demand_file_maps();

    for r in [&flat, &glibc] {
        crate::serial_println!("[rename] {} exit={}", r.name, r.exit as i64);
        for l in r.out.lines().filter(|l| !l.trim().is_empty()) {
            crate::serial_println!("[rename]   {} {}", r.name, l.trim());
        }
    }
    let glibc_ran = !glibc.not_run();
    // The glibc run must have had its lazily filled mapping, and step D must have
    // run: without them it proves less than it says.
    let lazy = glibc.has("setup mappings shared=1 big=1") && glibc.has("D the mapping of the unlinked file keeps its bytes");
    let ok = flat.passed() && (!glibc_ran || (glibc.passed() && lazy));
    let glibc_note = if glibc_ran {
        alloc::format!(
            "glibc with demand paging: {} checks ok, {} failed, lazily filled mapping and step D {}",
            glibc.ok_lines(),
            glibc.failed_lines(),
            if lazy { "ran" } else { "DID NOT RUN" }
        )
    } else {
        alloc::format!("glibc not run: {}", glibc.out.trim())
    };
    crate::serial_println!(
        "[rename] rename over an existing file, unlink + rename, held descriptors and mappings (freestanding: {} checks ok, {} failed; {glibc_note}) → {}",
        flat.ok_lines(),
        flat.failed_lines(),
        if ok { "OK ✓" } else { "FAILED" }
    );
}
