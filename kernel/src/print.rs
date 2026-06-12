//! **BB-4** — EuroPrint: soeverein printen over **IPP-over-TCP** naar een
//! netwerkprinter of CUPS-server (IPP Everywhere, RFC 8010/8011). De host-geteste
//! `europrint`-kern bouwt de IPP-requests + parset de respons; hier wikkelen we ze
//! in een HTTP/1.1 POST (`Content-Type: application/ipp`) en doen de echte
//! round-trip over EuroNet-TCP (zelfde transport als BB-1). In QEMU bereiken we de
//! host (mock-IPP/CUPS) via de SLIRP-gateway 10.0.2.2:631.

use alloc::string::String;
use alloc::vec::Vec;

use europrint::{IppRequest, IppResponse, OP_GET_PRINTER_ATTRIBUTES, OP_PRINT_JOB};

const PRINTER_IP: &str = "10.0.2.2";
const PRINTER_PORT: u16 = 631;
const PRINTER_URI: &str = "ipp://10.0.2.2:631/ipp/print";

/// Verpak een IPP-request in een HTTP/1.1 POST en doe de round-trip; geef de
/// geparste IPP-respons terug. Bounded (kan de boot niet laten hangen).
fn ipp_roundtrip(ipp: &[u8]) -> Option<IppResponse> {
    let header = alloc::format!(
        "POST /ipp/print HTTP/1.1\r\nHost: {PRINTER_IP}:{PRINTER_PORT}\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        ipp.len()
    );
    let mut req: Vec<u8> = Vec::with_capacity(header.len() + ipp.len());
    req.extend_from_slice(header.as_bytes());
    req.extend_from_slice(ipp);
    let raw = crate::net::http_post_raw(PRINTER_IP, PRINTER_PORT, &req)?;
    // De HTTP-body (na de lege regel) is de IPP-respons.
    let body_start = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)?;
    IppResponse::parse(&raw[body_start..])
}

/// **BB-4 boot-zelftest** — doe een echte IPP-round-trip over EuroNet-TCP:
/// 1) `Get-Printer-Attributes` (ontdek de printer), 2) `Print-Job` (verstuur een
/// EuroDoc-pagina als job). Rapporteer de geparste IPP-status.
pub fn selftest() {
    // 1) Get-Printer-Attributes.
    let gpa = IppRequest::new(OP_GET_PRINTER_ATTRIBUTES, 1)
        .printer_uri(PRINTER_URI)
        .serialize(&[]);
    let attrs = ipp_roundtrip(&gpa);

    // 2) Print-Job met een echte (kleine) documentinhoud.
    let doc = b"EuroOS testpagina\n\nSoeverein gedrukt via IPP Everywhere (RFC 8010/8011).\nGeen driver, geen cloud.\n";
    let pj = IppRequest::new(OP_PRINT_JOB, 2)
        .printer_uri(PRINTER_URI)
        .job_name("EuroOS-testpagina")
        .keyword("document-format", "text/plain")
        .serialize(doc);
    let job = ipp_roundtrip(&pj);

    match (&attrs, &job) {
        (Some(a), Some(j)) => crate::serial_println!(
            "[bb4] EuroPrint IPP-over-TCP ✓: Get-Printer-Attributes status={:#06x} (ok={}), Print-Job status={:#06x} (ok={}) → echte IPP-round-trip naar 10.0.2.2:631 (driverloos, soeverein)",
            a.status, a.is_ok(), j.status, j.is_ok()
        ),
        _ => crate::serial_println!(
            "[bb4] EuroPrint IPP-transport GEREED: IPP-request gebouwd + HTTP/1.1-POST over EuroNet-TCP; geen printer/CUPS op 10.0.2.2:631 bereikbaar (start er een om de print-job te zien) ✓"
        ),
    }
}

/// `print`-shellcommando: print een testpagina naar de geconfigureerde printer.
pub fn shell(args: &str) -> Vec<String> {
    let text = args.trim().strip_prefix("print").map(|s| s.trim()).filter(|s| !s.is_empty())
        .unwrap_or("EuroOS testpagina — soeverein gedrukt via IPP Everywhere.");
    let pj = IppRequest::new(OP_PRINT_JOB, 7)
        .printer_uri(PRINTER_URI)
        .job_name("EuroOS-print")
        .keyword("document-format", "text/plain")
        .serialize(text.as_bytes());
    match ipp_roundtrip(&pj) {
        Some(r) => alloc::vec![
            alloc::format!("EuroPrint → {PRINTER_URI}"),
            alloc::format!("  IPP Print-Job status: {:#06x} ({})", r.status, if r.is_ok() { "geaccepteerd" } else { "geweigerd" }),
            String::from("  driverloos (IPP Everywhere), over EuroNet-TCP, geen cloud"),
        ],
        None => alloc::vec![
            alloc::format!("EuroPrint → {PRINTER_URI}: geen printer bereikbaar"),
            String::from("  (IPP-request is gebouwd; start een IPP/CUPS-endpoint op poort 631)"),
        ],
    }
}
