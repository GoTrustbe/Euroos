//! EuroPrint — IPP (Internet Printing Protocol, RFC 8010/8011) core (plan I4).
//!
//! IPP is a binary protocol over HTTP with which EuroOS prints to network printers
//! (the modern, driverless standard — IPP Everywhere). This module builds valid
//! IPP **requests** (`Print-Job`, `Get-Printer-Attributes`) and parses the **status**
//! + attributes of an IPP response. The HTTP transport layer (POST to the printer)
//!   runs on top of it via EuroNet/EuroTLS. Pure `no_std` logic → the
//!   error-prone binary encoding is fully tested on the host.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// IPP operation ids.
pub const OP_PRINT_JOB: u16 = 0x0002;
pub const OP_GET_PRINTER_ATTRIBUTES: u16 = 0x000B;
pub const OP_GET_JOBS: u16 = 0x000A;

// IPP status codes.
pub const STATUS_OK: u16 = 0x0000;

// Attribute group tags.
const TAG_OPERATION: u8 = 0x01;
const TAG_END: u8 = 0x03;

// Value tags.
const TAG_INTEGER: u8 = 0x21;
const TAG_KEYWORD: u8 = 0x44;
const TAG_URI: u8 = 0x45;
const TAG_NAME: u8 = 0x42; // nameWithoutLanguage
const TAG_CHARSET: u8 = 0x47;
const TAG_LANGUAGE: u8 = 0x48;

/// An IPP attribute (within an operation group).
struct Attr {
    tag: u8,
    name: String,
    value: Vec<u8>,
}

/// An IPP request under construction.
pub struct IppRequest {
    operation: u16,
    request_id: u32,
    attrs: Vec<Attr>,
}

impl IppRequest {
    /// Begin a request for `operation` with `request_id`. The mandatory
    /// `attributes-charset` (utf-8) + `attributes-natural-language` (en) are added as
    /// the FIRST two attributes, as IPP prescribes.
    pub fn new(operation: u16, request_id: u32) -> Self {
        let mut r = IppRequest { operation, request_id, attrs: Vec::new() };
        r.attrs.push(Attr { tag: TAG_CHARSET, name: "attributes-charset".into(), value: b"utf-8".to_vec() });
        r.attrs.push(Attr {
            tag: TAG_LANGUAGE,
            name: "attributes-natural-language".into(),
            value: b"en".to_vec(),
        });
        r
    }

    /// Add the target printer URI (`printer-uri`).
    pub fn printer_uri(mut self, uri: &str) -> Self {
        self.attrs.push(Attr { tag: TAG_URI, name: "printer-uri".into(), value: uri.as_bytes().to_vec() });
        self
    }

    /// Add the job name (`job-name`).
    pub fn job_name(mut self, name: &str) -> Self {
        self.attrs.push(Attr { tag: TAG_NAME, name: "job-name".into(), value: name.as_bytes().to_vec() });
        self
    }

    /// Add a keyword attribute (e.g. `document-format`).
    pub fn keyword(mut self, name: &str, value: &str) -> Self {
        self.attrs.push(Attr { tag: TAG_KEYWORD, name: name.into(), value: value.as_bytes().to_vec() });
        self
    }

    /// Add an integer attribute (e.g. `copies`).
    pub fn integer(mut self, name: &str, value: i32) -> Self {
        self.attrs.push(Attr { tag: TAG_INTEGER, name: name.into(), value: value.to_be_bytes().to_vec() });
        self
    }

    /// Serialize to the IPP binary form. `doc` (optional) is added after the
    /// end-of-attributes tag (the document bytes to be printed, e.g. PDF).
    pub fn serialize(&self, doc: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.push(2); // version-major
        b.push(0); // version-minor (IPP 2.0)
        b.extend_from_slice(&self.operation.to_be_bytes());
        b.extend_from_slice(&self.request_id.to_be_bytes());
        b.push(TAG_OPERATION); // begin operation-attributes-group
        for a in &self.attrs {
            b.push(a.tag);
            b.extend_from_slice(&(a.name.len() as u16).to_be_bytes());
            b.extend_from_slice(a.name.as_bytes());
            b.extend_from_slice(&(a.value.len() as u16).to_be_bytes());
            b.extend_from_slice(&a.value);
        }
        b.push(TAG_END); // end-of-attributes
        b.extend_from_slice(doc);
        b
    }
}

/// A parsed IPP response: status + (name, value-bytes) attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IppResponse {
    pub version: (u8, u8),
    pub status: u16,
    pub request_id: u32,
    pub attributes: Vec<(String, Vec<u8>)>,
}

impl IppResponse {
    /// Parse an IPP response header + attributes (up to the end tag). `None` on garbage.
    pub fn parse(data: &[u8]) -> Option<IppResponse> {
        if data.len() < 8 {
            return None;
        }
        let version = (data[0], data[1]);
        let status = u16::from_be_bytes([data[2], data[3]]);
        let request_id = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let mut attributes = Vec::new();
        let mut p = 8;
        while p < data.len() {
            let tag = data[p];
            p += 1;
            if tag == TAG_END {
                break;
            }
            if tag <= 0x0F {
                continue; // group-begin tag (operation/job/printer) → next attribute
            }
            // value-tag: name-len, name, value-len, value.
            if p + 2 > data.len() {
                return None;
            }
            let nlen = u16::from_be_bytes([data[p], data[p + 1]]) as usize;
            p += 2;
            if p + nlen + 2 > data.len() {
                return None;
            }
            let name = String::from_utf8_lossy(&data[p..p + nlen]).into_owned();
            p += nlen;
            let vlen = u16::from_be_bytes([data[p], data[p + 1]]) as usize;
            p += 2;
            if p + vlen > data.len() {
                return None;
            }
            let value = data[p..p + vlen].to_vec();
            p += vlen;
            // An empty name = continuation value of the previous attribute (1setOf) — skip.
            if !name.is_empty() {
                attributes.push((name, value));
            }
        }
        Some(IppResponse { version, status, request_id, attributes })
    }

    pub fn is_ok(&self) -> bool {
        self.status == STATUS_OK
    }
    pub fn attr(&self, name: &str) -> Option<&[u8]> {
        self.attributes.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_slice())
    }
}

/// HTTP transport layer for IPP (RFC 8010 §5): an IPP request travels as the **body
/// of an HTTP POST** with `Content-Type: application/ipp`. This module builds the
/// HTTP envelope around a serialized IPP request and parses the HTTP response
/// (status + body) out of it. The bytes go via EuroNet (port 631) or EuroTLS (ipps).
pub mod http {
    use alloc::format;
    use alloc::vec::Vec;

    /// Build a full HTTP/1.1 `POST` request with the IPP payload as body.
    /// `path` is usually `/ipp/print` or `/`; `host` the printer hostname.
    pub fn post_request(host: &str, path: &str, ipp_body: &[u8]) -> Vec<u8> {
        let head = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             Content-Type: application/ipp\r\n\
             Content-Length: {}\r\n\
             Accept: application/ipp\r\n\
             Connection: close\r\n\r\n",
            ipp_body.len()
        );
        let mut out = head.into_bytes();
        out.extend_from_slice(ipp_body);
        out
    }

    /// Parse an HTTP response: return (status code, body). Supports both
    /// `Content-Length` and `Transfer-Encoding: chunked` (the two forms that
    /// CUPS/IPP servers use). None if the response is malformed.
    pub fn parse_response(data: &[u8]) -> Option<(u16, Vec<u8>)> {
        // Split headers/body on the first empty line (CRLFCRLF).
        let sep = find_subsequence(data, b"\r\n\r\n")?;
        let header = &data[..sep];
        let body_raw = &data[sep + 4..];

        // Status code from the first line: "HTTP/1.1 200 OK".
        let first_line_end = find_subsequence(header, b"\r\n").unwrap_or(header.len());
        let status_line = core::str::from_utf8(&header[..first_line_end]).ok()?;
        let status: u16 = status_line.split(' ').nth(1)?.parse().ok()?;

        // Header-field helper (case-insensitive on name).
        let header_str = core::str::from_utf8(header).ok()?;
        let chunked = header_value(header_str, "transfer-encoding")
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false);

        let body = if chunked {
            dechunk(body_raw)
        } else if let Some(cl) = header_value(header_str, "content-length").and_then(|v| v.trim().parse::<usize>().ok()) {
            body_raw[..cl.min(body_raw.len())].to_vec()
        } else {
            body_raw.to_vec()
        };
        Some((status, body))
    }

    fn header_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
        for line in header.split("\r\n").skip(1) {
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case(name) {
                    return Some(v.trim());
                }
            }
        }
        None
    }

    /// Decode a `chunked` body: successive `<hex-len>\r\n<data>\r\n`, ending
    /// on a 0-chunk.
    fn dechunk(mut data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(nl) = find_subsequence(data, b"\r\n") {
            let len_str = match core::str::from_utf8(&data[..nl]) {
                Ok(s) => s.trim(),
                Err(_) => break,
            };
            let len = match usize::from_str_radix(len_str.split(';').next().unwrap_or("0"), 16) {
                Ok(l) => l,
                Err(_) => break,
            };
            if len == 0 {
                break;
            }
            let start = nl + 2;
            let end = (start + len).min(data.len());
            out.extend_from_slice(&data[start..end]);
            // Skip the data + the trailing CRLF.
            if end + 2 > data.len() {
                break;
            }
            data = &data[end + 2..];
        }
        out
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_post_envelope() {
        let ipp = IppRequest::new(OP_GET_PRINTER_ATTRIBUTES, 1).serialize(&[]);
        let req = http::post_request("printer.local", "/ipp/print", &ipp);
        let s = String::from_utf8_lossy(&req);
        assert!(s.starts_with("POST /ipp/print HTTP/1.1\r\n"));
        assert!(s.contains("Content-Type: application/ipp\r\n"));
        assert!(s.contains(&format!("Content-Length: {}\r\n", ipp.len())));
        // The body after the empty line is exactly the IPP payload.
        let sep = req.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        assert_eq!(&req[sep + 4..], &ipp[..]);
    }

    #[test]
    fn http_parse_content_length() {
        let ipp = IppRequest::new(STATUS_OK, 7).serialize(&[]);
        let mut resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n",
            ipp.len()
        )
        .into_bytes();
        resp.extend_from_slice(&ipp);
        let (status, body) = http::parse_response(&resp).unwrap();
        assert_eq!(status, 200);
        // The body is a valid IPP response that we can parse.
        assert!(IppResponse::parse(&body).unwrap().is_ok());
    }

    #[test]
    fn http_parse_chunked() {
        // Body "ABCDEFGH" in two chunks (4 + 4) + 0-terminator.
        let resp = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nABCD\r\n4\r\nEFGH\r\n0\r\n\r\n";
        let (status, body) = http::parse_response(resp).unwrap();
        assert_eq!(status, 200);
        assert_eq!(&body, b"ABCDEFGH");
    }

    #[test]
    fn http_error_status() {
        let resp = b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\n\r\n";
        let (status, body) = http::parse_response(resp).unwrap();
        assert_eq!(status, 426);
        assert!(body.is_empty());
        assert!(http::parse_response(b"garbage").is_none());
    }

    #[test]
    fn print_job_request_structure() {
        let doc = b"%PDF-1.4 ...";
        let req = IppRequest::new(OP_PRINT_JOB, 1)
            .printer_uri("ipp://printer.local/ipp/print")
            .job_name("euro-test")
            .keyword("document-format", "application/pdf")
            .serialize(doc);
        // header
        assert_eq!(&req[0..2], &[2, 0]); // version 2.0
        assert_eq!(u16::from_be_bytes([req[2], req[3]]), OP_PRINT_JOB);
        assert_eq!(u32::from_be_bytes([req[4], req[5], req[6], req[7]]), 1);
        assert_eq!(req[8], TAG_OPERATION); // operation-attributes-group
        // the document is located after the end tag.
        let end = req.iter().rposition(|&b| b == TAG_END).unwrap();
        assert_eq!(&req[end + 1..], doc);
    }

    #[test]
    fn charset_and_language_are_first() {
        let req = IppRequest::new(OP_GET_PRINTER_ATTRIBUTES, 7).serialize(&[]);
        // after the 8-byte header + group-tag (idx 8) comes the first attribute.
        let nlen = u16::from_be_bytes([req[10], req[11]]) as usize;
        let name = core::str::from_utf8(&req[12..12 + nlen]).unwrap();
        assert_eq!(name, "attributes-charset");
    }

    #[test]
    fn integer_attribute_is_4_bytes_be() {
        let req = IppRequest::new(OP_PRINT_JOB, 1).integer("copies", 3).serialize(&[]);
        // find "copies" + read the value.
        let resp = IppResponse::parse(&req).unwrap();
        // parse reads the operation-group as attributes; "copies" must be 3.
        let v = resp.attr("copies").unwrap();
        assert_eq!(v, &3i32.to_be_bytes());
    }

    #[test]
    fn response_roundtrip() {
        // Build a "response" by encoding a request (same wire form) and parsing it
        // back — proves the header + attribute encoding.
        let bytes = IppRequest::new(STATUS_OK, 42)
            .printer_uri("ipp://p/")
            .serialize(&[]);
        let r = IppResponse::parse(&bytes).unwrap();
        assert_eq!(r.version, (2, 0));
        assert_eq!(r.request_id, 42);
        assert!(r.is_ok()); // status 0x0000
        assert_eq!(r.attr("printer-uri").unwrap(), b"ipp://p/");
        assert_eq!(r.attr("attributes-charset").unwrap(), b"utf-8");
    }

    #[test]
    fn parse_rejects_truncated() {
        assert_eq!(IppResponse::parse(&[2, 0, 0]), None);
    }
}
