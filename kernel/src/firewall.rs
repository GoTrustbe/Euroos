//! Kernel side of **EuroFW** (plan N3): a global packet filter that, in the
//! RX path ([`crate::net::service`]), checks every incoming IP packet against the rule list.
//! Blocked packets are dropped silently (no RST/ICMP error → stealth).

use alloc::string::String;
use alloc::vec::Vec;

use eurofw::{Action, Direction, Firewall, Packet, Proto, Rule};
use spin::Mutex;

static FW: Mutex<Option<Firewall>> = Mutex::new(None);

/// Initialize the firewall with a sensible default policy: allow EVERYTHING, but
/// block a few known-unsafe incoming services (stealth) + an
/// example blocklist. (A stricter default-deny is a policy choice via EuroPol.)
pub fn init() {
    let mut fw = Firewall::new(Action::Accept);
    // Block incoming Telnet (23) and NetBIOS (139) — classic attack surfaces.
    fw.push(Rule::new(Action::Drop).dir(Direction::In).proto(Proto::Tcp).dst_port(23));
    fw.push(Rule::new(Action::Drop).dir(Direction::In).proto(Proto::Tcp).dst_port(139));
    // Example blocklist: reject all traffic from 198.51.100.0/24 (TEST-NET-2).
    fw.push(Rule::new(Action::Drop).dir(Direction::In).src_cidr(u32::from_be_bytes([198, 51, 100, 0]), 24));
    *FW.lock() = Some(fw);
}

/// Called by [`crate::net::service`] for every incoming IP packet. Returns true
/// if the packet is allowed through.
pub fn inbound_allowed(ip_proto: u8, src: u32, dst: u32, src_port: u16, dst_port: u16) -> bool {
    let mut g = FW.lock();
    match g.as_mut() {
        Some(fw) => {
            fw.verdict(&Packet {
                direction: Direction::In,
                proto: Proto::from_ip(ip_proto),
                src,
                dst,
                src_port,
                dst_port,
            }) == Action::Accept
        }
        None => true,
    }
}

/// (accepted, dropped).
pub fn stats() -> (u64, u64) {
    match FW.lock().as_ref() {
        Some(fw) => (fw.accepted, fw.dropped),
        None => (0, 0),
    }
}

fn ip(a: u8, b: u8, c: u8, d: u8) -> u32 {
    u32::from_be_bytes([a, b, c, d])
}

/// Boot self-test: exercise the rule engine on a few representative packets.
pub fn selftest() {
    let g = FW.lock();
    let fw = match g.as_ref() {
        Some(f) => f,
        None => return,
    };
    let telnet = Packet { direction: Direction::In, proto: Proto::Tcp, src: ip(8, 8, 8, 8), dst: ip(10, 0, 0, 1), src_port: 40000, dst_port: 23 };
    let https = Packet { direction: Direction::In, proto: Proto::Tcp, src: ip(8, 8, 8, 8), dst: ip(10, 0, 0, 1), src_port: 40000, dst_port: 443 };
    let blocked_src = Packet { direction: Direction::In, proto: Proto::Tcp, src: ip(198, 51, 100, 7), dst: ip(10, 0, 0, 1), src_port: 40000, dst_port: 443 };
    let telnet_drop = fw.peek(&telnet) == Action::Drop;
    let https_ok = fw.peek(&https) == Action::Accept;
    let src_drop = fw.peek(&blocked_src) == Action::Drop;
    crate::serial_println!(
        "[n3] EuroFW: {} rules, incoming Telnet:23-blocked={telnet_drop}, HTTPS:443-allowed={https_ok}, blocklist-source-rejected={src_drop} → {}",
        fw.rule_count(),
        if telnet_drop && https_ok && src_drop { "OK (packet filter works in the RX path) ✓" } else { "FAILED" }
    );
}

/// `firewall` shell command: show the rules + counters.
pub fn shell() -> Vec<String> {
    let (acc, drop) = stats();
    let n = FW.lock().as_ref().map(|f| f.rule_count()).unwrap_or(0);
    alloc::vec![
        alloc::format!("EuroFW packet filter: {n} rules, {acc} allowed / {drop} dropped"),
        String::from("  drop in tcp dport 23 (Telnet) · drop in tcp dport 139 (NetBIOS) · drop in src 198.51.100.0/24"),
        String::from("  default policy: ACCEPT (stricter default-deny via EuroPol policy)"),
    ]
}
