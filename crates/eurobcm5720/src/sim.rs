//! A simulated BCM5720 for the host tests: registers, NIC SRAM behind the
//! window, the APE lock and event protocol, the bootcode handshake on a core
//! reset, NVRAM, the PHY behind MI_COMM, and the data path: the send ring is
//! read on a producer-mailbox write, received frames go into posted standard
//! buffers and onto the return ring, and the status block is written into the
//! driver's DMA window. Loopback (MAC or PHY) turns transmitted frames around.
//!
//! The model follows what the driver relies on, measured on a real chip where
//! that was possible (see `docs/SPRINT-PLAN-BCM5720.md`); it is not a full chip.

use std::collections::HashMap;

use super::regs::*;
use super::{Layout, Hal, RET_RING, SEND_RING, STD_RING};

pub struct Sim {
    regs: HashMap<u32, u32>,
    sram: HashMap<u32, u32>,
    ape_regs: HashMap<u32, u32>,
    grants: [u32; 8],
    pub phy: [u16; 32],
    pub nvram: Vec<u8>,
    mem: Vec<u8>,
    base: u64,
    // Device-side ring state.
    std_cons: usize,
    ret_prod: usize,
    tx_cons: usize,
    /// Status tag of the last status block written.
    pub tag: u32,
    // Observations.
    pub resets: u32,
    pub writes: u64,
    pub ape_writes: u64,
    pub ape_events: Vec<u32>,
    pub wire: Vec<Vec<u8>>,
    pub foreign_reads: Vec<u64>,
    pub foreign_writes: Vec<u64>,
    pub mbox_irq_writes: Vec<u32>,
    pub coal_now: bool,
    pub reset_held_grc_lock: bool,
    pub reset_held_nvram_lock: bool,
    pub quiesced_before_reset: bool,
    // Faults to inject.
    pub dead_bootcode: bool,
    pub ape_never_grants: bool,
    pub no_ape: bool,
    pub dead_tx: bool,
    pub corrupt_loopback: bool,
    pub drop_loopback: bool,
}

impl Sim {
    pub fn new(mac: [u8; 6], base: u64, len: usize) -> Sim {
        let mut s = Sim {
            regs: HashMap::new(),
            sram: HashMap::new(),
            ape_regs: HashMap::new(),
            grants: [0; 8],
            phy: [0; 32],
            nvram: vec![0; 0x200],
            mem: vec![0; len],
            base,
            std_cons: 0,
            ret_prod: 0,
            tx_cons: 0,
            tag: 0,
            resets: 0,
            writes: 0,
            ape_writes: 0,
            ape_events: Vec::new(),
            wire: Vec::new(),
            foreign_reads: Vec::new(),
            foreign_writes: Vec::new(),
            mbox_irq_writes: Vec::new(),
            coal_now: false,
            reset_held_grc_lock: false,
            reset_held_nvram_lock: false,
            quiesced_before_reset: false,
            dead_bootcode: false,
            ape_never_grants: false,
            no_ape: false,
            dead_tx: false,
            corrupt_loopback: false,
            drop_loopback: false,
        };
        // What the test chip showed before any driver touched it.
        s.regs.insert(PCI_ID, 0x165F_14E4);
        s.regs.insert(PCI_CMD, 0x0010_0006);
        s.regs.insert(PRODID_ASICREV, CHIPID_5720_A0);
        s.regs.insert(CPMU_STATUS, 0x001B_2024);
        s.regs.insert(MISC_HOST_CTRL, 0xF000_029A);
        s.regs.insert(GRC_MODE, 0x1001_0034);
        s.regs.insert(MAC_MODE, 0x1920_4808);
        s.regs.insert(MI_MODE, 0x0000_8000);
        s.regs.insert(MAC_ADDR0_HI, (mac[0] as u32) << 8 | mac[1] as u32);
        s.regs.insert(MAC_ADDR0_LO, u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]));
        s.sram.insert(SRAM_FW_MBOX, !FW_MAGIC);
        s.sram.insert(SRAM_DATA_SIG, FW_MAGIC);
        s.sram.insert(SRAM_DATA_CFG, 0x002B_1194); // ASF enabled
        s.sram.insert(SRAM_MAC_HI, SRAM_MAC_SIG << 16 | (mac[0] as u32) << 8 | mac[1] as u32);
        s.sram.insert(SRAM_MAC_LO, u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]));
        s.phy[MII_BMCR as usize] = 0x1140;
        s.phy[MII_BMSR as usize] = 0x7949;
        s.phy[MII_PHYSID1 as usize] = 0x0362;
        s.phy[MII_PHYSID2 as usize] = 0x5F60;
        s.phy[MII_AUX_STAT as usize] = 0x1000;
        s.nvram[0..4].copy_from_slice(&NVRAM_MAGIC.to_be_bytes());
        let o = NVRAM_MAC_FUNC0 as usize;
        s.nvram[o + 2..o + 8].copy_from_slice(&mac);
        s
    }

    pub fn reg(&self, off: u32) -> u32 {
        *self.regs.get(&off).unwrap_or(&0)
    }
    pub fn set_reg(&mut self, off: u32, v: u32) {
        self.regs.insert(off, v);
    }
    pub fn sram(&self, off: u32) -> u32 {
        *self.sram.get(&off).unwrap_or(&0)
    }
    pub fn set_sram(&mut self, off: u32, v: u32) {
        self.sram.insert(off, v);
    }
    pub fn ape(&self, off: u32) -> u32 {
        *self.ape_regs.get(&off).unwrap_or(&0)
    }
    pub fn locks_held(&self) -> usize {
        self.grants.iter().filter(|&&g| g != 0).count()
    }
    pub fn link_up_1000fd(&mut self) {
        self.phy[MII_BMSR as usize] |= BMSR_LSTATUS;
        self.phy[MII_AUX_STAT as usize] = 0x0700;
    }

    fn inside(&self, at: u64, len: usize) -> bool {
        at >= self.base && at + len as u64 <= self.base + self.mem.len() as u64
    }
    fn mem_read(&mut self, at: u64, len: usize) -> Option<Vec<u8>> {
        if !self.inside(at, len) {
            self.foreign_reads.push(at);
            return None;
        }
        let o = (at - self.base) as usize;
        Some(self.mem[o..o + len].to_vec())
    }
    fn mem_write(&mut self, at: u64, data: &[u8]) {
        if !self.inside(at, data.len()) {
            self.foreign_writes.push(at);
            return;
        }
        let o = (at - self.base) as usize;
        self.mem[o..o + data.len()].copy_from_slice(data);
    }
    fn mem_u32(&mut self, at: u64) -> u32 {
        let b = self.mem_read(at, 4).unwrap_or(vec![0; 4]);
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    pub fn status_word(&mut self) -> u32 {
        let at = self.status_addr();
        self.mem_u32(at)
    }
    fn status_addr(&self) -> u64 {
        (self.reg(HCC_STATUSBLK_HI) as u64) << 32 | self.reg(HCC_STATUSBLK_LO) as u64
    }
    fn write_status(&mut self) {
        if self.reg(HCC_MODE) & MODE_ENABLE == 0 {
            return;
        }
        let at = self.status_addr();
        let st = self.mem_u32(at) | STATUS_UPDATED;
        self.mem_write(at, &st.to_le_bytes());
        self.tag = (self.tag + 1) & 0xFF;
        self.mem_write(at + 4, &self.tag.to_le_bytes());
        self.mem_write(at + 10, &((self.std_cons % STD_RING) as u16).to_le_bytes());
        self.mem_write(at + 16, &((self.ret_prod % RET_RING) as u16).to_le_bytes());
        self.mem_write(at + 18, &((self.tx_cons % SEND_RING) as u16).to_le_bytes());
    }

    /// The (address, opaque) of standard-ring descriptor `slot`.
    pub fn std_bd(&mut self, l: Layout, slot: usize) -> (u64, u32) {
        let at = l.std_ring + slot as u64 * 32;
        let hi = self.mem_u32(at) as u64;
        let lo = self.mem_u32(at + 4) as u64;
        (hi << 32 | lo, self.mem_u32(at + 28))
    }

    fn loopback(&self) -> bool {
        self.reg(MAC_MODE) & MAC_MODE_INT_LOOPBACK != 0 || self.phy[MII_BMCR as usize] & BMCR_LOOPBACK != 0
    }

    /// A frame arrives from the wire (or the loopback). False when no posted
    /// buffer was free (the chip drops it) or the receiver is off.
    pub fn inject_rx(&mut self, frame: &[u8]) -> bool {
        if self.reg(RX_MODE) & RX_MODE_ENABLE == 0 {
            return false;
        }
        let host_prod = self.reg(MBOX_RX_STD_PROD) as usize % STD_RING;
        if self.std_cons % STD_RING == host_prod {
            return false;
        }
        let slot = self.std_cons % STD_RING;
        let ring = (self.reg(RX_STD_RCB_HADDR_HI) as u64) << 32 | self.reg(RX_STD_RCB_HADDR_LO) as u64;
        let bd = ring + slot as u64 * 32;
        let addr = (self.mem_u32(bd) as u64) << 32 | self.mem_u32(bd + 4) as u64;
        let opaque = self.mem_u32(bd + 28);
        let mut data = frame.to_vec();
        data.extend_from_slice(&[0xFC; 4]); // the FCS
        self.mem_write(addr, &data);
        self.std_cons += 1;
        self.push_return(addr, data.len() as u32, slot as u32, BD_FLAG_END, opaque);
        true
    }

    /// A return-ring entry exactly as given (a lying or broken chip).
    pub fn inject_return(&mut self, addr: u64, len: u32, flags: u32, opaque: u32) {
        self.push_return(addr, len, 0, flags | BD_FLAG_END, opaque);
    }

    fn push_return(&mut self, addr: u64, len: u32, std_slot: u32, flags: u32, opaque: u32) {
        let ring = (self.sram(SRAM_RX_RETURN_RCB) as u64) << 32 | self.sram(SRAM_RX_RETURN_RCB + 4) as u64;
        let at = ring + (self.ret_prod % RET_RING) as u64 * 32;
        let w = [(addr >> 32) as u32, addr as u32, std_slot << 16 | len, flags, 0, 0, 0, opaque];
        let mut b = Vec::new();
        for v in w {
            b.extend_from_slice(&v.to_le_bytes());
        }
        self.mem_write(at, &b);
        self.ret_prod += 1;
        self.write_status();
    }

    fn process_tx(&mut self, prod: usize) {
        if self.dead_tx || self.reg(TX_MODE) & TX_MODE_ENABLE == 0 {
            return;
        }
        let ring = (self.sram(SRAM_SEND_RCB) as u64) << 32 | self.sram(SRAM_SEND_RCB + 4) as u64;
        while self.tx_cons % SEND_RING != prod % SEND_RING {
            let at = ring + (self.tx_cons % SEND_RING) as u64 * 16;
            let addr = (self.mem_u32(at) as u64) << 32 | self.mem_u32(at + 4) as u64;
            let len = (self.mem_u32(at + 8) >> 16) as usize;
            self.tx_cons += 1;
            let Some(mut f) = self.mem_read(addr, len) else { continue };
            if self.loopback() {
                if self.drop_loopback {
                    continue;
                }
                if self.corrupt_loopback {
                    let last = f.len() - 1;
                    f[last] ^= 0xFF;
                }
                self.inject_rx(&f);
            } else {
                self.wire.push(f);
            }
        }
        self.write_status();
    }

    fn core_reset(&mut self) {
        self.reset_held_grc_lock = self.grants[APE_LOCK_GRC as usize] != 0 || self.no_ape;
        self.reset_held_nvram_lock = self.reg(NVRAM_SWARB) & NVRAM_SWARB_GNT1 != 0;
        self.quiesced_before_reset = self.reg(TX_MODE) & TX_MODE_ENABLE == 0 && self.reg(RDMA_MODE) & MODE_ENABLE == 0;
        self.resets += 1;
        for reg in [TX_MODE, RX_MODE, RDMA_MODE, WDMA_MODE, SDI_MODE, SBDI_MODE, SDC_MODE, SBDC_MODE, SRS_MODE, RDBDI_MODE,
                    RBDI_MODE, RDC_MODE, RBDC_MODE, RXLP_MODE, HCC_MODE, BMAN_MODE, NVRAM_SWARB, MBOX_RX_STD_PROD,
                    MBOX_TX_HOST_PROD0, MSI_MODE] {
            self.regs.insert(reg, 0);
        }
        self.regs.insert(MBOX_IRQ0, 1);
        self.std_cons = 0;
        self.ret_prod = 0;
        self.tx_cons = 0;
        if !self.dead_bootcode && self.sram(SRAM_FW_MBOX) == FW_MAGIC {
            self.sram.insert(SRAM_FW_MBOX, !FW_MAGIC);
        }
    }

    fn mi_comm(&mut self, v: u32) {
        let reg = ((v >> 16) & 0x1F) as usize;
        let phy = (v >> 21) & 0x1F;
        let mut r = v & !MI_COMM_BUSY;
        if phy != 1 {
            r |= MI_COMM_READ_FAIL;
        } else if v & MI_COMM_READ != 0 {
            r = (r & !0xFFFF) | self.phy[reg] as u32;
        } else if v & MI_COMM_WRITE != 0 {
            self.phy[reg] = v as u16;
        }
        self.regs.insert(MI_COMM, r);
    }

    fn nvram_cmd(&mut self) {
        let a = (self.reg(NVRAM_ADDR) & !3) as usize;
        let w = if a + 4 <= self.nvram.len() {
            u32::from_be_bytes([self.nvram[a], self.nvram[a + 1], self.nvram[a + 2], self.nvram[a + 3]])
        } else {
            0xFFFF_FFFF
        };
        self.regs.insert(NVRAM_RDDATA, w);
        self.regs.insert(NVRAM_CMD, NVRAM_CMD_DONE);
    }
}

impl Hal for Sim {
    fn reg_read(&mut self, off: u32) -> u32 {
        if (SRAM_WINDOW..SRAM_WINDOW + SRAM_WINDOW_LEN).contains(&off) {
            return self.sram(self.reg(MEM_WIN_BASE) + off - SRAM_WINDOW);
        }
        match off {
            MAC_TX_STATUS => {
                if self.loopback() || self.phy[MII_BMSR as usize] & BMSR_LSTATUS != 0 {
                    TX_STATUS_LINK_UP
                } else {
                    0
                }
            }
            _ => self.reg(off),
        }
    }

    fn reg_write(&mut self, off: u32, val: u32) {
        self.writes += 1;
        if (SRAM_WINDOW..SRAM_WINDOW + SRAM_WINDOW_LEN).contains(&off) {
            let at = self.reg(MEM_WIN_BASE) + off - SRAM_WINDOW;
            self.sram.insert(at, val);
            return;
        }
        match off {
            GRC_MISC_CFG => {
                self.regs.insert(off, val & !MISC_CFG_CORE_RESET);
                if val & MISC_CFG_CORE_RESET != 0 {
                    self.core_reset();
                }
            }
            NVRAM_SWARB => {
                let cur = self.reg(NVRAM_SWARB);
                if val & NVRAM_SWARB_SET1 != 0 {
                    self.regs.insert(off, cur | NVRAM_SWARB_GNT1);
                } else if val & NVRAM_SWARB_CLR1 != 0 {
                    self.regs.insert(off, cur & !NVRAM_SWARB_GNT1);
                }
            }
            NVRAM_CMD => {
                if val == NVRAM_CMD_READ {
                    self.nvram_cmd();
                }
            }
            MI_COMM => self.mi_comm(val),
            HCC_MODE => {
                self.regs.insert(off, val & !HCC_MODE_NOW);
                if val & HCC_MODE_NOW != 0 {
                    self.coal_now = true;
                    self.write_status();
                }
            }
            MBOX_TX_HOST_PROD0 => {
                self.regs.insert(off, val);
                self.process_tx(val as usize);
            }
            MBOX_IRQ0 => {
                self.mbox_irq_writes.push(val);
                self.regs.insert(off, val);
            }
            _ => {
                self.regs.insert(off, val);
            }
        }
    }

    fn ape_read(&mut self, off: u32) -> u32 {
        if self.no_ape {
            return 0;
        }
        match off {
            APE_SEG_SIG => APE_SEG_MAGIC,
            APE_FW_STATUS => 0x300,
            APE_FW_FEATURES => APE_FEATURE_NCSI,
            APE_FW_VERSION => 0x0105_0C00,
            o if (APE_LOCK_GRANT..APE_LOCK_GRANT + 32).contains(&o) => self.grants[((o - APE_LOCK_GRANT) / 4) as usize],
            _ => self.ape(off),
        }
    }

    fn ape_write(&mut self, off: u32, val: u32) {
        self.ape_writes += 1;
        if (APE_LOCK_REQ..APE_LOCK_REQ + 32).contains(&off) {
            if !self.ape_never_grants {
                self.grants[((off - APE_LOCK_REQ) / 4) as usize] = val;
            }
        } else if (APE_LOCK_GRANT..APE_LOCK_GRANT + 32).contains(&off) {
            self.grants[((off - APE_LOCK_GRANT) / 4) as usize] = 0;
        } else if off == APE_EVENT_STATUS {
            // The firmware services the event at once.
            self.ape_events.push(val & !APE_EVENT_PENDING);
            self.ape_regs.insert(off, val & !APE_EVENT_PENDING);
        } else {
            self.ape_regs.insert(off, val);
        }
    }

    fn dma_write(&mut self, iova: u64, data: &[u8]) {
        assert!(self.inside(iova, data.len()), "driver wrote outside its DMA window: {iova:#x}");
        let o = (iova - self.base) as usize;
        self.mem[o..o + data.len()].copy_from_slice(data);
    }

    fn dma_read(&mut self, iova: u64, out: &mut [u8]) {
        assert!(self.inside(iova, out.len()), "driver read outside its DMA window: {iova:#x}");
        let o = (iova - self.base) as usize;
        out.copy_from_slice(&self.mem[o..o + out.len()]);
    }

    fn delay_us(&mut self, _us: u32) {}
    fn sleep_ms(&mut self, _ms: u32) {}
}
