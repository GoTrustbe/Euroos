//! EuroTLS — een eigen TLS 1.3-client (RFC 8446) voor EuroOS.
//!
//! Sans-IO: deze crate kent geen sockets. De caller (de kernel, bovenop een
//! `TcpConn`) voedt ontvangen records in en krijgt te-versturen bytes terug.
//! Zo is de hele protocol- en sleutelschema-logica — de fout-gevoeligste laag —
//! op de host testbaar, zonder NIC of QEMU.
//!
//! Ciphersuite: **TLS_CHACHA20_POLY1305_SHA256** (één suite; software-vriendelijk,
//! geen AES-NI nodig, breed ondersteund). Sleuteluitwisseling: **X25519**.
//! Handtekening-verificatie van het servercertificaat is een latere fase
//! (EuroGuard cert-inspectie, 7.8) — de handshake zelf is volledig echt.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod aead;
pub mod chain;
pub mod handshake;
pub mod keyschedule;
pub mod record;
pub mod sig;
pub mod x509;

pub use handshake::{Tls13Client, TlsError, TlsState};

/// De onderhandelde ciphersuite-code (TLS_CHACHA20_POLY1305_SHA256).
pub const TLS_CHACHA20_POLY1305_SHA256: u16 = 0x1303;
/// De named group voor sleuteluitwisseling (x25519).
pub const GROUP_X25519: u16 = 0x001d;
/// Het handtekening-algoritme dat we adverteren (ed25519).
pub const SIG_ED25519: u16 = 0x0807;
pub const SIG_ECDSA_P256: u16 = 0x0403;
pub const SIG_RSA_PSS_SHA256: u16 = 0x0804;
pub const SIG_RSA_PKCS1_SHA256: u16 = 0x0401;
