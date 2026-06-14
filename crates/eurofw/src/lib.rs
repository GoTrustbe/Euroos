//! EuroFW — a **packet-filter / firewall** (plan N3).
//!
//! Network sovereignty requires control over what comes in and goes out. EuroFW is
//! a simple, fast **5-tuple rule engine**: per packet (direction, protocol,
//! source/​destination IP+CIDR, source/​destination port) the first matching rule
//! is applied (`ACCEPT`/`DROP`); if nothing matches, the **default policy** applies.
//! Pure `no_std` logic → host-tested, independent of the NIC driver. The kernel calls
//! [`Firewall::verdict`] in the RX/TX path.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Accept,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Any,
    Icmp,
    Tcp,
    Udp,
}

impl Proto {
    /// IP protocol number → `Proto` (1=ICMP, 6=TCP, 17=UDP).
    pub fn from_ip(n: u8) -> Proto {
        match n {
            1 => Proto::Icmp,
            6 => Proto::Tcp,
            17 => Proto::Udp,
            _ => Proto::Any,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    Both,
}

/// A 5-tuple description of a packet (IPv4 as u32, host order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    pub direction: Direction, // In or Out (never Both)
    pub proto: Proto,
    pub src: u32,
    pub dst: u32,
    pub src_port: u16,
    pub dst_port: u16,
}

/// A single firewall rule. `None` fields = wildcard (matches everything).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub action: Action,
    pub direction: Direction,
    pub proto: Proto,
    /// (network IP, prefix length) — CIDR. None = any IP.
    pub src: Option<(u32, u8)>,
    pub dst: Option<(u32, u8)>,
    pub src_port: Option<u16>,
    pub dst_port: Option<u16>,
}

impl Rule {
    /// An empty "any-direction, any-protocol" rule with the given action — build it
    /// up further with the `.*` helpers.
    pub fn new(action: Action) -> Rule {
        Rule {
            action,
            direction: Direction::Both,
            proto: Proto::Any,
            src: None,
            dst: None,
            src_port: None,
            dst_port: None,
        }
    }
    pub fn dir(mut self, d: Direction) -> Self {
        self.direction = d;
        self
    }
    pub fn proto(mut self, p: Proto) -> Self {
        self.proto = p;
        self
    }
    pub fn src_cidr(mut self, net: u32, prefix: u8) -> Self {
        self.src = Some((net, prefix));
        self
    }
    pub fn dst_cidr(mut self, net: u32, prefix: u8) -> Self {
        self.dst = Some((net, prefix));
        self
    }
    pub fn dst_port(mut self, p: u16) -> Self {
        self.dst_port = Some(p);
        self
    }
    pub fn src_port(mut self, p: u16) -> Self {
        self.src_port = Some(p);
        self
    }

    fn dir_matches(&self, d: Direction) -> bool {
        self.direction == Direction::Both || self.direction == d
    }

    /// Does this rule match the packet?
    pub fn matches(&self, p: &Packet) -> bool {
        self.dir_matches(p.direction)
            && (self.proto == Proto::Any || self.proto == p.proto)
            && self.src.map(|(n, pre)| cidr_match(n, pre, p.src)).unwrap_or(true)
            && self.dst.map(|(n, pre)| cidr_match(n, pre, p.dst)).unwrap_or(true)
            && self.src_port.map(|x| x == p.src_port).unwrap_or(true)
            && self.dst_port.map(|x| x == p.dst_port).unwrap_or(true)
    }
}

/// Does `ip` fall within the CIDR block `net/prefix`?
pub fn cidr_match(net: u32, prefix: u8, ip: u32) -> bool {
    if prefix == 0 {
        return true;
    }
    let mask = if prefix >= 32 { u32::MAX } else { !(u32::MAX >> prefix) };
    (ip & mask) == (net & mask)
}

/// The firewall: an ordered rule list + a default policy.
pub struct Firewall {
    rules: Vec<Rule>,
    default: Action,
    pub accepted: u64,
    pub dropped: u64,
}

impl Firewall {
    pub fn new(default: Action) -> Firewall {
        Firewall { rules: Vec::new(), default, accepted: 0, dropped: 0 }
    }

    pub fn push(&mut self, rule: Rule) {
        self.rules.push(rule);
    }
    pub fn set_default(&mut self, a: Action) {
        self.default = a;
    }
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
    pub fn clear(&mut self) {
        self.rules.clear();
    }

    /// Determine the action for a packet (first-match-wins; otherwise default policy).
    /// Updates the counter — handy for `firewall stats`.
    pub fn verdict(&mut self, p: &Packet) -> Action {
        let a = self.rules.iter().find(|r| r.matches(p)).map(|r| r.action).unwrap_or(self.default);
        match a {
            Action::Accept => self.accepted += 1,
            Action::Drop => self.dropped += 1,
        }
        a
    }

    /// Like `verdict` but without mutating the counters (for `simulate`/tests).
    pub fn peek(&self, p: &Packet) -> Action {
        self.rules.iter().find(|r| r.matches(p)).map(|r| r.action).unwrap_or(self.default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> u32 {
        u32::from_be_bytes([a, b, c, d])
    }

    #[test]
    fn cidr() {
        assert!(cidr_match(ip(10, 0, 0, 0), 8, ip(10, 1, 2, 3)));
        assert!(!cidr_match(ip(10, 0, 0, 0), 8, ip(11, 1, 2, 3)));
        assert!(cidr_match(ip(192, 168, 1, 0), 24, ip(192, 168, 1, 50)));
        assert!(!cidr_match(ip(192, 168, 1, 0), 24, ip(192, 168, 2, 50)));
        assert!(cidr_match(0, 0, ip(8, 8, 8, 8))); // /0 = any
    }

    #[test]
    fn default_deny_with_allowlist() {
        let mut fw = Firewall::new(Action::Drop); // default-deny
        // Allow outbound HTTPS + inbound SSH from the LAN.
        fw.push(Rule::new(Action::Accept).dir(Direction::Out).proto(Proto::Tcp).dst_port(443));
        fw.push(Rule::new(Action::Accept).dir(Direction::In).proto(Proto::Tcp).dst_port(22).src_cidr(ip(192, 168, 0, 0), 16));

        // Outbound HTTPS → allowed.
        assert_eq!(fw.peek(&Packet { direction: Direction::Out, proto: Proto::Tcp, src: ip(192,168,1,5), dst: ip(1,1,1,1), src_port: 50000, dst_port: 443 }), Action::Accept);
        // Outbound FTP → denied (default-deny).
        assert_eq!(fw.peek(&Packet { direction: Direction::Out, proto: Proto::Tcp, src: ip(192,168,1,5), dst: ip(1,1,1,1), src_port: 50000, dst_port: 21 }), Action::Drop);
        // Inbound SSH from the LAN → allowed.
        assert_eq!(fw.peek(&Packet { direction: Direction::In, proto: Proto::Tcp, src: ip(192,168,1,9), dst: ip(192,168,1,5), src_port: 40000, dst_port: 22 }), Action::Accept);
        // Inbound SSH from OUTSIDE the LAN → denied.
        assert_eq!(fw.peek(&Packet { direction: Direction::In, proto: Proto::Tcp, src: ip(8,8,8,8), dst: ip(192,168,1,5), src_port: 40000, dst_port: 22 }), Action::Drop);
    }

    #[test]
    fn first_match_wins_and_counters() {
        let mut fw = Firewall::new(Action::Accept);
        fw.push(Rule::new(Action::Drop).proto(Proto::Icmp)); // block all ping
        fw.push(Rule::new(Action::Accept).proto(Proto::Icmp)); // (never reached)
        let ping = Packet { direction: Direction::In, proto: Proto::Icmp, src: ip(8,8,8,8), dst: ip(10,0,0,1), src_port: 0, dst_port: 0 };
        assert_eq!(fw.verdict(&ping), Action::Drop);
        assert_eq!(fw.verdict(&ping), Action::Drop);
        assert_eq!(fw.dropped, 2);
        // A TCP packet falls through to default-accept.
        let tcp = Packet { direction: Direction::Out, proto: Proto::Tcp, src: ip(10,0,0,1), dst: ip(1,1,1,1), src_port: 1234, dst_port: 80 };
        assert_eq!(fw.verdict(&tcp), Action::Accept);
        assert_eq!(fw.accepted, 1);
    }

    #[test]
    fn proto_from_ip() {
        assert_eq!(Proto::from_ip(1), Proto::Icmp);
        assert_eq!(Proto::from_ip(6), Proto::Tcp);
        assert_eq!(Proto::from_ip(17), Proto::Udp);
        assert_eq!(Proto::from_ip(99), Proto::Any);
    }
}
