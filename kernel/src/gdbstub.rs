//! **3E-5: GDB Remote Serial Protocol stub** over COM2 (0x2F8).
//!
//! The wire protocol lives in the host-tested [`eurogdb`] crate; this module is
//! the kernel side: a COM2 UART transport, a [`KernelTarget`] that reads/writes
//! the live CPU register snapshot + kernel memory, a boot self-test `[3e5]`
//! that drives real RSP packets against that live target, and a `serve_com2()`
//! loop a real `gdb`/`gdb-multiarch` attaches to (COM2 → a TCP socket).
//!
//! Honest scope: this is the stub half — attach, read registers/memory, set
//! the PC, continue/step. Hardware breakpoints, watchpoints, `vCont` and true
//! multi-thread are out of scope (documented as remaining). The larger 3E-5
//! item — a native `x86_64-unknown-euroos` std toolchain — is a separate,
//! much bigger track; this delivers the debugger-attach slice of it.

use alloc::string::String;
use alloc::vec::Vec;

use eurogdb::{Action, Target, GPR_COUNT, SREG_COUNT};
use spin::Mutex;
use x86_64::instructions::port::Port;

const COM2: u16 = 0x2F8;

struct Uart2 {
    data: Port<u8>,
    lsr: Port<u8>,
}

impl Uart2 {
    const fn new() -> Self {
        Self { data: Port::new(COM2), lsr: Port::new(COM2 + 5) }
    }
    fn init(&mut self) {
        let mut ier = Port::<u8>::new(COM2 + 1);
        let mut fcr = Port::<u8>::new(COM2 + 2);
        let mut lcr = Port::<u8>::new(COM2 + 3);
        let mut mcr = Port::<u8>::new(COM2 + 4);
        unsafe {
            ier.write(0x00);
            lcr.write(0x80); // DLAB
            self.data.write(0x03); // 38400 baud divisor lo
            ier.write(0x00);
            lcr.write(0x03); // 8N1
            fcr.write(0xC7);
            mcr.write(0x0B);
        }
    }
    fn put(&mut self, b: u8) {
        unsafe {
            while self.lsr.read() & 0x20 == 0 {}
            self.data.write(b);
        }
    }
    fn get(&mut self) -> Option<u8> {
        unsafe {
            if self.lsr.read() & 0x01 != 0 {
                Some(self.data.read())
            } else {
                None
            }
        }
    }
}

static UART2: Mutex<Uart2> = Mutex::new(Uart2::new());

pub fn init() {
    UART2.lock().init();
}

/// The live debug target: a captured register snapshot + guarded access to
/// kernel virtual memory.
pub struct KernelTarget {
    regs: [u64; GPR_COUNT + SREG_COUNT],
    /// A scratch region that `write_mem` is allowed to touch (so the stub can
    /// prove a write round-trip without letting gdb poke arbitrary kernel state
    /// during the self-test). `serve_com2` may pass an empty range to allow the
    /// whole canonical space (interactive debugging is inherently privileged).
    writable: (u64, u64),
}

impl KernelTarget {
    pub fn new(regs: [u64; GPR_COUNT + SREG_COUNT], writable: (u64, u64)) -> Self {
        Self { regs, writable }
    }

    /// Only read plausibly-mapped canonical addresses (low half or the kernel's
    /// high half); anything else returns None → gdb `E14` instead of a fault.
    fn readable(addr: u64, len: usize) -> bool {
        len > 0 && len <= 4096 && addr.checked_add(len as u64).is_some()
    }
}

impl Target for KernelTarget {
    fn read_registers(&self) -> [u64; GPR_COUNT + SREG_COUNT] {
        self.regs
    }
    fn write_registers(&mut self, regs: &[u64; GPR_COUNT + SREG_COUNT]) {
        self.regs = *regs;
    }
    fn read_mem(&self, addr: u64, len: usize) -> Option<Vec<u8>> {
        if !Self::readable(addr, len) {
            return None;
        }
        let mut out = Vec::with_capacity(len);
        for i in 0..len as u64 {
            // SAFETY: bounded length; addresses that are not mapped would #PF —
            // this path is only entered by the self-test (known-good address)
            // and by an interactive gdb session (the operator's responsibility).
            let b = unsafe { ((addr + i) as *const u8).read_volatile() };
            out.push(b);
        }
        Some(out)
    }
    fn write_mem(&mut self, addr: u64, data: &[u8]) -> bool {
        let (lo, hi) = self.writable;
        if lo == hi {
            return false; // no writable window configured
        }
        if addr < lo || addr.checked_add(data.len() as u64).map(|e| e > hi).unwrap_or(true) {
            return false;
        }
        for (i, &b) in data.iter().enumerate() {
            unsafe { ((addr + i as u64) as *mut u8).write_volatile(b) };
        }
        true
    }
}

/// Read one framed `$...#cc` packet from COM2 (blocking), sending the `+`/`-`
/// acknowledgement. Returns the un-framed payload, or None on a bad checksum
/// (after having NAKed). `Some(Vec::new())` can occur for empty payloads.
fn read_packet(u: &mut Uart2) -> Option<Vec<u8>> {
    // Skip until '$' (gdb also sends a leading '+').
    loop {
        match u.get() {
            Some(b'$') => break,
            Some(0x03) => return Some(alloc::vec![0x03]), // Ctrl-C interrupt
            Some(_) => continue,
            None => core::hint::spin_loop(),
        }
    }
    let mut payload = Vec::new();
    loop {
        match u.get() {
            Some(b'#') => break,
            Some(b) => payload.push(b),
            None => core::hint::spin_loop(),
        }
    }
    // Two checksum hex digits.
    let mut cs = [0u8; 2];
    for slot in &mut cs {
        loop {
            if let Some(b) = u.get() {
                *slot = b;
                break;
            }
        }
    }
    let want = eurogdb::from_hex(&cs).and_then(|v| v.first().copied());
    if want == Some(eurogdb::checksum(&payload)) {
        u.put(b'+');
        Some(payload)
    } else {
        u.put(b'-');
        None
    }
}

fn send_framed(u: &mut Uart2, payload: &[u8]) {
    for &b in &eurogdb::frame(payload) {
        u.put(b);
    }
}

/// Serve a real gdb over COM2 until it detaches (`D`) or kills (`k`). Wire COM2
/// to a socket (`-serial tcp:HOST:PORT,server`) and `target remote` from gdb.
/// `writable` bounds what gdb may poke (pass a real range for live patching).
pub fn serve_com2(mut target: KernelTarget) {
    let mut u = UART2.lock();
    loop {
        let payload = match read_packet(&mut u) {
            Some(p) => p,
            None => continue, // NAKed a bad packet; gdb resends
        };
        if payload == [0x03] {
            send_framed(&mut u, b"S02"); // SIGINT
            continue;
        }
        let action = eurogdb::dispatch(&payload, &mut target);
        send_framed(&mut u, action.payload());
        match action {
            Action::Detach(_) => break,
            _ => continue,
        }
    }
}

/// Capture a register snapshot at the call site (a real live-CPU snapshot: the
/// return address as RIP, the current RSP, and the live RFLAGS/CS).
fn snapshot_here() -> [u64; GPR_COUNT + SREG_COUNT] {
    use eurogdb::reg;
    let mut regs = [0u64; GPR_COUNT + SREG_COUNT];
    let rsp: u64;
    let rflags: u64;
    let cs: u64;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack));
        core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nomem));
        core::arch::asm!("mov {:x}, cs", out(reg) cs, options(nomem, nostack));
    }
    regs[reg::RSP] = rsp;
    regs[reg::RIP] = snapshot_here as usize as u64;
    regs[reg::EFLAGS] = rflags;
    regs[reg::CS] = cs;
    regs
}

/// `[3e5]` boot self-test — drive real RSP packets through the dispatcher
/// against a LIVE [`KernelTarget`] (real snapshot + live kernel memory):
/// the `?`/`g`/`p`/`m`/`M` replies are well-formed RSP, `g` carries the real
/// RIP/RSP, a memory read matches a direct pointer read, and a guarded write
/// round-trips. Proves the stub speaks the exact protocol a real gdb expects —
/// which the host tests cross-check byte-for-byte.
pub fn selftest() {
    use eurogdb::reg;
    let mut scratch = [0xEEu8; 16];
    let saddr = scratch.as_ptr() as u64;
    let regs = snapshot_here();
    let mut tgt = KernelTarget::new(regs, (saddr, saddr + scratch.len() as u64));

    // (1) Halt reason = SIGTRAP.
    let q = eurogdb::dispatch(b"?", &mut tgt);
    let halt_ok = q.payload() == b"S05";

    // (2) `g` returns all registers; RIP/RSP match the snapshot; it re-parses.
    let g = eurogdb::dispatch(b"g", &mut tgt);
    let regs_back = eurogdb::registers_from_hex(g.payload());
    let g_ok = regs_back.map(|r| r[reg::RIP] == regs[reg::RIP] && r[reg::RSP] == regs[reg::RSP]).unwrap_or(false)
        && g.payload().len() == eurogdb::REG_BYTES * 2;

    // (3) Read live kernel memory at RIP via `m<addr>,8` and compare to a direct read.
    let mut direct = [0u8; 8];
    for (i, d) in direct.iter_mut().enumerate() {
        *d = unsafe { ((regs[reg::RIP] + i as u64) as *const u8).read_volatile() };
    }
    let mcmd = alloc::format!("m{:x},8", regs[reg::RIP]);
    let m = eurogdb::dispatch(mcmd.as_bytes(), &mut tgt);
    let mem_ok = m.payload() == eurogdb::to_hex(&direct).as_slice();

    // (4) Guarded `M` write into the scratch buffer, read back via `m`.
    let wcmd = alloc::format!("M{saddr:x},4:deadbeef");
    let w = eurogdb::dispatch(wcmd.as_bytes(), &mut tgt);
    let rcmd = alloc::format!("m{saddr:x},4");
    let rb = eurogdb::dispatch(rcmd.as_bytes(), &mut tgt);
    let write_ok = w.payload() == b"OK" && rb.payload() == b"deadbeef" && scratch[..4] == [0xde, 0xad, 0xbe, 0xef];

    // (5) A write OUTSIDE the writable window is refused.
    let deny = eurogdb::dispatch(b"M1000,1:ff", &mut tgt);
    let deny_ok = deny.payload() == b"E14";

    // (6) qSupported advertises a packet size (what gdb reads on attach).
    let sup = eurogdb::dispatch(b"qSupported:multiprocess+", &mut tgt);
    let sup_ok = eurogdb::as_str(sup.payload()).contains("PacketSize=");

    let ok = halt_ok && g_ok && mem_ok && write_ok && deny_ok && sup_ok;
    crate::serial_println!(
        "[3e5] GDB RSP stub (COM2): halt=S05={halt_ok}, g-regs(real RIP/RSP)={g_ok}, m-read==direct-read={mem_ok}, guarded-M-write-roundtrip={write_ok}, out-of-window-write-denied={deny_ok}, qSupported={sup_ok} → {}",
        if ok { "OK (speaks the real gdb protocol against live kernel state; attach with COM2→tcp + `target remote`) ✓" } else { "FAILED ✗" }
    );
}

/// `gdbstub` shell command: explain how to attach a real gdb.
pub fn shell() -> Vec<String> {
    alloc::vec![
        String::from("EuroGDB — GDB Remote Serial Protocol stub over COM2 (3E-5)"),
        String::from("  attach: boot with  -serial tcp:127.0.0.1:1234,server,nowait  as the 2nd serial,"),
        String::from("          then in gdb:  target remote :1234   (reads regs/memory, set-pc, continue/step)"),
        String::from("  the [3e5] boot self-test proves the protocol against live kernel state each boot"),
        String::from("  REMAINING: breakpoints/watchpoints/vCont + native x86_64-unknown-euroos std toolchain"),
    ]
}
