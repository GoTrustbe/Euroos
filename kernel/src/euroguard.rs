//! EuroGuard — systeembrede toegangs- & netwerkcontrole (Track 7).
//!
//! Eerste, ECHT draaiende snede van de spec: een policy-engine (Niveau 1,
//! systeembreed), per-app netwerkstatistieken (Fase 7.4) en een audit-ring
//! (Fase 7.8). Ingehaakt op de socket-`connect`-syscall (Fase 7.1) zodat een
//! app niet meer willekeurig naar buiten kan verbinden zonder dat de kernel
//! het beoordeelt én logt. Dit is de "harde policy-grens" van Mijlpaal A —
//! geen cosmetica.
//!
//! In een volwassen systeem komt de policy uit `/etc/euroguard/system.toml`
//! (Niveau 1), `~/.config/euroguard/user.toml` (Niveau 2) en per-app TOML
//! (Niveau 3), gecombineerd als **Systeem > Gebruiker > App**. Hier zit een
//! ingebouwde systeem-startset; de hiërarchie + TOML-opslag is Fase 7.2.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use euronet::ipv4::Ipv4Addr;
use spin::Mutex;

use crate::interrupts::ticks;

/// De beslissing van de policy-engine. (Fase 7.2 voegt `Ask` toe zodra de
/// userspace-daemon + dialoog-UI er zijn.)
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
}

/// Per-app netwerkstatistieken — het zicht onder de Netwerkmonitor (Fase 7.4).
#[derive(Default, Clone)]
pub struct AppStats {
    pub connects: u64,
    pub blocked: u64,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub hosts: Vec<Ipv4Addr>, // unieke gecontacteerde IP's
}

/// Eén regel in het auditlogboek (Fase 7.8). Lokaal, nooit automatisch verstuurd.
#[derive(Clone)]
pub struct AuditEvent {
    pub ticks: u64,
    pub kind: &'static str, // CONNECT · BLOCK · INFO
    pub app: String,
    pub detail: String,
}

struct EuroGuard {
    /// Niveau 1 — systeembrede blokkeringen.
    blocked_ips: Vec<Ipv4Addr>,
    blocked_ports: Vec<u16>,
    /// DNS-blokkeerlijst: ads/trackers/telemetrie op naam (ook subdomeinen).
    blocked_domains: Vec<String>,
    /// Per-app aggregatie (sleutel = app-identiteit, bv. "/bin/msock").
    apps: BTreeMap<String, AppStats>,
    /// DNS-querylog (domein → aantal) voor het "top queries"-overzicht.
    dns_log: BTreeMap<String, u64>,
    /// Audit-ring (begrensd) — nieuwste achteraan.
    audit: Vec<AuditEvent>,
    /// Totaal aantal geblokkeerde verzoeken (voor het dashboard-overzicht).
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

/// Laad de systeem-startpolicy (Niveau 1). Vervangt later het inlezen van
/// `/etc/euroguard/system.toml`. We blokkeren een bekend tracker/telemetrie-IP
/// en een paar verouderde/onveilige poorten.
pub fn init() {
    let mut g = GUARD.lock();
    // 203.0.113.0/24 is TEST-NET-3 — hier als stand-in voor een tracker-endpoint.
    g.blocked_ips.push(Ipv4Addr([203, 0, 113, 5]));
    g.blocked_ports.push(23); // telnet (klaartekst)
    g.blocked_ports.push(1900); // SSDP (lek-gevoelig)
    // DNS-blokkeerlijst (ads/trackers/telemetrie) — incl. subdomeinen.
    for d in ["ads.doubleclick.net", "telemetry.mozilla.org", "google-analytics.com", "graph.facebook.com"] {
        g.blocked_domains.push(d.to_string());
    }
    g.log("INFO", "euroguard", "systeem-policy geladen (Niveau 1)".to_string());
}

/// Laad de Niveau-1 systeem-policy uit een configbestand (Fase 7.2). Een
/// eenvoudig, leesbaar regelformaat: `block-ip <ip>`, `block-port <n>`,
/// `block-domain <naam>`; `#` is commentaar. Vervangt de huidige policy.
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
        alloc::format!("policy uit /etc/euroguard/system.conf: {ni} IP's, {np} poorten, {nd} domeinen"),
    );
}

/// Voeg op het systeem een geblokkeerd domein toe (de "Domein toevoegen"-actie
/// uit de spec — aangepaste blokkeringen). Direct van kracht.
pub fn add_blocked_domain(domain: &str) {
    let mut g = GUARD.lock();
    let d = domain.to_ascii_lowercase();
    if !g.blocked_domains.contains(&d) {
        g.blocked_domains.push(d.clone());
    }
    g.log("INFO", "shell", alloc::format!("domein geblokkeerd: {d}"));
}

/// Haal een domein van de blokkeerlijst (whitelist-actie). Geeft terug of er
/// iets verwijderd is.
pub fn remove_blocked_domain(domain: &str) -> bool {
    let mut g = GUARD.lock();
    let d = domain.to_ascii_lowercase();
    let before = g.blocked_domains.len();
    g.blocked_domains.retain(|x| x != &d);
    let removed = g.blocked_domains.len() < before;
    if removed {
        g.log("INFO", "shell", alloc::format!("domein gedeblokkeerd: {d}"));
    }
    removed
}

/// Voeg een app toe aan de tabel als die er nog niet is.
fn entry<'a>(g: &'a mut EuroGuard, app: &str) -> &'a mut AppStats {
    g.apps.entry(app.to_string()).or_default()
}

/// Policy-check voor een uitgaande verbinding (Fase 7.1). Beslist Allow/Block,
/// werkt de statistieken bij en schrijft een audit-event. Wordt aangeroepen door
/// de `connect`-syscall VÓÓR er een pakket de deur uitgaat.
pub fn check_connect(app: &str, ip: Ipv4Addr, port: u16) -> Decision {
    let mut g = GUARD.lock();
    let blocked = g.blocked_ips.contains(&ip) || g.blocked_ports.contains(&port);
    if blocked {
        g.blocked_total += 1;
        {
            let s = entry(&mut g, app);
            s.blocked += 1;
        }
        g.log("BLOCK", app, alloc::format!("connect {}:{} — systeemregel", ipfmt(ip), port));
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

/// Tel verzonden/ontvangen bytes per app (Fase 7.4).
pub fn record_bytes(app: &str, sent: u64, recv: u64) {
    let mut g = GUARD.lock();
    let s = entry(&mut g, app);
    s.bytes_sent += sent;
    s.bytes_recv += recv;
}

/// DNS-niveau-filtering (Fase 7.5/7.4): beoordeel + log een DNS-query voordat
/// die het netwerk op gaat. Een geblokkeerd domein (of subdomein daarvan) wordt
/// geweigerd — de app krijgt geen IP en kan dus niet verbinden. Dit is de
/// privacy-kern van EuroGuard: trackers/ads sneuvelen vóór er verkeer ontstaat.
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
        g.log("DNS-BLOK", app, alloc::format!("dns {domain} — blokkeerlijst"));
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

/// De DNS-querylog (top opgezochte domeinen) — Fase 7.4 "log_dns_queries".
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

/// Menselijke samenvatting van de actieve policy (Niveau 1).
pub fn policy_lines() -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    out.push("policy (Niveau 1 — /etc/euroguard/system.conf):".to_string());
    let ips: Vec<String> = g.blocked_ips.iter().map(|i| ipfmt(*i)).collect();
    out.push(alloc::format!("  geblokkeerde IP's:   {}", ips.join(", ")));
    let ports: Vec<String> = g.blocked_ports.iter().map(|p| p.to_string()).collect();
    out.push(alloc::format!("  geblokkeerde poorten: {}", ports.join(", ")));
    out.push(alloc::format!("  dns-blokkeerlijst:    {} domeinen", g.blocked_domains.len()));
    out
}

/// De Netwerkmonitor: per-app dataverbruik + verbindingen (Fase 7.4).
pub fn stats_lines() -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    out.push(alloc::format!("netwerkmonitor — {} geblokkeerd vandaag", g.blocked_total));
    if g.apps.is_empty() {
        out.push("  (nog geen app-activiteit)".to_string());
    }
    for (app, s) in g.apps.iter() {
        out.push(alloc::format!(
            "  {:<14} {} verb.  tx {} / rx {} bytes  {} geblok.",
            app, s.connects, s.bytes_sent, s.bytes_recv, s.blocked
        ));
    }
    out
}

/// Het auditlogboek (Fase 7.8), nieuwste eerst, max `limit` regels.
pub fn audit_lines(limit: usize) -> Vec<String> {
    let g = GUARD.lock();
    let mut out = Vec::new();
    out.push("auditlog (lokaal, nooit automatisch verzonden):".to_string());
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
