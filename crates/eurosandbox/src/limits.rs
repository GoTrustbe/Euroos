//! **ResourceLimits** for a container (3F-1) — the enforcement half that was
//! missing: a container may declare a memory / process-count / CPU-time /
//! wall-time ceiling, and [`Usage`] accounting refuses the operation that would
//! cross it. Pure logic so the accounting is host-tested.

/// Declared ceilings. `0` on a field means "no limit for that dimension".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceLimits {
    pub max_mem_bytes: u64,
    pub max_pids: u32,
    pub max_cpu_ms: u64,
    pub max_wall_ms: u64,
}

impl ResourceLimits {
    pub fn new(max_mem_bytes: u64, max_pids: u32, max_cpu_ms: u64, max_wall_ms: u64) -> Self {
        Self { max_mem_bytes, max_pids, max_cpu_ms, max_wall_ms }
    }
}

/// Live usage of a running container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub mem_bytes: u64,
    pub pids: u32,
    pub cpu_ms: u64,
    pub wall_ms: u64,
}

/// Which ceiling an operation would breach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitBreach {
    Memory,
    Pids,
    Cpu,
    Wall,
}

impl Usage {
    /// Would adding `mem`/`pids` keep us within `limits`? Returns the breached
    /// dimension if not. Does not mutate — call [`Self::charge`] to commit.
    pub fn check_alloc(&self, limits: &ResourceLimits, mem: u64, pids: u32) -> Result<(), LimitBreach> {
        if limits.max_mem_bytes != 0 && self.mem_bytes + mem > limits.max_mem_bytes {
            return Err(LimitBreach::Memory);
        }
        if limits.max_pids != 0 && self.pids + pids > limits.max_pids {
            return Err(LimitBreach::Pids);
        }
        Ok(())
    }

    /// Commit an allocation after [`Self::check_alloc`] passed.
    pub fn charge(&mut self, mem: u64, pids: u32) {
        self.mem_bytes += mem;
        self.pids += pids;
    }

    /// Release memory / exited processes.
    pub fn release(&mut self, mem: u64, pids: u32) {
        self.mem_bytes = self.mem_bytes.saturating_sub(mem);
        self.pids = self.pids.saturating_sub(pids);
    }

    /// Advance the CPU/wall clocks; returns the breached ceiling if either is now
    /// exceeded (the caller should stop/kill the container).
    pub fn tick(&mut self, limits: &ResourceLimits, cpu_ms: u64, wall_ms: u64) -> Result<(), LimitBreach> {
        self.cpu_ms += cpu_ms;
        self.wall_ms += wall_ms;
        if limits.max_cpu_ms != 0 && self.cpu_ms > limits.max_cpu_ms {
            return Err(LimitBreach::Cpu);
        }
        if limits.max_wall_ms != 0 && self.wall_ms > limits.max_wall_ms {
            return Err(LimitBreach::Wall);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_ceiling_enforced() {
        let lim = ResourceLimits::new(1024, 0, 0, 0);
        let mut u = Usage::default();
        assert!(u.check_alloc(&lim, 1000, 0).is_ok());
        u.charge(1000, 0);
        // 1000 + 100 > 1024 → refused, and usage is unchanged.
        assert_eq!(u.check_alloc(&lim, 100, 0), Err(LimitBreach::Memory));
        assert_eq!(u.mem_bytes, 1000);
        // Freeing memory makes room again.
        u.release(500, 0);
        assert!(u.check_alloc(&lim, 100, 0).is_ok());
    }

    #[test]
    fn pid_ceiling_enforced() {
        let lim = ResourceLimits::new(0, 4, 0, 0);
        let mut u = Usage::default();
        u.charge(0, 4);
        assert_eq!(u.check_alloc(&lim, 0, 1), Err(LimitBreach::Pids));
        u.release(0, 1);
        assert!(u.check_alloc(&lim, 0, 1).is_ok());
    }

    #[test]
    fn cpu_and_wall_ceilings_trip() {
        let lim = ResourceLimits::new(0, 0, 100, 1000);
        let mut u = Usage::default();
        assert!(u.tick(&lim, 90, 500).is_ok());
        assert_eq!(u.tick(&lim, 20, 100), Err(LimitBreach::Cpu)); // 110 > 100
        let mut u2 = Usage::default();
        assert_eq!(u2.tick(&lim, 10, 2000), Err(LimitBreach::Wall));
    }

    #[test]
    fn zero_means_unlimited() {
        let lim = ResourceLimits::default();
        let u = Usage::default();
        assert!(u.check_alloc(&lim, u64::MAX / 2, 100_000).is_ok());
    }
}
