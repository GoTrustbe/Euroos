//! DHCP client (RFC 2131) on top of BOOTP — enough to obtain a real lease:
//! DISCOVER → OFFER → REQUEST → ACK. Builds the BOOTP payload (240 bytes + options)
//! that goes into a UDP datagram (port 68→67).

use alloc::vec::Vec;

use crate::ipv4::Ipv4Addr;

pub const DISCOVER: u8 = 1;
pub const OFFER: u8 = 2;
pub const REQUEST: u8 = 3;
pub const ACK: u8 = 5;

const MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];

/// Lease data extracted from an OFFER/ACK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpInfo {
    pub msg_type: u8,
    pub your_ip: Ipv4Addr,
    pub server_id: Ipv4Addr,
    pub subnet: Option<Ipv4Addr>,
    pub router: Option<Ipv4Addr>,
    pub dns: Option<Ipv4Addr>,
    pub lease_secs: u32,
}

/// Build a DHCP message (BOOTP request) with the given options.
pub fn build(
    msg_type: u8,
    xid: u32,
    mac: [u8; 6],
    requested_ip: Option<Ipv4Addr>,
    server_id: Option<Ipv4Addr>,
) -> Vec<u8> {
    let mut b = Vec::with_capacity(300);
    b.push(1); // op = BOOTREQUEST
    b.push(1); // htype = Ethernet
    b.push(6); // hlen
    b.push(0); // hops
    b.extend_from_slice(&xid.to_be_bytes());
    b.extend_from_slice(&0u16.to_be_bytes()); // secs
    b.extend_from_slice(&0x8000u16.to_be_bytes()); // flags = broadcast
    b.extend_from_slice(&[0; 4]); // ciaddr
    b.extend_from_slice(&[0; 4]); // yiaddr
    b.extend_from_slice(&[0; 4]); // siaddr
    b.extend_from_slice(&[0; 4]); // giaddr
    b.extend_from_slice(&mac); // chaddr (6) ...
    b.extend_from_slice(&[0; 10]); // ... + 10 padding = 16
    b.extend_from_slice(&[0; 64]); // sname
    b.extend_from_slice(&[0; 128]); // file
    b.extend_from_slice(&MAGIC); // magic cookie

    // Options.
    b.extend_from_slice(&[53, 1, msg_type]); // DHCP message type
    if let Some(ip) = requested_ip {
        b.push(50); // requested IP
        b.push(4);
        b.extend_from_slice(&ip.0);
    }
    if let Some(sid) = server_id {
        b.push(54); // server identifier
        b.push(4);
        b.extend_from_slice(&sid.0);
    }
    // Parameter request list: subnet, router, DNS, domain, lease.
    b.extend_from_slice(&[55, 5, 1, 3, 6, 15, 51]);
    b.push(255); // end
    b
}

fn opt4(buf: &[u8], code: u8) -> Option<Ipv4Addr> {
    let mut i = 240;
    while i + 1 < buf.len() {
        let c = buf[i];
        if c == 255 {
            break;
        }
        if c == 0 {
            i += 1;
            continue;
        }
        let len = buf[i + 1] as usize;
        if c == code && len == 4 && i + 2 + 4 <= buf.len() {
            return Some(Ipv4Addr([buf[i + 2], buf[i + 3], buf[i + 4], buf[i + 5]]));
        }
        i += 2 + len;
    }
    None
}

/// Parse a DHCP reply (OFFER/ACK); returns the lease info.
pub fn parse(buf: &[u8]) -> Option<DhcpInfo> {
    if buf.len() < 240 || buf[236..240] != MAGIC {
        return None;
    }
    let your_ip = Ipv4Addr([buf[16], buf[17], buf[18], buf[19]]);
    // Option 53 = message type.
    let mut msg_type = 0u8;
    let mut lease_secs = 0u32;
    let mut i = 240;
    while i + 1 < buf.len() {
        let c = buf[i];
        if c == 255 {
            break;
        }
        if c == 0 {
            i += 1;
            continue;
        }
        let len = buf[i + 1] as usize;
        if i + 2 + len > buf.len() {
            break;
        }
        match c {
            53 if len == 1 => msg_type = buf[i + 2],
            51 if len == 4 => {
                lease_secs = u32::from_be_bytes([buf[i + 2], buf[i + 3], buf[i + 4], buf[i + 5]])
            }
            _ => {}
        }
        i += 2 + len;
    }
    Some(DhcpInfo {
        msg_type,
        your_ip,
        server_id: opt4(buf, 54).unwrap_or(Ipv4Addr([0; 4])),
        subnet: opt4(buf, 1),
        router: opt4(buf, 3),
        dns: opt4(buf, 6),
        lease_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_roundtrip_shape() {
        let mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let d = build(DISCOVER, 0xCAFEBABE, mac, None, None);
        assert!(d.len() >= 240);
        assert_eq!(&d[236..240], &MAGIC);
        assert_eq!(d[0], 1); // BOOTREQUEST
        assert_eq!(&d[28..34], &mac); // chaddr
        // option 53 = DISCOVER
        assert_eq!(&d[240..243], &[53, 1, DISCOVER]);
    }

    #[test]
    fn parse_offer() {
        let mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        // Build an 'OFFER' by setting yiaddr + options ourselves.
        let mut b = build(OFFER, 1, mac, None, Some(Ipv4Addr([10, 0, 2, 2])));
        b[16..20].copy_from_slice(&[10, 0, 2, 15]); // yiaddr
        let info = parse(&b).unwrap();
        assert_eq!(info.msg_type, OFFER);
        assert_eq!(info.your_ip, Ipv4Addr([10, 0, 2, 15]));
        assert_eq!(info.server_id, Ipv4Addr([10, 0, 2, 2]));
    }
}
