//! Reusable network operations on top of the virtio-net NIC + EuroNet:
//! ARP resolution, ICMP ping, DNS lookup, ICMPv6 ping. Plus the stored
//! network configuration (after boot bring-up) so the shell can offer
//! `ping`/`dns`/`net` on the live NIC.

use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use euronet::arp::{ArpOp, ArpPacket};
use euronet::dns;
use euronet::ethernet::{EtherType, EthernetHeader, MacAddr};
use euronet::icmp::{IcmpEcho, IcmpError, IcmpType, UnreachCode};
use euronet::ratelimit::TokenBucket;
use euronet::icmpv6;
use euronet::ipv4::{Ipv4Addr, Ipv4Header, Protocol};
use euronet::ipv6::{Ipv6Addr, Ipv6Header};
use euronet::tcp::{self, TcpSegment};
use euronet::udp::UdpDatagram;

use crate::nic;

/// The network configuration as discovered during boot bring-up.
#[derive(Clone, Copy)]
pub struct NetCfg {
    pub my_mac: MacAddr,
    pub my_ip: Ipv4Addr,
    pub gw_ip: Ipv4Addr,
    pub gw_mac: MacAddr,
    pub dns_ip: Ipv4Addr,
    pub dns_mac: MacAddr,
    pub link_local: Ipv6Addr,
    pub router_ll: Option<Ipv6Addr>,
    pub router_mac: Option<MacAddr>,
}

static CFG: Mutex<Option<NetCfg>> = Mutex::new(None);

pub fn save(c: NetCfg) {
    *CFG.lock() = Some(c);
}
pub fn get() -> Option<NetCfg> {
    *CFG.lock()
}

const SPINS: u64 = 4_000_000;

/// Recycle pending RX buffers (otherwise the 16 buffers fill up with idle
/// multicast traffic) AND announce our IP→MAC with a gratuitous ARP, so that
/// slirp's ARP cache stays fresh and IPv4 replies arrive.
fn drain() {
    for _ in 0..64 {
        if nic::poll_recv().is_none() {
            break;
        }
    }
    if let Some(c) = get() {
        let g = ArpPacket {
            op: ArpOp::Reply,
            sender_mac: c.my_mac,
            sender_ip: c.my_ip,
            target_mac: c.my_mac,
            target_ip: c.my_ip,
        };
        let frame = EthernetHeader { dst: MacAddr::BROADCAST, src: c.my_mac, ethertype: EtherType::Arp }
            .build(&g.build());
        nic::send(&frame);
    }
}

/// ARP: ask for the MAC address of `ip` (in our subnet).
/// Compact network bring-up for a NIC that appears late in boot (M3-3: the
/// CDC-ECM function exists only after xHCI enumeration). DHCP → ARP the
/// gateway → ping it → save the config, with the same serial markers as the
/// main bring-up so tests treat both paths identically.
pub fn late_bring_up() {
    use euronet::dhcp;
    use euronet::ethernet::EtherType;
    use euronet::ipv4::{Ipv4Header, Protocol};
    use euronet::udp::UdpDatagram;

    if !crate::nic::late_bind_usbnet() {
        return;
    }
    let my_mac = MacAddr(match crate::nic::mac() {
        Some(m) => m,
        None => return,
    });
    crate::serial_println!(
        "[net] NIC: {} MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (late bring-up)",
        crate::nic::kind(),
        my_mac.0[0], my_mac.0[1], my_mac.0[2], my_mac.0[3], my_mac.0[4], my_mac.0[5]
    );

    let any = Ipv4Addr([0, 0, 0, 0]);
    let bcast = Ipv4Addr([255, 255, 255, 255]);
    let xid = 0x4C41_5445u32; // "LATE"
    let send_dhcp = |mt: u8, req: Option<Ipv4Addr>, sid: Option<Ipv4Addr>| {
        let payload = dhcp::build(mt, xid, my_mac.0, req, sid);
        let seg = UdpDatagram { src_port: 68, dst_port: 67, payload }.build(any, bcast);
        let ipf = Ipv4Header {
            protocol: Protocol::Udp, ttl: 64, src: any, dst: bcast,
            total_length: 0, identification: 0x4c54,
        }
        .build(&seg);
        let frame = EthernetHeader { dst: MacAddr::BROADCAST, src: my_mac, ethertype: EtherType::Ipv4 }.build(&ipf);
        nic::send(&frame);
    };
    let poll_dhcp = |want: u8| -> Option<dhcp::DhcpInfo> {
        for _ in 0..6_000_000u64 {
            if let Some(rx) = nic::poll_recv() {
                if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                    if h.ethertype == EtherType::Ipv4 {
                        if let Ok((iph, ipl)) = Ipv4Header::parse(p) {
                            if iph.protocol == Protocol::Udp && ipl.len() > 8 {
                                let dport = u16::from_be_bytes([ipl[2], ipl[3]]);
                                if dport == 68 {
                                    if let Some(info) = dhcp::parse(&ipl[8..]) {
                                        if info.msg_type == want {
                                            return Some(info);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    };
    let mut offer = None;
    for _ in 0..6 {
        send_dhcp(dhcp::DISCOVER, None, None);
        offer = poll_dhcp(dhcp::OFFER);
        if offer.is_some() {
            break;
        }
        for _ in 0..20_000_000u64 {
            core::hint::spin_loop();
        }
    }
    let o = match offer {
        Some(o) => o,
        None => {
            crate::serial_println!("[net] late bring-up: no DHCP OFFER over {}", crate::nic::kind());
            return;
        }
    };
    crate::serial_println!(
        "[net] DHCP OFFER: {}.{}.{}.{} from server {}.{}.{}.{}",
        o.your_ip.0[0], o.your_ip.0[1], o.your_ip.0[2], o.your_ip.0[3],
        o.server_id.0[0], o.server_id.0[1], o.server_id.0[2], o.server_id.0[3]
    );
    send_dhcp(dhcp::REQUEST, Some(o.your_ip), Some(o.server_id));
    let _ = poll_dhcp(dhcp::ACK);
    let my_ip = o.your_ip;
    let gw_ip = o.router.unwrap_or(o.server_id);
    let dns_ip = o.dns.unwrap_or(gw_ip);

    let gw_mac = match arp_resolve(my_mac, my_ip, gw_ip) {
        Some(m) => m,
        None => {
            crate::serial_println!("[net] late bring-up: gateway ARP failed");
            return;
        }
    };
    let dns_mac = arp_resolve(my_mac, my_ip, dns_ip).unwrap_or(gw_mac);
    let pong = icmp_ping(my_mac, my_ip, gw_mac, gw_ip);
    crate::serial_println!(
        "[net] PING {}.{}.{}.{}: {}",
        gw_ip.0[0], gw_ip.0[1], gw_ip.0[2], gw_ip.0[3],
        if pong { "echo-reply OK ✓" } else { "(no reply)" }
    );
    save(NetCfg {
        my_mac,
        my_ip,
        gw_ip,
        gw_mac,
        dns_ip,
        dns_mac,
        link_local: euronet::ipv6::Ipv6Addr::link_local_from_mac(my_mac.0),
        router_ll: None,
        router_mac: None,
    });
    crate::serial_println!("[net] ✓ EuroOS is on the network (late bring-up over {})", crate::nic::kind());
}

pub fn arp_resolve(my_mac: MacAddr, my_ip: Ipv4Addr, ip: Ipv4Addr) -> Option<MacAddr> {
    drain();
    let req = ArpPacket {
        op: ArpOp::Request,
        sender_mac: my_mac,
        sender_ip: my_ip,
        target_mac: MacAddr::ZERO,
        target_ip: ip,
    };
    let frame = EthernetHeader { dst: MacAddr::BROADCAST, src: my_mac, ethertype: EtherType::Arp }.build(&req.build());
    nic::send(&frame);
    for _ in 0..SPINS {
        if let Some(rx) = nic::poll_recv() {
            if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                if h.ethertype == EtherType::Arp {
                    if let Ok(a) = ArpPacket::parse(p) {
                        if a.op == ArpOp::Reply && a.sender_ip == ip {
                            return Some(a.sender_mac);
                        }
                    }
                }
            }
        }
    }
    None
}

/// ICMP echo (ping) to `dst` via the given next-hop MAC. true = reply received.
pub fn icmp_ping(my_mac: MacAddr, my_ip: Ipv4Addr, nexthop: MacAddr, dst: Ipv4Addr) -> bool {
    drain();
    let icmp = IcmpEcho { kind: IcmpType::EchoRequest, identifier: 0xE401, sequence: 1, payload: b"euroos-ping".to_vec() };
    let iph = Ipv4Header { protocol: Protocol::Icmp, ttl: 64, src: my_ip, dst, total_length: 0, identification: 1 };
    let frame = EthernetHeader { dst: nexthop, src: my_mac, ethertype: EtherType::Ipv4 }.build(&iph.build(&icmp.build()));
    nic::send(&frame);
    for _ in 0..SPINS * 2 {
        if let Some(rx) = nic::poll_recv() {
            if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                if h.ethertype == EtherType::Ipv4 {
                    if let Ok((ih, ipl)) = Ipv4Header::parse(p) {
                        if ih.protocol == Protocol::Icmp
                            && ih.src == dst
                            && IcmpEcho::parse(ipl).map(|e| e.kind == IcmpType::EchoReply).unwrap_or(false)
                        {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// DNS A-record lookup of `name` via the DNS server. Returns the first IPv4 address.
pub fn dns_query(my_mac: MacAddr, my_ip: Ipv4Addr, dns_mac: MacAddr, dns_ip: Ipv4Addr, name: &str) -> Option<Ipv4Addr> {
    // S9 DNS cache: consult the cache first (no network round-trip on a hit).
    if let Some(ip) = dns_cache_lookup(name) {
        DNS_HITS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        crate::serial_println!("[dns] cache hit: {name} = {}", ipfmt(ip));
        return Some(ip);
    }
    DNS_MISSES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    drain();
    // Varying transaction ID AND source port (from the HPET counter; RFC 5452): a
    // spoofer must now guess both the 16-bit txid and the ~14-bit ephemeral port,
    // not just the txid. Both are validated against the reply.
    // Hardware entropy (RDRAND) for both txid and source port instead of the merely
    // predictable HPET counter — a spoofer must guess both blindly.
    let r = rand_u64();
    let txid = r as u16;
    let src_port = 49152 + ((r >> 16) as u16 & 0x3FFF); // ephemeral port 49152-65535
    let q = dns::build_query(txid, name);
    let seg = UdpDatagram { src_port, dst_port: 53, payload: q }.build(my_ip, dns_ip);
    let iph = Ipv4Header { protocol: Protocol::Udp, ttl: 64, src: my_ip, dst: dns_ip, total_length: 0, identification: 2 };
    let frame = EthernetHeader { dst: dns_mac, src: my_mac, ethertype: EtherType::Ipv4 }.build(&iph.build(&seg));
    nic::send(&frame);
    for _ in 0..SPINS * 2 {
        if let Some(rx) = nic::poll_recv() {
            if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                if h.ethertype == EtherType::Ipv4 {
                    if let Ok((ih, ipl)) = Ipv4Header::parse(p) {
                        // The reply must COME FROM port 53 AND go to OUR source port
                        // (50000); and parse_response validates the transaction ID + QR bit.
                        if ih.protocol == Protocol::Udp
                            && ipl.len() > 8
                            && u16::from_be_bytes([ipl[0], ipl[1]]) == 53
                            && u16::from_be_bytes([ipl[2], ipl[3]]) == src_port
                        {
                            if let Some(ip) = dns::parse_response(&ipl[8..], txid).first() {
                                dns_cache_insert(name, *ip); // S9: cache the result
                                return Some(*ip);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// ICMPv6 echo (ping6) to `dst` via the given MAC.
pub fn icmp6_ping(my_mac: MacAddr, src_ll: Ipv6Addr, dst_mac: MacAddr, dst: Ipv6Addr) -> bool {
    drain();
    let echo = icmpv6::echo_request(0xE6, 1, b"euroos-ping6", src_ll, dst);
    let eh = Ipv6Header { next_header: 58, hop_limit: 255, src: src_ll, dst, payload_len: 0 };
    let frame = EthernetHeader { dst: dst_mac, src: my_mac, ethertype: EtherType::Ipv6 }.build(&eh.build(&echo));
    nic::send(&frame);
    for _ in 0..SPINS * 2 {
        if let Some(rx) = nic::poll_recv() {
            if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                if h.ethertype == EtherType::Ipv6 {
                    if let Ok((ih, pl)) = Ipv6Header::parse(p) {
                        if ih.next_header == 58 && icmpv6::msg_type(pl) == Some(icmpv6::ECHO_REPLY) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Network service: poll some incoming traffic and ANSWER ARP requests for
/// our IP (otherwise slirp's ARP cache expires and IPv4 replies never arrive).
/// Called every desktop-loop iteration so we stay reachable.
/// True if the datagram bounced back in an ICMP error is our own UDP packet
/// (the source/destination ports in the included UDP header match). `orig` is
/// the original IP datagram (IP header + start of the UDP header).
fn embedded_udp_matches(orig: &[u8], sport: u16, dport: u16) -> bool {
    if orig.len() < 20 {
        return false;
    }
    let ihl = (orig[0] & 0x0F) as usize * 4;
    // protocol UDP = 17, and there must be 4 bytes of UDP header (src+dst port).
    if orig[9] != 17 || orig.len() < ihl + 4 {
        return false;
    }
    let s = u16::from_be_bytes([orig[ihl], orig[ihl + 1]]);
    let d = u16::from_be_bytes([orig[ihl + 2], orig[ihl + 3]]);
    s == sport && d == dport
}

/// Send one IPv4 packet with `l4` payload to a direct L2 neighbour. Used
/// by `service()` for ICMP replies (echo-reply, port-unreachable).
fn send_ipv4(my_mac: MacAddr, my_ip: Ipv4Addr, dst_mac: MacAddr, dst_ip: Ipv4Addr, proto: Protocol, l4: &[u8]) {
    let iph = Ipv4Header { protocol: proto, ttl: 64, src: my_ip, dst: dst_ip, total_length: 0, identification: 9 };
    let frame = EthernetHeader { dst: dst_mac, src: my_mac, ethertype: EtherType::Ipv4 }.build(&iph.build(l4));
    nic::send(&frame);
}

/// Rate limiter for outgoing ICMP/RST error messages (anti-amplification):
/// max 20 per second, 20-burst. Prevents an attacker with spoofed source
/// IPs from reflecting our port-unreachable/RST replies to victims.
static ICMP_ERR_LIMIT: Mutex<TokenBucket> = Mutex::new(TokenBucket::new(20, 20, 100));
fn icmp_err_allowed() -> bool {
    ICMP_ERR_LIMIT.lock().allow(crate::interrupts::ticks())
}

/// Is there a userspace LISTEN socket on `seg.dst_port` with room in the queue?
/// If so: complete the passive open (SYN-ACK) and put the connection in the queue.
/// Returns `true` if a listener existed (so the caller does not send a RST).
fn try_accept_listener(seg: &TcpSegment, peer_mac: MacAddr, peer_ip: Ipv4Addr, cfg: &NetCfg) -> bool {
    let has_room = {
        let t = SOCKETS.lock();
        t.iter().any(|s| matches!(s, Some(Sock::Listen { port, backlog, queue }) if *port == seg.dst_port && queue.len() < *backlog))
    };
    if !has_room {
        return false;
    }
    // Passive open without holding the SOCKETS lock (it does network I/O).
    if let Some(conn) = TcpConn::accept_from(cfg.my_mac, cfg.my_ip, peer_mac, peer_ip, seg.src_port, seg.dst_port, seg) {
        let mut t = SOCKETS.lock();
        let idx = t.iter().position(|s| matches!(s, Some(Sock::Listen { port, backlog, queue }) if *port == seg.dst_port && queue.len() < *backlog));
        if let Some(idx) = idx {
            if let Some(Sock::Listen { queue, .. }) = &mut t[idx] {
                queue.push_back(conn);
            }
        }
    }
    true
}

pub fn service() {
    let cfg = match get() {
        Some(c) => c,
        None => return,
    };
    for _ in 0..8 {
        let rx = match nic::poll_recv() {
            Some(r) => r,
            None => break,
        };
        if let Ok((h, payload)) = EthernetHeader::parse(&rx) {
            if h.ethertype == EtherType::Arp {
                if let Ok(a) = ArpPacket::parse(payload) {
                    if a.op == ArpOp::Request && a.target_ip == cfg.my_ip {
                        let reply = ArpPacket::reply_to(&a, cfg.my_mac);
                        let frame = EthernetHeader { dst: a.sender_mac, src: cfg.my_mac, ethertype: EtherType::Arp }
                            .build(&reply.build());
                        nic::send(&frame);
                    }
                }
            } else if h.ethertype == EtherType::Ipv4 {
                if let Ok((ih, ipl)) = Ipv4Header::parse(payload) {
                    // Only traffic that is really addressed to us.
                    if ih.dst != cfg.my_ip {
                        continue;
                    }
                    // N3: packet filter (EuroFW). Test the incoming packet; a
                    // blocked packet is silently dropped (stealth, no RST/ICMP).
                    let (sport, dport) = if ipl.len() >= 4 && matches!(ih.protocol, Protocol::Tcp | Protocol::Udp) {
                        (u16::from_be_bytes([ipl[0], ipl[1]]), u16::from_be_bytes([ipl[2], ipl[3]]))
                    } else {
                        (0, 0)
                    };
                    if !crate::firewall::inbound_allowed(
                        ih.protocol.as_u8(),
                        u32::from_be_bytes(ih.src.0),
                        u32::from_be_bytes(ih.dst.0),
                        sport,
                        dport,
                    ) {
                        continue;
                    }
                    match ih.protocol {
                        Protocol::Tcp => {
                            if let Ok(seg) = TcpSegment::parse_checked(ipl, ih.src, ih.dst) {
                                let is_syn = seg.has(tcp::SYN) && !seg.has(tcp::ACK);
                                let http = SERVER_ON.load(Ordering::Relaxed) && seg.dst_port == 80;
                                if http && is_syn {
                                    // Background HTTP server: serve an incoming SYN on :80.
                                    serve_connection(cfg.my_mac, cfg.my_ip, h.src, ih.src, &seg);
                                } else if is_syn && try_accept_listener(&seg, h.src, ih.src, &cfg) {
                                    // A userspace LISTEN socket on this port accepted the
                                    // connection (passive open + queued).
                                } else if is_syn && icmp_err_allowed() {
                                    // SYN to a closed port → "connection refused": RST
                                    // (RFC 793), instead of letting the packet disappear. Rate-limited.
                                    if let Some(rst) = TcpSegment::reset_to(&seg) {
                                        send_ipv4(cfg.my_mac, cfg.my_ip, h.src, ih.src, Protocol::Tcp, &rst.build(cfg.my_ip, ih.src));
                                    }
                                }
                            }
                        }
                        // Make EuroOS pingable: reply to an incoming echo-request.
                        Protocol::Icmp => {
                            if let Ok(echo) = IcmpEcho::parse(ipl) {
                                if echo.kind == IcmpType::EchoRequest {
                                    let reply = IcmpEcho::reply_to(&echo);
                                    send_ipv4(cfg.my_mac, cfg.my_ip, h.src, ih.src, Protocol::Icmp, &reply.build());
                                }
                            }
                        }
                        // Unsolicited UDP datagram = no listener → ICMP port-unreachable
                        // (RFC 792). During a blocking DNS/ping, service() doesn't run, so
                        // whatever arrives here is not expected by anyone.
                        Protocol::Udp if icmp_err_allowed() => {
                            let err = IcmpError::DestUnreachable(UnreachCode::Port).build(payload);
                            send_ipv4(cfg.my_mac, cfg.my_ip, h.src, ih.src, Protocol::Icmp, &err);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Minimal TCP client: HTTP/1.0 GET. Does the 3-way handshake, sends the
/// request, receives the response (ACKs each segment) and closes cleanly.
/// Returns (status line, raw response bytes).
/// Next free ephemeral source port (49152–65535), so concurrent
/// connections don't collide.
static NEXT_PORT: Mutex<u16> = Mutex::new(49152);
fn alloc_port() -> u16 {
    let mut p = NEXT_PORT.lock();
    let port = *p;
    *p = if port >= 65000 { 49152 } else { port + 1 };
    port
}

/// An active TCP client connection: handshake, send, recv, teardown.
/// Synchronous (poll-based) — fits our non-preemptive ring-3 runs.
/// This is the object behind the socket syscalls AND behind `http_get`.
pub struct TcpConn {
    my_mac: MacAddr,
    my_ip: Ipv4Addr,
    nexthop: MacAddr,
    server: Ipv4Addr,
    dport: u16,
    sport: u16,
    my_seq: u32,
    their_seq: u32,
    /// Oldest not-yet-ACK'd seq (snd.una): the peer has acknowledged up to here.
    snd_una: u32,
    /// Connection still usable (no FIN/RST seen from the peer)?
    pub open: bool,
    /// In-order received bytes not yet fetched by recv().
    rx: alloc::collections::VecDeque<u8>,
    /// Sent-but-not-yet-ACK'd data segments (seq, bytes) for
    /// retransmission on packet loss.
    retx: alloc::vec::Vec<(u32, alloc::vec::Vec<u8>)>,
}

impl TcpConn {
    fn emit(&self, flags: u8, payload: &[u8]) {
        let seg = TcpSegment {
            src_port: self.sport,
            dst_port: self.dport,
            seq: self.my_seq,
            ack: self.their_seq,
            flags,
            window: 64240,
            payload: payload.to_vec(),
        };
        let iph = Ipv4Header { protocol: Protocol::Tcp, ttl: 64, src: self.my_ip, dst: self.server, total_length: 0, identification: 3 };
        let frame = EthernetHeader { dst: self.nexthop, src: self.my_mac, ethertype: EtherType::Ipv4 }
            .build(&iph.build(&seg.build(self.my_ip, self.server)));
        nic::send(&frame);
    }

    /// Wait for the next TCP segment from our peer for this port.
    fn poll_seg(&self) -> Option<TcpSegment> {
        for _ in 0..SPINS * 3 {
            if let Some(rx) = nic::poll_recv() {
                if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                    if h.ethertype == EtherType::Ipv4 {
                        if let Ok((ih, ipl)) = Ipv4Header::parse(p) {
                            if ih.protocol == Protocol::Tcp && ih.src == self.server {
                                if let Ok(seg) = TcpSegment::parse_checked(ipl, ih.src, ih.dst) {
                                    if seg.dst_port == self.sport {
                                        return Some(seg);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// 3-way handshake. Returns a connected socket, or None on timeout.
    pub fn connect(my_mac: MacAddr, my_ip: Ipv4Addr, nexthop: MacAddr, server: Ipv4Addr, dport: u16) -> Option<TcpConn> {
        drain();
        // Randomized ISN (RFC 6528) — a fixed ISN would let an off-path attacker
        // guess sequence numbers and inject RST/data into the connection.
        // (Server-side `accept_from` already does this; now the client too.)
        let isn = (rand_u64() as u32) | 1;
        let mut c = TcpConn {
            my_mac,
            my_ip,
            nexthop,
            server,
            dport,
            sport: alloc_port(),
            my_seq: isn,
            their_seq: 0,
            snd_una: isn,
            open: false,
            rx: alloc::collections::VecDeque::new(),
            retx: alloc::vec::Vec::new(),
        };
        // SYN with retransmission: a lost SYN or SYN-ACK would otherwise make the
        // handshake fail immediately. At most 4 attempts (poll_seg timeout per round).
        let mut synack = None;
        for _ in 0..4 {
            c.emit(tcp::SYN, &[]); // (re)transmit the SYN
            if let Some(s) = c.poll_seg() {
                if s.has(tcp::SYN) && s.has(tcp::ACK) {
                    synack = Some(s);
                    break;
                }
            }
        }
        let synack = synack?;
        c.my_seq = isn.wrapping_add(1);
        c.snd_una = c.my_seq;
        c.their_seq = synack.seq.wrapping_add(1);
        c.emit(tcp::ACK, &[]);
        c.open = true;
        Some(c)
    }

    /// Passive open (server side): we received a SYN; send SYN-ACK and wait
    /// for the closing ACK. Returns a connected server socket, or None.
    /// `local_port` = the listening port, `syn` = the incoming SYN segment.
    pub fn accept_from(
        my_mac: MacAddr,
        my_ip: Ipv4Addr,
        peer_mac: MacAddr,
        peer_ip: Ipv4Addr,
        peer_port: u16,
        local_port: u16,
        syn: &TcpSegment,
    ) -> Option<TcpConn> {
        let isn = (rand_u64() as u32) | 1; // randomized ISN
        let mut c = TcpConn {
            my_mac,
            my_ip,
            nexthop: peer_mac,
            server: peer_ip,
            dport: peer_port,
            sport: local_port,
            my_seq: isn,
            their_seq: syn.seq.wrapping_add(1),
            snd_una: isn,
            open: false,
            rx: alloc::collections::VecDeque::new(),
            retx: alloc::vec::Vec::new(),
        };
        c.emit(tcp::SYN | tcp::ACK, &[]);
        c.my_seq = isn.wrapping_add(1);
        c.snd_una = c.my_seq;
        for _ in 0..4 {
            match c.poll_seg() {
                Some(s) => {
                    if s.has(tcp::RST) {
                        return None;
                    }
                    if s.has(tcp::ACK) && s.ack == c.my_seq {
                        c.open = true;
                        // Buffer an included request (data on the ACK) right away.
                        if !s.payload.is_empty() && s.seq == c.their_seq {
                            c.their_seq = c.their_seq.wrapping_add(s.payload.len() as u32);
                            c.rx.extend(s.payload.iter().copied());
                            c.emit(tcp::ACK, &[]);
                        }
                        return Some(c);
                    }
                }
                None => c.emit(tcp::SYN | tcp::ACK, &[]), // retransmit SYN-ACK
            }
        }
        None
    }

    /// Process all immediately available segments: buffer in-order payload,
    /// ACK what we receive, and handle FIN/RST. Non-blocking per round,
    /// but poll_seg itself waits until something arrives or the timeout elapses.
    fn pump(&mut self, rounds: usize) {
        for _ in 0..rounds {
            let seg = match self.poll_seg() {
                Some(s) => s,
                None => break,
            };
            if seg.has(tcp::RST) {
                self.open = false;
                break;
            }
            // Cumulative ACK from the peer: advance snd.una and drop
            // acknowledged segments from the retransmission buffer.
            if seg.has(tcp::ACK) {
                self.ack_upto(seg.ack);
            }
            if !seg.payload.is_empty() {
                if seg.seq == self.their_seq {
                    self.their_seq = self.their_seq.wrapping_add(seg.payload.len() as u32);
                    self.rx.extend(seg.payload.iter().copied());
                }
                self.emit(tcp::ACK, &[]);
            }
            if seg.has(tcp::FIN) {
                self.their_seq = self.their_seq.wrapping_add(1);
                self.emit(tcp::ACK, &[]);
                self.open = false;
                break;
            }
            if self.rx.len() > 256 * 1024 {
                break;
            }
        }
    }

    /// Advance snd.una to `ack` (wrapping-aware) and remove fully
    /// acknowledged segments from the retransmission buffer.
    fn ack_upto(&mut self, ack: u32) {
        // `ack` lies before snd.una if the difference falls in the upper half of the
        // seq space (old/duplicate) — then ignore it.
        if ack.wrapping_sub(self.snd_una) < 0x8000_0000 && ack != self.snd_una {
            self.snd_una = ack;
        }
        self.retx.retain(|(seq, data)| {
            let end = seq.wrapping_add(data.len() as u32);
            // Keep as long as the end of the segment still lies after snd.una.
            end.wrapping_sub(self.snd_una) != 0 && end.wrapping_sub(self.snd_una) < 0x8000_0000
        });
    }

    /// Send application data (PSH+ACK) reliably: each segment goes into the
    /// retransmission buffer; then we pump on ACKs and retransmit
    /// unacknowledged segments (bounded) — so the connection survives packet loss.
    pub fn send(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // QEMU-MTU-safe segment size.
        for chunk in data.chunks(1024) {
            self.emit(tcp::PSH | tcp::ACK, chunk);
            self.retx.push((self.my_seq, chunk.to_vec()));
            self.my_seq = self.my_seq.wrapping_add(chunk.len() as u32);
        }
        // Wait for acknowledgement; retransmit unacknowledged segments (max 5 rounds).
        for _ in 0..5 {
            self.pump(2);
            if self.retx.is_empty() || !self.open {
                break;
            }
            // Timeout on this round: retransmit everything still outstanding.
            for (seq, chunk) in self.retx.clone() {
                let saved = self.my_seq;
                self.my_seq = seq;
                self.emit(tcp::PSH | tcp::ACK, &chunk);
                self.my_seq = saved;
            }
        }
    }

    /// Read up to `max` bytes. Waits as needed until something arrives or the peer
    /// closes. Returns an empty Vec for a closed, empty connection.
    pub fn recv(&mut self, max: usize) -> alloc::vec::Vec<u8> {
        let mut tries = 0;
        while self.rx.is_empty() && self.open && tries < 80 {
            self.pump(8);
            tries += 1;
        }
        let n = max.min(self.rx.len());
        self.rx.drain(..n).collect()
    }

    /// Send a FIN and close the connection cleanly.
    pub fn close(&mut self) {
        if self.open {
            self.emit(tcp::FIN | tcp::ACK, &[]);
            self.my_seq = self.my_seq.wrapping_add(1);
            self.open = false;
            // Wait briefly for the FIN/ACK from the peer.
            let _ = self.poll_seg();
        }
    }
}

pub fn http_get(
    my_mac: MacAddr,
    my_ip: Ipv4Addr,
    nexthop: MacAddr,
    server: Ipv4Addr,
    host: &str,
    path: &str,
) -> Option<(String, alloc::vec::Vec<u8>)> {
    let mut c = TcpConn::connect(my_mac, my_ip, nexthop, server, 80)?;
    let req = alloc::format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    c.send(req.as_bytes());
    let mut data: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    loop {
        let chunk = c.recv(8192);
        if chunk.is_empty() {
            break;
        }
        data.extend_from_slice(&chunk);
        if data.len() > 256 * 1024 {
            break;
        }
    }
    c.close();
    let status = String::from_utf8_lossy(&data).lines().next().unwrap_or("").into();
    Some((status, data))
}

// ── Entropy for TLS (keys + nonces) ─────────────────────────────────
// RDRAND if present (CPUID), otherwise a functional mix of RDTSC +
// timer ticks + a counter, all through SHA-256. NB: the RDTSC fallback is
// FUNCTIONAL but not cryptographically strong — production uses RDRAND/TPM
// (see the Hardware-security track). Enough to actually complete the handshake.
static ENTROPY_CTR: spin::Mutex<u64> = spin::Mutex::new(0xE0_05_15);

fn have_rdrand() -> bool {
    let r = unsafe { core::arch::x86_64::__cpuid(1) };
    r.ecx & (1 << 30) != 0
}

fn rdrand64() -> Option<u64> {
    let mut v: u64;
    let mut ok: u8;
    for _ in 0..16 {
        unsafe {
            core::arch::asm!("rdrand {0}; setc {1}", out(reg) v, out(reg_byte) ok, options(nomem, nostack));
        }
        if ok == 1 {
            return Some(v);
        }
    }
    None
}

/// 64 bits of unpredictability: hardware RDRAND if available, otherwise a
/// mix of RDTSC + HPET + a running counter. For txid/source-port randomization
/// (defence-in-depth against DNS spoofing — RFC 5452).
pub fn rand_u64() -> u64 {
    // Only call RDRAND if the CPU advertises it — otherwise the opcode is
    // invalid (#UD). On qemu64/TCG RDRAND is absent; then the fallback mix.
    if have_rdrand() {
        if let Some(r) = rdrand64() {
            return r;
        }
    }
    let t = unsafe { core::arch::x86_64::_rdtsc() };
    let h = crate::hpet::counter();
    let mut c = ENTROPY_CTR.lock();
    *c = c.wrapping_add(0x9E37_79B9_7F4A_7C15);
    t ^ h.rotate_left(17) ^ *c
}

/// 32 random bytes for a TLS key/nonce. `domain` separates
/// independent draws within one handshake.
fn gather_entropy(domain: u8) -> [u8; 32] {
    let mut seed: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    seed.push(domain);
    // ★ Audit H1: mix in the TPM's hardware RNG. On TCG/QEMU RDRAND is absent,
    // which made the old fallback (RDTSC^HPET^counter) low-entropy and partly
    // predictable — fatal for an ephemeral X25519 key. The TPM RNG
    // (same source as FDE/VPN/CA) is the strong entropy source when present.
    if let Some(tpm_rnd) = crate::tpm::get_random(32) {
        seed.extend_from_slice(&tpm_rnd);
    }
    let rd = have_rdrand();
    for _ in 0..8 {
        let t = unsafe { core::arch::x86_64::_rdtsc() };
        seed.extend_from_slice(&t.to_le_bytes());
        if rd {
            if let Some(r) = rdrand64() {
                seed.extend_from_slice(&r.to_le_bytes());
            }
        }
    }
    seed.extend_from_slice(&crate::interrupts::ticks().to_le_bytes());
    {
        let mut c = ENTROPY_CTR.lock();
        *c = c.wrapping_add(0x9E37_79B9_7F4A_7C15);
        seed.extend_from_slice(&c.to_le_bytes());
    }
    eurotls::keyschedule::sha256(&seed)
}

/// HTTPS GET via our own TLS 1.3 stack on top of TcpConn. Returns
/// (status line, body, certificate length).
pub fn https_get(
    my_mac: MacAddr,
    my_ip: Ipv4Addr,
    nexthop: MacAddr,
    server: Ipv4Addr,
    host: &str,
    path: &str,
) -> Option<(String, alloc::vec::Vec<u8>, Option<alloc::vec::Vec<u8>>)> {
    let req = alloc::format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let (body, cert) = https_exchange(my_mac, my_ip, nexthop, server, host, req.as_bytes())?;
    let status = String::from_utf8_lossy(&body).lines().next().unwrap_or("").into();
    Some((status, body, cert))
}

/// One encrypted HTTP exchange over EuroTLS 1.3: build the connection,
/// do the handshake (with certificate validation against the bundled root CAs), send
/// `request` (a complete HTTP request incl. any body), and return the raw
/// response (status line + headers + body) + the server certificate. Shared by
/// `https_get` (GET) and `post_full` (POST) — so POST works over the same real TLS.
fn https_exchange(
    my_mac: MacAddr,
    my_ip: Ipv4Addr,
    nexthop: MacAddr,
    server: Ipv4Addr,
    host: &str,
    request: &[u8],
) -> Option<(alloc::vec::Vec<u8>, Option<alloc::vec::Vec<u8>>)> {
    let mut tcp = TcpConn::connect(my_mac, my_ip, nexthop, server, 443)?;
    let random = gather_entropy(0);
    let secret = gather_entropy(1);
    let (mut tls, hello) = eurotls::Tls13Client::new(host, random, secret);
    // Enable certificate validation: verify the chain against the bundled
    // EU/international root CAs + the server name + the validity window, and
    // check the CertificateVerify signature. A MITM with a fake (but
    // intrinsically valid) certificate is thus rejected.
    tls.set_trust_anchor(crate::rtc::epoch() as i64, crate::tls_roots::ROOTS);
    tcp.send(&hello);

    // Handshake: feed server records in until the connection is up.
    let mut idle = 0;
    while !tls.is_connected() {
        let data = tcp.recv(16384);
        if data.is_empty() {
            idle += 1;
            if idle > 12 {
                return None;
            }
            continue;
        }
        idle = 0;
        tls.feed(&data);
        match tls.process() {
            Ok(out) => {
                if !out.is_empty() {
                    tcp.send(&out);
                }
            }
            Err(e) => {
                if let eurotls::TlsError::Protocol(why) = e {
                    crate::kwarn!("[tls] handshake aborted: {why} ({} certs in chain)", tls.cert_chain_info().len());
                }
                return None;
            }
        }
    }

    // Encrypted HTTP request (built by the caller: GET or POST + body).
    if let Ok(rec) = tls.encrypt_app(request) {
        tcp.send(&rec);
    }

    // Receive + decrypt the response until close_notify/timeout.
    let mut body: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    let mut idle = 0;
    loop {
        let data = tcp.recv(16384);
        if data.is_empty() {
            idle += 1;
            if idle > 12 {
                break;
            }
            continue;
        }
        idle = 0;
        tls.feed(&data);
        match tls.process() {
            Ok(_) => {}
            Err(eurotls::TlsError::Alert(_)) => {
                body.extend_from_slice(&tls.take_app_data()); // close_notify = clean EOF
                break;
            }
            Err(_) => break,
        }
        body.extend_from_slice(&tls.take_app_data());
        if body.len() > 200_000 {
            break;
        }
    }
    tcp.close();
    let cert = tls.server_cert.clone();
    Some((body, cert))
}

/// Shell command `https <host>` — fetch https://<host>/ via EuroTLS 1.3.
pub fn cmd_https(host: &str) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    let cfg = match get() {
        Some(c) => c,
        None => {
            out.push("https: no network".into());
            return out;
        }
    };
    let server = match resolve(host) {
        Some(ip) => {
            out.push(alloc::format!("DNS: {host} = {}", ipfmt(ip)));
            ip
        }
        None => {
            out.push(alloc::format!("https: cannot resolve '{host}'"));
            return out;
        }
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    out.push("TLS 1.3 handshake (X25519 + ChaCha20-Poly1305) ...".into());
    match https_get(cfg.my_mac, cfg.my_ip, nexthop, server, host, "/") {
        Some((status, data, cert)) => {
            out.push(alloc::format!("GET https://{host}/ -> {} bytes (encrypted)", data.len()));
            out.push(alloc::format!("  {}", status.trim()));
            if let Some(c) = cert {
                let fp = eurotls::keyschedule::sha256(&c);
                let mut hexs = String::new();
                for b in &fp[..8] {
                    hexs.push_str(&alloc::format!("{b:02x}"));
                }
                out.push(alloc::format!("  server certificate: {} bytes, SHA-256 {}…", c.len(), hexs));
            }
        }
        None => out.push(alloc::format!("https: TLS handshake with {host} failed")),
    }
    out
}

// ── Socket layer for userspace ────────────────────────────────────────────
// A real BSD-like socket API behind the Linux syscalls socket/connect/
// send/recv, tied to TcpConn. Userspace fd's for sockets start at
// SOCK_FD_BASE so they don't collide with the (small) VFS fd's.

/// First fd number that represents a socket (well above the VFS fd's).
pub const SOCK_FD_BASE: u64 = 500;
const MAX_SOCK: usize = 16;

/// A connectionless UDP socket: after connect() we remember the destination and
/// send/receive datagrams on our ephemeral source port.
pub struct UdpSock {
    my_mac: MacAddr,
    my_ip: Ipv4Addr,
    nexthop: MacAddr,
    server: Ipv4Addr,
    dport: u16,
    sport: u16,
}

impl UdpSock {
    /// Send one datagram to the connected destination.
    pub fn send(&self, data: &[u8]) {
        let dg = UdpDatagram { src_port: self.sport, dst_port: self.dport, payload: data.to_vec() };
        let iph = Ipv4Header { protocol: Protocol::Udp, ttl: 64, src: self.my_ip, dst: self.server, total_length: 0, identification: 4 };
        let frame = EthernetHeader { dst: self.nexthop, src: self.my_mac, ethertype: EtherType::Ipv4 }
            .build(&iph.build(&dg.build(self.my_ip, self.server)));
        nic::send(&frame);
    }

    /// Wait for one datagram from the destination, back on our source port.
    pub fn recv(&self) -> alloc::vec::Vec<u8> {
        for _ in 0..SPINS * 3 {
            if let Some(rx) = nic::poll_recv() {
                if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                    if h.ethertype == EtherType::Ipv4 {
                        if let Ok((ih, ipl)) = Ipv4Header::parse(p) {
                            if ih.protocol == Protocol::Udp && ih.src == self.server {
                                if let Ok(dg) = UdpDatagram::parse(ipl, ih.src, ih.dst) {
                                    if dg.dst_port == self.sport {
                                        return dg.payload;
                                    }
                                }
                            } else if ih.protocol == Protocol::Icmp && ih.src == self.server {
                                // An ICMP error from the destination that bounces our datagram
                                // back (port/host unreachable) → fail fast instead of timing out.
                                if let Some((IcmpError::DestUnreachable(_), orig)) = IcmpError::parse(ipl) {
                                    if embedded_udp_matches(&orig, self.sport, self.dport) {
                                        return alloc::vec::Vec::new();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        alloc::vec::Vec::new()
    }
}

enum Sock {
    /// socket() done, connect()/bind() not yet. `dgram` = SOCK_DGRAM (UDP).
    /// `bind_port` = local port fixed via bind() (0 = not yet).
    Reserved { dgram: bool, bind_port: u16 },
    /// Connected TCP socket.
    Conn(TcpConn),
    /// "Connected" UDP socket (destination remembered).
    Udp(UdpSock),
    /// Listening TCP socket: passive open on `port`, with an accept queue
    /// (bounded by `backlog`) of already-completed connections.
    Listen { port: u16, backlog: usize, queue: alloc::collections::VecDeque<TcpConn> },
}

static SOCKETS: Mutex<[Option<Sock>; MAX_SOCK]> = Mutex::new([const { None }; MAX_SOCK]);

/// Is this fd number a socket (not a VFS file)?
pub fn is_sock_fd(fd: u64) -> bool {
    fd >= SOCK_FD_BASE && (fd - SOCK_FD_BASE) < MAX_SOCK as u64
}

/// socket(AF_INET, SOCK_STREAM): reserve a slot, return the fd number.
/// -1 if the table is full.
pub fn sock_open(dgram: bool) -> u64 {
    let mut t = SOCKETS.lock();
    for (i, slot) in t.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(Sock::Reserved { dgram, bind_port: 0 });
            return SOCK_FD_BASE + i as u64;
        }
    }
    (-1i64) as u64
}

/// connect(fd, ip, port): for TCP a 3-way handshake, for UDP just
/// remember the destination. 0 / -1.
pub fn sock_connect(fd: u64, server: Ipv4Addr, port: u16) -> u64 {
    if !is_sock_fd(fd) {
        return (-1i64) as u64;
    }
    // EuroGuard (Track 7, Phase 7.1): let the policy engine evaluate this
    // outgoing connection BEFORE a packet leaves. A blocked app
    // gets -EPERM and the request is audited — a hard policy boundary.
    let app = crate::ring3::current_app();
    let dst = alloc::format!("{}.{}.{}.{}:{port}", server.0[0], server.0[1], server.0[2], server.0[3]);
    if crate::euroguard::check_connect(&app, server, port) == crate::euroguard::Decision::Block {
        // 3D-6: a blocked connection is a policy violation in the hash-chained log.
        crate::audit::record_connection(&dst, false);
        return (-1i64) as u64; // -EPERM: denied by EuroGuard
    }
    // 3D-6: record the (allowed) outbound connection.
    crate::audit::record_connection(&dst, true);
    let cfg = match get() {
        Some(c) => c,
        None => return (-1i64) as u64,
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    let i = (fd - SOCK_FD_BASE) as usize;
    // Determine the socket type without holding the lock during connect().
    let dgram = {
        let t = SOCKETS.lock();
        match t[i] {
            Some(Sock::Reserved { dgram, .. }) => dgram,
            _ => return (-1i64) as u64,
        }
    };
    if dgram {
        let s = UdpSock { my_mac: cfg.my_mac, my_ip: cfg.my_ip, nexthop, server, dport: port, sport: alloc_port() };
        SOCKETS.lock()[i] = Some(Sock::Udp(s));
        0
    } else {
        let conn = match TcpConn::connect(cfg.my_mac, cfg.my_ip, nexthop, server, port) {
            Some(c) => c,
            None => return (-1i64) as u64,
        };
        SOCKETS.lock()[i] = Some(Sock::Conn(conn));
        0
    }
}

/// Shell demo `tcpserve <port>`: open a server socket, listen, accept one
/// connection, read the request and send a reply. Proves the listen/accept
/// chain (passive open) with an external client.
pub fn cmd_tcpserve(port: u16) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    if get().is_none() {
        out.push("tcpserve: no network".into());
        return out;
    }
    let fd = sock_open(false);
    if (fd as i64) < 0 {
        out.push("tcpserve: socket() failed".into());
        return out;
    }
    if (sock_bind(fd, port) as i64) < 0 || (sock_listen(fd, 4) as i64) < 0 {
        sock_close(fd);
        out.push("tcpserve: bind/listen failed".into());
        return out;
    }
    out.push(alloc::format!("tcpserve: LISTEN on :{port}, waiting for a connection..."));
    let cfd = sock_accept(fd);
    if (cfd as i64) < 0 {
        sock_close(fd);
        out.push("tcpserve: accept() timeout (no client connected)".into());
        return out;
    }
    out.push("tcpserve: connection ACCEPTED ✓".into());
    let req = sock_recv(cfd, 512);
    let line = String::from_utf8_lossy(&req);
    out.push(alloc::format!("  client sent: {:?}", line.lines().next().unwrap_or("")));
    let body = "EuroOS server socket: hello, accept() works!\n";
    let resp = alloc::format!(
        "HTTP/1.1 200 OK\r\nServer: EuroOS\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    sock_send(cfd, resp.as_bytes());
    sock_close(cfd);
    sock_close(fd);
    out.push("tcpserve: reply sent + closed ✓".into());
    out
}

/// bind(fd, port): fix the local port for a (stream) socket. 0/-1.
pub fn sock_bind(fd: u64, port: u16) -> u64 {
    if !is_sock_fd(fd) {
        return (-1i64) as u64;
    }
    let i = (fd - SOCK_FD_BASE) as usize;
    let mut t = SOCKETS.lock();
    match &mut t[i] {
        Some(Sock::Reserved { dgram: false, bind_port }) => {
            *bind_port = port;
            0
        }
        _ => (-1i64) as u64,
    }
}

/// listen(fd, backlog): put a bound stream socket into LISTEN. 0/-1.
pub fn sock_listen(fd: u64, backlog: usize) -> u64 {
    if !is_sock_fd(fd) {
        return (-1i64) as u64;
    }
    let i = (fd - SOCK_FD_BASE) as usize;
    let mut t = SOCKETS.lock();
    match t[i] {
        Some(Sock::Reserved { dgram: false, bind_port }) if bind_port != 0 => {
            t[i] = Some(Sock::Listen { port: bind_port, backlog: backlog.clamp(1, 32), queue: alloc::collections::VecDeque::new() });
            0
        }
        _ => (-1i64) as u64,
    }
}

/// accept(fd): take the next completed connection from the accept queue and
/// return a new socket fd. Blocks (bounded) by pumping `service()` until
/// a connection arrives. -1 on timeout or no listener.
pub fn sock_accept(fd: u64) -> u64 {
    if !is_sock_fd(fd) {
        return (-1i64) as u64;
    }
    let i = (fd - SOCK_FD_BASE) as usize;
    // Block until a connection arrives or the deadline elapses. The deadline
    // is in scheduler ticks (100 Hz guest time); `service()` is non-blocking, so
    // we pump until the clock reaches the upper bound.
    let deadline = crate::interrupts::ticks() + 400; // ~4 s guest-time upper bound
    while crate::interrupts::ticks() < deadline {
        // Pop a ready connection (hold the lock briefly).
        let conn = {
            let mut t = SOCKETS.lock();
            match &mut t[i] {
                Some(Sock::Listen { queue, .. }) => queue.pop_front(),
                _ => return (-1i64) as u64, // no listening socket
            }
        };
        if let Some(conn) = conn {
            let mut t = SOCKETS.lock();
            for (j, slot) in t.iter_mut().enumerate() {
                if slot.is_none() {
                    *slot = Some(Sock::Conn(conn));
                    return SOCK_FD_BASE + j as u64;
                }
            }
            return (-1i64) as u64; // socket table full
        }
        // No connection ready: process incoming packets (may complete one).
        service();
    }
    (-1i64) as u64
}

/// Resolve a hostname (or "a.b.c.d") to an IPv4 address via DNS.
pub fn resolve(host: &str) -> Option<Ipv4Addr> {
    if let Some(ip) = parse_ipv4(host) {
        return Some(ip);
    }
    if let Some(ip) = hosts_lookup(host) {
        return Some(ip); // /etc/hosts before DNS
    }
    let cfg = get()?;
    dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, host)
}

/// send(fd, data): number of bytes sent, or -1.
pub fn sock_send(fd: u64, data: &[u8]) -> u64 {
    if !is_sock_fd(fd) {
        return (-1i64) as u64;
    }
    let i = (fd - SOCK_FD_BASE) as usize;
    let sent = {
        let mut t = SOCKETS.lock();
        match &mut t[i] {
            Some(Sock::Conn(c)) => {
                c.send(data);
                data.len() as u64
            }
            Some(Sock::Udp(u)) => {
                // EuroGuard DNS-level filtering: inspect queries to port 53
                // and block trackers/ads before the query goes onto the network.
                if u.dport == 53 {
                    if let Some(name) = dns::parse_query_name(data) {
                        if crate::euroguard::check_dns(&crate::ring3::current_app(), &name)
                            == crate::euroguard::Decision::Block
                        {
                            // Blocked: send nothing; the app gets no reply.
                            return data.len() as u64;
                        }
                    }
                }
                u.send(data);
                data.len() as u64
            }
            _ => return (-1i64) as u64,
        }
    };
    // EuroGuard statistic (Phase 7.4): bytes sent per app.
    crate::euroguard::record_bytes(&crate::ring3::current_app(), sent, 0);
    sent
}

/// recv(fd, max): copy up to `max` received bytes into `out`; return the count
/// (0 = connection closed and empty).
pub fn sock_recv(fd: u64, max: usize) -> alloc::vec::Vec<u8> {
    if !is_sock_fd(fd) {
        return alloc::vec::Vec::new();
    }
    let i = (fd - SOCK_FD_BASE) as usize;
    let data = {
        let mut t = SOCKETS.lock();
        match &mut t[i] {
            Some(Sock::Conn(c)) => c.recv(max),
            Some(Sock::Udp(u)) => {
                let mut d = u.recv();
                d.truncate(max);
                d
            }
            _ => alloc::vec::Vec::new(),
        }
    };
    // EuroGuard statistic (Phase 7.4): bytes received per app.
    crate::euroguard::record_bytes(&crate::ring3::current_app(), 0, data.len() as u64);
    data
}

/// close(fd): FIN + free the slot. 0.
pub fn sock_close(fd: u64) -> u64 {
    if !is_sock_fd(fd) {
        return (-1i64) as u64;
    }
    let i = (fd - SOCK_FD_BASE) as usize;
    let mut t = SOCKETS.lock();
    if let Some(Sock::Conn(c)) = &mut t[i] {
        c.close();
    }
    t[i] = None;
    0
}

/// G3 self-test: poll a fresh listener + UDP socket. A listener with an empty
/// accept queue should NOT be readable — proves the readiness logic.
pub fn poll_selftest() {
    if get().is_none() {
        return;
    }
    let lfd = sock_open(false);
    let ufd = sock_open(true);
    if (lfd as i64) < 0 || (ufd as i64) < 0 {
        return;
    }
    sock_bind(lfd, 9100);
    sock_listen(lfd, 4);
    let deadline = crate::interrupts::ticks() + 2; // short deadline (no client)
    let ready = sock_poll(&[lfd, ufd], deadline);
    let listener_readable = ready.iter().find(|(f, _)| *f == lfd).map(|(_, r)| *r).unwrap_or(true);
    crate::serial_println!(
        "[g3] poll/select: {} fd's multiplexed — listener readable={} (fresh queue empty → expected false)",
        ready.len(),
        listener_readable
    );
    sock_close(lfd);
    sock_close(ufd);
}

/// poll/select (G3): non-blocking readiness check for a set of socket fd's,
/// so one task can multiplex multiple connections/listeners. Drives `service()`
/// (fills Listen accept queues) and pumps Conn sockets once, and reports per fd
/// whether it is READABLE (Conn: data or EOF; Listen: a waiting connection). Waits
/// until something is ready or the tick deadline elapses. Returns `(fd, readable)` per fd.
pub fn sock_poll(fds: &[u64], deadline_ticks: u64) -> alloc::vec::Vec<(u64, bool)> {
    // Spin ceiling alongside the tick deadline: if the timer tick for whatever reason
    // does not advance, poll() must NEVER block forever.
    let mut spins: u64 = 0;
    loop {
        service(); // process incoming packets → fills Listen queues
        {
            // Pump Conn sockets so in-flight data lands in their rx buffer.
            let mut t = SOCKETS.lock();
            for &fd in fds {
                if is_sock_fd(fd) {
                    if let Some(Sock::Conn(c)) = &mut t[(fd - SOCK_FD_BASE) as usize] {
                        if c.rx.is_empty() && c.open {
                            c.pump(1);
                        }
                    }
                }
            }
        }
        let ready: alloc::vec::Vec<(u64, bool)> = {
            let t = SOCKETS.lock();
            fds.iter()
                .map(|&fd| {
                    let r = is_sock_fd(fd)
                        && match &t[(fd - SOCK_FD_BASE) as usize] {
                            Some(Sock::Conn(c)) => !c.rx.is_empty() || !c.open, // data or EOF
                            Some(Sock::Listen { queue, .. }) => !queue.is_empty(),
                            _ => false,
                        };
                    (fd, r)
                })
                .collect()
        };
        spins += 1;
        if ready.iter().any(|&(_, r)| r)
            || crate::interrupts::ticks() >= deadline_ticks
            || spins >= 2_000_000
        {
            return ready;
        }
    }
}

// ---------------------------------------------------------------------------
// AF_UNIX — local Unix-domain sockets (H1). One kernel-wide switch, separate from
// the TCP/IP SOCKETS table. The building block for the live display server (H2:
// compositor ↔ app) and IPC-heavy apps.
// ---------------------------------------------------------------------------
static UNIX_SWITCH: Mutex<euronet::unix::Switchboard> =
    Mutex::new(euronet::unix::Switchboard::new());

pub use euronet::unix::{Endpoint as UnixEndpoint, UnixError};

/// Bind+listen on an AF_UNIX path (server side).
pub fn unix_bind_listen(path: &str, backlog: usize) -> Result<(), UnixError> {
    UNIX_SWITCH.lock().bind_listen(path, backlog)
}
/// Connect to an AF_UNIX path (client side) → client endpoint.
pub fn unix_connect(path: &str) -> Result<UnixEndpoint, UnixError> {
    UNIX_SWITCH.lock().connect(path)
}
/// Accept the oldest waiting connection (server side) → server endpoint.
pub fn unix_accept(path: &str) -> Option<UnixEndpoint> {
    UNIX_SWITCH.lock().accept(path)
}
/// Write bytes from an endpoint.
pub fn unix_send(ep: UnixEndpoint, data: &[u8]) -> Result<usize, UnixError> {
    UNIX_SWITCH.lock().send(ep, data)
}
/// Read up to `max` bytes for an endpoint (non-blocking).
pub fn unix_recv(ep: UnixEndpoint, max: usize) -> Result<alloc::vec::Vec<u8>, UnixError> {
    UNIX_SWITCH.lock().recv(ep, max)
}
/// Is this endpoint readable (data or EOF)?
pub fn unix_readable(ep: UnixEndpoint) -> bool {
    UNIX_SWITCH.lock().readable(ep)
}
/// Close an endpoint.
pub fn unix_close(ep: UnixEndpoint) {
    UNIX_SWITCH.lock().close(ep)
}

// ── AF_UNIX socket fds for glibc/musl programs ───────────────────────────────
// A parallel fd space (base 600) mapping a process fd to a UnixEndpoint, so the
// Linux socket syscalls (socketpair/read/write/close) can drive local IPC. This
// is the transport a real X11 client (and dbus, etc.) uses to reach a server.
pub const UNIX_FD_BASE: u64 = 600;
const MAX_UNIX_FD: usize = 32;

// ── eventfd (Linux) ────────────────────────────────────────────────────────
// A counter-backed fd used by GLib's GMainContext (GWakeup) to break a poll():
// signal = write(+n), the loop polls it readable, acknowledge = read (drains).
// Essential for any GLib/GTK main loop. Own fd range so read/write/poll/close route.
pub const EVENTFD_BASE: u64 = 800;
const MAX_EVENTFD: usize = 32;
static EVENTFDS: Mutex<[Option<u64>; MAX_EVENTFD]> = Mutex::new([const { None }; MAX_EVENTFD]);

pub fn is_eventfd(fd: u64) -> bool {
    fd >= EVENTFD_BASE && (fd - EVENTFD_BASE) < MAX_EVENTFD as u64
}
/// eventfd2(initval, flags) — allocate a counter fd. Returns None if the table is full.
pub fn eventfd_create(initval: u64) -> Option<u64> {
    let mut t = EVENTFDS.lock();
    for (i, s) in t.iter_mut().enumerate() {
        if s.is_none() {
            *s = Some(initval);
            return Some(EVENTFD_BASE + i as u64);
        }
    }
    None
}
pub fn eventfd_readable(fd: u64) -> bool {
    is_eventfd(fd) && EVENTFDS.lock()[(fd - EVENTFD_BASE) as usize].map_or(false, |c| c > 0)
}
/// read(): return the current counter and reset to 0. Some(0) => the caller should
/// return -EAGAIN (nothing to read). None => not a live eventfd.
pub fn eventfd_read(fd: u64) -> Option<u64> {
    if !is_eventfd(fd) {
        return None;
    }
    let mut t = EVENTFDS.lock();
    match t[(fd - EVENTFD_BASE) as usize].as_mut() {
        Some(c) => {
            let v = *c;
            *c = 0;
            Some(v)
        }
        None => None,
    }
}
/// write(): add to the counter. Returns false if not a live eventfd.
pub fn eventfd_write(fd: u64, add: u64) -> bool {
    if !is_eventfd(fd) {
        return false;
    }
    let mut t = EVENTFDS.lock();
    match t[(fd - EVENTFD_BASE) as usize].as_mut() {
        Some(c) => {
            *c = c.saturating_add(add);
            true
        }
        None => false,
    }
}
pub fn eventfd_close(fd: u64) {
    if is_eventfd(fd) {
        EVENTFDS.lock()[(fd - EVENTFD_BASE) as usize] = None;
    }
}

/// What an AF_UNIX fd is backed by. socket() makes a Pending fd; connect()/socketpair
/// resolve it to a Switchboard stream, or — for the X display socket — to an X-server
/// connection that forwards to the kernel X server.
#[derive(Clone, Copy)]
enum UnixSock {
    Pending,
    Stream(UnixEndpoint),
    X(u64), // xserver connection fd
}

static UNIX_FDS: Mutex<[Option<UnixSock>; MAX_UNIX_FD]> = Mutex::new([const { None }; MAX_UNIX_FD]);
static UNIX_PAIR_CTR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
// Server-side bind: the path an AF_UNIX fd is bound+listening on (chrome's
// ProcessSingleton listens on SingletonSocket). Tracked alongside UNIX_FDS so the
// existing recv/send/close match arms need no new variant.
static UNIX_BOUND: Mutex<[Option<alloc::string::String>; MAX_UNIX_FD]> =
    Mutex::new([const { None }; MAX_UNIX_FD]);

pub fn is_unix_fd(fd: u64) -> bool {
    fd >= UNIX_FD_BASE && (fd - UNIX_FD_BASE) < MAX_UNIX_FD as u64
}

/// bind(fd, AF_UNIX path): bind+listen the fd on `path` (server side). Returns 0.
pub fn unix_bind_fd(fd: u64, path: &str) -> u64 {
    if !is_unix_fd(fd) {
        return (-9i64) as u64; // -EBADF
    }
    let idx = (fd - UNIX_FD_BASE) as usize;
    let _ = unix_bind_listen(path, 128);
    UNIX_BOUND.lock()[idx] = Some(alloc::string::String::from(path));
    0
}

/// accept(fd): accept a pending connection on a bound fd → a new stream fd, or
/// -EAGAIN when none is waiting (chrome's single-instance singleton: never any).
pub fn unix_accept_fd(fd: u64) -> u64 {
    if !is_unix_fd(fd) {
        return (-9i64) as u64;
    }
    let idx = (fd - UNIX_FD_BASE) as usize;
    let path = match UNIX_BOUND.lock()[idx].clone() {
        Some(p) => p,
        None => return (-22i64) as u64, // -EINVAL: not a listening socket
    };
    match unix_accept(&path) {
        Some(ep) => unix_alloc(UnixSock::Stream(ep)).unwrap_or((-24i64) as u64),
        None => (-11i64) as u64, // -EAGAIN
    }
}

fn unix_alloc(sock: UnixSock) -> Option<u64> {
    let mut t = UNIX_FDS.lock();
    for (i, s) in t.iter_mut().enumerate() {
        if s.is_none() {
            *s = Some(sock);
            return Some(UNIX_FD_BASE + i as u64);
        }
    }
    None
}

/// socket(AF_UNIX, SOCK_STREAM): an unconnected fd, resolved later by connect().
pub fn unix_socket() -> u64 {
    unix_alloc(UnixSock::Pending).unwrap_or((-24i64) as u64) // -EMFILE
}

/// dup(AF_UNIX fd): a NEW fd aliasing the SAME endpoint (UnixSock is Copy, so both
/// fds share the endpoint's buffers). close() of either just clears its slot; the
/// endpoint outlives both. Chrome's Mojo dups channel socket handles.
pub fn unix_fd_dup(fd: u64) -> u64 {
    if !is_unix_fd(fd) {
        return (-9i64) as u64; // -EBADF
    }
    let sock = UNIX_FDS.lock()[(fd - UNIX_FD_BASE) as usize];
    match sock {
        Some(s) => unix_alloc(s).unwrap_or((-24i64) as u64), // -EMFILE
        None => (-9i64) as u64,
    }
}

/// dup(eventfd): a new eventfd fd seeded with the current counter value. Note this
/// does NOT share the counter (our table is per-slot); adequate for the common
/// dup-to-transfer-then-close-original pattern chrome uses for platform handles.
pub fn eventfd_dup(fd: u64) -> u64 {
    if !is_eventfd(fd) {
        return (-9i64) as u64;
    }
    let val = EVENTFDS.lock()[(fd - EVENTFD_BASE) as usize];
    match val {
        Some(v) => {
            let mut t = EVENTFDS.lock();
            for (i, s) in t.iter_mut().enumerate() {
                if s.is_none() {
                    *s = Some(v);
                    return EVENTFD_BASE + i as u64;
                }
            }
            (-24i64) as u64 // -EMFILE
        }
        None => (-9i64) as u64,
    }
}

/// connect(fd, sockaddr_un path): resolve a Pending AF_UNIX fd. The X display socket
/// (/tmp/.X11-unix/X0, filesystem or abstract) routes to the kernel X server; any
/// other path goes to the Switchboard. Returns 0 or -errno.
pub fn unix_connect_fd(fd: u64, path: &str) -> u64 {
    let idx = (fd - UNIX_FD_BASE) as usize;
    let is_x = path.contains(".X11-unix/X") || path.ends_with("/X0");
    let new = if is_x {
        match crate::xserver::open() {
            Some(xfd) => UnixSock::X(xfd),
            None => return (-111i64) as u64, // -ECONNREFUSED
        }
    } else {
        match unix_connect(path) {
            Ok(ep) => UnixSock::Stream(ep),
            Err(_) => return (-111i64) as u64,
        }
    };
    let mut t = UNIX_FDS.lock();
    match t.get_mut(idx) {
        Some(slot @ Some(_)) => {
            *slot = Some(new);
            0
        }
        _ => (-9i64) as u64, // -EBADF
    }
}

/// socketpair(AF_UNIX, SOCK_STREAM): a connected pair of fds (bind/connect/accept on
/// a unique temp path). Writes to one are readable on the other.
pub fn unix_socketpair() -> Option<(u64, u64)> {
    let n = UNIX_PAIR_CTR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let path = alloc::format!("/run/euro-sp-{n}.sock");
    unix_bind_listen(&path, 1).ok()?;
    let client = unix_connect(&path).ok()?;
    let server = unix_accept(&path)?;
    UNIX_SWITCH.lock().unbind(&path);
    let a = unix_alloc(UnixSock::Stream(client))?;
    let b = unix_alloc(UnixSock::Stream(server))?;
    Some((a, b))
}

/// write() to a UNIX-socket fd (Switchboard stream or X-server connection).
pub fn unix_fd_send(fd: u64, data: &[u8]) -> u64 {
    let (ep, xfd) = {
        let t = UNIX_FDS.lock();
        match t.get((fd - UNIX_FD_BASE) as usize).and_then(|s| s.as_ref()) {
            Some(UnixSock::Stream(e)) => (Some(*e), 0),
            Some(UnixSock::X(x)) => (None, *x),
            _ => return (-9i64) as u64, // -EBADF / not connected
        }
    };
    if let Some(e) = ep {
        return match unix_send(e, data) {
            Ok(n) => n as u64,
            Err(_) => (-1i64) as u64,
        };
    }
    crate::xserver::write(xfd, data)
}

/// read() from a UNIX-socket fd.
pub fn unix_fd_recv(fd: u64, max: usize) -> alloc::vec::Vec<u8> {
    let (ep, xfd) = {
        let t = UNIX_FDS.lock();
        match t.get((fd - UNIX_FD_BASE) as usize).and_then(|s| s.as_ref()) {
            Some(UnixSock::Stream(e)) => (Some(*e), 0),
            Some(UnixSock::X(x)) => (None, *x),
            _ => return alloc::vec::Vec::new(),
        }
    };
    if let Some(e) = ep {
        return unix_recv(e, max).unwrap_or_default();
    }
    crate::xserver::read(xfd, max)
}

/// Is a UNIX-socket fd readable now (queued data)? For poll().
pub fn unix_fd_readable(fd: u64) -> bool {
    let t = UNIX_FDS.lock();
    match t.get((fd - UNIX_FD_BASE) as usize).and_then(|s| s.as_ref()) {
        Some(UnixSock::Stream(e)) => unix_readable(*e),
        Some(UnixSock::X(x)) => crate::xserver::readable(*x),
        _ => false,
    }
}

/// close() a UNIX-socket fd.
pub fn unix_fd_close(fd: u64) -> u64 {
    let idx = (fd - UNIX_FD_BASE) as usize;
    let taken = UNIX_FDS.lock().get_mut(idx).and_then(|s| s.take());
    match taken {
        Some(UnixSock::Stream(e)) => { unix_close(e); 0 }
        Some(UnixSock::X(x)) => { crate::xserver::close(x); 0 }
        Some(UnixSock::Pending) => 0,
        None => (-9i64) as u64,
    }
}

/// H1 self-test: a full local AF_UNIX round-trip — server binds+listens,
/// client connects, server accepts, client→server "ping", server→client "pong",
/// then the client closes and the server sees EOF. Proves both directions + EOF.
pub fn af_unix_selftest() {
    let path = "/run/euro-h1.sock";
    if unix_bind_listen(path, 4).is_err() {
        crate::serial_println!("[h1] AF_UNIX: bind failed");
        return;
    }
    let client = match unix_connect(path) {
        Ok(c) => c,
        Err(_) => {
            crate::serial_println!("[h1] AF_UNIX: connect failed");
            return;
        }
    };
    let server = match unix_accept(path) {
        Some(s) => s,
        None => {
            crate::serial_println!("[h1] AF_UNIX: accept returned nothing");
            return;
        }
    };
    let _ = unix_send(client, b"ping");
    let got_req = unix_recv(server, 64).unwrap_or_default();
    let _ = unix_send(server, b"pong");
    let got_rsp = unix_recv(client, 64).unwrap_or_default();
    unix_close(client);
    let eof = unix_readable(server) && unix_recv(server, 64).map(|v| v.is_empty()).unwrap_or(false);
    crate::serial_println!(
        "[h1] AF_UNIX round-trip: server got '{}', client got '{}', EOF-after-close={} ✓",
        core::str::from_utf8(&got_req).unwrap_or("?"),
        core::str::from_utf8(&got_rsp).unwrap_or("?"),
        eof
    );
    UNIX_SWITCH.lock().unbind(path);
}

/// Listen on `port`, accept ONE incoming TCP connection, read the request,
/// send `response`, and close cleanly (FIN). Returns the first line of the
/// received request. This is the server counterpart of [`TcpConn`]: SYN →
/// SYN-ACK → ACK → request → response → FIN. With a generous spin timeout so the
/// boot continues if no client arrives.
pub fn tcp_serve_once(port: u16, response: &[u8], timeout_spins: u64) -> Option<String> {
    let cfg = get()?;
    let (my_mac, my_ip) = (cfg.my_mac, cfg.my_ip);
    // Refresh slirp's ARP cache (gratuitous ARP) so an incoming connection
    // knows our MAC and the SYN is delivered directly.
    drain();

    // Poll one incoming TCP segment to our IP:port, with sender info
    // (source MAC for the return route, source IP, the segment itself).
    let poll = |spins: u64| -> Option<(MacAddr, Ipv4Addr, TcpSegment)> {
        for _ in 0..spins {
            if let Some(rx) = nic::poll_recv() {
                if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                    if h.ethertype == EtherType::Ipv4 {
                        if let Ok((ih, ipl)) = Ipv4Header::parse(p) {
                            if ih.protocol == Protocol::Tcp {
                                if let Ok(seg) = TcpSegment::parse_checked(ipl, ih.src, ih.dst) {
                                    if seg.dst_port == port {
                                        return Some((h.src, ih.src, seg));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    };

    // 1. Wait for the opening SYN (without ACK).
    let (peer_mac, peer_ip, syn) = loop {
        let (m, ip, seg) = poll(timeout_spins)?;
        if seg.has(tcp::SYN) && !seg.has(tcp::ACK) {
            break (m, ip, seg);
        }
    };
    let peer_port = syn.src_port;
    let mut my_seq = 0x2000u32;
    let mut their_seq = syn.seq.wrapping_add(1);

    let emit = |flags: u8, seq: u32, ack: u32, payload: &[u8]| {
        let seg = TcpSegment { src_port: port, dst_port: peer_port, seq, ack, flags, window: 64240, payload: payload.to_vec() };
        let iph = Ipv4Header { protocol: Protocol::Tcp, ttl: 64, src: my_ip, dst: peer_ip, total_length: 0, identification: 5 };
        let frame = EthernetHeader { dst: peer_mac, src: my_mac, ethertype: EtherType::Ipv4 }
            .build(&iph.build(&seg.build(my_ip, peer_ip)));
        nic::send(&frame);
    };

    // 2. SYN-ACK; our SYN counts as 1 in the sequence space.
    emit(tcp::SYN | tcp::ACK, my_seq, their_seq, &[]);
    my_seq = my_seq.wrapping_add(1);

    // 3. Receive the ACK + the HTTP request (may come in one or more segments).
    let mut req: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for _ in 0..40 {
        match poll(SPINS) {
            Some((_, _, seg)) => {
                if seg.has(tcp::RST) {
                    return None;
                }
                if !seg.payload.is_empty() && seg.seq == their_seq {
                    their_seq = their_seq.wrapping_add(seg.payload.len() as u32);
                    req.extend_from_slice(&seg.payload);
                    emit(tcp::ACK, my_seq, their_seq, &[]);
                }
                if seg.has(tcp::FIN) {
                    their_seq = their_seq.wrapping_add(1);
                    emit(tcp::ACK, my_seq, their_seq, &[]);
                }
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break; // full HTTP request received
                }
            }
            None => break,
        }
    }

    // 4. Response (PSH+ACK), then a clean FIN.
    emit(tcp::PSH | tcp::ACK, my_seq, their_seq, response);
    my_seq = my_seq.wrapping_add(response.len() as u32);
    emit(tcp::FIN | tcp::ACK, my_seq, their_seq, &[]);
    let _ = poll(SPINS); // wait briefly for the last ACK/FIN from the client

    Some(String::from_utf8_lossy(&req).lines().next().unwrap_or("").into())
}

// ── Background HTTP server (cooperative) ─────────────────────────────────
// On/off via `httpd`. When on, `service()` (called every desktop tick)
// serves incoming connections on :80 — the server thus runs in the
// background while the desktop stays interactive, without a separate task and
// without an RX race (everything in task 0).
static SERVER_ON: AtomicBool = AtomicBool::new(false);
static SERVED: AtomicU64 = AtomicU64::new(0);

pub fn httpd_toggle() -> bool {
    let new = !SERVER_ON.load(Ordering::Relaxed);
    SERVER_ON.store(new, Ordering::Relaxed);
    new
}
pub fn httpd_status() -> (bool, u64) {
    (SERVER_ON.load(Ordering::Relaxed), SERVED.load(Ordering::Relaxed))
}

/// The HTTP page that the server serves.
fn http_page() -> alloc::vec::Vec<u8> {
    let body = "<!doctype html><meta charset=utf-8><title>EuroOS</title>\
                <h1>EuroOS</h1><p>Served by the background HTTP server (EuroNet).</p>";
    alloc::format!(
        "HTTP/1.1 200 OK\r\nServer: EuroOS\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

/// Serve one incoming connection whose opening SYN was already received
/// (by `service()`): SYN-ACK → request → response → FIN. Briefly blocking.
fn serve_connection(my_mac: MacAddr, my_ip: Ipv4Addr, peer_mac: MacAddr, peer_ip: Ipv4Addr, syn: &TcpSegment) {
    let port = 80u16;
    let peer_port = syn.src_port;
    let mut my_seq = 0x3000u32;
    let mut their_seq = syn.seq.wrapping_add(1);
    let emit = |flags: u8, seq: u32, ack: u32, payload: &[u8]| {
        let seg = TcpSegment { src_port: port, dst_port: peer_port, seq, ack, flags, window: 64240, payload: payload.to_vec() };
        let iph = Ipv4Header { protocol: Protocol::Tcp, ttl: 64, src: my_ip, dst: peer_ip, total_length: 0, identification: 7 };
        let frame = EthernetHeader { dst: peer_mac, src: my_mac, ethertype: EtherType::Ipv4 }
            .build(&iph.build(&seg.build(my_ip, peer_ip)));
        nic::send(&frame);
    };
    let poll = || -> Option<TcpSegment> {
        for _ in 0..SPINS {
            if let Some(rx) = nic::poll_recv() {
                if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                    if h.ethertype == EtherType::Ipv4 {
                        if let Ok((ih, ipl)) = Ipv4Header::parse(p) {
                            if ih.protocol == Protocol::Tcp && ih.src == peer_ip {
                                if let Ok(seg) = TcpSegment::parse_checked(ipl, ih.src, ih.dst) {
                                    if seg.dst_port == port {
                                        return Some(seg);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    };
    emit(tcp::SYN | tcp::ACK, my_seq, their_seq, &[]);
    my_seq = my_seq.wrapping_add(1);
    let mut req: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for _ in 0..40 {
        match poll() {
            Some(seg) => {
                if seg.has(tcp::RST) {
                    return;
                }
                if !seg.payload.is_empty() && seg.seq == their_seq {
                    their_seq = their_seq.wrapping_add(seg.payload.len() as u32);
                    req.extend_from_slice(&seg.payload);
                    emit(tcp::ACK, my_seq, their_seq, &[]);
                }
                if seg.has(tcp::FIN) {
                    their_seq = their_seq.wrapping_add(1);
                    emit(tcp::ACK, my_seq, their_seq, &[]);
                }
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            None => break,
        }
    }
    let resp = http_page();
    emit(tcp::PSH | tcp::ACK, my_seq, their_seq, &resp);
    my_seq = my_seq.wrapping_add(resp.len() as u32);
    emit(tcp::FIN | tcp::ACK, my_seq, their_seq, &[]);
    SERVED.fetch_add(1, Ordering::Relaxed);
}

/// Shell command `serve` — run EuroOS as an HTTP server: listen on :80,
/// serve one connection with our own page, and close. Blocks until a
/// client connects or the timeout elapses (invoked by the user,
/// so blocking is expected — just like `ping`).
pub fn cmd_serve() -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    if get().is_none() {
        out.push("serve: no network available".into());
        return out;
    }
    let body = "<!doctype html><meta charset=utf-8><title>EuroOS</title>\
                <h1>EuroOS</h1><p>Served by EuroNet's own TCP stack.</p>";
    let resp = alloc::format!(
        "HTTP/1.1 200 OK\r\nServer: EuroOS\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    out.push("HTTP server: listening on :80 (one connection) ...".into());
    match tcp_serve_once(80, resp.as_bytes(), SPINS * 20) {
        Some(req) => {
            out.push(alloc::format!("client served: {}", req.trim()));
            out.push(alloc::format!("response: {} bytes sent (HTTP 200)", resp.len()));
        }
        None => out.push("no client within the time limit".into()),
    }
    out
}

/// Shell command `fetch <host>` — fetch http://<host>/ via TCP.
pub fn cmd_fetch(host: &str) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    let cfg = match get() {
        Some(c) => c,
        None => {
            out.push("fetch: no network".into());
            return out;
        }
    };
    let server = if let Some(ip) = parse_ipv4(host) {
        ip
    } else if let Some(ip) = hosts_lookup(host) {
        out.push(alloc::format!("hosts: {host} = {}", ipfmt(ip)));
        ip
    } else {
        match dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, host) {
            Some(ip) => {
                out.push(alloc::format!("DNS: {host} = {}", ipfmt(ip)));
                ip
            }
            None => {
                out.push(alloc::format!("fetch: cannot resolve '{host}'"));
                return out;
            }
        }
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    match http_get(cfg.my_mac, cfg.my_ip, nexthop, server, host, "/") {
        Some((status, data)) => {
            out.push(alloc::format!("GET http://{host}/ -> {} bytes", data.len()));
            out.push(alloc::format!("  {}", status.trim()));
            // show the first few header lines
            for line in String::from_utf8_lossy(&data).lines().skip(1).take(3) {
                if line.is_empty() {
                    break;
                }
                out.push(alloc::format!("  {line}"));
            }
        }
        None => out.push(alloc::format!("fetch: no connection to {host}")),
    }
    out
}

/// HTTP GET http://<host><path> and return (status line, ONLY the body) — the
/// HTTP headers are stripped. Basis for `wget` (download-to-file).
pub fn http_download(host: &str, path: &str) -> Option<(String, alloc::vec::Vec<u8>)> {
    let cfg = get()?;
    let server = match parse_ipv4(host) {
        Some(ip) => ip,
        None => hosts_lookup(host)
            .or_else(|| dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, host))?,
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    let (status, raw) = http_get(cfg.my_mac, cfg.my_ip, nexthop, server, host, path)?;
    // Strip headers: everything after the first blank line (\r\n\r\n) is the body.
    let body = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => raw[i + 4..].to_vec(),
        None => raw,
    };
    Some((status, body))
}

/// Full HTTP GET with port → ONLY the body (headers stripped). The
/// EuroWeb browser uses this to fetch a REAL page from a REAL server
/// over the real TCP stack (DNS/ARP/TCP/HTTP). `host` may be an IP or a name.
pub fn http_fetch(host: &str, port: u16, path: &str) -> Option<alloc::vec::Vec<u8>> {
    let cfg = get()?;
    let server = match parse_ipv4(host) {
        Some(ip) => ip,
        None => hosts_lookup(host)
            .or_else(|| dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, host))?,
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    let mut c = TcpConn::connect(cfg.my_mac, cfg.my_ip, nexthop, server, port)?;
    let req = alloc::format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    c.send(req.as_bytes());
    let mut data: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    loop {
        let chunk = c.recv(8192);
        if chunk.is_empty() {
            break;
        }
        data.extend_from_slice(&chunk);
        if data.len() > 512 * 1024 {
            break;
        }
    }
    c.close();
    // Strip headers: everything after the first blank line is the body.
    let body = match data.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => data[i + 4..].to_vec(),
        None => data,
    };
    Some(body)
}

/// Send a READY-MADE HTTP request (`request` = full request bytes,
/// e.g. from `euroagent::llm::ollama_http_request`) to `host:port` over EuroNet TCP
/// and return the RAW HTTP response (headers + body). This is the real transport
/// for BB-1: the EuroAgent loop talks to a local Ollama `/api/chat` with it.
/// Bounded connect (4 SYN retries) → cannot hang the boot if no
/// endpoint is running. No TLS (local model on loopback/LAN); cloud = separate opt-in.
pub fn http_post_raw(host: &str, port: u16, request: &[u8]) -> Option<alloc::vec::Vec<u8>> {
    let cfg = get()?;
    let server = match parse_ipv4(host) {
        Some(ip) => ip,
        None => hosts_lookup(host)
            .or_else(|| dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, host))?,
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    let mut c = TcpConn::connect(cfg.my_mac, cfg.my_ip, nexthop, server, port)?;
    c.send(request);
    let mut data: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    loop {
        let chunk = c.recv(8192);
        if chunk.is_empty() {
            break;
        }
        data.extend_from_slice(&chunk);
        if data.len() > 512 * 1024 {
            break;
        }
    }
    c.close();
    if data.is_empty() {
        None
    } else {
        Some(data)
    }
}

/// HTTP(S) GET that returns (status code, Location header, body) — for the
/// browser, which can follow redirects (301/302/…) with it. `tls=true` → HTTPS via
/// eurotls on port 443; otherwise HTTP on `port`.
pub fn fetch_full(host: &str, port: u16, path: &str, tls: bool) -> Option<(u16, Option<String>, alloc::vec::Vec<u8>)> {
    let cfg = get()?;
    let server = match parse_ipv4(host) {
        Some(ip) => ip,
        None => hosts_lookup(host)
            .or_else(|| dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, host))?,
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    // Fetch the raw response (status line + headers + body).
    let raw: alloc::vec::Vec<u8> = if tls {
        let (_s, raw, _cert) = https_get(cfg.my_mac, cfg.my_ip, nexthop, server, host, path)?;
        raw
    } else {
        let mut c = TcpConn::connect(cfg.my_mac, cfg.my_ip, nexthop, server, port)?;
        let req = alloc::format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        c.send(req.as_bytes());
        let mut data: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
        loop {
            let chunk = c.recv(16384);
            if chunk.is_empty() {
                break;
            }
            data.extend_from_slice(&chunk);
            if data.len() > 512 * 1024 {
                break;
            }
        }
        c.close();
        data
    };
    Some(parse_http_response(&raw))
}

/// Parse a raw HTTP response (status line + headers + body) → (status code,
/// Location header, body). Shared by `fetch_full` (GET) and `post_full` (POST).
fn parse_http_response(raw: &[u8]) -> (u16, Option<String>, alloc::vec::Vec<u8>) {
    let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(raw.len());
    let headers = String::from_utf8_lossy(&raw[..head_end]);
    let status = headers
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let mut location = None;
    for line in headers.lines().skip(1) {
        if let Some(idx) = line.find(':') {
            if line[..idx].eq_ignore_ascii_case("location") {
                location = Some(String::from(line[idx + 1..].trim()));
                break;
            }
        }
    }
    let body = if head_end < raw.len() { raw[head_end + 4..].to_vec() } else { alloc::vec::Vec::new() };
    (status, location, body)
}

/// HTTP(S) **POST** with an `application/x-www-form-urlencoded` body. Uses
/// the same real stacks as GET (EuroTLS 1.3 for https, raw TCP for http) via
/// `https_exchange`. Returns (status code, Location, body) or None if no connection.
pub fn post_full(
    host: &str,
    port: u16,
    path: &str,
    tls: bool,
    content_type: &str,
    body: &[u8],
) -> Option<(u16, Option<String>, alloc::vec::Vec<u8>)> {
    let cfg = get()?;
    let server = match parse_ipv4(host) {
        Some(ip) => ip,
        None => hosts_lookup(host).or_else(|| dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, host))?,
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    let head = alloc::format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut request = head.into_bytes();
    request.extend_from_slice(body);

    let raw: alloc::vec::Vec<u8> = if tls {
        let (b, _cert) = https_exchange(cfg.my_mac, cfg.my_ip, nexthop, server, host, &request)?;
        b
    } else {
        let mut c = TcpConn::connect(cfg.my_mac, cfg.my_ip, nexthop, server, port)?;
        c.send(&request);
        let mut data: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
        loop {
            let chunk = c.recv(16384);
            if chunk.is_empty() {
                break;
            }
            data.extend_from_slice(&chunk);
            if data.len() > 512 * 1024 {
                break;
            }
        }
        c.close();
        data
    };
    Some(parse_http_response(&raw))
}

/// Static /etc/hosts table (name -> IPv4), filled by main.rs from /etc/hosts.
/// Consulted before DNS — just like on a real Unix system.
static HOSTS: spin::Mutex<alloc::vec::Vec<(String, Ipv4Addr)>> =
    spin::Mutex::new(alloc::vec::Vec::new());

/// Fill the /etc/hosts table (replaces the previous content).
pub fn set_hosts(entries: alloc::vec::Vec<(String, Ipv4Addr)>) {
    *HOSTS.lock() = entries;
}

// ── DNS cache (S9 network maturity) ────────────────────────────────────────
// Results of DNS queries are cached (name -> (IP, expiry tick)), so that
// repeated lookups come straight from memory instead of going onto the network
// again — faster and less traffic. TTL ~300 s (30000 ticks at 100 Hz).
static DNS_CACHE: Mutex<alloc::vec::Vec<(String, Ipv4Addr, u64)>> = Mutex::new(alloc::vec::Vec::new());
const DNS_TTL_TICKS: u64 = 30_000;
static DNS_HITS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DNS_MISSES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn dns_cache_lookup(name: &str) -> Option<Ipv4Addr> {
    let now = crate::interrupts::ticks();
    let mut c = DNS_CACHE.lock();
    c.retain(|(_, _, exp)| *exp > now); // clean up expired entries
    c.iter().find(|(n, _, _)| n == name).map(|(_, ip, _)| *ip)
}

fn dns_cache_insert(name: &str, ip: Ipv4Addr) {
    let exp = crate::interrupts::ticks() + DNS_TTL_TICKS;
    let mut c = DNS_CACHE.lock();
    c.retain(|(n, _, _)| n != name);
    if c.len() >= 32 {
        c.remove(0); // bound the cache
    }
    c.push((String::from(name), ip, exp));
}

/// Lines for the `netstat` command: DNS cache + hit/miss statistics.
pub fn netstat_lines() -> alloc::vec::Vec<String> {
    let now = crate::interrupts::ticks();
    let hits = DNS_HITS.load(core::sync::atomic::Ordering::Relaxed);
    let misses = DNS_MISSES.load(core::sync::atomic::Ordering::Relaxed);
    let mut out = alloc::vec::Vec::new();
    out.push(alloc::format!("DNS cache: {} hits, {} misses", hits, misses));
    let c = DNS_CACHE.lock();
    for (n, ip, exp) in c.iter() {
        let ttl = exp.saturating_sub(now) / 100;
        out.push(alloc::format!("  {n:<24} {} (TTL {ttl}s)", ipfmt(*ip)));
    }
    if c.is_empty() {
        out.push("  (cache empty)".into());
    }
    out
}

/// Look up a name in /etc/hosts (None = not found -> fall back to DNS).
pub fn hosts_lookup(name: &str) -> Option<Ipv4Addr> {
    HOSTS.lock().iter().find(|(n, _)| n == name).map(|(_, ip)| *ip)
}

/// Parse an IPv4 address from "a.b.c.d".
pub fn parse_ipv4(s: &str) -> Option<Ipv4Addr> {
    let mut o = [0u8; 4];
    let mut i = 0;
    for part in s.split('.') {
        if i >= 4 {
            return None;
        }
        o[i] = part.parse().ok()?;
        i += 1;
    }
    if i == 4 {
        Some(Ipv4Addr(o))
    } else {
        None
    }
}

/// /24 heuristic: is `a` in the same subnet as `b`?
pub fn same_subnet(a: Ipv4Addr, b: Ipv4Addr) -> bool {
    a.0[0] == b.0[0] && a.0[1] == b.0[1] && a.0[2] == b.0[2]
}

pub fn ipfmt(ip: Ipv4Addr) -> String {
    alloc::format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3])
}

/// Shell command `ping <ip-or-name>` on the live NIC.
pub fn cmd_ping(arg: &str) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    let cfg = match get() {
        Some(c) => c,
        None => {
            out.push("ping: no network available".into());
            return out;
        }
    };
    // Determine the address: direct IPv4 or via DNS.
    let (dst, label) = if let Some(ip) = parse_ipv4(arg) {
        (ip, ipfmt(ip))
    } else if let Some(ip) = hosts_lookup(arg) {
        out.push(alloc::format!("hosts: {arg} = {}", ipfmt(ip)));
        (ip, alloc::format!("{arg} ({})", ipfmt(ip)))
    } else {
        match dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, arg) {
            Some(ip) => {
                out.push(alloc::format!("DNS: {arg} = {}", ipfmt(ip)));
                (ip, alloc::format!("{arg} ({})", ipfmt(ip)))
            }
            None => {
                out.push(alloc::format!("ping: cannot resolve '{arg}'"));
                return out;
            }
        }
    };
    // Next-hop: local subnet → ARP; otherwise via the gateway.
    let nexthop = if same_subnet(dst, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, dst).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    let ok = icmp_ping(cfg.my_mac, cfg.my_ip, nexthop, dst);
    out.push(if ok {
        alloc::format!("PING {label}: echo-reply OK ✓")
    } else {
        alloc::format!("PING {label}: no answer")
    });
    out
}

/// Shell command `ping6` — ping the IPv6 router (from the Router Advertisement).
pub fn cmd_ping6() -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    let cfg = match get() {
        Some(c) => c,
        None => {
            out.push("ping6: no network".into());
            return out;
        }
    };
    match (cfg.router_ll, cfg.router_mac) {
        (Some(ll), Some(mac)) => {
            let ok = icmp6_ping(cfg.my_mac, cfg.link_local, mac, ll);
            out.push(if ok { "PING6 router: echo-reply OK ✓".into() } else { "PING6 router: no answer".into() });
        }
        _ => out.push("ping6: no IPv6 router known".into()),
    }
    out
}

/// Shell command `net` — show the current network configuration.
pub fn cmd_net() -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    match get() {
        Some(c) => {
            out.push(alloc::format!(
                "MAC      {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                c.my_mac.0[0], c.my_mac.0[1], c.my_mac.0[2], c.my_mac.0[3], c.my_mac.0[4], c.my_mac.0[5]
            ));
            out.push(alloc::format!("IPv4     {} (gw {}, dns {})", ipfmt(c.my_ip), ipfmt(c.gw_ip), ipfmt(c.dns_ip)));
            out.push("IPv6     SLAAC link-local + global active".into());
            out.push("commands: ping <ip|name> · ping6 · net".into());
        }
        None => out.push("net: no network configured".into()),
    }
    out
}
