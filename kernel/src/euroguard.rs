//! EuroGuard — system-wide access & network control (Track 7).
//!
//! First, ACTUALLY running slice of the spec: a policy engine (Level 1,
//! system-wide), per-app network statistics (Phase 7.4) and an audit ring
//! (Phase 7.8). Hooked into the socket `connect` syscall (Phase 7.1) so an
//! app can no longer connect outward arbitrarily without the kernel
//! evaluating and logging it. This is the "hard policy boundary" of Milestone A —
//! no cosmetics.
//!
//! In a mature system the policy comes from `/etc/euroguard/system.toml`
//! (Level 1), `~/.config/euroguard/user.toml` (Level 2) and per-app TOML
//! (Level 3), combined as **System > User > App**. Here there is a
//! built-in system startup set; the hierarchy + TOML storage is Phase 7.2.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use euronet::ipv4::Ipv4Addr;
use spin::Mutex;

use crate::interrupts::ticks;

/// The decision of the policy engine. (Phase 7.2 adds `Ask` once the
/// userspace daemon + dialog UI exist.)
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
}

/// Per-app network statistics — the view behind the Network monitor (Phase 7.4).
#[derive(Default, Clone)]
pub struct AppStats {
    pub connects: u64,
    pub blocked: u64,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub hosts: Vec<Ipv4Addr>, // unique contacted IPs
}

/// One line in the audit log (Phase 7.8). Local, never sent automatically.
#[derive(Clone)]
pub struct AuditEvent {
    pub ticks: u64,
    pub kind: &'static str, // CONNECT · BLOCK · INFO
    pub app: String,
    pub detail: String,
}

struct EuroGuard {
    /// Level 1 — system-wide blocks.
    blocked_ips: Vec<Ipv4Addr>,
    blocked_ports: Vec<u16>,
    /// DNS block list: ads/trackers/telemetry by name (including subdomains).
    blocked_domains: Vec<String>,
    /// Per-app aggregation (key = app identity, e.g. "/bin/msock").
    apps: BTreeMap<String, AppStats>,
    /// DNS query log (domain → count) for the "top queries" overview.
    dns_log: BTreeMap<String, u64>,
    /// Audit ring (bounded) — newest at the back.
    audit: Vec<AuditEvent>,
    /// Total number of blocked requests (for the dashboard overview).
    blocked_total: u64,
}

const AUDIT_MAX: usize = 64;
const HOSTS_MAX: usize = 16;

impl EuroGuard {
    const fn new() -> Self {
        EuroGuard {
            blocked_ips: Vec::new(),
            blocked_ports: Vec::new(),
            blocked_domains: Vec::new(),
            apps: BTreeMap::new(),
            dns_log: BTreeMap::new(),
            audit: Vec::new(),
            blocked_total: 0,
        }
    }

    fn log(&mut self, kind: &'static str, app: &str, detail: String) {
        if self.audit.len() >= AUDIT_MAX {
            self.audit.remove(0);
        }
        self.audit.push(AuditEvent { ticks: ticks(), kind, app: app.to_string(), detail });
    }
}

static GUARD: Mutex<EuroGuard> = Mutex::new(EuroGuard::new());

/// Load the system startup policy (Level 1). Later replaces reading in
/// `/etc/euroguard/system.toml`. We block a known tracker/telemetry IP
/// and a few outdated/insecure ports.
pub fn init() {
    let mut g = GUARD.lock();
    // 203.0.113.0/24 is TEST-NET-3 — here as a stand-in for a tracker endpoint.
    g.blocked_ips.push(Ipv4Addr([203, 0, 113, 5]));
    g.blocked_ports.push(23); // telnet (cleartext)
    g.blocked_ports.push(1900); // SSDP (leak-prone)
    // DNS block list (ads/trackers/telemetry) — incl. subdomains.
    for d in ["ads.doubleclick.net", "telemetry.mozilla.org", "google-analytics.com", "graph.facebook.com"] {
        g.blocked_domains.push(d.to_string());
    }
    g.log("INFO", "euroguard", "system policy loaded (Level 1)".to_string());
}

/// Load the Level-1 system policy from a config file (Phase 7.2). A
/// simple, readable rule format: `block-ip <ip>`, `block-port <n>`,
/// `block-domain <name>`; `#` is a comment. Replaces the current policy.
pub fn load_config(text: &str) {
    let mut g = GUARD.lock();
    g.blocked_ips.clear();
    g.blocked_ports.clear();
    g.blocked_domains.clear();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        match (it.next(), it.next()) {
            (Some("block-ip"), Some(v)) => {
                if let Some(ip) = crate::net::parse_ipv4(v) {
                    g.blocked_ips.push(ip);
                }
            }
            (Some("block-port"), Some(v)) => {
                if let Ok(p) = v.parse::<u16>() {
                    g.blocked_ports.push(p);
                }
            }
            (Some("block-domain"), Some(v)) => g.blocked_domains.push(v.to_ascii_lowercase()),
            _ => {}
        }
    }
    let (ni, np, nd) = (g.blocked_ips.len(), g.blocked_ports.len(), g.blocked_domains.len());
    g.log(
        "INFO",
        "euroguard",
        alloc::format!("policy from /etc/euroguard/system.conf: {ni} IPs, {np} ports, {nd} domains"),
    );
}

/// Add a blocked domain on the system (the "Add domain" action
/// from the spec — custom blocks). Effective immediately.
pub fn add_blocked_domain(domain: &str) {
    let mut g = GUARD.lock();
    let d = domain.to_ascii_lowercase();
    if !g.blocked_domains.contains(&d) {
        g.blocked_domains.push(d.clone());
    }
    g.log("INFO", "shell", alloc::format!("domain blocked: {d}"));
}

/// Remove a domain from the block list (whitelist action). Returns whether
/// anything was removed.
pub fn remove_blocked_domain(domain: &str) -> bool {
    let mut g = GUARD.lock();
    let d = domain.to_ascii_lowercase();
    let before = g.blocked_domains.len();
    g.blocked_domains.retain(|x| x != &d);
    let removed = g.blocked_domains.len() < before;
    if removed {
        g.log("INFO", "shell", alloc::format!("domain unblocked: {d}"));
    }
    removed
}

/// Add an app to the table if it is not there yet.
fn entry<'a>(g: &'a mut EuroGuard, app: &str) -> &'a mut AppStats {
    g.apps.entry(app.to_string()).or_default()
}

/// Policy check for an outgoing connection (Phase 7.1). Decides Allow/Block,
/// updates the statistics and writes an audit event. Called by the
/// `connect` syscall BEFORE a packet goes out the door.
pub fn check_connect(app: &str, ip: Ipv4Addr, port: u16) -> Decision {
    let mut g = GUARD.lock();
    let blocked = g.blocked_ips.contains(&ip) || g.blocked_ports.contains(&port);
    if blocked {
        g.blocked_total += 1;
        {
            let s = entry(&mut g, app);
            s.blocked += 1;
        }
        g.log("BLOCK", app, alloc::format!("connect {}:{} — system rule", ipfmt(ip), port));
        Decision::Block
    } else {
        {
            let s = entry(&mut g, app);
            s.connects += 1;
            if !s.hosts.contains(&ip) && s.hosts.len() < HOSTS_MAX {
                s.hosts.push(ip);
            }
        }
        g.log("CONNECT", app, alloc::format!("{}:{}", ipfmt(ip), port));
        Decision::Allow
    }
}

/// Count sent/received bytes per app (Phase 7.4).
pub fn record_bytes(app: &str, sent: u64, recv: u64) {
    let mut g = GUARD.lock();
    let s = entry(&mut g, app);
    s.bytes_sent += sent;
    s.bytes_recv += recv;
}

/// DNS-level filtering (Phase 7.5/7.4): evaluate + log a DNS query before
/// it goes out onto the network. A blocked domain (or subdomain thereof) is
/// refused — the app gets no IP and therefore cannot connect. This is the
/// privacy core of EuroGuard: trackers/ads die before any traffic arises.
pub fn check_dns(app: &str, domain: &str) -> Decision {
    let mut g = GUARD.lock();
    let d = domain.to_ascii_lowercase();
    let blocked = g
        .blocked_domains
        .iter()
        .any(|b| d == *b || d.ends_with(&alloc::format!(".{b}")));
    if blocked {
        g.blocked_total += 1;
        entry(&mut g, app).blocked += 1;
        g.log("DNS-BLOK", app, alloc::format!("dns {domain} — block list"));
        Decision::Block
    } else {
        *g.dns_log.entry(d).or_insert(0) += 1;
        g.log("DNS", app, alloc::format!("query {domain}"));
        Decision::Allow
    }
}

fn ipfmt(ip: Ipv4Addr) -> String {
    alloc::format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3])
}

/// The DNS query log (top looked-up domains) — Phase 7.4 "log_dns_queries".
pub fn dns_lines() -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    if g.dns_log.is_empty() {
        return out;
    }
    out.push("dns-queries (top):".to_string());
    let mut v: Vec<(&String, &u64)> = g.dns_log.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    for (dom, n) in v.into_iter().take(5) {
        out.push(alloc::format!("  {:<28} {}x", dom, n));
    }
    out
}

/// Human-readable summary of the active policy (Level 1).
pub fn policy_lines() -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    out.push("policy (Level 1 — /etc/euroguard/system.conf):".to_string());
    let ips: Vec<String> = g.blocked_ips.iter().map(|i| ipfmt(*i)).collect();
    out.push(alloc::format!("  blocked IPs:   {}", ips.join(", ")));
    let ports: Vec<String> = g.blocked_ports.iter().map(|p| p.to_string()).collect();
    out.push(alloc::format!("  blocked ports: {}", ports.join(", ")));
    out.push(alloc::format!("  dns block list:    {} domains", g.blocked_domains.len()));
    out
}

/// The Network monitor: per-app data usage + connections (Phase 7.4).
pub fn stats_lines() -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    out.push(alloc::format!("network monitor — {} blocked today", g.blocked_total));
    if g.apps.is_empty() {
        out.push("  (no app activity yet)".to_string());
    }
    for (app, s) in g.apps.iter() {
        out.push(alloc::format!(
            "  {:<14} {} conn.  tx {} / rx {} bytes  {} blocked",
            app, s.connects, s.bytes_sent, s.bytes_recv, s.blocked
        ));
    }
    out
}

/// The audit log (Phase 7.8), newest first, max `limit` lines.
pub fn audit_lines(limit: usize) -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    out.push("audit log (local, never sent automatically):".to_string());
    for e in g.audit.iter().rev().take(limit) {
        let mark = match e.kind {
            "BLOCK" | "DNS-BLOK" => "x",
            "CONNECT" | "DNS" => "+",
            _ => "-",
        };
        out.push(alloc::format!("  {} t+{:>5} {:<8} {:<14} {}", mark, e.ticks, e.kind, e.app, e.detail));
    }
    out
}
