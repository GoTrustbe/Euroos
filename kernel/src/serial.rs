//! COM1 (16550 UART) serial port for debug output.
//!
//! Crucial for kernel bring-up: this also works after `ExitBootServices`, when
//! there is no longer a UEFI console. QEMU captures it with `-serial file:serial.log`.
//! That way, on a black screen we can see exactly how far the kernel got.

use core::sync::atomic::Ordering;
use core::fmt::{self, Write};

use spin::Mutex;
use x86_64::instructions::port::Port;

const COM1: u16 = 0x3F8;

struct Uart {
    data: Port<u8>,
    lsr: Port<u8>,
    /// The quiet filter (see `QUIET`): the first bytes of a line, held until
    /// its tag is known, and what happens with the rest of the line.
    held: [u8; 16],
    held_n: usize,
    line: LineState,
    /// This print goes into the transmit queue instead of the port (see `DEFER`).
    to_queue: bool,
}

/// Deferred console output for the desktop loop (BUG-129).
///
/// A print holds the CPU with interrupts off until the UART has taken every
/// byte. Under a hypervisor each transmitter-load is two exits to the host,
/// and a host that is busy (a screendump, the user-mode network) answers one
/// in milliseconds, not microseconds; a real UART takes a character time per
/// byte. The desktop loop prints a line per DevTools request, per console
/// command, per heartbeat: one pass of it stood for 570 ms that way while a
/// page loaded. Its lines now go into this queue (memory only) and the task
/// `tx_task` sends them, one load at a time, with interrupts on in between.
///
/// Once the desktop loop runs, every print defers (BUG-251), not only task 0's:
/// a direct print polled the line status with interrupts off until the
/// transmitter took each load, and while the host's side of the port did not
/// drain (a screendump, a slow reader of the serial socket) a census line of
/// another task, or a `[tick-late]` line of the timer interrupt, held the CPU
/// for 50 ms to seconds. The queue is one FIFO, so the order of all lines is
/// kept. A fault, a panic, the NMI probe and a shutdown call `emergency()`: the
/// queue is sent at once and nothing is deferred from then on. A print nested
/// in another print on the same CPU (a fault inside a print) still writes
/// past the lock, to the port.
///
/// The queue is only touched with the UART lock held.
const TXQ_CAP: usize = 256 * 1024;
struct TxQ {
    buf: [u8; TXQ_CAP],
    head: usize,
    len: usize,
    /// The line at the head has been sent in part.
    midline: bool,
}
struct TxCell(core::cell::UnsafeCell<TxQ>);
// SAFETY: every access is under the UART lock (see `txq`).
unsafe impl Sync for TxCell {}
static TXQ: TxCell = TxCell(core::cell::UnsafeCell::new(TxQ { buf: [0; TXQ_CAP], head: 0, len: 0, midline: false }));
static DEFER: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static EMERGENCY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static TX_PENDING: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static TX_TASK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(usize::MAX);
/// Bytes that went through the queue, and times it was full (its content was
/// then sent by the printing task itself, as without a queue).
static TX_DEFERRED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static TX_FULL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// Since when the queue holds bytes (TSC; 0 = empty), and the longest stretch
/// in which it was not empty since the last look: an upper bound on how late a
/// line of the desktop loop reached the port.
static TX_BUSY_SINCE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static TX_BUSY_MAX: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// BUG-251: the sender found the transmitter busy (it went to sleep instead of
/// polling with interrupts off), and the longest load with interrupts off (TSC).
static TX_BUSY_SKIPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static TX_LOAD_MAX: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// (times the sender found the transmitter busy and slept, longest load with
/// interrupts off in microseconds since the last call).
pub fn take_tx_stats() -> (u64, u64) {
    let c = TX_LOAD_MAX.swap(0, Ordering::Relaxed);
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    (TX_BUSY_SKIPS.load(Ordering::Relaxed), c * 10_000 / per)
}

/// `serialcost` ([serial-cost], 11f-A): what the console output costs the
/// BSP, the one CPU every ring-3 thread runs on while `smpuser` is off. Queues
/// `kb` KiB of lines, lets the queue drain (kicking it like the desktop loop
/// does), and charges the run time of the desktop loop and of the sender task
/// in that stretch. Under a hypervisor every transmitter load is an exit to the
/// host's device model; measured ~40 us a byte on the BSP while a browser
/// typed (docs/proof/11f-keylat). With application processors taking kernel
/// work the BSP should pay next to nothing: limit 1 ms per KiB; SKIP without
/// APs (one CPU: the cost is unavoidable there).
pub fn cost_test(kb: usize) -> alloc::vec::Vec<alloc::string::String> {
    let kb = kb.clamp(1, 64);
    let tx = TX_TASK.load(Ordering::Relaxed);
    let run = |t: usize| if t == usize::MAX { 0 } else { crate::sched::run_tsc_now(t) };
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    // Let what is queued now go first.
    for _ in 0..200 {
        if TX_PENDING.load(Ordering::Relaxed) == 0 {
            break;
        }
        kick();
        crate::sched::sleep_ticks(1);
        crate::sched::yield_now();
    }
    let (r0, rt) = (run(0), run(tx));
    let (ap0, t0) = (AP_SENT.load(Ordering::Relaxed), unsafe { core::arch::x86_64::_rdtsc() });
    let line = "[serial-cost] filler 0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789abcdefghijklmnopqrstuvwxyz";
    let lines = kb * 1024 / (line.len() + 1);
    for _ in 0..lines {
        crate::serial_println!("{line}");
    }
    let mut waited = 0;
    while TX_PENDING.load(Ordering::Relaxed) > 0 && waited < 3000 {
        kick();
        crate::sched::sleep_ticks(1);
        crate::sched::yield_now();
        waited += 1;
    }
    let drained = waited < 3000;
    let wall = (unsafe { core::arch::x86_64::_rdtsc() } - t0) * 10 / per;
    let bsp = (run(0) - r0 + run(tx) - rt) * 10_000 / per; // us
    let bytes = (lines * (line.len() + 1)) as u64;
    let aps = crate::kwork::accepting() && !crate::kwork::online_aps().is_empty();
    let per_kb = bsp * 1024 / bytes.max(1);
    let verdict = if !aps {
        "SKIP (no application processor takes kernel work)"
    } else if per_kb <= 1000 && drained {
        "OK"
    } else {
        "FAILED"
    };
    let l = alloc::format!(
        "[serial-cost] {bytes} B of console lines: drained in {wall} ms, the BSP spent {} ms on them ({per_kb} us a KiB; limit 1000 with APs), sent by APs {} B -> {verdict}",
        bsp / 1000, AP_SENT.load(Ordering::Relaxed) - ap0);
    crate::serial_println!("{l}");
    alloc::vec![l]
}

/// Bytes an application processor sent (see `ap_drain`).
static AP_SENT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// A drain job is queued or running on an AP; the BSP's sender stays away.
static AP_DRAIN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// When the drain in flight was queued (TSC): the BSP's sender takes over if
/// an AP has not finished it within ten ticks.
static AP_DRAIN_SINCE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// The AP that sends (its core index + 1; 0 = not chosen yet).
static AP_CPU: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Queue a drain of the console on an application processor, if one takes
/// kernel work. True if a drain is (now) on its way.
fn ap_drain_submit() -> bool {
    if EMERGENCY.load(Ordering::Relaxed) || !crate::kwork::accepting() {
        return false;
    }
    if AP_DRAIN.swap(true, Ordering::AcqRel) {
        return true; // the one in flight sends this too
    }
    AP_DRAIN_SINCE.store(unsafe { core::arch::x86_64::_rdtsc() }, Ordering::Relaxed);
    let mut cpu = AP_CPU.load(Ordering::Relaxed);
    if cpu == 0 {
        // The last online AP: the first ones are where kwork's parallel jobs start.
        cpu = crate::kwork::online_aps().last().map(|c| c + 1).unwrap_or(0);
        AP_CPU.store(cpu, Ordering::Relaxed);
    }
    if cpu == 0 || crate::kwork::submit(cpu - 1, alloc::boxed::Box::new(ap_drain)).is_err() {
        AP_DRAIN.store(false, Ordering::Release);
        return false;
    }
    true
}

/// The job: send the queue, one load at a time (interrupts off only for the
/// load, as `drain_load` always does), waiting for the transmitter with a
/// pause between status reads. Bounded to ~20 ms so other kernel work on that
/// AP is not held up; what is left goes with the next kick.
fn ap_drain() {
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    loop {
        match drain_load() {
            0 => break,
            TX_BUSY | TX_OTHER => {
                for _ in 0..200 {
                    core::hint::spin_loop();
                }
            }
            n => {
                AP_SENT.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
        if unsafe { core::arch::x86_64::_rdtsc() } - start > 2 * per {
            break;
        }
    }
    AP_DRAIN.store(false, Ordering::Release);
}

/// The longest time (ms) the queue was not empty since the last call.
pub fn take_queue_delay_ms() -> u64 {
    let c = TX_BUSY_MAX.swap(0, Ordering::Relaxed);
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    c * 10 / per
}

/// SAFETY: the caller holds the UART lock.
#[allow(clippy::mut_from_ref)]
unsafe fn txq() -> &'static mut TxQ {
    &mut *TXQ.0.get()
}

/// Start deferring the desktop loop's output (once: also starts the task).
pub fn set_deferred(on: bool) {
    if on && TX_TASK.load(Ordering::Relaxed) == usize::MAX {
        match crate::sched::spawn_kernel(tx_task, -10) {
            Some(t) => TX_TASK.store(t, Ordering::Relaxed),
            None => return, // no task slot: stay synchronous
        }
    }
    DEFER.store(on, Ordering::Relaxed);
}

/// (deferring?, bytes sent through the queue, times the queue was full, bytes waiting).
pub fn deferred() -> (bool, u64, u64, usize) {
    (DEFER.load(Ordering::Relaxed) && !EMERGENCY.load(Ordering::Relaxed), TX_DEFERRED.load(Ordering::Relaxed),
     TX_FULL.load(Ordering::Relaxed), TX_PENDING.load(Ordering::Relaxed))
}

/// The desktop loop calls this at the end of a pass: send up to 256 bytes of
/// what it queued (its answers to console commands are short lines, and they
/// should not wait for the sender to get a turn), then wake the sender for
/// whatever is left.
pub fn kick() {
    // 11f-A: an application processor that takes kernel work sends the queue
    // instead, and the BSP (every ring-3 thread's CPU) pays nothing for it.
    if TX_PENDING.load(Ordering::Relaxed) > 0 && ap_drain_submit() {
        return;
    }
    let mut sent = 0usize;
    while sent < 256 && TX_PENDING.load(Ordering::Relaxed) > 0 {
        let n = drain_load();
        if n == 0 || n == TX_BUSY || n == TX_OTHER {
            break;
        }
        sent += n;
    }
    if TX_PENDING.load(Ordering::Relaxed) > 0 {
        let t = TX_TASK.load(Ordering::Relaxed);
        if t != usize::MAX {
            let _ = crate::sched::try_unblock_any(t);
        }
    }
}

/// Send everything that waits, now, and defer nothing from here on: for a
/// panic, a fatal fault, the NMI probe, a shutdown. `resume()` undoes it.
pub fn emergency() {
    EMERGENCY.store(true, Ordering::Relaxed);
    x86_64::instructions::interrupts::without_interrupts(|| {
        if PRINTING_CPU.load(Ordering::Acquire) == crate::apic::lapic_id().wrapping_add(1) {
            return; // inside a print of this cpu: the queue is in use below us
        }
        // Another cpu may be appending a line right now: give it a bounded moment instead of losing the queue.
        for _ in 0..200_000 {
            if let Some(mut u) = UART.try_lock() {
                u.finish_queue(true);
                break;
            }
            core::hint::spin_loop();
        }
    });
}

pub fn in_emergency() -> bool {
    EMERGENCY.load(Ordering::Relaxed)
}

pub fn resume() {
    EMERGENCY.store(false, Ordering::Relaxed);
}

/// `drain_load`: the transmitter has not taken the last load yet.
const TX_BUSY: usize = usize::MAX;
/// `drain_load`: another sender (another CPU) is at the port.
const TX_OTHER: usize = usize::MAX - 1;

/// Send one transmitter-load from the queue. Returns the bytes sent (0: the
/// queue is empty, `TX_BUSY`: the transmitter is still busy). Interrupts are
/// off only for this one load, and it never waits for the transmitter: one
/// status read, then one string output of at most a FIFO's worth (BUG-251).
/// It used to poll the line status with interrupts off until the transmitter
/// was empty. Under a hypervisor each poll is an exit that takes the device
/// lock of the host, and while the host's side of the port does not drain (a
/// screendump, a slow reader of the serial socket) that loop held the CPU with
/// interrupts off for 50 ms to seconds and kept the host's own device thread
/// from the lock it needed to drain the port.
///
/// The port accesses happen outside the UART lock: the lock only guards the
/// queue in memory, so a print (which only appends to the queue) never waits
/// for a port access that the host is slow to answer. `TX_PORT` keeps the
/// senders (this function on any cpu) from interleaving their loads.
fn drain_load() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if TX_PENDING.load(Ordering::Relaxed) == 0 {
            return 0;
        }
        if TX_PORT.swap(true, Ordering::Acquire) {
            return TX_OTHER; // another sender is at the port
        }
        let t0 = unsafe { core::arch::x86_64::_rdtsc() };
        // SAFETY: a read of the line status register.
        let n = if unsafe { Port::<u8>::new(COM1 + 5).read() } & 0x20 == 0 {
            TX_BUSY_SKIPS.fetch_add(1, Ordering::Relaxed);
            TX_BUSY
        } else {
            let mut out = [0u8; 16];
            let taken = match UART.try_lock() {
                Some(mut u) => u.take_load(&mut out),
                None => Err(1), // a print is appending: next time
            };
            match taken {
                Ok((o, n)) => {
                    Uart::burst_out(&out[..o]);
                    n
                }
                Err(n) => n,
            }
        };
        TX_PORT.store(false, Ordering::Release);
        let d = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0);
        TX_LOAD_MAX.fetch_max(d, Ordering::Relaxed);
        n
    })
}

/// A sender is at the port outside the UART lock (`drain_load`).
static TX_PORT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The sender of the deferred output. Sleeps two ticks when there is nothing
/// (and is woken earlier by `kick`), one tick when the transmitter is busy.
extern "C" fn tx_task() -> ! {
    loop {
        // An AP sends the queue (11f-A): look again in two ticks, the fallback
        // for output nobody kicks.
        let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
        let late = AP_DRAIN.load(Ordering::Acquire)
            && unsafe { core::arch::x86_64::_rdtsc() }.saturating_sub(AP_DRAIN_SINCE.load(Ordering::Relaxed)) > 10 * per;
        // Output that came after the last kick goes to the AP as well: the
        // sender woke for it on the BSP and paid 18-36 ms in a key's window.
        if !late && (AP_DRAIN.load(Ordering::Acquire) || (TX_PENDING.load(Ordering::Relaxed) > 0 && ap_drain_submit())) {
            crate::sched::sleep_ticks(2);
            crate::sched::yield_now();
            continue;
        }
        match drain_load() {
            0 => {
                crate::sched::sleep_ticks(2);
                crate::sched::yield_now();
            }
            TX_BUSY | TX_OTHER => {
                crate::sched::sleep_ticks(1);
                crate::sched::yield_now();
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LineState { Start, Pass, Mute }

/// Quiet console: lines of the per-event diagnostic tags below go to the
/// kernel log (`dmesg`, the panic dump) but not to the UART. Off during boot
/// and the self-tests; on when the desktop loop starts, unless the boot option
/// `serial.chatter=on` asks for everything (the shell command `chatter on|off`
/// switches at run time).
///
/// Why: a browser session wrote a megabyte of these lines (one per mmap, per
/// epoll_ctl, per DevTools event). Every sixteen bytes are two exits to the
/// host under a hypervisor and a character time of waiting on a real UART,
/// with interrupts off; the desktop loop, which prints the DevTools lines
/// itself, stood for up to eighteen seconds while GitHub loaded (BUG-126).
static QUIET: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Bytes kept from the UART by the quiet filter.
static MUTED_BYTES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn set_quiet(on: bool) {
    QUIET.store(on, Ordering::Relaxed);
}

pub fn quiet() -> (bool, u64) {
    (QUIET.load(Ordering::Relaxed), MUTED_BYTES.load(Ordering::Relaxed))
}

/// Is this the start of a line that stays out of the UART in quiet mode?
fn quiet_tag(head: &[u8]) -> bool {
    matches!(head,
        b"[linux-abi]" | b"[cst]" | b"[scm]" | b"[fsdiag]" | b"[inet]" | b"[slife]"
        | b"[cdp-in]" | b"[epctl]" | b"[glibc-thread]" | b"[mmaplib]" | b"[futex]"
        | b"[threads]" | b"[pollset]" | b"[madvise]" | b"[epoll]" | b"[sio]" | b"[msc]"
        | b"[sockfam]" | b"[sc]" | b"[cdp] <-" | b"[cdp] ->")
}

/// How many bytes the transmitter takes once it reports "empty": 16 with a
/// 16550A FIFO (found at init), 1 without one.
static TX_BURST: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(1);

impl Uart {
    const fn new() -> Self {
        Self {
            data: Port::new(COM1),
            lsr: Port::new(COM1 + 5),
            held: [0; 16],
            held_n: 0,
            line: LineState::Start,
            to_queue: false,
        }
    }

    fn init(&mut self) {
        let mut ier = Port::<u8>::new(COM1 + 1);
        let mut fcr = Port::<u8>::new(COM1 + 2);
        let mut lcr = Port::<u8>::new(COM1 + 3);
        let mut mcr = Port::<u8>::new(COM1 + 4);
        unsafe {
            ier.write(0x00); // interrupts off
            lcr.write(0x80); // DLAB on
            self.data.write(0x03); // divisor lo = 3 (38400 baud)
            ier.write(0x00); // divisor hi
            lcr.write(0x03); // 8N1, DLAB off
            fcr.write(0xC7); // FIFO on, clear, 14-byte threshold
            mcr.write(0x0B); // RTS/DSR/OUT2
            // A 16550A answers "FIFOs enabled" in the top two bits of the
            // interrupt identification register (same port as FCR, read).
            let fifo = fcr.read() & 0xC0 == 0xC0;
            TX_BURST.store(if fifo { 16 } else { 1 }, Ordering::Relaxed);
        }
    }

    /// Send up to one transmitter-load of `bytes`; returns how many went out.
    /// With the FIFO on, "transmitter holding register empty" (LSR bit 5)
    /// means the FIFO is empty, so sixteen bytes fit without asking again:
    /// one status read and one string output instead of two port accesses per
    /// byte. Under a hypervisor every port access is an exit to the host; on
    /// hardware every status poll waits for a character time. This was the
    /// largest single cost of a busy system with a talkative log (BUG-123).
    fn write_burst(&mut self, bytes: &[u8]) -> usize {
        let n = bytes.len().min(TX_BURST.load(Ordering::Relaxed));
        if n == 0 {
            return 0;
        }
        unsafe {
            while self.lsr.read() & 0x20 == 0 {}
            core::arch::asm!(
                "rep outsb",
                in("dx") COM1,
                inout("rsi") bytes.as_ptr() => _,
                inout("rcx") n => _,
                options(nostack, preserves_flags, readonly)
            );
        }
        n
    }

    fn write_byte(&mut self, b: u8) {
        unsafe {
            // Wait until the transmit-holding register is empty (LSR bit 5).
            while self.lsr.read() & 0x20 == 0 {}
            self.data.write(b);
        }
    }

    /// Non-blocking read of one byte from the UART; `None` if no data is ready
    /// (LSR bit 0 = Data Ready). Powers the host-driven serial console (COM1 input).
    fn read_byte(&mut self) -> Option<u8> {
        unsafe {
            if self.lsr.read() & 0x01 != 0 {
                Some(self.data.read())
            } else {
                None
            }
        }
    }
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}

impl Uart {
    /// The quiet filter: pass `bytes` on to the transmitter, except the lines
    /// whose tag is in `quiet_tag`. The state lives across calls (one line
    /// arrives in pieces from the formatter) and is guarded by the UART lock.
    fn write_filtered(&mut self, b: &[u8]) {
        let mut i = 0;
        while i < b.len() {
            match self.line {
                LineState::Pass | LineState::Mute => {
                    let end = b[i..].iter().position(|&c| c == b'\n').map(|p| i + p + 1).unwrap_or(b.len());
                    if self.line == LineState::Pass {
                        self.write_bytes(&b[i..end]);
                    } else {
                        MUTED_BYTES.fetch_add((end - i) as u64, Ordering::Relaxed);
                    }
                    if b[end - 1] == b'\n' {
                        self.line = LineState::Start;
                        self.held_n = 0;
                    }
                    i = end;
                }
                LineState::Start => {
                    let c = b[i];
                    if self.held_n == 0 && c != b'[' {
                        self.line = LineState::Pass; // no tag: this byte goes out above
                        continue;
                    }
                    i += 1;
                    self.held[self.held_n] = c;
                    self.held_n += 1;
                    let n = self.held_n;
                    // "[cdp]" needs three more bytes: only its raw message
                    // lines ("[cdp] <-", "[cdp] ->") are quiet.
                    let cdp = n >= 5 && &self.held[..5] == b"[cdp]";
                    let known = c == b'\n' || n == self.held.len() || (c == b']' && !cdp) || (cdp && n == 8);
                    if !known {
                        continue;
                    }
                    let held = self.held;
                    if c != b'\n' && quiet_tag(&held[..n]) {
                        MUTED_BYTES.fetch_add(n as u64, Ordering::Relaxed);
                        self.line = LineState::Mute;
                    } else {
                        self.write_bytes(&held[..n]);
                        self.line = if c == b'\n' { LineState::Start } else { LineState::Pass };
                    }
                    self.held_n = 0;
                }
            }
        }
    }

    /// Take up to `max` bytes off the queue, at most to the end of a line,
    /// and send them. The caller holds the UART lock. Returns the bytes sent.
    fn send_from_queue(&mut self, max: usize) -> usize {
        let mut tmp = [0u8; 256];
        let max = max.min(tmp.len());
        // SAFETY: UART lock held (we are a method of the locked Uart).
        let q = unsafe { txq() };
        let mut n = 0;
        while n < max && q.len > 0 {
            let b = q.buf[q.head];
            q.head = (q.head + 1) % TXQ_CAP;
            q.len -= 1;
            tmp[n] = b;
            n += 1;
            if b == b'\n' {
                break;
            }
        }
        if n == 0 {
            return 0;
        }
        q.midline = tmp[n - 1] != b'\n';
        TX_PENDING.store(q.len, Ordering::Relaxed);
        self.write_direct(&tmp[..n]);
        if q.len == 0 {
            let since = TX_BUSY_SINCE.swap(0, Ordering::Relaxed);
            if since != 0 {
                let d = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(since);
                TX_BUSY_MAX.fetch_max(d, Ordering::Relaxed);
            }
        }
        n
    }

    /// Take one load off the queue into `out`: `Ok((bytes in out, queued bytes
    /// taken))`, or `Err(n)` with what `drain_load` returns (0: empty; without a
    /// FIFO the byte was sent here, as before). The caller found the
    /// transmitter empty and holds the UART lock; it sends `out` after it let
    /// the lock go.
    fn take_load(&mut self, out: &mut [u8; 16]) -> Result<(usize, usize), usize> {
        // SAFETY: UART lock held (we are a method of the locked Uart).
        let q = unsafe { txq() };
        if q.len == 0 {
            return Err(0);
        }
        let room = TX_BURST.load(Ordering::Relaxed).min(16);
        if room < 2 {
            // No FIFO (a load is one byte): one byte at a time, as before.
            return Err(self.send_from_queue(1));
        }
        let mut o = 0;
        let mut n = 0;
        while q.len > 0 {
            let b = q.buf[q.head];
            let need = if b == b'\n' { 2 } else { 1 };
            if o + need > room {
                break;
            }
            if b == b'\n' {
                out[o] = b'\r';
                out[o + 1] = b'\n';
                o += 2;
            } else {
                out[o] = b;
                o += 1;
            }
            q.head = (q.head + 1) % TXQ_CAP;
            q.len -= 1;
            n += 1;
            if b == b'\n' {
                break;
            }
        }
        if n == 0 {
            return Err(TX_BUSY);
        }
        q.midline = out[o - 1] != b'\n';
        TX_PENDING.store(q.len, Ordering::Relaxed);
        if q.len == 0 {
            let since = TX_BUSY_SINCE.swap(0, Ordering::Relaxed);
            if since != 0 {
                let d = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(since);
                TX_BUSY_MAX.fetch_max(d, Ordering::Relaxed);
            }
        }
        Ok((o, n))
    }

    /// One string output of `bytes` (at most a transmitter-load) to the data
    /// register, without looking at the line status (the caller did).
    fn burst_out(bytes: &[u8]) {
        // SAFETY: COM1 data port; `bytes` is a valid buffer of that length.
        unsafe {
            core::arch::asm!(
                "rep outsb",
                in("dx") COM1,
                inout("rsi") bytes.as_ptr() => _,
                inout("rcx") bytes.len() => _,
                options(nostack, preserves_flags, readonly)
            );
        }
    }

    /// Send the whole queue (`all`), or only the rest of a line that was sent
    /// in part, so that a direct print does not land in the middle of it.
    fn finish_queue(&mut self, all: bool) {
        loop {
            // SAFETY: UART lock held.
            let q = unsafe { txq() };
            if q.len == 0 || !(all || q.midline) {
                break;
            }
            self.send_from_queue(256);
        }
    }

    fn queue_push(&mut self, bytes: &[u8]) {
        // SAFETY: UART lock held.
        if unsafe { txq() }.len + bytes.len() > TXQ_CAP {
            // Full: the printer sends the backlog itself (nothing is dropped).
            TX_FULL.fetch_add(1, Ordering::Relaxed);
            self.finish_queue(true);
            if bytes.len() > TXQ_CAP {
                self.write_direct(bytes);
                return;
            }
        }
        let q = unsafe { txq() };
        if q.len == 0 && !bytes.is_empty() {
            TX_BUSY_SINCE.store(unsafe { core::arch::x86_64::_rdtsc() }, Ordering::Relaxed);
        }
        let mut tail = (q.head + q.len) % TXQ_CAP;
        for &b in bytes {
            q.buf[tail] = b;
            tail = (tail + 1) % TXQ_CAP;
        }
        q.len += bytes.len();
        TX_PENDING.store(q.len, Ordering::Relaxed);
        TX_DEFERRED.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        if self.to_queue {
            self.queue_push(bytes);
        } else {
            self.write_direct(bytes);
        }
    }

    fn write_direct(&mut self, bytes: &[u8]) {
        // "\n" goes out as "\r\n"; everything between two newlines goes out in
        // transmitter-loads.
        let mut rest = bytes;
        while !rest.is_empty() {
            let line = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
            let mut run = &rest[..line];
            while !run.is_empty() {
                let n = self.write_burst(run);
                run = &run[n..];
            }
            if line < rest.len() {
                let mut crlf: &[u8] = b"\r\n";
                while !crlf.is_empty() {
                    let n = self.write_burst(crlf);
                    crlf = &crlf[n..];
                }
                rest = &rest[line + 1..];
            } else {
                rest = &rest[line..];
            }
        }
    }
}

static UART: Mutex<Uart> = Mutex::new(Uart::new());

pub fn init() {
    UART.lock().init();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    // Tee: write to the UART and to the kmsg ring (S1 observability), so that
    // `dmesg` and the panic handler have the recent history. Lock order is
    // always UART -> RING (klog::tee takes no UART lock), so no deadlock.
    //
    // Interrupts OFF while the UART lock is held (BUG-007 class): interrupt
    // handlers print too (the xHCI MSI-X harvest logs its first reports); if one
    // preempts a task mid-print, it spins forever on this lock with interrupts
    // disabled — a silent boot hang. With the lock only ever held under IF=0,
    // an IRQ-context print can never see it taken on this CPU.
    //
    // Re-entrancy on the SAME cpu is the case IF=0 cannot cover: a page fault
    // while the line is being formatted (an argument that reads user memory), or
    // an NMI, runs a handler that prints too, and that print would spin forever
    // on a lock its own cpu holds. Run 37 on the NUC wedged exactly there
    // (serial::_print+0x2a, IF=0, on a fork child's CR3), and the NMI probe
    // printed nothing for the same reason. A nested print writes past the lock:
    // the holder is suspended underneath, so the port is free in practice, and a
    // garbled line beats a dead machine. It skips the kmsg tee (the ring lock
    // may be held by the same suspended frame).
    x86_64::instructions::interrupts::without_interrupts(|| {
        let me = crate::apic::lapic_id().wrapping_add(1);
        if PRINTING_CPU.load(Ordering::Acquire) == me {
            // A second handle on the same port: the Uart is only the two port
            // numbers, the outer frame holding the lock cannot run until we return,
            // and the UART registers tolerate the interleaving.
            let mut uart = Uart::new();
            let _ = uart.write_fmt(args);
            return;
        }
        // A bounded wait: the holder is one task on this core with interrupts off,
        // so a lock still taken after 200 million spins belongs to a task that died
        // or slept while printing (runs 37 and 38 sat here forever, on a fork
        // child's CR3 and on the boot CR3). Force it open, say whose it was, and
        // go on: the next run names the path that leaves the lock behind.
        let mut spins = 0u64;
        let mut uart = loop {
            if let Some(g) = UART.try_lock() {
                break g;
            }
            core::hint::spin_loop();
            spins += 1;
            if spins == 200_000_000 {
                let holder = PRINTING_TASK.load(Ordering::Relaxed);
                let cpu = PRINTING_CPU.load(Ordering::Relaxed);
                // SAFETY: the holder cannot be running (single core, IF=0 here) and
                // will never release; the port is idle.
                unsafe { UART.force_unlock() };
                let mut u = Uart::new();
                let _ = u.write_fmt(format_args!(
                    "\n[serial] UART lock held by task {holder} (cpu {cpu}) through 200M spins: forced open, the holder died or slept while printing\n"
                ));
            }
        };
        PRINTING_CPU.store(me, Ordering::Release);
        // Lock-free on purpose: the census prints with SCHED held, and current()
        // takes SCHED (run 39 wedged in exactly that nested print).
        let task = crate::sched::current_lockfree();
        PRINTING_TASK.store(task, Ordering::Relaxed);
        // Every print goes into the queue once the desktop loop defers (BUG-129,
        // BUG-251); before that, and in an emergency, a print goes to the port,
        // after the whole queue in an emergency.
        let emergency = EMERGENCY.load(Ordering::Relaxed);
        let defer = !emergency && DEFER.load(Ordering::Relaxed);
        if !defer {
            uart.finish_queue(emergency);
        }
        uart.to_queue = defer;
        struct Tee<'a>(&'a mut Uart);
        impl Write for Tee<'_> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                if QUIET.load(Ordering::Relaxed) {
                    self.0.write_filtered(s.as_bytes());
                } else {
                    self.0.write_str(s)?;
                }
                crate::klog::tee(s);
                Ok(())
            }
        }
        let _ = Tee(&mut uart).write_fmt(args);
        uart.to_queue = false;
        PRINTING_CPU.store(0, Ordering::Release);
    });
}

/// LAPIC id + 1 of the cpu that holds the UART lock inside `_print` (0 = none),
/// so a print nested on the same cpu can tell "held by me, suspended" from "held
/// by another core, about to be released".
static PRINTING_CPU: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Scheduler task index of that holder, for the forced-open message.
static PRINTING_TASK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Non-blocking read of one input byte from COM1 (`None` if nothing pending).
/// Used by the host-driven serial console to stream shell commands in.
pub fn read_byte() -> Option<u8> {
    // Interrupts off while the lock is held, like _print: the desktop loop polls
    // this with interrupts enabled, and an interrupt handler that prints (the
    // xHCI harvest logs every keyboard report) found the lock taken by the very
    // task it interrupted and spun on it with interrupts off. That was the wedge
    // "right after the first keystrokes" of runs 3, 12, 20, 28, 29 and 41 (named
    // by the forced-open message: holder task 0, no printer recorded).
    x86_64::instructions::interrupts::without_interrupts(|| {
        UART.try_lock().and_then(|mut u| u.read_byte())
    })
}

/// Write raw bytes DIRECTLY to the UART (panic-safe: `try_lock`, and no
/// tee back to the ring — prevents re-locking RING during a panic dump).
pub fn write_raw(bytes: &[u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(mut uart) = UART.try_lock() {
            uart.finish_queue(true);
            for &b in bytes {
                if b == b'\n' {
                    uart.write_byte(b'\r');
                }
                uart.write_byte(b);
            }
        }
    });
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
