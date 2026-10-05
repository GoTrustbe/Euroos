//! Kernel-enforced per-process syscall profiles (sprint WP1, process isolation
//! for the Linux-ABI layer). A profile is the set of Linux syscalls a process
//! may make; anything outside it ends the process, the way seccomp does on
//! Linux. Pure logic, host-tested; the kernel consults `check` on every
//! Linux-ABI syscall of a process whose profile is not `Full`.

/// Which syscall policy a process runs under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
    /// Every syscall the kernel implements (the EuroGuard capability checks
    /// still apply). The default for a compat process.
    Full,
    /// A Chromium renderer, GPU or utility child: may compute, talk to its
    /// broker over AF_UNIX and map memory, but may not start processes, reach
    /// the network, change its identity or modify the file system.
    Renderer,
    /// `seccomp` strict mode as Linux defines it: read, write, exit and
    /// sigreturn only. A process asks for this itself (`prctl(PR_SET_SECCOMP,
    /// SECCOMP_MODE_STRICT)` or `seccomp(SECCOMP_SET_MODE_STRICT)`).
    Strict,
}

impl Profile {
    pub fn from_u8(v: u8) -> Profile {
        match v {
            1 => Profile::Renderer,
            2 => Profile::Strict,
            _ => Profile::Full,
        }
    }
    pub fn as_u8(self) -> u8 {
        match self {
            Profile::Full => 0,
            Profile::Renderer => 1,
            Profile::Strict => 2,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Profile::Full => "full",
            Profile::Renderer => "renderer",
            Profile::Strict => "strict",
        }
    }
}

/// What the kernel must do with a syscall under a profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Allow,
    /// Allowed for now, but worth a log line: a W^X mapping under the renderer
    /// profile (enforcement waits until the browser's JIT behaviour is measured).
    LogOnly(&'static str),
    /// End the process with this signal number (SIGKILL = 9 for strict mode,
    /// SIGSYS = 31 for a profile violation), exit status 128 + signal.
    Kill(u8),
}

const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;
const CLONE_THREAD: u64 = 0x10000;
const AF_UNIX: u64 = 1;

const SIGKILL: u8 = 9;
const SIGSYS: u8 = 31;

/// Decide `num(a1, .., a3, ..)` under `profile`. `a1` is the first argument
/// (clone flags, socket domain), `a3` the third (mmap/mprotect protection).
pub fn check(profile: Profile, num: u64, a1: u64, a3: u64) -> Verdict {
    match profile {
        Profile::Full => Verdict::Allow,
        Profile::Strict => {
            // read, write, exit, exit_group, rt_sigreturn: the Linux strict set.
            if matches!(num, 0 | 1 | 15 | 60 | 231) {
                Verdict::Allow
            } else {
                Verdict::Kill(SIGKILL)
            }
        }
        Profile::Renderer => renderer(num, a1, a3),
    }
}

fn renderer(num: u64, a1: u64, a3: u64) -> Verdict {
    let denied = matches!(
        num,
        // new processes and program images
        57 | 58 | 59 | 322
        // tracing, modules, mounts, reboot, host name
        | 101 | 175 | 176 | 165 | 166 | 161 | 155 | 169 | 170 | 171
        // identity changes
        | 105 | 106 | 113 | 114 | 117 | 119 | 122 | 123
        // inbound network and raw sockets
        | 49 | 50 | 43 | 288
        // file-system modification. unlink/unlinkat/rmdir stay allowed: a renderer
        // deletes its own unlinked-but-open shared-memory files (measured on
        // Chromium 152, 2026-10-05); what it may delete is the VFS's uid check.
        | 85 | 76 | 82 | 316 | 83 | 258 | 86 | 265 | 88 | 266 | 90 | 268 | 92 | 260 | 133
        // System V shared memory and semaphores
        | 29 | 30 | 31 | 64 | 65 | 66
        // keyrings and the clock
        | 248 | 249 | 250 | 164 | 227 | 229
    );
    if denied {
        return Verdict::Kill(SIGSYS);
    }
    match num {
        // clone: threads only, never a new process.
        56 if a1 & CLONE_THREAD == 0 => Verdict::Kill(SIGSYS),
        // socket: the broker channel is AF_UNIX; no AF_INET/AF_INET6/AF_NETLINK.
        41 if a1 != AF_UNIX => Verdict::Kill(SIGSYS),
        // mmap / mprotect: a writable+executable mapping is logged, not yet refused.
        9 | 10 if a3 & (PROT_WRITE | PROT_EXEC) == (PROT_WRITE | PROT_EXEC) => Verdict::LogOnly("W^X: writable+executable mapping"),
        _ => Verdict::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_allows_everything() {
        for n in 0..450u64 {
            assert_eq!(check(Profile::Full, n, 0, 7), Verdict::Allow);
        }
    }

    #[test]
    fn strict_is_the_linux_strict_set() {
        for n in [0u64, 1, 15, 60, 231] {
            assert_eq!(check(Profile::Strict, n, 0, 0), Verdict::Allow);
        }
        for n in [2u64, 257, 9, 41, 59, 157, 202] {
            assert_eq!(check(Profile::Strict, n, 0, 0), Verdict::Kill(SIGKILL), "syscall {n}");
        }
    }

    #[test]
    fn renderer_denies_process_network_and_fs_changes() {
        for n in [57u64, 58, 59, 101, 165, 105, 49, 50, 82, 83, 90, 92, 29] {
            assert_eq!(check(Profile::Renderer, n, 0, 0), Verdict::Kill(SIGSYS), "syscall {n}");
        }
        // fork via clone (no CLONE_THREAD) is a new process; a thread is fine.
        assert_eq!(check(Profile::Renderer, 56, 0x1200011, 0), Verdict::Kill(SIGSYS));
        assert_eq!(check(Profile::Renderer, 56, 0x3d0f00 | CLONE_THREAD, 0), Verdict::Allow);
        // AF_UNIX to the broker yes, AF_INET no.
        assert_eq!(check(Profile::Renderer, 41, AF_UNIX, 0), Verdict::Allow);
        assert_eq!(check(Profile::Renderer, 41, 2, 0), Verdict::Kill(SIGSYS));
    }

    #[test]
    fn renderer_keeps_what_rendering_needs() {
        // read/write/mmap/munmap/futex/openat/close/epoll/sendmsg/recvmsg/memfd/clock_gettime
        for n in [0u64, 1, 9, 11, 202, 257, 3, 232, 233, 46, 47, 319, 228, 14, 13, 157, 87, 263] {
            assert_eq!(check(Profile::Renderer, n, 0, 3), Verdict::Allow, "syscall {n}");
        }
    }

    #[test]
    fn renderer_logs_wx_mappings_but_allows_them_for_now() {
        assert_eq!(check(Profile::Renderer, 9, 0, 7), Verdict::LogOnly("W^X: writable+executable mapping"));
        assert_eq!(check(Profile::Renderer, 10, 0, 6), Verdict::LogOnly("W^X: writable+executable mapping"));
        assert_eq!(check(Profile::Renderer, 10, 0, 5), Verdict::Allow);
        assert_eq!(check(Profile::Renderer, 9, 0, 3), Verdict::Allow);
    }

    #[test]
    fn profile_round_trips_through_a_byte() {
        for p in [Profile::Full, Profile::Renderer, Profile::Strict] {
            assert_eq!(Profile::from_u8(p.as_u8()), p);
        }
    }
}
