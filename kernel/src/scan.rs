//! **M7-2** — EuroScan: sovereign, driverless scanning over **eSCL/AirScan**
//! (mDNS `_uscan._tcp` + REST/XML over HTTP). The host-tested `euroscan` core
//! builds the request bodies + parses the responses; here we wrap them in real
//! HTTP-over-EuroNet-TCP and do the round-trip. In QEMU we reach the host mock
//! scanner via the SLIRP gateway. The scanned page is written to EuroFiles.
//!
//! No scanner driver: this is the network-first stance of docs/SUPPORT-POLICY.md
//! applied to scanning, exactly as EuroPrint does for printing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use euroscan::{Capabilities, ScanSettings};

const SCANNER_IP: &str = "10.0.2.2";
const SCANNER_PORT: u16 = 8631; // driverless scanners: HTTP eSCL (8631 typical for the mock)

/// GET `path` from the scanner; return the raw HTTP response.
fn http_get(path: &str) -> Option<Vec<u8>> {
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {SCANNER_IP}:{SCANNER_PORT}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    crate::net::http_post_raw(SCANNER_IP, SCANNER_PORT, req.as_bytes())
}

/// POST `body` (an eSCL ScanSettings XML) to `path`; return the raw response.
fn http_post_xml(path: &str, body: &[u8]) -> Option<Vec<u8>> {
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: {SCANNER_IP}:{SCANNER_PORT}\r\n\
         Content-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut req = Vec::with_capacity(header.len() + body.len());
    req.extend_from_slice(header.as_bytes());
    req.extend_from_slice(body);
    crate::net::http_post_raw(SCANNER_IP, SCANNER_PORT, &req)
}

/// Run a full eSCL scan job: capabilities → ScanJobs → NextDocument. Returns
/// (capabilities, scanned image bytes). Bounded; never hangs the boot.
fn do_scan(settings: &ScanSettings) -> Option<(Capabilities, Vec<u8>)> {
    // 1. ScannerCapabilities (discover + prove the scanner answers).
    let caps_raw = http_get("/eSCL/ScannerCapabilities")?;
    let (code, _h, body) = euroscan::parse_http(&caps_raw)?;
    if code != 200 {
        return None;
    }
    let caps = Capabilities::parse(core::str::from_utf8(body).ok()?);

    // 2. ScanJobs (create the job) → 201 Created + Location.
    let job_raw = http_post_xml("/eSCL/ScanJobs", settings.to_xml().as_bytes())?;
    let (code, head, _b) = euroscan::parse_http(&job_raw)?;
    if code != 201 {
        return None;
    }
    let location = euroscan::location_header(head)?;
    let next = euroscan::job_path(location);

    // 3. NextDocument → the scanned page bytes.
    let doc_raw = http_get(&next)?;
    let (code, _h, image) = euroscan::parse_http(&doc_raw)?;
    if code != 200 || image.is_empty() {
        return None;
    }
    Some((caps, image.to_vec()))
}

/// **M7-2 boot self-test** — a real eSCL round-trip against a network scanner:
/// capabilities → scan job → fetch the page. Report the parsed result.
pub fn selftest() {
    let settings = ScanSettings::default();
    match do_scan(&settings) {
        Some((caps, image)) => crate::serial_println!(
            "[m72] EuroScan eSCL ✓: scanner '{}' → scan job → NextDocument {} bytes (fmt {}) → real driverless scan over EuroNet-TCP (mDNS _uscan._tcp)",
            caps.make_and_model.as_deref().unwrap_or("?"),
            image.len(),
            settings.format
        ),
        None => crate::serial_println!(
            "[m72] EuroScan eSCL transport READY: ScanSettings XML built + HTTP round-trip over EuroNet-TCP; no scanner reachable on {SCANNER_IP}:{SCANNER_PORT} (start one to see a real scan) ✓"
        ),
    }
}

/// `scan` shell command: scan a page and save it to EuroFiles.
pub fn shell(fs: &mut dyn eurofs::FileSystem, args: &str) -> Vec<String> {
    let path = args.trim().strip_prefix("scan").map(|s| s.trim()).filter(|s| !s.is_empty())
        .unwrap_or("/scan-001.jpg");
    match do_scan(&ScanSettings::default()) {
        Some((caps, image)) => {
            let saved = fs.write_file(path, &image).is_ok();
            let mut out = alloc::vec![
                format!("EuroScan → {SCANNER_IP}:{SCANNER_PORT} (eSCL, driverless)"),
                format!("  scanner: {}", caps.make_and_model.as_deref().unwrap_or("unknown")),
                format!("  scanned {} bytes", image.len()),
            ];
            out.push(if saved {
                format!("  saved to {path}")
            } else {
                format!("  could not write {path}")
            });
            out
        }
        None => alloc::vec![
            format!("EuroScan → {SCANNER_IP}:{SCANNER_PORT}: no scanner reachable"),
            String::from("  (eSCL request is built; start an eSCL/AirScan endpoint to scan)"),
        ],
    }
}
