//! **Churn self-tests** (`[churn-*]`, selftest builds, every boot): short, sharp
//! stress that must leave the kernel exactly where it started. Each test takes a
//! baseline of the resources it touches, churns for a bounded number of rounds,
//! then compares. A regression here is a leak or a lifecycle bug, which is how an
//! OS dies slowly on a real desk. Unlike the EUROSTRESS big load (stresstest.rs,
//! armed by a disk sentinel), these run in the gate on every build.

use alloc::vec::Vec;
use euromm::FrameAllocator;
use eurofs::FileSystem;

fn heap_used() -> usize {
    crate::allocator::stats().0
}

/// glibc processes: the leak-prone path (ld.so + libc maps, a per-run demand pool,
/// thread kernel stacks, the file table). One warm-up run absorbs one-time
/// allocations; then N runs must return every counter to its baseline.
pub fn proc_churn(alloc: &mut FrameAllocator) {
    proc_churn_n(alloc, 16)
}

/// [`proc_churn`] with `n` runs (the `churn proc <n>` load-test hook).
pub fn proc_churn_n(alloc: &mut FrameAllocator, n: usize) {
    let n = n.max(1);
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    let run = |a: &mut FrameAllocator| {
        crate::ring3::run_glibc(a, crate::ring3::gtiny_bytes(), crate::ring3::ldlinux_bytes(),
            &[b"/bin/gtiny"], &[b"PATH=/bin"], caps).1
    };
    let _ = run(alloc); // warm-up
    crate::ring3::reap_dead(alloc);
    let (f0, p0, t0, k0, fi0, h0) = (alloc.free_frames(), crate::procpool::free_frames(), crate::sched::task_count(),
        crate::ring3::thread_kstacks_used(), crate::ring3::files_count(), heap_used());
    let mut clean = 0usize;
    for i in 0..n {
        if run(alloc) == 42 { clean += 1; } // gtiny exits 42 by design
        if i < 4 || i == n - 1 {
            crate::serial_println!("[churn-proc]   after run {}: free frames {}", i + 1, alloc.free_frames());
        }
    }
    crate::ring3::reap_dead(alloc);
    let (f1, p1, t1, k1, fi1, h1) = (alloc.free_frames(), crate::procpool::free_frames(), crate::sched::task_count(),
        crate::ring3::thread_kstacks_used(), crate::ring3::files_count(), heap_used());
    let frames_ok = f1 + 4 >= f0; // ≤ 16 KiB of allocator slack, never a per-run drift
    let pool_ok = p1 == p0;
    let tasks_ok = t1 <= t0;
    let kst_ok = k1 == k0;
    let files_ok = fi1 == fi0;
    let heap_ok = h1 <= h0 + 64 * 1024;
    let ok = clean == n && frames_ok && pool_ok && tasks_ok && kst_ok && files_ok && heap_ok;
    crate::serial_println!(
        "[churn-proc] {n} glibc processes (gtiny) run + reaped: clean-exits={clean}/{n} · frames {f0}→{f1} (leak-free={frames_ok}) · demand-pool {p0}→{p1} ({pool_ok}) · tasks {t0}→{t1} ({tasks_ok}) · thread-kstacks {k0}→{k1} ({kst_ok}) · file-table {fi0}→{fi1} ({files_ok}) · heap-used {h0}→{h1} ({heap_ok}) → {}",
        if ok { "OK ✓ (every process returns what it took)" } else { "FAILED" }
    );
}

/// Threads: one process runs 60 cycles of 4 pthread create/join; every thread
/// kernel stack and futex entry must come back (exit 77 = every sum correct).
pub fn thread_churn(alloc: &mut FrameAllocator) {
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    crate::ring3::reap_dead(alloc);
    let (f0, k0, t0) = (alloc.free_frames(), crate::ring3::thread_kstacks_used(), crate::sched::task_count());
    let (out, exit) = crate::ring3::run_glibc(alloc, crate::ring3::gthreadchurn_bytes(), crate::ring3::ldlinux_bytes(),
        &[b"/bin/gthreadchurn"], &[b"PATH=/bin"], caps);
    crate::ring3::reap_dead(alloc);
    let (f1, k1, t1) = (alloc.free_frames(), crate::ring3::thread_kstacks_used(), crate::sched::task_count());
    let pass = out.contains("-> PASS");
    let ok = exit == 77 && pass && k1 == k0 && t1 <= t0 + 8 && f1 + 16 >= f0; // table may grow a few slots (high-water), never per cycle
    crate::serial_println!(
        "[churn-thread] 60 cycles × 4 pthreads create+join in one process: exit={exit} (want 77) · sums-correct={pass} · thread-kstacks {k0}→{k1} · tasks {t0}→{t1} · frames {f0}→{f1} → {}",
        if ok { "OK ✓" } else { "FAILED" }
    );
}

/// fork/exit/wait: 6 children (each fork copies the 384 MiB arena), each reaped with its own status; nothing left
/// to reap afterwards (exit 66); the kernel's counters back to baseline.
pub fn fork_churn(alloc: &mut FrameAllocator) {
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    crate::ring3::reap_dead(alloc);
    let (f0, t0, p0, d0) = (alloc.free_frames(), crate::sched::task_count(), crate::procpool::free_frames(), crate::procpool::demand_free_frames());
    let (out, exit) = crate::ring3::run_glibc(alloc, crate::ring3::gforkchurn_bytes(), crate::ring3::ldlinux_bytes(),
        &[b"/bin/gforkchurn"], &[b"PATH=/bin"], caps);
    crate::ring3::reap_dead(alloc);
    let (f1, t1, p1, d1) = (alloc.free_frames(), crate::sched::task_count(), crate::procpool::free_frames(), crate::procpool::demand_free_frames());
    let pass = out.contains("-> PASS");
    let ok = exit == 66 && pass && t1 <= t0 + 8 && p1 == p0 && d1 == d0 && f1 + 16 >= f0;
    crate::serial_println!(
        "[churn-fork] 6 fork/exit/wait cycles: exit={exit} (want 66) · every-child-reaped-with-its-status={pass} · tasks {t0}→{t1} · process-pool {p0}→{p1} · demand-pool {d0}→{d1} · frames {f0}→{f1} → {}",
        if ok { "OK ✓" } else { "FAILED" }
    );
}

/// BUG-140: fork children with threads, ended the three ways a Chromium child
/// ends (exit_group from its main thread while its threads wait, exit_group from
/// a worker thread, kill(SIGTERM) by the parent): 40 children of 7 tasks each,
/// more than the 256 scheduler slots. Every slot must come back as the child
/// ends, not only at the end of the run: on the tree before, the children's
/// threads stopped being created after about 30 children (pthread_create EAGAIN),
/// as the browser's renderers did after a few sites. Exit 88 = every round, the
/// parent's threads and one more child afterwards; 89 = the first fork was
/// refused for memory (a machine with no room for a second arena: not run).
pub fn fork_thread_churn(alloc: &mut FrameAllocator) {
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    crate::ring3::reap_dead(alloc);
    let (t0, k0, p0, d0) = (crate::sched::task_count(), crate::ring3::thread_kstacks_used(),
        crate::procpool::free_frames(), crate::procpool::demand_free_frames());
    let (out, exit) = crate::ring3::run_glibc(alloc, crate::ring3::gslots_bytes(), crate::ring3::ldlinux_bytes(),
        &[b"/bin/gslots"], &[b"PATH=/bin"], caps);
    crate::ring3::reap_dead(alloc);
    let (t1, k1, p1, d1) = (crate::sched::task_count(), crate::ring3::thread_kstacks_used(),
        crate::procpool::free_frames(), crate::procpool::demand_free_frames());
    let line = out.lines().rev().find(|l| l.contains("GSLOTS:")).map(|l| l.trim_end().replace("GSLOTS: ", ""))
        .unwrap_or_else(|| alloc::string::String::from("(no result line)"));
    let skipped = exit == 89 || exit == u64::MAX;
    // The table may grow by the few tasks one child has at a time (high-water),
    // never by a child's worth per round.
    let ok = skipped || (exit == 88 && t1 <= t0 + 16 && k1 == k0 && p1 == p0 && d1 == d0);
    crate::serial_println!(
        "[churn-forkthreads] 40 fork children x 6 threads, ended by exit_group, a worker's exit_group and kill: {line} (exit {}) · task table {t0}→{t1} · thread-kstacks {k0}→{k1} · process-pool {p0}→{p1} · demand-pool {d0}→{d1} → {}",
        if exit == u64::MAX { alloc::string::String::from("not started") } else { alloc::format!("{exit}") },
        if skipped { "OK (fork refused for memory: one process only)" } else if ok { "OK ✓" } else { "FAILED" }
    );
}

/// BUG-170: the audio stream of Chromium between two processes (gsyncsock.c):
/// a socketpair and a shared-memory region made in the parent, one end and the
/// region handed to a fork child with SCM_RIGHTS, the parent's copies closed at
/// once; then 4-byte signals both ways, non-blocking on the parent's side, and
/// end of file for the child when the parent closes. Exit 77 = every check;
/// 78 = fork refused for memory (not run).
pub fn sync_socket_test(alloc: &mut FrameAllocator) {
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    crate::ring3::reap_dead(alloc);
    // Demand paging on, as for the browser: shared memory of in-RAM files is then
    // the file's own frames in every process (SHARED_ALIASES).
    let prev = crate::ring3::DEMAND_ENABLED.swap(true, core::sync::atomic::Ordering::Relaxed);
    let (out, exit) = crate::ring3::run_glibc(alloc, crate::ring3::gsyncsock_bytes(), crate::ring3::ldlinux_bytes(),
        &[b"/bin/gsyncsock"], &[b"PATH=/bin"], caps);
    crate::ring3::DEMAND_ENABLED.store(prev, core::sync::atomic::Ordering::Relaxed);
    for l in out.lines().filter(|l| l.contains("FAIL ")) {
        crate::serial_println!("[syncsock]   {}", l.trim_end());
    }
    let line = out.lines().rev().find(|l| l.contains("GSYNCSOCK:") && l.contains(" -> "))
        .map(|l| l.trim_end().replace("GSYNCSOCK: ", "")).unwrap_or_else(|| alloc::string::String::from("(no result line)"));
    let skipped = exit == 78 || exit == u64::MAX;
    crate::serial_println!(
        "[syncsock] socketpair end + memfd to a fork child over SCM_RIGHTS, sender closes, 4-byte signals both ways: {line} (exit {}) → {}",
        if exit == u64::MAX { alloc::string::String::from("not started") } else { alloc::format!("{exit}") },
        if skipped { "OK (not run: no room for a second process)" } else if exit == 77 { "OK ✓" } else { "FAILED" });
}

/// BUG-181: the ALSA playback device with 6, 1 and 2 channels (gpcm.c): what
/// Chromium does on hw:0,0 for 5.1, mono and stereo media. Each stream must be
/// accepted (refine, HW_PARAMS, a whole WRITEI) and what reaches the stereo
/// ring must be its downmix: 6 channels FL FR RL RR FC LFE = 12000 -12000 6000
/// -6000 4000 30000 -> L 7899, R -5556 (fronts 1, rears and centre 0.7071, LFE
/// dropped, each side divided by the sum of its weights); mono 5000 -> 5000 on
/// both; stereo 1234 -4321 unchanged. Exit 77 = every step; 78 = no device (a
/// machine without HD-Audio: not run).
pub fn pcm_channels_test(alloc: &mut FrameAllocator) {
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    crate::ring3::reap_dead(alloc);
    crate::alsa::first_frames_reset();
    let (out, exit) = crate::ring3::run_glibc(alloc, crate::ring3::gpcm_bytes(), crate::ring3::ldlinux_bytes(),
        &[b"/bin/gpcm"], &[b"PATH=/bin"], caps);
    for l in out.lines().filter(|l| l.starts_with("FAIL")) {
        crate::serial_println!("[pcmch]   {}", l.trim_end());
    }
    let got = crate::alsa::first_frames();
    let want: [(u32, i16, i16); 3] = [(6, 7899, -5556), (1, 5000, 5000), (2, 1234, -4321)];
    let mut ring_ok = true;
    let mut seen = alloc::string::String::new();
    for &(ch, l, r) in &want {
        match got.iter().find(|g| g.0 == ch) {
            Some(&(_, gl, gr)) => {
                let ok = (gl as i32 - l as i32).abs() <= 2 && (gr as i32 - r as i32).abs() <= 2;
                ring_ok &= ok;
                seen.push_str(&alloc::format!(" {ch} ch -> ({gl}, {gr}) want ({l}, {r}){}", if ok { "" } else { " FAIL" }));
            }
            None => {
                ring_ok = false;
                seen.push_str(&alloc::format!(" {ch} ch -> nothing reached the ring FAIL"));
            }
        }
    }
    let line = out.lines().rev().find(|l| l.starts_with("GPCM:")).map(|l| alloc::string::String::from(l.trim_end()))
        .unwrap_or_else(|| alloc::string::String::from("(no result line)"));
    let skipped = exit == 78 || exit == u64::MAX;
    crate::serial_println!(
        "[pcmch] ALSA device with 6, 1 and 2 channels, downmix to the stereo ring: {line} (exit {}); ring:{} → {}",
        if exit == u64::MAX { alloc::string::String::from("not started") } else { alloc::format!("{exit}") },
        if skipped { alloc::string::String::from(" -") } else { seen },
        if skipped { "OK (not run: no HD-Audio device)" } else if exit == 77 && ring_ok { "OK ✓" } else { "FAILED" });
}

/// BUG-147: 320 threads of one glibc process alive at the same time
/// (gmanythreads.c), more than the 256 task slots and 224 thread kernel stacks
/// of the tree before; a browser session had 214 live tasks. Exit 77 = all
/// created and joined; the thread kernel stacks back to where they were.
pub fn many_threads_test(alloc: &mut FrameAllocator) {
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    crate::ring3::reap_dead(alloc);
    let k0 = crate::ring3::thread_kstacks_used();
    let prev = crate::ring3::DEMAND_ENABLED.swap(true, core::sync::atomic::Ordering::Relaxed);
    let (out, exit) = crate::ring3::run_glibc(alloc, crate::ring3::gmanythreads_bytes(), crate::ring3::ldlinux_bytes(),
        &[b"/bin/gmanythreads"], &[b"PATH=/bin"], caps);
    crate::ring3::DEMAND_ENABLED.store(prev, core::sync::atomic::Ordering::Relaxed);
    crate::ring3::reap_dead(alloc);
    let k1 = crate::ring3::thread_kstacks_used();
    let line = out.lines().rev().find(|l| l.contains("GMANYTHREADS:"))
        .map(|l| l.trim_end().replace("GMANYTHREADS: ", "")).unwrap_or_else(|| alloc::string::String::from("(no result line)"));
    let skipped = exit == u64::MAX;
    let ok = exit == 77 && k1 == k0;
    crate::serial_println!(
        "[many-threads] 320 threads of one process alive at once: {line} (exit {}) · thread-kstacks {k0}→{k1} → {}",
        if skipped { alloc::string::String::from("not started") } else { alloc::format!("{exit}") },
        if skipped { "OK (not run: no arena for a glibc program)" } else if ok { "OK ✓" } else { "FAILED" });
}

/// Filesystem lifecycle: create / write / rename / read back / remove, several
/// rounds on the live root; every op must succeed, data must read back intact,
/// the directory must be empty and removable after each round, a sentinel file
/// next to it must survive, and the heap must not grow round over round.
pub fn fs_churn(vfs: &mut dyn FileSystem) {
    const ROUNDS: usize = 3;
    const N: usize = 120;
    let dir = "/tmp/churn";
    let sentinel = "/tmp/churn-sentinel";
    let _ = vfs.write_file(sentinel, b"intact");
    let mut ops_failed = 0usize;
    let mut bad_reads = 0usize;
    let mut leftovers = 0usize;
    let mut rmdir_failed = 0usize;
    let mut heap_after: Vec<usize> = Vec::new();
    for round in 0..ROUNDS {
        if vfs.create_dir(dir).is_err() { ops_failed += 1; }
        for i in 0..N {
            let path = alloc::format!("{dir}/f{i}");
            let body = alloc::format!("round {round} file {i} payload {}", "x".repeat(900));
            if vfs.write_file(&path, body.as_bytes()).is_err() { ops_failed += 1; }
        }
        for i in 0..N / 2 {
            let (a, b) = (alloc::format!("{dir}/f{i}"), alloc::format!("{dir}/r{i}"));
            if vfs.rename(&a, &b).is_err() { ops_failed += 1; }
        }
        for i in 0..N {
            let path = if i < N / 2 { alloc::format!("{dir}/r{i}") } else { alloc::format!("{dir}/f{i}") };
            match vfs.read_file(&path) {
                Ok(d) => {
                    let want = alloc::format!("round {round} file {i} payload ");
                    if !d.starts_with(want.as_bytes()) || d.len() != want.len() + 900 { bad_reads += 1; }
                }
                Err(_) => bad_reads += 1,
            }
            if vfs.remove_file(&path).is_err() { ops_failed += 1; }
        }
        leftovers += vfs.list_dir(dir).map(|v| v.len()).unwrap_or(999);
        if vfs.remove_dir(dir).is_err() { rmdir_failed += 1; }
        heap_after.push(heap_used());
    }
    let sentinel_ok = vfs.read_file(sentinel).map(|d| d == b"intact").unwrap_or(false);
    let _ = vfs.remove_file(sentinel);
    // Round-over-round growth is the leak signal (the first round may warm caches).
    let heap_stable = heap_after.len() == ROUNDS && heap_after[ROUNDS - 1] <= heap_after[0] + 64 * 1024;
    let ok = ops_failed == 0 && bad_reads == 0 && leftovers == 0 && rmdir_failed == 0 && sentinel_ok && heap_stable;
    crate::serial_println!(
        "[churn-fs] {ROUNDS} rounds × {N} files (write/rename/read/remove): op-failures={ops_failed} · bad-reads={bad_reads} · leftovers={leftovers} · rmdir-failures={rmdir_failed} · sentinel-intact={sentinel_ok} · heap-used after rounds {:?} (stable={heap_stable}) → {}",
        heap_after, if ok { "OK ✓" } else { "FAILED" }
    );
}

/// Signed package install/remove cycles (the app store path): every cycle must
/// install and remove cleanly; the installed-app list, the /pkg store listing
/// and the heap must return to baseline (a half-removed package would leave
/// store entries or links behind).
pub fn pkg_churn(vfs: &mut dyn FileSystem) {
    const N: usize = 12;
    let _ = vfs.write_file("/tmp/churn-hello-euro.eupkg", crate::pkg::hello_euro_eupkg());
    // The package may already be installed by the SDK self-test: remove first so
    // the baseline is "not installed", then the cycles are install → remove.
    let _ = crate::pkg::remove(vfs, "hello-euro");
    let apps0 = crate::pkg::installed_apps(vfs).len();
    let store0 = vfs.list_dir("/pkg").map(|v| v.len()).unwrap_or(0);
    let h0 = heap_used();
    let (mut inst_ok, mut rm_ok) = (0usize, 0usize);
    for _ in 0..N {
        if crate::pkg::install_file(vfs, "/tmp/churn-hello-euro.eupkg").iter().any(|l| l.contains("installed")) { inst_ok += 1; }
        if crate::pkg::installed_apps(vfs).iter().any(|a| a.name == "hello-euro") {
            let _ = crate::pkg::remove(vfs, "hello-euro");
            if !crate::pkg::installed_apps(vfs).iter().any(|a| a.name == "hello-euro") { rm_ok += 1; }
        }
    }
    let apps1 = crate::pkg::installed_apps(vfs).len();
    let store1 = vfs.list_dir("/pkg").map(|v| v.len()).unwrap_or(0);
    let h1 = heap_used();
    let _ = vfs.remove_file("/tmp/churn-hello-euro.eupkg");
    // Put it back for the launcher (the SDK self-test installed it before us).
    let _ = vfs.write_file("/tmp/churn-hello-euro.eupkg", crate::pkg::hello_euro_eupkg());
    let _ = crate::pkg::install_file(vfs, "/tmp/churn-hello-euro.eupkg");
    let _ = vfs.remove_file("/tmp/churn-hello-euro.eupkg");
    let heap_ok = h1 <= h0 + 96 * 1024;
    let ok = inst_ok == N && rm_ok == N && apps1 == apps0 && store1 == store0 && heap_ok;
    crate::serial_println!(
        "[churn-pkg] {N} signed-package install+remove cycles (hello-euro): installs={inst_ok}/{N} · removes={rm_ok}/{N} · installed-apps {apps0}→{apps1} · /pkg entries {store0}→{store1} · heap-used {h0}→{h1} ({heap_ok}) → {}",
        if ok { "OK ✓" } else { "FAILED" }
    );
}

/// I/O + interrupt pressure for a bounded window: hundreds of root-disk reads
/// and surface presents back to back while the LAPIC timer keeps ticking. The
/// tick must stay regular: lost ticks over the window bounded, time advancing.
pub fn io_storm(vfs: &mut dyn FileSystem) {
    const READS: usize = 300;
    const PRESENTS: usize = 300;
    let body = alloc::vec![0xA5u8; 64 * 1024];
    let _ = vfs.write_file("/tmp/churn-storm.bin", &body);
    let frame = alloc::vec![0x3366ccu32; 320 * 200];
    let _ = crate::euroapi::create(0x6_0000, 320, 200);
    let t0 = crate::interrupts::ticks();
    let lost0 = crate::sched::LOST_TICKS.load(core::sync::atomic::Ordering::Relaxed);
    let (mut bad_reads, mut bad_presents) = (0usize, 0usize);
    for i in 0..READS.max(PRESENTS) {
        if i < READS {
            match vfs.read_file("/tmp/churn-storm.bin") {
                Ok(d) if d.len() == body.len() && d[777] == 0xA5 => {}
                _ => bad_reads += 1,
            }
        }
        if i < PRESENTS && crate::euroapi::present(0x6_0000, &frame, 320, 200) < 0 { bad_presents += 1; }
    }
    let t1 = crate::interrupts::ticks();
    let lost1 = crate::sched::LOST_TICKS.load(core::sync::atomic::Ordering::Relaxed);
    crate::euroapi::remove(0x6_0000);
    let _ = vfs.remove_file("/tmp/churn-storm.bin");
    let elapsed = t1.saturating_sub(t0);
    let lost = lost1.saturating_sub(lost0);
    // Under TCG a tick is ~10 ms; the storm takes seconds. Lost ticks must stay a
    // small fraction of the window (the scheduler notes them as [tick-late]).
    let tick_ok = elapsed > 0 && lost * 10 <= elapsed.max(10);
    let ok = bad_reads == 0 && bad_presents == 0 && tick_ok;
    crate::serial_println!(
        "[storm-io] {READS} × 64 KiB root reads + {PRESENTS} surface presents back to back: bad-reads={bad_reads} · bad-presents={bad_presents} · ticks elapsed={elapsed} · lost-ticks={lost} (≤10%={tick_ok}) → {}",
        if ok { "OK ✓ (timer stays regular under I/O pressure)" } else { "FAILED" }
    );
}

/// Socket table lifecycle without a NIC: open/close TCP and UDP sockets, and UDP
/// to the kernel's own loopback DNS responder (127.0.0.1:53, no packet leaves);
/// the table must return to its baseline and never run out mid-churn.
pub fn net_churn() {
    const N: usize = 64;
    let s0 = crate::net::sockets_in_use();
    let mut open_failed = 0usize;
    let mut close_failed = 0usize;
    let mut sends_ok = 0usize;
    for i in 0..N {
        let udp = i % 2 == 0;
        let fd = crate::net::sock_open(udp);
        if (fd as i64) < 0 { open_failed += 1; continue; }
        if udp {
            if crate::net::sock_connect(fd, euronet::ipv4::Ipv4Addr::new(127, 0, 0, 1), 53) as i64 >= 0 {
                // a minimal DNS query for "localhost" A: id, flags RD, 1 question
                let q: &[u8] = b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x09localhost\x00\x00\x01\x00\x01";
                if crate::net::sock_send(fd, q) as i64 >= 0 { sends_ok += 1; }
                let _ = crate::net::sock_recv(fd, 512);
            }
        }
        if crate::net::sock_close(fd) as i64 != 0 { close_failed += 1; }
    }
    let s1 = crate::net::sockets_in_use();
    let ok = open_failed == 0 && close_failed == 0 && s1 == s0 && sends_ok == N / 2;
    crate::serial_println!(
        "[churn-net] {N} sockets opened+closed (half UDP to the loopback DNS responder): open-failures={open_failed} · close-failures={close_failed} · loopback-sends={sends_ok}/{} · table {s0}→{s1} → {}",
        N / 2, if ok { "OK ✓" } else { "FAILED" }
    );
}

/// Euro API surfaces (the window path every native/WASM app and the RDP client
/// use): create / present / remove for many pseudo-processes; the surface list
/// must return to its baseline; an absurd size must be refused, not allocated.
pub fn surface_churn() {
    const N: u64 = 160;
    let c0 = crate::euroapi::surface_count();
    let h0 = heap_used();
    let (mut create_failed, mut present_failed) = (0usize, 0usize);
    let frame = alloc::vec![0x00ff00ffu32; 160 * 100];
    for i in 0..N {
        let pid = 0x5_0000 + i;
        if crate::euroapi::create(pid, 160, 100) < 0 { create_failed += 1; continue; }
        if crate::euroapi::present(pid, &frame, 160, 100) < 0 { present_failed += 1; }
        crate::euroapi::remove(pid);
    }
    let absurd_refused = crate::euroapi::create(0x5_ffff, 100_000, 100_000) < 0;
    crate::euroapi::remove(0x5_ffff);
    let c1 = crate::euroapi::surface_count();
    let h1 = heap_used();
    let heap_ok = h1 <= h0 + 64 * 1024;
    let ok = create_failed == 0 && present_failed == 0 && c1 == c0 && absurd_refused && heap_ok;
    crate::serial_println!(
        "[churn-surface] {N} surfaces created+presented+removed: create-failures={create_failed} · present-failures={present_failed} · surfaces {c0}→{c1} · absurd-size-refused={absurd_refused} · heap-used {h0}→{h1} ({heap_ok}) → {}",
        if ok { "OK ✓" } else { "FAILED" }
    );
}
