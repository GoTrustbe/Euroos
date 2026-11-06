//! A simulated e1000 for the host tests: registers, the legacy descriptor
//! rings and the bus, close enough to the datasheet behaviour that the driver
//! logic is exercised for real (head/tail ownership, DD write-back, reset
//! clearing the ring registers, a full ring dropping frames). Bus accesses
//! outside the driver's DMA window are recorded, not performed: that is the
//! question the IOMMU answers on real hardware.

use std::collections::HashMap;

use super::{reg, Hal, CTRL_RST, DESC_STA_DD, NDESC, RXD_STA_EOP};

pub struct Sim {
    regs: HashMap<u32, u32>,
    mem: Vec<u8>,
    base: u64,
    mac: [u8; 6],
    pub wire: Vec<Vec<u8>>,
    pub resets: u32,
    pub stuck_reset: bool,
    pub dead_tx: bool,
    pub foreign_writes: Vec<u64>,
    pub foreign_reads: Vec<u64>,
}

impl Sim {
    pub fn new(mac: [u8; 6], base: u64, len: usize) -> Sim {
        let mut s = Sim {
            regs: HashMap::new(),
            mem: vec![0; len],
            base,
            mac,
            wire: Vec::new(),
            resets: 0,
            stuck_reset: false,
            dead_tx: false,
            foreign_writes: Vec::new(),
            foreign_reads: Vec::new(),
        };
        s.load_mac();
        s
    }

    fn load_mac(&mut self) {
        let m = self.mac;
        self.regs.insert(reg::RAL0, u32::from_le_bytes([m[0], m[1], m[2], m[3]]));
        self.regs.insert(reg::RAH0, u32::from_le_bytes([m[4], m[5], 0, 0]));
    }

    pub fn reg(&self, off: u32) -> u32 {
        *self.regs.get(&off).unwrap_or(&0)
    }

    fn in_window(&self, addr: u64, len: usize) -> bool {
        addr >= self.base && addr + len as u64 <= self.base + self.mem.len() as u64
    }

    fn bus_write(&mut self, addr: u64, data: &[u8]) -> bool {
        if !self.in_window(addr, data.len()) {
            self.foreign_writes.push(addr);
            return false;
        }
        let o = (addr - self.base) as usize;
        self.mem[o..o + data.len()].copy_from_slice(data);
        true
    }

    fn bus_read(&mut self, addr: u64, out: &mut [u8]) -> bool {
        if !self.in_window(addr, out.len()) {
            self.foreign_reads.push(addr);
            return false;
        }
        let o = (addr - self.base) as usize;
        out.copy_from_slice(&self.mem[o..o + out.len()]);
        true
    }

    pub fn mem_u64(&self, addr: u64) -> u64 {
        let o = (addr - self.base) as usize;
        u64::from_le_bytes(self.mem[o..o + 8].try_into().unwrap())
    }

    fn ring(&self, lo: u32, hi: u32) -> u64 {
        (self.reg(hi) as u64) << 32 | self.reg(lo) as u64
    }

    /// A frame arrives from the wire. False when the device owns no free slot.
    pub fn inject_rx(&mut self, frame: &[u8]) -> bool {
        self.inject_rx_with_len(frame, frame.len() as u16)
    }

    /// Like `inject_rx`, but the descriptor reports `len` (a lying device).
    pub fn inject_rx_with_len(&mut self, frame: &[u8], len: u16) -> bool {
        let (rdh, rdt) = (self.reg(reg::RDH), self.reg(reg::RDT));
        if rdh == rdt {
            return false; // the device owns RDH..RDT-1: nothing free
        }
        let d = self.ring(reg::RDBAL, reg::RDBAH) + rdh as u64 * 16;
        let mut desc = [0u8; 16];
        assert!(self.bus_read(d, &mut desc), "RX ring outside the window");
        let buf = u64::from_le_bytes(desc[0..8].try_into().unwrap());
        self.bus_write(buf, frame);
        desc[8..10].copy_from_slice(&len.to_le_bytes());
        desc[12] = DESC_STA_DD | RXD_STA_EOP;
        assert!(self.bus_write(d, &desc));
        self.regs.insert(reg::RDH, (rdh + 1) % NDESC as u32);
        true
    }

    fn process_tx(&mut self) {
        if self.dead_tx {
            return;
        }
        let base = self.ring(reg::TDBAL, reg::TDBAH);
        loop {
            let (tdh, tdt) = (self.reg(reg::TDH), self.reg(reg::TDT));
            if tdh == tdt {
                break;
            }
            let d = base + tdh as u64 * 16;
            let mut desc = [0u8; 16];
            assert!(self.bus_read(d, &mut desc), "TX ring outside the window");
            let addr = u64::from_le_bytes(desc[0..8].try_into().unwrap());
            let len = u16::from_le_bytes([desc[8], desc[9]]) as usize;
            let mut data = vec![0u8; len];
            if self.bus_read(addr, &mut data) {
                self.wire.push(data);
            }
            desc[12] |= DESC_STA_DD;
            assert!(self.bus_write(d, &desc));
            self.regs.insert(reg::TDH, (tdh + 1) % NDESC as u32);
        }
    }
}

impl Hal for Sim {
    fn reg_read(&mut self, off: u32) -> u32 {
        if off == reg::ICR {
            return self.regs.remove(&off).unwrap_or(0); // read-to-clear
        }
        self.reg(off)
    }

    fn reg_write(&mut self, off: u32, val: u32) {
        if off == reg::CTRL && val & CTRL_RST != 0 {
            if self.stuck_reset {
                self.regs.insert(off, val);
                return;
            }
            // A full reset: every register back to default, MAC reloaded.
            self.resets += 1;
            self.regs.clear();
            self.load_mac();
            self.regs.insert(off, val & !CTRL_RST);
            return;
        }
        self.regs.insert(off, val);
        if off == reg::TDT {
            self.process_tx();
        }
    }

    fn dma_write(&mut self, iova: u64, data: &[u8]) {
        assert!(self.in_window(iova, data.len()), "driver wrote outside its DMA window: {iova:#x}");
        self.bus_write(iova, data);
    }

    fn dma_read(&mut self, iova: u64, out: &mut [u8]) {
        assert!(self.in_window(iova, out.len()), "driver read outside its DMA window: {iova:#x}");
        self.bus_read(iova, out);
    }
}
