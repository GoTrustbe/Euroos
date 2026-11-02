//! EuroBCM5720: the Broadcom BCM5720 (NetXtreme, `14e4:165f`) driver logic,
//! free of hardware access.
//!
//! What a 5720 needs, in the order this crate does it (`Bcm5720::init`):
//!
//! 1. **Identify** the chip through the PCI config shadow in BAR0 (ids, the
//!    gen-2 product id), the PCI function (CPMU status) and the management
//!    processor (APE, BAR2): signature, firmware state, NCSI/DASH.
//! 2. **Handshake and reset.** Clear stale APE locks, tell the APE and the
//!    bootcode a driver is starting, take the NVRAM and APE GRC locks, write
//!    `KevT` into the firmware mailbox, reset the core clocks, and wait until
//!    the bootcode answers `!KevT`. Then the driver state START_DONE.
//! 3. **Block init**: byte swapping, DMA control, buffer manager, flow-through
//!    queues, the ring control blocks (standard RX producer ring in host
//!    memory, send ring and receive return ring control blocks in NIC SRAM),
//!    host coalescing with a 32-byte status block, the MAC, the DMA engines
//!    and every send/receive state machine.
//! 4. **Rings**: 64 RX buffers posted on the 512-entry standard ring, an empty
//!    1024-entry return ring, an empty 512-entry send ring.
//!
//! The driver talks to the device only through [`Hal`]: BAR0 registers, BAR2
//! (APE) registers, the driver's own DMA window, and two delays. The userspace
//! driver (`userland/bcm5720d`) implements it with volatile accesses to its
//! granted windows; the host tests use a simulated chip (`sim`). Nothing here
//! is `unsafe`.
//!
//! Sources (hardware facts only, see `regs`): FreeBSD `bge` and Linux `tg3`,
//! read to understand the chip; no code from either is used. The values the
//! test chip showed are recorded in `docs/SPRINT-PLAN-BCM5720.md`.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod regs;
#[cfg(test)]
mod sim;

use regs::*;

/// Entries in the standard RX producer ring (5717+: programmable; bge uses 512).
pub const STD_RING: usize = 512;
/// Entries in the receive return ring.
pub const RET_RING: usize = 1024;
/// Entries in the send ring (fixed at 512 on the 5717 family).
pub const SEND_RING: usize = 512;
/// RX buffers the driver keeps posted.
pub const NRX: usize = 64;
/// TX buffers (one frame each).
pub const NTX: usize = 32;
/// Bytes per buffer.
pub const BUF_SIZE: usize = 2048;
/// Largest frame the chip may place in a standard buffer (with FCS).
pub const RX_BUF_LEN: u32 = 1536;
/// Ethernet frame check sequence, included in the return descriptor's length.
pub const FCS: usize = 4;
const PAGE: u64 = 4096;
const RX_BD: u64 = 32;
const TX_BD: u64 = 16;

/// Bounded waits (counted in `Hal::delay_us(10)` steps unless noted).
pub const FW_POLLS: u32 = 100_000; // ~1 s for the bootcode handshake
pub const ENABLE_POLLS: u32 = 2_000; // ~20 ms for a block to start or stop
pub const APE_POLLS: u32 = 20_000; // 50 us each: ~1 s for an APE lock
pub const MI_POLLS: u32 = 5_000;
pub const TX_POLLS: u32 = 20_000;

/// The device's view of the world.
pub trait Hal {
    /// BAR0 register read / write (offset into the 64 KiB register window).
    fn reg_read(&mut self, off: u32) -> u32;
    fn reg_write(&mut self, off: u32, val: u32);
    /// BAR2 (APE) register read / write. A driver without the APE window
    /// returns 0 from `ape_read`, which reads as "no APE firmware".
    fn ape_read(&mut self, off: u32) -> u32;
    fn ape_write(&mut self, off: u32, val: u32);
    /// Write / read the driver's DMA window at bus address `iova`.
    fn dma_write(&mut self, iova: u64, data: &[u8]);
    fn dma_read(&mut self, iova: u64, out: &mut [u8]);
    /// Busy-wait about `us` microseconds.
    fn delay_us(&mut self, us: u32);
    /// Wait at least `ms` milliseconds (may give the CPU away).
    fn sleep_ms(&mut self, ms: u32);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The DMA window is too small or not page-aligned.
    BadLayout,
    /// Not a 14e4:165f with a 5720 product id.
    NotBcm5720 { id: u32, chip: u32 },
    /// An APE lock was not granted within a second (management firmware busy).
    ApeLock(u32),
    /// The NVRAM arbitration lock was not granted.
    NvramLock,
    /// The bootcode never answered the firmware mailbox after the reset.
    FirmwareTimeout(u32),
    /// Memory decode was off after the core reset (the driver cannot reach
    /// the chip any more; on the 5720 the reset does not do this).
    MemoryDecodeLost,
    /// A block did not start (buffer manager, flow-through queues, coalescing).
    BlockTimeout(u32),
    /// No valid MAC address anywhere.
    NoMac,
    /// A frame of fewer than 14 or more than 1518 bytes.
    FrameSize,
    /// The chip never consumed a send descriptor.
    TxTimeout,
    /// A PHY register access never completed.
    PhyTimeout,
}

impl Error {
    /// A number for the `INIT_FAILED` report: kind << 32 | detail.
    pub fn code(&self) -> u64 {
        match *self {
            Error::BadLayout => 1 << 32,
            Error::NotBcm5720 { chip, .. } => 2 << 32 | chip as u64,
            Error::ApeLock(l) => 3 << 32 | l as u64,
            Error::NvramLock => 4 << 32,
            Error::FirmwareTimeout(v) => 5 << 32 | v as u64,
            Error::MemoryDecodeLost => 6 << 32,
            Error::BlockTimeout(r) => 7 << 32 | r as u64,
            Error::NoMac => 8 << 32,
            Error::FrameSize => 9 << 32,
            Error::TxTimeout => 10 << 32,
            Error::PhyTimeout => 11 << 32,
        }
    }
}

/// Where everything sits in the DMA window (bus addresses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub status: u64,
    pub std_ring: u64,
    pub ret_ring: u64,
    pub send_ring: u64,
    pub rx_bufs: u64,
    pub tx_bufs: u64,
}

impl Layout {
    /// Pages: 1 status block, 4 standard ring, 8 return ring, 2 send ring,
    /// 1 spare, 32 RX buffers, 16 TX buffers.
    pub const PAGES: u64 = 64;
    pub const BYTES: u64 = Self::PAGES * PAGE;

    pub fn in_window(base: u64, len: u64) -> Result<Layout, Error> {
        if base % PAGE != 0 || len < Self::BYTES || base.checked_add(len).is_none() {
            return Err(Error::BadLayout);
        }
        Ok(Layout {
            status: base,
            std_ring: base + PAGE,
            ret_ring: base + 5 * PAGE,
            send_ring: base + 13 * PAGE,
            rx_bufs: base + 16 * PAGE,
            tx_bufs: base + 48 * PAGE,
        })
    }

    pub fn rx_buf(&self, i: usize) -> u64 {
        self.rx_bufs + (i * BUF_SIZE) as u64
    }
    pub fn tx_buf(&self, i: usize) -> u64 {
        self.tx_bufs + (i * BUF_SIZE) as u64
    }
    fn std_bd(&self, slot: usize) -> u64 {
        self.std_ring + slot as u64 * RX_BD
    }
    fn ret_bd(&self, slot: usize) -> u64 {
        self.ret_ring + slot as u64 * RX_BD
    }
    fn send_bd(&self, slot: usize) -> u64 {
        self.send_ring + slot as u64 * TX_BD
    }
}

/// What `probe` learned about the chip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChipInfo {
    pub chip_id: u32,
    pub pci_func: u32,
    pub phy_addr: u32,
    /// APE firmware present and ready.
    pub ape: bool,
    pub ape_ncsi: bool,
    pub ape_version: u32,
    /// The bootcode runs ASF (new handshake).
    pub asf: bool,
}

/// Where the MAC came from (for the boot log).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacSource {
    /// The bootcode's SRAM mailbox (it copies the NVRAM value there).
    SramMailbox,
    /// Read from NVRAM by this driver.
    Nvram,
    /// The MAC address registers.
    Registers,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tx: u64,
    pub rx: u64,
    /// Frames the chip flagged as errored, or with a length or buffer index
    /// that cannot be ours: dropped, never read.
    pub rx_dropped: u64,
    pub irqs: u64,
}

/// Link state as the PHY reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub up: bool,
    /// 10, 100 or 1000 when up.
    pub mbps: u32,
    pub full_duplex: bool,
}

/// Which loopback a test ran through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loopback {
    /// BMCR loopback in the PHY at 1000/full: MAC, GMII and the PHY's digital part.
    Phy,
    /// The MAC's internal loopback (deprecated by Broadcom on CPMU chips).
    Mac,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopResult {
    pub sent: u32,
    pub received: u32,
    /// Received frames whose bytes equal what was sent.
    pub matched: u32,
}

/// One initialised BCM5720 with one queue pair.
#[derive(Debug)]
pub struct Bcm5720 {
    layout: Layout,
    info: ChipInfo,
    mac: [u8; 6],
    mac_source: MacSource,
    mac_mode: u32,
    /// Producer counters (free-running; ring slot = counter % ring size).
    std_prod: usize,
    ret_cons: usize,
    tx_prod: usize,
    pub stats: Stats,
}

// ── small helpers ────────────────────────────────────────────────────────────

fn wait_set(hal: &mut impl Hal, reg: u32, bits: u32, polls: u32) -> bool {
    for _ in 0..polls {
        if hal.reg_read(reg) & bits == bits {
            return true;
        }
        hal.delay_us(10);
    }
    false
}

fn wait_clear(hal: &mut impl Hal, reg: u32, bits: u32, polls: u32) -> bool {
    for _ in 0..polls {
        if hal.reg_read(reg) & bits == 0 {
            return true;
        }
        hal.delay_us(10);
    }
    false
}

/// Read a word of NIC SRAM through the 32 KiB window (base back to 0 after).
pub fn sram_read(hal: &mut impl Hal, off: u32) -> u32 {
    hal.reg_write(MEM_WIN_BASE, off & !(SRAM_WINDOW_LEN - 1));
    let v = hal.reg_read(SRAM_WINDOW + (off & (SRAM_WINDOW_LEN - 1)));
    hal.reg_write(MEM_WIN_BASE, 0);
    v
}

pub fn sram_write(hal: &mut impl Hal, off: u32, val: u32) {
    hal.reg_write(MEM_WIN_BASE, off & !(SRAM_WINDOW_LEN - 1));
    hal.reg_write(SRAM_WINDOW + (off & (SRAM_WINDOW_LEN - 1)), val);
    hal.reg_write(MEM_WIN_BASE, 0);
}

fn dma_u32(hal: &mut impl Hal, at: u64) -> u32 {
    let mut b = [0u8; 4];
    hal.dma_read(at, &mut b);
    u32::from_le_bytes(b)
}

fn dma_u16(hal: &mut impl Hal, at: u64) -> u16 {
    let mut b = [0u8; 2];
    hal.dma_read(at, &mut b);
    u16::from_le_bytes(b)
}

fn words(w: &[u32]) -> [u8; 32] {
    let mut b = [0u8; 32];
    for (i, v) in w.iter().enumerate().take(8) {
        b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    b
}

fn valid_mac(m: &[u8; 6]) -> bool {
    *m != [0; 6] && m[0] & 1 == 0 && *m != [0xff; 6]
}

// ── APE (management processor) ───────────────────────────────────────────────

/// The lock bit a function uses (PHY locks always use the function-0 bit).
fn ape_bit(lock: u32, func: u32) -> u32 {
    match lock {
        APE_LOCK_PHY0 | APE_LOCK_PHY1 | 3 | 5 => APE_LOCK_DRIVER0,
        _ if func == 0 => APE_LOCK_DRIVER0,
        _ => 1 << func,
    }
}

/// Take an APE lock. Without APE firmware there is nobody to share with: Ok.
pub fn ape_lock(hal: &mut impl Hal, info: &ChipInfo, lock: u32) -> Result<(), Error> {
    if !info.ape {
        return Ok(());
    }
    let bit = ape_bit(lock, info.pci_func);
    hal.ape_write(APE_LOCK_REQ + 4 * lock, bit);
    for _ in 0..APE_POLLS {
        if hal.ape_read(APE_LOCK_GRANT + 4 * lock) == bit {
            return Ok(());
        }
        hal.delay_us(50);
    }
    hal.ape_write(APE_LOCK_GRANT + 4 * lock, bit); // revoke the request
    Err(Error::ApeLock(lock))
}

pub fn ape_unlock(hal: &mut impl Hal, info: &ChipInfo, lock: u32) {
    if info.ape {
        hal.ape_write(APE_LOCK_GRANT + 4 * lock, ape_bit(lock, info.pci_func));
    }
}

/// Post an event to the APE firmware (waits for the previous one, bounded).
fn ape_event(hal: &mut impl Hal, info: &ChipInfo, event: u32) {
    if !info.ape {
        return;
    }
    for _ in 0..10 {
        if ape_lock(hal, info, APE_LOCK_MEM).is_err() {
            return;
        }
        let st = hal.ape_read(APE_EVENT_STATUS);
        if st & APE_EVENT_PENDING == 0 {
            hal.ape_write(APE_EVENT_STATUS, event | APE_EVENT_PENDING);
            ape_unlock(hal, info, APE_LOCK_MEM);
            hal.ape_write(APE_EVENT, APE_EVENT_1);
            return;
        }
        ape_unlock(hal, info, APE_LOCK_MEM);
        hal.delay_us(100);
    }
}

/// Tell the APE firmware a host driver started (or unloaded).
fn ape_driver_state(hal: &mut impl Hal, info: &ChipInfo, start: bool) {
    if !info.ape {
        return;
    }
    if start {
        let count = if hal.ape_read(APE_HOST_SEG_SIG) == APE_HOST_SEG_MAGIC {
            hal.ape_read(APE_HOST_INIT_COUNT).wrapping_add(1)
        } else {
            0
        };
        hal.ape_write(APE_HOST_INIT_COUNT, count);
        hal.ape_write(APE_HOST_SEG_SIG, APE_HOST_SEG_MAGIC);
        hal.ape_write(APE_HOST_SEG_LEN, APE_HOST_SEG_LEN_MAGIC);
        // Driver id: an "other OS" id with version 1.0 (EuroOS has no
        // registered value; the field is informational for the BMC).
        hal.ape_write(APE_HOST_DRIVER_ID, 0xF000_0000 | (1 << 16));
        hal.ape_write(APE_HOST_BEHAVIOR, APE_HOST_BEHAV_NO_PHYLOCK);
        hal.ape_write(APE_HOST_HEARTBEAT_INT_MS, 0); // no heartbeat expected
        hal.ape_write(APE_HOST_DRVR_STATE, APE_HOST_STATE_START);
        ape_event(hal, info, APE_EVENT_STATE_START | APE_EVENT_DRIVER | APE_EVENT_STATE_CHANGE);
    } else {
        hal.ape_write(APE_HOST_DRVR_STATE, APE_HOST_STATE_UNLOAD);
        ape_event(hal, info, APE_EVENT_STATE_UNLOAD | APE_EVENT_DRIVER | APE_EVENT_STATE_CHANGE);
    }
}

// ── PHY ──────────────────────────────────────────────────────────────────────

fn mi_cmd(hal: &mut impl Hal, cmd: u32) -> Result<u32, Error> {
    hal.reg_write(MI_COMM, cmd | MI_COMM_BUSY);
    for _ in 0..MI_POLLS {
        hal.delay_us(10);
        let v = hal.reg_read(MI_COMM);
        if v & MI_COMM_BUSY == 0 {
            hal.delay_us(5);
            return Ok(hal.reg_read(MI_COMM));
        }
    }
    Err(Error::PhyTimeout)
}

pub fn phy_read(hal: &mut impl Hal, info: &ChipInfo, reg: u32) -> Result<u16, Error> {
    ape_lock(hal, info, APE_LOCK_PHY0 + if info.pci_func == 1 { 2 } else { 0 })?;
    let r = mi_cmd(hal, MI_COMM_READ | (info.phy_addr & 0x1F) << 21 | (reg & 0x1F) << 16);
    ape_unlock(hal, info, APE_LOCK_PHY0 + if info.pci_func == 1 { 2 } else { 0 });
    let v = r?;
    if v & MI_COMM_READ_FAIL != 0 {
        return Err(Error::PhyTimeout);
    }
    Ok(v as u16)
}

pub fn phy_write(hal: &mut impl Hal, info: &ChipInfo, reg: u32, val: u16) -> Result<(), Error> {
    ape_lock(hal, info, APE_LOCK_PHY0 + if info.pci_func == 1 { 2 } else { 0 })?;
    let r = mi_cmd(hal, MI_COMM_WRITE | (info.phy_addr & 0x1F) << 21 | (reg & 0x1F) << 16 | val as u32);
    ape_unlock(hal, info, APE_LOCK_PHY0 + if info.pci_func == 1 { 2 } else { 0 });
    r.map(|_| ())
}

// ── NVRAM ────────────────────────────────────────────────────────────────────

fn nvram_lock(hal: &mut impl Hal) -> Result<(), Error> {
    hal.reg_write(NVRAM_SWARB, NVRAM_SWARB_SET1);
    for _ in 0..8000 {
        if hal.reg_read(NVRAM_SWARB) & NVRAM_SWARB_GNT1 != 0 {
            return Ok(());
        }
        hal.delay_us(20);
    }
    Err(Error::NvramLock)
}

fn nvram_unlock(hal: &mut impl Hal) {
    hal.reg_write(NVRAM_SWARB, NVRAM_SWARB_CLR1);
    let _ = hal.reg_read(NVRAM_SWARB);
}

/// Read one 32-bit NVRAM word (as stored: big-endian bytes). Only for offsets
/// in the first flash page, where every 5720 flash type maps 1:1.
pub fn nvram_read_be(hal: &mut impl Hal, addr: u32) -> Result<[u8; 4], Error> {
    nvram_lock(hal)?;
    let access = hal.reg_read(NVRAM_ACCESS);
    hal.reg_write(NVRAM_ACCESS, access | NVRAM_ACCESS_ENABLE);
    hal.reg_write(NVRAM_ADDR, addr & !3);
    hal.reg_write(NVRAM_CMD, NVRAM_CMD_READ);
    let done = wait_set(hal, NVRAM_CMD, NVRAM_CMD_DONE, 10_000);
    let word = hal.reg_read(NVRAM_RDDATA);
    hal.reg_write(NVRAM_ACCESS, access);
    nvram_unlock(hal);
    if !done {
        return Err(Error::NvramLock);
    }
    Ok(word.to_be_bytes())
}

/// The MAC address stored in NVRAM for `func`, if the image is valid.
pub fn nvram_mac(hal: &mut impl Hal, func: u32) -> Option<[u8; 6]> {
    if u32::from_be_bytes(nvram_read_be(hal, 0).ok()?) != NVRAM_MAGIC {
        return None;
    }
    let off = if func & 1 == 1 { NVRAM_MAC_FUNC1 } else { NVRAM_MAC_FUNC0 };
    let hi = nvram_read_be(hal, off).ok()?;
    let lo = nvram_read_be(hal, off + 4).ok()?;
    let m = [hi[2], hi[3], lo[0], lo[1], lo[2], lo[3]];
    valid_mac(&m).then_some(m)
}

// ── Probe, reset ─────────────────────────────────────────────────────────────

/// Identify the chip and its management firmware. Reads only.
pub fn probe(hal: &mut impl Hal) -> Result<ChipInfo, Error> {
    let id = hal.reg_read(PCI_ID);
    let chip = hal.reg_read(PRODID_ASICREV);
    if id != 0x165F_14E4 || chip >> 12 != ASICREV_5720 {
        return Err(Error::NotBcm5720 { id, chip });
    }
    let func = hal.reg_read(CPMU_STATUS) >> 30;
    let sig = hal.ape_read(APE_SEG_SIG);
    let status = hal.ape_read(APE_FW_STATUS);
    let ape = sig == APE_SEG_MAGIC && status & APE_FW_READY != 0;
    let features = if ape { hal.ape_read(APE_FW_FEATURES) } else { 0 };
    Ok(ChipInfo {
        chip_id: chip,
        pci_func: func,
        phy_addr: func + 1,
        ape,
        ape_ncsi: features & APE_FEATURE_NCSI != 0,
        ape_version: if ape { hal.ape_read(APE_FW_VERSION) } else { 0 },
        asf: false,
    })
}

/// Stop the DMA engines and the MAC of whatever ran before (a crashed
/// instance, the firmware's own setup). Each block's enable bit is cleared
/// and waited for, bounded; a block that does not stop is reset anyway.
fn quiesce(hal: &mut impl Hal) {
    hal.reg_write(MBOX_IRQ0, 1);
    let rx = hal.reg_read(RX_MODE);
    hal.reg_write(RX_MODE, rx & !RX_MODE_ENABLE);
    for reg in [RBDI_MODE, RXLP_MODE, RDBDI_MODE, RDC_MODE, RBDC_MODE, SRS_MODE, SBDI_MODE, SDI_MODE, RDMA_MODE, SDC_MODE, SBDC_MODE] {
        let v = hal.reg_read(reg);
        hal.reg_write(reg, v & !MODE_ENABLE);
        wait_clear(hal, reg, MODE_ENABLE, 100);
    }
    let mm = hal.reg_read(MAC_MODE);
    hal.reg_write(MAC_MODE, mm & !MAC_MODE_TXDMA_ENB);
    let tx = hal.reg_read(TX_MODE);
    hal.reg_write(TX_MODE, tx & !TX_MODE_ENABLE);
    wait_clear(hal, TX_MODE, TX_MODE_ENABLE, 100);
    for reg in [HCC_MODE, WDMA_MODE] {
        let v = hal.reg_read(reg);
        hal.reg_write(reg, v & !MODE_ENABLE);
        wait_clear(hal, reg, MODE_ENABLE, 100);
    }
}

/// The core reset with the firmware handshake around it. Returns whether the
/// bootcode runs ASF (the new handshake).
fn reset(hal: &mut impl Hal, info: &ChipInfo) -> Result<bool, Error> {
    // Before: the bootcode must see the magic, the ASF firmware "driver start".
    sram_write(hal, SRAM_FW_MBOX, FW_MAGIC);
    sram_write(hal, SRAM_DRV_STATE, DRV_STATE_START);
    ape_driver_state(hal, info, true);

    nvram_lock(hal)?; // the reset releases it
    if let Err(e) = ape_lock(hal, info, APE_LOCK_GRC) {
        nvram_unlock(hal);
        return Err(e);
    }
    let keep_mac = hal.reg_read(MAC_MODE) & (MAC_MODE_HALF_DUPLEX | MAC_MODE_PORT_MASK | MAC_MODE_APE_RX_EN | MAC_MODE_APE_TX_EN);
    let keep_grc = hal.reg_read(GRC_MODE) & GRC_KEEP_B2H;
    hal.reg_write(MISC_HOST_CTRL, MHC_INIT);
    hal.reg_write(GRC_FASTBOOT_PC, 0);
    // Full core clock while the bootcode runs (it is slow in link-aware mode).
    hal.reg_write(CPMU_CLCK_ORIDE, CPMU_CLCK_ORIDE_MAC);
    hal.reg_write(GRC_MISC_CFG, MISC_CFG_NO_PCIE_RETRAIN);
    hal.reg_write(GRC_MISC_CFG, MISC_CFG_NO_PCIE_RETRAIN | MISC_CFG_CORE_RESET | MISC_CFG_PRESCALER_66MHZ);
    hal.sleep_ms(100);
    // On the 5720 the core reset keeps the PCI command register (measured);
    // if memory decode were gone nothing below could reach the chip.
    let decode = hal.reg_read(PCI_CMD) & PCI_CMD_MEM != 0;
    let mut answered = 0;
    if decode {
        hal.reg_write(MISC_HOST_CTRL, MHC_INIT);
        hal.reg_write(PCISTATE, PCISTATE_ROM_ENABLE | PCISTATE_ROM_RETRY_ENABLE | if info.ape { PCISTATE_ALLOW_APE } else { 0 });
        hal.reg_write(MARB_MODE, MODE_ENABLE);
        hal.reg_write(GRC_MODE, GRC_SWAP_OPTIONS | keep_grc);
        for _ in 0..FW_POLLS {
            answered = sram_read(hal, SRAM_FW_MBOX);
            if answered == !FW_MAGIC {
                break;
            }
            hal.delay_us(10);
        }
        let mm = hal.reg_read(MAC_MODE);
        hal.reg_write(MAC_MODE, (mm & !(MAC_MODE_HALF_DUPLEX | MAC_MODE_PORT_MASK | MAC_MODE_APE_RX_EN | MAC_MODE_APE_TX_EN)) | keep_mac);
    }
    ape_unlock(hal, info, APE_LOCK_GRC);
    let oride = hal.reg_read(CPMU_CLCK_ORIDE);
    hal.reg_write(CPMU_CLCK_ORIDE, oride & !CPMU_CLCK_ORIDE_MAC);
    if !decode {
        return Err(Error::MemoryDecodeLost);
    }
    if answered != !FW_MAGIC {
        return Err(Error::FirmwareTimeout(answered));
    }
    let asf = sram_read(hal, SRAM_DATA_SIG) == FW_MAGIC && sram_read(hal, SRAM_DATA_CFG) & SRAM_DATA_CFG_ASF != 0;
    if asf {
        sram_write(hal, SRAM_DRV_STATE, DRV_STATE_START);
        sram_write(hal, SRAM_DRV_STATE, DRV_STATE_START_DONE);
    }
    Ok(asf)
}

impl Bcm5720 {
    /// Reset the chip and bring up one RX and one TX ring in `layout`.
    pub fn init(hal: &mut impl Hal, layout: Layout) -> Result<Bcm5720, Error> {
        let mut info = probe(hal)?;
        hal.reg_write(MISC_HOST_CTRL, MHC_INIT);
        if info.ape {
            // Locks a dead instance may still hold (bge: lock init).
            for lock in 0..=APE_LOCK_GPIO {
                hal.ape_write(APE_LOCK_GRANT + 4 * lock, ape_bit(lock, info.pci_func));
            }
        }
        quiesce(hal);
        info.asf = reset(hal, &info)?;
        let (mac, mac_source) = Self::find_mac(hal, &info)?;
        let mut nic = Bcm5720 { layout, info, mac, mac_source, mac_mode: 0, std_prod: 0, ret_cons: 0, tx_prod: 0, stats: Stats::default() };
        nic.chip_init(hal);
        nic.block_init(hal)?;
        Ok(nic)
    }

    fn find_mac(hal: &mut impl Hal, info: &ChipInfo) -> Result<([u8; 6], MacSource), Error> {
        let hi = sram_read(hal, SRAM_MAC_HI);
        if hi >> 16 == SRAM_MAC_SIG {
            let lo = sram_read(hal, SRAM_MAC_LO);
            let m = [(hi >> 8) as u8, hi as u8, (lo >> 24) as u8, (lo >> 16) as u8, (lo >> 8) as u8, lo as u8];
            if valid_mac(&m) {
                return Ok((m, MacSource::SramMailbox));
            }
        }
        if let Some(m) = nvram_mac(hal, info.pci_func) {
            return Ok((m, MacSource::Nvram));
        }
        let hi = hal.reg_read(MAC_ADDR0_HI);
        let lo = hal.reg_read(MAC_ADDR0_LO);
        let m = [(hi >> 8) as u8, hi as u8, (lo >> 24) as u8, (lo >> 16) as u8, (lo >> 8) as u8, lo as u8];
        if valid_mac(&m) {
            return Ok((m, MacSource::Registers));
        }
        Err(Error::NoMac)
    }

    /// Byte swapping, DMA control, the timer prescaler.
    fn chip_init(&mut self, hal: &mut impl Hal) {
        let dma = (hal.reg_read(DMA_RW_CTRL) & !DMA_RW_DIS_CACHE_ALIGNMENT) | DMA_RW_TAGGED_STATUS_WA;
        hal.reg_write(DMA_RW_CTRL, dma);
        let keep = hal.reg_read(GRC_MODE) & GRC_KEEP_B2H;
        let stackup = if self.info.asf { GRC_STACKUP } else { 0 };
        hal.reg_write(GRC_MODE, GRC_SWAP_OPTIONS | keep | GRC_MAC_ATTN_INTR | GRC_HOST_SEND_BDS | GRC_TX_NO_PHDR_CSUM | stackup);
        let misc = hal.reg_read(GRC_MISC_CFG);
        hal.reg_write(GRC_MISC_CFG, (misc & !0xFF) | MISC_CFG_PRESCALER_66MHZ);
    }

    /// Buffer manager, rings, coalescing, MAC, DMA engines, state machines.
    fn block_init(&mut self, hal: &mut impl Hal) -> Result<(), Error> {
        let l = self.layout;
        hal.reg_write(MEM_WIN_BASE, 0);
        // Buffer manager watermarks for a standard MTU (5717 family values).
        hal.reg_write(BMAN_MBUF_READDMA_LOWAT, 0);
        hal.reg_write(BMAN_MBUF_MACRX_LOWAT, 0x2A);
        hal.reg_write(BMAN_MBUF_HIWAT, 0xA0);
        hal.reg_write(BMAN_DESC_LOWAT, 5);
        hal.reg_write(BMAN_DESC_HIWAT, 10);
        hal.reg_write(BMAN_MODE, MODE_ENABLE | BMAN_LOMBUF_ATTN);
        if !wait_set(hal, BMAN_MODE, MODE_ENABLE, ENABLE_POLLS) {
            return Err(Error::BlockTimeout(BMAN_MODE));
        }
        hal.reg_write(FTQ_RESET, 0xFFFF_FFFF);
        hal.reg_write(FTQ_RESET, 0);
        let mut ok = false;
        for _ in 0..ENABLE_POLLS {
            if hal.reg_read(FTQ_RESET) == 0 {
                ok = true;
                break;
            }
            hal.delay_us(10);
        }
        if !ok {
            return Err(Error::BlockTimeout(FTQ_RESET));
        }

        // Host memory: zero the status block and the three rings.
        let zero = [0u8; PAGE as usize];
        for p in 0..16 {
            hal.dma_write(l.status + p * PAGE, &zero);
        }

        // Standard RX producer ring: in host memory, 512 entries, 1536-byte buffers.
        hal.reg_write(RX_STD_RCB_HADDR_HI, (l.std_ring >> 32) as u32);
        hal.reg_write(RX_STD_RCB_HADDR_LO, l.std_ring as u32);
        hal.reg_write(RX_STD_RCB_MAXLEN_FLAGS, (STD_RING as u32) << 16 | RX_BUF_LEN << 2);
        hal.reg_write(RX_STD_RCB_NICADDR, NIC_STD_RING_5717);
        hal.reg_write(MBOX_RX_STD_PROD, 0);
        // No jumbo ring.
        hal.reg_write(RX_JUMBO_RCB_MAXLEN_FLAGS, RCB_FLAG_USE_EXT_RX_BD | RCB_FLAG_DISABLED);
        hal.reg_write(MBOX_RX_JUMBO_PROD, 0);
        hal.reg_write(RBDI_STD_REPL_THRESH, 8);
        hal.reg_write(STD_REPLENISH_LWM, 32);
        hal.reg_write(JMB_REPLENISH_LWM, 16);

        // Send rings: 4 control blocks in SRAM (5717 family); ring 0 is ours.
        for i in 0..4u32 {
            let rcb = SRAM_SEND_RCB + i * 16;
            sram_write(hal, rcb + 8, RCB_FLAG_DISABLED);
            sram_write(hal, rcb + 12, 0);
        }
        sram_write(hal, SRAM_SEND_RCB, (l.send_ring >> 32) as u32);
        sram_write(hal, SRAM_SEND_RCB + 4, l.send_ring as u32);
        sram_write(hal, SRAM_SEND_RCB + 12, NIC_SEND_RING_5717);
        sram_write(hal, SRAM_SEND_RCB + 8, (SEND_RING as u32) << 16);
        hal.reg_write(MBOX_TX_HOST_PROD0, 0);

        // Receive return rings: 16 control blocks; ring 0 is ours.
        for i in 0..16u32 {
            let rcb = SRAM_RX_RETURN_RCB + i * 16;
            sram_write(hal, rcb, 0);
            sram_write(hal, rcb + 4, 0);
            sram_write(hal, rcb + 8, RCB_FLAG_DISABLED);
            sram_write(hal, rcb + 12, 0);
            hal.reg_write(MBOX_RX_CONS0 + i * 8, 0);
        }
        sram_write(hal, SRAM_RX_RETURN_RCB, (l.ret_ring >> 32) as u32);
        sram_write(hal, SRAM_RX_RETURN_RCB + 4, l.ret_ring as u32);
        sram_write(hal, SRAM_RX_RETURN_RCB + 12, 0);
        sram_write(hal, SRAM_RX_RETURN_RCB + 8, (RET_RING as u32) << 16);

        // MAC address (slot 0; slot 1 may belong to the management firmware),
        // backoff seed, IPG/slot time, MTU.
        let m = self.mac;
        hal.reg_write(MAC_ADDR0_HI, (m[0] as u32) << 8 | m[1] as u32);
        hal.reg_write(MAC_ADDR0_LO, (m[2] as u32) << 24 | (m[3] as u32) << 16 | (m[4] as u32) << 8 | m[5] as u32);
        hal.reg_write(TX_RANDOM_BACKOFF, m.iter().map(|&b| b as u32).sum::<u32>() & 0x3FF);
        let keep = hal.reg_read(TX_LENGTHS) & TX_LENGTHS_KEEP_5720;
        hal.reg_write(TX_LENGTHS, TX_LENGTHS_DEFAULT | keep);
        hal.reg_write(RX_MTU, 1500 + 14 + 4 + 4);

        // Receive list placement: one class, unmatched frames to ring 1 (= our ring 0).
        hal.reg_write(RX_RULES_CFG, 0x08);
        hal.reg_write(RXLP_CFG, 0x181);
        hal.reg_write(RXLP_STATS_ENABLE_MASK, 0x007F_FFFF);
        hal.reg_write(RXLP_STATS_CTL, 1);

        // Host coalescing: an update per descriptor, a 32-byte status block.
        hal.reg_write(HCC_MODE, 0);
        if !wait_clear(hal, HCC_MODE, MODE_ENABLE, ENABLE_POLLS) {
            return Err(Error::BlockTimeout(HCC_MODE));
        }
        hal.reg_write(HCC_RX_COAL_TICKS, 10);
        hal.reg_write(HCC_TX_COAL_TICKS, 10);
        hal.reg_write(HCC_RX_MAX_BDS, 1);
        hal.reg_write(HCC_TX_MAX_BDS, 1);
        hal.reg_write(HCC_RX_MAX_BDS_INT, 1);
        hal.reg_write(HCC_TX_MAX_BDS_INT, 1);
        hal.reg_write(HCC_STATUSBLK_HI, (l.status >> 32) as u32);
        hal.reg_write(HCC_STATUSBLK_LO, l.status as u32);
        hal.reg_write(HCC_MODE, HCC_MODE_STATBLK_32 | MODE_ENABLE);
        hal.reg_write(RBDC_MODE, MODE_ENABLE | MODE_ATTN);
        hal.reg_write(RXLP_MODE, MODE_ENABLE);

        // MAC: DMA on, statistics cleared and on, GMII; the APE keeps its path.
        let ape = if self.info.ape { MAC_MODE_APE_RX_EN | MAC_MODE_APE_TX_EN } else { 0 };
        self.mac_mode = MAC_MODE_TXDMA_ENB | MAC_MODE_RXDMA_ENB | MAC_MODE_RX_STATS_ENB | MAC_MODE_TX_STATS_ENB | MAC_MODE_FRMHDR_DMA_ENB | MAC_MODE_PORT_GMII | ape;
        hal.reg_write(MAC_MODE, self.mac_mode | MAC_MODE_RX_STATS_CLEAR | MAC_MODE_TX_STATS_CLEAR);
        hal.delay_us(40);
        let lc = hal.reg_read(GRC_LOCAL_CTRL);
        hal.reg_write(GRC_LOCAL_CTRL, lc | LOCAL_CTRL_INTR_ON_ATTN);

        // DMA engines.
        hal.reg_write(WDMA_MODE, MODE_ENABLE | WDMA_ALL_ATTNS | WDMA_STATUS_TAG_FIX);
        hal.delay_us(40);
        let rsv = hal.reg_read(RDMA_RSRVCTRL);
        hal.reg_write(RDMA_RSRVCTRL, rsv | RDMA_RSRVCTRL_FIFO_OFLW_FIX);
        let lso = hal.reg_read(RDMA_LSO_CRPTEN_CTRL);
        hal.reg_write(RDMA_LSO_CRPTEN_CTRL, lso | RDMA_LSO_BLEN_BD_512 | RDMA_LSO_BLEN_LSO_4K);
        let keep = hal.reg_read(RDMA_MODE) & RDMA_H2BNC_VLAN_DET;
        hal.reg_write(RDMA_MODE, (MODE_ENABLE | RDMA_ALL_ATTNS | RDMA_FIFO_LONG_BURST | keep) & !RDMA_MULT_DMA_RD_DIS);
        hal.delay_us(40);
        // 5719/5720 erratum: a read DMA channel that reports a length above a
        // standard frame needs the TX length workaround.
        let mut long = false;
        for ch in 0..2 {
            let v = hal.reg_read(RDMA_LENGTH + ch * 4);
            if v & 0xFFFF > 1536 || v >> 16 > 1536 {
                long = true;
            }
        }
        if long {
            let lso = hal.reg_read(RDMA_LSO_CRPTEN_CTRL);
            hal.reg_write(RDMA_LSO_CRPTEN_CTRL, lso | RDMA_TX_LENGTH_WA_5720);
        }

        // Receive and send state machines.
        hal.reg_write(RDC_MODE, MODE_ENABLE);
        hal.reg_write(RBDI_MODE, MODE_ENABLE | MODE_ATTN);
        hal.reg_write(RDBDI_MODE, MODE_ENABLE);
        hal.reg_write(SBDC_MODE, MODE_ENABLE);
        hal.reg_write(SDC_MODE, MODE_ENABLE);
        hal.reg_write(SDI_MODE, MODE_ENABLE);
        hal.reg_write(SBDI_MODE, MODE_ENABLE);
        hal.reg_write(SRS_MODE, MODE_ENABLE);
        hal.reg_write(SDI_STATS_ENABLE_MASK, 0x007F_FFFF);
        hal.reg_write(SDI_STATS_CTL, 3);

        // Link events: clear, the PHY is polled by the driver (no autopoll).
        hal.reg_write(MAC_STATUS, MAC_STATUS_CLEAR);
        hal.reg_write(MI_STATUS, 0);
        let mi = hal.reg_read(MI_MODE);
        hal.reg_write(MI_MODE, mi & !MI_MODE_AUTOPOLL);
        hal.delay_us(80);
        let ev = hal.reg_read(MAC_EVENT_ENB);
        hal.reg_write(MAC_EVENT_ENB, ev | MAC_EVENT_LINK_CHANGED);

        // Post the RX buffers.
        for i in 0..NRX {
            self.post_rx(hal, i);
        }
        hal.reg_write(MBOX_RX_STD_PROD, (self.std_prod % STD_RING) as u32);

        // Transmitter, receiver.
        let keep = hal.reg_read(TX_MODE) & TX_MODE_KEEP_5720;
        hal.reg_write(TX_MODE, TX_MODE_ENABLE | TX_MODE_MBUF_LOCKUP_FIX | keep);
        hal.delay_us(100);
        let rx = hal.reg_read(RX_MODE) & !RX_MODE_PROMISC;
        hal.reg_write(RX_MODE, rx | RX_MODE_ENABLE);
        hal.delay_us(10);
        hal.reg_write(MAX_RX_FRAME_LOWAT, 1);

        // Interrupts: MSI/MSI-X, one-shot off; unmasked at the PCI level, the
        // interrupt mailbox opened.
        let msi = hal.reg_read(MSI_MODE);
        hal.reg_write(MSI_MODE, msi | MODE_ENABLE | MSI_MODE_ONE_SHOT_DISABLE);
        let mhc = hal.reg_read(MISC_HOST_CTRL);
        hal.reg_write(MISC_HOST_CTRL, (mhc | MHC_CLEAR_INT) & !MHC_MASK_PCI_INT);
        hal.reg_write(MBOX_IRQ0, 0);
        Ok(())
    }

    /// Put RX buffer `buf` on the standard ring at the next producer slot.
    fn post_rx(&mut self, hal: &mut impl Hal, buf: usize) {
        let slot = self.std_prod % STD_RING;
        let a = self.layout.rx_buf(buf);
        let bd = words(&[(a >> 32) as u32, a as u32, (slot as u32) << 16 | RX_BUF_LEN, BD_FLAG_END, 0, 0, 0, buf as u32]);
        hal.dma_write(self.layout.std_bd(slot), &bd);
        self.std_prod += 1;
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }
    pub fn mac_source(&self) -> MacSource {
        self.mac_source
    }
    pub fn info(&self) -> ChipInfo {
        self.info
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }

    fn tx_consumer(&self, hal: &mut impl Hal) -> usize {
        dma_u16(hal, self.layout.status + 18) as usize
    }

    fn rx_producer(&self, hal: &mut impl Hal) -> usize {
        dma_u16(hal, self.layout.status + 16) as usize
    }

    /// Transmit one frame and wait (bounded) until the chip consumed it, so its
    /// buffer can be reused.
    pub fn transmit(&mut self, hal: &mut impl Hal, frame: &[u8]) -> Result<(), Error> {
        if frame.len() < 14 || frame.len() > 1518 {
            return Err(Error::FrameSize);
        }
        let buf = self.layout.tx_buf(self.tx_prod % NTX);
        hal.dma_write(buf, frame);
        self.post_tx(hal, buf, frame.len() as u16)?;
        self.stats.tx += 1;
        Ok(())
    }

    /// Transmit from an arbitrary bus address: how the confinement self-test
    /// plays a malicious driver (the IOMMU must refuse the read).
    pub fn transmit_raw(&mut self, hal: &mut impl Hal, addr: u64, len: u16) -> Result<(), Error> {
        self.post_tx(hal, addr, len)
    }

    fn post_tx(&mut self, hal: &mut impl Hal, addr: u64, len: u16) -> Result<(), Error> {
        let slot = self.tx_prod % SEND_RING;
        let mut bd = [0u8; 16];
        bd[0..4].copy_from_slice(&((addr >> 32) as u32).to_le_bytes());
        bd[4..8].copy_from_slice(&(addr as u32).to_le_bytes());
        bd[8..12].copy_from_slice(&((len as u32) << 16 | BD_FLAG_END).to_le_bytes());
        hal.dma_write(self.layout.send_bd(slot), &bd);
        self.tx_prod += 1;
        let want = self.tx_prod % SEND_RING;
        hal.reg_write(MBOX_TX_HOST_PROD0, want as u32);
        for i in 0..TX_POLLS {
            if self.tx_consumer(hal) == want {
                return Ok(());
            }
            if i % 64 == 0 {
                self.coalesce_now(hal);
            }
            hal.delay_us(1);
        }
        Err(Error::TxTimeout)
    }

    /// Ask host coalescing for a status block update now.
    pub fn coalesce_now(&mut self, hal: &mut impl Hal) {
        hal.reg_write(HCC_MODE, HCC_MODE_STATBLK_32 | MODE_ENABLE | HCC_MODE_NOW);
    }

    /// Post one extra standard-ring descriptor whose buffer is `addr` (the
    /// confinement self-test: the chip's write there must be refused). The
    /// chip caches posted descriptors, so an already posted one cannot be
    /// re-aimed reliably (seen on the real chip); this one is used after the
    /// `NRX` buffers posted before it. Its return entry carries an index that
    /// is no buffer of ours, so `receive` drops it.
    pub fn post_rx_escape(&mut self, hal: &mut impl Hal, addr: u64) {
        let slot = self.std_prod % STD_RING;
        let bd = words(&[(addr >> 32) as u32, addr as u32, (slot as u32) << 16 | RX_BUF_LEN, BD_FLAG_END, 0, 0, 0, u32::MAX]);
        hal.dma_write(self.layout.std_bd(slot), &bd);
        self.std_prod += 1;
        hal.reg_write(MBOX_RX_STD_PROD, (self.std_prod % STD_RING) as u32);
    }

    /// Take the next received frame into `out` (without FCS). Returns its
    /// length, or None when nothing new arrived. A descriptor the chip flagged
    /// as errored, or with a buffer index or length that cannot be ours, is
    /// dropped and counted; whatever we can recycle goes back on the ring.
    pub fn receive(&mut self, hal: &mut impl Hal, out: &mut [u8]) -> Option<(usize, usize)> {
        loop {
            let prod = self.rx_producer(hal) % RET_RING;
            if self.ret_cons % RET_RING == prod {
                return None;
            }
            let at = self.layout.ret_bd(self.ret_cons % RET_RING);
            let mut d = [0u8; 32];
            hal.dma_read(at, &mut d);
            let w = |i: usize| u32::from_le_bytes([d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]]);
            let len = (w(2) & 0xFFFF) as usize;
            let flags = w(3) & 0xFFFF;
            let err = w(5) >> 16;
            let buf = w(7) as usize;
            self.ret_cons += 1;
            hal.reg_write(MBOX_RX_CONS0, (self.ret_cons % RET_RING) as u32);
            if buf >= NRX {
                self.stats.rx_dropped += 1; // not a buffer we posted: nothing to read or recycle
                continue;
            }
            let ok = flags & RXBD_FLAG_ERROR == 0
                && err & !RXBD_ERR_ODD_NIBBLE == 0
                && len >= 14 + FCS
                && len <= RX_BUF_LEN as usize
                && len - FCS <= out.len();
            if ok {
                hal.dma_read(self.layout.rx_buf(buf), &mut out[..len - FCS]);
            }
            self.post_rx(hal, buf);
            hal.reg_write(MBOX_RX_STD_PROD, (self.std_prod % STD_RING) as u32);
            if ok {
                self.stats.rx += 1;
                return Some((len - FCS, buf));
            }
            self.stats.rx_dropped += 1;
        }
    }

    /// An interrupt arrived. The chip runs in tagged-status mode (bge: MSI on
    /// the 5717 family only works with it): writing the status tag of the
    /// block we just read into the interrupt mailbox acknowledges up to that
    /// update and re-arms; a newer update raises a new interrupt.
    pub fn ack_irq(&mut self, hal: &mut impl Hal) {
        self.stats.irqs += 1;
        let st = dma_u32(hal, self.layout.status);
        let tag = dma_u32(hal, self.layout.status + 4) & 0xFF;
        hal.dma_write(self.layout.status, &(st & !STATUS_UPDATED).to_le_bytes());
        hal.reg_write(MBOX_IRQ0, tag << 24);
    }

    /// Link state from the PHY (BMSR twice: the link bit latches low).
    pub fn link(&mut self, hal: &mut impl Hal) -> Result<Link, Error> {
        let _ = phy_read(hal, &self.info, MII_BMSR)?;
        let bmsr = phy_read(hal, &self.info, MII_BMSR)?;
        if bmsr & BMSR_LSTATUS == 0 {
            return Ok(Link { up: false, mbps: 0, full_duplex: false });
        }
        let aux = phy_read(hal, &self.info, MII_AUX_STAT)?;
        let (mbps, full) = match (aux >> 8) & 7 {
            1 => (10, false),
            2 => (10, true),
            3 => (100, false),
            5 => (100, true),
            6 => (1000, false),
            7 => (1000, true),
            _ => (0, false),
        };
        // The MAC's port mode follows the link speed.
        let port = if mbps == 1000 { MAC_MODE_PORT_GMII } else { MAC_MODE_PORT_MII };
        self.mac_mode = (self.mac_mode & !(MAC_MODE_PORT_MASK | MAC_MODE_HALF_DUPLEX)) | port | if full { 0 } else { MAC_MODE_HALF_DUPLEX };
        hal.reg_write(MAC_MODE, self.mac_mode);
        Ok(Link { up: true, mbps, full_duplex: full })
    }

    pub fn phy_id(&mut self, hal: &mut impl Hal) -> Result<u32, Error> {
        let a = phy_read(hal, &self.info, MII_PHYSID1)?;
        let b = phy_read(hal, &self.info, MII_PHYSID2)?;
        Ok((a as u32) << 16 | b as u32)
    }

    /// Send `frames` test frames to ourselves through `mode` loopback and
    /// compare what comes back byte for byte. The chip is put back in normal
    /// mode afterwards (PHY: autonegotiation as before).
    pub fn loopback_test(&mut self, hal: &mut impl Hal, mode: Loopback, frames: u32) -> Result<LoopResult, Error> {
        let mut res = LoopResult::default();
        let saved_bmcr = match mode {
            Loopback::Phy => {
                let b = phy_read(hal, &self.info, MII_BMCR)?;
                phy_write(hal, &self.info, MII_BMCR, BMCR_LOOPBACK | BMCR_FULLDPLX | BMCR_SPEED1000)?;
                hal.delay_us(40);
                hal.reg_write(MAC_MODE, (self.mac_mode & !(MAC_MODE_PORT_MASK | MAC_MODE_HALF_DUPLEX)) | MAC_MODE_PORT_GMII);
                // The MAC sees the PHY's loopback link within ~100 ms.
                for _ in 0..100 {
                    if hal.reg_read(MAC_TX_STATUS) & TX_STATUS_LINK_UP != 0 {
                        break;
                    }
                    hal.sleep_ms(1);
                }
                Some(b)
            }
            Loopback::Mac => {
                hal.reg_write(MAC_MODE, (self.mac_mode & !(MAC_MODE_PORT_MASK | MAC_MODE_HALF_DUPLEX)) | MAC_MODE_PORT_GMII | MAC_MODE_INT_LOOPBACK);
                hal.delay_us(40);
                None
            }
        };
        // Anything already queued is not part of the test.
        let mut out = [0u8; BUF_SIZE];
        while self.receive(hal, &mut out).is_some() {}
        let mut result = Ok(());
        for k in 0..frames {
            let f = loop_frame(self.mac, k);
            let n = 60 + (k as usize * 97) % 1400;
            if let Err(e) = self.transmit(hal, &f[..n]) {
                result = Err(e);
                break;
            }
            res.sent += 1;
            let mut got = None;
            for i in 0..2000 {
                if let Some((len, _)) = self.receive(hal, &mut out) {
                    got = Some(len);
                    break;
                }
                if i % 16 == 0 {
                    self.coalesce_now(hal);
                }
                hal.delay_us(5);
            }
            if let Some(len) = got {
                res.received += 1;
                if len == n && out[..len] == f[..n] {
                    res.matched += 1;
                }
            }
        }
        if let Some(b) = saved_bmcr {
            phy_write(hal, &self.info, MII_BMCR, b & !BMCR_LOOPBACK)?;
        }
        hal.reg_write(MAC_MODE, self.mac_mode);
        hal.delay_us(40);
        result.map(|_| res)
    }

    /// Mask interrupts, stop the engines, tell the firmware the driver unloads.
    pub fn stop(&mut self, hal: &mut impl Hal) {
        quiesce(hal);
        if self.info.asf {
            sram_write(hal, SRAM_DRV_STATE, DRV_STATE_UNLOAD);
            sram_write(hal, SRAM_DRV_STATE, DRV_STATE_UNLOAD_DONE);
        }
        ape_driver_state(hal, &self.info, false);
    }
}

/// What bcm5720d and its supervisor (`kernel/src/udrv_bcm5720.rs`) say to each
/// other beyond `eurodrv`: extra grant words and `Report` codes.
pub mod wire {
    use super::{ChipInfo, LoopResult, Loopback, MacSource};

    /// Words after `eurodrv::abi::DriverInfo` in the info block: the APE
    /// window (BAR2) base and length, and options.
    pub const EXTRA_WORDS: usize = 3;
    /// Option: run the loopback self-test after every start (boot self-test).
    pub const OPT_SELFTEST: u64 = 1;

    pub const PROBE: u8 = 0x51;
    pub const LINK: u8 = 0x52;
    pub const LOOPBACK: u8 = 0x53;
    pub const ESCAPE_DONE: u8 = 0x54;
    pub const RECOVERED: u8 = 0x55;
    pub const INIT_FAILED: u8 = 0x56;
    pub const PHY_ID: u8 = 0x57;

    /// PROBE: a = chip id << 32 | APE version; b = MAC (48 bits) | source << 48
    /// | NVRAM agrees << 52 | APE << 56 | NCSI << 57 | ASF << 58.
    pub fn probe(info: &ChipInfo, mac: [u8; 6], src: MacSource, nvram_agrees: bool) -> (u64, u64) {
        let mut m = 0u64;
        for b in mac {
            m = m << 8 | b as u64;
        }
        let s = match src {
            MacSource::SramMailbox => 0,
            MacSource::Nvram => 1,
            MacSource::Registers => 2,
        };
        let a = (info.chip_id as u64) << 32 | info.ape_version as u64;
        let b = m | s << 48 | (nvram_agrees as u64) << 52 | (info.ape as u64) << 56 | (info.ape_ncsi as u64) << 57 | (info.asf as u64) << 58;
        (a, b)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Probe {
        pub chip_id: u32,
        pub ape_version: u32,
        pub mac: [u8; 6],
        pub source: u8,
        pub nvram_agrees: bool,
        pub ape: bool,
        pub ncsi: bool,
        pub asf: bool,
    }

    pub fn parse_probe(a: u64, b: u64) -> Probe {
        let mut mac = [0u8; 6];
        for (i, m) in mac.iter_mut().enumerate() {
            *m = (b >> (40 - 8 * i)) as u8;
        }
        Probe {
            chip_id: (a >> 32) as u32,
            ape_version: a as u32,
            mac,
            source: ((b >> 48) & 0xF) as u8,
            nvram_agrees: b >> 52 & 1 == 1,
            ape: b >> 56 & 1 == 1,
            ncsi: b >> 57 & 1 == 1,
            asf: b >> 58 & 1 == 1,
        }
    }

    /// LOOPBACK: a = mode << 32 | sent; b = received << 32 | matched.
    pub fn loopback(mode: Loopback, r: &LoopResult) -> (u64, u64) {
        let m = match mode {
            Loopback::Phy => 0u64,
            Loopback::Mac => 1,
        };
        (m << 32 | r.sent as u64, (r.received as u64) << 32 | r.matched as u64)
    }

    pub fn parse_loopback(a: u64, b: u64) -> (Loopback, LoopResult) {
        let mode = if a >> 32 == 1 { Loopback::Mac } else { Loopback::Phy };
        (mode, LoopResult { sent: a as u32, received: (b >> 32) as u32, matched: b as u32 })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn probe_and_loopback_reports_roundtrip() {
            let info = ChipInfo { chip_id: 0x0572_0000, pci_func: 0, phy_addr: 1, ape: true, ape_ncsi: true, ape_version: 0x0105_0C00, asf: true };
            let mac = [0x80, 0x18, 0x44, 0xf2, 0x71, 0x3a];
            let (a, b) = probe(&info, mac, MacSource::SramMailbox, true);
            let p = parse_probe(a, b);
            assert_eq!((p.chip_id, p.ape_version, p.mac, p.source), (0x0572_0000, 0x0105_0C00, mac, 0));
            assert!(p.nvram_agrees && p.ape && p.ncsi && p.asf);
            let (a, b) = probe(&ChipInfo::default(), mac, MacSource::Registers, false);
            let p = parse_probe(a, b);
            assert_eq!(p.source, 2);
            assert!(!p.nvram_agrees && !p.ape && !p.ncsi && !p.asf);
            let r = LoopResult { sent: 16, received: 15, matched: 14 };
            let (a, b) = loopback(Loopback::Mac, &r);
            assert_eq!(parse_loopback(a, b), (Loopback::Mac, r));
        }
    }
}

/// A loopback test frame: to and from `mac`, EtherType 0x88B5 (IEEE local
/// experimental), a tag, the sequence number and a pattern.
pub fn loop_frame(mac: [u8; 6], seq: u32) -> [u8; 1514] {
    let mut f = [0u8; 1514];
    f[0..6].copy_from_slice(&mac);
    f[6..12].copy_from_slice(&mac);
    f[12] = 0x88;
    f[13] = 0xB5;
    f[14..30].copy_from_slice(b"EuroOS bcm5720d ");
    f[30..34].copy_from_slice(&seq.to_be_bytes());
    for (i, b) in f[34..].iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(31).wrapping_add(seq) as u8;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::sim::Sim;
    use super::*;

    const BASE: u64 = 0x0080_0000;
    const MAC: [u8; 6] = [0x80, 0x18, 0x44, 0xf2, 0x71, 0x3a];

    fn up() -> (Sim, Bcm5720) {
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        let n = Bcm5720::init(&mut s, l).unwrap();
        (s, n)
    }

    fn frame(n: usize, tag: u8) -> Vec<u8> {
        let mut f: Vec<u8> = (0..n).map(|i| (i as u8) ^ tag).collect();
        f[0..6].copy_from_slice(&MAC);
        f
    }

    #[test]
    fn init_identifies_resets_and_does_the_firmware_handshake() {
        let (s, n) = up();
        let i = n.info();
        assert_eq!(i.chip_id, CHIPID_5720_A0);
        assert!(i.ape && i.ape_ncsi && i.asf);
        assert_eq!((i.pci_func, i.phy_addr), (0, 1));
        assert_eq!(s.resets, 1);
        assert_eq!(s.sram(SRAM_FW_MBOX), !FW_MAGIC, "bootcode answered");
        assert_eq!(s.sram(SRAM_DRV_STATE), DRV_STATE_START_DONE);
        assert_eq!(s.ape(APE_HOST_SEG_SIG), APE_HOST_SEG_MAGIC);
        assert_eq!(s.ape(APE_HOST_DRVR_STATE), APE_HOST_STATE_START);
        assert!(s.ape_events.iter().any(|e| e & APE_EVENT_STATE_START != 0));
        assert!(s.reset_held_grc_lock, "the core reset ran under the APE GRC lock");
        assert!(s.reset_held_nvram_lock, "and under the NVRAM lock");
        assert_eq!(s.locks_held(), 0, "every lock released");
        assert_eq!(n.mac(), MAC);
        assert_eq!(n.mac_source(), MacSource::SramMailbox);
    }

    #[test]
    fn init_programs_rings_and_engines() {
        let (mut s, n) = up();
        let l = n.layout();
        assert_eq!(s.reg(RX_STD_RCB_HADDR_LO) as u64, l.std_ring);
        assert_eq!(s.reg(RX_STD_RCB_MAXLEN_FLAGS), 512 << 16 | 1536 << 2);
        assert_eq!(s.sram(SRAM_SEND_RCB + 4) as u64, l.send_ring);
        assert_eq!(s.sram(SRAM_SEND_RCB + 8), 512 << 16);
        assert_eq!(s.sram(SRAM_RX_RETURN_RCB + 4) as u64, l.ret_ring);
        assert_eq!(s.sram(SRAM_RX_RETURN_RCB + 8), 1024 << 16);
        for i in 1..16 {
            assert_eq!(s.sram(SRAM_RX_RETURN_RCB + i * 16 + 8), RCB_FLAG_DISABLED);
        }
        assert_eq!(s.reg(HCC_STATUSBLK_LO) as u64, l.status);
        assert_eq!(s.reg(MBOX_RX_STD_PROD), NRX as u32, "64 buffers posted");
        for reg in [TX_MODE, RX_MODE, RDMA_MODE, WDMA_MODE, SDI_MODE, SBDI_MODE, RDBDI_MODE, RBDI_MODE, HCC_MODE, BMAN_MODE] {
            assert!(s.reg(reg) & MODE_ENABLE != 0, "block {reg:#x} enabled");
        }
        let mm = s.reg(MAC_MODE);
        assert!(mm & MAC_MODE_APE_RX_EN != 0 && mm & MAC_MODE_APE_TX_EN != 0, "APE keeps its path");
        assert_eq!(s.reg(MAC_ADDR0_HI), 0x8018);
        assert_eq!(s.reg(MAC_ADDR0_LO), 0x44f2_713a);
        assert_eq!(s.reg(MBOX_IRQ0), 0, "interrupts opened");
        assert!(s.reg(MSI_MODE) & MODE_ENABLE != 0);
        // Every posted descriptor names its own buffer.
        for i in 0..NRX {
            let (addr, opaque) = s.std_bd(l, i);
            assert_eq!((addr, opaque), (l.rx_buf(i), i as u32));
        }
    }

    #[test]
    fn the_wrong_chip_is_refused_before_anything_is_written() {
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        s.set_reg(PRODID_ASICREV, 0x0571_9000);
        assert!(matches!(Bcm5720::init(&mut s, l), Err(Error::NotBcm5720 { .. })));
        assert_eq!(s.writes, 0);
    }

    #[test]
    fn a_silent_bootcode_or_a_busy_ape_fail_cleanly() {
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        s.dead_bootcode = true;
        assert_eq!(Bcm5720::init(&mut s, l).unwrap_err(), Error::FirmwareTimeout(FW_MAGIC));
        assert_eq!(s.locks_held(), 0, "the GRC lock is released on the error path");
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        s.ape_never_grants = true;
        assert_eq!(Bcm5720::init(&mut s, l).unwrap_err(), Error::ApeLock(APE_LOCK_GRC));
        assert_eq!(s.resets, 0, "no reset without the lock");
    }

    #[test]
    fn without_ape_firmware_no_ape_register_is_written() {
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        s.no_ape = true;
        let n = Bcm5720::init(&mut s, l).unwrap();
        assert!(!n.info().ape);
        assert_eq!(s.ape_writes, 0);
        assert_eq!(s.reg(MAC_MODE) & (MAC_MODE_APE_RX_EN | MAC_MODE_APE_TX_EN), 0);
    }

    #[test]
    fn the_mac_falls_back_to_nvram_then_registers() {
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        s.set_sram(SRAM_MAC_HI, 0);
        let n = Bcm5720::init(&mut s, l).unwrap();
        assert_eq!((n.mac(), n.mac_source()), (MAC, MacSource::Nvram));
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        s.set_sram(SRAM_MAC_HI, 0);
        s.nvram[0] = 0; // no valid image
        let n = Bcm5720::init(&mut s, l).unwrap();
        assert_eq!((n.mac(), n.mac_source()), (MAC, MacSource::Registers));
        let mut s = Sim::new([0; 6], BASE, Layout::BYTES as usize);
        s.nvram[0] = 0;
        assert_eq!(Bcm5720::init(&mut s, l).unwrap_err(), Error::NoMac);
    }

    #[test]
    fn nvram_mac_reads_the_big_endian_words() {
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        assert_eq!(nvram_mac(&mut s, 0), Some(MAC));
        assert_eq!(s.reg(NVRAM_SWARB) & NVRAM_SWARB_GNT1, 0, "arbitration released");
    }

    #[test]
    fn transmit_puts_exact_bytes_on_the_wire_across_ring_wraps() {
        let (mut s, mut n) = up();
        for k in 0..600u32 {
            let f = frame(60 + (k as usize % 1400), k as u8);
            n.transmit(&mut s, &f).unwrap();
            assert_eq!(s.wire.last().unwrap(), &f);
        }
        assert_eq!(s.wire.len(), 600);
        assert_eq!(s.reg(MBOX_TX_HOST_PROD0), (600 % SEND_RING) as u32);
        assert_eq!(n.transmit(&mut s, &[0u8; 13]), Err(Error::FrameSize));
        assert_eq!(n.transmit(&mut s, &[0u8; 1519]), Err(Error::FrameSize));
    }

    #[test]
    fn a_dead_send_engine_times_out() {
        let (mut s, mut n) = up();
        s.dead_tx = true;
        assert_eq!(n.transmit(&mut s, &frame(60, 1)), Err(Error::TxTimeout));
    }

    #[test]
    fn receive_returns_frames_in_order_and_recycles_across_both_rings() {
        let (mut s, mut n) = up();
        let mut out = [0u8; BUF_SIZE];
        assert_eq!(n.receive(&mut s, &mut out), None);
        // 1100 frames: more than the 1024-entry return ring, the 512-entry
        // standard ring and the 64 buffers, in batches the device can hold.
        let mut k = 0u32;
        while k < 1100 {
            let batch: Vec<Vec<u8>> = (0..40).map(|j| frame(64 + ((k + j) as usize % 1400), (k + j) as u8)).collect();
            for f in &batch {
                assert!(s.inject_rx(f), "a posted buffer was free");
            }
            for f in &batch {
                let (len, _) = n.receive(&mut s, &mut out).unwrap();
                assert_eq!(&out[..len], &f[..]);
            }
            k += 40;
        }
        assert_eq!(n.stats.rx, 1120);
        assert_eq!(n.receive(&mut s, &mut out), None);
    }

    #[test]
    fn a_full_standard_ring_makes_the_device_drop_not_overwrite() {
        let (mut s, mut n) = up();
        let accepted = (0..80u8).filter(|&k| s.inject_rx(&frame(60, k))).count();
        assert_eq!(accepted, NRX);
        let mut out = [0u8; BUF_SIZE];
        for k in 0..NRX as u8 {
            let (len, _) = n.receive(&mut s, &mut out).unwrap();
            assert_eq!(&out[..len], &frame(60, k)[..]);
        }
    }

    #[test]
    fn lying_or_errored_return_descriptors_are_dropped_not_followed() {
        let (mut s, mut n) = up();
        let mut out = [0u8; BUF_SIZE];
        // A buffer index we never posted: nothing is read anywhere.
        s.inject_return(0, 100, 0, 9999);
        // A length past the buffer.
        s.inject_return(0, 4000, 0, 3);
        // An error flag.
        s.inject_return(0, 100, RXBD_FLAG_ERROR, 4);
        s.inject_rx(&frame(70, 5));
        let reads_before = s.foreign_reads.len();
        let (len, _) = n.receive(&mut s, &mut out).unwrap();
        assert_eq!(len, 70);
        assert_eq!(n.stats.rx_dropped, 3);
        assert_eq!(s.foreign_reads.len(), reads_before);
        // A frame longer than the caller's buffer is dropped too.
        s.inject_rx(&frame(1500, 6));
        let mut small = [0u8; 100];
        assert_eq!(n.receive(&mut s, &mut small), None);
        assert_eq!(n.stats.rx_dropped, 4);
    }

    #[test]
    fn phy_loopback_brings_every_frame_back_intact() {
        let (mut s, mut n) = up();
        let r = n.loopback_test(&mut s, Loopback::Phy, 16).unwrap();
        assert_eq!(r, LoopResult { sent: 16, received: 16, matched: 16 });
        assert!(s.wire.is_empty(), "nothing left the port");
        assert_eq!(s.phy[MII_BMCR as usize] & BMCR_LOOPBACK, 0, "PHY back to normal");
        assert_eq!(s.reg(MAC_MODE) & MAC_MODE_INT_LOOPBACK, 0);
    }

    #[test]
    fn mac_loopback_works_and_a_broken_path_is_reported_not_hidden() {
        let (mut s, mut n) = up();
        let r = n.loopback_test(&mut s, Loopback::Mac, 4).unwrap();
        assert_eq!(r, LoopResult { sent: 4, received: 4, matched: 4 });
        s.corrupt_loopback = true;
        let r = n.loopback_test(&mut s, Loopback::Mac, 4).unwrap();
        assert_eq!((r.sent, r.received, r.matched), (4, 4, 0));
        s.corrupt_loopback = false;
        s.drop_loopback = true;
        let r = n.loopback_test(&mut s, Loopback::Phy, 3).unwrap();
        assert_eq!((r.sent, r.received), (3, 0));
    }

    #[test]
    fn link_follows_the_phy() {
        let (mut s, mut n) = up();
        assert_eq!(n.link(&mut s).unwrap(), Link { up: false, mbps: 0, full_duplex: false });
        assert_eq!(n.phy_id(&mut s).unwrap(), 0x0362_5f60);
        s.link_up_1000fd();
        assert_eq!(n.link(&mut s).unwrap(), Link { up: true, mbps: 1000, full_duplex: true });
        assert_eq!(s.reg(MAC_MODE) & MAC_MODE_PORT_MASK, MAC_MODE_PORT_GMII);
        assert_eq!(s.locks_held(), 0, "PHY lock released after every access");
    }

    #[test]
    fn interrupts_are_acknowledged_through_the_mailbox() {
        let (mut s, mut n) = up();
        s.inject_rx(&frame(60, 1));
        assert!(s.status_word() & STATUS_UPDATED != 0);
        assert!(s.reg(MISC_HOST_CTRL) & MHC_TAGGED_STATUS != 0, "tagged status mode");
        let mut out = [0u8; BUF_SIZE];
        n.receive(&mut s, &mut out).unwrap();
        n.ack_irq(&mut s);
        assert_eq!(s.status_word() & STATUS_UPDATED, 0, "status marked read");
        assert_eq!(s.reg(MBOX_IRQ0), s.tag << 24, "the tag of the block we read is written back");
        let first = s.tag;
        s.inject_rx(&frame(60, 2));
        n.ack_irq(&mut s);
        assert_ne!(s.tag, first);
        assert_eq!(s.reg(MBOX_IRQ0), s.tag << 24, "each acknowledgement carries the newest tag");
        assert_eq!(n.stats.irqs, 2);
    }

    #[test]
    fn a_new_instance_after_a_crash_starts_from_a_reset_chip() {
        let (mut s, mut n) = up();
        n.transmit(&mut s, &frame(60, 9)).unwrap();
        s.inject_rx(&frame(60, 8));
        let l = n.layout();
        let mut n2 = Bcm5720::init(&mut s, l).unwrap();
        assert_eq!(s.resets, 2);
        assert!(s.quiesced_before_reset, "engines stopped before the reset");
        let mut out = [0u8; BUF_SIZE];
        assert_eq!(n2.receive(&mut s, &mut out), None, "the old frame went with the reset");
        n2.transmit(&mut s, &frame(61, 1)).unwrap();
        assert_eq!(s.wire.last().unwrap(), &frame(61, 1));
    }

    #[test]
    fn raw_descriptors_reach_the_bus_address_they_name() {
        let (mut s, mut n) = up();
        n.post_rx_escape(&mut s, 0x1000);
        let mut out = [0u8; BUF_SIZE];
        for k in 0..NRX as u8 {
            assert!(s.inject_rx(&frame(60, k)));
            assert!(n.receive(&mut s, &mut out).is_some());
        }
        assert!(s.foreign_writes.is_empty(), "the posted buffers come first");
        s.inject_rx(&frame(60, 99));
        assert_eq!(s.foreign_writes, vec![0x1000]);
        assert_eq!(n.receive(&mut s, &mut out), None, "the escape slot's entry is not a frame of ours");
        assert_eq!(n.stats.rx_dropped, 1);
        let _ = n.transmit_raw(&mut s, 0x2000, 64);
        assert_eq!(s.foreign_reads, vec![0x2000]);
    }

    #[test]
    fn stop_tells_the_firmware_and_the_ape() {
        let (mut s, mut n) = up();
        n.stop(&mut s);
        assert_eq!(s.sram(SRAM_DRV_STATE), DRV_STATE_UNLOAD_DONE);
        assert_eq!(s.ape(APE_HOST_DRVR_STATE), APE_HOST_STATE_UNLOAD);
        assert_eq!(s.reg(TX_MODE) & TX_MODE_ENABLE, 0);
        assert_eq!(s.reg(RX_MODE) & RX_MODE_ENABLE, 0);
        assert_eq!(s.reg(MBOX_IRQ0), 1);
    }

    #[test]
    fn layout_needs_an_aligned_big_enough_window() {
        assert_eq!(Layout::in_window(BASE + 8, Layout::BYTES), Err(Error::BadLayout));
        assert_eq!(Layout::in_window(BASE, Layout::BYTES - 1), Err(Error::BadLayout));
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        assert_eq!(l.tx_buf(NTX - 1) + BUF_SIZE as u64, BASE + Layout::BYTES);
        assert!(l.ret_ring + (RET_RING as u64) * 32 <= l.send_ring);
        assert!(l.send_ring + (SEND_RING as u64) * 16 <= l.rx_bufs);
        assert!(l.std_ring + (STD_RING as u64) * 32 <= l.ret_ring);
    }
}
