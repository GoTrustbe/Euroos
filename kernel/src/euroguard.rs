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
    /// Per-country connection counts (the "which apps go to which countries"
    /// view). Key = ISO alpha-2, e.g. "CN"; "??" = not in the geo table.
    pub countries: BTreeMap<String, u64>,
}

/// A per-app network rule (the user-controllable "what can this app reach"
/// policy). This is the Level-3 (per-app) layer the module always intended.
#[derive(Clone, Default)]
pub enum AppNet {
    /// No per-app restriction (system + geo rules still apply).
    #[default]
    Default,
    /// The app may not open any outbound connection at all.
    Blocked,
    /// The app may connect ONLY to these CIDR blocks (allow-list); everything
    /// else is refused. Empty list = block everything, same as `Blocked`.
    AllowOnly(Vec<(u32, u8)>),
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
    /// Geo-blocked countries (ISO alpha-2, e.g. "CN"). No connection is allowed
    /// to an IP the geo table attributes to one of these.
    blocked_countries: Vec<String>,
    /// The IP-to-country table (curated built-in + a loaded feed).
    geo: eurogeo::Table,
    /// Per-app network policy (Level 3) — the user-controllable rules.
    app_rules: BTreeMap<String, AppNet>,
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
            blocked_countries: Vec::new(),
            geo: eurogeo::Table::new(),
            app_rules: BTreeMap::new(),
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
    g.blocked_countries.clear();
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
            (Some("block-country"), Some(v)) => {
                let cc = v.to_ascii_uppercase();
                if !g.blocked_countries.contains(&cc) {
                    g.blocked_countries.push(cc);
                }
            }
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

/// The host-byte-order u32 of an IPv4 (for the geo table + CIDR checks).
fn ip_u32(ip: Ipv4Addr) -> u32 {
    u32::from_be_bytes(ip.0)
}

fn cidr_has(net: u32, prefix: u8, ip: u32) -> bool {
    if prefix == 0 {
        return true;
    }
    let mask = if prefix >= 32 { u32::MAX } else { !((1u32 << (32 - prefix)) - 1) };
    (ip & mask) == (net & mask)
}

/// Policy check for an outgoing connection (Phase 7.1). Decides Allow/Block,
/// updates the per-app statistics (incl. destination country) and writes an
/// audit event. Called by the `connect` syscall BEFORE a packet goes out the
/// door. Order of evaluation: per-app rule → system IP/port block → geo block.
pub fn check_connect(app: &str, ip: Ipv4Addr, port: u16) -> Decision {
    let mut g = GUARD.lock();
    let u = ip_u32(ip);
    let country = g.geo.country_of(u).map(|c| c.to_string());
    let cc = country.clone().unwrap_or_else(|| "??".to_string());

    // 1. Per-app rule (the user's per-app network policy).
    let app_block = match g.app_rules.get(app) {
        Some(AppNet::Blocked) => Some("app blocked from the network"),
        Some(AppNet::AllowOnly(list)) => {
            if list.iter().any(|(n, p)| cidr_has(*n, *p, u)) {
                None
            } else {
                Some("outside the app's allow-list")
            }
        }
        _ => None,
    };
    // 2. System-wide IP / port block.
    let sys_block = g.blocked_ips.contains(&ip) || g.blocked_ports.contains(&port);
    // 3. Geo (country) block.
    let geo_block = country.as_deref().map(|c| g.blocked_countries.iter().any(|b| b == c)).unwrap_or(false);

    let reason = app_block
        .map(|r| r.to_string())
        .or_else(|| sys_block.then(|| "system rule".to_string()))
        .or_else(|| geo_block.then(|| alloc::format!("geo-block {cc}")));

    if let Some(reason) = reason {
        g.blocked_total += 1;
        {
            let s = entry(&mut g, app);
            s.blocked += 1;
            *s.countries.entry(cc.clone()).or_insert(0) += 1;
        }
        g.log("BLOCK", app, alloc::format!("connect {}:{} [{cc}] — {reason}", ipfmt(ip), port));
        Decision::Block
    } else {
        {
            let s = entry(&mut g, app);
            s.connects += 1;
            *s.countries.entry(cc.clone()).or_insert(0) += 1;
            if !s.hosts.contains(&ip) && s.hosts.len() < HOSTS_MAX {
                s.hosts.push(ip);
            }
        }
        g.log("CONNECT", app, alloc::format!("{}:{} [{cc}]", ipfmt(ip), port));
        Decision::Allow
    }
}

// ── User-controllable network policy (the per-app + geo control surface) ──────

/// Block (or unblock) all connections to a country by ISO alpha-2 code.
pub fn set_country_blocked(cc: &str, blocked: bool) {
    let cc = cc.to_ascii_uppercase();
    let mut g = GUARD.lock();
    g.blocked_countries.retain(|c| c != &cc);
    if blocked {
        g.blocked_countries.push(cc.clone());
        g.log("INFO", "shell", alloc::format!("country blocked: {cc}"));
    } else {
        g.log("INFO", "shell", alloc::format!("country unblocked: {cc}"));
    }
}

/// Set the per-app network policy (the user's "what can this app reach").
pub fn set_app_net(app: &str, rule: AppNet) {
    let mut g = GUARD.lock();
    let label = match &rule {
        AppNet::Default => "default (system + geo rules only)",
        AppNet::Blocked => "network BLOCKED",
        AppNet::AllowOnly(_) => "restricted to an allow-list",
    };
    if matches!(rule, AppNet::Default) {
        g.app_rules.remove(app);
    } else {
        g.app_rules.insert(app.to_string(), rule);
    }
    g.log("INFO", "shell", alloc::format!("app {app}: {label}"));
}

/// Load additional IP-to-country blocks from a geo feed (e.g.
/// `/etc/euroguard/geoip.conf`). Extends the built-in curated table.
pub fn load_geo_feed(text: &str) -> usize {
    let mut g = GUARD.lock();
    let n = g.geo.load_feed(text);
    g.log("INFO", "euroguard", alloc::format!("geo feed loaded: +{n} blocks"));
    n
}

/// The country the geo table attributes `ip` to (for callers / the report).
pub fn country_of(ip: Ipv4Addr) -> Option<String> {
    GUARD.lock().geo.country_of(ip_u32(ip)).map(|c| c.to_string())
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

/// Human-readable summary of the active policy (Level 1 + geo + per-app).
pub fn policy_lines() -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    out.push("policy (Level 1 — /etc/euroguard/system.conf):".to_string());
    let ips: Vec<String> = g.blocked_ips.iter().map(|i| ipfmt(*i)).collect();
    out.push(alloc::format!("  blocked IPs:   {}", ips.join(", ")));
    let ports: Vec<String> = g.blocked_ports.iter().map(|p| p.to_string()).collect();
    out.push(alloc::format!("  blocked ports: {}", ports.join(", ")));
    out.push(alloc::format!("  dns block list:    {} domains", g.blocked_domains.len()));
    out.push(alloc::format!(
        "  geo-block:     {} ({} country blocks in the table)",
        if g.blocked_countries.is_empty() { "none".to_string() } else { g.blocked_countries.join(", ") },
        g.geo.len()
    ));
    if !g.app_rules.is_empty() {
        out.push("  per-app network rules:".to_string());
        for (app, rule) in g.app_rules.iter() {
            let d = match rule {
                AppNet::Blocked => "network blocked".to_string(),
                AppNet::AllowOnly(l) => alloc::format!("allow-only ({} block(s))", l.len()),
                AppNet::Default => "default".to_string(),
            };
            out.push(alloc::format!("    {:<16} {}", app, d));
        }
    }
    out
}

/// Whether an app is currently cut off from the network entirely.
pub fn app_is_blocked(app: &str) -> bool {
    matches!(GUARD.lock().app_rules.get(app), Some(AppNet::Blocked))
}

/// A one-word label for an app's network policy (for the `apps` roster column).
pub fn app_net_label(app: &str) -> String {
    let g = GUARD.lock();
    match g.app_rules.get(app) {
        Some(AppNet::Blocked) => "BLOCKED".to_string(),
        Some(AppNet::AllowOnly(l)) => alloc::format!("allow-only({})", l.len()),
        _ => "default".to_string(),
    }
}

/// The network summary for ONE app (its rule + traffic + countries + hosts),
/// for the unified per-app control screen. Empty vec if the app has no activity
/// and no rule.
pub fn app_summary_lines(app: &str) -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    let rule = match g.app_rules.get(app) {
        Some(AppNet::Blocked) => "BLOCKED (no network)".to_string(),
        Some(AppNet::AllowOnly(l)) => alloc::format!("allow-only ({} network(s))", l.len()),
        _ => "default (system + geo rules only)".to_string(),
    };
    out.push(alloc::format!("  network policy: {rule}"));
    if let Some(s) = g.apps.get(app) {
        out.push(alloc::format!(
            "  traffic: {} conn · {} blocked · tx {} / rx {} B",
            s.connects, s.blocked, s.bytes_sent, s.bytes_recv
        ));
        let mut cs: Vec<(&String, &u64)> = s.countries.iter().collect();
        cs.sort_by(|a, b| b.1.cmp(a.1));
        if !cs.is_empty() {
            let parts: Vec<String> = cs.iter().take(8).map(|(c, n)| alloc::format!("{c}:{n}")).collect();
            out.push(alloc::format!("  countries: {}", parts.join("  ")));
        }
        if !s.hosts.is_empty() {
            let ips: Vec<String> = s
                .hosts
                .iter()
                .take(8)
                .map(|ip| {
                    let cc = g.geo.country_of(ip_u32(*ip)).unwrap_or("??");
                    alloc::format!("{}[{cc}]", ipfmt(*ip))
                })
                .collect();
            out.push(alloc::format!("  hosts: {}", ips.join("  ")));
        }
    } else {
        out.push("  traffic: (no connections yet)".to_string());
    }
    out
}

/// The per-app network report: for each app, its connection/blocked counts,
/// bytes, and the destination countries it talked to ("which apps go to which
/// countries"). This is the reporting surface behind the network monitor.
pub fn app_report_lines() -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    out.push(alloc::format!("per-app network report — {} blocked total", g.blocked_total));
    if g.apps.is_empty() {
        out.push("  (no app network activity yet)".to_string());
        return out;
    }
    for (app, s) in g.apps.iter() {
        let rule = match g.app_rules.get(app) {
            Some(AppNet::Blocked) => " [BLOCKED]",
            Some(AppNet::AllowOnly(_)) => " [allow-only]",
            _ => "",
        };
        out.push(alloc::format!(
            "  {:<16}{} {} conn · {} blocked · tx {} / rx {} B",
            app, rule, s.connects, s.blocked, s.bytes_sent, s.bytes_recv
        ));
        // Destination countries (sorted by count).
        let mut cs: Vec<(&String, &u64)> = s.countries.iter().collect();
        cs.sort_by(|a, b| b.1.cmp(a.1));
        if !cs.is_empty() {
            let parts: Vec<String> = cs.iter().take(6).map(|(c, n)| alloc::format!("{c}:{n}")).collect();
            out.push(alloc::format!("      countries: {}", parts.join("  ")));
        }
        // A few contacted IPs (with country).
        if !s.hosts.is_empty() {
            let ips: Vec<String> = s
                .hosts
                .iter()
                .take(6)
                .map(|ip| {
                    let cc = g.geo.country_of(ip_u32(*ip)).unwrap_or("??");
                    alloc::format!("{}[{cc}]", ipfmt(*ip))
                })
                .collect();
            out.push(alloc::format!("      hosts: {}", ips.join("  ")));
        }
    }
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

/// Boot self-test for the network-control core (geo-block + per-app policy +
/// report). Proves, in one boot, that: a country block refuses a connection to
/// a China-attributed IP while a local IP is allowed; a per-app block cuts an
/// app off entirely; an allow-list restricts an app to its CIDR; and the report
/// attributes flows to countries. Uses distinct test app names so it never
/// perturbs a real app's policy.
pub fn selftest() {
    use euronet::ipv4::Ipv4Addr;
    let cn = Ipv4Addr([202, 96, 0, 5]); // inside China Telecom 202.96.0.0/11
    let local = Ipv4Addr([10, 0, 2, 2]); // the test gateway, not geo-mapped

    // Country attribution from the built-in geo table.
    let cn_detected = country_of(cn).as_deref() == Some("CN");
    let local_unmapped = country_of(local).is_none();

    // Geo-block: block CN, then a connection to a CN IP must be refused, a local
    // one allowed.
    set_country_blocked("CN", true);
    let cn_blocked = check_connect("euroguard-test", cn, 443) == Decision::Block;
    let local_allowed = check_connect("euroguard-test", local, 443) == Decision::Allow;
    set_country_blocked("CN", false); // leave the system as we found it

    // Per-app block: cut a named app off the network entirely.
    set_app_net("euroguard-evil", AppNet::Blocked);
    let app_blocked = check_connect("euroguard-evil", local, 443) == Decision::Block;
    set_app_net("euroguard-evil", AppNet::Default);

    // Allow-list: an app restricted to 10.0.0.0/8 reaches the gateway but not
    // an outside address (the AI-agent-clamp shape).
    set_app_net("euroguard-agent", AppNet::AllowOnly(alloc::vec![(u32::from_be_bytes([10, 0, 0, 0]), 8)]));
    let allow_in = check_connect("euroguard-agent", local, 443) == Decision::Allow;
    let allow_out = check_connect("euroguard-agent", Ipv4Addr([1, 2, 3, 4]), 443) == Decision::Block;
    set_app_net("euroguard-agent", AppNet::Default);

    let ok = cn_detected && local_unmapped && cn_blocked && local_allowed && app_blocked && allow_in && allow_out;
    crate::serial_println!(
        "[7guard] EuroGuard network control: geo-attribution(CN={cn_detected}, local-unmapped={local_unmapped}), \
         geo-block(CN-refused={cn_blocked}, local-allowed={local_allowed}), per-app-block={app_blocked}, \
         allow-list(in={allow_in}, out-refused={allow_out}) → {}",
        if ok {
            "OK (per-app + geo network control: block a country, cut an app off, or clamp an agent to an allow-list) ✓"
        } else {
            "FAILED ✗"
        }
    );
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
