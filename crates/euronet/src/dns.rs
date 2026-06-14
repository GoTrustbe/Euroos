//! Minimal DNS client (RFC 1035): build an A-record query and parse the
//! answers (including name-compression pointers). Goes in a UDP datagram
//! to port 53.

use alloc::vec::Vec;

use crate::ipv4::Ipv4Addr;

/// Build a DNS query for an A-record (IPv4) of `name`.
pub fn build_query(id: u16, name: &str) -> Vec<u8> {
    let mut q = Vec::with_capacity(32);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // flags: standard query, recursion desired
    q.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT = 1
    q.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // AN/NS/AR count = 0
    for label in name.split('.').filter(|l| !l.is_empty()) {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0); // end of QNAME
    q.extend_from_slice(&1u16.to_be_bytes()); // QTYPE = A
    q.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
    q
}

/// Skip a (possibly compressed) name; return the new offset.
/// Read the queried domain name from a DNS query (the question section), e.g.
/// "ads.doubleclick.net". Does not follow compression pointers — a query QNAME is
/// always literal. For EuroGuard's DNS-level filtering.
pub fn parse_query_name(buf: &[u8]) -> Option<alloc::string::String> {
    if buf.len() < 13 {
        return None;
    }
    let mut pos = 12; // after the 12-byte header
    let mut name = alloc::string::String::new();
    loop {
        if pos >= buf.len() {
            return None;
        }
        let len = buf[pos] as usize;
        if len == 0 {
            break;
        }
        if len & 0xC0 != 0 {
            return None; // no pointers in a query QNAME
        }
        pos += 1;
        if pos + len > buf.len() {
            return None;
        }
        if !name.is_empty() {
            name.push('.');
        }
        for &b in &buf[pos..pos + len] {
            name.push(b as char);
        }
        pos += len;
    }
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Skip a (possibly compressed) name; return the new offset.
fn skip_name(buf: &[u8], mut pos: usize) -> usize {
    loop {
        if pos >= buf.len() {
            return buf.len();
        }
        let b = buf[pos];
        if b == 0 {
            return pos + 1;
        }
        if b & 0xC0 == 0xC0 {
            return pos + 2; // compression pointer (2 bytes)
        }
        pos += 1 + b as usize;
    }
}

/// Parse the A-records (IPv4 addresses) from a DNS answer.
pub fn parse_a_records(buf: &[u8]) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    if buf.len() < 12 {
        return out;
    }
    let ancount = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    // Skip the question section: QNAME + QTYPE(2) + QCLASS(2).
    let mut pos = skip_name(buf, 12) + 4;
    for _ in 0..ancount {
        pos = skip_name(buf, pos);
        if pos + 10 > buf.len() {
            break;
        }
        let typ = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        if typ == 1 && rdlen == 4 && pos + 4 <= buf.len() {
            out.push(Ipv4Addr([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]));
        }
        pos += rdlen;
    }
    out
}

/// Parse the A-records from a DNS RESPONSE, but ONLY if the transaction ID matches
/// `expected_id` AND the QR bit (response) is set. This way a spoofed
/// or mismatched answer is rejected (anti-cache-poisoning) → empty list. The
/// caller should always use this function instead of `parse_a_records` directly.
pub fn parse_response(buf: &[u8], expected_id: u16) -> Vec<Ipv4Addr> {
    if buf.len() < 12 {
        return Vec::new();
    }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    let is_response = buf[2] & 0x80 != 0; // QR bit (response)
    if id != expected_id || !is_response {
        return Vec::new();
    }
    parse_a_records(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_shape() {
        let q = build_query(0xABCD, "euro-os.eu");
        assert_eq!(&q[0..2], &[0xAB, 0xCD]);
        assert_eq!(&q[4..6], &[0, 1]); // QDCOUNT = 1
        // labels: 7"euro-os" 2"eu" 0
        assert_eq!(q[12], 7);
        assert_eq!(&q[13..20], b"euro-os");
        assert_eq!(q[20], 2);
        assert_eq!(&q[21..23], b"eu");
        assert_eq!(q[23], 0);
        assert_eq!(&q[24..28], &[0, 1, 0, 1]); // A / IN
    }

    #[test]
    fn query_name_roundtrip() {
        let q = build_query(1, "ads.doubleclick.net");
        assert_eq!(parse_query_name(&q).as_deref(), Some("ads.doubleclick.net"));
        // A too-short buffer returns None.
        assert_eq!(parse_query_name(&[0u8; 4]), None);
    }

    #[test]
    fn parse_answer_with_compression() {
        // Answer to a query for "a.bc": header + question + 1 answer with
        // a compression pointer to the name + A-record 93.184.216.34.
        let mut b = Vec::new();
        b.extend_from_slice(&0x1234u16.to_be_bytes()); // id
        b.extend_from_slice(&0x8180u16.to_be_bytes()); // flags (response)
        b.extend_from_slice(&1u16.to_be_bytes()); // QD
        b.extend_from_slice(&1u16.to_be_bytes()); // AN
        b.extend_from_slice(&[0, 0, 0, 0]);
        // question: 1"a" 2"bc" 0 + A + IN
        b.extend_from_slice(&[1, b'a', 2, b'b', b'c', 0, 0, 1, 0, 1]);
        // answer: pointer to offset 12 (0xC00C), type A, class IN, ttl, rdlen 4, ip
        b.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34]);
        let ips = parse_a_records(&b);
        assert_eq!(ips, vec![Ipv4Addr([93, 184, 216, 34])]);
    }

    #[test]
    fn parse_response_valideert_transaction_id_en_qr() {
        let mut b = Vec::new();
        b.extend_from_slice(&0x1234u16.to_be_bytes()); // id
        b.extend_from_slice(&0x8180u16.to_be_bytes()); // flags: QR=1 (response)
        b.extend_from_slice(&1u16.to_be_bytes()); // QD
        b.extend_from_slice(&1u16.to_be_bytes()); // AN
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&[1, b'a', 2, b'b', b'c', 0, 0, 1, 0, 1]);
        b.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34]);
        // Correct id + response → A-record.
        assert_eq!(parse_response(&b, 0x1234), vec![Ipv4Addr([93, 184, 216, 34])]);
        // Wrong id (spoofed/mismatched) → empty.
        assert!(parse_response(&b, 0x9999).is_empty());
        // Same packet but QR=0 (a QUERY, not a response) → empty.
        let mut q = b.clone();
        q[2] = 0x00; // clear the QR bit
        assert!(parse_response(&q, 0x1234).is_empty());
    }
}
