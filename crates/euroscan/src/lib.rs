//! EuroScan — eSCL / AirScan driverless scanning (plan M7-2).
//!
//! The scanner counterpart of driverless printing: modern scanners (and
//! multifunction printers) expose the **eSCL** protocol — a small REST/XML API
//! over HTTP(S), discovered via mDNS `_uscan._tcp`. EuroOS speaks it directly,
//! with **no scanner driver at all**, exactly the network-first stance in
//! `docs/SUPPORT-POLICY.md`.
//!
//! The flow (eSCL 1.0, "Mopria/AirScan"):
//!   1. `GET  /eSCL/ScannerCapabilities` → XML: formats, resolutions, sources.
//!   2. `POST /eSCL/ScanJobs` with a `ScanSettings` XML body → `201 Created`
//!      with a `Location:` header pointing at the new job.
//!   3. `GET  <job>/NextDocument` → the scanned page bytes (JPEG or PDF).
//!
//! This crate builds the request bodies and parses the responses. The error-
//! prone XML/HTTP handling is pure `no_std` logic → fully host-tested; the
//! kernel `scan.rs` wraps it in real HTTP-over-EuroNet.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The mDNS service type driverless scanners advertise (`_uscans._tcp` for TLS).
pub const ESCL_SERVICE: &str = "_uscan._tcp.local";
pub const ESCL_SERVICE_TLS: &str = "_uscans._tcp.local";

/// A scan input source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Platen, // flatbed
    Adf,    // automatic document feeder
}

impl Source {
    fn tag(self) -> &'static str {
        match self {
            Source::Platen => "Platen",
            Source::Adf => "Feeder",
        }
    }
}

/// A colour mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Rgb24,
    Gray8,
    BlackAndWhite1,
}

impl ColorMode {
    fn tag(self) -> &'static str {
        match self {
            ColorMode::Rgb24 => "RGB24",
            ColorMode::Gray8 => "Grayscale8",
            ColorMode::BlackAndWhite1 => "BlackAndWhite1",
        }
    }
}

/// The settings for one scan job.
#[derive(Debug, Clone)]
pub struct ScanSettings {
    pub source: Source,
    pub color: ColorMode,
    pub dpi: u32,
    pub format: String, // MIME, e.g. "image/jpeg" or "application/pdf"
}

impl Default for ScanSettings {
    fn default() -> Self {
        ScanSettings {
            source: Source::Platen,
            color: ColorMode::Rgb24,
            dpi: 300,
            format: String::from("image/jpeg"),
        }
    }
}

impl ScanSettings {
    /// Build the eSCL `ScanSettings` XML body for `POST /eSCL/ScanJobs`.
    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <scan:ScanSettings xmlns:scan=\"http://schemas.hp.com/imaging/escl/2011/05/03\" \
             xmlns:pwg=\"http://www.pwg.org/schemas/2010/12/sm\">\
             <pwg:Version>2.6</pwg:Version>\
             <scan:Intent>Document</scan:Intent>\
             <pwg:InputSource>{}</pwg:InputSource>\
             <scan:ColorMode>{}</scan:ColorMode>\
             <scan:XResolution>{}</scan:XResolution>\
             <scan:YResolution>{}</scan:YResolution>\
             <pwg:DocumentFormat>{}</pwg:DocumentFormat>\
             </scan:ScanSettings>",
            self.source.tag(),
            self.color.tag(),
            self.dpi,
            self.dpi,
            self.format
        )
    }
}

/// A subset of the parsed `ScannerCapabilities` (enough to pick sane settings +
/// prove the scanner answered).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub make_and_model: Option<String>,
    pub has_platen: bool,
    pub has_adf: bool,
    pub formats: Vec<String>,
    pub max_dpi: u32,
}

impl Capabilities {
    /// Parse a `ScannerCapabilities` XML document (tolerant, tag-scanning; not a
    /// full XML parser — eSCL documents are simple and flat enough).
    pub fn parse(xml: &str) -> Capabilities {
        let mut c = Capabilities {
            make_and_model: inner(xml, "MakeAndModel").map(String::from),
            ..Default::default()
        };
        c.has_platen = xml.contains("<scan:Platen") || xml.contains("<Platen");
        c.has_adf = xml.contains(":Adf") || xml.contains("<Adf") || xml.contains("Feeder");
        // Every DocumentFormat / DocumentFormatExt element.
        for tag in ["pwg:DocumentFormat", "scan:DocumentFormatExt", "DocumentFormat"] {
            let mut rest = xml;
            while let Some(v) = inner(rest, strip_ns(tag)) {
                if !v.is_empty() && !c.formats.iter().any(|f| *f == v) {
                    c.formats.push(String::from(v));
                }
                // advance past this occurrence
                match rest.find(&format!("</{tag}>")) {
                    Some(i) => rest = &rest[i + tag.len() + 3..],
                    None => break,
                }
            }
        }
        // Largest resolution value we can find.
        for key in ["XResolution", "Resolution"] {
            let mut rest = xml;
            while let Some(v) = inner(rest, key) {
                if let Ok(n) = v.trim().parse::<u32>() {
                    c.max_dpi = c.max_dpi.max(n);
                }
                match rest.find(&format!("</{key}>")).or_else(|| rest.find(&format!("{key}>"))) {
                    Some(i) => rest = &rest[i + key.len() + 1..],
                    None => break,
                }
            }
        }
        c
    }
}

/// Strip a namespace prefix (`pwg:Foo` → `Foo`).
fn strip_ns(tag: &str) -> &str {
    tag.rsplit(':').next().unwrap_or(tag)
}

/// Return the text between the first `<...Name>` and `</...Name>` (namespace-
/// agnostic on the closing tag: matches any prefix). `None` if absent.
fn inner<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    // Find an opening tag ending in `name>` (covers `<pwg:name>` and `<name>`).
    let open_pat = format!("{name}>");
    let start_tag = xml.find(&open_pat)?;
    let start = start_tag + open_pat.len();
    // Closing tag: `</...name>`.
    let close_pat = "</";
    let rest = &xml[start..];
    // Find the next `</...name>`.
    let mut idx = 0;
    loop {
        let rel = rest[idx..].find(&close_pat)?;
        let abs = idx + rel;
        let after = &rest[abs + 2..];
        if let Some(gt) = after.find('>') {
            let tagname = &after[..gt];
            if tagname.ends_with(name) {
                return Some(rest[..abs].trim());
            }
            idx = abs + 2 + gt + 1;
        } else {
            return None;
        }
    }
}

/// Parse an HTTP response into `(status_code, headers_blob, body)`.
pub fn parse_http(raw: &[u8]) -> Option<(u16, &str, &[u8])> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = core::str::from_utf8(&raw[..split]).ok()?;
    let body = &raw[split + 4..];
    let status_line = head.lines().next()?;
    // "HTTP/1.1 201 Created"
    let code = status_line.split_whitespace().nth(1)?.parse::<u16>().ok()?;
    Some((code, head, body))
}

/// Extract the `Location:` header value (the job URI) from a headers blob.
pub fn location_header(headers: &str) -> Option<&str> {
    for line in headers.lines() {
        if let Some(v) = line
            .strip_prefix("Location:")
            .or_else(|| line.strip_prefix("location:"))
        {
            return Some(v.trim());
        }
    }
    None
}

/// Turn an absolute-or-relative job Location into a path for the NextDocument
/// GET (drops scheme+host if present; ensures it ends without a trailing slash).
pub fn job_path(location: &str) -> String {
    let path = if let Some(i) = location.find("://") {
        // skip scheme://host
        match location[i + 3..].find('/') {
            Some(j) => &location[i + 3 + j..],
            None => "/",
        }
    } else {
        location
    };
    let trimmed = path.trim_end_matches('/');
    format!("{trimmed}/NextDocument")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_settings_xml_has_fields() {
        let s = ScanSettings {
            source: Source::Adf,
            color: ColorMode::Gray8,
            dpi: 600,
            format: String::from("application/pdf"),
        };
        let xml = s.to_xml();
        assert!(xml.contains("<pwg:InputSource>Feeder</pwg:InputSource>"));
        assert!(xml.contains("<scan:ColorMode>Grayscale8</scan:ColorMode>"));
        assert!(xml.contains("<scan:XResolution>600</scan:XResolution>"));
        assert!(xml.contains("<pwg:DocumentFormat>application/pdf</pwg:DocumentFormat>"));
    }

    #[test]
    fn capabilities_parse() {
        let xml = "<?xml version=\"1.0\"?>\
            <scan:ScannerCapabilities xmlns:scan=\"x\" xmlns:pwg=\"y\">\
            <pwg:MakeAndModel>EuroScan Virtual 3000</pwg:MakeAndModel>\
            <scan:Platen><scan:PlatenInputCaps>\
            <scan:SettingProfiles><scan:SettingProfile>\
            <pwg:DocumentFormat>image/jpeg</pwg:DocumentFormat>\
            <scan:DocumentFormatExt>application/pdf</scan:DocumentFormatExt>\
            <scan:ColorModes><scan:ColorMode>RGB24</scan:ColorMode></scan:ColorModes>\
            <scan:SupportedResolutions><scan:DiscreteResolutions>\
            <scan:DiscreteResolution><scan:XResolution>300</scan:XResolution>\
            <scan:YResolution>300</scan:YResolution></scan:DiscreteResolution>\
            <scan:DiscreteResolution><scan:XResolution>600</scan:XResolution>\
            </scan:DiscreteResolution></scan:DiscreteResolutions></scan:SupportedResolutions>\
            </scan:SettingProfile></scan:SettingProfiles>\
            </scan:PlatenInputCaps></scan:Platen>\
            </scan:ScannerCapabilities>";
        let c = Capabilities::parse(xml);
        assert_eq!(c.make_and_model.as_deref(), Some("EuroScan Virtual 3000"));
        assert!(c.has_platen);
        assert!(c.formats.iter().any(|f| f == "image/jpeg"));
        assert!(c.formats.iter().any(|f| f == "application/pdf"));
        assert_eq!(c.max_dpi, 600);
    }

    #[test]
    fn http_and_location_parse() {
        let raw = b"HTTP/1.1 201 Created\r\nLocation: http://10.0.2.2:8631/eSCL/ScanJobs/9d2\r\nContent-Length: 0\r\n\r\n";
        let (code, head, body) = parse_http(raw).unwrap();
        assert_eq!(code, 201);
        assert_eq!(body.len(), 0);
        let loc = location_header(head).unwrap();
        assert_eq!(loc, "http://10.0.2.2:8631/eSCL/ScanJobs/9d2");
        assert_eq!(job_path(loc), "/eSCL/ScanJobs/9d2/NextDocument");
    }

    #[test]
    fn job_path_relative() {
        assert_eq!(job_path("/eSCL/ScanJobs/abc/"), "/eSCL/ScanJobs/abc/NextDocument");
    }

    #[test]
    fn next_document_body_returned() {
        // A 200 response whose body is the scanned image.
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 4\r\n\r\n".to_vec();
        raw.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0]); // JPEG SOI
        let (code, _h, body) = parse_http(&raw).unwrap();
        assert_eq!(code, 200);
        assert_eq!(body, &[0xFF, 0xD8, 0xFF, 0xE0]);
    }
}
