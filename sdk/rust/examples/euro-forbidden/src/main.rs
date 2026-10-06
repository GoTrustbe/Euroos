//! euro-forbidden: a native app that tries ONE Linux syscall (open(2), number 2).
//! Under the euro profile that is not a call, it is the end of the process:
//! the kernel's `[euroapp]` self-test expects this binary to be killed with
//! SIGSYS before the handler ever sees the path. It logs first, so a log line
//! without a kill is the failure signature.
#![no_std]
#![no_main]

fn main() -> i32 {
    let _ = eurosdk::log("euro-forbidden: about to call Linux open(2); the euro profile must kill me");
    let path = b"/etc/euroid/users.db\0";
    let r: u64;
    unsafe {
        core::arch::asm!("syscall", inlateout("rax") 2u64 => r, in("rdi") path.as_ptr(), in("rsi") 0u64, in("rdx") 0u64,
            lateout("rcx") _, lateout("r11") _, options(nostack));
    }
    let _ = eurosdk::log(if (r as i64) < 0 { "euro-forbidden: open was REFUSED but I am still alive (FAIL)" } else { "euro-forbidden: open SUCCEEDED (FAIL)" });
    7
}

eurosdk::entry!(main);
