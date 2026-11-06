//! `SharedSwitchboard`: AF_UNIX with **per-connection locks** (J1).
//!
//! [`super::Switchboard`] owns every listener and every connection in one value,
//! so the kernel kept it behind ONE mutex: two processes streaming over two
//! unrelated sockets serialised on each other, on every `read` and `write`.
//!
//! Here the control plane and the data plane are split:
//!
//! - **listeners** (bind / connect / accept / unbind) sit behind one lock
//!   (class `UnixTable`). These are rare.
//! - the **connection table** is a per-CPU replicated read-mostly lock
//!   (`eurolock::sync::ReadMostly`, also `UnixTable`): a lookup reads only its own
//!   core's replica, long enough to clone the `Arc` of one connection; it is
//!   written only when a connection is created or freed.
//! - **each connection has its own lock** (class `UnixConn`) around its two
//!   byte FIFOs. `send`, `recv`, `readable`, `available`, `at_eof` and `close`
//!   take only that lock.
//!
//! No path holds two `UnixTable` locks or two connections at once. Every lock
//! here is `Mode::Irq`: these functions run inside syscalls, which enter with
//! IF=0 (FMASK), so by the IRQSAVE rule every holder must have interrupts off.
//!
//! Semantics are identical to `Switchboard` (same errors, same EOF rules, and
//! connection ids are never reused, because the kernel keys descriptors in
//! flight by `(conn, side)`); the host tests below run the same scenarios plus
//! real multi-threaded stress.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use eurolock::sync::{Mutex, ReadMostly, Replica};
use eurolock::{LockClass, LockSite, Mode, SHARDS};

use super::{Conn, Endpoint, Listener, Side, UnixError};

pub static SITE_LISTENERS: LockSite = LockSite::new("unix.listeners", LockClass::UnixTable);
pub static SITE_CONN_TABLE: LockSite = LockSite::new("unix.conn_table", LockClass::UnixTable);
pub static SITE_CONN: LockSite = LockSite::new("unix.conn (per-connection)", LockClass::UnixConn);

type ConnRef = Arc<Mutex<Conn>>;

/// The concurrent local-socket switchboard (see module docs).
pub struct SharedSwitchboard {
    listeners: Mutex<BTreeMap<String, Listener>>,
    conns: ReadMostly<Vec<Option<ConnRef>>>,
}

impl Default for SharedSwitchboard {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedSwitchboard {
    pub const fn new() -> Self {
        SharedSwitchboard {
            listeners: Mutex::new_site(&SITE_LISTENERS, Mode::Irq, BTreeMap::new()),
            conns: ReadMostly::new_site(&SITE_CONN_TABLE, Mode::Irq, [const { Replica::new(Vec::new()) }; SHARDS]),
        }
    }

    pub fn bind_listen(&self, path: &str, backlog: usize) -> Result<(), UnixError> {
        let mut l = self.listeners.lock();
        if l.contains_key(path) {
            return Err(UnixError::AddrInUse);
        }
        l.insert(String::from(path), Listener { backlog: backlog.max(1), pending: VecDeque::new() });
        Ok(())
    }

    pub fn unbind(&self, path: &str) -> bool {
        self.listeners.lock().remove(path).is_some()
    }

    pub fn is_listening(&self, path: &str) -> bool {
        self.listeners.lock().contains_key(path)
    }

    fn new_conn(&self) -> u32 {
        let c = Arc::new(Mutex::new_site(
            &SITE_CONN,
            Mode::Irq,
            Conn { a_to_b: VecDeque::new(), b_to_a: VecDeque::new(), a_open: true, b_open: true },
        ));
        self.conns.write(|t| {
            t.push(Some(c.clone()));
            (t.len() - 1) as u32
        })
    }

    fn drop_conn(&self, id: u32) {
        self.conns.write(|t| {
            if let Some(slot) = t.get_mut(id as usize) {
                *slot = None;
            }
        });
    }

    /// Client side: connect to the listener on `path` (see `Switchboard::connect`).
    pub fn connect(&self, path: &str) -> Result<Endpoint, UnixError> {
        // Check first (cheap refusal), create the connection without holding the
        // listener lock (no UnixTable nesting), then queue it if still allowed.
        {
            let l = self.listeners.lock();
            let lst = l.get(path).ok_or(UnixError::ConnRefused)?;
            if lst.pending.len() >= lst.backlog {
                return Err(UnixError::Backlog);
            }
        }
        let conn = self.new_conn();
        let queued = {
            let mut l = self.listeners.lock();
            match l.get_mut(path) {
                None => Err(UnixError::ConnRefused),
                Some(lst) if lst.pending.len() >= lst.backlog => Err(UnixError::Backlog),
                Some(lst) => {
                    lst.pending.push_back(Endpoint { conn, side: Side::B });
                    Ok(())
                }
            }
        };
        match queued {
            Ok(()) => Ok(Endpoint { conn, side: Side::A }),
            Err(e) => {
                // Lost a race with unbind or another connect: undo.
                self.drop_conn(conn);
                Err(e)
            }
        }
    }

    pub fn accept(&self, path: &str) -> Option<Endpoint> {
        self.listeners.lock().get_mut(path)?.pending.pop_front()
    }

    pub fn pending(&self, path: &str) -> usize {
        self.listeners.lock().get(path).map_or(0, |l| l.pending.len())
    }

    /// The connection behind `ep`; the table lock is released on return.
    fn conn(&self, ep: Endpoint) -> Result<ConnRef, UnixError> {
        self.conns
            .read(|t| t.get(ep.conn as usize).and_then(|c| c.clone()))
            .ok_or(UnixError::BadEndpoint)
    }

    pub fn send(&self, ep: Endpoint, data: &[u8]) -> Result<usize, UnixError> {
        let c = self.conn(ep)?;
        let mut c = c.lock();
        let peer_open = match ep.side {
            Side::A => c.b_open,
            Side::B => c.a_open,
        };
        if !peer_open {
            return Err(UnixError::BrokenPipe);
        }
        match ep.side {
            Side::A => c.a_to_b.extend(data.iter().copied()),
            Side::B => c.b_to_a.extend(data.iter().copied()),
        }
        Ok(data.len())
    }

    pub fn recv(&self, ep: Endpoint, max: usize) -> Result<Vec<u8>, UnixError> {
        let c = self.conn(ep)?;
        let mut c = c.lock();
        let q = match ep.side {
            Side::A => &mut c.b_to_a,
            Side::B => &mut c.a_to_b,
        };
        let n = max.min(q.len());
        Ok(q.drain(..n).collect())
    }

    pub fn readable(&self, ep: Endpoint) -> bool {
        let Ok(c) = self.conn(ep) else {
            return false;
        };
        let c = c.lock();
        match ep.side {
            Side::A => !c.b_to_a.is_empty() || !c.b_open,
            Side::B => !c.a_to_b.is_empty() || !c.a_open,
        }
    }

    pub fn at_eof(&self, ep: Endpoint) -> bool {
        let Ok(c) = self.conn(ep) else {
            return true;
        };
        let c = c.lock();
        match ep.side {
            Side::A => c.b_to_a.is_empty() && !c.b_open,
            Side::B => c.a_to_b.is_empty() && !c.a_open,
        }
    }

    pub fn available(&self, ep: Endpoint) -> usize {
        self.conn(ep).map_or(0, |c| {
            let c = c.lock();
            match ep.side {
                Side::A => c.b_to_a.len(),
                Side::B => c.a_to_b.len(),
            }
        })
    }

    /// Close `ep`; when both sides are closed the connection is freed.
    pub fn close(&self, ep: Endpoint) {
        let Ok(c) = self.conn(ep) else {
            return;
        };
        let both_closed = {
            let mut c = c.lock();
            match ep.side {
                Side::A => c.a_open = false,
                Side::B => c.b_open = false,
            }
            !c.a_open && !c.b_open
        };
        if both_closed {
            self.drop_conn(ep.conn);
        }
    }

    /// Connections currently alive (diagnostics / leak checks).
    pub fn live_connections(&self) -> usize {
        self.conns.read(|t| t.iter().filter(|c| c.is_some()).count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::vec::Vec as SVec;

    // The same scenarios as the single-lock Switchboard tests.

    #[test]
    fn refused_and_addr_in_use() {
        let sw = SharedSwitchboard::new();
        assert_eq!(sw.connect("/run/x.sock"), Err(UnixError::ConnRefused));
        sw.bind_listen("/run/x.sock", 4).unwrap();
        assert_eq!(sw.bind_listen("/run/x.sock", 4), Err(UnixError::AddrInUse));
    }

    #[test]
    fn roundtrip_partial_reads_backlog_eof() {
        let sw = SharedSwitchboard::new();
        sw.bind_listen("/s", 2).unwrap();
        let c = sw.connect("/s").unwrap();
        sw.connect("/s").unwrap();
        assert_eq!(sw.connect("/s"), Err(UnixError::Backlog));
        // A refused connect must not leak a connection.
        assert_eq!(sw.live_connections(), 2);
        let s = sw.accept("/s").unwrap();
        sw.send(c, b"abcdef").unwrap();
        assert!(sw.readable(s));
        assert_eq!(sw.recv(s, 3).unwrap(), b"abc");
        assert_eq!(sw.available(s), 3);
        assert_eq!(sw.recv(s, 100).unwrap(), b"def");
        sw.send(s, b"pong").unwrap();
        assert_eq!(sw.recv(c, 64).unwrap(), b"pong");
        assert!(!sw.readable(c));
        sw.close(c);
        assert!(sw.readable(s));
        assert!(sw.at_eof(s));
        assert_eq!(sw.recv(s, 64).unwrap(), b"");
        assert_eq!(sw.send(s, b"x"), Err(UnixError::BrokenPipe));
        sw.close(s);
        assert_eq!(sw.send(c, b"x"), Err(UnixError::BadEndpoint));
        assert!(sw.unbind("/s"));
        assert_eq!(sw.connect("/s"), Err(UnixError::ConnRefused));
    }

    #[test]
    fn connection_ids_are_never_reused() {
        let sw = SharedSwitchboard::new();
        sw.bind_listen("/s", 8).unwrap();
        let c1 = sw.connect("/s").unwrap();
        let s1 = sw.accept("/s").unwrap();
        sw.close(c1);
        sw.close(s1);
        let c2 = sw.connect("/s").unwrap();
        assert_ne!(c1.key().0, c2.key().0);
    }

    /// Four threads each stream 200 KB through their own connection (and read
    /// back the echo of the other side): bytes must arrive complete and in order.
    #[test]
    fn parallel_streams_are_exact_and_ordered() {
        let sw = Arc::new(SharedSwitchboard::new());
        sw.bind_listen("/srv", 16).unwrap();
        let pairs: SVec<(Endpoint, Endpoint)> = (0..4)
            .map(|_| {
                let c = sw.connect("/srv").unwrap();
                let s = sw.accept("/srv").unwrap();
                (c, s)
            })
            .collect();
        let hs: SVec<_> = pairs
            .into_iter()
            .enumerate()
            .map(|(t, (c, s))| {
                let sw = sw.clone();
                std::thread::spawn(move || {
                    let mut expect = 0u32;
                    let mut sent = 0u32;
                    let total = 50_000u32; // u32 words = 200 KB
                    while expect < total {
                        if sent < total {
                            let w = (sent ^ (t as u32) << 24).to_le_bytes();
                            sw.send(c, &w).unwrap();
                            sent += 1;
                        }
                        let got = sw.recv(s, 4 * 64).unwrap();
                        assert_eq!(got.len() % 4, 0, "words are never torn");
                        for w in got.chunks(4) {
                            let v = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                            assert_eq!(v, expect ^ (t as u32) << 24, "stream {t} out of order");
                            expect += 1;
                        }
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
    }

    /// Connect/accept/close churn on two threads while two others stream: no
    /// panic, no lost bytes on the streaming pairs, no leaked connections.
    #[test]
    fn control_plane_churn_during_streaming() {
        let sw = Arc::new(SharedSwitchboard::new());
        sw.bind_listen("/data", 4).unwrap();
        sw.bind_listen("/churn", 1024).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let churners: SVec<_> = (0..2)
            .map(|_| {
                let sw = sw.clone();
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        if let Ok(c) = sw.connect("/churn") {
                            if let Some(s) = sw.accept("/churn") {
                                let _ = sw.send(s, b"hi");
                                sw.close(s);
                            }
                            sw.close(c);
                            n += 1;
                        }
                    }
                    n
                })
            })
            .collect();
        let streams: SVec<_> = (0..2)
            .map(|_| {
                let c = sw.connect("/data").unwrap();
                let s = sw.accept("/data").unwrap();
                let sw = sw.clone();
                std::thread::spawn(move || {
                    let mut total = 0usize;
                    for i in 0..5_000u32 {
                        sw.send(c, &i.to_le_bytes()).unwrap();
                        total += sw.recv(s, 1 << 16).unwrap().len();
                    }
                    total += sw.recv(s, 1 << 20).unwrap().len();
                    assert_eq!(total, 5_000 * 4);
                    sw.close(c);
                    sw.close(s);
                })
            })
            .collect();
        for s in streams {
            s.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        let churned: u64 = churners.into_iter().map(|h| h.join().unwrap()).sum();
        assert!(churned > 0);
        // Every churned connection was closed on both sides (an accept that lost
        // its race leaves a pending B-side; drain those too).
        while let Some(s) = sw.accept("/churn") {
            sw.close(s);
        }
        assert_eq!(sw.live_connections(), 0, "every connection closed on both sides is freed");
    }
}
