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

/// `[rename-disk]` (BUG-111): an atomic save by a Linux program, as the
/// Terminal runs it, must reach EuroFS, and a program of the next boot must
/// see it. `renprobe save DIR 1` creates three documents, `renprobe save DIR
/// 2` renames a new file over a.txt, replaces b.txt by unlink + rename and
/// unlinks c.txt. After each run the Terminal's write-back runs; then EuroFS
/// is read directly. Then the program's file table forgets the directory (as
/// the table of the next boot does not have it) and `renprobe check DIR 2`
/// has to find version 2 through EuroFS.
pub fn disk_selftest(falloc: &mut FrameAllocator, fs: &mut dyn eurofs::FileSystem) {
    const DIR: &str = "/tmp/rendisk";
    let caps = ring3::CAP_CONSOLE | ring3::CAP_FILE | ring3::CAP_PROC_INFO;
    if !ring3::verify_program("/bin/renprobe", RENPROBE_ELF) {
        crate::serial_println!("[rename-disk] /bin/renprobe: signature INVALID or unknown, program NOT run → FAILED");
        return;
    }
    let _ = fs.create_dir("/tmp");
    let _ = fs.create_dir(DIR);
    let names = ["a.txt", "b.txt", "c.txt", "a.txt.tmp", "b.txt.new"];
    for n in names {
        let _ = fs.remove_file(&alloc::format!("{DIR}/{n}"));
    }
    let mut run = |fs: &mut dyn eurofs::FileSystem, args: &[&[u8]]| -> (u64, String, alloc::vec::Vec<String>) {
        let mut argv: alloc::vec::Vec<&[u8]> = alloc::vec![b"/bin/renprobe"];
        argv.extend_from_slice(args);
        let (exit, out) = ring3::with_backing_fs(&mut *fs, || {
            x86_64::instructions::interrupts::without_interrupts(|| ring3::run_args(falloc, RENPROBE_ELF, &argv, caps, true))
        });
        let wb = ring3::write_back(&mut *fs);
        (exit, out, wb)
    };
    // What EuroFS holds: (a, b, c, temporary files left).
    fn disk(fs: &mut dyn eurofs::FileSystem) -> (String, String, String, usize) {
        let rd = |fs: &mut dyn eurofs::FileSystem, n: &str| -> String {
            fs.read_file(&alloc::format!("{DIR}/{n}"))
                .map(|b| String::from(String::from_utf8_lossy(&b).trim()))
                .unwrap_or_else(|_| String::from("(none)"))
        };
        let tmp = ["a.txt.tmp", "b.txt.new"].iter().filter(|n| fs.read_file(&alloc::format!("{DIR}/{n}")).is_ok()).count();
        (rd(fs, "a.txt"), rd(fs, "b.txt"), rd(fs, "c.txt"), tmp)
    }
    let mut lines: alloc::vec::Vec<String> = alloc::vec::Vec::new();
    let passed = |out: &str| out.lines().any(|l| l.trim() == "renprobe: PASS");

    let (e1, o1, w1) = run(&mut *fs, &[b"save", DIR.as_bytes(), b"1"]);
    let d1 = disk(&mut *fs);
    let v1 = passed(&o1) && e1 == 0 && d1.0 == "document a version 1" && d1.1 == "document b version 1" && d1.2 == "document c version 1" && d1.3 == 0;
    lines.push(alloc::format!("save 1: exit {} write-back {:?}; EuroFS a={:?} b={:?} c={:?} tmp={}", e1 as i64, w1, d1.0, d1.1, d1.2, d1.3));

    let (e2, o2, w2) = run(&mut *fs, &[b"save", DIR.as_bytes(), b"2"]);
    let d2 = disk(&mut *fs);
    let v2 = passed(&o2) && e2 == 0 && d2.0 == "document a version 2" && d2.1 == "document b version 2" && d2.2 == "(none)" && d2.3 == 0;
    lines.push(alloc::format!("save 2: exit {} write-back {:?}; EuroFS a={:?} b={:?} c={:?} tmp={}", e2 as i64, w2, d2.0, d2.1, d2.2, d2.3));

    // The next boot: the table does not know the directory.
    ring3::fsback_forget_for_test(&alloc::format!("{DIR}/"));
    let (e3, o3, w3) = run(&mut *fs, &[b"check", DIR.as_bytes(), b"2"]);
    let d3 = disk(&mut *fs);
    let v3 = passed(&o3) && e3 == 0 && d3 == d2;
    lines.push(alloc::format!("check 2 with a table that does not know {DIR}: exit {} write-back {:?}", e3 as i64, w3));
    for l in o3.lines().filter(|l| l.contains(" FAILED ")) {
        lines.push(alloc::format!("  {}", l.trim()));
    }
    for l in &lines {
        crate::serial_println!("[rename-disk]   {l}");
    }
    for n in names {
        let _ = fs.remove_file(&alloc::format!("{DIR}/{n}"));
    }
    ring3::fsback_forget_for_test(&alloc::format!("{DIR}/"));
    let ok = v1 && v2 && v3;
    crate::serial_println!(
        "[rename-disk] a Linux program's save (create; rename over, unlink + rename, unlink) on EuroFS: save 1 {}, save 2 {}, seen by the next boot {} → {}",
        if v1 { "ok" } else { "WRONG" },
        if v2 { "ok" } else { "WRONG" },
        if v3 { "ok" } else { "WRONG" },
        if ok { "OK ✓" } else { "FAILED" }
    );
}
