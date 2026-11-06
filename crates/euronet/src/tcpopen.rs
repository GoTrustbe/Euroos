//! Opening a TCP connection, without I/O: the decisions the kernel's connect
//! and passive open make, kept here so the host can test them.
//!
//! - [`syn_sent_input`]: what a segment means to an active open in SYN-SENT
//!   (RFC 793 §3.4 "Reset Generation" and the SYN-SENT rules of §3.9, with
//!   RFC 5961 §3 for resets). The case that matters in practice is the
//!   half-open peer: a peer that still holds an old control block for the same
//!   four-tuple answers our SYN with a bare ACK, and the answer to that is a RST
//!   with `seq = SEG.ACK`, after which the next SYN goes through.
//! - [`SynSchedule`]: the bounded handshake of an active open: SYN
//!   retransmissions at 1 s, then doubling (RFC 6298 §5 starts at 1 s), and a
//!   fresh port and ISN after two unanswered rounds. A peer may drop a SYN that
//!   collides with an old control block without any answer (BSD stacks such as
//!   slirp do when the SYN's sequence number lies left of their `rcv_nxt`), so
//!   retrying the same four-tuple can never succeed.
//! - [`pick_port`] and [`RecentPorts`]: ephemeral ports chosen at random
//!   (RFC 6056 §3.3.1) but never one in use or closed a short while ago, so a
//!   new connection does not land on a peer's TIME-WAIT or FIN-WAIT-2 block.
//! - [`SynQueue`]: the passive side's SYN-RCVD entries (a bounded table with
//!   SYN-ACK retransmission and expiry), so a listener completes handshakes
//!   without waiting for them.
//!
//! Times are in scheduler ticks (100 Hz in the kernel); the caller supplies `now`.

use alloc::vec::Vec;

use crate::tcp::{ACK, RST, SYN};

/// What an incoming segment means to a connection in SYN-SENT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynSent {
    /// SYN+ACK acknowledging our SYN: established; `irs` is the peer's ISN.
    Established { irs: u32 },
    /// An acceptable RST: the port is closed (connection refused).
    Refused,
    /// An ACK that does not acknowledge our SYN (a half-open connection at the
    /// peer, RFC 793 Figure 10): send `<SEQ=seq><CTL=RST>` and keep waiting.
    SendReset { seq: u32 },
    /// Nothing for us (a reset without an acceptable ACK, a bare SYN, data).
    Ignore,
}

/// RFC 793 SYN-SENT processing for one segment, our initial sequence `iss`.
pub fn syn_sent_input(iss: u32, flags: u8, seq: u32, ack: u32) -> SynSent {
    let has = |f: u8| flags & f != 0;
    // Our SYN occupies iss; the only acceptable ACK is iss + 1 (SND.NXT).
    let ack_ok = ack == iss.wrapping_add(1);
    if has(ACK) && !ack_ok {
        // "If SEG.ACK =< ISS, or SEG.ACK > SND.NXT, send a reset (unless the
        // RST bit is set, if so drop the segment and return)".
        if has(RST) {
            return SynSent::Ignore;
        }
        return SynSent::SendReset { seq: ack };
    }
    if has(RST) {
        // With an acceptable ACK the peer refused; without one, drop it.
        return if has(ACK) { SynSent::Refused } else { SynSent::Ignore };
    }
    if has(SYN) && has(ACK) {
        return SynSent::Established { irs: seq };
    }
    SynSent::Ignore
}

/// The bounded active open: per round a SYN and a wait.
pub struct SynSchedule;

impl SynSchedule {
    /// Rounds (SYN transmissions) before the connect gives up.
    pub const ROUNDS: usize = 4;
    /// Before this round the connection takes a new port and ISN.
    pub const NEW_IDENTITY_AT: usize = 2;
    /// Ticks to wait for an answer in round `round` (0-based): 1 s, 2 s, then
    /// the same again for the new identity. 6 s in all.
    pub fn wait_ticks(round: usize) -> u64 {
        let r = round % Self::NEW_IDENTITY_AT;
        100 << r
    }
    /// Does round `round` start with a fresh port and ISN?
    pub fn new_identity(round: usize) -> bool {
        round == Self::NEW_IDENTITY_AT
    }
    /// Upper bound of the whole handshake in ticks.
    pub fn total_ticks() -> u64 {
        (0..Self::ROUNDS).map(Self::wait_ticks).sum()
    }
}

/// The last `N` ports handed out, to keep them out of the next picks.
pub struct RecentPorts<const N: usize> {
    ring: [u16; N],
    next: usize,
}

impl<const N: usize> RecentPorts<N> {
    pub const fn new() -> Self {
        RecentPorts { ring: [0; N], next: 0 }
    }
    pub fn contains(&self, port: u16) -> bool {
        port != 0 && self.ring.contains(&port)
    }
    pub fn remember(&mut self, port: u16) {
        if N == 0 {
            return;
        }
        self.ring[self.next] = port;
        self.next = (self.next + 1) % N;
    }
}

impl<const N: usize> Default for RecentPorts<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// A random port in `lo..hi` for which `avoid` is false. A handful of random
/// draws first; then a scan from a random start, so the result is always a
/// free port when one exists. Only when every port is to be avoided does it
/// return a random one anyway.
pub fn pick_port(mut rand: impl FnMut() -> u32, lo: u16, hi: u16, avoid: impl Fn(u16) -> bool) -> u16 {
    let span = (hi as u32).saturating_sub(lo as u32).max(1);
    for _ in 0..32 {
        let p = lo + (rand() % span) as u16;
        if !avoid(p) {
            return p;
        }
    }
    let start = rand() % span;
    for i in 0..span {
        let p = lo + ((start + i) % span) as u16;
        if !avoid(p) {
            return p;
        }
    }
    lo + start as u16
}

/// One half-open passive connection (SYN received, SYN-ACK sent).
#[derive(Debug)]
pub struct Embryo<K, T> {
    pub key: K,
    /// Our initial sequence number (the SYN-ACK's seq).
    pub iss: u32,
    /// The peer's initial sequence number (its SYN's seq).
    pub irs: u32,
    /// SYN-ACKs sent so far.
    pub tries: u8,
    /// Tick at which the next SYN-ACK retransmission (or expiry) is due.
    pub due: u64,
    /// Whatever the caller keeps with it (the kernel: the connection itself).
    pub data: T,
}

/// What to do about a SYN for a listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynAction {
    /// A new half-open entry was created: send its SYN-ACK.
    New,
    /// A retransmitted SYN for an entry we already have: send the SYN-ACK again.
    Retransmit,
    /// The table is full: drop the SYN (the peer retransmits it).
    Full,
}

/// What a due timer asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Retransmit,
    Expired,
}

/// The SYN-RCVD table of all listeners: bounded, and every entry carries its
/// own SYN-ACK retransmission timer.
pub struct SynQueue<K, T> {
    entries: Vec<Embryo<K, T>>,
    cap: usize,
}

impl<K: PartialEq + Copy, T> SynQueue<K, T> {
    /// SYN-ACKs sent before an entry is given up (1 + 3 retransmissions).
    pub const MAX_TRIES: u8 = 4;

    pub const fn new(cap: usize) -> Self {
        SynQueue { entries: Vec::new(), cap }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn count(&self, pred: impl Fn(&K) -> bool) -> usize {
        self.entries.iter().filter(|e| pred(&e.key)).count()
    }
    pub fn get(&self, key: &K) -> Option<&Embryo<K, T>> {
        self.entries.iter().find(|e| e.key == *key)
    }
    pub fn get_mut(&mut self, key: &K) -> Option<&mut Embryo<K, T>> {
        self.entries.iter_mut().find(|e| e.key == *key)
    }
    pub fn keys(&self) -> Vec<K> {
        self.entries.iter().map(|e| e.key).collect()
    }
    /// Retransmission delay after the `tries`-th SYN-ACK: 1, 2, 4, 8 s.
    pub fn rto(tries: u8) -> u64 {
        100u64 << (tries.saturating_sub(1).min(4))
    }

    /// A SYN for `key`. `make` builds the entry's data for a new one (its ISN
    /// and data); `room` is false when the listener's backlog is used up.
    pub fn on_syn(&mut self, key: K, irs: u32, now: u64, room: bool, make: impl FnOnce() -> (u32, T)) -> SynAction {
        if let Some(e) = self.entries.iter_mut().find(|e| e.key == key) {
            if e.irs == irs {
                return SynAction::Retransmit;
            }
            // A new SYN on the same four-tuple with another ISN: the peer gave up
            // the old attempt. Start over.
            let (iss, data) = make();
            *e = Embryo { key, iss, irs, tries: 1, due: now + Self::rto(1), data };
            return SynAction::New;
        }
        if !room || self.entries.len() >= self.cap {
            return SynAction::Full;
        }
        let (iss, data) = make();
        self.entries.push(Embryo { key, iss, irs, tries: 1, due: now + Self::rto(1), data });
        SynAction::New
    }

    /// The SYN-ACK for `key` was sent again (after [`Due::Retransmit`] or a
    /// retransmitted SYN): count it and rearm.
    pub fn sent_again(&mut self, key: &K, now: u64) {
        if let Some(e) = self.get_mut(key) {
            e.tries = e.tries.saturating_add(1);
            e.due = now + Self::rto(e.tries);
        }
    }

    /// The handshake's final ACK: `ack` must be iss + 1. Removes and returns the
    /// entry when it completes the handshake.
    pub fn on_ack(&mut self, key: &K, ack: u32) -> Option<Embryo<K, T>> {
        let i = self.entries.iter().position(|e| e.key == *key)?;
        if ack != self.entries[i].iss.wrapping_add(1) {
            return None;
        }
        Some(self.entries.swap_remove(i))
    }

    /// A reset for `key`: honoured at the exact next sequence (RFC 5961), which
    /// in SYN-RCVD is irs + 1. Removes and returns the entry when accepted.
    pub fn on_rst(&mut self, key: &K, seq: u32) -> Option<Embryo<K, T>> {
        let i = self.entries.iter().position(|e| e.key == *key)?;
        if seq != self.entries[i].irs.wrapping_add(1) {
            return None;
        }
        Some(self.entries.swap_remove(i))
    }

    /// Remove an entry outright (its listener closed).
    pub fn remove(&mut self, key: &K) -> Option<Embryo<K, T>> {
        let i = self.entries.iter().position(|e| e.key == *key)?;
        Some(self.entries.swap_remove(i))
    }

    /// Timers due at `now`: the keys that need their SYN-ACK again, and the
    /// entries that ran out of tries (removed and returned).
    pub fn due(&mut self, now: u64) -> (Vec<K>, Vec<Embryo<K, T>>) {
        let mut again = Vec::new();
        let mut expired = Vec::new();
        let mut i = 0;
        while i < self.entries.len() {
            let e = &self.entries[i];
            if now >= e.due {
                if e.tries >= Self::MAX_TRIES {
                    expired.push(self.entries.swap_remove(i));
                    continue;
                }
                again.push(e.key);
            }
            i += 1;
        }
        (again, expired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tcp::FIN;

    #[test]
    fn syn_ack_with_the_right_ack_establishes() {
        assert_eq!(syn_sent_input(1000, SYN | ACK, 77, 1001), SynSent::Established { irs: 77 });
    }

    #[test]
    fn stale_ack_from_a_half_open_peer_asks_for_a_reset_at_its_ack() {
        // slirp's answer to a SYN that hits an old FIN-WAIT-2 block: a bare ACK
        // with the old connection's numbers.
        assert_eq!(syn_sent_input(1000, ACK, 5, 0xdead_beef), SynSent::SendReset { seq: 0xdead_beef });
        // A SYN+ACK that acknowledges something else is the same case.
        assert_eq!(syn_sent_input(1000, SYN | ACK, 5, 999), SynSent::SendReset { seq: 999 });
        // ack == iss (one too low) is unacceptable too.
        assert_eq!(syn_sent_input(1000, ACK, 5, 1000), SynSent::SendReset { seq: 1000 });
    }

    #[test]
    fn resets_count_only_with_an_acceptable_ack() {
        assert_eq!(syn_sent_input(1000, RST | ACK, 0, 1001), SynSent::Refused);
        assert_eq!(syn_sent_input(1000, RST | ACK, 0, 4), SynSent::Ignore);
        assert_eq!(syn_sent_input(1000, RST, 0, 0), SynSent::Ignore);
    }

    #[test]
    fn other_segments_are_ignored_in_syn_sent() {
        assert_eq!(syn_sent_input(1000, SYN, 5, 0), SynSent::Ignore);
        assert_eq!(syn_sent_input(1000, FIN, 5, 0), SynSent::Ignore);
        assert_eq!(syn_sent_input(u32::MAX, SYN | ACK, 9, 0), SynSent::Established { irs: 9 });
    }

    #[test]
    fn the_handshake_is_bounded_and_changes_identity_once() {
        let waits: Vec<u64> = (0..SynSchedule::ROUNDS).map(SynSchedule::wait_ticks).collect();
        assert_eq!(waits, [100, 200, 100, 200]);
        assert_eq!(SynSchedule::total_ticks(), 600, "6 s at 100 Hz, not 4 x 12 M spins");
        let fresh: Vec<usize> = (0..SynSchedule::ROUNDS).filter(|&r| SynSchedule::new_identity(r)).collect();
        assert_eq!(fresh, [2]);
    }

    #[test]
    fn recent_ports_remember_the_last_n() {
        let mut r: RecentPorts<3> = RecentPorts::new();
        assert!(!r.contains(0), "port 0 is never 'recent'");
        for p in [50000, 50001, 50002] {
            r.remember(p);
        }
        assert!(r.contains(50000) && r.contains(50002));
        r.remember(50003);
        assert!(!r.contains(50000), "the oldest falls out");
        assert!(r.contains(50003));
    }

    #[test]
    fn pick_port_skips_avoided_ports_and_finds_the_last_free_one() {
        let mut x = 7u32;
        let mut rand = move || {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            x
        };
        // Everything but one port is taken: the scan finds it.
        let p = pick_port(&mut rand, 49152, 49252, |p| p != 49200);
        assert_eq!(p, 49200);
        // Avoid an odd set: the result is in range and not avoided.
        for _ in 0..1000 {
            let p = pick_port(&mut rand, 49152, 65000, |p| p % 3 == 0);
            assert!((49152..65000).contains(&p) && p % 3 != 0);
        }
        // Nothing free: still a port in range.
        let p = pick_port(&mut rand, 49152, 49160, |_| true);
        assert!((49152..49160).contains(&p));
    }

    #[test]
    fn a_constant_random_source_still_avoids_ports() {
        let p = pick_port(|| 0, 49152, 49160, |p| p == 49152);
        assert_ne!(p, 49152);
    }

    type Key = (u32, u16, u16);

    #[test]
    fn syn_queue_creates_retransmits_and_completes() {
        let mut q: SynQueue<Key, u8> = SynQueue::new(4);
        let k = (1, 40000, 80);
        assert_eq!(q.on_syn(k, 500, 0, true, || (9000, 1)), SynAction::New);
        // The peer retransmits its SYN: the same entry, the SYN-ACK again.
        assert_eq!(q.on_syn(k, 500, 10, true, || panic!("no new entry")), SynAction::Retransmit);
        assert_eq!(q.len(), 1);
        // A wrong ACK does not complete it.
        assert!(q.on_ack(&k, 9000).is_none());
        let e = q.on_ack(&k, 9001).expect("ACK of iss+1 completes the handshake");
        assert_eq!((e.iss, e.irs, e.data), (9000, 500, 1));
        assert!(q.is_empty());
    }

    #[test]
    fn syn_queue_is_bounded_by_capacity_and_backlog() {
        let mut q: SynQueue<Key, ()> = SynQueue::new(2);
        assert_eq!(q.on_syn((1, 1, 80), 1, 0, true, || (1, ())), SynAction::New);
        assert_eq!(q.on_syn((1, 2, 80), 1, 0, false, || (1, ())), SynAction::Full, "no backlog room");
        assert_eq!(q.on_syn((1, 3, 80), 1, 0, true, || (1, ())), SynAction::New);
        assert_eq!(q.on_syn((1, 4, 80), 1, 0, true, || (1, ())), SynAction::Full, "table full");
        assert_eq!(q.count(|k| k.2 == 80), 2);
    }

    #[test]
    fn a_new_isn_on_the_same_tuple_restarts_the_entry() {
        let mut q: SynQueue<Key, u8> = SynQueue::new(2);
        let k = (1, 40000, 80);
        q.on_syn(k, 500, 0, true, || (9000, 1));
        assert_eq!(q.on_syn(k, 777, 5, true, || (4000, 2)), SynAction::New);
        let e = q.get(&k).unwrap();
        assert_eq!((e.iss, e.irs, e.tries, e.data), (4000, 777, 1, 2));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn syn_ack_retransmits_back_off_and_the_entry_expires() {
        let mut q: SynQueue<Key, ()> = SynQueue::new(4);
        let k = (1, 40000, 80);
        q.on_syn(k, 500, 0, true, || (9000, ()));
        let mut now = 0;
        let mut sent = vec![0u64];
        loop {
            now += 1;
            let (again, expired) = q.due(now);
            if !expired.is_empty() {
                break;
            }
            for key in again {
                q.sent_again(&key, now);
                sent.push(now);
            }
            assert!(now < 10_000);
        }
        // SYN-ACKs at 0, 1 s, 3 s, 7 s; given up at 15 s.
        assert_eq!(sent, [0, 100, 300, 700]);
        assert_eq!(now, 1500);
        assert!(q.is_empty());
    }

    #[test]
    fn resets_in_syn_rcvd_need_the_exact_sequence() {
        let mut q: SynQueue<Key, ()> = SynQueue::new(4);
        let k = (1, 40000, 80);
        q.on_syn(k, 500, 0, true, || (9000, ()));
        assert!(q.on_rst(&k, 12345).is_none(), "blind reset ignored");
        assert!(q.on_rst(&k, 501).is_some());
        assert!(q.is_empty());
    }
}
