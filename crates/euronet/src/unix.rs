//! AF_UNIX — Unix-domain (local) sockets (H1).
//!
//! A **local**, reliable, in-kernel byte stream between two processes on
//! the same machine, addressed via a path name (`/run/foo.sock`) instead of an
//! IP port. The building block for the live display server (H2: compositor ↔ app) and
//! IPC-heavy apps — no network, so no checksums/retransmission, purely
//! guaranteed, ordered bytes.
//!
//! Model: one central [`Switchboard`] owns *all* connections, so no
//! shared ownership (Rc/Arc) is needed — in the kernel it sits behind one
//! Mutex. An endpoint is a lightweight [`Endpoint`] handle `(conn, side)`; the two
//! sides A/B of a connection share two crossed byte FIFOs. `no_std`+alloc,
//! fully testable on the host (no NIC/QEMU).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

/// Which side of a connection an endpoint is. Side A is the **client**
/// (`connect`), side B the **server** (`accept`). A writes into `a_to_b` and reads
/// from `b_to_a`; B the other way around.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    A,
    B,
}

/// Lightweight reference to one side of one connection. Copyable — fits in the
/// kernel SOCKETS table without ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    conn: u32,
    side: Side,
}

/// One bidirectional connection: two byte FIFOs + an open flag per side.
struct Conn {
    a_to_b: VecDeque<u8>,
    b_to_a: VecDeque<u8>,
    a_open: bool,
    b_open: bool,
}

/// A listener bound to a path, with a queue of not-yet-accepted
/// server endpoints (the B sides of incoming `connect` requests).
struct Listener {
    backlog: usize,
    pending: VecDeque<Endpoint>,
}

/// Error codes — deliberately the same meaning as the POSIX errnos the Linux ABI
/// must return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnixError {
    /// No listener on that path (ECONNREFUSED).
    ConnRefused,
    /// Path already in use (EADDRINUSE).
    AddrInUse,
    /// Accept queue full (the backlog has been reached).
    Backlog,
    /// Invalid/closed endpoint (EBADF).
    BadEndpoint,
    /// The other side is closed (EPIPE) — on write.
    BrokenPipe,
}

/// The central local-socket switchboard. Owns all listeners + connections.
#[derive(Default)]
pub struct Switchboard {
    listeners: BTreeMap<String, Listener>,
    conns: Vec<Option<Conn>>,
}

impl Switchboard {
    pub const fn new() -> Self {
        Switchboard {
            listeners: BTreeMap::new(),
            conns: Vec::new(),
        }
    }

    /// Bind + listen on `path`. One step (like an AF_UNIX server that does `bind`
    /// then `listen`). Error if the path already has a listener.
    pub fn bind_listen(&mut self, path: &str, backlog: usize) -> Result<(), UnixError> {
        if self.listeners.contains_key(path) {
            return Err(UnixError::AddrInUse);
        }
        self.listeners.insert(
            String::from(path),
            Listener {
                backlog: backlog.max(1),
                pending: VecDeque::new(),
            },
        );
        Ok(())
    }

    /// Remove the listener on `path` (already-accepted connections stay
    /// alive; still-pending ones are dropped). Returns `true` if there was one.
    pub fn unbind(&mut self, path: &str) -> bool {
        self.listeners.remove(path).is_some()
    }

    /// Is there a listener on this path?
    pub fn is_listening(&self, path: &str) -> bool {
        self.listeners.contains_key(path)
    }

    /// Client side: connect to the listener on `path`. Creates a new
    /// connection, places the **server** side (B) in the accept queue, and returns the
    /// **client** side (A). Error if there is no listener or the backlog is full.
    pub fn connect(&mut self, path: &str) -> Result<Endpoint, UnixError> {
        let l = self.listeners.get_mut(path).ok_or(UnixError::ConnRefused)?;
        if l.pending.len() >= l.backlog {
            return Err(UnixError::Backlog);
        }
        let conn = self.conns.len() as u32;
        self.conns.push(Some(Conn {
            a_to_b: VecDeque::new(),
            b_to_a: VecDeque::new(),
            a_open: true,
            b_open: true,
        }));
        l.pending.push_back(Endpoint {
            conn,
            side: Side::B,
        });
        Ok(Endpoint {
            conn,
            side: Side::A,
        })
    }

    /// Server side: accept the oldest pending connection on `path`. Returns the
    /// **server** side (B), or `None` if the queue is empty (non-blocking).
    pub fn accept(&mut self, path: &str) -> Option<Endpoint> {
        self.listeners.get_mut(path)?.pending.pop_front()
    }

    /// Number of pending (not-yet-accepted) connections on `path`.
    pub fn pending(&self, path: &str) -> usize {
        self.listeners.get(path).map_or(0, |l| l.pending.len())
    }

    fn conn_mut(&mut self, ep: Endpoint) -> Result<&mut Conn, UnixError> {
        self.conns
            .get_mut(ep.conn as usize)
            .and_then(|c| c.as_mut())
            .ok_or(UnixError::BadEndpoint)
    }

    fn conn_ref(&self, ep: Endpoint) -> Result<&Conn, UnixError> {
        self.conns
            .get(ep.conn as usize)
            .and_then(|c| c.as_ref())
            .ok_or(UnixError::BadEndpoint)
    }

    /// Write bytes from `ep`. A writes toward B and vice versa. Error if the
    /// other side is closed (EPIPE). Returns the number of bytes written (all).
    pub fn send(&mut self, ep: Endpoint, data: &[u8]) -> Result<usize, UnixError> {
        let c = self.conn_mut(ep)?;
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

    /// Read up to `max` bytes for `ep` from its incoming FIFO. A reads `b_to_a`,
    /// B reads `a_to_b`. Non-blocking: returns a (possibly empty) Vec.
    pub fn recv(&mut self, ep: Endpoint, max: usize) -> Result<Vec<u8>, UnixError> {
        let c = self.conn_mut(ep)?;
        let q = match ep.side {
            Side::A => &mut c.b_to_a,
            Side::B => &mut c.a_to_b,
        };
        let n = max.min(q.len());
        Ok(q.drain(..n).collect())
    }

    /// Is `ep` READABLE? (incoming data present, or the other side is closed →
    /// EOF is readable so that `poll`/`recv` returns 0 instead of waiting forever.)
    pub fn readable(&self, ep: Endpoint) -> bool {
        let Ok(c) = self.conn_ref(ep) else {
            return false;
        };
        match ep.side {
            Side::A => !c.b_to_a.is_empty() || !c.b_open,
            Side::B => !c.a_to_b.is_empty() || !c.a_open,
        }
    }

    /// Number of immediately readable bytes for `ep` (without EOF).
    pub fn available(&self, ep: Endpoint) -> usize {
        self.conn_ref(ep).map_or(0, |c| match ep.side {
            Side::A => c.b_to_a.len(),
            Side::B => c.a_to_b.len(),
        })
    }

    /// Close `ep`. The other side stays readable until its FIFO is empty, after which
    /// `readable` signals EOF. If both sides are closed, the connection is
    /// cleaned up (slot freed).
    pub fn close(&mut self, ep: Endpoint) {
        if let Some(slot) = self.conns.get_mut(ep.conn as usize) {
            if let Some(c) = slot.as_mut() {
                match ep.side {
                    Side::A => c.a_open = false,
                    Side::B => c.b_open = false,
                }
                if !c.a_open && !c.b_open {
                    *slot = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_without_listener() {
        let mut sw = Switchboard::new();
        assert_eq!(sw.connect("/run/x.sock"), Err(UnixError::ConnRefused));
    }

    #[test]
    fn bind_twice_is_addr_in_use() {
        let mut sw = Switchboard::new();
        assert!(sw.bind_listen("/run/x.sock", 4).is_ok());
        assert_eq!(sw.bind_listen("/run/x.sock", 4), Err(UnixError::AddrInUse));
    }

    #[test]
    fn full_roundtrip_both_directions() {
        let mut sw = Switchboard::new();
        sw.bind_listen("/run/echo.sock", 4).unwrap();
        let client = sw.connect("/run/echo.sock").unwrap();
        assert_eq!(sw.pending("/run/echo.sock"), 1);
        let server = sw.accept("/run/echo.sock").unwrap();
        assert_eq!(sw.pending("/run/echo.sock"), 0);

        // client -> server
        sw.send(client, b"ping").unwrap();
        assert!(sw.readable(server));
        assert_eq!(sw.recv(server, 64).unwrap(), b"ping");
        // server -> client
        sw.send(server, b"pong").unwrap();
        assert_eq!(sw.recv(client, 64).unwrap(), b"pong");
        // drained: nothing left, peers still open => not readable
        assert!(!sw.readable(client));
    }

    #[test]
    fn partial_reads_preserve_order() {
        let mut sw = Switchboard::new();
        sw.bind_listen("/s", 1).unwrap();
        let c = sw.connect("/s").unwrap();
        let s = sw.accept("/s").unwrap();
        sw.send(c, b"abcdef").unwrap();
        assert_eq!(sw.recv(s, 3).unwrap(), b"abc");
        assert_eq!(sw.available(s), 3);
        assert_eq!(sw.recv(s, 100).unwrap(), b"def");
        assert_eq!(sw.available(s), 0);
    }

    #[test]
    fn backlog_is_enforced() {
        let mut sw = Switchboard::new();
        sw.bind_listen("/s", 2).unwrap();
        sw.connect("/s").unwrap();
        sw.connect("/s").unwrap();
        assert_eq!(sw.connect("/s"), Err(UnixError::Backlog));
        // accept frees a slot
        sw.accept("/s").unwrap();
        assert!(sw.connect("/s").is_ok());
    }

    #[test]
    fn close_signals_eof_then_broken_pipe() {
        let mut sw = Switchboard::new();
        sw.bind_listen("/s", 1).unwrap();
        let c = sw.connect("/s").unwrap();
        let s = sw.accept("/s").unwrap();
        sw.send(c, b"hi").unwrap();
        sw.close(c); // client gone
                     // server can still drain buffered bytes...
        assert!(sw.readable(s));
        assert_eq!(sw.recv(s, 64).unwrap(), b"hi");
        // ...and now sees EOF (peer closed, buffer empty)
        assert!(sw.readable(s));
        assert_eq!(sw.recv(s, 64).unwrap(), b"");
        // writing to the closed peer is EPIPE
        assert_eq!(sw.send(s, b"x"), Err(UnixError::BrokenPipe));
    }

    #[test]
    fn unbind_then_refused() {
        let mut sw = Switchboard::new();
        sw.bind_listen("/s", 1).unwrap();
        assert!(sw.is_listening("/s"));
        assert!(sw.unbind("/s"));
        assert!(!sw.is_listening("/s"));
        assert_eq!(sw.connect("/s"), Err(UnixError::ConnRefused));
    }

    #[test]
    fn closing_both_sides_frees_the_slot() {
        let mut sw = Switchboard::new();
        sw.bind_listen("/s", 1).unwrap();
        let c = sw.connect("/s").unwrap();
        let s = sw.accept("/s").unwrap();
        sw.close(c);
        sw.close(s);
        // both closed => the endpoint handles are now invalid
        assert_eq!(sw.send(c, b"x"), Err(UnixError::BadEndpoint));
        assert_eq!(sw.recv(s, 1), Err(UnixError::BadEndpoint));
    }
}
