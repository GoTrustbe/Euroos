//! EuroE1000: the Intel e1000/e1000e driver logic, free of hardware access.
//!
//! One driver for the 82540EM ("e1000") and the 82574L ("e1000e", QEMU's q35
//! default NIC) through the **legacy descriptor** format, which both implement
//! with the same queue-0 register block. Register offsets and bit positions
//! are hardware facts from the Intel 8254x and 82574 software developer's
//! manuals (the same values `kernel/src/e1000.rs` uses).
//!
//! The logic talks to the device only through [`Hal`]: register reads and
//! writes, and reads and writes of the driver's own DMA window. The userspace
//! driver (`userland/e1000d`) implements `Hal` with volatile accesses to its
//! granted MMIO and DMA windows; the host tests implement it with a simulated
//! device (`sim`). Nothing here is `unsafe`.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(test)]
mod sim;

/// Register offsets (82540EM / 82574L, legacy queue 0).
pub mod reg {
    pub const CTRL: u32 = 0x0000;
    pub const STATUS: u32 = 0x0008;
    pub const ICR: u32 = 0x00C0;
    pub const IMS: u32 = 0x00D0;
    pub const IMC: u32 = 0x00D8;
    pub const IVAR: u32 = 0x00E4; // 82574L only: MSI-X cause -> vector
    pub const RCTL: u32 = 0x0100;
    pub const TCTL: u32 = 0x0400;
    pub const RDBAL: u32 = 0x2800;
    pub const RDBAH: u32 = 0x2804;
    pub const RDLEN: u32 = 0x2808;
    pub const RDH: u32 = 0x2810;
    pub const RDT: u32 = 0x2818;
    pub const TDBAL: u32 = 0x3800;
    pub const TDBAH: u32 = 0x3804;
    pub const TDLEN: u32 = 0x3808;
    pub const TDH: u32 = 0x3810;
    pub const TDT: u32 = 0x3818;
    pub const MTA: u32 = 0x5200; // 128 dwords
    pub const RAL0: u32 = 0x5400;
    pub const RAH0: u32 = 0x5404;
}

pub const CTRL_SLU: u32 = 1 << 6;
pub const CTRL_RST: u32 = 1 << 26;
pub const RCTL_EN: u32 = 1 << 1;
pub const RCTL_BAM: u32 = 1 << 15;
pub const RCTL_SECRC: u32 = 1 << 26;
pub const TCTL_EN: u32 = 1 << 1;
pub const TCTL_PSP: u32 = 1 << 3;
pub const RAH_AV: u32 = 1 << 31;
/// Interrupt causes (ICR/IMS).
pub const ICR_TXDW: u32 = 1 << 0;
pub const ICR_LSC: u32 = 1 << 2;
pub const ICR_RXDMT0: u32 = 1 << 4;
pub const ICR_RXT0: u32 = 1 << 7;
pub const ICR_RXQ0: u32 = 1 << 20; // 82574L MSI-X: receive queue 0
/// IVAR (82574L): RxQ0, TxQ0 and "other" causes all to MSI-X vector 0, valid.
pub const IVAR_ALL_TO_VEC0: u32 = (1 << 3) | (1 << 11) | (1 << 19);

/// Descriptor command / status bits (legacy format).
pub const TXD_CMD_EOP: u8 = 1 << 0;
pub const TXD_CMD_IFCS: u8 = 1 << 1;
pub const TXD_CMD_RS: u8 = 1 << 3;
pub const DESC_STA_DD: u8 = 1 << 0;
pub const RXD_STA_EOP: u8 = 1 << 1;

pub const NDESC: usize = 16;
pub const DESC_SIZE: u64 = 16;
pub const BUF_SIZE: usize = 2048;
const PAGE: u64 = 4096;

/// Polls allowed for a reset to complete, and for a transmit to report done.
pub const RESET_POLLS: u32 = 100_000;
pub const TX_POLLS: u32 = 2_000_000;

/// The device's view of the world: registers and the driver's DMA window.
pub trait Hal {
    fn reg_read(&mut self, off: u32) -> u32;
    fn reg_write(&mut self, off: u32, val: u32);
    /// Write `data` at bus address `iova` (must lie in the driver's DMA window).
    fn dma_write(&mut self, iova: u64, data: &[u8]);
    /// Read `out.len()` bytes at bus address `iova` (in the DMA window).
    fn dma_read(&mut self, iova: u64, out: &mut [u8]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The DMA window is too small or not page-aligned.
    BadLayout,
    /// CTRL.RST never cleared.
    ResetTimeout,
    /// RAL0/RAH0 hold no address (no EEPROM, or a dead device).
    NoMac,
    /// A frame of zero or more than BUF_SIZE bytes.
    FrameSize,
    /// The device never reported a transmit descriptor done.
    TxTimeout,
}

/// Where the rings and buffers sit in the DMA window (bus addresses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub rx_ring: u64,
    pub tx_ring: u64,
    pub rx_bufs: u64,
    pub tx_bufs: u64,
}

impl Layout {
    /// Bytes the layout needs: a page per ring, NDESC buffers per direction.
    pub const BYTES: u64 = 2 * PAGE + 2 * (NDESC * BUF_SIZE) as u64;

    /// Carve the layout out of a DMA window `[base, base + len)`.
    pub fn in_window(base: u64, len: u64) -> Result<Layout, Error> {
        if base % PAGE != 0 || len < Self::BYTES || base.checked_add(len).is_none() {
            return Err(Error::BadLayout);
        }
        let rx_bufs = base + 2 * PAGE;
        Ok(Layout { rx_ring: base, tx_ring: base + PAGE, rx_bufs, tx_bufs: rx_bufs + (NDESC * BUF_SIZE) as u64 })
    }

    fn rx_desc(&self, i: usize) -> u64 {
        self.rx_ring + i as u64 * DESC_SIZE
    }
    fn tx_desc(&self, i: usize) -> u64 {
        self.tx_ring + i as u64 * DESC_SIZE
    }
    fn rx_buf(&self, i: usize) -> u64 {
        self.rx_bufs + (i * BUF_SIZE) as u64
    }
    fn tx_buf(&self, i: usize) -> u64 {
        self.tx_bufs + (i * BUF_SIZE) as u64
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tx: u64,
    pub rx: u64,
    /// Frames dropped on receive (too long for the caller, or a length the
    /// device should never report).
    pub rx_dropped: u64,
    pub irqs: u64,
}

/// One initialised e1000 queue pair.
#[derive(Debug)]
pub struct E1000 {
    layout: Layout,
    rx_head: usize,
    tx_tail: usize,
    mac: [u8; 6],
    pub stats: Stats,
}

fn wr_u64(hal: &mut impl Hal, at: u64, v: u64) {
    hal.dma_write(at, &v.to_le_bytes());
}

impl E1000 {
    /// Reset the device and bring up one RX and one TX ring in `layout`.
    /// `msix`: route the queue-0 causes to MSI-X vector 0 (82574L) and unmask
    /// the receive interrupt; otherwise every interrupt stays masked (polled).
    pub fn init(hal: &mut impl Hal, layout: Layout, msix: bool) -> Result<E1000, Error> {
        // Full device reset: whatever a previous driver instance left in the
        // ring registers (a crashed one, for instance) is gone after this.
        hal.reg_write(reg::IMC, 0xFFFF_FFFF);
        let ctrl = hal.reg_read(reg::CTRL);
        hal.reg_write(reg::CTRL, ctrl | CTRL_RST);
        let mut polls = 0;
        while hal.reg_read(reg::CTRL) & CTRL_RST != 0 {
            polls += 1;
            if polls >= RESET_POLLS {
                return Err(Error::ResetTimeout);
            }
        }
        hal.reg_write(reg::IMC, 0xFFFF_FFFF); // reset re-arms nothing, but be explicit
        let _ = hal.reg_read(reg::ICR); // clear pending causes

        // MAC: loaded from the EEPROM into RAL0/RAH0 by the reset.
        let ral = hal.reg_read(reg::RAL0);
        let rah = hal.reg_read(reg::RAH0);
        let mac = [ral as u8, (ral >> 8) as u8, (ral >> 16) as u8, (ral >> 24) as u8, rah as u8, (rah >> 8) as u8];
        if mac == [0; 6] || mac == [0xff; 6] {
            return Err(Error::NoMac);
        }
        hal.reg_write(reg::RAH0, rah | RAH_AV);
        for i in 0..128 {
            hal.reg_write(reg::MTA + i * 4, 0);
        }

        // Rings: zero both, point every RX descriptor at its buffer.
        let zero = [0u8; PAGE as usize];
        hal.dma_write(layout.rx_ring, &zero);
        hal.dma_write(layout.tx_ring, &zero);
        for i in 0..NDESC {
            wr_u64(hal, layout.rx_desc(i), layout.rx_buf(i));
        }
        hal.reg_write(reg::RDBAL, layout.rx_ring as u32);
        hal.reg_write(reg::RDBAH, (layout.rx_ring >> 32) as u32);
        hal.reg_write(reg::RDLEN, (NDESC as u64 * DESC_SIZE) as u32);
        hal.reg_write(reg::RDH, 0);
        hal.reg_write(reg::RDT, (NDESC - 1) as u32); // all but one slot to the device
        hal.reg_write(reg::TDBAL, layout.tx_ring as u32);
        hal.reg_write(reg::TDBAH, (layout.tx_ring >> 32) as u32);
        hal.reg_write(reg::TDLEN, (NDESC as u64 * DESC_SIZE) as u32);
        hal.reg_write(reg::TDH, 0);
        hal.reg_write(reg::TDT, 0);

        // Receive: enable, broadcast, 2 KiB buffers (BSIZE 00), strip CRC.
        hal.reg_write(reg::RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);
        // Transmit: enable, pad short frames, collision threshold/distance.
        hal.reg_write(reg::TCTL, TCTL_EN | TCTL_PSP | (0x10 << 4) | (0x40 << 12));
        let ctrl = hal.reg_read(reg::CTRL);
        hal.reg_write(reg::CTRL, ctrl | CTRL_SLU);

        if msix {
            hal.reg_write(reg::IVAR, IVAR_ALL_TO_VEC0);
            hal.reg_write(reg::IMS, ICR_RXQ0 | ICR_RXT0 | ICR_RXDMT0 | ICR_LSC);
        }
        Ok(E1000 { layout, rx_head: 0, tx_tail: 0, mac, stats: Stats::default() })
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Transmit one frame and wait (bounded) until the device reports it done,
    /// so the slot can be reused at once.
    pub fn transmit(&mut self, hal: &mut impl Hal, frame: &[u8]) -> Result<(), Error> {
        if frame.is_empty() || frame.len() > BUF_SIZE {
            return Err(Error::FrameSize);
        }
        let i = self.tx_tail;
        let buf = self.layout.tx_buf(i);
        hal.dma_write(buf, frame);
        self.post_tx(hal, i, buf, frame.len() as u16)?;
        self.stats.tx += 1;
        Ok(())
    }

    /// Transmit from an arbitrary bus address. A driver can always put any
    /// address in a descriptor; this is how the confinement self-test plays a
    /// malicious driver (the IOMMU must refuse the read).
    pub fn transmit_raw(&mut self, hal: &mut impl Hal, addr: u64, len: u16) -> Result<(), Error> {
        let i = self.tx_tail;
        self.post_tx(hal, i, addr, len)
    }

    fn post_tx(&mut self, hal: &mut impl Hal, i: usize, addr: u64, len: u16) -> Result<(), Error> {
        let d = self.layout.tx_desc(i);
        let mut desc = [0u8; 16];
        desc[0..8].copy_from_slice(&addr.to_le_bytes());
        desc[8..10].copy_from_slice(&len.to_le_bytes());
        desc[11] = TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS;
        hal.dma_write(d, &desc);
        self.tx_tail = (i + 1) % NDESC;
        hal.reg_write(reg::TDT, self.tx_tail as u32);
        let mut sta = [0u8; 1];
        for _ in 0..TX_POLLS {
            hal.dma_read(d + 12, &mut sta);
            if sta[0] & DESC_STA_DD != 0 {
                return Ok(());
            }
        }
        Err(Error::TxTimeout)
    }

    /// Point RX descriptor `slot` at an arbitrary bus address (the confinement
    /// self-test: the device's write there must be refused by the IOMMU).
    /// `restore_rx_buffer` puts the driver's own buffer back.
    pub fn aim_rx_slot(&mut self, hal: &mut impl Hal, slot: usize, addr: u64) {
        let d = self.layout.rx_desc(slot % NDESC);
        wr_u64(hal, d, addr);
    }

    pub fn restore_rx_buffer(&mut self, hal: &mut impl Hal, slot: usize) {
        let s = slot % NDESC;
        let d = self.layout.rx_desc(s);
        wr_u64(hal, d, self.layout.rx_buf(s));
    }

    /// The RX slot the device fills next.
    pub fn rx_next_slot(&self) -> usize {
        self.rx_head
    }

    /// Take the next received frame into `out`. Returns its length, or None
    /// when nothing new arrived. A frame longer than `out`, or a length the
    /// device should never report (> BUF_SIZE), is dropped and counted; the
    /// descriptor is recycled either way.
    pub fn receive(&mut self, hal: &mut impl Hal, out: &mut [u8]) -> Option<usize> {
        loop {
            let i = self.rx_head;
            let d = self.layout.rx_desc(i);
            // The status byte (DD) first, the rest of the descriptor only after
            // it showed DD: the device writes the descriptor back while we may
            // be reading it, and a HAL that reads bytes in ascending order
            // (e1000d does) could otherwise see DD with the old, zeroed length
            // and drop a good frame. Same class as euronvme's phase/id read.
            let mut sta = [0u8; 1];
            hal.dma_read(d + 12, &mut sta);
            if sta[0] & DESC_STA_DD == 0 {
                return None;
            }
            let mut desc = [0u8; 16];
            hal.dma_read(d, &mut desc);
            let len = u16::from_le_bytes([desc[8], desc[9]]) as usize;
            let ok = len > 0 && len <= BUF_SIZE && len <= out.len() && desc[12] & RXD_STA_EOP != 0;
            if ok {
                hal.dma_read(self.layout.rx_buf(i), &mut out[..len]);
            }
            // Recycle: our own buffer back in, status cleared, slot to the device.
            let mut fresh = [0u8; 16];
            fresh[0..8].copy_from_slice(&self.layout.rx_buf(i).to_le_bytes());
            hal.dma_write(d, &fresh);
            hal.reg_write(reg::RDT, i as u32);
            self.rx_head = (i + 1) % NDESC;
            if ok {
                self.stats.rx += 1;
                return Some(len);
            }
            self.stats.rx_dropped += 1;
        }
    }

    /// Read (and so acknowledge) the interrupt causes.
    pub fn ack_irq(&mut self, hal: &mut impl Hal) -> u32 {
        self.stats.irqs += 1;
        hal.reg_read(reg::ICR)
    }

    /// Mask every interrupt and stop both queues (a clean shutdown).
    pub fn stop(&mut self, hal: &mut impl Hal) {
        hal.reg_write(reg::IMC, 0xFFFF_FFFF);
        hal.reg_write(reg::RCTL, 0);
        hal.reg_write(reg::TCTL, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::sim::Sim;
    use super::*;

    const BASE: u64 = 0x0040_0000;
    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

    fn up(msix: bool) -> (Sim, E1000) {
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        let e = E1000::init(&mut s, l, msix).unwrap();
        (s, e)
    }

    fn frame(n: usize, tag: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8) ^ tag).collect()
    }

    #[test]
    fn init_resets_reads_the_mac_and_programs_the_rings() {
        let (s, e) = up(false);
        assert_eq!(e.mac(), MAC);
        assert_eq!(s.resets, 1);
        assert_eq!(s.reg(reg::RDBAL) as u64, BASE);
        assert_eq!(s.reg(reg::TDBAL) as u64, BASE + 4096);
        assert_eq!(s.reg(reg::RDLEN), 256);
        assert_eq!(s.reg(reg::RDT), 15);
        assert_eq!(s.reg(reg::TDT), 0);
        assert!(s.reg(reg::RCTL) & RCTL_EN != 0 && s.reg(reg::TCTL) & TCTL_EN != 0);
        assert!(s.reg(reg::RAH0) & RAH_AV != 0);
        assert_eq!(s.reg(reg::IMS), 0, "polled: no interrupt unmasked");
        // Every RX descriptor points at its own buffer.
        let l = e.layout();
        for i in 0..NDESC {
            assert_eq!(s.mem_u64(l.rx_ring + i as u64 * 16), l.rx_bufs + (i * BUF_SIZE) as u64);
        }
    }

    #[test]
    fn msix_init_routes_causes_and_unmasks_receive() {
        let (s, _e) = up(true);
        assert_eq!(s.reg(reg::IVAR), IVAR_ALL_TO_VEC0);
        assert!(s.reg(reg::IMS) & ICR_RXQ0 != 0 && s.reg(reg::IMS) & ICR_RXT0 != 0);
    }

    #[test]
    fn transmit_puts_exact_bytes_on_the_wire_and_wraps() {
        let (mut s, mut e) = up(false);
        for k in 0..40u8 {
            let f = frame(60 + k as usize, k);
            e.transmit(&mut s, &f).unwrap();
            assert_eq!(s.wire.last().unwrap(), &f);
        }
        assert_eq!(s.wire.len(), 40);
        assert_eq!(e.stats.tx, 40);
        assert_eq!(s.reg(reg::TDT), (40 % NDESC) as u32);
    }

    #[test]
    fn transmit_refuses_bad_sizes_and_times_out_on_a_dead_device() {
        let (mut s, mut e) = up(false);
        assert_eq!(e.transmit(&mut s, &[]), Err(Error::FrameSize));
        assert_eq!(e.transmit(&mut s, &[0u8; BUF_SIZE + 1]), Err(Error::FrameSize));
        s.dead_tx = true;
        assert_eq!(e.transmit(&mut s, &[1u8; 60]), Err(Error::TxTimeout));
        assert!(s.wire.is_empty());
    }

    #[test]
    fn receive_returns_frames_in_order_and_recycles_across_wraps() {
        let (mut s, mut e) = up(false);
        let mut out = [0u8; BUF_SIZE];
        assert_eq!(e.receive(&mut s, &mut out), None);
        for round in 0..5u8 {
            // Fill 10 at a time (fewer than the 15 slots the device owns).
            let sent: Vec<Vec<u8>> = (0..10).map(|k| frame(64 + k, round.wrapping_mul(31) ^ k as u8)).collect();
            for f in &sent {
                assert!(s.inject_rx(f), "device had a free slot");
            }
            for f in &sent {
                let n = e.receive(&mut s, &mut out).unwrap();
                assert_eq!(&out[..n], &f[..]);
            }
            assert_eq!(e.receive(&mut s, &mut out), None);
        }
        assert_eq!(e.stats.rx, 50);
    }

    #[test]
    fn a_full_ring_makes_the_device_drop_not_overwrite() {
        let (mut s, mut e) = up(false);
        let mut accepted = 0;
        for k in 0..20u8 {
            if s.inject_rx(&frame(60, k)) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 15, "one slot always stays with the driver");
        let mut out = [0u8; BUF_SIZE];
        for k in 0..15u8 {
            let n = e.receive(&mut s, &mut out).unwrap();
            assert_eq!(&out[..n], &frame(60, k)[..]);
        }
    }

    /// A HAL that reads DMA memory one byte at a time in ascending order, like
    /// e1000d, and lets the device post a frame after the `at`-th byte read.
    struct ByteWise<'a> {
        sim: &'a mut Sim,
        read: usize,
        at: usize,
        frame: Option<Vec<u8>>,
    }

    impl Hal for ByteWise<'_> {
        fn reg_read(&mut self, off: u32) -> u32 {
            self.sim.reg_read(off)
        }
        fn reg_write(&mut self, off: u32, val: u32) {
            self.sim.reg_write(off, val)
        }
        fn dma_write(&mut self, iova: u64, data: &[u8]) {
            self.sim.dma_write(iova, data)
        }
        fn dma_read(&mut self, iova: u64, out: &mut [u8]) {
            for (k, b) in out.iter_mut().enumerate() {
                let mut one = [0u8; 1];
                self.sim.dma_read(iova + k as u64, &mut one);
                *b = one[0];
                self.read += 1;
                if self.read == self.at {
                    if let Some(f) = self.frame.take() {
                        assert!(self.sim.inject_rx(&f));
                    }
                }
            }
        }
    }

    #[test]
    fn a_descriptor_written_back_during_the_read_is_not_torn() {
        // The device completes the descriptor after every possible number of
        // bytes the driver has read so far: the driver must never drop it.
        for at in 1..=40 {
            let (mut s, mut e) = up(false);
            let f = frame(98, at as u8);
            let mut out = [0u8; BUF_SIZE];
            let mut got = None;
            {
                let mut h = ByteWise { sim: &mut s, read: 0, at, frame: Some(f.clone()) };
                for _ in 0..64 {
                    if let Some(n) = e.receive(&mut h, &mut out) {
                        got = Some(n);
                        break;
                    }
                }
            }
            assert_eq!(e.stats.rx_dropped, 0, "frame posted after byte {at} was dropped (torn descriptor)");
            assert_eq!(got, Some(f.len()), "frame posted after byte {at}");
            assert_eq!(&out[..f.len()], &f[..]);
        }
    }

    #[test]
    fn oversize_or_lying_lengths_are_dropped_and_counted() {
        let (mut s, mut e) = up(false);
        // A frame longer than the caller's buffer.
        s.inject_rx(&frame(1500, 1));
        let mut small = [0u8; 100];
        assert_eq!(e.receive(&mut s, &mut small), None);
        assert_eq!(e.stats.rx_dropped, 1);
        // A device that reports a length past its buffer (a malicious or broken
        // device must not make us read past our buffer).
        s.inject_rx_with_len(&frame(60, 2), (BUF_SIZE + 512) as u16);
        s.inject_rx(&frame(70, 3));
        let mut out = [0u8; BUF_SIZE];
        let n = e.receive(&mut s, &mut out).unwrap();
        assert_eq!(n, 70, "the bad descriptor is skipped, the next frame arrives");
        assert_eq!(e.stats.rx_dropped, 2);
    }

    #[test]
    fn init_fails_cleanly_without_mac_or_reset() {
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        let mut s = Sim::new([0; 6], BASE, Layout::BYTES as usize);
        assert_eq!(E1000::init(&mut s, l, false).unwrap_err(), Error::NoMac);
        let mut s = Sim::new(MAC, BASE, Layout::BYTES as usize);
        s.stuck_reset = true;
        assert_eq!(E1000::init(&mut s, l, false).unwrap_err(), Error::ResetTimeout);
    }

    #[test]
    fn layout_needs_an_aligned_big_enough_window() {
        assert_eq!(Layout::BYTES, 2 * 4096 + 2 * 16 * 2048);
        assert_eq!(Layout::in_window(BASE + 8, Layout::BYTES), Err(Error::BadLayout));
        assert_eq!(Layout::in_window(BASE, Layout::BYTES - 1), Err(Error::BadLayout));
        assert_eq!(Layout::in_window(u64::MAX & !0xfff, Layout::BYTES), Err(Error::BadLayout));
        let l = Layout::in_window(BASE, 32 * 4096).unwrap();
        assert_eq!(l.tx_bufs + (NDESC * BUF_SIZE) as u64, BASE + Layout::BYTES);
    }

    #[test]
    fn a_reinit_after_a_crash_starts_from_a_clean_device() {
        // A previous instance left the rings mid-flight; a new init must reset.
        let (mut s, mut e) = up(false);
        e.transmit(&mut s, &frame(60, 9)).unwrap();
        s.inject_rx(&frame(60, 8));
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        let mut e2 = E1000::init(&mut s, l, false).unwrap();
        assert_eq!(s.resets, 2);
        assert_eq!(s.reg(reg::TDT), 0);
        let mut out = [0u8; BUF_SIZE];
        assert_eq!(e2.receive(&mut s, &mut out), None, "the old frame went with the reset");
        e2.transmit(&mut s, &frame(61, 1)).unwrap();
        assert_eq!(s.wire.last().unwrap(), &frame(61, 1));
    }

    #[test]
    fn raw_descriptors_reach_the_bus_address_they_name() {
        // What the device is asked to do with a foreign address; in the kernel
        // the IOMMU stands between this request and memory.
        let (mut s, mut e) = up(false);
        e.aim_rx_slot(&mut s, e.rx_next_slot(), 0x1000);
        s.inject_rx(&frame(60, 5));
        assert_eq!(s.foreign_writes, vec![0x1000]);
        e.restore_rx_buffer(&mut s, 0);
        let _ = e.transmit_raw(&mut s, 0x2000, 64);
        assert_eq!(s.foreign_reads, vec![0x2000]);
    }
}
