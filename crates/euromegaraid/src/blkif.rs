//! The block interface between the driver process and the kernel, carried as
//! EuroIPC messages. This is the thin coupling layer: the MFI logic does not
//! depend on it, so it can be swapped for a shared block-over-IPC interface.
//!
//! Data never travels in messages. A request names a **data slot** of the
//! driver's DMA window ([`crate::layout`]); the kernel copies write data into
//! the slot before sending `Write` and copies read data out after `Done`. The
//! slot index is checked on both sides; the driver never learns a kernel
//! address and the kernel never trusts one from the driver.
//!
//! | tag | message | direction | body (little-endian) |
//! |---|---|---|---|
//! | 1 | `Disk` | driver -> kernel | index u8, target u8, state u8, sectors u64, sector_size u32 |
//! | 2 | `Ready` | driver -> kernel | disks u8, slots u8, slot_bytes u32 |
//! | 3 | `Read` | kernel -> driver | tag u32, disk u8, slot u8, lba u64, sectors u32 |
//! | 4 | `Write` | kernel -> driver | same as `Read` |
//! | 5 | `Flush` | kernel -> driver | tag u32 |
//! | 6 | `Done` | driver -> kernel | tag u32, status u8, scsi u8 |
//! | 7 | `Irq` | kernel -> driver | count u32 |
//! | 8 | `Crash` | kernel -> driver | (self-test: fault now) |
//! | 9 | `CrashDuringWrite` | kernel -> driver | nth u32 (self-test: fault right after posting the nth write) |
//! | 10 | `DmaProbe` | kernel -> driver | kind u8, target u64 (self-test) |
//! | 11 | `Report` | driver -> kernel | code u8, a u64, b u64 |
//!
//! Decoding is strict: exact lengths, known tags only.

pub const TAG_DISK: u8 = 1;
pub const TAG_READY: u8 = 2;
pub const TAG_READ: u8 = 3;
pub const TAG_WRITE: u8 = 4;
pub const TAG_FLUSH: u8 = 5;
pub const TAG_DONE: u8 = 6;
pub const TAG_IRQ: u8 = 7;
pub const TAG_CRASH: u8 = 8;
pub const TAG_CRASH_DURING_WRITE: u8 = 9;
pub const TAG_DMA_PROBE: u8 = 10;
pub const TAG_REPORT: u8 = 11;

/// Largest encoded message.
pub const MAX_MSG: usize = 32;

/// `Done.status` values besides the MFI status codes.
pub const DONE_BAD_REQUEST: u8 = 0xF0;
pub const DONE_NO_FRAME: u8 = 0xF1;

/// `DmaProbe.kind`: aim a DCMD's data buffer (device writes) at the target.
pub const PROBE_DEVICE_WRITE: u8 = 1;
/// `DmaProbe.kind`: post a frame whose address is the target (device reads it).
pub const PROBE_DEVICE_READ: u8 = 2;
/// `Report.code`: a probe was posted and its outcome is known (a = kind, b = status).
pub const REPORT_PROBE_DONE: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    Disk { index: u8, target: u8, state: u8, sectors: u64, sector_size: u32 },
    Ready { disks: u8, slots: u8, slot_bytes: u32 },
    Read(Io),
    Write(Io),
    Flush { tag: u32 },
    Done { tag: u32, status: u8, scsi: u8 },
    Irq { count: u32 },
    Crash,
    CrashDuringWrite { nth: u32 },
    DmaProbe { kind: u8, target: u64 },
    Report { code: u8, a: u64, b: u64 },
}

/// One block request: `sectors` 512-byte sectors at `lba` of disk `disk`,
/// data in slot `slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Io {
    pub tag: u32,
    pub disk: u8,
    pub slot: u8,
    pub lba: u64,
    pub sectors: u32,
}

impl Io {
    /// Is this request acceptable for a disk of `disk_sectors` sectors, given
    /// `slots` slots of `slot_bytes` each? (Checked by the driver on receipt.)
    pub fn valid(&self, disk_sectors: u64, slots: usize, slot_bytes: u32) -> bool {
        let fits_slot = self.sectors > 0 && (self.sectors as u64) * 512 <= slot_bytes as u64;
        let in_disk = self.lba.checked_add(self.sectors as u64).is_some_and(|e| e <= disk_sectors);
        fits_slot && in_disk && (self.slot as usize) < slots
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlkError {
    Empty,
    UnknownTag(u8),
    BadLength,
    NoRoom,
}

struct W<'a> {
    b: &'a mut [u8],
    n: usize,
}

impl W<'_> {
    fn u8(&mut self, v: u8) {
        self.b[self.n] = v;
        self.n += 1;
    }
    fn u32(&mut self, v: u32) {
        self.b[self.n..self.n + 4].copy_from_slice(&v.to_le_bytes());
        self.n += 4;
    }
    fn u64(&mut self, v: u64) {
        self.b[self.n..self.n + 8].copy_from_slice(&v.to_le_bytes());
        self.n += 8;
    }
}

struct R<'a> {
    b: &'a [u8],
    n: usize,
}

impl R<'_> {
    fn u8(&mut self) -> u8 {
        let v = self.b[self.n];
        self.n += 1;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = u32::from_le_bytes([self.b[self.n], self.b[self.n + 1], self.b[self.n + 2], self.b[self.n + 3]]);
        self.n += 4;
        v
    }
    fn u64(&mut self) -> u64 {
        let mut a = [0u8; 8];
        a.copy_from_slice(&self.b[self.n..self.n + 8]);
        self.n += 8;
        u64::from_le_bytes(a)
    }
}

fn body_len(tag: u8) -> Option<usize> {
    Some(match tag {
        TAG_DISK => 15,
        TAG_READY => 6,
        TAG_READ | TAG_WRITE => 18,
        TAG_FLUSH => 4,
        TAG_DONE => 6,
        TAG_IRQ => 4,
        TAG_CRASH => 0,
        TAG_CRASH_DURING_WRITE => 4,
        TAG_DMA_PROBE => 9,
        TAG_REPORT => 17,
        _ => return None,
    })
}

fn tag_of(m: &Msg) -> u8 {
    match m {
        Msg::Disk { .. } => TAG_DISK,
        Msg::Ready { .. } => TAG_READY,
        Msg::Read(_) => TAG_READ,
        Msg::Write(_) => TAG_WRITE,
        Msg::Flush { .. } => TAG_FLUSH,
        Msg::Done { .. } => TAG_DONE,
        Msg::Irq { .. } => TAG_IRQ,
        Msg::Crash => TAG_CRASH,
        Msg::CrashDuringWrite { .. } => TAG_CRASH_DURING_WRITE,
        Msg::DmaProbe { .. } => TAG_DMA_PROBE,
        Msg::Report { .. } => TAG_REPORT,
    }
}

/// Encode `m` into `out`; returns the length.
pub fn encode(m: &Msg, out: &mut [u8]) -> Result<usize, BlkError> {
    let tag = tag_of(m);
    let need = 1 + body_len(tag).unwrap_or(0);
    if out.len() < need {
        return Err(BlkError::NoRoom);
    }
    let mut w = W { b: out, n: 0 };
    w.u8(tag);
    match *m {
        Msg::Disk { index, target, state, sectors, sector_size } => {
            w.u8(index);
            w.u8(target);
            w.u8(state);
            w.u64(sectors);
            w.u32(sector_size);
        }
        Msg::Ready { disks, slots, slot_bytes } => {
            w.u8(disks);
            w.u8(slots);
            w.u32(slot_bytes);
        }
        Msg::Read(io) | Msg::Write(io) => {
            w.u32(io.tag);
            w.u8(io.disk);
            w.u8(io.slot);
            w.u64(io.lba);
            w.u32(io.sectors);
        }
        Msg::Flush { tag } => w.u32(tag),
        Msg::Done { tag, status, scsi } => {
            w.u32(tag);
            w.u8(status);
            w.u8(scsi);
        }
        Msg::Irq { count } => w.u32(count),
        Msg::Crash => {}
        Msg::CrashDuringWrite { nth } => w.u32(nth),
        Msg::DmaProbe { kind, target } => {
            w.u8(kind);
            w.u64(target);
        }
        Msg::Report { code, a, b } => {
            w.u8(code);
            w.u64(a);
            w.u64(b);
        }
    }
    debug_assert_eq!(w.n, need);
    Ok(need)
}

/// Decode one message (strict).
pub fn decode(b: &[u8]) -> Result<Msg, BlkError> {
    let (&tag, body) = b.split_first().ok_or(BlkError::Empty)?;
    let want = body_len(tag).ok_or(BlkError::UnknownTag(tag))?;
    if body.len() != want {
        return Err(BlkError::BadLength);
    }
    let mut r = R { b: body, n: 0 };
    Ok(match tag {
        TAG_DISK => Msg::Disk { index: r.u8(), target: r.u8(), state: r.u8(), sectors: r.u64(), sector_size: r.u32() },
        TAG_READY => Msg::Ready { disks: r.u8(), slots: r.u8(), slot_bytes: r.u32() },
        TAG_READ | TAG_WRITE => {
            let io = Io { tag: r.u32(), disk: r.u8(), slot: r.u8(), lba: r.u64(), sectors: r.u32() };
            if tag == TAG_READ { Msg::Read(io) } else { Msg::Write(io) }
        }
        TAG_FLUSH => Msg::Flush { tag: r.u32() },
        TAG_DONE => Msg::Done { tag: r.u32(), status: r.u8(), scsi: r.u8() },
        TAG_IRQ => Msg::Irq { count: r.u32() },
        TAG_CRASH => Msg::Crash,
        TAG_CRASH_DURING_WRITE => Msg::CrashDuringWrite { nth: r.u32() },
        TAG_DMA_PROBE => Msg::DmaProbe { kind: r.u8(), target: r.u64() },
        TAG_REPORT => Msg::Report { code: r.u8(), a: r.u64(), b: r.u64() },
        t => return Err(BlkError::UnknownTag(t)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(m: Msg) {
        let mut b = [0u8; MAX_MSG];
        let n = encode(&m, &mut b).unwrap();
        assert!(n <= MAX_MSG);
        assert_eq!(decode(&b[..n]), Ok(m));
        // One byte short or long is an error, never a guess.
        assert_eq!(decode(&b[..n - 1]).err().map(|e| e == BlkError::BadLength || n == 1), Some(true));
        let mut long = [0u8; MAX_MSG + 1];
        long[..n].copy_from_slice(&b[..n]);
        assert_eq!(decode(&long[..n + 1]), Err(BlkError::BadLength));
    }

    #[test]
    fn every_message_roundtrips() {
        let io = Io { tag: 0xDEAD_BEEF, disk: 1, slot: 3, lba: u64::MAX - 7, sectors: 128 };
        rt(Msg::Disk { index: 0, target: 1, state: 3, sectors: 1 << 40, sector_size: 512 });
        rt(Msg::Ready { disks: 2, slots: 4, slot_bytes: 65536 });
        rt(Msg::Read(io));
        rt(Msg::Write(io));
        rt(Msg::Flush { tag: 5 });
        rt(Msg::Done { tag: 5, status: 0x2D, scsi: 2 });
        rt(Msg::Irq { count: 9 });
        rt(Msg::CrashDuringWrite { nth: 3 });
        rt(Msg::DmaProbe { kind: PROBE_DEVICE_READ, target: 0x1234_5000 });
        rt(Msg::Report { code: REPORT_PROBE_DONE, a: 1, b: 0 });
        let mut b = [0u8; 4];
        assert_eq!(encode(&Msg::Crash, &mut b), Ok(1));
        assert_eq!(decode(&b[..1]), Ok(Msg::Crash));
    }

    #[test]
    fn strictness() {
        assert_eq!(decode(&[]), Err(BlkError::Empty));
        assert_eq!(decode(&[0]), Err(BlkError::UnknownTag(0)));
        assert_eq!(decode(&[200, 0, 0]), Err(BlkError::UnknownTag(200)));
        assert_eq!(decode(&[TAG_CRASH, 0]), Err(BlkError::BadLength));
        let mut small = [0u8; 5];
        assert_eq!(encode(&Msg::Read(Io { tag: 0, disk: 0, slot: 0, lba: 0, sectors: 1 }), &mut small), Err(BlkError::NoRoom));
    }

    #[test]
    fn request_validation() {
        let ok = Io { tag: 1, disk: 0, slot: 0, lba: 0, sectors: 128 };
        assert!(ok.valid(1000, 4, 65536));
        assert!(!Io { sectors: 129, ..ok }.valid(1000, 4, 65536)); // larger than a slot
        assert!(!Io { sectors: 0, ..ok }.valid(1000, 4, 65536));
        assert!(!Io { slot: 4, ..ok }.valid(1000, 4, 65536));
        assert!(!Io { lba: 900, ..ok }.valid(1000, 4, 65536)); // runs past the end
        assert!(Io { lba: 872, ..ok }.valid(1000, 4, 65536)); // exactly to the end
        assert!(!Io { lba: u64::MAX, sectors: 1, ..ok }.valid(u64::MAX, 4, 65536)); // wraps
    }
}
