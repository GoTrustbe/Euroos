//! The MFI controller logic: bring-up (handshake + init frame), command
//! submission through the inbound queue port, and the reply queue.
//!
//! The reply queue is a ring of 32-bit contexts in host memory plus two index
//! words: the firmware writes a completed frame's context at the producer
//! index and advances it (modulo the ring length = the firmware's command
//! count), the host consumes up to the producer and writes its consumer index
//! back. Contexts come from the device, so each one is checked: it must carry
//! this driver instance's generation, name a frame that is in flight, and that
//! frame's status byte must no longer read "pending". Anything else is counted
//! as stray and ignored (a late completion from before a controller reset, a
//! device that writes garbage).

use crate::frame::{self, Dir, Sg, FRAME_SIZE, STATUS_PENDING};
use crate::fw::{FwState, FwStatus, Handshake, Step};
use crate::info::{self, CtrlInfo, LdList};
use crate::layout::{Layout, DCMD_BYTES, MAX_FRAMES, RING_MAX};
use crate::regs;

/// A ring index the firmware updates in host memory while we may be reading
/// it. megaraidd reads DMA memory a byte at a time in ascending order, so an
/// update from 0x00FF to 0x0100 between two bytes reads as 0x01FF (inside a
/// 1008-entry ring): the driver would consume 256 reply slots the firmware
/// never wrote, count them stray, and then skip the real completions that land
/// there (their commands time out). Read until two reads in a row agree (an
/// update changes the value, so a torn read cannot repeat itself); bounded to
/// 16 retries, after which the last read is used.
fn read_index<H: Hal>(h: &mut H, at: u64) -> u32 {
    let read = |h: &mut H| {
        let mut b = [0u8; 4];
        h.dma_read(at, &mut b);
        u32::from_le_bytes(b)
    };
    let mut last = read(h);
    for _ in 0..16 {
        let now = read(h);
        if now == last {
            return now;
        }
        last = now;
    }
    last
}

/// What the logic needs from the outside world.
pub trait Hal {
    fn read32(&mut self, off: u32) -> u32;
    fn write32(&mut self, off: u32, val: u32);
    /// Read device-visible memory inside the driver's DMA window.
    fn dma_read(&mut self, addr: u64, out: &mut [u8]);
    /// Write device-visible memory inside the driver's DMA window.
    fn dma_write(&mut self, addr: u64, data: &[u8]);
    fn now_ms(&mut self) -> u64;
    /// A short pause while waiting on the device.
    fn relax(&mut self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The firmware is in FAULT (raw status word).
    Fault(u32),
    HandshakeTimeout(FwState),
    Flapping,
    /// The firmware asks for a reply ring larger than the window holds, or 0.
    RingSize(u32),
    /// The init frame failed with this status.
    Init(u8),
    /// A synchronous command timed out.
    Timeout,
    /// No free frame.
    Busy,
    /// A frame could not be built (bad SGL/CDB).
    Frame,
    /// A frame address the queue port cannot express.
    Post,
    /// A synchronous command completed with (cmd_status, scsi_status).
    Status(u8, u8),
    /// A synchronous command was attempted while others are in flight.
    NotIdle,
    /// The device returned a block that does not parse.
    BadData,
}

/// A completed command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    pub frame: usize,
    /// The caller's tag given at submission.
    pub user: u32,
    pub status: u8,
    pub scsi: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub posted: u64,
    pub completed: u64,
    /// Contexts that were not ours (wrong generation, unknown frame, frame idle, still pending).
    pub stray: u64,
    /// Producer indices outside the ring.
    pub bad_producer: u64,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    busy: bool,
    user: u32,
}

pub struct Mfi {
    lay: Layout,
    gen: u16,
    ring_len: u32,
    consumer: u32,
    slots: [Slot; MAX_FRAMES],
    pub fw: FwStatus,
    pub stats: Stats,
}

/// How long the init frame may take.
pub const INIT_TIMEOUT_MS: u64 = 5_000;
/// How long a synchronous DCMD/pass-through may take.
pub const SYNC_TIMEOUT_MS: u64 = 30_000;

impl Mfi {
    /// The context of frame `i` for generation `gen`: never 0, never all ones.
    pub fn context(gen: u16, i: usize) -> u32 {
        ((gen as u32) << 16) | (i as u32 + 1)
    }

    fn frame_of(&self, ctx: u32) -> Option<usize> {
        if (ctx >> 16) as u16 != self.gen {
            return None;
        }
        let i = (ctx & 0xFFFF) as usize;
        (1..=MAX_FRAMES).contains(&i).then(|| i - 1)
    }

    /// Drive the firmware to READY, hand it the reply queue, unmask its
    /// interrupt. `gen` must differ between consecutive driver instances
    /// (late completions of a previous instance are then recognised).
    pub fn bring_up<H: Hal>(h: &mut H, lay: Layout, gen: u16, handshake_timeout_ms: u64) -> Result<Mfi, Error> {
        let gen = if gen == 0 || gen == 0xFFFF { 1 } else { gen };
        let mut hs = Handshake::new(handshake_timeout_ms);
        loop {
            let raw = h.read32(regs::OSP0);
            let now = h.now_ms();
            match hs.step(raw, now) {
                Step::Done => break,
                Step::Doorbell(v) => h.write32(regs::IDB, v),
                Step::MaskThenDoorbell(v) => {
                    h.write32(regs::OMSK, regs::MASK_ALL);
                    h.write32(regs::IDB, v);
                }
                Step::Wait => h.relax(),
                Step::Fault(r) => return Err(Error::Fault(r)),
                Step::Timeout(s) => return Err(Error::HandshakeTimeout(s)),
                Step::Flapping => return Err(Error::Flapping),
            }
        }
        let fw = FwStatus(h.read32(regs::OSP0));
        let ring_len = fw.max_cmds();
        if ring_len == 0 || ring_len > RING_MAX {
            return Err(Error::RingSize(ring_len));
        }
        // Interrupts off while the queue is (re)built; a clean slate in memory:
        // anything a previous instance or the reset left in the ring goes.
        h.write32(regs::OMSK, regs::MASK_ALL);
        let zero = [0u8; 256];
        let mut a = lay.frame(0);
        while a < lay.ring() + RING_MAX as u64 * 4 {
            h.dma_write(a, &zero);
            a += zero.len() as u64;
        }
        h.dma_write(lay.qinfo(), &frame::qinfo(ring_len, lay.ring(), lay.producer(), lay.consumer()));
        let ctx = Mfi::context(gen, 0);
        let f = frame::init(ctx, lay.qinfo());
        h.dma_write(lay.frame(0), &f);
        let post = regs::post_value(lay.frame(0), 0).ok_or(Error::Post)?;
        h.write32(regs::IQP, post);
        let start = h.now_ms();
        let status = loop {
            let mut hdr = [0u8; 4];
            h.dma_read(lay.frame(0), &mut hdr);
            if hdr[2] != STATUS_PENDING {
                break hdr[2];
            }
            if h.now_ms().saturating_sub(start) > INIT_TIMEOUT_MS {
                return Err(Error::Timeout);
            }
            h.relax();
        };
        if status != frame::STAT_OK {
            return Err(Error::Init(status));
        }
        // The init completion may itself have been posted to the ring: start
        // consuming from wherever the producer stands now.
        let consumer = read_index(h, lay.producer()) % ring_len;
        h.dma_write(lay.consumer(), &consumer.to_le_bytes());
        h.write32(regs::ODCR0, regs::MASK_ALL);
        h.write32(regs::OMSK, regs::GEN2_UNMASK);
        Ok(Mfi {
            lay,
            gen,
            ring_len,
            consumer,
            slots: [Slot { busy: false, user: 0 }; MAX_FRAMES],
            fw,
            stats: Stats::default(),
        })
    }

    pub fn layout(&self) -> &Layout {
        &self.lay
    }

    pub fn generation(&self) -> u16 {
        self.gen
    }

    pub fn in_flight(&self) -> usize {
        self.slots.iter().filter(|s| s.busy).count()
    }

    fn free_frame(&self) -> Option<usize> {
        self.slots.iter().position(|s| !s.busy)
    }

    /// Post a frame built by `build(context, frame_index)`. Returns the frame index.
    pub fn post<H: Hal>(
        &mut self,
        h: &mut H,
        user: u32,
        build: impl FnOnce(u32, usize) -> Result<[u8; FRAME_SIZE], frame::FrameError>,
    ) -> Result<usize, Error> {
        let i = self.free_frame().ok_or(Error::Busy)?;
        let f = build(Mfi::context(self.gen, i), i).map_err(|_| Error::Frame)?;
        let addr = self.lay.frame(i);
        let post = regs::post_value(addr, 0).ok_or(Error::Post)?;
        h.dma_write(addr, &f);
        self.slots[i] = Slot { busy: true, user };
        self.stats.posted += 1;
        h.write32(regs::IQP, post);
        Ok(i)
    }

    /// Logical-drive read or write into/out of `buf` (inside the window).
    pub fn post_ld_io<H: Hal>(&mut self, h: &mut H, user: u32, write: bool, target: u8, lba: u64, sectors: u32, buf: u64) -> Result<usize, Error> {
        if !self.lay.contains(buf, sectors as u64 * 512) {
            return Err(Error::Frame);
        }
        let lay = self.lay;
        self.post(h, user, |ctx, i| frame::ld_io(ctx, write, target, lba, sectors, lay.sense(i), buf))
    }

    /// Cache flush (controller + disks). Also drains: the firmware completes it
    /// after the writes before it.
    pub fn post_flush<H: Hal>(&mut self, h: &mut H, user: u32) -> Result<usize, Error> {
        let mut mbox = [0u8; 12];
        mbox[0] = frame::FLUSH_CTRL_AND_DISK;
        self.post(h, user, |ctx, _| frame::dcmd(ctx, frame::DCMD_CTRL_CACHE_FLUSH, mbox, None))
    }

    /// A DCMD whose data buffer is `target` (self-test only: the kernel's
    /// IOMMU must refuse the device's write there).
    pub fn post_info_to<H: Hal>(&mut self, h: &mut H, user: u32, target: u64) -> Result<usize, Error> {
        self.post(h, user, |ctx, _| {
            frame::dcmd(ctx, frame::DCMD_CTRL_GET_INFO, [0; 12], Some((Sg { addr: target, len: info::CTRL_INFO_LEN as u32 }, Dir::FromDevice)))
        })
    }

    /// Post `addr` itself as a frame address (self-test only: the device must
    /// be refused when it reads a "frame" outside the window). Nothing is
    /// marked in flight: whatever completes for it is stray by construction.
    pub fn post_foreign_frame<H: Hal>(&mut self, h: &mut H, addr: u64) -> Result<(), Error> {
        let post = regs::post_value(addr & !0x1F, 0).ok_or(Error::Post)?;
        h.write32(regs::IQP, post);
        Ok(())
    }

    /// Acknowledge the gen2 reply interrupt (read status, write it back).
    pub fn ack_irq<H: Hal>(&mut self, h: &mut H) -> u32 {
        let s = h.read32(regs::OSTS);
        if s != 0 {
            h.write32(regs::ODCR0, s);
            let _ = h.read32(regs::OSTS);
        }
        s
    }

    /// Is something waiting in the reply ring?
    pub fn pending<H: Hal>(&self, h: &mut H) -> bool {
        read_index(h, self.lay.producer()) != self.consumer
    }

    /// Consume the reply ring up to the producer index. Calls `done` for every
    /// valid completion; returns how many.
    pub fn reap<H: Hal>(&mut self, h: &mut H, mut done: impl FnMut(Completion)) -> usize {
        // The producer index first (the firmware writes the frame status and
        // the reply context before it moves the index), read so that it cannot
        // be torn; then the entries up to it.
        let producer = read_index(h, self.lay.producer());
        if producer >= self.ring_len {
            self.stats.bad_producer += 1;
            return 0;
        }
        let mut n = 0;
        let mut guard = 0;
        while self.consumer != producer && guard < self.ring_len {
            guard += 1;
            let mut c = [0u8; 4];
            h.dma_read(self.lay.ring() + self.consumer as u64 * 4, &mut c);
            self.consumer = (self.consumer + 1) % self.ring_len;
            let ctx = u32::from_le_bytes(c);
            let Some(i) = self.frame_of(ctx).filter(|&i| self.slots[i].busy) else {
                self.stats.stray += 1;
                continue;
            };
            let mut hdr = [0u8; 4];
            h.dma_read(self.lay.frame(i), &mut hdr);
            let (status, scsi) = frame::status(&hdr);
            if status == STATUS_PENDING {
                self.stats.stray += 1; // a context for a frame the device has not finished
                continue;
            }
            let user = self.slots[i].user;
            self.slots[i].busy = false;
            self.stats.completed += 1;
            n += 1;
            done(Completion { frame: i, user, status, scsi });
        }
        h.dma_write(self.lay.consumer(), &self.consumer.to_le_bytes());
        n
    }

    /// Run one command to completion (bring-up only: nothing else in flight).
    fn exec_sync<H: Hal>(
        &mut self,
        h: &mut H,
        build: impl FnOnce(u32, usize) -> Result<[u8; FRAME_SIZE], frame::FrameError>,
    ) -> Result<(), Error> {
        if self.in_flight() != 0 {
            return Err(Error::NotIdle);
        }
        let i = self.post(h, u32::MAX, build)?;
        let start = h.now_ms();
        loop {
            let mut got = None;
            self.reap(h, |c| {
                if c.frame == i {
                    got = Some((c.status, c.scsi));
                }
            });
            if let Some((s, scsi)) = got {
                return if s == frame::STAT_OK { Ok(()) } else { Err(Error::Status(s, scsi)) };
            }
            if h.now_ms().saturating_sub(start) > SYNC_TIMEOUT_MS {
                return Err(Error::Timeout);
            }
            h.relax();
        }
    }

    fn read_dcmd_buf<H: Hal>(&self, h: &mut H, out: &mut [u8]) {
        h.dma_read(self.lay.dcmd_buf(), out);
    }

    fn clear_dcmd_buf<H: Hal>(&self, h: &mut H) {
        let zero = [0u8; 256];
        for k in 0..(DCMD_BYTES as u64 / 256) {
            h.dma_write(self.lay.dcmd_buf() + k * 256, &zero);
        }
    }

    /// DCMD CTRL_GET_INFO.
    pub fn ctrl_info<H: Hal>(&mut self, h: &mut H) -> Result<CtrlInfo, Error> {
        self.clear_dcmd_buf(h);
        let buf = self.lay.dcmd_buf();
        self.exec_sync(h, |ctx, _| {
            frame::dcmd(ctx, frame::DCMD_CTRL_GET_INFO, [0; 12], Some((Sg { addr: buf, len: info::CTRL_INFO_LEN as u32 }, Dir::FromDevice)))
        })?;
        let mut b = [0u8; info::CTRL_INFO_LEN];
        self.read_dcmd_buf(h, &mut b);
        CtrlInfo::parse(&b).ok_or(Error::BadData)
    }

    /// DCMD LD_GET_LIST.
    pub fn ld_list<H: Hal>(&mut self, h: &mut H) -> Result<LdList, Error> {
        self.clear_dcmd_buf(h);
        let buf = self.lay.dcmd_buf();
        self.exec_sync(h, |ctx, _| {
            frame::dcmd(ctx, frame::DCMD_LD_GET_LIST, [0; 12], Some((Sg { addr: buf, len: info::LD_LIST_LEN as u32 }, Dir::FromDevice)))
        })?;
        let mut b = [0u8; info::LD_LIST_LEN];
        self.read_dcmd_buf(h, &mut b);
        LdList::parse(&b).ok_or(Error::BadData)
    }

    /// SCSI READ CAPACITY(16) to a logical drive through the pass-through frame.
    pub fn read_capacity<H: Hal>(&mut self, h: &mut H, target: u8) -> Result<(u64, u32), Error> {
        self.clear_dcmd_buf(h);
        let buf = self.lay.dcmd_buf();
        let lay = self.lay;
        self.exec_sync(h, |ctx, i| {
            frame::pass_ld(ctx, target, &frame::cdb_read_capacity16(), lay.sense(i), Some((Sg { addr: buf, len: 32 }, Dir::FromDevice)))
        })?;
        let mut b = [0u8; 32];
        self.read_dcmd_buf(h, &mut b);
        info::read_capacity16(&b).ok_or(Error::BadData)
    }

    /// Synchronous cache flush (bring-up: drains whatever a previous instance left).
    pub fn flush_sync<H: Hal>(&mut self, h: &mut H) -> Result<(), Error> {
        let mut mbox = [0u8; 12];
        mbox[0] = frame::FLUSH_CTRL_AND_DISK;
        self.exec_sync(h, |ctx, _| frame::dcmd(ctx, frame::DCMD_CTRL_CACHE_FLUSH, mbox, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{SLOTS, SLOT_BYTES, WINDOW_BYTES};
    use crate::sim::{Sim, BASE};

    fn up(sim: &mut Sim) -> Mfi {
        let lay = Layout::in_window(BASE, WINDOW_BYTES).unwrap();
        Mfi::bring_up(sim, lay, 0x1234, 10_000).expect("bring-up")
    }

    #[test]
    fn bring_up_info_and_lds() {
        let mut sim = Sim::new(&[(0, 2048), (1, 8192)]);
        let mut m = up(&mut sim);
        assert_eq!(m.fw.max_cmds(), 1008);
        assert!(sim.intr_enabled());
        let ci = m.ctrl_info(&mut sim).unwrap();
        assert_eq!(ci.product.as_str(), "Sim MegaRAID SAS 9260-8i");
        assert_eq!(ci.lds_present, 2);
        let l = m.ld_list(&mut sim).unwrap();
        assert_eq!(l.count, 2);
        assert_eq!(l.lds[1].target, 1);
        assert_eq!(l.lds[1].sectors, 8192);
        assert_eq!(m.read_capacity(&mut sim, 1).unwrap(), (8192, 512));
        assert!(matches!(m.read_capacity(&mut sim, 9), Err(Error::Status(frame::STAT_DEVICE_NOT_FOUND, _))));
        m.flush_sync(&mut sim).unwrap();
        assert_eq!(sim.flushes, 1);
        assert_eq!(m.stats.stray, 0);
    }

    #[test]
    fn handshake_from_operational_and_boot_states() {
        // A previous driver left it OPERATIONAL: reset to READY first.
        let mut sim = Sim::new(&[(0, 64)]);
        let _ = up(&mut sim);
        let mut m = up(&mut sim);
        assert!(sim.resets >= 1);
        assert_eq!(m.ld_list(&mut sim).unwrap().count, 1);
        // A cold controller that needs the handshake doorbells.
        let mut sim = Sim::new(&[(0, 64)]);
        sim.boot_script(&[0x4000_0000, 0x6000_0000, 0x9000_0000, 0x8000_0000]);
        let m = up(&mut sim);
        assert_eq!(m.fw.max_cmds(), 1008);
        assert!(sim.saw_doorbell(regs::IDB_CLEAR_HANDSHAKE | regs::IDB_HOTPLUG));
        assert!(sim.saw_doorbell(regs::IDB_HOTPLUG));
    }

    #[test]
    fn fault_and_stuck_firmware() {
        let mut sim = Sim::new(&[(0, 64)]);
        sim.force_status(Some(0xF000_00AB));
        let lay = Layout::in_window(BASE, WINDOW_BYTES).unwrap();
        assert_eq!(Mfi::bring_up(&mut sim, lay, 1, 1000).err(), Some(Error::Fault(0xF000_00AB)));
        sim.force_status(Some(0x8000_0000));
        assert_eq!(Mfi::bring_up(&mut sim, lay, 1, 1000).err(), Some(Error::HandshakeTimeout(FwState::DeviceScan)));
        sim.force_status(Some(0xB000_0000)); // READY but zero commands
        assert_eq!(Mfi::bring_up(&mut sim, lay, 1, 1000).err(), Some(Error::RingSize(0)));
        sim.force_status(Some(0xB000_0000 | 4000));
        assert_eq!(Mfi::bring_up(&mut sim, lay, 1, 1000).err(), Some(Error::RingSize(4000)));
    }

    fn pattern(seed: u8, n: usize) -> std::vec::Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31) ^ seed ^ (i >> 9) as u8).collect()
    }

    #[test]
    fn ld_write_read_through_the_reply_ring() {
        let mut sim = Sim::new(&[(0, 4096), (1, 4096)]);
        sim.set_deferred(true); // completions only when the "device" gets time
        let mut m = up(&mut sim);
        let lay = *m.layout();
        // Four writes in flight at once, one per slot, then reap them together.
        for s in 0..SLOTS {
            let data = pattern(s as u8, SLOT_BYTES as usize);
            sim.dma_write(lay.slot(s).unwrap(), &data[..]);
            m.post_ld_io(&mut sim, 100 + s as u32, true, 1, s as u64 * 128, 128, lay.slot(s).unwrap()).unwrap();
        }
        assert_eq!(m.in_flight(), SLOTS);
        sim.run_device();
        let mut users = std::vec::Vec::new();
        m.reap(&mut sim, |c| {
            assert_eq!(c.status, frame::STAT_OK);
            users.push(c.user);
        });
        users.sort();
        assert_eq!(users, [100, 101, 102, 103]);
        assert_eq!(m.in_flight(), 0);
        // On the disk, byte for byte.
        for s in 0..SLOTS {
            let want = pattern(s as u8, SLOT_BYTES as usize);
            assert_eq!(&sim.disk(1)[s * 65536..(s + 1) * 65536], &want[..]);
        }
        // Read back through a different slot.
        let zero = std::vec![0u8; SLOT_BYTES as usize];
        sim.dma_write(lay.slot(0).unwrap(), &zero);
        m.post_ld_io(&mut sim, 7, false, 1, 3 * 128, 128, lay.slot(0).unwrap()).unwrap();
        sim.run_device();
        let mut ok = false;
        m.reap(&mut sim, |c| ok = c.user == 7 && c.status == 0);
        assert!(ok);
        let mut back = std::vec![0u8; SLOT_BYTES as usize];
        sim.dma_read(lay.slot(0).unwrap(), &mut back);
        assert_eq!(&back[..], &pattern(3, SLOT_BYTES as usize)[..]);
    }

    #[test]
    fn errors_come_back_as_status() {
        let mut sim = Sim::new(&[(0, 256)]);
        let mut m = up(&mut sim);
        let lay = *m.layout();
        m.post_ld_io(&mut sim, 1, false, 0, 250, 16, lay.slot(0).unwrap()).unwrap();
        m.post_ld_io(&mut sim, 2, false, 5, 0, 1, lay.slot(1).unwrap()).unwrap();
        let mut got = std::vec::Vec::new();
        m.reap(&mut sim, |c| got.push((c.user, c.status)));
        got.sort();
        assert_eq!(got, [(1, frame::STAT_LD_LBA_OUT_OF_RANGE), (2, frame::STAT_DEVICE_NOT_FOUND)]);
        // A buffer outside the window is refused before anything is posted.
        assert_eq!(m.post_ld_io(&mut sim, 3, false, 0, 0, 1, BASE - 4096), Err(Error::Frame));
    }

    #[test]
    fn stray_and_stale_contexts_are_ignored() {
        let mut sim = Sim::new(&[(0, 256)]);
        let mut m = up(&mut sim);
        let lay = *m.layout();
        sim.set_deferred(true);
        m.post_ld_io(&mut sim, 1, false, 0, 0, 8, lay.slot(0).unwrap()).unwrap();
        // Garbage, a previous generation's context for the same frame, a frame
        // that is not in flight, and our own context before the device finished.
        sim.inject_context(0xFFFF_FFFF);
        sim.inject_context(Mfi::context(0x1233, 0));
        sim.inject_context(Mfi::context(0x1234, 5));
        sim.inject_context(Mfi::context(0x1234, 0));
        let mut n = 0;
        m.reap(&mut sim, |_| n += 1);
        assert_eq!(n, 0);
        assert_eq!(m.stats.stray, 4);
        assert_eq!(m.in_flight(), 1);
        sim.run_device();
        m.reap(&mut sim, |c| {
            assert_eq!(c.user, 1);
            n += 1
        });
        assert_eq!(n, 1);
        // A producer index outside the ring is not followed.
        sim.dma_write(lay.producer(), &5000u32.to_le_bytes());
        assert_eq!(m.reap(&mut sim, |_| panic!("no completion expected")), 0);
        assert_eq!(m.stats.bad_producer, 1);
    }

    #[test]
    fn ring_wraps_at_the_firmware_command_count() {
        let mut sim = Sim::with_cmds(&[(0, 64)], 5);
        let mut m = up(&mut sim);
        let lay = *m.layout();
        for k in 0..23u32 {
            m.post_ld_io(&mut sim, k, false, 0, 0, 1, lay.slot(0).unwrap()).unwrap();
            let mut got = None;
            m.reap(&mut sim, |c| got = Some(c.user));
            assert_eq!(got, Some(k));
        }
        assert_eq!(m.stats.completed, 23);
        assert_eq!(m.stats.stray, 0);
    }

    /// A HAL that reads DMA memory a byte at a time in ascending order, like
    /// megaraidd, and lets the firmware run its queued commands right after
    /// the `at`-th byte read.
    struct ByteWise<'a> {
        sim: &'a mut Sim,
        read: usize,
        at: usize,
    }

    impl Hal for ByteWise<'_> {
        fn read32(&mut self, off: u32) -> u32 {
            self.sim.read32(off)
        }
        fn write32(&mut self, off: u32, val: u32) {
            self.sim.write32(off, val)
        }
        fn dma_read(&mut self, addr: u64, out: &mut [u8]) {
            for (k, b) in out.iter_mut().enumerate() {
                let mut one = [0u8; 1];
                self.sim.dma_read(addr + k as u64, &mut one);
                *b = one[0];
                self.read += 1;
                if self.read == self.at {
                    self.sim.run_device();
                }
            }
        }
        fn dma_write(&mut self, addr: u64, data: &[u8]) {
            self.sim.dma_write(addr, data)
        }
        fn now_ms(&mut self) -> u64 {
            self.sim.now_ms()
        }
        fn relax(&mut self) {
            self.sim.relax()
        }
    }

    #[test]
    fn a_producer_index_updated_during_the_read_is_not_torn() {
        // Producer at 0x00FF; the firmware completes one command (0x00FF ->
        // 0x0100) between bytes of the driver's read of the index. That one
        // completion must be reaped, nothing counted stray, and the next
        // command must still complete (the consumer stays in step).
        for at in 1..=8 {
            let mut sim = Sim::new(&[(0, 64)]);
            let mut m = up(&mut sim);
            let lay = *m.layout();
            let producer = |sim: &mut Sim| {
                let mut b = [0u8; 4];
                sim.dma_read(lay.producer(), &mut b);
                u32::from_le_bytes(b)
            };
            while producer(&mut sim) != 0xFF {
                m.post_ld_io(&mut sim, 0, false, 0, 0, 1, lay.slot(0).unwrap()).unwrap();
                assert_eq!(m.reap(&mut sim, |_| {}), 1);
            }
            sim.set_deferred(true);
            m.post_ld_io(&mut sim, 7, false, 0, 0, 1, lay.slot(0).unwrap()).unwrap();
            let mut got = std::vec::Vec::new();
            {
                let mut h = ByteWise { sim: &mut sim, read: 0, at };
                for _ in 0..4 {
                    m.reap(&mut h, |c| got.push(c.user));
                }
            }
            assert_eq!(got, [7], "firmware update after byte {at}");
            assert_eq!(m.stats.stray, 0, "unwritten reply slots taken (update after byte {at})");
            m.post_ld_io(&mut sim, 8, false, 0, 0, 1, lay.slot(0).unwrap()).unwrap();
            sim.run_device();
            m.reap(&mut sim, |c| got.push(c.user));
            assert_eq!(got, [7, 8], "the next command completes (update after byte {at})");
        }
    }

    #[test]
    fn frames_run_out_cleanly() {
        let mut sim = Sim::new(&[(0, 64)]);
        sim.set_deferred(true);
        let mut m = up(&mut sim);
        let lay = *m.layout();
        for k in 0..MAX_FRAMES as u32 {
            m.post_ld_io(&mut sim, k, false, 0, 0, 1, lay.slot(0).unwrap()).unwrap();
        }
        assert_eq!(m.post_ld_io(&mut sim, 99, false, 0, 0, 1, lay.slot(0).unwrap()), Err(Error::Busy));
        assert_eq!(m.ctrl_info(&mut sim).err(), Some(Error::NotIdle));
        sim.run_device();
        assert_eq!(m.reap(&mut sim, |_| {}), MAX_FRAMES);
    }

    #[test]
    fn restart_with_a_new_generation_drops_late_completions() {
        let mut sim = Sim::new(&[(0, 64)]);
        sim.set_deferred(true);
        let mut m1 = up(&mut sim);
        let lay = *m1.layout();
        m1.post_ld_io(&mut sim, 1, true, 0, 0, 8, lay.slot(0).unwrap()).unwrap();
        // The driver dies here. A new instance resets the controller.
        let mut m2 = Mfi::bring_up(&mut sim, lay, 0x4321, 10_000).unwrap();
        // The reset aborted the old command; a stale completion that shows up anyway is stray.
        sim.inject_context(Mfi::context(0x1234, 0));
        m2.post_ld_io(&mut sim, 2, false, 0, 0, 8, lay.slot(1).unwrap()).unwrap();
        sim.run_device();
        let mut got = std::vec::Vec::new();
        m2.reap(&mut sim, |c| got.push(c.user));
        assert_eq!(got, [2]);
        assert_eq!(m2.stats.stray, 1);
    }

    #[test]
    fn dma_probes_stay_outside_the_window() {
        let mut sim = Sim::new(&[(0, 64)]);
        let mut m = up(&mut sim);
        // The device would write controller info to a "kernel" page: the
        // simulated IOMMU refuses it and records the fault.
        let target = BASE + WINDOW_BYTES + 0x10_000;
        m.post_info_to(&mut sim, 5, target).unwrap();
        let mut got = None;
        m.reap(&mut sim, |c| got = Some(c.user));
        assert_eq!(got, Some(5));
        assert_eq!(sim.faults_write, 1);
        // A frame address outside the window: the read is refused, the status
        // write too, and the context that comes back is stray.
        m.post_foreign_frame(&mut sim, target).unwrap();
        m.reap(&mut sim, |_| panic!("a foreign frame must not complete one of ours"));
        assert!(sim.faults_read >= 1);
        assert!(m.stats.stray >= 1);
    }
}
