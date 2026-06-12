//! AF_UNIX — Unix-domain (lokale) sockets (H1).
//!
//! Een **lokale**, betrouwbare, in-kernel byte-stroom tussen twee processen op
//! dezelfde machine, geadresseerd via een padnaam (`/run/foo.sock`) i.p.v. een
//! IP-poort. De bouwsteen voor de live display-server (H2: compositor ↔ app) en
//! IPC-zware apps — géén netwerk, dus geen checksums/retransmissie, puur
//! gegarandeerde, geordende bytes.
//!
//! Model: één centrale [`Switchboard`] bezit *alle* verbindingen, zodat er geen
//! gedeeld eigenaarschap (Rc/Arc) nodig is — in de kernel staat hij achter één
//! Mutex. Een endpoint is een lichte [`Endpoint`]-handle `(conn, kant)`; de twee
//! kanten A/B van een verbinding delen twee gekruiste byte-FIFO's. `no_std`+alloc,
//! volledig op de host testbaar (geen NIC/QEMU).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

/// Welke kant van een verbinding een endpoint is. Kant A is de **client**
/// (`connect`), kant B de **server** (`accept`). A schrijft in `a_to_b` en leest
/// uit `b_to_a`; B andersom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    A,
    B,
}

/// Lichte verwijzing naar één kant van één verbinding. Kopieerbaar — past in de
/// kernel-SOCKETS-tabel zonder eigenaarschap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    conn: u32,
    side: Side,
}

/// Eén bidirectionele verbinding: twee byte-FIFO's + een open-vlag per kant.
struct Conn {
    a_to_b: VecDeque<u8>,
    b_to_a: VecDeque<u8>,
    a_open: bool,
    b_open: bool,
}

/// Een luisteraar gebonden aan een pad, met een wachtrij van nog-niet-geaccepteerde
/// server-endpoints (de B-kanten van inkomende `connect`-aanvragen).
struct Listener {
    backlog: usize,
    pending: VecDeque<Endpoint>,
}

/// Foutcodes — bewust dezelfde betekenis als de POSIX-errno's die de Linux-ABI
/// terugmoet geven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnixError {
    /// Geen luisteraar op dat pad (ECONNREFUSED).
    ConnRefused,
    /// Pad al in gebruik (EADDRINUSE).
    AddrInUse,
    /// Accept-wachtrij vol (de backlog is bereikt).
    Backlog,
    /// Ongeldig/gesloten endpoint (EBADF).
    BadEndpoint,
    /// Andere kant is dicht (EPIPE) — bij schrijven.
    BrokenPipe,
}

/// De centrale lokale-socket-schakelaar. Bezit alle luisteraars + verbindingen.
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

    /// Bind + luister op `path`. Eén stap (zoals een AF_UNIX-server die `bind`
    /// dan `listen` doet). Fout als het pad al een luisteraar heeft.
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

    /// Verwijder de luisteraar op `path` (al-geaccepteerde verbindingen blijven
    /// leven; nog-wachtende worden verworpen). Geeft `true` als er één was.
    pub fn unbind(&mut self, path: &str) -> bool {
        self.listeners.remove(path).is_some()
    }

    /// Is er een luisteraar op dit pad?
    pub fn is_listening(&self, path: &str) -> bool {
        self.listeners.contains_key(path)
    }

    /// Client-zijde: verbind met de luisteraar op `path`. Maakt een nieuwe
    /// verbinding, zet de **server**-kant (B) in de accept-wachtrij, en geeft de
    /// **client**-kant (A) terug. Fout als er geen luisteraar is of de backlog vol is.
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

    /// Server-zijde: accepteer de oudste wachtende verbinding op `path`. Geeft de
    /// **server**-kant (B), of `None` als de wachtrij leeg is (niet-blokkerend).
    pub fn accept(&mut self, path: &str) -> Option<Endpoint> {
        self.listeners.get_mut(path)?.pending.pop_front()
    }

    /// Aantal wachtende (nog te accepteren) verbindingen op `path`.
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

    /// Schrijf bytes vanaf `ep`. A schrijft richting B en omgekeerd. Fout als de
    /// andere kant dicht is (EPIPE). Geeft het aantal geschreven bytes (alles).
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

    /// Lees tot `max` bytes voor `ep` uit zijn inkomende FIFO. A leest `b_to_a`,
    /// B leest `a_to_b`. Niet-blokkerend: geeft een (mogelijk lege) Vec.
    pub fn recv(&mut self, ep: Endpoint, max: usize) -> Result<Vec<u8>, UnixError> {
        let c = self.conn_mut(ep)?;
        let q = match ep.side {
            Side::A => &mut c.b_to_a,
            Side::B => &mut c.a_to_b,
        };
        let n = max.min(q.len());
        Ok(q.drain(..n).collect())
    }

    /// Is `ep` LEESBAAR? (inkomende data aanwezig, óf de andere kant is dicht →
    /// EOF is leesbaar zodat `poll`/`recv` 0 teruggeeft i.p.v. eeuwig wachten.)
    pub fn readable(&self, ep: Endpoint) -> bool {
        let Ok(c) = self.conn_ref(ep) else {
            return false;
        };
        match ep.side {
            Side::A => !c.b_to_a.is_empty() || !c.b_open,
            Side::B => !c.a_to_b.is_empty() || !c.a_open,
        }
    }

    /// Aantal direct leesbare bytes voor `ep` (zonder EOF).
    pub fn available(&self, ep: Endpoint) -> usize {
        self.conn_ref(ep).map_or(0, |c| match ep.side {
            Side::A => c.b_to_a.len(),
            Side::B => c.a_to_b.len(),
        })
    }

    /// Sluit `ep`. De andere kant blijft leesbaar tot zijn FIFO leeg is, daarna
    /// signaleert `readable` EOF. Als beide kanten dicht zijn, wordt de verbinding
    /// opgeruimd (slot vrijgegeven).
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
