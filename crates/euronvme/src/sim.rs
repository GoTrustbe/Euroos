//! A simulated NVMe controller for the host tests: registers, doorbells, an
//! admin and one I/O queue pair in a simulated DMA window, a RAM namespace.
//! It executes a command when its submission-queue doorbell is written, the
//! way a real controller fetches from the SQ, and posts a completion entry
//! with the phase tag. DMA outside the window is refused (like an IOMMU) and
//! recorded.

use crate::{reg, Hal, CC_EN, CSTS_CFS, CSTS_RDY, QDEPTH};

pub struct SimNvme {
    base: u64,
    mem: Vec<u8>,
    disk: Vec<u8>,
    cc: u32,
    csts: u32,
    aqa: u32,
    asq: u64,
    acq: u64,
    /// (sq base, cq base, sq head, cq tail, cq phase, created) for qid 0 and 1.
    q: [(u64, u64, u16, u16, u16, bool); 2],
    cq_irq: bool,
    commands: u64,
    resets: u64,
    fail_next: Option<u16>,
    dead: bool,
    fatal: bool,
    last_foreign: Option<u64>,
}

impl SimNvme {
    pub fn new(base: u64, window: usize, sectors: u64) -> SimNvme {
        SimNvme {
            base,
            mem: vec![0; window],
            disk: vec![0; sectors as usize * 512],
            cc: 0,
            csts: 0,
            aqa: 0,
            asq: 0,
            acq: 0,
            q: [(0, 0, 0, 0, 1, false); 2],
            cq_irq: false,
            commands: 0,
            resets: 0,
            fail_next: None,
            dead: false,
            fatal: false,
            last_foreign: None,
        }
    }
    pub fn enabled(&self) -> bool {
        self.cc & CC_EN != 0 && self.csts & CSTS_RDY != 0
    }
    pub fn io_queues(&self) -> (bool, bool) {
        (self.q[1].5, self.q[1].0 != 0)
    }
    pub fn io_cq_irq(&self) -> bool {
        self.cq_irq
    }
    pub fn disk(&self) -> &[u8] {
        &self.disk
    }
    pub fn commands(&self) -> u64 {
        self.commands
    }
    pub fn resets(&self) -> u64 {
        self.resets
    }
    pub fn fail_next_io(&mut self, status: u16) {
        self.fail_next = Some(status);
    }
    pub fn set_dead(&mut self, d: bool) {
        self.dead = d;
    }
    pub fn set_fatal(&mut self, f: bool) {
        self.fatal = f;
    }
    pub fn last_foreign(&self) -> Option<u64> {
        self.last_foreign
    }

    fn in_window(&self, a: u64, len: usize) -> bool {
        a >= self.base && a + len as u64 <= self.base + self.mem.len() as u64
    }
    fn rd(&self, a: u64, out: &mut [u8]) {
        let o = (a - self.base) as usize;
        out.copy_from_slice(&self.mem[o..o + out.len()]);
    }
    fn wr(&mut self, a: u64, d: &[u8]) {
        let o = (a - self.base) as usize;
        self.mem[o..o + d.len()].copy_from_slice(d);
    }

    fn post(&mut self, qid: usize, cid: u16, status: u16) {
        let (_, cq, head, tail, phase, _) = self.q[qid];
        let mut e = [0u8; 16];
        e[8..10].copy_from_slice(&head.to_le_bytes()); // SQ head pointer
        let dw3 = cid as u32 | (phase as u32) << 16 | (status as u32) << 17;
        e[12..16].copy_from_slice(&dw3.to_le_bytes());
        self.wr(cq + tail as u64 * 16, &e);
        let nt = (tail + 1) % QDEPTH;
        self.q[qid].3 = nt;
        if nt == 0 {
            self.q[qid].4 ^= 1;
        }
    }

    fn run(&mut self, qid: usize, new_tail: u16) {
        while self.q[qid].2 != new_tail {
            let (sq, _, head, ..) = self.q[qid];
            let mut c = [0u8; 64];
            self.rd(sq + head as u64 * 64, &mut c);
            self.q[qid].2 = (head + 1) % QDEPTH;
            self.commands += 1;
            let dw = |i: usize| u32::from_le_bytes([c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]]);
            let op = c[0];
            let cid = (dw(0) >> 16) as u16;
            let prp1 = dw(6) as u64 | (dw(7) as u64) << 32;
            let prp2 = dw(8) as u64 | (dw(9) as u64) << 32;
            let status = if qid == 0 { self.admin(op, &c, prp1) } else { self.io(op, prp1, prp2, dw(10) as u64 | (dw(11) as u64) << 32, dw(12) as u16 + 1) };
            self.post(qid, cid, status);
        }
    }

    fn admin(&mut self, op: u8, c: &[u8; 64], prp1: u64) -> u16 {
        let dw10 = u32::from_le_bytes([c[40], c[41], c[42], c[43]]);
        let dw11 = u32::from_le_bytes([c[44], c[45], c[46], c[47]]);
        match op {
            0x06 => {
                if !self.in_window(prp1, 4096) {
                    return 0x0006;
                }
                let mut page = vec![0u8; 4096];
                if dw10 & 0xFF == 1 {
                    let m = b"EuroSim NVMe";
                    page[24..24 + m.len()].copy_from_slice(m);
                    for b in &mut page[24 + m.len()..64] {
                        *b = b' ';
                    }
                } else {
                    let n = (self.disk.len() / 512) as u64;
                    page[0..8].copy_from_slice(&n.to_le_bytes());
                    page[26] = 0; // FLBAS: format 0
                    page[128 + 2] = 9; // LBAF0: 512-byte sectors
                }
                self.wr(prp1, &page);
                0
            }
            0x05 => {
                self.q[1].1 = prp1;
                self.q[1].3 = 0;
                self.q[1].4 = 1;
                self.q[1].5 = true;
                self.cq_irq = dw11 & 2 != 0;
                0
            }
            0x01 => {
                if !self.q[1].5 {
                    return 0x0100 | 0x01; // completion queue invalid
                }
                self.q[1].0 = prp1;
                self.q[1].2 = 0;
                0
            }
            _ => 0x0001, // invalid opcode
        }
    }

    fn io(&mut self, op: u8, prp1: u64, prp2: u64, lba: u64, n: u16) -> u16 {
        if let Some(s) = self.fail_next.take() {
            return s;
        }
        let bytes = n as usize * 512;
        let first = bytes.min(4096 - (prp1 % 4096) as usize);
        let ok = |s: &SimNvme, a: u64, l: usize| l == 0 || s.in_window(a, l);
        if !ok(self, prp1, first) || (bytes > first && !ok(self, prp2, bytes - first)) {
            self.last_foreign = Some(if !ok(self, prp1, first) { prp1 } else { prp2 });
            return 0x0006; // internal error: the DMA was refused
        }
        let off = lba as usize * 512;
        if off + bytes > self.disk.len() {
            return 0x0080; // LBA out of range
        }
        match op {
            0x02 => {
                let d = self.disk[off..off + bytes].to_vec();
                self.wr(prp1, &d[..first]);
                if bytes > first {
                    self.wr(prp2, &d[first..]);
                }
                0
            }
            0x01 => {
                let mut d = vec![0u8; bytes];
                self.rd(prp1, &mut d[..first]);
                if bytes > first {
                    let mut rest = vec![0u8; bytes - first];
                    self.rd(prp2, &mut rest);
                    d[first..].copy_from_slice(&rest);
                }
                self.disk[off..off + bytes].copy_from_slice(&d);
                0
            }
            _ => 0x0001,
        }
    }
}

impl Hal for SimNvme {
    fn reg_read(&mut self, off: u32) -> u32 {
        match off {
            reg::CAP_LO => (QDEPTH as u32 - 1) | (20 << 24), // MQES, TO
            reg::CAP_HI => 0,                                 // DSTRD 0: 4-byte stride
            reg::VS => 0x0001_0400,
            reg::CC => self.cc,
            reg::CSTS => self.csts,
            _ => 0,
        }
    }

    fn reg_write(&mut self, off: u32, val: u32) {
        match off {
            reg::CC => {
                let was = self.cc & CC_EN != 0;
                self.cc = val;
                if val & CC_EN == 0 {
                    if was || self.csts & CSTS_RDY != 0 {
                        self.resets += 1;
                    }
                    self.csts &= !CSTS_RDY;
                    self.q = [(0, 0, 0, 0, 1, false); 2];
                    if self.dead {
                        self.csts |= CSTS_RDY; // a dead controller never goes idle
                    }
                } else if !was && !self.dead {
                    if self.fatal {
                        self.csts |= CSTS_CFS;
                    } else {
                        self.csts |= CSTS_RDY;
                        self.q[0] = (self.asq, self.acq, 0, 0, 1, true);
                        let _ = self.aqa;
                    }
                }
            }
            reg::AQA => self.aqa = val,
            reg::ASQ_LO => self.asq = (self.asq & !0xFFFF_FFFF) | val as u64,
            reg::ASQ_HI => self.asq = (self.asq & 0xFFFF_FFFF) | (val as u64) << 32,
            reg::ACQ_LO => self.acq = (self.acq & !0xFFFF_FFFF) | val as u64,
            reg::ACQ_HI => self.acq = (self.acq & 0xFFFF_FFFF) | (val as u64) << 32,
            o if o >= reg::DOORBELL_BASE => {
                let i = (o - reg::DOORBELL_BASE) / 4;
                let qid = (i / 2) as usize;
                if qid < 2 && i % 2 == 0 && self.csts & CSTS_RDY != 0 {
                    self.run(qid, val as u16 % QDEPTH);
                }
            }
            _ => {}
        }
    }

    fn dma_write(&mut self, iova: u64, data: &[u8]) {
        assert!(self.in_window(iova, data.len()), "driver wrote outside its DMA window");
        self.wr(iova, data);
    }

    fn dma_read(&mut self, iova: u64, out: &mut [u8]) {
        assert!(self.in_window(iova, out.len()), "driver read outside its DMA window");
        self.rd(iova, out);
    }
}
