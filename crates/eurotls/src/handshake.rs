//! TLS 1.3 client state machine (RFC 8446 §4). Sans-IO: feed records in via
//! [`Tls13Client::feed`], process via [`Tls13Client::process`] (returns the
//! bytes to send), and afterwards exchange application data. Ciphersuite
//! TLS_CHACHA20_POLY1305_SHA256, key exchange X25519.

use alloc::string::String;
use alloc::vec::Vec;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::aead;
use crate::chain;
use crate::keyschedule::{KeySchedule, TrafficKeys, Transcript};
use crate::sig;
use crate::x509::{Certificate, PubKeyAlg};
use crate::record::{
    aead_aad, build_record, read_record, CT_ALERT, CT_APPLICATION_DATA, CT_CHANGE_CIPHER_SPEC, CT_HANDSHAKE,
};
use crate::{GROUP_X25519, SIG_ECDSA_P256, SIG_ED25519, SIG_RSA_PKCS1_SHA256, SIG_RSA_PSS_SHA256, TLS_CHACHA20_POLY1305_SHA256};

type HmacSha256 = Hmac<Sha256>;

// Handshake message types.
const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_NEW_SESSION_TICKET: u8 = 4;
const HS_ENCRYPTED_EXTENSIONS: u8 = 8;
const HS_CERTIFICATE: u8 = 11;
const HS_CERTIFICATE_VERIFY: u8 = 15;
const HS_FINISHED: u8 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsState {
    Start,
    WaitServerHello,
    WaitServerFinished,
    Connected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsError {
    NeedMore,
    Alert(u8),
    BadRecord,
    Decrypt,
    BadFinished,
    Unsupported,
    Protocol(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Epoch {
    None,
    Handshake,
    Application,
}

pub struct Tls13Client {
    state: TlsState,
    transcript: Transcript,
    ks: KeySchedule,
    secret: StaticSecret,
    sni: String,

    client_hs: Option<TrafficKeys>,
    server_hs: Option<TrafficKeys>,
    client_ap: Option<TrafficKeys>,
    server_ap: Option<TrafficKeys>,

    server_epoch: Epoch,
    client_epoch: Epoch,
    server_seq: u64,
    client_seq: u64,

    rx: Vec<u8>,      // raw received record bytes
    hs_buf: Vec<u8>,  // decrypted handshake bytes (reassembly)
    app_buf: Vec<u8>, // decrypted application data

    /// The raw server certificate (first in the chain) for later inspection.
    pub server_cert: Option<Vec<u8>>,
    /// The full chain offered by the server (leaf first), DER per cert.
    cert_chain: Vec<Vec<u8>>,
    /// Reference instant (epoch seconds) for the validity check; set via
    /// [`set_trust_anchor`].
    now: i64,
    /// Trusted root CAs (DER). `None` = no trust anchor set yet → the
    /// handshake FAILS at certificate validation (fail-closed), unless
    /// `allow_insecure_no_verification()` has been called explicitly.
    trust_roots: Option<&'static [&'static [u8]]>,
    /// Explicit, greppable opt-out: skip certificate validation (only for
    /// host tests/dev without a trust store). Defaults to `false` — never silently.
    insecure_skip_verify: bool,
}

impl Tls13Client {
    /// Create a client + the ClientHello record (ready to send in cleartext).
    /// `random` and `secret_scalar` MUST be real randomness
    /// (the kernel supplies them via RDRAND).
    pub fn new(sni: &str, random: [u8; 32], secret_scalar: [u8; 32]) -> (Self, Vec<u8>) {
        let secret = StaticSecret::from(secret_scalar);
        let pubkey = PublicKey::from(&secret);
        let mut c = Tls13Client {
            state: TlsState::Start,
            transcript: Transcript::new(),
            ks: KeySchedule::new(),
            secret,
            sni: String::from(sni),
            client_hs: None,
            server_hs: None,
            client_ap: None,
            server_ap: None,
            server_epoch: Epoch::None,
            client_epoch: Epoch::None,
            server_seq: 0,
            client_seq: 0,
            rx: Vec::new(),
            hs_buf: Vec::new(),
            app_buf: Vec::new(),
            server_cert: None,
            cert_chain: Vec::new(),
            now: 0,
            trust_roots: None,
            insecure_skip_verify: false,
        };
        let ch_body = c.build_client_hello(&random, pubkey.as_bytes());
        let ch_msg = wrap_handshake(HS_CLIENT_HELLO, &ch_body);
        c.transcript.update(&ch_msg);
        c.state = TlsState::WaitServerHello;
        let record = build_record(CT_HANDSHAKE, &ch_msg);
        (c, record)
    }

    pub fn state(&self) -> TlsState {
        self.state
    }
    pub fn is_connected(&self) -> bool {
        self.state == TlsState::Connected
    }

    /// Enable certificate validation: after receiving the server certificate
    /// the chain is anchored to `roots` (validity + hostname + per-step
    /// signature) AND the CertificateVerify signature is checked.
    /// `now` = epoch seconds (from the kernel RTC). MUST be called before
    /// `process()`. If never called, validation stays off (the
    /// handshake MAC remains binding regardless).
    pub fn set_trust_anchor(&mut self, now: i64, roots: &'static [&'static [u8]]) {
        self.now = now;
        self.trust_roots = Some(roots);
    }

    /// EXPLICIT opt-out: skip certificate and CertificateVerify validation.
    /// Strictly for host tests/dev without a trust store — NEVER in production.
    /// The name is deliberately loud and greppable so an audit finds every insecure use.
    pub fn allow_insecure_no_verification(&mut self) {
        self.insecure_skip_verify = true;
    }

    /// Append received bytes to the internal record buffer.
    pub fn feed(&mut self, data: &[u8]) {
        self.rx.extend_from_slice(data);
    }

    // ── ClientHello ──────────────────────────────────────────────────────
    fn build_client_hello(&self, random: &[u8; 32], key_share: &[u8; 32]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&[0x03, 0x03]); // legacy_version
        b.extend_from_slice(random);
        // legacy_session_id: 32 bytes (middlebox compat), reuse some randomness.
        b.push(32);
        b.extend_from_slice(random);
        // cipher_suites
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(&TLS_CHACHA20_POLY1305_SHA256.to_be_bytes());
        // legacy_compression_methods: [0]
        b.push(1);
        b.push(0);
        // extensions
        let mut ext = Vec::new();
        // server_name (SNI)
        {
            let host = self.sni.as_bytes();
            let mut sni = Vec::new();
            let mut list = Vec::new();
            list.push(0); // name_type host_name
            list.extend_from_slice(&(host.len() as u16).to_be_bytes());
            list.extend_from_slice(host);
            sni.extend_from_slice(&(list.len() as u16).to_be_bytes());
            sni.extend_from_slice(&list);
            push_ext(&mut ext, 0x0000, &sni);
        }
        // supported_versions: TLS 1.3
        push_ext(&mut ext, 0x002b, &[2u8, 0x03, 0x04]);
        // supported_groups: x25519
        {
            let mut g = Vec::new();
            g.extend_from_slice(&2u16.to_be_bytes());
            g.extend_from_slice(&GROUP_X25519.to_be_bytes());
            push_ext(&mut ext, 0x000a, &g);
        }
        // signature_algorithms
        {
            let algs = [SIG_ED25519, SIG_ECDSA_P256, SIG_RSA_PSS_SHA256, SIG_RSA_PKCS1_SHA256];
            let mut s = Vec::new();
            s.extend_from_slice(&((algs.len() * 2) as u16).to_be_bytes());
            for a in algs {
                s.extend_from_slice(&a.to_be_bytes());
            }
            push_ext(&mut ext, 0x000d, &s);
        }
        // key_share: x25519 public key
        {
            let mut ks = Vec::new();
            let mut entry = Vec::new();
            entry.extend_from_slice(&GROUP_X25519.to_be_bytes());
            entry.extend_from_slice(&(key_share.len() as u16).to_be_bytes());
            entry.extend_from_slice(key_share);
            ks.extend_from_slice(&(entry.len() as u16).to_be_bytes());
            ks.extend_from_slice(&entry);
            push_ext(&mut ext, 0x0033, &ks);
        }
        b.extend_from_slice(&(ext.len() as u16).to_be_bytes());
        b.extend_from_slice(&ext);
        b
    }

    // ── Processing ───────────────────────────────────────────────────────
    /// Process all complete records in the buffer; return the bytes to send
    /// (may be empty). Drives the state forward until `Connected`.
    pub fn process(&mut self) -> Result<Vec<u8>, TlsError> {
        let mut out = Vec::new();
        loop {
            let (rec, n) = match read_record(&self.rx) {
                Ok(Some(v)) => v,
                Ok(None) => break, // not enough bytes yet
                Err(_) => return Err(TlsError::Protocol("record length > 2^14+256")),
            };
            self.rx.drain(..n);
            match rec.ctype {
                CT_CHANGE_CIPHER_SPEC => continue, // middlebox compat: ignore
                CT_ALERT => {
                    self.state = TlsState::Failed;
                    let code = *rec.fragment.get(1).unwrap_or(&0);
                    return Err(TlsError::Alert(code));
                }
                CT_HANDSHAKE => {
                    // Only before the encrypted flight: this is the ServerHello.
                    self.hs_buf.extend_from_slice(&rec.fragment);
                    self.drain_handshake(&mut out)?;
                }
                CT_APPLICATION_DATA => {
                    // Encrypted record: decrypt with the current server epoch.
                    let keys = match self.server_epoch {
                        Epoch::Handshake => self.server_hs.as_ref(),
                        Epoch::Application => self.server_ap.as_ref(),
                        Epoch::None => return Err(TlsError::Protocol("data before keys")),
                    }
                    .ok_or(TlsError::Protocol("no server keys"))?;
                    let aad = aead_aad(rec.fragment.len());
                    let pt = aead::open(&keys.key, &keys.iv, self.server_seq, &aad, &rec.fragment)
                        .ok_or(TlsError::Decrypt)?;
                    self.server_seq += 1;
                    // Strip trailing zeros + inner content type (RFC 8446 §5.4).
                    let (inner_type, content) = split_inner(&pt)?;
                    match inner_type {
                        CT_HANDSHAKE => {
                            self.hs_buf.extend_from_slice(content);
                            self.drain_handshake(&mut out)?;
                        }
                        CT_APPLICATION_DATA => self.app_buf.extend_from_slice(content),
                        CT_ALERT => {
                            self.state = TlsState::Failed;
                            return Err(TlsError::Alert(*content.get(1).unwrap_or(&0)));
                        }
                        _ => return Err(TlsError::BadRecord),
                    }
                }
                _ => return Err(TlsError::BadRecord),
            }
        }
        Ok(out)
    }

    /// Parse complete handshake messages from `hs_buf` and process them.
    fn drain_handshake(&mut self, out: &mut Vec<u8>) -> Result<(), TlsError> {
        loop {
            if self.hs_buf.len() < 4 {
                return Ok(());
            }
            let mtype = self.hs_buf[0];
            let len = ((self.hs_buf[1] as usize) << 16) | ((self.hs_buf[2] as usize) << 8) | (self.hs_buf[3] as usize);
            if self.hs_buf.len() < 4 + len {
                return Ok(()); // wait for more
            }
            let msg: Vec<u8> = self.hs_buf.drain(..4 + len).collect();
            self.handle_handshake_msg(mtype, &msg, out)?;
        }
    }

    fn handle_handshake_msg(&mut self, mtype: u8, msg: &[u8], out: &mut Vec<u8>) -> Result<(), TlsError> {
        match mtype {
            HS_SERVER_HELLO => {
                self.transcript.update(msg);
                self.handle_server_hello(&msg[4..])?;
            }
            HS_ENCRYPTED_EXTENSIONS => {
                self.transcript.update(msg);
            }
            HS_CERTIFICATE => {
                self.parse_certificate(&msg[4..]);
                self.transcript.update(msg);
                // Verify the chain against the trust store (if enabled).
                self.validate_chain()?;
            }
            HS_CERTIFICATE_VERIFY => {
                // Here the server proves possession of the private key of the
                // leaf certificate by signing the transcript hash (up to and
                // including Certificate). Verify before we add this message to
                // the transcript (RFC 8446 §4.4.3).
                let th = self.transcript.hash();
                self.verify_certificate_verify(&msg[4..], &th)?;
                self.transcript.update(msg);
            }
            HS_FINISHED => {
                // Server Finished: verify the MAC over the transcript up to and
                // including CertificateVerify (so before we add this message).
                let server_hs = self.server_hs.as_ref().ok_or(TlsError::Protocol("no hs keys"))?;
                let expected = hmac_sha256(&server_hs.finished_key, &self.transcript.hash());
                if msg[4..] != expected[..] {
                    self.state = TlsState::Failed;
                    return Err(TlsError::BadFinished);
                }
                self.transcript.update(msg);
                self.finish_handshake(out)?;
            }
            HS_NEW_SESSION_TICKET => { /* after the handshake: ignore (no resumption) */ }
            _ => return Err(TlsError::Protocol("unexpected handshake message")),
        }
        Ok(())
    }

    fn handle_server_hello(&mut self, body: &[u8]) -> Result<(), TlsError> {
        // legacy_version(2) random(32) session_id<u8> cipher(2) comp(1) ext<u16>
        let mut p = 2 + 32;
        if body.len() < p + 1 {
            return Err(TlsError::BadRecord);
        }
        let sid_len = body[p] as usize;
        p += 1 + sid_len;
        if body.len() < p + 3 {
            return Err(TlsError::BadRecord);
        }
        let cipher = u16::from_be_bytes([body[p], body[p + 1]]);
        if cipher != TLS_CHACHA20_POLY1305_SHA256 {
            return Err(TlsError::Unsupported);
        }
        p += 2 + 1; // cipher + legacy_compression
        if body.len() < p + 2 {
            return Err(TlsError::BadRecord);
        }
        let ext_len = u16::from_be_bytes([body[p], body[p + 1]]) as usize;
        p += 2;
        let ext_end = p + ext_len;
        if body.len() < ext_end {
            return Err(TlsError::BadRecord);
        }
        // Find the key_share extension (0x0033) → server x25519 public key.
        let mut server_pub: Option<[u8; 32]> = None;
        let mut q = p;
        while q + 4 <= ext_end {
            let etype = u16::from_be_bytes([body[q], body[q + 1]]);
            let elen = u16::from_be_bytes([body[q + 2], body[q + 3]]) as usize;
            let edata = &body[q + 4..(q + 4 + elen).min(body.len())];
            if etype == 0x0033 && edata.len() >= 4 {
                let grp = u16::from_be_bytes([edata[0], edata[1]]);
                let klen = u16::from_be_bytes([edata[2], edata[3]]) as usize;
                if grp == GROUP_X25519 && klen == 32 && edata.len() >= 4 + 32 {
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&edata[4..36]);
                    server_pub = Some(k);
                }
            }
            q += 4 + elen;
        }
        let server_pub = server_pub.ok_or(TlsError::Protocol("no server key_share"))?;

        // ECDHE + key schedule (handshake secrets over transcript CH||SH).
        let shared = self.secret.diffie_hellman(&PublicKey::from(server_pub));
        let ecdhe = *shared.as_bytes();
        let th = self.transcript.hash();
        let (cs, ss) = self.ks.derive_handshake(&ecdhe, &th);
        self.client_hs = Some(cs);
        self.server_hs = Some(ss);
        self.server_epoch = Epoch::Handshake;
        self.server_seq = 0;
        self.state = TlsState::WaitServerFinished;
        Ok(())
    }

    fn parse_certificate(&mut self, body: &[u8]) {
        // certificate_request_context<u8> + certificate_list<u24>. Each entry =
        // cert_data<u24> (DER) + extensions<u16>. We keep the whole chain.
        self.cert_chain.clear();
        if body.is_empty() {
            return;
        }
        let ctx_len = body[0] as usize;
        let mut p = 1 + ctx_len;
        if body.len() < p + 3 {
            return;
        }
        let list_len = ((body[p] as usize) << 16) | ((body[p + 1] as usize) << 8) | (body[p + 2] as usize);
        p += 3;
        let list_end = (p + list_len).min(body.len());
        while p + 3 <= list_end {
            let cert_len = ((body[p] as usize) << 16) | ((body[p + 1] as usize) << 8) | (body[p + 2] as usize);
            p += 3;
            if p + cert_len > list_end {
                break;
            }
            self.cert_chain.push(body[p..p + cert_len].to_vec());
            p += cert_len;
            // Skip CertificateEntry extensions.
            if p + 2 > list_end {
                break;
            }
            let ext_len = u16::from_be_bytes([body[p], body[p + 1]]) as usize;
            p += 2 + ext_len;
        }
        self.server_cert = self.cert_chain.first().cloned();
    }

    /// Diagnostics: (sig_alg, pubkey_alg, is_ca) per certificate in the chain.
    pub fn cert_chain_info(&self) -> Vec<(crate::x509::SigAlg, PubKeyAlg, bool)> {
        self.cert_chain
            .iter()
            .filter_map(|d| Certificate::parse(d).ok().map(|c| (c.sig_alg, c.pubkey_alg, c.is_ca)))
            .collect()
    }

    /// Verify the offered chain against the trust store. FAIL-CLOSED: without a
    /// trust anchor the connection fails (unless `allow_insecure_no_
    /// verification()` is set explicitly). On failure: fail the connection.
    fn validate_chain(&mut self) -> Result<(), TlsError> {
        let roots = match self.trust_roots {
            Some(r) => r,
            None => {
                if self.insecure_skip_verify {
                    return Ok(()); // explicit opt-out (host test/dev)
                }
                self.state = TlsState::Failed;
                return Err(TlsError::Protocol("no trust anchor set — validation required"));
            }
        };
        if self.cert_chain.is_empty() {
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("server sent no certificate"));
        }
        let ts = chain::TrustStore::from_ders(roots);
        let slices: Vec<&[u8]> = self.cert_chain.iter().map(|v| v.as_slice()).collect();
        match chain::validate(&slices, &self.sni, self.now, &ts) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.state = TlsState::Failed;
                let why = match e {
                    chain::ChainError::EmptyChain => "chain empty",
                    chain::ChainError::Parse(_) => "cert unreadable",
                    chain::ChainError::Expired => "expired / not valid",
                    chain::ChainError::HostnameMismatch => "hostname matches no SAN",
                    chain::ChainError::BrokenChain => "chain broken (issuer≠subject)",
                    chain::ChainError::IssuerNotCa => "issuer is not a CA",
                    chain::ChainError::BadSignature => "signature in chain invalid",
                    chain::ChainError::UnknownCa => "no trusted root (unknown CA)",
                };
                Err(TlsError::Protocol(why))
            }
        }
    }

    /// Check the CertificateVerify signature with the public key of the leaf
    /// certificate (no-op if validation is off). `transcript_hash` =
    /// the transcript hash up to and including the Certificate message.
    fn verify_certificate_verify(&mut self, body: &[u8], transcript_hash: &[u8]) -> Result<(), TlsError> {
        if self.trust_roots.is_none() {
            if self.insecure_skip_verify {
                return Ok(()); // explicit opt-out (host test/dev)
            }
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("no trust anchor set — CertificateVerify required"));
        }
        if body.len() < 4 {
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("short CertificateVerify"));
        }
        let scheme = u16::from_be_bytes([body[0], body[1]]);
        let sig_len = u16::from_be_bytes([body[2], body[3]]) as usize;
        if body.len() < 4 + sig_len {
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("CertificateVerify length"));
        }
        let signature = &body[4..4 + sig_len];

        // The signed block (RFC 8446 §4.4.3): 64×0x20, context string, 0x00,
        // followed by the transcript hash.
        let mut content = Vec::with_capacity(64 + 34 + transcript_hash.len());
        content.extend_from_slice(&[0x20u8; 64]);
        content.extend_from_slice(b"TLS 1.3, server CertificateVerify");
        content.push(0x00);
        content.extend_from_slice(transcript_hash);

        let leaf_der = self.cert_chain.first().ok_or(TlsError::Protocol("no leaf certificate"))?;
        let leaf = Certificate::parse(leaf_der).map_err(|_| TlsError::Protocol("leaf cert unreadable"))?;

        // SignatureScheme → primitive; the leaf key type must match.
        let ok = match scheme {
            0x0403 => leaf.pubkey_alg == PubKeyAlg::EcP256 && sig::verify_ecdsa_p256(leaf.pubkey, &content, signature),
            0x0807 => leaf.pubkey_alg == PubKeyAlg::Ed25519 && sig::verify_ed25519(leaf.pubkey, &content, signature),
            0x0804 => leaf.pubkey_alg == PubKeyAlg::Rsa && sig::verify_rsa_pss_sha256(leaf.pubkey, &content, signature),
            _ => false,
        };
        if !ok {
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("CertificateVerify signature invalid"));
        }
        Ok(())
    }

    /// Server Finished verified → derive app keys, send (CCS +) our
    /// encrypted Finished, switch to the application epoch.
    fn finish_handshake(&mut self, out: &mut Vec<u8>) -> Result<(), TlsError> {
        // App keys over transcript CH..server Finished.
        let th_sf = self.transcript.hash();
        let (cap, sap) = self.ks.derive_application(&th_sf);

        // Client Finished = HMAC(client_hs.finished_key, transcript hash) (incl.
        // server Finished). Send encrypted in the handshake epoch.
        let client_hs = self.client_hs.as_ref().ok_or(TlsError::Protocol("no client hs"))?;
        let verify = hmac_sha256(&client_hs.finished_key, &th_sf);
        let fin_msg = wrap_handshake(HS_FINISHED, &verify);

        // Middlebox compat: a ChangeCipherSpec record beforehand.
        out.extend_from_slice(&build_record(CT_CHANGE_CIPHER_SPEC, &[0x01]));
        // Encrypt the Finished with the client handshake keys (seq 0).
        let rec = self.seal_record(CT_HANDSHAKE, &fin_msg, &client_hs.key, &client_hs.iv, self.client_seq);
        self.client_seq += 1;
        out.extend_from_slice(&rec);

        // Switch both sides to the application epoch.
        self.client_ap = Some(cap);
        self.server_ap = Some(sap);
        self.client_epoch = Epoch::Application;
        self.client_seq = 0;
        self.server_epoch = Epoch::Application;
        self.server_seq = 0;
        self.state = TlsState::Connected;
        Ok(())
    }

    fn seal_record(&self, inner_type: u8, content: &[u8], key: &[u8; 32], iv: &[u8; 12], seq: u64) -> Vec<u8> {
        let mut pt = Vec::with_capacity(content.len() + 1);
        pt.extend_from_slice(content);
        pt.push(inner_type); // inner content type
        let total = pt.len() + 16; // + AEAD tag
        let aad = aead_aad(total);
        let ct = aead::seal(key, iv, seq, &aad, &pt);
        build_record(CT_APPLICATION_DATA, &ct)
    }

    // ── Application data ──────────────────────────────────────────────────
    /// Encrypt application data into one record to send.
    pub fn encrypt_app(&mut self, data: &[u8]) -> Result<Vec<u8>, TlsError> {
        let keys = self.client_ap.as_ref().ok_or(TlsError::Protocol("not connected"))?;
        let rec = self.seal_record(CT_APPLICATION_DATA, data, &keys.key, &keys.iv, self.client_seq);
        self.client_seq += 1;
        Ok(rec)
    }

    /// Fetch the application data decrypted so far (and empty the buffer).
    pub fn take_app_data(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.app_buf)
    }
}

// ── Helper functions ─────────────────────────────────────────────────────
fn wrap_handshake(mtype: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + body.len());
    m.push(mtype);
    let l = body.len();
    m.push((l >> 16) as u8);
    m.push((l >> 8) as u8);
    m.push(l as u8);
    m.extend_from_slice(body);
    m
}

fn push_ext(ext: &mut Vec<u8>, etype: u16, data: &[u8]) {
    ext.extend_from_slice(&etype.to_be_bytes());
    ext.extend_from_slice(&(data.len() as u16).to_be_bytes());
    ext.extend_from_slice(data);
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac key");
    m.update(msg);
    let tag = m.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&tag);
    out
}

/// Strip the AEAD padding (trailing zero bytes) and extract the inner content
/// type (the last non-zero byte) (RFC 8446 §5.4).
fn split_inner(pt: &[u8]) -> Result<(u8, &[u8]), TlsError> {
    let mut end = pt.len();
    while end > 0 && pt[end - 1] == 0 {
        end -= 1;
    }
    if end == 0 {
        return Err(TlsError::BadRecord);
    }
    Ok((pt[end - 1], &pt[..end - 1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_hello_is_wellformed() {
        let (_c, rec) = Tls13Client::new("euro-os.eu", [0xAB; 32], [0x11; 32]);
        // record: type=22 handshake, version 0303
        assert_eq!(rec[0], CT_HANDSHAKE);
        assert_eq!(&rec[1..3], &[0x03, 0x03]);
        let rlen = u16::from_be_bytes([rec[3], rec[4]]) as usize;
        assert_eq!(rec.len(), 5 + rlen);
        // handshake msg type = client_hello (1)
        assert_eq!(rec[5], HS_CLIENT_HELLO);
        // SNI host must appear somewhere in the ClientHello
        assert!(rec.windows(10).any(|w| w == b"euro-os.eu"));
        // cipher suite chacha20 (0x1303) present
        assert!(rec.windows(2).any(|w| w == [0x13, 0x03]));
    }

    #[test]
    fn split_inner_strips_padding() {
        let (t, c) = split_inner(&[1, 2, 3, 22, 0, 0, 0]).unwrap();
        assert_eq!(t, 22);
        assert_eq!(c, &[1, 2, 3]);
        assert!(split_inner(&[0, 0, 0]).is_err());
    }

    #[test]
    fn alert_record_surfaces() {
        let (mut c, _) = Tls13Client::new("x", [0; 32], [1; 32]);
        // An alert record (level=fatal=2, desc=handshake_failure=40).
        c.feed(&build_record(CT_ALERT, &[2, 40]));
        assert_eq!(c.process(), Err(TlsError::Alert(40)));
    }

    #[test]
    fn validatie_is_fail_closed_zonder_trust_anchor() {
        // Audit #8: without a trust anchor certificate validation MUST fail (fail-closed),
        // not silently accept every certificate.
        let (mut c, _) = Tls13Client::new("x", [0; 32], [1; 32]);
        assert!(c.validate_chain().is_err(), "no anchor → must fail");
        assert!(c.verify_certificate_verify(&[], &[]).is_err(), "no anchor → CertVerify must fail");
        // Explicit, greppable opt-out reopens it (host test/dev only).
        c.allow_insecure_no_verification();
        assert!(c.validate_chain().is_ok(), "opt-out → allowed");
    }
}
