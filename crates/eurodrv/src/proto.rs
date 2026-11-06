//! The wire format between the netstack, the kernel and a NIC driver, carried
//! as EuroIPC messages. One message is one tag byte plus a fixed or bounded
//! body; a frame is a whole Ethernet frame without FCS.
//!
//! | tag | message | direction | body |
//! |---|---|---|---|
//! | 1 | `Hello` | driver -> netstack | MAC (6) : the link is up |
//! | 2 | `Tx` | netstack -> driver | frame (14..=1518) |
//! | 3 | `Rx` | driver -> netstack | frame (14..=1518) |
//! | 4 | `Irq` | kernel -> driver | count (u32 LE) since the last delivery |
//! | 5 | `Crash` | kernel -> driver | none (self-test: fault on purpose) |
//! | 6 | `DmaEscape` | kernel -> driver | rx target (u64 LE), tx target (u64 LE) (self-test) |
//! | 7 | `Report` | driver -> netstack | code (1), a (u64 LE), b (u64 LE) |
//! | 8 | `BlkInfo` | disk driver -> kernel | sectors (u64 LE), sector size (u32 LE): the disk is up |
//! | 9 | `BlkRead` | kernel -> disk driver | id (u32), lba (u64), count (u16) |
//! | 10 | `BlkWrite` | kernel -> disk driver | id (u32), lba (u64), data (512..=BLK_CHUNK, whole 512-byte units) |
//! | 11 | `BlkDone` | disk driver -> kernel | id (u32), status (u16, 0 = ok), data (0..=BLK_CHUNK, a read's bytes) |
//! | 12 | `CrashAfterWrites` | kernel -> disk driver | n (u32) (self-test: fault after n more writes) |
//! | 13 | `DmaProbe` | kernel -> disk driver | target (u64 LE) (self-test: a read aimed outside the window) |
//!
//! The block messages carry at most [`BLK_CHUNK`] bytes of data each; the
//! kernel splits a filesystem block into chunks.
//!
//! Decoding is strict: a wrong length, an unknown tag or an out-of-range frame
//! is an error, never a guess. Both sides are untrusted by each other.

pub const TAG_HELLO: u8 = 1;
pub const TAG_TX: u8 = 2;
pub const TAG_RX: u8 = 3;
pub const TAG_IRQ: u8 = 4;
pub const TAG_CRASH: u8 = 5;
pub const TAG_DMA_ESCAPE: u8 = 6;
pub const TAG_REPORT: u8 = 7;
pub const TAG_BLK_INFO: u8 = 8;
pub const TAG_BLK_READ: u8 = 9;
pub const TAG_BLK_WRITE: u8 = 10;
pub const TAG_BLK_DONE: u8 = 11;
pub const TAG_CRASH_AFTER_WRITES: u8 = 12;
pub const TAG_DMA_PROBE: u8 = 13;

/// Largest data payload of one block message.
pub const BLK_CHUNK: usize = 1024;
/// Block data comes in whole units of this size.
pub const BLK_UNIT: usize = 512;

/// Smallest frame: an Ethernet header.
pub const MIN_FRAME: usize = 14;
/// Largest frame: 1500 bytes payload, 14 header, 4 VLAN tag (no FCS).
pub const MAX_FRAME: usize = 1518;
/// Largest encoded message.
pub const MAX_MSG: usize = 1 + MAX_FRAME;

/// Report codes (driver -> netstack, for the self-tests and the log).
pub const REPORT_DMA_ESCAPE_DONE: u8 = 1;
/// The disk driver ran the `DmaProbe` read (a = 1 when the device reported an error).
pub const REPORT_DMA_PROBE_DONE: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg<'a> {
    Hello { mac: [u8; 6] },
    Tx(&'a [u8]),
    Rx(&'a [u8]),
    Irq { count: u32 },
    Crash,
    DmaEscape { rx_target: u64, tx_target: u64 },
    Report { code: u8, a: u64, b: u64 },
    BlkInfo { sectors: u64, sector_size: u32 },
    BlkRead { id: u32, lba: u64, count: u16 },
    BlkWrite { id: u32, lba: u64, data: &'a [u8] },
    BlkDone { id: u32, status: u16, data: &'a [u8] },
    CrashAfterWrites { n: u32 },
    DmaProbe { target: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtoError {
    Empty,
    UnknownTag(u8),
    BadLength,
    FrameSize,
    /// The output buffer is too small.
    NoRoom,
}

fn frame_ok(f: &[u8]) -> bool {
    (MIN_FRAME..=MAX_FRAME).contains(&f.len())
}

fn blk_write_ok(d: &[u8]) -> bool {
    !d.is_empty() && d.len() <= BLK_CHUNK && d.len() % BLK_UNIT == 0
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// Encode `msg` into `out`; returns the length.
pub fn encode(msg: &Msg, out: &mut [u8]) -> Result<usize, ProtoError> {
    let need = match msg {
        Msg::Hello { .. } => 7,
        Msg::Tx(f) | Msg::Rx(f) => {
            if !frame_ok(f) {
                return Err(ProtoError::FrameSize);
            }
            1 + f.len()
        }
        Msg::Irq { .. } => 5,
        Msg::Crash => 1,
        Msg::DmaEscape { .. } => 17,
        Msg::Report { .. } => 18,
        Msg::BlkInfo { .. } => 13,
        Msg::BlkRead { .. } => 15,
        Msg::BlkWrite { data, .. } => {
            if !blk_write_ok(data) {
                return Err(ProtoError::FrameSize);
            }
            13 + data.len()
        }
        Msg::BlkDone { data, .. } => {
            if data.len() > BLK_CHUNK {
                return Err(ProtoError::FrameSize);
            }
            7 + data.len()
        }
        Msg::CrashAfterWrites { .. } => 5,
        Msg::DmaProbe { .. } => 9,
    };
    if out.len() < need {
        return Err(ProtoError::NoRoom);
    }
    match msg {
        Msg::Hello { mac } => {
            out[0] = TAG_HELLO;
            out[1..7].copy_from_slice(mac);
        }
        Msg::Tx(f) => {
            out[0] = TAG_TX;
            out[1..need].copy_from_slice(f);
        }
        Msg::Rx(f) => {
            out[0] = TAG_RX;
            out[1..need].copy_from_slice(f);
        }
        Msg::Irq { count } => {
            out[0] = TAG_IRQ;
            out[1..5].copy_from_slice(&count.to_le_bytes());
        }
        Msg::Crash => out[0] = TAG_CRASH,
        Msg::DmaEscape { rx_target, tx_target } => {
            out[0] = TAG_DMA_ESCAPE;
            out[1..9].copy_from_slice(&rx_target.to_le_bytes());
            out[9..17].copy_from_slice(&tx_target.to_le_bytes());
        }
        Msg::Report { code, a, b } => {
            out[0] = TAG_REPORT;
            out[1] = *code;
            out[2..10].copy_from_slice(&a.to_le_bytes());
            out[10..18].copy_from_slice(&b.to_le_bytes());
        }
        Msg::BlkInfo { sectors, sector_size } => {
            out[0] = TAG_BLK_INFO;
            out[1..9].copy_from_slice(&sectors.to_le_bytes());
            out[9..13].copy_from_slice(&sector_size.to_le_bytes());
        }
        Msg::BlkRead { id, lba, count } => {
            out[0] = TAG_BLK_READ;
            out[1..5].copy_from_slice(&id.to_le_bytes());
            out[5..13].copy_from_slice(&lba.to_le_bytes());
            out[13..15].copy_from_slice(&count.to_le_bytes());
        }
        Msg::BlkWrite { id, lba, data } => {
            out[0] = TAG_BLK_WRITE;
            out[1..5].copy_from_slice(&id.to_le_bytes());
            out[5..13].copy_from_slice(&lba.to_le_bytes());
            out[13..need].copy_from_slice(data);
        }
        Msg::BlkDone { id, status, data } => {
            out[0] = TAG_BLK_DONE;
            out[1..5].copy_from_slice(&id.to_le_bytes());
            out[5..7].copy_from_slice(&status.to_le_bytes());
            out[7..need].copy_from_slice(data);
        }
        Msg::CrashAfterWrites { n } => {
            out[0] = TAG_CRASH_AFTER_WRITES;
            out[1..5].copy_from_slice(&n.to_le_bytes());
        }
        Msg::DmaProbe { target } => {
            out[0] = TAG_DMA_PROBE;
            out[1..9].copy_from_slice(&target.to_le_bytes());
        }
    }
    Ok(need)
}

/// Decode one message. Strict: exact lengths for fixed bodies, bounded frames.
pub fn decode(b: &[u8]) -> Result<Msg<'_>, ProtoError> {
    let (&tag, body) = b.split_first().ok_or(ProtoError::Empty)?;
    let exact = |n: usize| if body.len() == n { Ok(()) } else { Err(ProtoError::BadLength) };
    match tag {
        TAG_HELLO => {
            exact(6)?;
            let mut mac = [0u8; 6];
            mac.copy_from_slice(body);
            Ok(Msg::Hello { mac })
        }
        TAG_TX | TAG_RX => {
            if !frame_ok(body) {
                return Err(ProtoError::FrameSize);
            }
            Ok(if tag == TAG_TX { Msg::Tx(body) } else { Msg::Rx(body) })
        }
        TAG_IRQ => {
            exact(4)?;
            Ok(Msg::Irq { count: u32::from_le_bytes([body[0], body[1], body[2], body[3]]) })
        }
        TAG_CRASH => {
            exact(0)?;
            Ok(Msg::Crash)
        }
        TAG_DMA_ESCAPE => {
            exact(16)?;
            Ok(Msg::DmaEscape { rx_target: u64_at(body, 0), tx_target: u64_at(body, 8) })
        }
        TAG_REPORT => {
            exact(17)?;
            Ok(Msg::Report { code: body[0], a: u64_at(body, 1), b: u64_at(body, 9) })
        }
        TAG_BLK_INFO => {
            exact(12)?;
            Ok(Msg::BlkInfo { sectors: u64_at(body, 0), sector_size: u32_at(body, 8) })
        }
        TAG_BLK_READ => {
            exact(14)?;
            Ok(Msg::BlkRead { id: u32_at(body, 0), lba: u64_at(body, 4), count: u16::from_le_bytes([body[12], body[13]]) })
        }
        TAG_BLK_WRITE => {
            if body.len() < 12 {
                return Err(ProtoError::BadLength);
            }
            let data = &body[12..];
            if !blk_write_ok(data) {
                return Err(ProtoError::FrameSize);
            }
            Ok(Msg::BlkWrite { id: u32_at(body, 0), lba: u64_at(body, 4), data })
        }
        TAG_BLK_DONE => {
            if body.len() < 6 {
                return Err(ProtoError::BadLength);
            }
            let data = &body[6..];
            if data.len() > BLK_CHUNK {
                return Err(ProtoError::FrameSize);
            }
            Ok(Msg::BlkDone { id: u32_at(body, 0), status: u16::from_le_bytes([body[4], body[5]]), data })
        }
        TAG_CRASH_AFTER_WRITES => {
            exact(4)?;
            Ok(Msg::CrashAfterWrites { n: u32_at(body, 0) })
        }
        TAG_DMA_PROBE => {
            exact(8)?;
            Ok(Msg::DmaProbe { target: u64_at(body, 0) })
        }
        t => Err(ProtoError::UnknownTag(t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(m: Msg) {
        let mut buf = [0u8; MAX_MSG];
        let n = encode(&m, &mut buf).unwrap();
        assert_eq!(decode(&buf[..n]).unwrap(), m);
    }

    #[test]
    fn every_message_roundtrips() {
        let frame = [0xabu8; 60];
        let max = [7u8; MAX_FRAME];
        let min = [1u8; MIN_FRAME];
        rt(Msg::Hello { mac: [0x52, 0x54, 0, 0x12, 0x34, 0x56] });
        rt(Msg::Tx(&frame));
        rt(Msg::Rx(&max));
        rt(Msg::Rx(&min));
        rt(Msg::Irq { count: 0xdead_beef });
        rt(Msg::Crash);
        rt(Msg::DmaEscape { rx_target: 0x0010_0000, tx_target: u64::MAX });
        rt(Msg::Report { code: REPORT_DMA_ESCAPE_DONE, a: 1, b: 2 });
    }

    #[test]
    fn frames_out_of_range_are_refused_both_ways() {
        let mut buf = [0u8; MAX_MSG + 8];
        assert_eq!(encode(&Msg::Tx(&[0u8; 13]), &mut buf), Err(ProtoError::FrameSize));
        assert_eq!(encode(&Msg::Rx(&[0u8; MAX_FRAME + 1]), &mut buf), Err(ProtoError::FrameSize));
        let mut long = [0u8; MAX_FRAME + 2];
        long[0] = TAG_RX;
        assert_eq!(decode(&long), Err(ProtoError::FrameSize));
        assert_eq!(decode(&[TAG_TX, 1, 2, 3]), Err(ProtoError::FrameSize));
        assert_eq!(decode(&[TAG_TX]), Err(ProtoError::FrameSize));
    }

    #[test]
    fn strict_lengths_and_tags() {
        assert_eq!(decode(&[]), Err(ProtoError::Empty));
        assert_eq!(decode(&[0]), Err(ProtoError::UnknownTag(0)));
        assert_eq!(decode(&[200, 1]), Err(ProtoError::UnknownTag(200)));
        assert_eq!(decode(&[TAG_HELLO, 1, 2, 3, 4, 5]), Err(ProtoError::BadLength));
        assert_eq!(decode(&[TAG_HELLO, 1, 2, 3, 4, 5, 6, 7]), Err(ProtoError::BadLength));
        assert_eq!(decode(&[TAG_IRQ, 1, 0, 0]), Err(ProtoError::BadLength));
        assert_eq!(decode(&[TAG_CRASH, 0]), Err(ProtoError::BadLength));
        assert_eq!(decode(&[TAG_DMA_ESCAPE, 0, 0]), Err(ProtoError::BadLength));
        assert_eq!(decode(&[TAG_REPORT; 10]), Err(ProtoError::BadLength));
    }

    #[test]
    fn block_messages_roundtrip() {
        let chunk = [0x5au8; BLK_CHUNK];
        rt(Msg::BlkInfo { sectors: 131_072, sector_size: 512 });
        rt(Msg::BlkRead { id: 7, lba: u64::MAX - 1, count: 2 });
        rt(Msg::BlkWrite { id: 8, lba: 12, data: &chunk });
        rt(Msg::BlkWrite { id: 9, lba: 0, data: &chunk[..BLK_UNIT] });
        rt(Msg::BlkDone { id: 9, status: 0, data: &chunk });
        rt(Msg::BlkDone { id: 10, status: 0x281, data: &[] });
        rt(Msg::CrashAfterWrites { n: 20 });
        rt(Msg::DmaProbe { target: 0xdead_0000 });
        let mut buf = [0u8; MAX_MSG];
        assert!(encode(&Msg::BlkWrite { id: 1, lba: 0, data: &chunk }, &mut buf).unwrap() <= MAX_MSG);
    }

    #[test]
    fn block_data_must_be_whole_units_within_one_chunk() {
        let mut buf = [0u8; MAX_MSG];
        assert_eq!(encode(&Msg::BlkWrite { id: 1, lba: 0, data: &[] }, &mut buf), Err(ProtoError::FrameSize));
        assert_eq!(encode(&Msg::BlkWrite { id: 1, lba: 0, data: &[0u8; 100] }, &mut buf), Err(ProtoError::FrameSize));
        assert_eq!(encode(&Msg::BlkWrite { id: 1, lba: 0, data: &[0u8; BLK_CHUNK + BLK_UNIT] }, &mut buf), Err(ProtoError::FrameSize));
        assert_eq!(encode(&Msg::BlkDone { id: 1, status: 0, data: &[0u8; BLK_CHUNK + 1] }, &mut buf), Err(ProtoError::FrameSize));
        // Hostile input on the wire: short headers, odd data, too much data.
        assert_eq!(decode(&[TAG_BLK_WRITE, 0, 0, 0]), Err(ProtoError::BadLength));
        let mut odd = [0u8; 1 + 12 + 513];
        odd[0] = TAG_BLK_WRITE;
        assert_eq!(decode(&odd), Err(ProtoError::FrameSize));
        let mut big = [0u8; 1 + 6 + BLK_CHUNK + 1];
        big[0] = TAG_BLK_DONE;
        assert_eq!(decode(&big), Err(ProtoError::FrameSize));
        assert_eq!(decode(&[TAG_BLK_READ, 1, 2]), Err(ProtoError::BadLength));
        assert_eq!(decode(&[TAG_BLK_INFO; 12]), Err(ProtoError::BadLength));
        assert_eq!(decode(&[TAG_DMA_PROBE, 0]), Err(ProtoError::BadLength));
    }

    #[test]
    fn no_room_is_an_error_not_a_truncation() {
        let mut small = [0u8; 10];
        assert_eq!(encode(&Msg::Tx(&[0u8; 60]), &mut small), Err(ProtoError::NoRoom));
        assert_eq!(encode(&Msg::Report { code: 0, a: 0, b: 0 }, &mut small), Err(ProtoError::NoRoom));
        let mut seven = [0u8; 7];
        assert_eq!(encode(&Msg::Hello { mac: [1; 6] }, &mut seven), Ok(7));
    }
}
