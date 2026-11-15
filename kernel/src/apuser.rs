//! Ring 3 on the application processors: steps S3 to S6 of
//! docs/design/SMP-SCHEDULER.md. This is the test-first state of S3: the mode
//! switch, the per-AP counters and the test pinning exist, nothing is placed
//! yet (every ring-3 task runs on the BSP), so `[ap-user]` fails.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

pub const MAX_CPU: usize = 8;
pub const OFF: u8 = 0;
pub const TEST: u8 = 1;
pub const ON: u8 = 2;
static MODE: AtomicU8 = AtomicU8::new(OFF);

pub fn mode() -> u8 {
    MODE.load(Ordering::Relaxed)
}
pub fn mode_name(m: u8) -> &'static str {
    match m {
        TEST => "test",
        ON => "on",
        _ => "off",
    }
}
pub fn parse_mode(v: &str) -> Option<u8> {
    match v.trim() {
        "off" => Some(OFF),
        "test" => Some(TEST),
        "on" => Some(ON),
        _ => None,
    }
}
pub fn set_mode(m: u8) {
    MODE.store(m, Ordering::Relaxed);
}

pub static ST_BOUNCE_SC: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_BOUNCE_PF: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_BOUNCE_OTHER: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_USER_TSC: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];

/// The APs (here: every online AP; none can run ring 3 yet).
pub fn user_aps() -> alloc::vec::Vec<usize> {
    crate::kwork::online_aps()
}
pub fn apic_of(cpu: usize) -> u8 {
    cpu as u8
}
/// Pinning is not implemented yet: the plan is ignored.
pub fn pin_plan(_cpus: &[usize]) {}
pub fn stat_lines() -> alloc::vec::Vec<alloc::string::String> {
    alloc::vec![alloc::format!("[smpstat] smpuser={} (no AP runs ring 3 yet)", mode_name(mode()))]
}
