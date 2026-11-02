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

/// Smallest frame: an Ethernet header.
pub const MIN_FRAME: usize = 14;
/// Largest frame: 1500 bytes payload, 14 header, 4 VLAN tag (no FCS).
pub const MAX_FRAME: usize = 1518;
/// Largest encoded message.
pub const MAX_MSG: usize = 1 + MAX_FRAME;

/// Report codes (driver -> netstack, for the self-tests and the log).
pub const REPORT_DMA_ESCAPE_DONE: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg<'a> {
    Hello { mac: [u8; 6] },
    Tx(&'a [u8]),
    Rx(&'a [u8]),
    Irq { count: u32 },
    Crash,
    DmaEscape { rx_target: u64, tx_target: u64 },
    Report { code: u8, a: u64, b: u64 },
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
    fn no_room_is_an_error_not_a_truncation() {
        let mut small = [0u8; 10];
        assert_eq!(encode(&Msg::Tx(&[0u8; 60]), &mut small), Err(ProtoError::NoRoom));
        assert_eq!(encode(&Msg::Report { code: 0, a: 0, b: 0 }, &mut small), Err(ProtoError::NoRoom));
        let mut seven = [0u8; 7];
        assert_eq!(encode(&Msg::Hello { mac: [1; 6] }, &mut seven), Ok(7));
    }
}
