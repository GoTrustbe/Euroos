//! EuroCrash — **kernel crash-dump-formaat + recovery** (plan Y).
//!
//! G1 vangt stack-overflows recoverable op, maar een volwassen kernel legt bij een
//! fatale `#PF`/`#DF`/`#GP`/paniek een **gestructureerde momentopname** van de
//! kerneltoestand vast (registers, foutvector, CR2/CR3, build-hash), zodat een
//! productie-crash achteraf te debuggen is i.p.v. giswerk. Deze crate is het
//! architectuur-onafhankelijke **dump-formaat** (één 512-byte sector → "minidump"):
//! coderen/decoderen + checksum. De kernel schrijft 'm naar een gereserveerd
//! schijf-blok en leest 'm bij de volgende boot terug (recovery-modus).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub const CRASH_MAGIC: u64 = 0x4555_524F_4352_5348; // "EUROCRSH"
pub const DUMP_VERSION: u32 = 1;
pub const DUMP_BYTES: usize = 512; // één sector = minidump

/// Een minidump: registers + fout-context op het crash-moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashDump {
    pub version: u32,
    pub vector: u8,      // exceptie-vector: #DF=8, #GP=13, #PF=14, paniek=0xFF
    pub error_code: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
    pub cr2: u64,        // faulting address bij #PF
    pub cr3: u64,        // page-table-root
    pub regs: [u64; 16], // rax,rbx,rcx,rdx,rsi,rdi,rbp,rsp,r8..r15 (caller-volgorde)
    pub build_hash: u64, // kernel-build-identiteit
    pub uptime_ms: u64,
    pub seq: u64,        // oplopend per dump (welke is de nieuwste)
}

impl CrashDump {
    pub fn new(vector: u8, error_code: u64, rip: u64, rsp: u64, rflags: u64) -> Self {
        CrashDump {
            version: DUMP_VERSION,
            vector,
            error_code,
            rip,
            rsp,
            rflags,
            cr2: 0,
            cr3: 0,
            regs: [0; 16],
            build_hash: 0,
            uptime_ms: 0,
            seq: 0,
        }
    }

    /// Menselijke naam van de exceptie-vector.
    pub fn vector_name(&self) -> &'static str {
        match self.vector {
            8 => "#DF double-fault",
            13 => "#GP general-protection",
            14 => "#PF page-fault",
            6 => "#UD invalid-opcode",
            0xFF => "panic",
            _ => "exception",
        }
    }

    /// Codeer naar een 512-byte sector (met magic + checksum).
    pub fn encode(&self) -> [u8; DUMP_BYTES] {
        let mut b = [0u8; DUMP_BYTES];
        w64(&mut b, 0, CRASH_MAGIC);
        w32(&mut b, 8, self.version);
        b[12] = self.vector;
        w64(&mut b, 16, self.error_code);
        w64(&mut b, 24, self.rip);
        w64(&mut b, 32, self.rsp);
        w64(&mut b, 40, self.rflags);
        w64(&mut b, 48, self.cr2);
        w64(&mut b, 56, self.cr3);
        for (i, &r) in self.regs.iter().enumerate() {
            w64(&mut b, 64 + i * 8, r);
        }
        w64(&mut b, 192, self.build_hash);
        w64(&mut b, 200, self.uptime_ms);
        w64(&mut b, 208, self.seq);
        // Checksum (XOR-fold) over alles vóór het checksum-veld op offset 504.
        let csum = fold(&b[..504]);
        w64(&mut b, 504, csum);
        b
    }

    /// Decodeer een sector → een dump (None bij verkeerde magic/checksum).
    pub fn decode(b: &[u8]) -> Option<CrashDump> {
        if b.len() < DUMP_BYTES || r64(b, 0) != CRASH_MAGIC {
            return None;
        }
        if r64(b, 504) != fold(&b[..504]) {
            return None;
        }
        let mut regs = [0u64; 16];
        for (i, r) in regs.iter_mut().enumerate() {
            *r = r64(b, 64 + i * 8);
        }
        Some(CrashDump {
            version: r32(b, 8),
            vector: b[12],
            error_code: r64(b, 16),
            rip: r64(b, 24),
            rsp: r64(b, 32),
            rflags: r64(b, 40),
            cr2: r64(b, 48),
            cr3: r64(b, 56),
            regs,
            build_hash: r64(b, 192),
            uptime_ms: r64(b, 200),
            seq: r64(b, 208),
        })
    }
}

fn fold(b: &[u8]) -> u64 {
    let mut acc = 0xC0FF_EE00_1234_5678u64;
    for (i, &x) in b.iter().enumerate() {
        acc ^= (x as u64) << ((i % 8) * 8);
        acc = acc.rotate_left(7).wrapping_add(0x9E37_79B9_7F4A_7C15);
    }
    acc
}

fn w64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
fn w32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn r64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]])
}
fn r32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut d = CrashDump::new(14, 0x2, 0x1_4000_1234, 0xFFFF_8000_0000, 0x202);
        d.cr2 = 0xDEAD_BEEF;
        d.cr3 = 0x1000;
        d.regs[0] = 0xAAAA;
        d.regs[15] = 0xBBBB;
        d.build_hash = 0x0123_4567_89AB_CDEF;
        d.seq = 7;
        let enc = d.encode();
        let back = CrashDump::decode(&enc).unwrap();
        assert_eq!(back, d);
        assert_eq!(back.vector_name(), "#PF page-fault");
        assert_eq!(back.cr2, 0xDEAD_BEEF);
    }

    #[test]
    fn rejects_bad_magic_and_checksum() {
        assert!(CrashDump::decode(&[0u8; DUMP_BYTES]).is_none());
        let mut enc = CrashDump::new(13, 0, 0x1234, 0, 0).encode();
        enc[24] ^= 0xFF; // corrupt de rip → checksum mismatch
        assert!(CrashDump::decode(&enc).is_none());
    }

    #[test]
    fn newest_by_seq() {
        let a = {
            let mut d = CrashDump::new(8, 0, 1, 0, 0);
            d.seq = 3;
            d
        };
        let b = {
            let mut d = CrashDump::new(14, 0, 2, 0, 0);
            d.seq = 9;
            d
        };
        assert!(b.seq > a.seq);
    }
}
