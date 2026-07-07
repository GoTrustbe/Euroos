//! Host-driven **serial console** (COM1 input → shell).
//!
//! The UART gained a non-blocking RX path ([`crate::serial::read_byte`]); this module
//! accumulates an input line and runs it through the normal [`crate::shell`], framing the
//! result so a host-side load-test harness can drive EuroOS over the serial line (no GUI /
//! QMP keystrokes needed). The shell has no scripting loops — the loop lives on the host;
//! this is the in-kernel half that turns serial bytes into shell commands.
//!
//! Wire protocol (so the host can synchronise without guessing timings):
//!   • the kernel emits `[scon-ready]` once at startup and after every command completes;
//!   • the echoed command is `[scon] $ <cmd>`; each output line is `[scon] <line>`.
//! The host sends a line terminated by `\n`, then waits for the next `[scon-ready]`.

use alloc::string::String;
use spin::Mutex;

use crate::shell::{self, ShellCtx};

/// Pending input line, assembled from raw COM1 bytes and decoded as UTF-8 at end-of-line
/// (so non-ASCII filenames — Greek, Chinese, emoji — survive instead of being dropped).
static LINE: Mutex<alloc::vec::Vec<u8>> = Mutex::new(alloc::vec::Vec::new());
static STARTED: Mutex<bool> = Mutex::new(false);

const MAX_LINE: usize = 1024; // guard against an unterminated flood

/// Poll COM1 for input and execute any complete command line through the shell.
/// Call once per desktop tick; cheap when idle (RX empty → returns immediately).
pub fn poll(ctx: &mut ShellCtx) {
    // One-time readiness banner so the host harness can sync before the first command.
    {
        let mut s = STARTED.lock();
        if !*s {
            *s = true;
            crate::serial_println!("[scon-ready]"); // serial console accepting commands on COM1
        }
    }

    // Drain up to a bounded number of bytes this tick (don't starve the compositor).
    let mut commands: alloc::vec::Vec<String> = alloc::vec::Vec::new();
    for _ in 0..512 {
        let b = match crate::serial::read_byte() {
            Some(b) => b,
            None => break,
        };
        match b {
            b'\r' | b'\n' => {
                let mut line = LINE.lock();
                // Decode the accumulated bytes as UTF-8 (lossy: a stray byte becomes U+FFFD
                // rather than dropping the whole line) so Unicode command lines work.
                let decoded = alloc::string::String::from_utf8_lossy(&line);
                let s = decoded.trim();
                if !s.is_empty() {
                    commands.push(alloc::string::String::from(s));
                }
                line.clear();
            }
            0x08 | 0x7f => {
                // backspace / DEL (pops one byte; fine for the harness, which sends whole lines)
                LINE.lock().pop();
            }
            // Printable ASCII (0x20–0x7e) AND all UTF-8 continuation/lead bytes (0x80–0xFF):
            // buffer the raw byte; it's decoded as UTF-8 at end-of-line above.
            0x20..=0x7e | 0x80..=0xff => {
                let mut line = LINE.lock();
                if line.len() < MAX_LINE {
                    line.push(b);
                }
            }
            _ => {} // ignore other C0 control bytes
        }
    }

    // Execute the completed command lines (lock on LINE already released).
    for cmd in commands {
        crate::serial_println!("[scon] $ {cmd}");
        for l in shell::exec(ctx, &cmd) {
            crate::serial_println!("[scon] {l}");
        }
        crate::serial_println!("[scon-ready]");
    }
}
