//! `[net-async]` boot self-test (BUG-019 / BUG-026), with synthetic peers.
//!
//! The peers live on TEST-NET-1 (192.0.2.0/24, RFC 5737), which no real
//! network routes. While the test runs, every frame this stack sends to such
//! an address is captured here instead of reaching the NIC, and the test
//! injects the peers' frames into the receive demux as if the NIC had
//! delivered them. That makes the inbound engine and the connect path
//! testable at boot without a host-side client, and deterministic:
//!
//! - httpd: N clients open connections at once; one `service()` pass must
//!   answer every SYN, the next one every request, the next one retire them.
//! - listen backlog: more SYNs than the backlog; the excess is dropped (no
//!   RST), accept() takes the completed ones, retransmitted SYNs get in after.
//! - connect: a plain answer; a half-open peer that answers our SYN with a
//!   stale ACK (expect a RST at its ACK, then the connection); a peer that
//!   drops our SYNs silently (expect a fresh port after 1 s + 2 s); and our
//!   FIN after the peer closed first.

use super::*;

static ON: AtomicBool = AtomicBool::new(false);
static CAPTURED: Mutex<alloc::vec::Vec<alloc::vec::Vec<u8>>> = Mutex::new(alloc::vec::Vec::new());

/// How the scripted server 192.0.2.50 answers SYNs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Answer,
    /// First SYN: a bare ACK of an old connection; then answer.
    StaleAck,
    /// SYNs from the first port it sees are dropped; another port is answered.
    Silent,
}
static SCRIPT: Mutex<(Script, bool, u16)> = Mutex::new((Script::Answer, false, 0));
const SERVER: Ipv4Addr = Ipv4Addr([192, 0, 2, 50]);
const STALE_ACK: u32 = 0x5EED_0001;
const PEER_ISN: u32 = 0x0BAD_C0DE;
const PEER_MAC: MacAddr = MacAddr([0x02, 0, 0x5e, 0, 0x02, 0x01]);

fn is_test_peer(ip: Ipv4Addr) -> bool {
    ip.0[0] == 192 && ip.0[1] == 0 && ip.0[2] == 2
}

/// Parse a frame we sent: (destination, TCP segment).
fn tcp_of(frame: &[u8]) -> Option<(Ipv4Addr, TcpSegment)> {
    let (h, p) = EthernetHeader::parse(frame).ok()?;
    if h.ethertype != EtherType::Ipv4 {
        return None;
    }
    let (ih, ipl) = Ipv4Header::parse(p).ok()?;
    if ih.protocol != Protocol::Tcp {
        return None;
    }
    Some((ih.dst, TcpSegment::parse(ipl).ok()?))
}

/// tx() hook: true when the frame went to a synthetic peer (captured).
pub(super) fn capture(frame: &[u8]) -> bool {
    if !ON.load(Ordering::Relaxed) {
        return false;
    }
    let Ok((h, p)) = EthernetHeader::parse(frame) else { return false };
    if h.ethertype != EtherType::Ipv4 {
        return false;
    }
    let Ok((ih, _)) = Ipv4Header::parse(p) else { return false };
    if !is_test_peer(ih.dst) {
        return false;
    }
    CAPTURED.lock().push(frame.to_vec());
    if let Some((dst, s)) = tcp_of(frame) {
        respond(dst, &s);
    }
    true
}

/// The scripted peers' immediate answers.
fn respond(dst: Ipv4Addr, s: &TcpSegment) {
    // Our FIN to any synthetic peer is acknowledged at once (close() waits for it).
    if s.has(tcp::FIN) {
        inject(dst, s.dst_port, s.src_port, s.ack, s.seq.wrapping_add(s.payload.len() as u32 + 1), tcp::ACK, &[]);
        return;
    }
    if dst != SERVER || !s.has(tcp::SYN) || s.has(tcp::ACK) {
        return;
    }
    let answer = {
        let mut sc = SCRIPT.lock();
        match sc.0 {
            Script::Answer => true,
            Script::StaleAck => {
                if !sc.1 {
                    sc.1 = true;
                    drop(sc);
                    inject(dst, s.dst_port, s.src_port, 777, STALE_ACK, tcp::ACK, &[]);
                    return;
                }
                true
            }
            Script::Silent => {
                if sc.2 == 0 {
                    sc.2 = s.src_port;
                }
                s.src_port != sc.2
            }
        }
    };
    if answer {
        inject(dst, s.dst_port, s.src_port, PEER_ISN, s.seq.wrapping_add(1), tcp::SYN | tcp::ACK, &[]);
    }
}

/// A frame from synthetic peer `src`:`sport` to us, into the receive demux.
fn inject(src: Ipv4Addr, sport: u16, dport: u16, seq: u32, ack: u32, flags: u8, payload: &[u8]) {
    let Some(cfg) = get() else { return };
    let seg = TcpSegment { src_port: sport, dst_port: dport, seq, ack, flags, window: 64240, payload: payload.to_vec() };
    let iph = Ipv4Header { protocol: Protocol::Tcp, ttl: 64, src, dst: cfg.my_ip, total_length: 0, identification: 0x7e57 };
    let frame = EthernetHeader { dst: cfg.my_mac, src: PEER_MAC, ethertype: EtherType::Ipv4 }.build(&iph.build(&seg.build(src, cfg.my_ip)));
    route_frame(frame, cfg.my_ip);
}

fn take_captured() -> alloc::vec::Vec<(Ipv4Addr, TcpSegment)> {
    core::mem::take(&mut *CAPTURED.lock()).iter().filter_map(|f| tcp_of(f)).collect()
}

fn client(i: usize) -> Ipv4Addr {
    Ipv4Addr([192, 0, 2, 10 + i as u8])
}

/// One timed service() pass (µs).
fn pass() -> u64 {
    let t = time_us();
    service();
    time_us().saturating_sub(t)
}

/// N concurrent clients on the httpd. (ok, detail)
fn httpd_test(n: usize) -> (bool, String) {
    let saved = SERVER_ON.swap(true, Ordering::Relaxed);
    let served0 = SERVED.load(Ordering::Relaxed);
    let req = b"GET / HTTP/1.0\r\nHost: euroos\r\n\r\n";
    let port = |i: usize| 42000 + i as u16;
    let irs = |i: usize| 1000 * (i as u32 + 1);
    take_captured();
    for i in 0..n {
        inject(client(i), port(i), 80, irs(i), 0, tcp::SYN, &[]);
    }
    let p1 = pass();
    // Our ISN per client, from its SYN-ACK.
    let mut iss = alloc::vec![None; n];
    for (dst, s) in take_captured() {
        if s.has(tcp::SYN) && s.has(tcp::ACK) {
            if let Some(i) = (0..n).find(|&i| client(i) == dst && port(i) == s.dst_port && s.ack == irs(i).wrapping_add(1)) {
                iss[i] = Some(s.seq);
            }
        }
    }
    let synacks = iss.iter().filter(|x| x.is_some()).count();
    for i in 0..n {
        if let Some(our) = iss[i] {
            inject(client(i), port(i), 80, irs(i) + 1, our.wrapping_add(1), tcp::ACK, &[]);
            inject(client(i), port(i), 80, irs(i) + 1, our.wrapping_add(1), tcp::PSH | tcp::ACK, req);
        }
    }
    let p2 = pass();
    let mut answered = 0;
    let mut ends: alloc::vec::Vec<(usize, u32)> = alloc::vec::Vec::new();
    for (dst, s) in take_captured() {
        let Some(i) = (0..n).find(|&i| client(i) == dst && port(i) == s.dst_port) else { continue };
        if s.payload.starts_with(b"HTTP/1.1 200") {
            answered += 1;
        }
        if s.has(tcp::FIN) {
            ends.push((i, s.seq.wrapping_add(1)));
        }
    }
    // The clients close too (our FIN was acknowledged by the capture hook).
    for &(i, after_fin) in &ends {
        inject(client(i), port(i), 80, irs(i) + 1 + req.len() as u32, after_fin, tcp::FIN | tcp::ACK, &[]);
    }
    let p3 = pass();
    let (half, conns) = passive::counts();
    let served = SERVED.load(Ordering::Relaxed) - served0;
    SERVER_ON.store(saved, Ordering::Relaxed);
    let longest = p1.max(p2).max(p3);
    let ok = synacks == n && answered == n && ends.len() == n && served == n as u64 && half == 0 && conns == 0;
    (
        ok,
        alloc::format!(
            "{n} concurrent clients: {synacks} SYN-ACKs from one service() pass, {answered} answered, {} closed, {half} half-open + {conns} open left, passes {p1}/{p2}/{p3} us (longest {longest} us)",
            ends.len()
        ),
    )
}

/// More SYNs than the backlog of a LISTEN socket.
fn backlog_test() -> (bool, String) {
    const PORT: u16 = 9181;
    const BACKLOG: usize = 4;
    const CLIENTS: usize = 6;
    let lfd = sock_open(false);
    if (lfd as i64) < 0 || (sock_bind(lfd, PORT) as i64) < 0 || (sock_listen(lfd, BACKLOG) as i64) < 0 {
        sock_close(lfd);
        return (false, "socket/bind/listen failed".into());
    }
    let port = |i: usize| 43000 + i as u16;
    let irs = |i: usize| 5000 * (i as u32 + 1);
    let mut accepted = alloc::vec::Vec::new();
    let mut first_synacks = 0;
    let mut done = [false; CLIENTS];
    take_captured();
    // Round 1: every client; round 2: the ones still waiting retransmit.
    for round in 0..2 {
        for i in 0..CLIENTS {
            if !done[i] {
                inject(client(i), port(i), PORT, irs(i), 0, tcp::SYN, &[]);
            }
        }
        pass();
        let mut got = 0;
        for (dst, s) in take_captured() {
            if s.has(tcp::SYN) && s.has(tcp::ACK) {
                if let Some(i) = (0..CLIENTS).find(|&i| client(i) == dst && port(i) == s.dst_port) {
                    got += 1;
                    done[i] = true;
                    inject(client(i), port(i), PORT, irs(i) + 1, s.seq.wrapping_add(1), tcp::ACK, &[]);
                }
            }
        }
        if round == 0 {
            first_synacks = got;
        }
        pass();
        loop {
            let fd = sock_accept_nowait(lfd);
            if (fd as i64) < 0 {
                break;
            }
            accepted.push(fd);
        }
    }
    let n_acc = accepted.len();
    for fd in accepted {
        sock_close(fd);
    }
    sock_close(lfd);
    take_captured();
    let ok = first_synacks == BACKLOG && n_acc == CLIENTS;
    (ok, alloc::format!("backlog {BACKLOG}, {CLIENTS} SYNs: {first_synacks} half-open + {} dropped, then {n_acc}/{CLIENTS} accepted after the retransmitted SYNs", CLIENTS - first_synacks))
}

/// One connect to the scripted server; (connected, µs, RSTs we sent, our ports used).
fn connect_once(script: Script) -> (Option<TcpConn>, u64, usize, usize) {
    *SCRIPT.lock() = (script, false, 0);
    take_captured();
    let Some(cfg) = get() else { return (None, 0, 0, 0) };
    let t = time_us();
    let c = TcpConn::connect(cfg.my_mac, cfg.my_ip, cfg.gw_mac, SERVER, 80);
    let us = time_us().saturating_sub(t);
    let caps = take_captured();
    let rsts = caps.iter().filter(|(_, s)| s.has(tcp::RST) && s.seq == STALE_ACK).count();
    let mut ports: alloc::vec::Vec<u16> = caps.iter().filter(|(_, s)| s.has(tcp::SYN)).map(|(_, s)| s.src_port).collect();
    ports.dedup();
    (c, us, rsts, ports.len())
}

fn connect_test() -> (bool, String) {
    let (a, a_us, _, _) = connect_once(Script::Answer);
    // The peer closes first (HTTP/1.0): data + FIN, then our close must send our FIN.
    let mut fin_ok = false;
    if let Some(mut c) = a {
        let our = c.my_seq;
        inject(SERVER, 80, c.sport, PEER_ISN.wrapping_add(1), our, tcp::PSH | tcp::ACK | tcp::FIN, b"bye");
        let d = c.recv(64);
        c.close();
        let caps = take_captured();
        fin_ok = d == b"bye" && caps.iter().any(|(_, s)| s.has(tcp::FIN) && s.seq == our);
    }
    let (b, b_us, b_rst, _) = connect_once(Script::StaleAck);
    let b_ok = b.is_some() && b_rst == 1;
    drop(b);
    let (c, c_us, _, c_ports) = connect_once(Script::Silent);
    let c_ok = c.is_some() && c_ports == 2;
    drop(c);
    take_captured();
    let bound = euronet::tcpopen::SynSchedule::total_ticks() / 100;
    let ok = a_us > 0 && fin_ok && b_ok && c_ok && c_us < bound * 1_000_000;
    (
        ok,
        alloc::format!(
            "answered {} us; half-open peer (stale ACK): RST at its ACK {}, established in {} us; silent drop: fresh port after {} ms ({} ports); our FIN after the peer's: {}; bound {} s",
            a_us, if b_rst == 1 { "sent" } else { "MISSING" }, b_us, c_us / 1000, c_ports, if fin_ok { "sent" } else { "MISSING" }, bound
        ),
    )
}

/// The `[net-async]` markers.
pub fn async_selftest() {
    if get().is_none() {
        return;
    }
    ON.store(true, Ordering::Relaxed);
    let (h_ok, h) = httpd_test(16);
    let (l_ok, l) = backlog_test();
    let (c_ok, c) = connect_test();
    ON.store(false, Ordering::Relaxed);
    CAPTURED.lock().clear();
    let v = |ok: bool| if ok { "→ OK ✓" } else { "FAILED" };
    crate::serial_println!("[net-async] inbound httpd (synthetic peers, TEST-NET-1): {h} {}", v(h_ok));
    crate::serial_println!("[net-async] listen: {l} {}", v(l_ok));
    crate::serial_println!("[net-async] connect: {c} {}", v(c_ok));
    crate::serial_println!("[net-async] {}", tcp_open_report());
}
