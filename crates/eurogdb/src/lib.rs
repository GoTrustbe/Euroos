//! **EuroGDB** — the GDB **Remote Serial Protocol** (RSP) stub core (3E-5).
//!
//! A from-scratch, `no_std`, host-tested implementation of the wire protocol a
//! real `gdb`/`gdb-multiarch` speaks over a serial line: `$<payload>#<csum>`
//! framing with `+`/`-` acknowledgements, the amd64 `g`-packet register layout,
//! and dispatch of the core commands (`?`, `g`, `G`, `m`, `M`, `p`, `P`, `c`,
//! `s`, `qSupported`, `qAttached`, `D`, `k`). The actual CPU register access,
//! memory peek/poke and single-step are provided by the kernel via the
//! [`Target`] trait; this crate is pure protocol so it can be exhaustively
//! tested on the host against a mock target — and byte-for-byte against what a
//! real gdb sends.
//!
//! Scope (honest): this is the *stub* half — enough for gdb to attach, read
//! registers/memory, set the pc and continue/step. Hardware breakpoints,
//! watchpoints, the `vCont` packet and multi-thread (`H`/`T`) support are
//! out of scope here (documented as remaining).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// The amd64 general layout gdb expects in the `g`/`G` packet: 16 GPRs + rip
/// (each 8 bytes), then eflags/cs/ss/ds/es/fs/gs (each 4 bytes). 168 bytes.
pub const GPR_COUNT: usize = 17; // rax..r15, rip
pub const SREG_COUNT: usize = 7; // eflags, cs, ss, ds, es, fs, gs
pub const REG_BYTES: usize = GPR_COUNT * 8 + SREG_COUNT * 4;

/// gdb register indices (used by the `p`/`P` single-register packets).
pub mod reg {
    pub const RAX: usize = 0;
    pub const RBX: usize = 1;
    pub const RCX: usize = 2;
    pub const RDX: usize = 3;
    pub const RSI: usize = 4;
    pub const RDI: usize = 5;
    pub const RBP: usize = 6;
    pub const RSP: usize = 7;
    pub const R8: usize = 8;
    pub const R15: usize = 15;
    pub const RIP: usize = 16;
    pub const EFLAGS: usize = 17;
    pub const CS: usize = 18;
    pub const GS: usize = 24;
}

/// What the stub needs from the running kernel. All addresses are the target's
/// own virtual addresses.
pub trait Target {
    /// The 24 gdb registers (see the module constants): `[0..17]` = GPRs+rip,
    /// `[17..24]` = eflags/cs/ss/ds/es/fs/gs (only the low 32 bits are used).
    fn read_registers(&self) -> [u64; GPR_COUNT + SREG_COUNT];
    /// Overwrite the registers (from a `G` packet).
    fn write_registers(&mut self, regs: &[u64; GPR_COUNT + SREG_COUNT]);
    /// Read `len` bytes at `addr`. `None` if the range is not readable (→ gdb `E`).
    fn read_mem(&self, addr: u64, len: usize) -> Option<Vec<u8>>;
    /// Write `data` at `addr`. `false` if not writable.
    fn write_mem(&mut self, addr: u64, data: &[u8]) -> bool;
    /// The stop reason as a signal number (SIGTRAP = 5 after a break/step).
    fn halt_signal(&self) -> u8 {
        5
    }
}

// ── hex helpers ────────────────────────────────────────────────────────────

fn hex_nibble(n: u8) -> u8 {
    match n {
        0..=9 => b'0' + n,
        _ => b'a' + (n - 10),
    }
}

fn from_hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Append `byte` as two lowercase hex chars.
fn push_hex_u8(out: &mut Vec<u8>, byte: u8) {
    out.push(hex_nibble(byte >> 4));
    out.push(hex_nibble(byte & 0x0F));
}

/// Bytes → hex string (as gdb sends memory / register contents).
pub fn to_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    for &b in data {
        push_hex_u8(&mut out, b);
    }
    out
}

/// Hex string → bytes. `None` on an odd length or a non-hex char.
pub fn from_hex(s: &[u8]) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < s.len() {
        out.push((from_hex_nibble(s[i])? << 4) | from_hex_nibble(s[i + 1])?);
        i += 2;
    }
    Some(out)
}

/// The modulo-256 checksum of an RSP payload.
pub fn checksum(payload: &[u8]) -> u8 {
    payload.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

/// Frame a payload into a full `$<payload>#<cc>` RSP packet.
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.push(b'$');
    out.extend_from_slice(payload);
    out.push(b'#');
    push_hex_u8(&mut out, checksum(payload));
    out
}

/// Extract the payload of a `$...#cc` packet and verify the checksum.
/// `None` on a malformed frame or a checksum mismatch.
pub fn unframe(packet: &[u8]) -> Option<Vec<u8>> {
    let start = packet.iter().position(|&b| b == b'$')?;
    let hash = packet.iter().rposition(|&b| b == b'#')?;
    // Need the '$' before the '#', and two checksum digits after the '#'.
    if hash <= start || hash + 3 > packet.len() {
        return None;
    }
    let payload = &packet[start + 1..hash];
    let cs = from_hex(&packet[hash + 1..hash + 3])?;
    if cs.first().copied()? != checksum(payload) {
        return None;
    }
    Some(payload.to_vec())
}

fn u64_le_hex(out: &mut Vec<u8>, v: u64) {
    for b in v.to_le_bytes() {
        push_hex_u8(out, b);
    }
}

fn u32_le_hex(out: &mut Vec<u8>, v: u32) {
    for b in v.to_le_bytes() {
        push_hex_u8(out, b);
    }
}

/// Serialise all registers into the `g`-packet hex form.
pub fn registers_to_hex(regs: &[u64; GPR_COUNT + SREG_COUNT]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REG_BYTES * 2);
    for &r in regs.iter().take(GPR_COUNT) {
        u64_le_hex(&mut out, r);
    }
    for &r in regs.iter().skip(GPR_COUNT) {
        u32_le_hex(&mut out, r as u32);
    }
    out
}

/// Parse a `G`-packet hex string back into the register array.
pub fn registers_from_hex(hex: &[u8]) -> Option<[u64; GPR_COUNT + SREG_COUNT]> {
    let bytes = from_hex(hex)?;
    if bytes.len() < REG_BYTES {
        return None;
    }
    let mut regs = [0u64; GPR_COUNT + SREG_COUNT];
    let mut o = 0;
    for r in regs.iter_mut().take(GPR_COUNT) {
        let mut a = [0u8; 8];
        a.copy_from_slice(&bytes[o..o + 8]);
        *r = u64::from_le_bytes(a);
        o += 8;
    }
    for r in regs.iter_mut().skip(GPR_COUNT) {
        let mut a = [0u8; 4];
        a.copy_from_slice(&bytes[o..o + 4]);
        *r = u32::from_le_bytes(a) as u64;
        o += 4;
    }
    Some(regs)
}

/// What the transport layer should do after producing a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send this reply payload (already un-framed) and keep serving.
    Reply(Vec<u8>),
    /// Send the reply, then resume the target (continue).
    Continue(Vec<u8>),
    /// Send the reply, then single-step, then report the stop.
    Step(Vec<u8>),
    /// Detach / kill — stop serving.
    Detach(Vec<u8>),
}

impl Action {
    pub fn payload(&self) -> &[u8] {
        match self {
            Action::Reply(p) | Action::Continue(p) | Action::Step(p) | Action::Detach(p) => p,
        }
    }
}

fn stop_reply(sig: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(3);
    v.push(b'S');
    push_hex_u8(&mut v, sig);
    v
}

/// Dispatch ONE decoded RSP payload against `target`, returning the reply +
/// what the transport should do next. This is the whole protocol brain.
pub fn dispatch(payload: &[u8], target: &mut dyn Target) -> Action {
    if payload.is_empty() {
        return Action::Reply(Vec::new()); // empty = "unsupported"
    }
    match payload[0] {
        // Halt reason.
        b'?' => Action::Reply(stop_reply(target.halt_signal())),
        // Read all registers.
        b'g' => Action::Reply(registers_to_hex(&target.read_registers())),
        // Write all registers.
        b'G' => match registers_from_hex(&payload[1..]) {
            Some(regs) => {
                target.write_registers(&regs);
                Action::Reply(b"OK".to_vec())
            }
            None => Action::Reply(b"E22".to_vec()),
        },
        // Read one register: `p<n>`.
        b'p' => match parse_hex_u64(&payload[1..]) {
            Some(n) => {
                let regs = target.read_registers();
                let idx = n as usize;
                if idx < GPR_COUNT {
                    let mut out = Vec::new();
                    u64_le_hex(&mut out, regs[idx]);
                    Action::Reply(out)
                } else if idx < GPR_COUNT + SREG_COUNT {
                    let mut out = Vec::new();
                    u32_le_hex(&mut out, regs[idx] as u32);
                    Action::Reply(out)
                } else {
                    Action::Reply(b"E01".to_vec())
                }
            }
            None => Action::Reply(b"E01".to_vec()),
        },
        // Write one register: `P<n>=<value>`.
        b'P' => match parse_p_write(&payload[1..]) {
            Some((n, val)) if (n as usize) < GPR_COUNT + SREG_COUNT => {
                let mut regs = target.read_registers();
                regs[n as usize] = val;
                target.write_registers(&regs);
                Action::Reply(b"OK".to_vec())
            }
            _ => Action::Reply(b"E01".to_vec()),
        },
        // Read memory: `m<addr>,<len>`.
        b'm' => match parse_addr_len(&payload[1..]) {
            Some((addr, len)) => match target.read_mem(addr, len) {
                Some(data) => Action::Reply(to_hex(&data)),
                None => Action::Reply(b"E14".to_vec()),
            },
            None => Action::Reply(b"E22".to_vec()),
        },
        // Write memory: `M<addr>,<len>:<hexbytes>`.
        b'M' => match parse_m_write(&payload[1..]) {
            Some((addr, data)) => {
                if target.write_mem(addr, &data) {
                    Action::Reply(b"OK".to_vec())
                } else {
                    Action::Reply(b"E14".to_vec())
                }
            }
            None => Action::Reply(b"E22".to_vec()),
        },
        // Continue / step: the reply is the stop packet after resuming.
        b'c' => Action::Continue(stop_reply(target.halt_signal())),
        b's' => Action::Step(stop_reply(target.halt_signal())),
        // Detach / kill.
        b'D' => Action::Detach(b"OK".to_vec()),
        b'k' => Action::Detach(Vec::new()),
        // Queries.
        b'q' => Action::Reply(handle_query(payload)),
        b'H' => Action::Reply(b"OK".to_vec()), // thread select — single-thread stub
        _ => Action::Reply(Vec::new()),        // unknown → empty ("unsupported")
    }
}

fn handle_query(payload: &[u8]) -> Vec<u8> {
    if payload.starts_with(b"qSupported") {
        return b"PacketSize=4000".to_vec();
    }
    if payload.starts_with(b"qAttached") {
        return b"1".to_vec(); // attached to an existing process
    }
    if payload == b"qC" {
        return b"QC1".to_vec();
    }
    if payload.starts_with(b"qfThreadInfo") {
        return b"m1".to_vec();
    }
    if payload.starts_with(b"qsThreadInfo") {
        return b"l".to_vec();
    }
    Vec::new()
}

fn parse_hex_u64(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &c in s {
        v = v.checked_mul(16)?.checked_add(from_hex_nibble(c)? as u64)?;
    }
    Some(v)
}

fn parse_addr_len(s: &[u8]) -> Option<(u64, usize)> {
    let comma = s.iter().position(|&b| b == b',')?;
    let addr = parse_hex_u64(&s[..comma])?;
    let len = parse_hex_u64(&s[comma + 1..])? as usize;
    Some((addr, len))
}

fn parse_m_write(s: &[u8]) -> Option<(u64, Vec<u8>)> {
    let comma = s.iter().position(|&b| b == b',')?;
    let colon = s.iter().position(|&b| b == b':')?;
    let addr = parse_hex_u64(&s[..comma])?;
    let len = parse_hex_u64(&s[comma + 1..colon])? as usize;
    let data = from_hex(&s[colon + 1..])?;
    if data.len() != len {
        return None;
    }
    Some((addr, data))
}

fn parse_p_write(s: &[u8]) -> Option<(u64, u64)> {
    let eq = s.iter().position(|&b| b == b'=')?;
    let n = parse_hex_u64(&s[..eq])?;
    // Value is little-endian hex bytes.
    let bytes = from_hex(&s[eq + 1..])?;
    let mut a = [0u8; 8];
    for (i, &b) in bytes.iter().take(8).enumerate() {
        a[i] = b;
    }
    Some((n, u64::from_le_bytes(a)))
}

/// Convenience for the transport: decode a full framed packet and dispatch it.
pub fn handle_packet(packet: &[u8], target: &mut dyn Target) -> Option<Action> {
    let payload = unframe(packet)?;
    Some(dispatch(&payload, target))
}

/// The printable form of an `Action`'s framed reply (for logging / tests).
pub fn framed_reply(action: &Action) -> Vec<u8> {
    frame(action.payload())
}

/// UTF-8 lossy view (tests/logging).
pub fn as_str(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTarget {
        regs: [u64; GPR_COUNT + SREG_COUNT],
        mem: Vec<u8>,
        base: u64,
    }

    impl MockTarget {
        fn new() -> Self {
            let mut regs = [0u64; GPR_COUNT + SREG_COUNT];
            regs[reg::RIP] = 0xffff_8000_0010_1234;
            regs[reg::RSP] = 0xffff_8000_0020_0000;
            regs[reg::RAX] = 0x1122_3344_5566_7788;
            regs[reg::EFLAGS] = 0x202;
            regs[reg::CS] = 0x08;
            Self { regs, mem: (0u8..64).collect(), base: 0x1000 }
        }
    }

    impl Target for MockTarget {
        fn read_registers(&self) -> [u64; GPR_COUNT + SREG_COUNT] {
            self.regs
        }
        fn write_registers(&mut self, regs: &[u64; GPR_COUNT + SREG_COUNT]) {
            self.regs = *regs;
        }
        fn read_mem(&self, addr: u64, len: usize) -> Option<Vec<u8>> {
            let off = addr.checked_sub(self.base)? as usize;
            self.mem.get(off..off + len).map(|s| s.to_vec())
        }
        fn write_mem(&mut self, addr: u64, data: &[u8]) -> bool {
            let off = match addr.checked_sub(self.base) {
                Some(o) => o as usize,
                None => return false,
            };
            if off + data.len() > self.mem.len() {
                return false;
            }
            self.mem[off..off + data.len()].copy_from_slice(data);
            true
        }
    }

    #[test]
    fn frame_unframe_roundtrip_and_checksum() {
        let f = frame(b"OK");
        assert_eq!(as_str(&f), "$OK#9a");
        assert_eq!(unframe(&f).unwrap(), b"OK");
        // A corrupted checksum is rejected.
        let mut bad = f.clone();
        *bad.last_mut().unwrap() = b'0';
        assert!(unframe(&bad).is_none());
    }

    #[test]
    fn halt_reason_is_sigtrap() {
        let mut t = MockTarget::new();
        assert_eq!(dispatch(b"?", &mut t), Action::Reply(b"S05".to_vec()));
    }

    #[test]
    fn read_registers_roundtrips_through_g_packet() {
        let mut t = MockTarget::new();
        let g = match dispatch(b"g", &mut t) {
            Action::Reply(p) => p,
            _ => panic!(),
        };
        assert_eq!(g.len(), REG_BYTES * 2);
        // rax is the first 8 bytes, little-endian hex.
        assert_eq!(&g[..16], b"8877665544332211");
        // Round-trips back to the same registers.
        let parsed = registers_from_hex(&g).unwrap();
        assert_eq!(parsed, t.read_registers());
    }

    #[test]
    fn write_registers_via_G() {
        let mut t = MockTarget::new();
        let mut regs = t.read_registers();
        regs[reg::RIP] = 0xdead_beef;
        let mut pkt = alloc::vec![b'G'];
        pkt.extend_from_slice(&registers_to_hex(&regs));
        assert_eq!(dispatch(&pkt, &mut t), Action::Reply(b"OK".to_vec()));
        assert_eq!(t.read_registers()[reg::RIP], 0xdead_beef);
    }

    #[test]
    fn read_and_write_memory() {
        let mut t = MockTarget::new();
        // m1000,4 → the first 4 mock bytes 00 01 02 03.
        let r = dispatch(b"m1000,4", &mut t);
        assert_eq!(r, Action::Reply(b"00010203".to_vec()));
        // M1000,2:aabb → write, then read back.
        assert_eq!(dispatch(b"M1000,2:aabb", &mut t), Action::Reply(b"OK".to_vec()));
        assert_eq!(dispatch(b"m1000,2", &mut t), Action::Reply(b"aabb".to_vec()));
        // Out-of-range read → error.
        assert_eq!(dispatch(b"m9999,4", &mut t), Action::Reply(b"E14".to_vec()));
    }

    #[test]
    fn single_register_read_write() {
        let mut t = MockTarget::new();
        // p0 = rax little-endian.
        assert_eq!(dispatch(b"p0", &mut t), Action::Reply(b"8877665544332211".to_vec()));
        // P10=<val> writes r8 (index 8 = "8" hex? no, index 8). Use index 0.
        assert_eq!(dispatch(b"P0=efbeadde00000000", &mut t), Action::Reply(b"OK".to_vec()));
        assert_eq!(t.read_registers()[reg::RAX], 0x0000_0000_dead_beef);
    }

    #[test]
    fn continue_and_step_carry_stop_reply() {
        let mut t = MockTarget::new();
        assert_eq!(dispatch(b"c", &mut t), Action::Continue(b"S05".to_vec()));
        assert_eq!(dispatch(b"s", &mut t), Action::Step(b"S05".to_vec()));
    }

    #[test]
    fn queries_and_detach() {
        let mut t = MockTarget::new();
        assert_eq!(dispatch(b"qSupported:multiprocess+", &mut t), Action::Reply(b"PacketSize=4000".to_vec()));
        assert_eq!(dispatch(b"qAttached", &mut t), Action::Reply(b"1".to_vec()));
        assert_eq!(dispatch(b"D", &mut t), Action::Detach(b"OK".to_vec()));
        // Unknown packet → empty reply ("unsupported").
        assert_eq!(dispatch(b"zZz", &mut t), Action::Reply(Vec::new()));
    }
}
