//! Boot-zelftest voor **EuroMail** (AC-4): RFC822/MIME-berichtparser.
//! Kern: [`euromail`].

use crate::serial_println;

pub fn selftest() {
    // Decoders.
    let b64 = euromail::base64_decode("RXVyb09T") == b"EuroOS";
    let qp = euromail::quoted_printable_decode("caf=C3=A9", false) == "café".as_bytes();
    let hdr = euromail::decode_header("=?utf-8?B?SGFsbG8=?=") == "Hallo";

    // Volledig multipart-bericht met bijlage.
    let raw = "From: Jan <jan@euro-os.eu>\r\nTo: anna@x.be\r\nSubject: =?utf-8?Q?Caf=C3=A9?=\r\nContent-Type: multipart/mixed; boundary=\"XX\"\r\n\r\n--XX\r\nContent-Type: text/plain\r\n\r\nHallo wereld\r\n--XX\r\nContent-Type: application/octet-stream; name=\"data.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nRXVyb09T\r\n--XX--\r\n";
    let e = euromail::parse(raw);
    let from_ok = e.from.first().map(|a| a.email == "jan@euro-os.eu" && a.name == "Jan").unwrap_or(false);
    let subj_ok = e.subject == "Café";
    let text_ok = e.text.contains("Hallo wereld");
    let attach_ok = e.attachments.len() == 1
        && e.attachments[0].filename == "data.bin"
        && e.attachments[0].data == b"EuroOS";

    let ok = b64 && qp && hdr && from_ok && subj_ok && text_ok && attach_ok;
    serial_println!(
        "[ma] EuroMail: base64={} QP={} RFC2047={} | van={} onderwerp(\"{}\")={} tekst={} bijlage={} {}",
        b64, qp, hdr, from_ok, e.subject, subj_ok, text_ok, attach_ok,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
