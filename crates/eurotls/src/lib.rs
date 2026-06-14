//! EuroTLS — a custom TLS 1.3 client (RFC 8446) for EuroOS.
//!
//! Sans-IO: this crate knows no sockets. The caller (the kernel, on top of a
//! `TcpConn`) feeds in received records and gets bytes to send back.
//! This way the entire protocol and key-schedule logic — the most error-prone layer —
//! is testable on the host, without a NIC or QEMU.
//!
//! Ciphersuite: **TLS_CHACHA20_POLY1305_SHA256** (one suite; software-friendly,
//! no AES-NI needed, widely supported). Key exchange: **X25519**.
//! Signature verification of the server certificate is a later phase
//! (EuroGuard cert inspection, 7.8) — the handshake itself is fully real.
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

/// The negotiated ciphersuite code (TLS_CHACHA20_POLY1305_SHA256).
pub const TLS_CHACHA20_POLY1305_SHA256: u16 = 0x1303;
/// The named group for key exchange (x25519).
pub const GROUP_X25519: u16 = 0x001d;
/// The signature algorithm we advertise (ed25519).
pub const SIG_ED25519: u16 = 0x0807;
pub const SIG_ECDSA_P256: u16 = 0x0403;
pub const SIG_RSA_PSS_SHA256: u16 = 0x0804;
pub const SIG_RSA_PKCS1_SHA256: u16 = 0x0401;
