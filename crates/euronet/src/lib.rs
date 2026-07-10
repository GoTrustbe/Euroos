//! EuroNet — in-house, RFC-compliant network stack (Track 4 of EuroKernel).
//!
//! This crate does (Phase 1) purely **packet parsing/building + checksums**: no
//! driver, no sockets. That is deliberately the most error-prone layer of an OS
//! and is fully testable on the host — without a NIC or QEMU. The driver/socket
//! integration (VirtIO, TCP state machine) is a later phase.
//!
//! Everything is **big-endian** (network byte order) via explicit `from_be_bytes`/
//! `to_be_bytes` — never direct casts. `no_std` + `alloc`; tests run on std.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod arp;
pub mod dhcp;
pub mod dhcpv6;
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

/// Shared parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    TooShort,
    BadChecksum,
    Malformed,
    Unsupported,
}

pub type NetResult<T> = Result<T, NetError>;
