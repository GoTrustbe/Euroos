//! EuroNet — eigen, RFC-conforme netwerkstack (Track 4 van EuroKernel).
//!
//! Deze crate doet (Fase 1) puur **packet parsing/building + checksums**: geen
//! driver, geen sockets. Dat is bewust de meest fout-gevoelige laag van een OS
//! en is volledig op de host testbaar — zonder NIC of QEMU. De driver-/socket-
//! integratie (VirtIO, TCP-state machine) is een latere fase.
//!
//! Alles is **big-endian** (network byte order) via expliciete `from_be_bytes`/
//! `to_be_bytes` — nooit directe casts. `no_std` + `alloc`; tests draaien std.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod arp;
pub mod dhcp;
pub mod dns;
pub mod checksum;
pub mod ethernet;
pub mod icmp;
pub mod icmpv6;
pub mod ratelimit;
pub mod tcpcc;
pub mod ipv6;
pub mod ipv4;
pub mod tcp;
pub mod udp;
pub mod unix;

pub use arp::{ArpOp, ArpPacket};
pub use ethernet::{EtherType, EthernetHeader, MacAddr};
pub use icmp::{IcmpEcho, IcmpError, IcmpType, UnreachCode};
pub use ipv4::{Ipv4Addr, Ipv4Header, Protocol};
pub use udp::UdpDatagram;

/// Gedeelde parse-fout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    TooShort,
    BadChecksum,
    Malformed,
    Unsupported,
}

pub type NetResult<T> = Result<T, NetError>;
