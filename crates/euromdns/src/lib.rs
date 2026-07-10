//! EuroMDNS — **multicast DNS** (RFC 6762) + **DNS-SD** (RFC 6763).
//!
//! "Plug it in and it appears" without any central server: a host answers
//! queries for its own `.local` name on the mDNS multicast group
//! (224.0.0.251 / ff02::fb, port 5353), and advertises services (printers, file
//! shares) so peers discover them. This is the sovereign, zero-config
//! complement to the DNS resolver — no cloud, no coordinator.
//!
//! Pure `no_std` message logic (reuses [`eurodns`] wire encoding); the kernel
//! drives it over UDP multicast.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use eurodns::{encode_name, CLASS_IN, TYPE_A, TYPE_AAAA};

pub const MDNS_PORT: u16 = 5353;
/// IPv4 mDNS multicast group.
pub const MDNS_V4: [u8; 4] = [224, 0, 0, 251];
/// IPv6 mDNS multicast group (ff02::fb).
pub const MDNS_V6: [u8; 16] = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfb];

pub const TYPE_PTR: u16 = 12;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_SRV: u16 = 33;
/// The cache-flush bit OR'd into the class of a unique authoritative answer.
const CACHE_FLUSH: u16 = 0x8000;

/// Is this an mDNS (`.local`) name?
pub fn is_local(name: &str) -> bool {
    let n = name.trim_end_matches('.');
    n.eq_ignore_ascii_case("local") || n.to_ascii_lowercase().ends_with(".local")
}

/// Build an mDNS query for `name`/`qtype` (id 0, standard query — RFC 6762).
pub fn build_query(name: &str, qtype: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0u16.to_be_bytes()); // id = 0
    b.extend_from_slice(&0u16.to_be_bytes()); // flags: standard query
    b.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    b.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    b.extend_from_slice(&encode_name(name));
    b.extend_from_slice(&qtype.to_be_bytes());
    b.extend_from_slice(&CLASS_IN.to_be_bytes());
    b
}

fn header(answers: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0u16.to_be_bytes()); // id 0
    b.extend_from_slice(&0x8400u16.to_be_bytes()); // QR=1, AA=1
    b.extend_from_slice(&0u16.to_be_bytes()); // qdcount 0 (responses omit the question)
    b.extend_from_slice(&answers.to_be_bytes()); // ancount
    b.extend_from_slice(&[0, 0, 0, 0]); // ns/ar
    b
}

fn rr(out: &mut Vec<u8>, name: &str, rtype: u16, ttl: u32, rdata: &[u8]) {
    out.extend_from_slice(&encode_name(name));
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&(CLASS_IN | CACHE_FLUSH).to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
}

/// A service instance to advertise via DNS-SD.
pub struct Service<'a> {
    /// e.g. `_ipp._tcp.local` (a printer).
    pub service_type: &'a str,
    /// e.g. `EuroPrint._ipp._tcp.local`.
    pub instance: &'a str,
    /// The host offering it, e.g. `euro-host.local`.
    pub host: &'a str,
    pub port: u16,
    /// TXT key=value pairs.
    pub txt: &'a [&'a str],
}

/// A host that answers mDNS queries for its own name + advertised services.
pub struct Responder<'a> {
    pub hostname: &'a str, // e.g. "euro-host.local"
    pub ipv4: Option<[u8; 4]>,
    pub ipv6: Option<[u8; 16]>,
    pub service: Option<Service<'a>>,
}

impl Responder<'_> {
    /// Build an unsolicited A/AAAA announcement for this host.
    pub fn announce_host(&self) -> Vec<u8> {
        let mut answers: Vec<u8> = Vec::new();
        let mut n = 0u16;
        if let Some(ip) = self.ipv4 {
            rr(&mut answers, self.hostname, TYPE_A, 120, &ip);
            n += 1;
        }
        if let Some(ip) = self.ipv6 {
            rr(&mut answers, self.hostname, TYPE_AAAA, 120, &ip);
            n += 1;
        }
        let mut out = header(n);
        out.extend_from_slice(&answers);
        out
    }

    /// Answer an mDNS query. Returns a response packet if it matches this host's
    /// name (A/AAAA) or advertised service (PTR/SRV/TXT); `None` otherwise, so we
    /// stay quiet for queries that are not ours (RFC 6762 politeness).
    pub fn respond_to(&self, qname: &str, qtype: u16) -> Option<Vec<u8>> {
        let ours = qname.eq_ignore_ascii_case(self.hostname);
        if ours && (qtype == TYPE_A || qtype == 255) {
            if let Some(ip) = self.ipv4 {
                let mut a = Vec::new();
                rr(&mut a, self.hostname, TYPE_A, 120, &ip);
                let mut out = header(1);
                out.extend_from_slice(&a);
                return Some(out);
            }
        }
        if ours && qtype == TYPE_AAAA {
            if let Some(ip) = self.ipv6 {
                let mut a = Vec::new();
                rr(&mut a, self.hostname, TYPE_AAAA, 120, &ip);
                let mut out = header(1);
                out.extend_from_slice(&a);
                return Some(out);
            }
        }
        // DNS-SD: a PTR query for our service type → instance + SRV + TXT.
        if let Some(svc) = &self.service {
            if qtype == TYPE_PTR && qname.eq_ignore_ascii_case(svc.service_type) {
                let mut a = Vec::new();
                rr(&mut a, svc.service_type, TYPE_PTR, 120, &encode_name(svc.instance));
                // SRV: priority(2) weight(2) port(2) target-name.
                let mut srv = alloc::vec![0u8, 0, 0, 0];
                srv.extend_from_slice(&svc.port.to_be_bytes());
                srv.extend_from_slice(&encode_name(svc.host));
                rr(&mut a, svc.instance, TYPE_SRV, 120, &srv);
                // TXT: each entry length-prefixed.
                let mut txt = Vec::new();
                for kv in svc.txt {
                    txt.push(kv.len() as u8);
                    txt.extend_from_slice(kv.as_bytes());
                }
                if txt.is_empty() {
                    txt.push(0);
                }
                rr(&mut a, svc.instance, TYPE_TXT, 120, &txt);
                let mut out = header(3);
                out.extend_from_slice(&a);
                return Some(out);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Responder<'static> {
        Responder {
            hostname: "euro-host.local",
            ipv4: Some([192, 168, 1, 42]),
            ipv6: None,
            service: Some(Service {
                service_type: "_ipp._tcp.local",
                instance: "EuroPrint._ipp._tcp.local",
                host: "euro-host.local",
                port: 631,
                txt: &["rp=printers/euro", "ty=EuroPrint"],
            }),
        }
    }

    #[test]
    fn local_detection() {
        assert!(is_local("euro-host.local"));
        assert!(is_local("printer.LOCAL."));
        assert!(!is_local("euro-os.eu"));
    }

    #[test]
    fn query_has_qname_and_type() {
        let q = build_query("euro-host.local", TYPE_A);
        assert_eq!(u16::from_be_bytes([q[4], q[5]]), 1); // qdcount
        assert_eq!(&q[q.len() - 4..], &[0, TYPE_A as u8, 0, CLASS_IN as u8]);
    }

    #[test]
    fn answers_own_name_only() {
        let h = host();
        // A query for our name is answered.
        let r = h.respond_to("euro-host.local", TYPE_A).unwrap();
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 1); // ancount = 1
        assert!(r.windows(4).any(|w| w == [192, 168, 1, 42])); // our IP is in the answer
        // A query for someone else's name → silence.
        assert!(h.respond_to("other.local", TYPE_A).is_none());
    }

    #[test]
    fn service_discovery_ptr() {
        let h = host();
        // A DNS-SD PTR query for our service type returns PTR+SRV+TXT.
        let r = h.respond_to("_ipp._tcp.local", TYPE_PTR).unwrap();
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 3); // PTR + SRV + TXT
        // The port 631 is present in the SRV rdata, and the TXT key is carried.
        assert!(r.windows(2).any(|w| w == 631u16.to_be_bytes()));
        assert!(r.windows(8).any(|w| w == b"ty=EuroP"));
        // An unrelated service type is ignored.
        assert!(h.respond_to("_http._tcp.local", TYPE_PTR).is_none());
    }
}
