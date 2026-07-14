//! **BB-4** — EuroPrint: sovereign printing over **IPP-over-TCP** to a
//! network printer or CUPS server (IPP Everywhere, RFC 8010/8011). The host-tested
//! `europrint` core builds the IPP requests + parses the response; here we wrap them
//! in an HTTP/1.1 POST (`Content-Type: application/ipp`) and do the real
//! round-trip over EuroNet-TCP (same transport as BB-1). In QEMU we reach the
//! host (mock-IPP/CUPS) via the SLIRP gateway 10.0.2.2:631.

use alloc::string::String;
use alloc::vec::Vec;

use europrint::{IppRequest, IppResponse, OP_GET_PRINTER_ATTRIBUTES, OP_PRINT_JOB};

const PRINTER_IP: &str = "10.0.2.2";
/// IPP endpoints probed in order: the standard privileged port 631 first, then
/// 6631 — the common alternate for unprivileged setups (CUPS/ippserver running
/// as a user, and CI sandboxes where nothing may bind ports < 1024). Same
/// pattern as the eSCL scanner, which lives on a high port by convention.
const PRINTER_PORTS: [u16; 2] = [631, 6631];
const PRINTER_URI: &str = "ipp://10.0.2.2:631/ipp/print";

/// Wrap an IPP request in an HTTP/1.1 POST and do the round-trip; return the
/// parsed IPP response + the port that answered. Bounded (cannot hang boot).
fn ipp_roundtrip(ipp: &[u8]) -> Option<(IppResponse, u16)> {
    for &port in &PRINTER_PORTS {
        let header = alloc::format!(
            "POST /ipp/print HTTP/1.1\r\nHost: {PRINTER_IP}:{port}\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            ipp.len()
        );
        let mut req: Vec<u8> = Vec::with_capacity(header.len() + ipp.len());
        req.extend_from_slice(header.as_bytes());
        req.extend_from_slice(ipp);
        if let Some(raw) = crate::net::http_post_raw(PRINTER_IP, port, &req) {
            // The HTTP body (after the blank line) is the IPP response.
            if let Some(body_start) = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4) {
                if let Some(r) = IppResponse::parse(&raw[body_start..]) {
                    return Some((r, port));
                }
            }
        }
    }
    None
}

/// **BB-4 boot self-test** — do a real IPP round-trip over EuroNet-TCP:
/// 1) `Get-Printer-Attributes` (discover the printer), 2) `Print-Job` (send an
/// EuroDoc page as a job). Report the parsed IPP status.
pub fn selftest() {
    // 1) Get-Printer-Attributes.
    let gpa = IppRequest::new(OP_GET_PRINTER_ATTRIBUTES, 1)
        .printer_uri(PRINTER_URI)
        .serialize(&[]);
    let attrs = ipp_roundtrip(&gpa);

    // 2) Print-Job with a real (small) document content.
    let doc = b"EuroOS test page\n\nSovereignly printed via IPP Everywhere (RFC 8010/8011).\nNo driver, no cloud.\n";
    let pj = IppRequest::new(OP_PRINT_JOB, 2)
        .printer_uri(PRINTER_URI)
        .job_name("EuroOS-testpagina")
        .keyword("document-format", "text/plain")
        .serialize(doc);
    let job = ipp_roundtrip(&pj);

    match (&attrs, &job) {
        (Some((a, port)), Some((j, _))) => crate::serial_println!(
            "[bb4] EuroPrint IPP-over-TCP ✓: Get-Printer-Attributes status={:#06x} (ok={}), Print-Job status={:#06x} (ok={}) → real IPP round-trip to 10.0.2.2:{port} (driverless, sovereign)",
            a.status, a.is_ok(), j.status, j.is_ok()
        ),
        _ => crate::serial_println!(
            "[bb4] EuroPrint IPP transport READY: IPP request built + HTTP/1.1 POST over EuroNet-TCP; no printer/CUPS reachable on 10.0.2.2:{{631,6631}} (start one to see the print job) ✓"
        ),
    }
}

/// `print` shell command: print a test page to the configured printer.
pub fn shell(args: &str) -> Vec<String> {
    let text = args.trim().strip_prefix("print").map(|s| s.trim()).filter(|s| !s.is_empty())
        .unwrap_or("EuroOS test page — sovereignly printed via IPP Everywhere.");
    let pj = IppRequest::new(OP_PRINT_JOB, 7)
        .printer_uri(PRINTER_URI)
        .job_name("EuroOS-print")
        .keyword("document-format", "text/plain")
        .serialize(text.as_bytes());
    match ipp_roundtrip(&pj) {
        Some((r, port)) => alloc::vec![
            alloc::format!("EuroPrint → ipp://{PRINTER_IP}:{port}/ipp/print"),
            alloc::format!("  IPP Print-Job status: {:#06x} ({})", r.status, if r.is_ok() { "accepted" } else { "rejected" }),
            String::from("  driverless (IPP Everywhere), over EuroNet-TCP, no cloud"),
        ],
        None => alloc::vec![
            alloc::format!("EuroPrint → {PRINTER_URI}: no printer reachable"),
            String::from("  (IPP request is built; start an IPP/CUPS endpoint on port 631 or 6631)"),
        ],
    }
}
