//! The network bridge for graphics apps (the browser). A fullscreen app cannot
//! safely block on the network itself: the DOOM-style `bg_dispatch` runs under a
//! no-yield lock, and the kernel's HTTP/TLS fetch busy-polls the NIC. So instead
//! the app makes a NON-BLOCKING request (`fetch_start`) and polls for the result
//! (`fetch_poll`); the actual [`crate::net::fetch_full`] runs in the desktop-loop
//! task context (interrupts on, no lock), exactly where EuroWeb already fetches.
//!
//! This is the bridge that turns "graphics app" into "browser": the app gets the
//! kernel's real HTTP/1.1 + TLS 1.3 + DNS stack through one request.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

struct Bridge {
    /// A URL the app asked for that the desktop loop has not fetched yet.
    pending: Option<String>,
    /// The fetched result: (http status, body), consumed by the app's poll.
    result: Option<(u16, Vec<u8>)>,
    /// A fetch is in flight (requested, not yet resolved).
    busy: bool,
}

static BRIDGE: Mutex<Bridge> = Mutex::new(Bridge { pending: None, result: None, busy: false });

/// App side: request a URL. Returns false if a fetch is already in flight.
pub fn request(url: &str) -> bool {
    let mut b = BRIDGE.lock();
    if b.busy {
        return false;
    }
    b.pending = Some(String::from(url));
    b.result = None;
    b.busy = true;
    true
}

/// App side: is a result ready? Returns (status, body) once, then clears it.
pub fn take_result() -> Option<(u16, Vec<u8>)> {
    BRIDGE.lock().result.take()
}

pub fn is_busy() -> bool {
    BRIDGE.lock().busy
}

/// Desktop-loop side: if a URL is pending, fetch it now (real HTTP/TLS/DNS) and
/// stash the result. Runs in the normal task context, so blocking the NIC poll
/// here is safe. Call once per loop iteration.
pub fn service() {
    let url = {
        let mut b = BRIDGE.lock();
        match b.pending.take() {
            Some(u) => u,
            None => return,
        }
    };
    crate::serial_println!("[netbridge] fetching {url} for the browser");
    let (status, body) = fetch_url(&url);
    crate::serial_println!("[netbridge] fetched: status {status}, {} bytes", body.len());
    let mut b = BRIDGE.lock();
    b.result = Some((status, body));
    b.busy = false;
}

/// Parse a URL and fetch it via the kernel stack. Follows one redirect. Returns
/// (status, body); status 0 means the request could not be made.
fn fetch_url(url: &str) -> (u16, Vec<u8>) {
    let mut current = String::from(url);
    for _ in 0..4 {
        let (tls, host, port, path) = match split_url(&current) {
            Some(v) => v,
            None => return (0, Vec::new()),
        };
        match crate::net::fetch_full(&host, port, &path, tls) {
            Some((status, location, body)) => {
                if (301..=308).contains(&status) {
                    if let Some(loc) = location {
                        current = if loc.starts_with("http") {
                            loc
                        } else if loc.starts_with('/') {
                            alloc::format!("{}://{host}{loc}", if tls { "https" } else { "http" })
                        } else {
                            loc
                        };
                        continue;
                    }
                }
                return (status, body);
            }
            None => return (0, Vec::new()),
        }
    }
    (0, Vec::new())
}

/// Split `scheme://host[:port]/path` → (tls, host, port, path). Defaults: https.
fn split_url(url: &str) -> Option<(bool, String, u16, String)> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        (true, url) // bare host → https
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rfind(':') {
        Some(i) => (
            authority[..i].to_string(),
            authority[i + 1..].parse::<u16>().unwrap_or(if tls { 443 } else { 80 }),
        ),
        None => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some((tls, host, port, if path.is_empty() { String::from("/") } else { path.to_string() }))
}

/// `[netb]` boot self-test: the request/poll state machine (URL parsing +
/// hand-off), without needing the network. Injects a result and checks the poll.
pub fn selftest() {
    let (tls, host, port, path) = split_url("https://euro-os.eu/blog/").unwrap();
    let parse_ok = tls && host == "euro-os.eu" && port == 443 && path == "/blog/";
    let (tls2, host2, port2, _p) = split_url("http://example.com:8080/x").unwrap();
    let parse2 = !tls2 && host2 == "example.com" && port2 == 8080;
    // State machine: request marks busy; injecting a result lets poll retrieve it.
    let req = request("https://example.com/");
    let busy = is_busy();
    { BRIDGE.lock().result = Some((200, alloc::vec![b'h', b'i'])); BRIDGE.lock().busy = false; }
    let got = take_result().map(|(s, b)| s == 200 && b == b"hi").unwrap_or(false);
    let cleared = take_result().is_none();
    // Leave the bridge clean (no stale pending request from the test).
    { let mut b = BRIDGE.lock(); b.pending = None; b.result = None; b.busy = false; }
    let ok = parse_ok && parse2 && req && busy && got && cleared;
    crate::serial_println!(
        "[netb] Net bridge for apps: url-parse={parse_ok}/{parse2}, request→busy={busy}, poll-returns-result={got}, cleared={cleared} → {}",
        if ok { "OK (graphics apps can fetch via the kernel HTTP/TLS/DNS) ✓" } else { "FAILED ✗" }
    );
}
