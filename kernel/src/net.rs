//! Herbruikbare netwerk-operaties bovenop de virtio-net NIC + EuroNet:
//! ARP-resolutie, ICMP-ping, DNS-lookup, ICMPv6-ping. Plus de bewaarde
//! netwerkconfiguratie (na de boot-bring-up) zodat de shell `ping`/`dns`/`net`
//! kan aanbieden op de live NIC.

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

use crate::virtio_net;

/// De netwerkconfiguratie zoals ontdekt tijdens de boot-bring-up.
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

/// Recycle wachtende RX-buffers (anders raken de 16 buffers vol met idle
/// multicast-verkeer) ÉN kondig ons IP→MAC aan met een gratuitous ARP, zodat
/// slirp's ARP-cache vers blijft en IPv4-antwoorden binnenkomen.
fn drain() {
    for _ in 0..64 {
        if virtio_net::poll_recv().is_none() {
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
        virtio_net::send(&frame);
    }
}

/// ARP: vraag het MAC-adres van `ip` (in ons subnet).
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
    virtio_net::send(&frame);
    for _ in 0..SPINS {
        if let Some(rx) = virtio_net::poll_recv() {
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

/// ICMP echo (ping) naar `dst` via de gegeven next-hop-MAC. true = reply ontvangen.
pub fn icmp_ping(my_mac: MacAddr, my_ip: Ipv4Addr, nexthop: MacAddr, dst: Ipv4Addr) -> bool {
    drain();
    let icmp = IcmpEcho { kind: IcmpType::EchoRequest, identifier: 0xE401, sequence: 1, payload: b"euroos-ping".to_vec() };
    let iph = Ipv4Header { protocol: Protocol::Icmp, ttl: 64, src: my_ip, dst, total_length: 0, identification: 1 };
    let frame = EthernetHeader { dst: nexthop, src: my_mac, ethertype: EtherType::Ipv4 }.build(&iph.build(&icmp.build()));
    virtio_net::send(&frame);
    for _ in 0..SPINS * 2 {
        if let Some(rx) = virtio_net::poll_recv() {
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

/// DNS A-record-lookup van `name` via de DNS-server. Geeft het eerste IPv4-adres.
pub fn dns_query(my_mac: MacAddr, my_ip: Ipv4Addr, dns_mac: MacAddr, dns_ip: Ipv4Addr, name: &str) -> Option<Ipv4Addr> {
    // S9 DNS-cache: eerst de cache raadplegen (geen netwerkronde bij een hit).
    if let Some(ip) = dns_cache_lookup(name) {
        DNS_HITS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        crate::serial_println!("[dns] cache-hit: {name} = {}", ipfmt(ip));
        return Some(ip);
    }
    DNS_MISSES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    drain();
    // Variërende transaction-ID ÉN bronpoort (uit de HPET-teller; RFC 5452): een
    // spoofer moet nu zowel de 16-bits txid als de ~14-bits ephemere poort raden,
    // niet enkel de txid. Beide worden op het antwoord gevalideerd.
    // Hardware-entropie (RDRAND) voor zowel txid als bronpoort i.p.v. de louter
    // voorspelbare HPET-teller — een spoofer moet beide blind raden.
    let r = rand_u64();
    let txid = r as u16;
    let src_port = 49152 + ((r >> 16) as u16 & 0x3FFF); // ephemere poort 49152-65535
    let q = dns::build_query(txid, name);
    let seg = UdpDatagram { src_port, dst_port: 53, payload: q }.build(my_ip, dns_ip);
    let iph = Ipv4Header { protocol: Protocol::Udp, ttl: 64, src: my_ip, dst: dns_ip, total_length: 0, identification: 2 };
    let frame = EthernetHeader { dst: dns_mac, src: my_mac, ethertype: EtherType::Ipv4 }.build(&iph.build(&seg));
    virtio_net::send(&frame);
    for _ in 0..SPINS * 2 {
        if let Some(rx) = virtio_net::poll_recv() {
            if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                if h.ethertype == EtherType::Ipv4 {
                    if let Ok((ih, ipl)) = Ipv4Header::parse(p) {
                        // Antwoord moet van poort 53 KOMEN én naar ONZE bronpoort (50000)
                        // gaan; en parse_response valideert de transaction-ID + QR-bit.
                        if ih.protocol == Protocol::Udp
                            && ipl.len() > 8
                            && u16::from_be_bytes([ipl[0], ipl[1]]) == 53
                            && u16::from_be_bytes([ipl[2], ipl[3]]) == src_port
                        {
                            if let Some(ip) = dns::parse_response(&ipl[8..], txid).first() {
                                dns_cache_insert(name, *ip); // S9: resultaat cachen
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

/// ICMPv6 echo (ping6) naar `dst` via de gegeven MAC.
pub fn icmp6_ping(my_mac: MacAddr, src_ll: Ipv6Addr, dst_mac: MacAddr, dst: Ipv6Addr) -> bool {
    drain();
    let echo = icmpv6::echo_request(0xE6, 1, b"euroos-ping6", src_ll, dst);
    let eh = Ipv6Header { next_header: 58, hop_limit: 255, src: src_ll, dst, payload_len: 0 };
    let frame = EthernetHeader { dst: dst_mac, src: my_mac, ethertype: EtherType::Ipv6 }.build(&eh.build(&echo));
    virtio_net::send(&frame);
    for _ in 0..SPINS * 2 {
        if let Some(rx) = virtio_net::poll_recv() {
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

/// Netwerk-service: poll wat binnenkomend verkeer en BEANTWOORD ARP-requests voor
/// ons IP (anders verloopt slirp's ARP-cache en komen IPv4-antwoorden nooit aan).
/// Wordt elke desktop-lus-iteratie aangeroepen zodat we bereikbaar blijven.
/// True als het in een ICMP-fout teruggekaatste datagram óns eigen UDP-pakket is
/// (de bron/doel-poorten in de meegestuurde UDP-header komen overeen). `orig` is
/// het oorspronkelijke IP-datagram (IP-header + begin van de UDP-header).
fn embedded_udp_matches(orig: &[u8], sport: u16, dport: u16) -> bool {
    if orig.len() < 20 {
        return false;
    }
    let ihl = (orig[0] & 0x0F) as usize * 4;
    // protocol UDP = 17, en er moeten 4 bytes UDP-header (src+dst-poort) zijn.
    if orig[9] != 17 || orig.len() < ihl + 4 {
        return false;
    }
    let s = u16::from_be_bytes([orig[ihl], orig[ihl + 1]]);
    let d = u16::from_be_bytes([orig[ihl + 2], orig[ihl + 3]]);
    s == sport && d == dport
}

/// Verstuur één IPv4-pakket met `l4`-payload naar een directe L2-buur. Gebruikt
/// door `service()` voor ICMP-antwoorden (echo-reply, poort-onbereikbaar).
fn send_ipv4(my_mac: MacAddr, my_ip: Ipv4Addr, dst_mac: MacAddr, dst_ip: Ipv4Addr, proto: Protocol, l4: &[u8]) {
    let iph = Ipv4Header { protocol: proto, ttl: 64, src: my_ip, dst: dst_ip, total_length: 0, identification: 9 };
    let frame = EthernetHeader { dst: dst_mac, src: my_mac, ethertype: EtherType::Ipv4 }.build(&iph.build(l4));
    virtio_net::send(&frame);
}

/// Snelheidsbegrenzer voor uitgaande ICMP/RST-foutmeldingen (anti-amplificatie):
/// max 20 per seconde, 20-burst. Voorkomt dat een aanvaller met vervalste bron-
/// IP's onze poort-onbereikbaar/RST-antwoorden naar slachtoffers laat reflecteren.
static ICMP_ERR_LIMIT: Mutex<TokenBucket> = Mutex::new(TokenBucket::new(20, 20, 100));
fn icmp_err_allowed() -> bool {
    ICMP_ERR_LIMIT.lock().allow(crate::interrupts::ticks())
}

/// Is er een userspace LISTEN-socket op `seg.dst_port` met ruimte in de wachtrij?
/// Zo ja: voltooi de passieve open (SYN-ACK) en zet de verbinding in de wachtrij.
/// Geeft `true` als er een luisteraar bestond (zodat de beller geen RST stuurt).
fn try_accept_listener(seg: &TcpSegment, peer_mac: MacAddr, peer_ip: Ipv4Addr, cfg: &NetCfg) -> bool {
    let has_room = {
        let t = SOCKETS.lock();
        t.iter().any(|s| matches!(s, Some(Sock::Listen { port, backlog, queue }) if *port == seg.dst_port && queue.len() < *backlog))
    };
    if !has_room {
        return false;
    }
    // Passieve open zónder de SOCKETS-lock vast te houden (doet netwerk-I/O).
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
        let rx = match virtio_net::poll_recv() {
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
                        virtio_net::send(&frame);
                    }
                }
            } else if h.ethertype == EtherType::Ipv4 {
                if let Ok((ih, ipl)) = Ipv4Header::parse(payload) {
                    // Alleen verkeer dat écht aan ons gericht is.
                    if ih.dst != cfg.my_ip {
                        continue;
                    }
                    // N3: packet-filter (EuroFW). Toets het inkomende pakket; een
                    // geblokkeerd pakket wordt stil gedropt (stealth, geen RST/ICMP).
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
                                    // Achtergrond-HTTP-server: bedien een inkomende SYN op :80.
                                    serve_connection(cfg.my_mac, cfg.my_ip, h.src, ih.src, &seg);
                                } else if is_syn && try_accept_listener(&seg, h.src, ih.src, &cfg) {
                                    // Een userspace LISTEN-socket op deze poort heeft de
                                    // verbinding aangenomen (passieve open + in de wachtrij).
                                } else if is_syn && icmp_err_allowed() {
                                    // SYN naar een gesloten poort → "connection refused": RST
                                    // (RFC 793), i.p.v. het pakket te laten verdwijnen. Begrensd.
                                    if let Some(rst) = TcpSegment::reset_to(&seg) {
                                        send_ipv4(cfg.my_mac, cfg.my_ip, h.src, ih.src, Protocol::Tcp, &rst.build(cfg.my_ip, ih.src));
                                    }
                                }
                            }
                        }
                        // Maak EuroOS ping-baar: beantwoord een inkomende echo-request.
                        Protocol::Icmp => {
                            if let Ok(echo) = IcmpEcho::parse(ipl) {
                                if echo.kind == IcmpType::EchoRequest {
                                    let reply = IcmpEcho::reply_to(&echo);
                                    send_ipv4(cfg.my_mac, cfg.my_ip, h.src, ih.src, Protocol::Icmp, &reply.build());
                                }
                            }
                        }
                        // Ongevraagd UDP-datagram = geen luisteraar → ICMP poort-onbereikbaar
                        // (RFC 792). Tijdens een blokkerende DNS/ping draait service() niet, dus
                        // wat hier binnenkomt wordt door niemand verwacht.
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

/// Minimale TCP-client: HTTP/1.0 GET. Doet de 3-way handshake, stuurt het
/// verzoek, ontvangt het antwoord (ACK't elk segment) en sluit netjes af.
/// Geeft (statusregel, ruwe respons-bytes) terug.
/// Volgende vrije efemere bronpoort (49152–65535), zodat gelijktijdige
/// verbindingen niet botsen.
static NEXT_PORT: Mutex<u16> = Mutex::new(49152);
fn alloc_port() -> u16 {
    let mut p = NEXT_PORT.lock();
    let port = *p;
    *p = if port >= 65000 { 49152 } else { port + 1 };
    port
}

/// Een actieve TCP-clientverbinding: handshake, send, recv, teardown.
/// Synchroon (poll-gebaseerd) — past in onze niet-preëmptieve ring-3-runs.
/// Dit is het object achter de socket-syscalls én achter `http_get`.
pub struct TcpConn {
    my_mac: MacAddr,
    my_ip: Ipv4Addr,
    nexthop: MacAddr,
    server: Ipv4Addr,
    dport: u16,
    sport: u16,
    my_seq: u32,
    their_seq: u32,
    /// Oudste nog niet ge-ACK'te seq (snd.una): tot hier heeft de peer bevestigd.
    snd_una: u32,
    /// Verbinding nog bruikbaar (geen FIN/RST van de peer gezien)?
    pub open: bool,
    /// In-order ontvangen, nog niet door recv() opgehaalde bytes.
    rx: alloc::collections::VecDeque<u8>,
    /// Verzonden-maar-nog-niet-ge-ACK'te datasegmenten (seq, bytes) voor
    /// retransmissie bij pakketverlies.
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
        virtio_net::send(&frame);
    }

    /// Wacht op het volgende TCP-segment van onze peer voor deze poort.
    fn poll_seg(&self) -> Option<TcpSegment> {
        for _ in 0..SPINS * 3 {
            if let Some(rx) = virtio_net::poll_recv() {
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

    /// 3-way handshake. Geeft een verbonden socket terug, of None bij timeout.
    pub fn connect(my_mac: MacAddr, my_ip: Ipv4Addr, nexthop: MacAddr, server: Ipv4Addr, dport: u16) -> Option<TcpConn> {
        drain();
        // Gerandomiseerde ISN (RFC 6528) — een vaste ISN zou een off-path-aanvaller
        // toelaten sequentienummers te raden en RST/data in de verbinding te spuiten.
        // (Server-zijde `accept_from` doet dit al; nu ook de client.)
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
        // SYN met retransmissie: een verloren SYN of SYN-ACK laat de handshake
        // anders meteen mislukken. Maximaal 4 pogingen (poll_seg time-out per ronde).
        let mut synack = None;
        for _ in 0..4 {
            c.emit(tcp::SYN, &[]); // (re)transmit de SYN
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

    /// Passieve open (server-kant): we ontvingen een SYN; stuur SYN-ACK en wacht
    /// op de afsluitende ACK. Geeft een verbonden server-socket terug, of None.
    /// `local_port` = de luisterpoort, `syn` = het binnengekomen SYN-segment.
    pub fn accept_from(
        my_mac: MacAddr,
        my_ip: Ipv4Addr,
        peer_mac: MacAddr,
        peer_ip: Ipv4Addr,
        peer_port: u16,
        local_port: u16,
        syn: &TcpSegment,
    ) -> Option<TcpConn> {
        let isn = (rand_u64() as u32) | 1; // gerandomiseerde ISN
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
                        // Een meegestuurd verzoek (data op de ACK) meteen bufferen.
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

    /// Verwerk alle direct beschikbare segmenten: buffer in-order payload,
    /// ACK wat we ontvangen, en verwerk FIN/RST. Niet-blokkerend per ronde,
    /// maar poll_seg wacht zelf tot er iets binnenkomt of de timeout verstrijkt.
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
            // Cumulatieve ACK van de peer: schuif snd.una vooruit en laat
            // bevestigde segmenten uit de retransmissie-buffer vallen.
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

    /// Schuif snd.una vooruit naar `ack` (wrapping-bewust) en verwijder volledig
    /// bevestigde segmenten uit de retransmissie-buffer.
    fn ack_upto(&mut self, ack: u32) {
        // `ack` ligt vóór snd.una als het verschil in de bovenste helft van de
        // seq-ruimte valt (oud/duplicaat) — dan negeren.
        if ack.wrapping_sub(self.snd_una) < 0x8000_0000 && ack != self.snd_una {
            self.snd_una = ack;
        }
        self.retx.retain(|(seq, data)| {
            let end = seq.wrapping_add(data.len() as u32);
            // Behoud zolang het einde van het segment nog ná snd.una ligt.
            end.wrapping_sub(self.snd_una) != 0 && end.wrapping_sub(self.snd_una) < 0x8000_0000
        });
    }

    /// Verstuur applicatiedata (PSH+ACK) betrouwbaar: elk segment komt in de
    /// retransmissie-buffer; daarna pompen we op ACK's en retransmitteren we
    /// onbevestigde segmenten (bounded) — zo overleeft de verbinding pakketverlies.
    pub fn send(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // QEMU-MTU-veilige segmentgrootte.
        for chunk in data.chunks(1024) {
            self.emit(tcp::PSH | tcp::ACK, chunk);
            self.retx.push((self.my_seq, chunk.to_vec()));
            self.my_seq = self.my_seq.wrapping_add(chunk.len() as u32);
        }
        // Wacht op bevestiging; retransmit onbevestigde segmenten (max 5 rondes).
        for _ in 0..5 {
            self.pump(2);
            if self.retx.is_empty() || !self.open {
                break;
            }
            // Time-out op deze ronde: retransmit alles wat nog open staat.
            for (seq, chunk) in self.retx.clone() {
                let saved = self.my_seq;
                self.my_seq = seq;
                self.emit(tcp::PSH | tcp::ACK, &chunk);
                self.my_seq = saved;
            }
        }
    }

    /// Lees tot `max` bytes. Wacht zo nodig tot er iets binnenkomt of de peer
    /// sluit. Geeft een lege Vec bij een afgesloten, lege verbinding.
    pub fn recv(&mut self, max: usize) -> alloc::vec::Vec<u8> {
        let mut tries = 0;
        while self.rx.is_empty() && self.open && tries < 80 {
            self.pump(8);
            tries += 1;
        }
        let n = max.min(self.rx.len());
        self.rx.drain(..n).collect()
    }

    /// Stuur een FIN en sluit de verbinding netjes af.
    pub fn close(&mut self) {
        if self.open {
            self.emit(tcp::FIN | tcp::ACK, &[]);
            self.my_seq = self.my_seq.wrapping_add(1);
            self.open = false;
            // Wacht kort op de FIN/ACK van de peer.
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

// ── Entropie voor TLS (sleutels + nonces) ─────────────────────────────────
// RDRAND indien aanwezig (CPUID), anders een functionele mix van RDTSC +
// timer-ticks + een teller, alles door SHA-256. NB: de RDTSC-fallback is
// FUNCTIONEEL maar niet cryptografisch sterk — productie gebruikt RDRAND/TPM
// (zie de Hardware-security-track). Voldoende om de handshake echt te voltooien.
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

/// 64 bits onvoorspelbaarheid: hardware-RDRAND indien beschikbaar, anders een
/// mix van RDTSC + HPET + een lopende teller. Voor txid/bronpoort-randomisatie
/// (defence-in-depth tegen DNS-spoofing — RFC 5452).
pub fn rand_u64() -> u64 {
    // RDRAND alleen aanroepen als de CPU het adverteert — anders is het opcode
    // ongeldig (#UD). Op qemu64/TCG ontbreekt RDRAND; dan de fallback-mix.
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

/// 32 willekeurige bytes voor een TLS-sleutel/nonce. `domain` scheidt
/// onafhankelijke trekkingen binnen één handshake.
fn gather_entropy(domain: u8) -> [u8; 32] {
    let mut seed: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    seed.push(domain);
    // ★ Audit H1: meng de hardware-RNG van de TPM in. Op TCG/QEMU ontbreekt RDRAND,
    // waardoor de oude fallback (RDTSC^HPET^teller) laag-entropisch en deels
    // voorspelbaar was — dodelijk voor een efemere X25519-sleutel. De TPM-RNG
    // (zelfde bron als FDE/VPN/CA) is de sterke entropiebron wanneer ze aanwezig is.
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

/// HTTPS GET via onze eigen TLS 1.3-stack bovenop TcpConn. Geeft
/// (statusregel, body, certificaatlengte) terug.
pub fn https_get(
    my_mac: MacAddr,
    my_ip: Ipv4Addr,
    nexthop: MacAddr,
    server: Ipv4Addr,
    host: &str,
    path: &str,
) -> Option<(String, alloc::vec::Vec<u8>, Option<alloc::vec::Vec<u8>>)> {
    let mut tcp = TcpConn::connect(my_mac, my_ip, nexthop, server, 443)?;
    let random = gather_entropy(0);
    let secret = gather_entropy(1);
    let (mut tls, hello) = eurotls::Tls13Client::new(host, random, secret);
    // Schakel certificaatvalidatie in: verifieer de keten tegen de gebundelde
    // EU/internationale root-CA's + de servernaam + het geldigheidsvenster, en
    // controleer de CertificateVerify-handtekening. Een MITM met een vals (maar
    // op zichzelf geldig) certificaat wordt zo geweigerd.
    tls.set_trust_anchor(crate::rtc::epoch() as i64, crate::tls_roots::ROOTS);
    tcp.send(&hello);

    // Handshake: voer serverrecords in tot de verbinding staat.
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
                    crate::kwarn!("[tls] handshake afgebroken: {why} ({} certs in keten)", tls.cert_chain_info().len());
                }
                return None;
            }
        }
    }

    // Versleuteld HTTP-verzoek.
    let req = alloc::format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if let Ok(rec) = tls.encrypt_app(req.as_bytes()) {
        tcp.send(&rec);
    }

    // Ontvang + ontsleutel het antwoord tot close_notify/timeout.
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
                body.extend_from_slice(&tls.take_app_data()); // close_notify = nette EOF
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
    let status = String::from_utf8_lossy(&body).lines().next().unwrap_or("").into();
    Some((status, body, cert))
}

/// Shell-commando `https <host>` — haal https://<host>/ op via EuroTLS 1.3.
pub fn cmd_https(host: &str) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    let cfg = match get() {
        Some(c) => c,
        None => {
            out.push("https: geen netwerk".into());
            return out;
        }
    };
    let server = match resolve(host) {
        Some(ip) => {
            out.push(alloc::format!("DNS: {host} = {}", ipfmt(ip)));
            ip
        }
        None => {
            out.push(alloc::format!("https: kan '{host}' niet resolven"));
            return out;
        }
    };
    let nexthop = if same_subnet(server, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    out.push("TLS 1.3-handshake (X25519 + ChaCha20-Poly1305) ...".into());
    match https_get(cfg.my_mac, cfg.my_ip, nexthop, server, host, "/") {
        Some((status, data, cert)) => {
            out.push(alloc::format!("GET https://{host}/ -> {} bytes (versleuteld)", data.len()));
            out.push(alloc::format!("  {}", status.trim()));
            if let Some(c) = cert {
                let fp = eurotls::keyschedule::sha256(&c);
                let mut hexs = String::new();
                for b in &fp[..8] {
                    hexs.push_str(&alloc::format!("{b:02x}"));
                }
                out.push(alloc::format!("  servercertificaat: {} bytes, SHA-256 {}…", c.len(), hexs));
            }
        }
        None => out.push(alloc::format!("https: TLS-handshake met {host} mislukt")),
    }
    out
}

// ── Socket-laag voor userspace ────────────────────────────────────────────
// Een echte BSD-achtige socket-API achter de Linux-syscalls socket/connect/
// send/recv, gekoppeld aan TcpConn. Userspace fd's voor sockets liggen vanaf
// SOCK_FD_BASE zodat ze niet botsen met de (kleine) VFS-fd's.

/// Eerste fd-nummer dat een socket voorstelt (ruim boven de VFS-fd's).
pub const SOCK_FD_BASE: u64 = 500;
const MAX_SOCK: usize = 16;

/// Een verbindingsloze UDP-socket: na connect() onthouden we de bestemming en
/// versturen/ontvangen we datagrammen op onze efemere bronpoort.
pub struct UdpSock {
    my_mac: MacAddr,
    my_ip: Ipv4Addr,
    nexthop: MacAddr,
    server: Ipv4Addr,
    dport: u16,
    sport: u16,
}

impl UdpSock {
    /// Verstuur één datagram naar de verbonden bestemming.
    pub fn send(&self, data: &[u8]) {
        let dg = UdpDatagram { src_port: self.sport, dst_port: self.dport, payload: data.to_vec() };
        let iph = Ipv4Header { protocol: Protocol::Udp, ttl: 64, src: self.my_ip, dst: self.server, total_length: 0, identification: 4 };
        let frame = EthernetHeader { dst: self.nexthop, src: self.my_mac, ethertype: EtherType::Ipv4 }
            .build(&iph.build(&dg.build(self.my_ip, self.server)));
        virtio_net::send(&frame);
    }

    /// Wacht op één datagram van de bestemming, terug op onze bronpoort.
    pub fn recv(&self) -> alloc::vec::Vec<u8> {
        for _ in 0..SPINS * 3 {
            if let Some(rx) = virtio_net::poll_recv() {
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
                                // Een ICMP-fout van de bestemming die óns datagram terugkaatst
                                // (poort/host onbereikbaar) → snel falen i.p.v. uittimen.
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
    /// socket() gedaan, connect()/bind() nog niet. `dgram` = SOCK_DGRAM (UDP).
    /// `bind_port` = via bind() vastgelegde lokale poort (0 = nog niet).
    Reserved { dgram: bool, bind_port: u16 },
    /// Verbonden TCP-socket.
    Conn(TcpConn),
    /// "Verbonden" UDP-socket (bestemming onthouden).
    Udp(UdpSock),
    /// Luisterende TCP-socket: passieve open op `port`, met een accept-wachtrij
    /// (begrensd door `backlog`) van al voltooide verbindingen.
    Listen { port: u16, backlog: usize, queue: alloc::collections::VecDeque<TcpConn> },
}

static SOCKETS: Mutex<[Option<Sock>; MAX_SOCK]> = Mutex::new([const { None }; MAX_SOCK]);

/// Is dit fd-nummer een socket (niet een VFS-bestand)?
pub fn is_sock_fd(fd: u64) -> bool {
    fd >= SOCK_FD_BASE && (fd - SOCK_FD_BASE) < MAX_SOCK as u64
}

/// socket(AF_INET, SOCK_STREAM): reserveer een slot, geef het fd-nummer terug.
/// -1 als de tabel vol is.
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

/// connect(fd, ip, port): voor TCP een 3-way handshake, voor UDP enkel de
/// bestemming onthouden. 0 / -1.
pub fn sock_connect(fd: u64, server: Ipv4Addr, port: u16) -> u64 {
    if !is_sock_fd(fd) {
        return (-1i64) as u64;
    }
    // EuroGuard (Track 7, Fase 7.1): laat de policy-engine deze uitgaande
    // verbinding beoordelen VÓÓR er een pakket vertrekt. Een geblokkeerde app
    // krijgt -EPERM en het verzoek wordt geaudit — een harde policy-grens.
    let app = crate::ring3::current_app();
    if crate::euroguard::check_connect(&app, server, port) == crate::euroguard::Decision::Block {
        return (-1i64) as u64; // -EPERM: door EuroGuard geweigerd
    }
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
    // Bepaal het sockettype zonder de lock vast te houden tijdens connect().
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

/// Shell-demo `tcpserve <port>`: open een server-socket, luister, accepteer één
/// verbinding, lees het verzoek en stuur een antwoord. Bewijst de listen/accept-
/// keten (passieve open) met een externe client.
pub fn cmd_tcpserve(port: u16) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    if get().is_none() {
        out.push("tcpserve: geen netwerk".into());
        return out;
    }
    let fd = sock_open(false);
    if (fd as i64) < 0 {
        out.push("tcpserve: socket() faalde".into());
        return out;
    }
    if (sock_bind(fd, port) as i64) < 0 || (sock_listen(fd, 4) as i64) < 0 {
        sock_close(fd);
        out.push("tcpserve: bind/listen faalde".into());
        return out;
    }
    out.push(alloc::format!("tcpserve: LISTEN op :{port}, wacht op een verbinding..."));
    let cfd = sock_accept(fd);
    if (cfd as i64) < 0 {
        sock_close(fd);
        out.push("tcpserve: accept() time-out (geen client verbond)".into());
        return out;
    }
    out.push("tcpserve: verbinding AANGENOMEN ✓".into());
    let req = sock_recv(cfd, 512);
    let line = String::from_utf8_lossy(&req);
    out.push(alloc::format!("  client stuurde: {:?}", line.lines().next().unwrap_or("")));
    let body = "EuroOS server-socket: hallo, accept() werkt!\n";
    let resp = alloc::format!(
        "HTTP/1.1 200 OK\r\nServer: EuroOS\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    sock_send(cfd, resp.as_bytes());
    sock_close(cfd);
    sock_close(fd);
    out.push("tcpserve: antwoord verzonden + gesloten ✓".into());
    out
}

/// bind(fd, port): leg de lokale poort vast voor een (stream-)socket. 0/-1.
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

/// listen(fd, backlog): zet een gebonden stream-socket in LISTEN. 0/-1.
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

/// accept(fd): haal de volgende voltooide verbinding uit de accept-wachtrij en
/// geef een nieuw socket-fd terug. Blokkeert (begrensd) door `service()` te
/// pompen tot er een verbinding arriveert. -1 bij time-out of geen luisteraar.
pub fn sock_accept(fd: u64) -> u64 {
    if !is_sock_fd(fd) {
        return (-1i64) as u64;
    }
    let i = (fd - SOCK_FD_BASE) as usize;
    // Blokkeer tot een verbinding arriveert of de deadline verstrijkt. De deadline
    // is in scheduler-ticks (100 Hz gast-tijd); `service()` is niet-blokkerend, dus
    // we pompen tot de klok de bovengrens bereikt.
    let deadline = crate::interrupts::ticks() + 400; // ~4 s gast-tijd bovengrens
    while crate::interrupts::ticks() < deadline {
        // Pop een klaarstaande verbinding (lock kort vasthouden).
        let conn = {
            let mut t = SOCKETS.lock();
            match &mut t[i] {
                Some(Sock::Listen { queue, .. }) => queue.pop_front(),
                _ => return (-1i64) as u64, // geen luisterende socket
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
            return (-1i64) as u64; // sockettabel vol
        }
        // Geen verbinding klaar: verwerk inkomende pakketten (kan er één voltooien).
        service();
    }
    (-1i64) as u64
}

/// Resolve een hostnaam (of "a.b.c.d") naar een IPv4-adres via DNS.
pub fn resolve(host: &str) -> Option<Ipv4Addr> {
    if let Some(ip) = parse_ipv4(host) {
        return Some(ip);
    }
    if let Some(ip) = hosts_lookup(host) {
        return Some(ip); // /etc/hosts vóór DNS
    }
    let cfg = get()?;
    dns_query(cfg.my_mac, cfg.my_ip, cfg.dns_mac, cfg.dns_ip, host)
}

/// send(fd, data): aantal verzonden bytes, of -1.
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
                // EuroGuard DNS-niveau-filtering: inspecteer queries naar poort 53
                // en blokkeer trackers/ads vóór de query het netwerk op gaat.
                if u.dport == 53 {
                    if let Some(name) = dns::parse_query_name(data) {
                        if crate::euroguard::check_dns(&crate::ring3::current_app(), &name)
                            == crate::euroguard::Decision::Block
                        {
                            // Geblokkeerd: niets versturen; de app krijgt geen antwoord.
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
    // EuroGuard-statistiek (Fase 7.4): verzonden bytes per app.
    crate::euroguard::record_bytes(&crate::ring3::current_app(), sent, 0);
    sent
}

/// recv(fd, max): kopieer tot `max` ontvangen bytes naar `out`; geef het aantal
/// terug (0 = verbinding gesloten en leeg).
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
    // EuroGuard-statistiek (Fase 7.4): ontvangen bytes per app.
    crate::euroguard::record_bytes(&crate::ring3::current_app(), 0, data.len() as u64);
    data
}

/// close(fd): FIN + geef het slot vrij. 0.
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

/// G3-zelftest: poll een verse listener + UDP-socket. Een listener met een lege
/// accept-queue hoort NIET leesbaar te zijn — bewijst de gereedheids-logica.
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
    let deadline = crate::interrupts::ticks() + 2; // korte deadline (geen client)
    let ready = sock_poll(&[lfd, ufd], deadline);
    let listener_readable = ready.iter().find(|(f, _)| *f == lfd).map(|(_, r)| *r).unwrap_or(true);
    crate::serial_println!(
        "[g3] poll/select: {} fd's gemultiplexed — listener leesbaar={} (verse queue leeg → verwacht false)",
        ready.len(),
        listener_readable
    );
    sock_close(lfd);
    sock_close(ufd);
}

/// poll/select (G3): niet-blokkerende gereedheids-check voor een set socket-fd's,
/// zodat één taak meerdere verbindingen/listeners kan multiplexen. Drijft `service()`
/// (vult Listen-accept-queues) en pompt Conn-sockets éénmaal, en rapporteert per fd
/// of die LEESBAAR is (Conn: data of EOF; Listen: een wachtende verbinding). Wacht
/// tot iets klaar is of de tick-deadline verstrijkt. Geeft `(fd, readable)` per fd.
pub fn sock_poll(fds: &[u64], deadline_ticks: u64) -> alloc::vec::Vec<(u64, bool)> {
    // Spin-plafond náást de tick-deadline: als de timer-tick om welke reden dan ook
    // niet vordert, mag poll() nóóit eeuwig blokkeren.
    let mut spins: u64 = 0;
    loop {
        service(); // verwerk inkomende pakketten → vult Listen-queues
        {
            // Pomp Conn-sockets zodat in-flight data in hun rx-buffer komt.
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
                            Some(Sock::Conn(c)) => !c.rx.is_empty() || !c.open, // data of EOF
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
// AF_UNIX — lokale Unix-domain-sockets (H1). Eén kernelbrede schakelaar, los van
// de TCP/IP-SOCKETS-tabel. De bouwsteen voor de live display-server (H2:
// compositor ↔ app) en IPC-zware apps.
// ---------------------------------------------------------------------------
static UNIX_SWITCH: Mutex<euronet::unix::Switchboard> =
    Mutex::new(euronet::unix::Switchboard::new());

pub use euronet::unix::{Endpoint as UnixEndpoint, UnixError};

/// Bind+luister op een AF_UNIX-pad (server-zijde).
pub fn unix_bind_listen(path: &str, backlog: usize) -> Result<(), UnixError> {
    UNIX_SWITCH.lock().bind_listen(path, backlog)
}
/// Verbind met een AF_UNIX-pad (client-zijde) → client-endpoint.
pub fn unix_connect(path: &str) -> Result<UnixEndpoint, UnixError> {
    UNIX_SWITCH.lock().connect(path)
}
/// Accepteer de oudste wachtende verbinding (server-zijde) → server-endpoint.
pub fn unix_accept(path: &str) -> Option<UnixEndpoint> {
    UNIX_SWITCH.lock().accept(path)
}
/// Schrijf bytes vanaf een endpoint.
pub fn unix_send(ep: UnixEndpoint, data: &[u8]) -> Result<usize, UnixError> {
    UNIX_SWITCH.lock().send(ep, data)
}
/// Lees tot `max` bytes voor een endpoint (niet-blokkerend).
pub fn unix_recv(ep: UnixEndpoint, max: usize) -> Result<alloc::vec::Vec<u8>, UnixError> {
    UNIX_SWITCH.lock().recv(ep, max)
}
/// Is dit endpoint leesbaar (data of EOF)?
pub fn unix_readable(ep: UnixEndpoint) -> bool {
    UNIX_SWITCH.lock().readable(ep)
}
/// Sluit een endpoint.
pub fn unix_close(ep: UnixEndpoint) {
    UNIX_SWITCH.lock().close(ep)
}

/// H1-zelftest: een volledige lokale AF_UNIX round-trip — server bindt+luistert,
/// client verbindt, server accepteert, client→server "ping", server→client "pong",
/// daarna sluit de client en ziet de server EOF. Bewijst beide richtingen + EOF.
pub fn af_unix_selftest() {
    let path = "/run/euro-h1.sock";
    if unix_bind_listen(path, 4).is_err() {
        crate::serial_println!("[h1] AF_UNIX: bind faalde");
        return;
    }
    let client = match unix_connect(path) {
        Ok(c) => c,
        Err(_) => {
            crate::serial_println!("[h1] AF_UNIX: connect faalde");
            return;
        }
    };
    let server = match unix_accept(path) {
        Some(s) => s,
        None => {
            crate::serial_println!("[h1] AF_UNIX: accept gaf niets");
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
        "[h1] AF_UNIX round-trip: server kreeg '{}', client kreeg '{}', EOF-na-close={} ✓",
        core::str::from_utf8(&got_req).unwrap_or("?"),
        core::str::from_utf8(&got_rsp).unwrap_or("?"),
        eof
    );
    UNIX_SWITCH.lock().unbind(path);
}

/// Luister op `port`, accepteer ÉÉN inkomende TCP-verbinding, lees het verzoek,
/// stuur `response`, en sluit netjes af (FIN). Geeft de eerste regel van het
/// ontvangen verzoek terug. Dit is de server-tegenhanger van [`TcpConn`]: SYN →
/// SYN-ACK → ACK → verzoek → antwoord → FIN. Met een ruime spin-timeout zodat de
/// boot doorloopt als er geen client komt.
pub fn tcp_serve_once(port: u16, response: &[u8], timeout_spins: u64) -> Option<String> {
    let cfg = get()?;
    let (my_mac, my_ip) = (cfg.my_mac, cfg.my_ip);
    // Ververs slirp's ARP-cache (gratuitous ARP) zodat een inkomende verbinding
    // ons MAC kent en de SYN direct wordt afgeleverd.
    drain();

    // Poll één binnenkomend TCP-segment naar onze IP:port, mét afzender-info
    // (bron-MAC voor de retourroute, bron-IP, het segment zelf).
    let poll = |spins: u64| -> Option<(MacAddr, Ipv4Addr, TcpSegment)> {
        for _ in 0..spins {
            if let Some(rx) = virtio_net::poll_recv() {
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

    // 1. Wacht op de openende SYN (zonder ACK).
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
        virtio_net::send(&frame);
    };

    // 2. SYN-ACK; ons SYN telt voor 1 in de sequentieruimte.
    emit(tcp::SYN | tcp::ACK, my_seq, their_seq, &[]);
    my_seq = my_seq.wrapping_add(1);

    // 3. Ontvang de ACK + het HTTP-verzoek (kan in één of meer segmenten komen).
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
                    break; // volledig HTTP-verzoek ontvangen
                }
            }
            None => break,
        }
    }

    // 4. Antwoord (PSH+ACK), dan een nette FIN.
    emit(tcp::PSH | tcp::ACK, my_seq, their_seq, response);
    my_seq = my_seq.wrapping_add(response.len() as u32);
    emit(tcp::FIN | tcp::ACK, my_seq, their_seq, &[]);
    let _ = poll(SPINS); // wacht kort op de laatste ACK/FIN van de client

    Some(String::from_utf8_lossy(&req).lines().next().unwrap_or("").into())
}

// ── Achtergrond-HTTP-server (coöperatief) ─────────────────────────────────
// Aan-/uit via `httpd`. Wanneer aan bedient `service()` (elke desktop-tick
// aangeroepen) inkomende verbindingen op :80 — de server draait dus op de
// achtergrond terwijl de desktop interactief blijft, zonder aparte taak en
// zonder RX-race (alles in taak 0).
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

/// De HTTP-pagina die de server uitserveert.
fn http_page() -> alloc::vec::Vec<u8> {
    let body = "<!doctype html><meta charset=utf-8><title>EuroOS</title>\
                <h1>EuroOS</h1><p>Bediend door de achtergrond-HTTP-server (EuroNet).</p>";
    alloc::format!(
        "HTTP/1.1 200 OK\r\nServer: EuroOS\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

/// Bedien één inkomende verbinding waarvan de openende SYN al ontvangen is
/// (door `service()`): SYN-ACK → verzoek → antwoord → FIN. Kort blokkerend.
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
        virtio_net::send(&frame);
    };
    let poll = || -> Option<TcpSegment> {
        for _ in 0..SPINS {
            if let Some(rx) = virtio_net::poll_recv() {
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

/// Shell-commando `serve` — draai EuroOS als HTTP-server: luister op :80,
/// bedien één verbinding met onze eigen pagina, en sluit. Blokkeert tot een
/// client verbindt of de timeout verstrijkt (door de gebruiker aangeroepen,
/// dus blokkeren is verwacht — net als `ping`).
pub fn cmd_serve() -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    if get().is_none() {
        out.push("serve: geen netwerk beschikbaar".into());
        return out;
    }
    let body = "<!doctype html><meta charset=utf-8><title>EuroOS</title>\
                <h1>EuroOS</h1><p>Bediend door EuroNet's eigen TCP-stack.</p>";
    let resp = alloc::format!(
        "HTTP/1.1 200 OK\r\nServer: EuroOS\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    out.push("HTTP-server: luistert op :80 (één verbinding) ...".into());
    match tcp_serve_once(80, resp.as_bytes(), SPINS * 20) {
        Some(req) => {
            out.push(alloc::format!("client bediend: {}", req.trim()));
            out.push(alloc::format!("antwoord: {} bytes verzonden (HTTP 200)", resp.len()));
        }
        None => out.push("geen client binnen de tijd".into()),
    }
    out
}

/// Shell-commando `fetch <host>` — haal http://<host>/ op via TCP.
pub fn cmd_fetch(host: &str) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    let cfg = match get() {
        Some(c) => c,
        None => {
            out.push("fetch: geen netwerk".into());
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
                out.push(alloc::format!("fetch: kan '{host}' niet resolven"));
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
            // toon de eerste paar header-regels
            for line in String::from_utf8_lossy(&data).lines().skip(1).take(3) {
                if line.is_empty() {
                    break;
                }
                out.push(alloc::format!("  {line}"));
            }
        }
        None => out.push(alloc::format!("fetch: geen verbinding met {host}")),
    }
    out
}

/// HTTP GET http://<host><path> en geef (statuslijn, ENKEL de body) terug — de
/// HTTP-headers worden afgeknipt. Basis voor `wget` (download-naar-bestand).
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
    // Headers afknippen: alles na de eerste lege regel (\r\n\r\n) is de body.
    let body = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => raw[i + 4..].to_vec(),
        None => raw,
    };
    Some((status, body))
}

/// Volledige HTTP-GET met poort → ENKEL de body (headers afgeknipt). De
/// EuroWeb-browser gebruikt dit om een ECHTE pagina van een ECHTE server te halen
/// over de echte TCP-stack (DNS/ARP/TCP/HTTP). `host` mag een IP of naam zijn.
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
    // Headers afknippen: alles na de eerste lege regel is de body.
    let body = match data.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => data[i + 4..].to_vec(),
        None => data,
    };
    Some(body)
}

/// Stuur een KANT-EN-KLARE HTTP-request (`request` = volledige request-bytes,
/// bv. uit `euroagent::llm::ollama_http_request`) naar `host:port` over EuroNet-TCP
/// en geef de RUWE HTTP-response (headers + body) terug. Dit is het echte transport
/// voor BB-1: de EuroAgent-lus praat hiermee met een lokale Ollama-`/api/chat`.
/// Bounded connect (4 SYN-retries) → kan de boot niet laten hangen als er geen
/// endpoint draait. Geen TLS (lokaal model op loopback/LAN); cloud = aparte opt-in.
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

/// HTTP(S)-GET die (statuscode, Location-header, body) teruggeeft — voor de
/// browser, die hiermee redirects (301/302/…) kan volgen. `tls=true` → HTTPS via
/// eurotls op poort 443; anders HTTP op `port`.
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
    // Ruwe respons (statuslijn + headers + body) ophalen.
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
    // Statuscode + Location-header parsen uit de headers.
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
    Some((status, location, body))
}

/// Statische /etc/hosts-tabel (naam -> IPv4), door main.rs gevuld uit /etc/hosts.
/// Wordt vóór DNS geraadpleegd — net als op een echt Unix-systeem.
static HOSTS: spin::Mutex<alloc::vec::Vec<(String, Ipv4Addr)>> =
    spin::Mutex::new(alloc::vec::Vec::new());

/// Vul de /etc/hosts-tabel (vervangt de vorige inhoud).
pub fn set_hosts(entries: alloc::vec::Vec<(String, Ipv4Addr)>) {
    *HOSTS.lock() = entries;
}

// ── DNS-cache (S9 netwerk-volwassenheid) ────────────────────────────────────
// Resultaten van DNS-queries worden gecachet (naam -> (IP, vervaltick)), zodat
// herhaalde lookups direct uit het geheugen komen i.p.v. opnieuw het netwerk op
// te gaan — sneller én minder verkeer. TTL ~300 s (30000 ticks bij 100 Hz).
static DNS_CACHE: Mutex<alloc::vec::Vec<(String, Ipv4Addr, u64)>> = Mutex::new(alloc::vec::Vec::new());
const DNS_TTL_TICKS: u64 = 30_000;
static DNS_HITS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DNS_MISSES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn dns_cache_lookup(name: &str) -> Option<Ipv4Addr> {
    let now = crate::interrupts::ticks();
    let mut c = DNS_CACHE.lock();
    c.retain(|(_, _, exp)| *exp > now); // verlopen entries opruimen
    c.iter().find(|(n, _, _)| n == name).map(|(_, ip, _)| *ip)
}

fn dns_cache_insert(name: &str, ip: Ipv4Addr) {
    let exp = crate::interrupts::ticks() + DNS_TTL_TICKS;
    let mut c = DNS_CACHE.lock();
    c.retain(|(n, _, _)| n != name);
    if c.len() >= 32 {
        c.remove(0); // begrens de cache
    }
    c.push((String::from(name), ip, exp));
}

/// Regels voor het `netstat`-commando: DNS-cache + hit/miss-statistiek.
pub fn netstat_lines() -> alloc::vec::Vec<String> {
    let now = crate::interrupts::ticks();
    let hits = DNS_HITS.load(core::sync::atomic::Ordering::Relaxed);
    let misses = DNS_MISSES.load(core::sync::atomic::Ordering::Relaxed);
    let mut out = alloc::vec::Vec::new();
    out.push(alloc::format!("DNS-cache: {} hits, {} misses", hits, misses));
    let c = DNS_CACHE.lock();
    for (n, ip, exp) in c.iter() {
        let ttl = exp.saturating_sub(now) / 100;
        out.push(alloc::format!("  {n:<24} {} (TTL {ttl}s)", ipfmt(*ip)));
    }
    if c.is_empty() {
        out.push("  (cache leeg)".into());
    }
    out
}

/// Zoek een naam op in /etc/hosts (None = niet gevonden -> val terug op DNS).
pub fn hosts_lookup(name: &str) -> Option<Ipv4Addr> {
    HOSTS.lock().iter().find(|(n, _)| n == name).map(|(_, ip)| *ip)
}

/// Parse een IPv4-adres uit "a.b.c.d".
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

/// /24-heuristiek: zit `a` in hetzelfde subnet als `b`?
pub fn same_subnet(a: Ipv4Addr, b: Ipv4Addr) -> bool {
    a.0[0] == b.0[0] && a.0[1] == b.0[1] && a.0[2] == b.0[2]
}

pub fn ipfmt(ip: Ipv4Addr) -> String {
    alloc::format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3])
}

/// Shell-commando `ping <ip-of-naam>` op de live NIC.
pub fn cmd_ping(arg: &str) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    let cfg = match get() {
        Some(c) => c,
        None => {
            out.push("ping: geen netwerk beschikbaar".into());
            return out;
        }
    };
    // Adres bepalen: direct IPv4 of via DNS.
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
                out.push(alloc::format!("ping: kan '{arg}' niet resolven"));
                return out;
            }
        }
    };
    // Next-hop: lokaal subnet → ARP; anders via de gateway.
    let nexthop = if same_subnet(dst, cfg.my_ip) {
        arp_resolve(cfg.my_mac, cfg.my_ip, dst).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    let ok = icmp_ping(cfg.my_mac, cfg.my_ip, nexthop, dst);
    out.push(if ok {
        alloc::format!("PING {label}: echo-reply OK ✓")
    } else {
        alloc::format!("PING {label}: geen antwoord")
    });
    out
}

/// Shell-commando `ping6` — ping de IPv6-router (uit de Router Advertisement).
pub fn cmd_ping6() -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    let cfg = match get() {
        Some(c) => c,
        None => {
            out.push("ping6: geen netwerk".into());
            return out;
        }
    };
    match (cfg.router_ll, cfg.router_mac) {
        (Some(ll), Some(mac)) => {
            let ok = icmp6_ping(cfg.my_mac, cfg.link_local, mac, ll);
            out.push(if ok { "PING6 router: echo-reply OK ✓".into() } else { "PING6 router: geen antwoord".into() });
        }
        _ => out.push("ping6: geen IPv6-router bekend".into()),
    }
    out
}

/// Shell-commando `net` — toon de huidige netwerkconfiguratie.
pub fn cmd_net() -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    match get() {
        Some(c) => {
            out.push(alloc::format!(
                "MAC      {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                c.my_mac.0[0], c.my_mac.0[1], c.my_mac.0[2], c.my_mac.0[3], c.my_mac.0[4], c.my_mac.0[5]
            ));
            out.push(alloc::format!("IPv4     {} (gw {}, dns {})", ipfmt(c.my_ip), ipfmt(c.gw_ip), ipfmt(c.dns_ip)));
            out.push("IPv6     SLAAC link-local + globaal actief".into());
            out.push("commando's: ping <ip|naam> · ping6 · net".into());
        }
        None => out.push("net: geen netwerk geconfigureerd".into()),
    }
    out
}
