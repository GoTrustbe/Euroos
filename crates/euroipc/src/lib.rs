//! **EuroIPC core**: the port/channel table behind the EuroIPC syscalls, with
//! **per-channel locks** (J1 fine-grained locking).
//!
//! The kernel module used to keep every port and every queued message in one
//! `Vec` behind one mutex, and appended a formatted audit line (under a second
//! global mutex) on every send. Two unrelated process pairs talking on two
//! unrelated ports therefore serialised on each other twice per message.
//!
//! Here:
//! - the **channel table** is a per-CPU replicated read-mostly lock
//!   (`eurolock::sync::ReadMostly`, class `IpcTable`), written only by
//!   `register`; `send`/`recv` read their own core's replica just long enough to
//!   find the channel and clone its `Arc`, so lookups share no cache line.
//! - **each channel has its own lock** (class `IpcChannel`) around its message
//!   queue.
//! - the **audit trail** is a lock-free ring of fixed-size records per core (a
//!   per-slot sequence number makes a torn read detectable and skipped), merged
//!   by timestamp and formatted only when someone reads it.
//!
//! All locks are `Mode::Irq`: EuroIPC runs inside syscalls (IF=0 via FMASK) and,
//! with the driver work, may be fed from interrupt context, so by the IRQSAVE
//! rule every holder runs with interrupts off. Lock order: `IpcTable` (released)
//! → `IpcChannel` → `Heap`; no path holds two channels.
//!
//! Semantics are those of the original kernel bus: a port has one owner; `send`
//! tags each message with the sender pid; `recv` takes the oldest message from
//! the receiver's (first) port; the permission check is still the open hook.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicU64, Ordering};

use eurolock::sync::{cpu_shard, Mutex, ReadMostly, Replica};
use eurolock::{LockClass, LockSite, Mode, SHARDS};

pub static SITE_TABLE: LockSite = LockSite::new("ipc.channel_table", LockClass::IpcTable);
pub static SITE_CHANNEL: LockSite = LockSite::new("ipc.channel (per-port)", LockClass::IpcChannel);

/// `-ESRCH`: no such port / the receiver owns no port.
pub const ESRCH: i64 = -3;
/// `-EAGAIN`: nothing queued.
pub const EAGAIN: i64 = -11;

struct Channel {
    port: u32,
    owner: u64,
    /// `NO_RESTRICTION`, or the only sender pid this port accepts.
    only_from: AtomicU64,
    queue: Mutex<VecDeque<(u64, Vec<u8>)>>,
}

const NO_RESTRICTION: u64 = u64::MAX;

/// Messages one port may hold; further sends are refused (`-EAGAIN`) and counted.
pub const MAX_QUEUE: usize = 256;
/// `-EPERM`: a restricted port refused this sender.
pub const EPERM: i64 = -1;

/// Why a `recv` returned nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecvError {
    /// The receiver owns no port (`-ESRCH`).
    NoPort,
    /// The port exists but its queue is empty (`-EAGAIN`).
    Empty,
}

impl RecvError {
    pub fn errno(self) -> i64 {
        match self {
            RecvError::NoPort => ESRCH,
            RecvError::Empty => EAGAIN,
        }
    }
}

/// One audit event, as stored in the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditEvent {
    Claimed { pid: u64, port: u32, restricted: bool },
    Sent { sender: u64, port: u32, owner: u64, len: u64 },
    Refused { sender: u64, port: u32 },
}

impl AuditEvent {
    pub fn line(&self) -> String {
        match *self {
            AuditEvent::Claimed { pid, port, restricted } => {
                alloc::format!("pid {pid} claimed port {port}{}", if restricted { " (restricted)" } else { "" })
            }
            AuditEvent::Refused { sender, port } => alloc::format!("pid {sender} -> port {port}: REFUSED (restricted port)"),
            AuditEvent::Sent { sender, port, owner, len } => {
                alloc::format!("pid {sender} -> port {port} (pid {owner}): {len} bytes")
            }
        }
    }
}

const RING: usize = 64;

/// Lock-free audit ring (a display ring for the system window, like the old
/// six-line buffer; the tamper-evident audit chain is `kernel/src/audit.rs`).
/// One ring per CPU shard, so senders on different cores never write the same
/// cache lines; a reader merges the shards by timestamp. Writers claim a slot
/// with one `fetch_add` on their own ring; each slot has a sequence word that is
/// odd while being written and `2*ticket+2` when done, so a reader can tell a
/// complete record of the right generation from a torn or stale one (and skips
/// those).
#[repr(align(64))]
struct AuditRing {
    head: AtomicU64,
    // per slot: [seq, stamp, kind, a, b, c, d]
    slots: [[AtomicU64; 7]; RING],
}

impl AuditRing {
    const fn new() -> Self {
        AuditRing { head: AtomicU64::new(0), slots: [const { [const { AtomicU64::new(0) }; 7] }; RING] }
    }

    fn push(&self, stamp: u64, ev: AuditEvent) {
        let ticket = self.head.fetch_add(1, Ordering::AcqRel);
        let s = &self.slots[(ticket % RING as u64) as usize];
        s[0].store(2 * ticket + 1, Ordering::Relaxed);
        fence(Ordering::Release); // the odd marker is visible before any field changes
        let (k, a, b, c, d) = match ev {
            AuditEvent::Claimed { pid, port, restricted } => (1, pid, port as u64, 0, restricted as u64),
            AuditEvent::Sent { sender, port, owner, len } => (2, sender, port as u64, owner, len),
            AuditEvent::Refused { sender, port } => (3, sender, port as u64, 0, 0),
        };
        s[1].store(stamp, Ordering::Relaxed);
        s[2].store(k, Ordering::Relaxed);
        s[3].store(a, Ordering::Relaxed);
        s[4].store(b, Ordering::Relaxed);
        s[5].store(c, Ordering::Relaxed);
        s[6].store(d, Ordering::Relaxed);
        s[0].store(2 * ticket + 2, Ordering::Release);
    }

    /// Up to the most recent `n` complete events of this ring: (stamp, event).
    fn recent(&self, n: usize, out: &mut Vec<(u64, AuditEvent)>) {
        let head = self.head.load(Ordering::Acquire);
        let n = (n as u64).min(head).min(RING as u64);
        for ticket in head - n..head {
            let s = &self.slots[(ticket % RING as u64) as usize];
            let seq = s[0].load(Ordering::Acquire);
            if seq != 2 * ticket + 2 {
                continue; // still being written, or already overwritten
            }
            let st = s[1].load(Ordering::Relaxed);
            let k = s[2].load(Ordering::Relaxed);
            let a = s[3].load(Ordering::Relaxed);
            let b = s[4].load(Ordering::Relaxed);
            let c = s[5].load(Ordering::Relaxed);
            let d = s[6].load(Ordering::Relaxed);
            fence(Ordering::Acquire); // field loads complete before the re-check
            if s[0].load(Ordering::Relaxed) != seq {
                continue; // overwritten while we read
            }
            out.push((
                st,
                match k {
                    1 => AuditEvent::Claimed { pid: a, port: b as u32, restricted: d != 0 },
                    3 => AuditEvent::Refused { sender: a, port: b as u32 },
                    _ => AuditEvent::Sent { sender: a, port: b as u32, owner: c, len: d },
                },
            ));
        }
    }
}

#[repr(align(64))]
struct PaddedCounter(AtomicU64);

/// Default audit clock for hosts without a better one: a global counter (gives
/// a total order; the kernel passes its TSC instead, which needs no shared line).
fn counter_stamp() -> u64 {
    static C: AtomicU64 = AtomicU64::new(0);
    C.fetch_add(1, Ordering::Relaxed)
}

/// The EuroIPC port table (see crate docs).
pub struct ChannelTable {
    table: ReadMostly<Vec<Arc<Channel>>>,
    audit: [AuditRing; SHARDS],
    sends: [PaddedCounter; SHARDS],
    stamp: fn() -> u64,
    /// Sends refused because the queue was full / the sender was not accepted.
    pub dropped_full: AtomicU64,
    pub refused_sender: AtomicU64,
}

impl Default for ChannelTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelTable {
    pub const fn new() -> Self {
        Self::with_clock(counter_stamp)
    }

    /// A table whose audit records are ordered by `stamp` (monotonic per core,
    /// comparable across cores, e.g. the TSC).
    pub const fn with_clock(stamp: fn() -> u64) -> Self {
        ChannelTable {
            table: ReadMostly::new_site(&SITE_TABLE, Mode::Irq, [const { Replica::new(Vec::new()) }; SHARDS]),
            audit: [const { AuditRing::new() }; SHARDS],
            sends: [const { PaddedCounter(AtomicU64::new(0)) }; SHARDS],
            stamp,
            dropped_full: AtomicU64::new(0),
            refused_sender: AtomicU64::new(0),
        }
    }

    fn audit(&self, ev: AuditEvent) {
        self.audit[cpu_shard()].push((self.stamp)(), ev);
    }

    /// Claim `port` for `pid`. `true` = claimed, `false` = already in use.
    pub fn register(&self, pid: u64, port: u32) -> bool {
        self.claim(pid, port, None)
    }

    /// Claim `port` for `pid`, accepting messages only from `only_from` (the
    /// userspace-driver ports).
    pub fn register_restricted(&self, pid: u64, port: u32, only_from: u64) -> bool {
        self.claim(pid, port, Some(only_from))
    }

    fn claim(&self, pid: u64, port: u32, only_from: Option<u64>) -> bool {
        let ch = Arc::new(Channel {
            port,
            owner: pid,
            only_from: AtomicU64::new(only_from.unwrap_or(NO_RESTRICTION)),
            queue: Mutex::new_site(&SITE_CHANNEL, Mode::Irq, VecDeque::new()),
        });
        let claimed = self.table.write(|t| {
            if t.iter().any(|c| c.port == port) {
                false
            } else {
                t.push(ch.clone());
                true
            }
        });
        if claimed {
            self.audit(AuditEvent::Claimed { pid, port, restricted: only_from.is_some() });
        }
        claimed
    }

    /// Change which sender a restricted port accepts (a restarted driver has a
    /// new pid). Lock-free: the restriction is an atomic on the channel.
    pub fn set_only_from(&self, port: u32, only_from: u64) {
        if let Some(c) = self.by_port(port) {
            c.only_from.store(only_from, Ordering::Release);
        }
    }

    /// Release a port and drop its queue (its owner died). A send racing with
    /// this finishes on the detached channel and is dropped with it.
    pub fn unregister(&self, port: u32) {
        let gone = self.table.write(|t| t.iter().position(|c| c.port == port).map(|i| t.remove(i)));
        drop(gone); // the last reference goes outside the table lock
    }

    fn by_port(&self, port: u32) -> Option<Arc<Channel>> {
        self.table.read(|t| t.iter().find(|c| c.port == port).cloned())
    }

    fn by_owner(&self, pid: u64) -> Option<Arc<Channel>> {
        self.table.read(|t| t.iter().find(|c| c.owner == pid).cloned())
    }

    /// Queue `data` on `port`, tagged with `sender` (audited). Returns the byte
    /// count, or `Err(ESRCH)` (no such port), `Err(EPERM)` (a restricted port
    /// refused this sender) or `Err(EAGAIN)` (queue full, `MAX_QUEUE`).
    pub fn send(&self, sender: u64, port: u32, data: &[u8]) -> Result<usize, i64> {
        self.send_with(sender, port, data, true)
    }

    /// Like [`send`](Self::send); `audit_it = false` for a data path that would
    /// otherwise flood the audit ring (every network frame of a userspace NIC).
    pub fn send_with(&self, sender: u64, port: u32, data: &[u8], audit_it: bool) -> Result<usize, i64> {
        let ch = self.by_port(port).ok_or(ESRCH)?;
        let only = ch.only_from.load(Ordering::Acquire);
        if only != NO_RESTRICTION && only != sender {
            self.refused_sender.fetch_add(1, Ordering::Relaxed);
            self.audit(AuditEvent::Refused { sender, port });
            return Err(EPERM);
        }
        // Permission-check hook for unrestricted ports (still: allowed);
        // EuroGuard policy comes later.
        let msg = data.to_vec(); // allocate BEFORE taking the channel lock
        {
            let mut q = ch.queue.lock();
            if q.len() >= MAX_QUEUE {
                drop(q);
                self.dropped_full.fetch_add(1, Ordering::Relaxed);
                return Err(EAGAIN);
            }
            q.push_back((sender, msg));
        }
        self.sends[cpu_shard()].0.fetch_add(1, Ordering::Relaxed);
        if audit_it {
            self.audit(AuditEvent::Sent { sender, port, owner: ch.owner, len: data.len() as u64 });
        }
        Ok(data.len())
    }

    /// Take the oldest message of `port` (by port number, for kernel-side
    /// receivers): (sender, data).
    pub fn take(&self, port: u32) -> Option<(u64, Vec<u8>)> {
        let ch = self.by_port(port)?;
        let m = ch.queue.lock().pop_front();
        m
    }

    /// Take the oldest message from `pid`'s port: (sender, data).
    pub fn recv(&self, pid: u64) -> Result<(u64, Vec<u8>), RecvError> {
        let ch = self.by_owner(pid).ok_or(RecvError::NoPort)?;
        let m = ch.queue.lock().pop_front();
        m.ok_or(RecvError::Empty)
    }

    /// Messages queued on `port` (None = no such port).
    pub fn queued(&self, port: u32) -> Option<usize> {
        self.by_port(port).map(|c| c.queue.lock().len())
    }

    /// The last `n` audit events (all cores merged by timestamp), oldest first.
    pub fn audit_recent(&self, n: usize) -> Vec<AuditEvent> {
        let mut all = Vec::new();
        for r in &self.audit {
            r.recent(n, &mut all);
        }
        all.sort_by_key(|(st, _)| *st);
        let skip = all.len().saturating_sub(n);
        all.into_iter().skip(skip).map(|(_, e)| e).collect()
    }

    /// The last `n` audit events as text lines.
    pub fn audit_lines(&self, n: usize) -> Vec<String> {
        self.audit_recent(n).iter().map(|e| e.line()).collect()
    }

    pub fn total_sends(&self) -> u64 {
        self.sends.iter().map(|c| c.0.load(Ordering::Relaxed)).sum()
    }

    pub fn channels(&self) -> usize {
        self.table.read(|t| t.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::vec;

    // Host stand-in for the kernel hooks: each test thread is a "core" with its
    // own shard, so the per-CPU replicas, counters and audit rings are really
    // exercised from different shards.
    struct ThreadCpus;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::thread_local! {
        static CPU: usize = (NEXT.fetch_add(1, Ordering::Relaxed) % SHARDS as u64) as usize;
    }
    impl eurolock::LockHooks for ThreadCpus {
        fn irq_save(&self) -> bool {
            true
        }
        fn irq_restore(&self, _: bool) {}
        fn on_acquire(&self, _: LockClass, _: Mode, _: bool) {}
        fn on_release(&self, _: LockClass, _: Mode) {}
        fn cpu(&self) -> usize {
            CPU.with(|c| *c)
        }
    }
    static HOOKS: ThreadCpus = ThreadCpus;
    fn setup() {
        eurolock::sync::install_hooks(&HOOKS);
    }

    #[test]
    fn audit_merges_all_cores_in_time_order() {
        setup();
        let t = Arc::new(ChannelTable::new());
        t.register(1, 1);
        // Senders on 4 threads (4 shards), strictly one after another, so the
        // global order is known: thread k sends k*10..k*10+9.
        for k in 0..4u64 {
            let t = t.clone();
            std::thread::spawn(move || {
                for i in 0..10 {
                    t.send(k * 10 + i, 1, b"").unwrap();
                }
            })
            .join()
            .unwrap();
        }
        let ev = t.audit_recent(25);
        assert_eq!(ev.len(), 25);
        let senders: Vec<u64> = ev
            .iter()
            .map(|e| match e {
                AuditEvent::Sent { sender, .. } => *sender,
                _ => panic!("only sends in the last 25"),
            })
            .collect();
        assert_eq!(senders, (15..40).collect::<Vec<u64>>(), "merged across shards in send order");
        assert_eq!(t.total_sends(), 40);
    }

    #[test]
    fn register_send_recv_roundtrip() {
        let t = ChannelTable::new();
        assert!(t.register(10, 500));
        assert!(!t.register(11, 500), "a port has one owner");
        assert_eq!(t.recv(10), Err(RecvError::Empty));
        assert_eq!(t.recv(99), Err(RecvError::NoPort));
        assert_eq!(t.send(7, 501, b"x"), Err(ESRCH));
        assert_eq!(t.send(7, 500, b"hello"), Ok(5));
        assert_eq!(t.send(8, 500, b"world"), Ok(5));
        assert_eq!(t.queued(500), Some(2));
        assert_eq!(t.recv(10), Ok((7, b"hello".to_vec())));
        assert_eq!(t.recv(10), Ok((8, b"world".to_vec())));
        assert_eq!(RecvError::Empty.errno(), -11);
        assert_eq!(RecvError::NoPort.errno(), -3);
    }

    #[test]
    fn restricted_ports_bounded_queues_take_and_unregister() {
        let t = ChannelTable::new();
        assert!(t.register_restricted(50, 700, 0));
        assert_eq!(t.send(9, 700, b"x"), Err(EPERM), "only pid 0 may send");
        assert_eq!(t.refused_sender.load(Ordering::Relaxed), 1);
        assert_eq!(t.send(0, 700, b"irq"), Ok(3));
        t.set_only_from(700, 9);
        assert_eq!(t.send(0, 700, b"x"), Err(EPERM));
        assert_eq!(t.send(9, 700, b"y"), Ok(1));
        assert_eq!(t.take(700), Some((0, b"irq".to_vec())));
        assert_eq!(t.take(700), Some((9, b"y".to_vec())));
        assert_eq!(t.take(700), None);
        // Bounded: MAX_QUEUE messages, then EAGAIN (counted, nothing lost silently).
        assert!(t.register(1, 701));
        for _ in 0..MAX_QUEUE {
            t.send_with(2, 701, b"m", false).unwrap();
        }
        assert_eq!(t.send(2, 701, b"m"), Err(EAGAIN));
        assert_eq!(t.dropped_full.load(Ordering::Relaxed), 1);
        // send_with(audit=false) left no audit line per message.
        assert!(t.audit_lines(64).iter().all(|l| !l.contains("-> port 701")));
        assert!(t.audit_lines(64).iter().any(|l| l.contains("claimed port 700 (restricted)")));
        assert!(t.audit_lines(64).iter().any(|l| l.contains("REFUSED")));
        t.unregister(701);
        assert_eq!(t.send(2, 701, b"m"), Err(ESRCH));
        assert!(t.register(3, 701), "an unregistered port can be claimed again");
    }

    #[test]
    fn audit_keeps_the_last_events_in_order() {
        let t = ChannelTable::new();
        t.register(1, 5);
        for i in 0..10u64 {
            t.send(2, 5, &vec![0; i as usize]).unwrap();
        }
        let l = t.audit_lines(6);
        assert_eq!(l.len(), 6);
        assert_eq!(l[0], "pid 2 -> port 5 (pid 1): 4 bytes");
        assert_eq!(l[5], "pid 2 -> port 5 (pid 1): 9 bytes");
        let first = t.audit_recent(11);
        assert_eq!(first[0], AuditEvent::Claimed { pid: 1, port: 5, restricted: false });
    }

    #[test]
    fn audit_ring_wraps_without_losing_recent_events() {
        let t = ChannelTable::new();
        t.register(1, 5);
        for i in 0..(3 * RING as u64) {
            t.send(i, 5, b"").unwrap();
        }
        let r = t.audit_recent(RING);
        assert_eq!(r.len(), RING);
        assert_eq!(r[RING - 1], AuditEvent::Sent { sender: 3 * RING as u64 - 1, port: 5, owner: 1, len: 0 });
    }

    /// Four producer/consumer pairs on four ports, in parallel: every message
    /// arrives exactly once, in order per sender, on the right port.
    #[test]
    fn parallel_pairs_on_separate_channels() {
        setup();
        let t = Arc::new(ChannelTable::new());
        for p in 0..4u32 {
            assert!(t.register(100 + p as u64, 1000 + p));
        }
        let n = 20_000u32;
        let mut hs = vec![];
        for p in 0..4u32 {
            let tp = t.clone();
            hs.push(std::thread::spawn(move || {
                for i in 0..n {
                    // Bounded queue: back off while the consumer catches up.
                    loop {
                        match tp.send(200 + p as u64, 1000 + p, &i.to_le_bytes()) {
                            Ok(_) => break,
                            Err(EAGAIN) => std::thread::yield_now(),
                            Err(e) => panic!("{e}"),
                        }
                    }
                }
            }));
            let tc = t.clone();
            hs.push(std::thread::spawn(move || {
                let mut next = 0u32;
                while next < n {
                    match tc.recv(100 + p as u64) {
                        Ok((sender, d)) => {
                            assert_eq!(sender, 200 + p as u64);
                            assert_eq!(u32::from_le_bytes([d[0], d[1], d[2], d[3]]), next);
                            next += 1;
                        }
                        Err(RecvError::Empty) => std::thread::yield_now(),
                        Err(e) => panic!("{e:?}"),
                    }
                }
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(t.total_sends(), 4 * n as u64);
        for p in 0..4 {
            assert_eq!(t.queued(1000 + p), Some(0));
        }
    }

    /// Many senders into ONE port: the per-channel lock serialises them, nothing
    /// is lost, and registrations racing in the background do not disturb it.
    #[test]
    fn many_senders_one_port_while_registering() {
        setup();
        let t = Arc::new(ChannelTable::new());
        assert!(t.register(1, 1));
        let stop = Arc::new(AtomicBool::new(false));
        let reg = {
            let t = t.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut p = 10_000u32;
                while !stop.load(Ordering::Relaxed) {
                    t.register(p as u64, p);
                    p += 1;
                }
            })
        };
        let senders: Vec<_> = (0..4u64)
            .map(|s| {
                let t = t.clone();
                std::thread::spawn(move || {
                    let mut sent = 0u64;
                    while sent < 5_000 {
                        match t.send(s, 1, b"m") {
                            Ok(_) => sent += 1,
                            Err(EAGAIN) => std::thread::yield_now(),
                            Err(e) => panic!("{e}"),
                        }
                    }
                })
            })
            .collect();
        // One consumer drains the port while the senders run.
        let consumer = {
            let t = t.clone();
            std::thread::spawn(move || {
                let mut got = 0u64;
                while got < 20_000 {
                    match t.recv(1) {
                        Ok((_, d)) => {
                            assert_eq!(d, b"m");
                            got += 1;
                        }
                        Err(_) => std::thread::yield_now(),
                    }
                }
                got
            })
        };
        for h in senders {
            h.join().unwrap();
        }
        assert_eq!(consumer.join().unwrap(), 20_000, "every accepted message arrives exactly once");
        stop.store(true, Ordering::Relaxed);
        reg.join().unwrap();
        assert_eq!(t.queued(1), Some(0));
        assert!(t.queued(1).unwrap() <= MAX_QUEUE);
        // Audit readers racing writers never see a torn record.
        for e in t.audit_recent(RING) {
            match e {
                AuditEvent::Sent { port, len, .. } => assert!(port == 1 && len == 1),
                AuditEvent::Claimed { pid, port, .. } => assert_eq!(pid, port as u64),
                AuditEvent::Refused { .. } => panic!("no restricted ports here"),
            }
        }
    }

    #[test]
    fn audit_reader_concurrent_with_writers_sees_only_whole_records() {
        setup();
        let t = Arc::new(ChannelTable::new());
        t.register(9, 9);
        let stop = Arc::new(AtomicBool::new(false));
        let writers: Vec<_> = (0..3u64)
            .map(|w| {
                let t = t.clone();
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut i = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        // len encodes the sender so a torn record would mismatch.
                        let len = (w * 1000 + i % 1000) as usize % 64;
                        t.send(w, 9, &vec![0; len]).unwrap();
                        let _ = t.take(9); // keep the bounded queue from filling
                        i += 1;
                    }
                })
            })
            .collect();
        for _ in 0..2_000 {
            for e in t.audit_recent(RING) {
                if let AuditEvent::Sent { sender, port, owner, .. } = e {
                    assert!(sender < 3 && port == 9 && owner == 9);
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        for w in writers {
            w.join().unwrap();
        }
    }
}
