//! A simulated MFI gen2 controller for the host tests.
//!
//! It models what the driver depends on, the way QEMU `hw/scsi/megasas.c`
//! behaves: the status word in OSP0, the doorbell transitions (READY doorbell
//! and an all-ones interrupt mask both soft-reset the firmware), frames posted
//! through IQP, the init frame's queue info, DCMDs, LD read/write, a
//! READ CAPACITY(16) pass-through, and completions written to the reply ring
//! with the producer index advanced modulo the command count (only while the
//! interrupt is unmasked). Device DMA outside the driver's window is refused
//! and counted, like an IOMMU fault.

use std::vec::Vec;

use crate::frame::{self, get32, get64};
use crate::info;
use crate::layout::WINDOW_BYTES;
use crate::mfi::Hal;
use crate::regs;

/// Where the simulated window lives.
pub const BASE: u64 = 0x0010_0000;

const READY: u32 = 0xB000_0000;
const OPERATIONAL: u32 = 0xC000_0000;

pub struct Sim {
    mem: Vec<u8>,
    cmds: u32,
    sge: u32,
    state: u32,
    forced: Option<u32>,
    script: Vec<u32>,
    acked: u32,
    intr_mask: u32,
    doorbell: bool,
    ring: u64,
    pi: u64,
    head: u32,
    queued: Vec<u64>,
    deferred: bool,
    disks: Vec<(u8, Vec<u8>)>,
    now: u64,
    doorbells: Vec<u32>,
    pub resets: u32,
    pub flushes: u32,
    pub faults_read: u32,
    pub faults_write: u32,
}

impl Sim {
    pub fn new(disks: &[(u8, u64)]) -> Sim {
        Sim::with_cmds(disks, 1008)
    }

    pub fn with_cmds(disks: &[(u8, u64)], cmds: u32) -> Sim {
        Sim {
            mem: std::vec![0; WINDOW_BYTES as usize],
            cmds,
            sge: 80,
            state: READY,
            forced: None,
            script: Vec::new(),
            acked: 0,
            intr_mask: regs::MASK_ALL,
            doorbell: false,
            ring: 0,
            pi: 0,
            head: 0,
            queued: Vec::new(),
            deferred: false,
            disks: disks.iter().map(|&(t, s)| (t, std::vec![0u8; s as usize * 512])).collect(),
            now: 0,
            doorbells: Vec::new(),
            resets: 0,
            flushes: 0,
            faults_read: 0,
            faults_write: 0,
        }
    }

    /// Start in these states (status words) before READY.
    pub fn boot_script(&mut self, states: &[u32]) {
        self.script = states.to_vec();
        self.state = self.script.first().copied().unwrap_or(READY);
    }

    pub fn force_status(&mut self, s: Option<u32>) {
        self.forced = s;
    }

    pub fn set_deferred(&mut self, d: bool) {
        self.deferred = d;
    }

    pub fn intr_enabled(&self) -> bool {
        self.intr_mask != regs::MASK_ALL
    }

    pub fn saw_doorbell(&self, v: u32) -> bool {
        self.doorbells.contains(&v)
    }

    pub fn disk(&self, target: u8) -> &[u8] {
        &self.disks.iter().find(|d| d.0 == target).unwrap().1
    }

    fn in_window(&self, addr: u64, len: usize) -> bool {
        addr >= BASE && addr.checked_add(len as u64).is_some_and(|e| e <= BASE + WINDOW_BYTES)
    }

    // ── device-side DMA (through the "IOMMU") ────────────────────────────────
    fn dev_read(&mut self, addr: u64, out: &mut [u8]) -> bool {
        if !self.in_window(addr, out.len()) {
            self.faults_read += 1;
            return false;
        }
        let o = (addr - BASE) as usize;
        out.copy_from_slice(&self.mem[o..o + out.len()]);
        true
    }

    fn dev_write(&mut self, addr: u64, data: &[u8]) -> bool {
        if !self.in_window(addr, data.len()) {
            self.faults_write += 1;
            return false;
        }
        let o = (addr - BASE) as usize;
        self.mem[o..o + data.len()].copy_from_slice(data);
        true
    }

    fn soft_reset(&mut self) {
        self.queued.clear(); // abort everything outstanding
        self.ring = 0;
        self.pi = 0;
        self.head = 0;
        self.state = READY;
        self.doorbell = false;
        self.intr_mask = regs::MASK_ALL;
        self.resets += 1;
    }

    /// Put `ctx` into the reply ring as the device would.
    fn complete(&mut self, ctx: u32) {
        if self.ring == 0 {
            return;
        }
        let at = self.ring + self.head as u64 * 4;
        self.dev_write(at, &ctx.to_le_bytes());
        if self.intr_enabled() {
            self.head = (self.head + 1) % self.cmds;
            let h = self.head;
            self.dev_write(self.pi, &h.to_le_bytes());
            self.doorbell = true;
        }
    }

    /// Test hook: a context appears in the ring without a frame behind it.
    pub fn inject_context(&mut self, ctx: u32) {
        self.complete(ctx);
    }

    pub fn run_device(&mut self) {
        let q = std::mem::take(&mut self.queued);
        for a in q {
            self.process(a);
        }
    }

    fn set_status(&mut self, frame: u64, status: u8, scsi: u8) {
        self.dev_write(frame + 3, &[scsi]);
        self.dev_write(frame + 2, &[status]);
    }

    fn process(&mut self, addr: u64) {
        let mut ctxb = [0u8; 8];
        let ctx_ok = self.dev_read(addr + 8, &mut ctxb);
        let mut f = [0u8; frame::FRAME_SIZE];
        if !ctx_ok || !self.dev_read(addr, &mut f) {
            // QEMU: the frame cannot be mapped -> BUSY status written to the
            // frame address, and whatever "context" was read is completed.
            self.set_status(addr, frame::STAT_SCSI_DONE_WITH_ERROR, 0x08);
            self.complete(0xDEAD_0000 | (addr as u32 & 0xFFFF));
            return;
        }
        let ctx = get32(&f, 8);
        let (status, scsi) = match f[0] {
            frame::CMD_INIT => self.init(&f),
            frame::CMD_DCMD => self.dcmd(&f),
            frame::CMD_LD_READ | frame::CMD_LD_WRITE => self.ld_io(&f),
            frame::CMD_LD_SCSI_IO => self.pass(&f),
            _ => (frame::STAT_INVALID_CMD, 0),
        };
        self.set_status(addr, status, scsi);
        self.complete(ctx);
    }

    fn init(&mut self, f: &[u8]) -> (u8, u8) {
        let qa = get32(f, 24) as u64 | ((get32(f, 28) as u64) << 32);
        let mut q = [0u8; frame::QINFO_LEN];
        if !self.dev_read(qa, &mut q) {
            return (frame::STAT_MEMORY_NOT_AVAILABLE, 0);
        }
        if get32(&q, 4) > self.cmds {
            return (frame::STAT_INVALID_PARAMETER, 0);
        }
        self.ring = get64(&q, 8);
        self.pi = get64(&q, 16);
        let mut h = [0u8; 4];
        self.dev_read(self.pi, &mut h);
        self.head = u32::from_le_bytes(h) % self.cmds;
        self.state = OPERATIONAL;
        (frame::STAT_OK, 0)
    }

    fn sg(f: &[u8], at: usize) -> (u64, usize) {
        (get64(f, at), get32(f, at + 8) as usize)
    }

    fn dcmd(&mut self, f: &[u8]) -> (u8, u8) {
        let (addr, len) = Sim::sg(f, 40);
        match get32(f, 24) {
            frame::DCMD_CTRL_GET_INFO => {
                if len < info::CTRL_INFO_LEN {
                    return (frame::STAT_INVALID_PARAMETER, 0);
                }
                let b = info::encode_ctrl_info("Sim MegaRAID SAS 9260-8i", "SIM0001", "1.80.0-sim", self.cmds as u16, self.disks.len() as u16);
                self.dev_write(addr, &b);
                (frame::STAT_OK, 0)
            }
            frame::DCMD_LD_GET_LIST => {
                let mut b = std::vec![0u8; info::LD_LIST_LEN];
                b[0..4].copy_from_slice(&(self.disks.len() as u32).to_le_bytes());
                for (i, (t, d)) in self.disks.iter().enumerate() {
                    let e = 8 + i * 16;
                    b[e] = *t;
                    b[e + 4] = info::LD_OPTIMAL;
                    b[e + 8..e + 16].copy_from_slice(&((d.len() / 512) as u64).to_le_bytes());
                }
                let n = len.min(b.len());
                self.dev_write(addr, &b[..n]);
                (frame::STAT_OK, 0)
            }
            frame::DCMD_CTRL_CACHE_FLUSH => {
                self.flushes += 1;
                (frame::STAT_OK, 0)
            }
            _ => (frame::STAT_INVALID_DCMD, 0),
        }
    }

    fn ld_io(&mut self, f: &[u8]) -> (u8, u8) {
        let target = f[4];
        let sectors = get32(f, 20) as usize;
        let lba = get32(f, 32) as u64 | ((get32(f, 36) as u64) << 32);
        let (addr, len) = Sim::sg(f, 40);
        let Some(di) = self.disks.iter().position(|d| d.0 == target) else {
            return (frame::STAT_DEVICE_NOT_FOUND, 0);
        };
        let size = self.disks[di].1.len() as u64 / 512;
        if lba + sectors as u64 > size || len != sectors * 512 {
            return (frame::STAT_LD_LBA_OUT_OF_RANGE, 0);
        }
        let off = lba as usize * 512;
        if f[0] == frame::CMD_LD_WRITE {
            let mut buf = std::vec![0u8; len];
            if !self.dev_read(addr, &mut buf) {
                return (frame::STAT_SCSI_IO_FAILED, 0);
            }
            self.disks[di].1[off..off + len].copy_from_slice(&buf);
        } else {
            let buf = self.disks[di].1[off..off + len].to_vec();
            if !self.dev_write(addr, &buf) {
                return (frame::STAT_SCSI_IO_FAILED, 0);
            }
        }
        (frame::STAT_OK, 0)
    }

    fn pass(&mut self, f: &[u8]) -> (u8, u8) {
        let target = f[4];
        let Some(di) = self.disks.iter().position(|d| d.0 == target) else {
            return (frame::STAT_DEVICE_NOT_FOUND, 0);
        };
        if f[32] != 0x9E || f[33] != 0x10 {
            return (frame::STAT_SCSI_DONE_WITH_ERROR, 0x02);
        }
        let blocks = self.disks[di].1.len() as u64 / 512;
        let mut b = [0u8; 32];
        b[0..8].copy_from_slice(&(blocks - 1).to_be_bytes());
        b[8..12].copy_from_slice(&512u32.to_be_bytes());
        let (addr, len) = Sim::sg(f, 48);
        self.dev_write(addr, &b[..len.min(32)]);
        (frame::STAT_OK, 0)
    }

    fn status_word(&self) -> u32 {
        if let Some(f) = self.forced {
            return f;
        }
        self.state | (self.sge << 16) | self.cmds
    }
}

impl Hal for Sim {
    fn read32(&mut self, off: u32) -> u32 {
        match off {
            regs::OSP0 | regs::OMSG0 => self.status_word(),
            regs::OSTS => {
                if self.intr_enabled() && self.doorbell {
                    regs::GEN2_REPLY
                } else {
                    0
                }
            }
            regs::OMSK => self.intr_mask,
            _ => 0,
        }
    }

    fn write32(&mut self, off: u32, val: u32) {
        match off {
            regs::IDB => {
                self.doorbells.push(val);
                self.acked |= val;
                if val & regs::IDB_ABORT != 0 {
                    self.queued.clear();
                }
                if val & regs::IDB_READY != 0 {
                    self.soft_reset();
                }
            }
            regs::OMSK => {
                self.intr_mask = val;
                if val == regs::MASK_ALL {
                    self.soft_reset(); // QEMU: masking everything resets the firmware
                }
            }
            regs::ODCR0 => self.doorbell = false,
            regs::IQP => {
                let addr = (val & !0x1F) as u64;
                if self.deferred {
                    self.queued.push(addr);
                } else {
                    self.process(addr);
                }
            }
            _ => {}
        }
    }

    fn dma_read(&mut self, addr: u64, out: &mut [u8]) {
        assert!(self.in_window(addr, out.len()), "driver read outside its window: {addr:#x}");
        let o = (addr - BASE) as usize;
        out.copy_from_slice(&self.mem[o..o + out.len()]);
    }

    fn dma_write(&mut self, addr: u64, data: &[u8]) {
        assert!(self.in_window(addr, data.len()), "driver write outside its window: {addr:#x}");
        let o = (addr - BASE) as usize;
        self.mem[o..o + data.len()].copy_from_slice(data);
    }

    fn now_ms(&mut self) -> u64 {
        self.now
    }

    fn relax(&mut self) {
        self.now += 10;
        // Boot script: firmware states that need a doorbell wait for it.
        if !self.script.is_empty() {
            let need = match self.state >> 28 {
                0x6 => regs::IDB_CLEAR_HANDSHAKE,
                0x9 => regs::IDB_HOTPLUG,
                _ => 0,
            };
            if need == 0 || self.acked & need != 0 {
                self.script.remove(0);
                self.acked = 0;
                self.state = self.script.first().copied().unwrap_or(READY);
            }
        }
        self.run_device();
    }
}
