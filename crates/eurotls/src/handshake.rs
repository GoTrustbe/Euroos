//! TLS 1.3-client state machine (RFC 8446 §4). Sans-IO: voed records in via
//! [`Tls13Client::feed`], verwerk via [`Tls13Client::process`] (geeft te
//! versturen bytes terug), en wissel daarna applicatiedata uit. Ciphersuite
//! TLS_CHACHA20_POLY1305_SHA256, sleuteluitwisseling X25519.

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

// Handshake-berichttypes.
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

    rx: Vec<u8>,      // ruwe ontvangen record-bytes
    hs_buf: Vec<u8>,  // ontsleutelde handshake-bytes (her-assemblage)
    app_buf: Vec<u8>, // ontsleutelde applicatiedata

    /// Het ruwe servercertificaat (eerste in de keten) voor latere inspectie.
    pub server_cert: Option<Vec<u8>>,
    /// De volledige door de server aangeboden keten (leaf eerst), DER per cert.
    cert_chain: Vec<Vec<u8>>,
    /// Peilmoment (epoch-seconden) voor geldigheidscontrole; gezet via
    /// [`set_trust_anchor`].
    now: i64,
    /// Vertrouwde root-CA's (DER). `None` = nog geen trust anchor gezet → de
    /// handshake FAALT bij certificaatcontrole (fail-closed), tenzij expliciet
    /// `allow_insecure_no_verification()` is aangeroepen.
    trust_roots: Option<&'static [&'static [u8]]>,
    /// Expliciete, greppable opt-out: sla certificaatvalidatie over (alleen voor
    /// host-tests/dev zónder trust store). Standaard `false` — nooit stilzwijgend.
    insecure_skip_verify: bool,
}

impl Tls13Client {
    /// Maak een client + de ClientHello-record (klaar om in klare tekst te
    /// versturen). `random` en `secret_scalar` MOETEN echte willekeur zijn
    /// (de kernel levert ze via RDRAND).
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

    /// Schakel certificaatvalidatie in: na ontvangst van het servercertificaat
    /// wordt de keten aan `roots` verankerd (geldigheid + hostnaam + per-stap-
    /// handtekening) én wordt de CertificateVerify-handtekening gecontroleerd.
    /// `now` = epoch-seconden (van de kernel-RTC). MOET vóór `process()` worden
    /// aangeroepen. Wordt dit nooit aangeroepen, dan blijft validatie uit (de
    /// handshake-MAC blijft hoe dan ook bindend).
    pub fn set_trust_anchor(&mut self, now: i64, roots: &'static [&'static [u8]]) {
        self.now = now;
        self.trust_roots = Some(roots);
    }

    /// EXPLICIETE opt-out: sla certificaat- en CertificateVerify-validatie over.
    /// Uitsluitend voor host-tests/dev zónder trust store — NOOIT in productie.
    /// De naam is bewust luid en greppable zodat een audit elk onveilig gebruik vindt.
    pub fn allow_insecure_no_verification(&mut self) {
        self.insecure_skip_verify = true;
    }

    /// Voeg ontvangen bytes toe aan de interne record-buffer.
    pub fn feed(&mut self, data: &[u8]) {
        self.rx.extend_from_slice(data);
    }

    // ── ClientHello ──────────────────────────────────────────────────────
    fn build_client_hello(&self, random: &[u8; 32], key_share: &[u8; 32]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&[0x03, 0x03]); // legacy_version
        b.extend_from_slice(random);
        // legacy_session_id: 32 bytes (middlebox-compat), hergebruik wat willekeur.
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

    // ── Verwerking ───────────────────────────────────────────────────────
    /// Verwerk alle volledige records in de buffer; geef te versturen bytes
    /// terug (kan leeg zijn). Zet de staat door tot `Connected`.
    pub fn process(&mut self) -> Result<Vec<u8>, TlsError> {
        let mut out = Vec::new();
        loop {
            let (rec, n) = match read_record(&self.rx) {
                Ok(Some(v)) => v,
                Ok(None) => break, // nog niet genoeg bytes
                Err(_) => return Err(TlsError::Protocol("recordlengte > 2^14+256")),
            };
            self.rx.drain(..n);
            match rec.ctype {
                CT_CHANGE_CIPHER_SPEC => continue, // middlebox-compat: negeren
                CT_ALERT => {
                    self.state = TlsState::Failed;
                    let code = *rec.fragment.get(1).unwrap_or(&0);
                    return Err(TlsError::Alert(code));
                }
                CT_HANDSHAKE => {
                    // Alleen vóór de versleutelde flight: dit is de ServerHello.
                    self.hs_buf.extend_from_slice(&rec.fragment);
                    self.drain_handshake(&mut out)?;
                }
                CT_APPLICATION_DATA => {
                    // Versleuteld record: ontsleutel met de huidige server-epoch.
                    let keys = match self.server_epoch {
                        Epoch::Handshake => self.server_hs.as_ref(),
                        Epoch::Application => self.server_ap.as_ref(),
                        Epoch::None => return Err(TlsError::Protocol("data vóór sleutels")),
                    }
                    .ok_or(TlsError::Protocol("geen serversleutels"))?;
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

    /// Parse complete handshake-berichten uit `hs_buf` en verwerk ze.
    fn drain_handshake(&mut self, out: &mut Vec<u8>) -> Result<(), TlsError> {
        loop {
            if self.hs_buf.len() < 4 {
                return Ok(());
            }
            let mtype = self.hs_buf[0];
            let len = ((self.hs_buf[1] as usize) << 16) | ((self.hs_buf[2] as usize) << 8) | (self.hs_buf[3] as usize);
            if self.hs_buf.len() < 4 + len {
                return Ok(()); // wacht op meer
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
                // Verifieer de keten tegen de trust store (indien ingeschakeld).
                self.validate_chain()?;
            }
            HS_CERTIFICATE_VERIFY => {
                // De server bewijst hier bezit van de privésleutel van het leaf-
                // certificaat door de transcript-hash (T/M Certificate) te
                // ondertekenen. Verifieer vóór we dit bericht aan de transcript
                // toevoegen (RFC 8446 §4.4.3).
                let th = self.transcript.hash();
                self.verify_certificate_verify(&msg[4..], &th)?;
                self.transcript.update(msg);
            }
            HS_FINISHED => {
                // Server Finished: verifieer de MAC over de transcript T/M
                // CertificateVerify (dus vóór we dit bericht toevoegen).
                let server_hs = self.server_hs.as_ref().ok_or(TlsError::Protocol("geen hs-sleutels"))?;
                let expected = hmac_sha256(&server_hs.finished_key, &self.transcript.hash());
                if msg[4..] != expected[..] {
                    self.state = TlsState::Failed;
                    return Err(TlsError::BadFinished);
                }
                self.transcript.update(msg);
                self.finish_handshake(out)?;
            }
            HS_NEW_SESSION_TICKET => { /* na de handshake: negeren (geen resumption) */ }
            _ => return Err(TlsError::Protocol("onverwacht handshake-bericht")),
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
        // Zoek de key_share-extensie (0x0033) → server x25519 public key.
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
        let server_pub = server_pub.ok_or(TlsError::Protocol("geen server key_share"))?;

        // ECDHE + sleutelschema (handshake-secrets over transcript CH||SH).
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
        // certificate_request_context<u8> + certificate_list<u24>. Elke entry =
        // cert_data<u24> (DER) + extensions<u16>. We bewaren de hele keten.
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
            // CertificateEntry-extensies overslaan.
            if p + 2 > list_end {
                break;
            }
            let ext_len = u16::from_be_bytes([body[p], body[p + 1]]) as usize;
            p += 2 + ext_len;
        }
        self.server_cert = self.cert_chain.first().cloned();
    }

    /// Diagnostiek: (sig_alg, pubkey_alg, is_ca) per certificaat in de keten.
    pub fn cert_chain_info(&self) -> Vec<(crate::x509::SigAlg, PubKeyAlg, bool)> {
        self.cert_chain
            .iter()
            .filter_map(|d| Certificate::parse(d).ok().map(|c| (c.sig_alg, c.pubkey_alg, c.is_ca)))
            .collect()
    }

    /// Verifieer de aangeboden keten tegen de trust store. FAIL-CLOSED: zonder
    /// trust anchor faalt de verbinding (tenzij expliciet `allow_insecure_no_
    /// verification()` is gezet). Bij mislukking: faal de verbinding.
    fn validate_chain(&mut self) -> Result<(), TlsError> {
        let roots = match self.trust_roots {
            Some(r) => r,
            None => {
                if self.insecure_skip_verify {
                    return Ok(()); // expliciete opt-out (host-test/dev)
                }
                self.state = TlsState::Failed;
                return Err(TlsError::Protocol("geen trust anchor gezet — validatie verplicht"));
            }
        };
        if self.cert_chain.is_empty() {
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("server stuurde geen certificaat"));
        }
        let ts = chain::TrustStore::from_ders(roots);
        let slices: Vec<&[u8]> = self.cert_chain.iter().map(|v| v.as_slice()).collect();
        match chain::validate(&slices, &self.sni, self.now, &ts) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.state = TlsState::Failed;
                let why = match e {
                    chain::ChainError::EmptyChain => "keten leeg",
                    chain::ChainError::Parse(_) => "cert onleesbaar",
                    chain::ChainError::Expired => "verlopen / niet geldig",
                    chain::ChainError::HostnameMismatch => "hostnaam matcht geen SAN",
                    chain::ChainError::BrokenChain => "keten onderbroken (issuer≠subject)",
                    chain::ChainError::IssuerNotCa => "uitgever is geen CA",
                    chain::ChainError::BadSignature => "handtekening in keten ongeldig",
                    chain::ChainError::UnknownCa => "geen vertrouwde root (onbekende CA)",
                };
                Err(TlsError::Protocol(why))
            }
        }
    }

    /// Controleer de CertificateVerify-handtekening met de publieke sleutel van
    /// het leaf-certificaat (geen-op als validatie uitstaat). `transcript_hash` =
    /// de transcript-hash T/M het Certificate-bericht.
    fn verify_certificate_verify(&mut self, body: &[u8], transcript_hash: &[u8]) -> Result<(), TlsError> {
        if self.trust_roots.is_none() {
            if self.insecure_skip_verify {
                return Ok(()); // expliciete opt-out (host-test/dev)
            }
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("geen trust anchor gezet — CertificateVerify verplicht"));
        }
        if body.len() < 4 {
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("korte CertificateVerify"));
        }
        let scheme = u16::from_be_bytes([body[0], body[1]]);
        let sig_len = u16::from_be_bytes([body[2], body[3]]) as usize;
        if body.len() < 4 + sig_len {
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("CertificateVerify-lengte"));
        }
        let signature = &body[4..4 + sig_len];

        // Het ondertekende blok (RFC 8446 §4.4.3): 64×0x20, contextstring, 0x00,
        // gevolgd door de transcript-hash.
        let mut content = Vec::with_capacity(64 + 34 + transcript_hash.len());
        content.extend_from_slice(&[0x20u8; 64]);
        content.extend_from_slice(b"TLS 1.3, server CertificateVerify");
        content.push(0x00);
        content.extend_from_slice(transcript_hash);

        let leaf_der = self.cert_chain.first().ok_or(TlsError::Protocol("geen leaf-certificaat"))?;
        let leaf = Certificate::parse(leaf_der).map_err(|_| TlsError::Protocol("leaf-cert onleesbaar"))?;

        // SignatureScheme → primitive; het leaf-sleuteltype moet passen.
        let ok = match scheme {
            0x0403 => leaf.pubkey_alg == PubKeyAlg::EcP256 && sig::verify_ecdsa_p256(leaf.pubkey, &content, signature),
            0x0807 => leaf.pubkey_alg == PubKeyAlg::Ed25519 && sig::verify_ed25519(leaf.pubkey, &content, signature),
            0x0804 => leaf.pubkey_alg == PubKeyAlg::Rsa && sig::verify_rsa_pss_sha256(leaf.pubkey, &content, signature),
            _ => false,
        };
        if !ok {
            self.state = TlsState::Failed;
            return Err(TlsError::Protocol("CertificateVerify-handtekening ongeldig"));
        }
        Ok(())
    }

    /// Server Finished verifieerd → leid app-sleutels af, stuur (CCS +) onze
    /// versleutelde Finished, schakel naar de applicatie-epoch.
    fn finish_handshake(&mut self, out: &mut Vec<u8>) -> Result<(), TlsError> {
        // App-sleutels over transcript CH..server Finished.
        let th_sf = self.transcript.hash();
        let (cap, sap) = self.ks.derive_application(&th_sf);

        // Client Finished = HMAC(client_hs.finished_key, transcript-hash) (incl.
        // server Finished). Verstuur versleuteld in de handshake-epoch.
        let client_hs = self.client_hs.as_ref().ok_or(TlsError::Protocol("geen client hs"))?;
        let verify = hmac_sha256(&client_hs.finished_key, &th_sf);
        let fin_msg = wrap_handshake(HS_FINISHED, &verify);

        // Middlebox-compat: een ChangeCipherSpec-record vooraf.
        out.extend_from_slice(&build_record(CT_CHANGE_CIPHER_SPEC, &[0x01]));
        // Versleutel de Finished met de client-handshake-sleutels (seq 0).
        let rec = self.seal_record(CT_HANDSHAKE, &fin_msg, &client_hs.key, &client_hs.iv, self.client_seq);
        self.client_seq += 1;
        out.extend_from_slice(&rec);

        // Schakel beide kanten naar de applicatie-epoch.
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
        let total = pt.len() + 16; // + AEAD-tag
        let aad = aead_aad(total);
        let ct = aead::seal(key, iv, seq, &aad, &pt);
        build_record(CT_APPLICATION_DATA, &ct)
    }

    // ── Applicatiedata ──────────────────────────────────────────────────
    /// Versleutel applicatiedata tot één te versturen record.
    pub fn encrypt_app(&mut self, data: &[u8]) -> Result<Vec<u8>, TlsError> {
        let keys = self.client_ap.as_ref().ok_or(TlsError::Protocol("niet verbonden"))?;
        let rec = self.seal_record(CT_APPLICATION_DATA, data, &keys.key, &keys.iv, self.client_seq);
        self.client_seq += 1;
        Ok(rec)
    }

    /// Haal de tot nu toe ontsleutelde applicatiedata op (en leeg de buffer).
    pub fn take_app_data(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.app_buf)
    }
}

// ── Hulpfuncties ─────────────────────────────────────────────────────────
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

/// Strip de AEAD-padding (trailing nul-bytes) en haal de inner content-type
/// (de laatste niet-nul byte) eruf (RFC 8446 §5.4).
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
        // SNI-host moet ergens in de ClientHello zitten
        assert!(rec.windows(10).any(|w| w == b"euro-os.eu"));
        // cipher suite chacha20 (0x1303) aanwezig
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
        // Een alert-record (level=fatal=2, desc=handshake_failure=40).
        c.feed(&build_record(CT_ALERT, &[2, 40]));
        assert_eq!(c.process(), Err(TlsError::Alert(40)));
    }

    #[test]
    fn validatie_is_fail_closed_zonder_trust_anchor() {
        // Audit #8: zonder trust anchor MOET certificaatvalidatie falen (fail-closed),
        // niet stilzwijgend elk certificaat aanvaarden.
        let (mut c, _) = Tls13Client::new("x", [0; 32], [1; 32]);
        assert!(c.validate_chain().is_err(), "geen anchor → moet falen");
        assert!(c.verify_certificate_verify(&[], &[]).is_err(), "geen anchor → CertVerify moet falen");
        // Expliciete, greppable opt-out heropent het (alleen host-test/dev).
        c.allow_insecure_no_verification();
        assert!(c.validate_chain().is_ok(), "opt-out → toegestaan");
    }
}
