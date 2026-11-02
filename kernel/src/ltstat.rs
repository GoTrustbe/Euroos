//! Load-test hooks for the serial harness (`scripts/loadtest.py`, LOADTEST-R630).
//!
//! The shell has no loops; the harness on the host drives these over `scon`:
//!
//! - `ltstat`: one machine-readable line with the counters a leak or a slow
//!   death shows up in (free frames, heap, scheduler tasks, background
//!   processes live/zombie, thread kernel stacks, file table, process pools,
//!   ticks). A soak samples it every round.
//! - `spawn <prog> <n>`: start `n` signed musl programs from `/bin/<prog>` as
//!   preemptively scheduled background processes at once (each with its own
//!   address space), the "many processes at the same time" load. The desktop
//!   loop reaps them when they exit.
//! - `churn <proc|thread|fork|fs|net> <n>`: the `[churn-*]` boot self-tests on
//!   demand, at a chosen scale; their verdict lines go to the serial console.
//! - `httpd [on|off]`: the background HTTP server on :80 (served from the
//!   desktop loop) for inbound connection tests.
//!
//! Nothing here changes kernel behaviour; it only exposes existing paths.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use eurofs::FileSystem;
use euromm::FrameAllocator;

/// Pids for `spawn` (clear of the boot-time demo pids and the service range).
static NEXT_PID: AtomicU64 = AtomicU64::new(70_000);

pub fn stat_line(mem: &mut FrameAllocator) -> String {
    let (heap_used, heap_free) = crate::allocator::stats();
    let (live, zombie) = crate::ring3::bg_counts();
    format!(
        "ltstat frames_free={} frames_usable={} heap_used={} heap_free={} tasks={} bg_live={} bg_zombie={} kstacks={} files={} procpool_free={} demand_free={} ticks={}",
        mem.free_frames(),
        mem.usable_frames(),
        heap_used,
        heap_free,
        crate::sched::task_count(),
        live,
        zombie,
        crate::ring3::thread_kstacks_used(),
        crate::ring3::files_count(),
        crate::procpool::free_frames(),
        crate::procpool::demand_free_frames(),
        crate::interrupts::ticks()
    )
}

pub fn spawn(fs: &mut dyn FileSystem, mem: &mut FrameAllocator, prog: &str, n: &str) -> Vec<String> {
    let n: usize = n.trim().parse().unwrap_or(1).clamp(1, 4096);
    if prog.is_empty() || prog.contains('/') {
        return alloc::vec![String::from("usage: spawn <program in /bin> <count>")];
    }
    let path = format!("/bin/{prog}");
    let bytes = match fs.read_file(&path) {
        Ok(b) => b,
        Err(e) => return alloc::vec![format!("spawn: cannot read {path}: {e:?}")],
    };
    if !crate::ring3::verify_program(&path, &bytes) {
        return alloc::vec![format!("spawn: {path} rejected (signature)")];
    }
    let first = NEXT_PID.load(Ordering::Relaxed);
    let mut started = 0usize;
    for _ in 0..n {
        let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        if !crate::ring3::spawn_bg_musl(mem, &bytes, pid, prog.as_bytes()) {
            break;
        }
        started += 1;
    }
    let mut out = alloc::vec![format!(
        "spawn: started {started} of {n} x {path} (pids {first}..{})",
        first + started as u64
    )];
    if started < n {
        out.push(format!("spawn: stopped early: out of task slots or memory after {started}"));
    }
    out.push(stat_line(mem));
    out
}

pub fn churn(fs: &mut dyn FileSystem, mem: &mut FrameAllocator, kind: &str, n: &str) -> Vec<String> {
    let n: usize = n.trim().parse().unwrap_or(1).clamp(1, 100_000);
    match kind {
        "proc" => crate::churn::proc_churn_n(mem, n),
        "thread" => {
            for _ in 0..n {
                crate::churn::thread_churn(mem);
            }
        }
        "fork" => {
            for _ in 0..n {
                crate::churn::fork_churn(mem);
            }
        }
        "fs" => {
            for _ in 0..n {
                crate::churn::fs_churn(&mut *fs);
            }
        }
        "net" => {
            for _ in 0..n {
                crate::churn::net_churn();
            }
        }
        _ => return alloc::vec![String::from("usage: churn <proc|thread|fork|fs|net> <n>")],
    }
    crate::ring3::reap_dead(mem);
    alloc::vec![format!("churn {kind} x{n} done (verdicts: [churn-{kind}] on the serial console)"), stat_line(mem)]
}

pub fn httpd(arg: &str) -> Vec<String> {
    let (on, _) = crate::net::httpd_status();
    match arg {
        "on" if !on => {
            crate::net::httpd_toggle();
        }
        "off" if on => {
            crate::net::httpd_toggle();
        }
        "" | "status" | "on" | "off" => {}
        _ => return alloc::vec![String::from("usage: httpd [on|off|status]")],
    }
    let (on, served) = crate::net::httpd_status();
    alloc::vec![format!("httpd: {} on :80, served {served}", if on { "listening" } else { "off" })]
}
