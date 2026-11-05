//! Inbound TCP without waiting (BUG-019).
//!
//! A SYN for the background httpd or a LISTEN socket used to be served on the
//! spot: `serve_connection` / `accept_from` sent the SYN-ACK and then polled
//! the NIC (4 M iterations per look, up to 40 looks) for the client's ACK and
//! request, inside `service()`, i.e. inside the desktop loop. One client at a
//! time, and the loop (console, compositor, watchdog) stood still meanwhile:
//! with four host clients on the httpd under KVM 39 answered, 50 failed, p99
//! 7.4 s, and the console went silent for over a minute.
//!
//! Now a SYN only creates a half-open entry (SYN-RCVD) in a bounded table and
//! sends the SYN-ACK; every later step happens in [`step`], which never waits:
//! it takes what the per-connection queues hold, completes handshakes,
//! retransmits SYN-ACKs on a timer, answers httpd requests and retires
//! finished connections. A completed handshake for a LISTEN socket goes to its
//! accept queue and wakes the tasks sleeping in a network wait (accept).
//! `service()` (desktop loop, accept, poll) and `pump_all()` (desktop and
//! launcher loops) drive it.
//!
//! The bookkeeping of the half-open table (backlog, SYN-ACK backoff, expiry,
//! RFC 5961 resets) is `euronet::tcpopen::SynQueue`, host-tested. The listen
//! queue / accept queue split follows the classic BSD design (see Stevens,
//! TCP/IP Illustrated vol. 2, ch. 28-30); Redox's smoltcp-based netstack keeps a
//! socket per pending connection the same way. No code is taken from either.

use super::*;
use core::sync::atomic::AtomicUsize;
use euronet::tcpopen::{SynAction, SynQueue};

/// Half-open entries over all listeners and the httpd.
const SYNQ_CAP: usize = 64;
/// Concurrent httpd connections (half-open ones included).
const HTTPD_MAX: usize = 32;
const HTTPD_PORT: u16 = 80;
/// Ticks a client gets to send its request.
const HTTPD_REQ_TICKS: u64 = 1000;
/// Ticks an answered connection may wait for the client's FIN.
const HTTPD_LINGER_TICKS: u64 = 500;
/// A request is answered at the latest when this much of it arrived.
const HTTPD_REQ_MAX: usize = 8192;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Listen,
    Httpd,
}

/// What a half-open entry carries: the connection itself.
struct Half {
    conn: TcpConn,
    owner: Owner,
}

/// One httpd connection after its handshake.
struct HttpdConn {
    conn: TcpConn,
    req: alloc::vec::Vec<u8>,
    born: u64,
    answered: Option<u64>,
    last_tick: u64,
}

struct Passive {
    synq: SynQueue<ConnKey, Half>,
    httpd: alloc::vec::Vec<HttpdConn>,
}

// A plain spin lock, taken with interrupts off and never held across a wait.
// Not a EuroLock: it is held while the per-connection queues (net.PORTQ) are
// read, and two locks of the Net class may not nest.
static PASSIVE: Mutex<Passive> = Mutex::new(Passive { synq: SynQueue::new(SYNQ_CAP), httpd: alloc::vec::Vec::new() });
/// Entries in PASSIVE (the fast path of `step` when nothing is going on).
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Counters for the report and the self-test.
pub(super) static SYNS: AtomicU64 = AtomicU64::new(0);
pub(super) static SYN_DROPPED: AtomicU64 = AtomicU64::new(0);
pub(super) static SYNACK_RETX: AtomicU64 = AtomicU64::new(0);
pub(super) static HALF_EXPIRED: AtomicU64 = AtomicU64::new(0);
pub(super) static ESTABLISHED: AtomicU64 = AtomicU64::new(0);
pub(super) static ACCEPT_OVERFLOW: AtomicU64 = AtomicU64::new(0);
/// Longest `step` in microseconds (HPET).
pub(super) static STEP_MAX_US: AtomicU64 = AtomicU64::new(0);

fn set_active(p: &Passive) {
    ACTIVE.store(p.synq.len() + p.httpd.len(), Ordering::Relaxed);
}

/// A bare SYN arrived for `key` (firewall already passed). True when the
/// httpd or a LISTEN socket owns the port (then no RST is sent, even when the
/// SYN had to be dropped for lack of room: the client retransmits).
pub(super) fn on_syn(seg: &TcpSegment, key: ConnKey, peer_mac: MacAddr, cfg: &NetCfg) -> bool {
    let httpd = SERVER_ON.load(Ordering::Relaxed) && seg.dst_port == HTTPD_PORT;
    // Socket locks before PASSIVE, never inside it.
    let listener = if httpd { None } else { listener_info(seg.dst_port) };
    if !httpd && listener.is_none() {
        return false;
    }
    SYNS.fetch_add(1, Ordering::Relaxed);
    let now = crate::interrupts::ticks();
    let _g = crate::sched::IfOffGuard::new();
    let mut p = PASSIVE.lock();
    if httpd {
        // The client reuses the four-tuple of a connection we still hold: an
        // answered one is finished from its side, a fresh SYN replaces it.
        if let Some(i) = p.httpd.iter().position(|h| h.conn.key() == key) {
            if p.httpd[i].answered.is_none() {
                return true;
            }
            p.httpd.swap_remove(i);
        }
    }
    let (owner, room) = match listener {
        None => {
            let busy = p.httpd.len() + p.synq.count(|k| k.lport == HTTPD_PORT);
            (Owner::Httpd, busy < HTTPD_MAX)
        }
        Some((backlog, queued)) => {
            let half = p.synq.count(|k| k.lport == seg.dst_port);
            (Owner::Listen, queued + half < backlog)
        }
    };
    let action = p.synq.on_syn(key, seg.seq, now, room, || {
        let c = TcpConn::new_passive(cfg.my_mac, cfg.my_ip, peer_mac, key.peer, seg.dst_port, seg);
        (c.my_seq, Half { conn: c, owner })
    });
    match action {
        SynAction::New => {
            if let Some(e) = p.synq.get(&key) {
                e.data.conn.emit_syn_ack();
            }
        }
        SynAction::Retransmit => {
            if let Some(e) = p.synq.get(&key) {
                e.data.conn.emit_syn_ack();
            }
            p.synq.sent_again(&key, now);
            SYNACK_RETX.fetch_add(1, Ordering::Relaxed);
        }
        SynAction::Full => {
            SYN_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
    set_active(&p);
    true
}

/// Move every inbound connection on as far as it can go now. Never waits.
pub(super) fn step() {
    if ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    let t0 = time_us();
    let now = crate::interrupts::ticks();
    let mut ready: alloc::vec::Vec<(u16, TcpConn)> = alloc::vec::Vec::new();
    {
        let _g = crate::sched::IfOffGuard::new();
        // Another context is stepping: it does the work.
        let Some(mut p) = PASSIVE.try_lock() else { return };
        rx_route();
        let p = &mut *p;

        // 1. Half-open: the handshake's final ACK, or a reset.
        for key in p.synq.keys() {
            while let Some(seg) = portq_take(key) {
                if seg.has(tcp::RST) {
                    if p.synq.on_rst(&key, seg.seq).is_some() {
                        break;
                    }
                    continue;
                }
                if seg.has(tcp::ACK) {
                    if let Some(e) = p.synq.on_ack(&key, seg.ack) {
                        let Half { mut conn, owner } = e.data;
                        conn.passive_established(seg);
                        ESTABLISHED.fetch_add(1, Ordering::Relaxed);
                        match owner {
                            Owner::Httpd => p.httpd.push(HttpdConn { conn, req: alloc::vec::Vec::new(), born: now, answered: None, last_tick: now }),
                            Owner::Listen => ready.push((key.lport, conn)),
                        }
                        break;
                    }
                }
                // Anything else before the handshake completes is dropped; the
                // peer retransmits it.
            }
        }

        // 2. SYN-ACK retransmissions (1, 2, 4 s) and expiry.
        let (again, expired) = p.synq.due(now);
        for k in again {
            if let Some(e) = p.synq.get(&k) {
                e.data.conn.emit_syn_ack();
            }
            p.synq.sent_again(&k, now);
            SYNACK_RETX.fetch_add(1, Ordering::Relaxed);
        }
        HALF_EXPIRED.fetch_add(expired.len() as u64, Ordering::Relaxed);
        drop(expired);

        // 3. httpd connections: read the request, answer, close, retire.
        let mut page: Option<alloc::vec::Vec<u8>> = None;
        let mut i = 0;
        while i < p.httpd.len() {
            let h = &mut p.httpd[i];
            h.conn.pump_nowait();
            if h.answered.is_none() {
                let n = h.conn.rx.len().min(HTTPD_REQ_MAX.saturating_sub(h.req.len()));
                h.req.extend(h.conn.rx.drain(..n));
                h.conn.rx.clear();
                let complete = h.req.windows(4).any(|w| w == b"\r\n\r\n");
                if complete || h.conn.peer_fin || h.req.len() >= HTTPD_REQ_MAX {
                    let body = page.get_or_insert_with(http_page);
                    h.conn.send_nowait(body);
                    h.conn.close_nowait();
                    h.answered = Some(now);
                    SERVED.fetch_add(1, Ordering::Relaxed);
                }
            }
            if now.wrapping_sub(h.last_tick) >= 100 {
                h.last_tick = now;
                h.conn.tick();
            }
            let done = match h.answered {
                Some(t) => {
                    (h.conn.peer_fin && h.conn.fin_acked() && h.conn.retx.is_empty())
                        || h.conn.reset
                        || now.wrapping_sub(t) > HTTPD_LINGER_TICKS
                }
                None => h.conn.reset || now.wrapping_sub(h.born) > HTTPD_REQ_TICKS,
            };
            if done {
                // Drop: a FIN still owed goes out, the queue is freed.
                p.httpd.swap_remove(i);
                continue;
            }
            i += 1;
        }
        set_active(p);
    }

    // 4. Completed handshakes to their listeners (socket locks, outside PASSIVE).
    if !ready.is_empty() {
        for (port, conn) in ready {
            if let Err(c) = listener_push(port, conn) {
                ACCEPT_OVERFLOW.fetch_add(1, Ordering::Relaxed);
                drop(c);
            }
        }
        // accept() sleeps in network waits: wake it.
        wake_rx_waiters();
    }
    STEP_MAX_US.fetch_max(time_us().saturating_sub(t0), Ordering::Relaxed);
}

/// A LISTEN socket on `port` closed: forget its half-open connections.
pub(super) fn listener_closed(port: u16) {
    if listener_info(port).is_some() {
        return; // another listener on the same port remains
    }
    let _g = crate::sched::IfOffGuard::new();
    let mut p = PASSIVE.lock();
    for k in p.synq.keys() {
        if k.lport == port && p.synq.get(&k).map(|e| e.data.owner == Owner::Listen).unwrap_or(false) {
            p.synq.remove(&k);
        }
    }
    set_active(&p);
}

/// (half-open entries, httpd connections) right now.
pub(super) fn counts() -> (usize, usize) {
    let _g = crate::sched::IfOffGuard::new();
    let p = PASSIVE.lock();
    (p.synq.len(), p.httpd.len())
}
