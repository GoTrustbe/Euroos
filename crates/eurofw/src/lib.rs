//! EuroFW — een **packet-filter / firewall** (plan N3).
//!
//! Netwerk-soevereiniteit vraagt om controle over wat er in- en uitgaat. EuroFW is
//! een eenvoudige, snelle **5-tuple-regelmotor**: per pakket (richting, protocol,
//! bron/​bestemming-IP+CIDR, bron/​bestemming-poort) wordt de eerste passende regel
//! toegepast (`ACCEPT`/`DROP`); matcht niets, dan geldt het **standaardbeleid**.
//! Pure `no_std`-logica → host-getest, los van de NIC-driver. De kernel roept
//! [`Firewall::verdict`] aan in het RX/TX-pad.

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
    /// IP-protocolnummer → `Proto` (1=ICMP, 6=TCP, 17=UDP).
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

/// Een 5-tuple-beschrijving van een pakket (IPv4 als u32, host-volgorde).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    pub direction: Direction, // In of Out (nooit Both)
    pub proto: Proto,
    pub src: u32,
    pub dst: u32,
    pub src_port: u16,
    pub dst_port: u16,
}

/// Eén firewall-regel. `None`-velden = wildcard (matcht alles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub action: Action,
    pub direction: Direction,
    pub proto: Proto,
    /// (netwerk-IP, prefix-lengte) — CIDR. None = elk IP.
    pub src: Option<(u32, u8)>,
    pub dst: Option<(u32, u8)>,
    pub src_port: Option<u16>,
    pub dst_port: Option<u16>,
}

impl Rule {
    /// Een lege "elke-richting, elk-protocol"-regel met de gegeven actie — bouw 'm
    /// verder uit met de `.*`-helpers.
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

    /// Matcht deze regel het pakket?
    pub fn matches(&self, p: &Packet) -> bool {
        self.dir_matches(p.direction)
            && (self.proto == Proto::Any || self.proto == p.proto)
            && self.src.map(|(n, pre)| cidr_match(n, pre, p.src)).unwrap_or(true)
            && self.dst.map(|(n, pre)| cidr_match(n, pre, p.dst)).unwrap_or(true)
            && self.src_port.map(|x| x == p.src_port).unwrap_or(true)
            && self.dst_port.map(|x| x == p.dst_port).unwrap_or(true)
    }
}

/// Valt `ip` binnen het CIDR-blok `net/prefix`?
pub fn cidr_match(net: u32, prefix: u8, ip: u32) -> bool {
    if prefix == 0 {
        return true;
    }
    let mask = if prefix >= 32 { u32::MAX } else { !(u32::MAX >> prefix) };
    (ip & mask) == (net & mask)
}

/// De firewall: een geordende regellijst + een standaardbeleid.
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

    /// Bepaal de actie voor een pakket (eerste-match-wint; anders standaardbeleid).
    /// Werkt de teller bij — handig voor `firewall stats`.
    pub fn verdict(&mut self, p: &Packet) -> Action {
        let a = self.rules.iter().find(|r| r.matches(p)).map(|r| r.action).unwrap_or(self.default);
        match a {
            Action::Accept => self.accepted += 1,
            Action::Drop => self.dropped += 1,
        }
        a
    }

    /// Zoals `verdict` maar zonder de tellers te muteren (voor `simulate`/tests).
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
        assert!(cidr_match(0, 0, ip(8, 8, 8, 8))); // /0 = elk
    }

    #[test]
    fn default_deny_with_allowlist() {
        let mut fw = Firewall::new(Action::Drop); // default-deny
        // Sta uitgaande HTTPS toe + ingaande SSH vanaf het LAN.
        fw.push(Rule::new(Action::Accept).dir(Direction::Out).proto(Proto::Tcp).dst_port(443));
        fw.push(Rule::new(Action::Accept).dir(Direction::In).proto(Proto::Tcp).dst_port(22).src_cidr(ip(192, 168, 0, 0), 16));

        // Uitgaande HTTPS → toegestaan.
        assert_eq!(fw.peek(&Packet { direction: Direction::Out, proto: Proto::Tcp, src: ip(192,168,1,5), dst: ip(1,1,1,1), src_port: 50000, dst_port: 443 }), Action::Accept);
        // Uitgaande FTP → geweigerd (default-deny).
        assert_eq!(fw.peek(&Packet { direction: Direction::Out, proto: Proto::Tcp, src: ip(192,168,1,5), dst: ip(1,1,1,1), src_port: 50000, dst_port: 21 }), Action::Drop);
        // Ingaande SSH vanaf het LAN → toegestaan.
        assert_eq!(fw.peek(&Packet { direction: Direction::In, proto: Proto::Tcp, src: ip(192,168,1,9), dst: ip(192,168,1,5), src_port: 40000, dst_port: 22 }), Action::Accept);
        // Ingaande SSH van BUITEN het LAN → geweigerd.
        assert_eq!(fw.peek(&Packet { direction: Direction::In, proto: Proto::Tcp, src: ip(8,8,8,8), dst: ip(192,168,1,5), src_port: 40000, dst_port: 22 }), Action::Drop);
    }

    #[test]
    fn first_match_wins_and_counters() {
        let mut fw = Firewall::new(Action::Accept);
        fw.push(Rule::new(Action::Drop).proto(Proto::Icmp)); // blokkeer alle ping
        fw.push(Rule::new(Action::Accept).proto(Proto::Icmp)); // (nooit bereikt)
        let ping = Packet { direction: Direction::In, proto: Proto::Icmp, src: ip(8,8,8,8), dst: ip(10,0,0,1), src_port: 0, dst_port: 0 };
        assert_eq!(fw.verdict(&ping), Action::Drop);
        assert_eq!(fw.verdict(&ping), Action::Drop);
        assert_eq!(fw.dropped, 2);
        // Een TCP-pakket valt door naar default-accept.
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
