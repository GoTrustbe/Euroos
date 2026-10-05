//! Per-process resource limits (`getrlimit`/`setrlimit`/`prlimit64`), kept and
//! checked the Linux way: a soft limit never above the hard limit, and an
//! unprivileged process can lower but never raise its hard limit.

pub const RLIMIT_CPU: u32 = 0;
pub const RLIMIT_FSIZE: u32 = 1;
pub const RLIMIT_DATA: u32 = 2;
pub const RLIMIT_STACK: u32 = 3;
pub const RLIMIT_CORE: u32 = 4;
pub const RLIMIT_RSS: u32 = 5;
pub const RLIMIT_NPROC: u32 = 6;
pub const RLIMIT_NOFILE: u32 = 7;
pub const RLIMIT_MEMLOCK: u32 = 8;
pub const RLIMIT_AS: u32 = 9;
pub const RLIMIT_LOCKS: u32 = 10;
pub const RLIMIT_SIGPENDING: u32 = 11;
pub const RLIMIT_MSGQUEUE: u32 = 12;
pub const RLIMIT_NICE: u32 = 13;
pub const RLIMIT_RTPRIO: u32 = 14;
pub const RLIMIT_RTTIME: u32 = 15;
pub const RLIM_NLIMITS: usize = 16;
pub const RLIM_INFINITY: u64 = u64::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    pub cur: [u64; RLIM_NLIMITS],
    pub max: [u64; RLIM_NLIMITS],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LimitError {
    /// Unknown resource number.
    Invalid,
    /// Soft above hard, or raising the hard limit.
    Perm,
}

impl Limits {
    /// The defaults of a fresh process: `nofile` is the kernel's fd table size,
    /// `stack` 8 MiB soft, `core` 0, the rest unlimited.
    pub fn defaults(max_fd: u64) -> Limits {
        let mut l = Limits { cur: [RLIM_INFINITY; RLIM_NLIMITS], max: [RLIM_INFINITY; RLIM_NLIMITS] };
        l.cur[RLIMIT_STACK as usize] = 8 * 1024 * 1024;
        l.cur[RLIMIT_CORE as usize] = 0;
        l.cur[RLIMIT_NOFILE as usize] = max_fd;
        l.max[RLIMIT_NOFILE as usize] = max_fd;
        l.cur[RLIMIT_NPROC as usize] = 4096;
        l.max[RLIMIT_NPROC as usize] = 4096;
        l
    }

    pub fn get(&self, resource: u32) -> Result<(u64, u64), LimitError> {
        let r = resource as usize;
        if r >= RLIM_NLIMITS {
            return Err(LimitError::Invalid);
        }
        Ok((self.cur[r], self.max[r]))
    }

    /// `setrlimit` for an unprivileged process.
    pub fn set(&mut self, resource: u32, cur: u64, max: u64) -> Result<(), LimitError> {
        let r = resource as usize;
        if r >= RLIM_NLIMITS {
            return Err(LimitError::Invalid);
        }
        if cur > max || max > self.max[r] {
            return Err(LimitError::Perm);
        }
        self.cur[r] = cur;
        self.max[r] = max;
        Ok(())
    }

    /// Would a new descriptor number `fd` exceed `RLIMIT_NOFILE`?
    pub fn fd_exceeds(&self, fd: u64) -> bool {
        fd >= self.cur[RLIMIT_NOFILE as usize]
    }

    /// Would one more task (thread or child) exceed `RLIMIT_NPROC`?
    pub fn tasks_exceed(&self, tasks_now: u64) -> bool {
        tasks_now >= self.cur[RLIMIT_NPROC as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let l = Limits::defaults(1024);
        assert_eq!(l.get(RLIMIT_NOFILE), Ok((1024, 1024)));
        assert_eq!(l.get(RLIMIT_STACK), Ok((8 << 20, RLIM_INFINITY)));
        assert_eq!(l.get(RLIMIT_AS), Ok((RLIM_INFINITY, RLIM_INFINITY)));
        assert_eq!(l.get(99), Err(LimitError::Invalid));
    }

    #[test]
    fn lowering_is_allowed_raising_the_hard_limit_is_not() {
        let mut l = Limits::defaults(1024);
        assert_eq!(l.set(RLIMIT_NOFILE, 8, 64), Ok(()));
        assert_eq!(l.get(RLIMIT_NOFILE), Ok((8, 64)));
        assert!(l.fd_exceeds(8));
        assert!(!l.fd_exceeds(7));
        assert_eq!(l.set(RLIMIT_NOFILE, 32, 64), Ok(())); // soft up to the hard limit is fine
        assert_eq!(l.set(RLIMIT_NOFILE, 32, 128), Err(LimitError::Perm));
        assert_eq!(l.set(RLIMIT_NOFILE, 65, 64), Err(LimitError::Perm));
        assert_eq!(l.set(RLIMIT_NPROC, 2, 2), Ok(()));
        assert!(l.tasks_exceed(2));
        assert!(!l.tasks_exceed(1));
    }
}
