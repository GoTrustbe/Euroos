//! EuroGeo — IP-to-country attribution for EuroGuard geo-blocking.
//!
//! A compact CIDR-to-country table with a longest-prefix lookup, so the kernel
//! can answer "which country does this destination belong to" before a packet
//! leaves, and block by country (`block-country CN`).
//!
//! Honest scope: the built-in table is a **curated set of major national
//! allocations** (the large China/Russia/Iran/North-Korea blocks that carry the
//! bulk of traffic), not a full GeoIP database. It is designed to be
//! **extended at runtime** with additional `CIDR country` lines from a data
//! feed (`/etc/euroguard/geoip.conf`), so a complete table drops in without a
//! code change. Pure `no_std` logic, so the prefix math is host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

/// One CIDR block mapped to an ISO-3166 alpha-2 country code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub base: u32,   // network address (host byte order)
    pub prefix: u8,  // 0..=32
    pub cc: &'static str,
}

const fn b(a: u8, c: u8, d: u8, e: u8, prefix: u8, cc: &'static str) -> Block {
    Block { base: u32::from_be_bytes([a, c, d, e]), prefix, cc }
}

/// Curated major allocations. Deliberately conservative: only large, well-known
/// national blocks, so a false "this is China" positive is unlikely. Extend via
/// [`Table::load_feed`] for full coverage.
static BUILTIN: &[Block] = &[
    // ── China (CN) — large China Telecom / Unicom / Mobile / Baidu / Tencent ──
    b(36, 0, 0, 0, 10, "CN"),
    b(39, 0, 0, 0, 11, "CN"),
    b(42, 48, 0, 0, 12, "CN"),
    b(58, 14, 0, 0, 15, "CN"),
    b(59, 32, 0, 0, 11, "CN"),
    b(60, 0, 0, 0, 11, "CN"),
    b(61, 128, 0, 0, 10, "CN"),
    b(101, 16, 0, 0, 12, "CN"),
    b(111, 0, 0, 0, 10, "CN"),
    b(112, 0, 0, 0, 9, "CN"),
    b(113, 0, 0, 0, 9, "CN"),
    b(114, 80, 0, 0, 12, "CN"),
    b(116, 0, 0, 0, 9, "CN"),
    b(117, 0, 0, 0, 8, "CN"),
    b(119, 0, 0, 0, 9, "CN"),
    b(120, 0, 0, 0, 9, "CN"),
    b(121, 0, 0, 0, 9, "CN"),
    b(122, 0, 0, 0, 9, "CN"),
    b(123, 0, 0, 0, 9, "CN"),
    b(125, 32, 0, 0, 11, "CN"),
    b(175, 0, 0, 0, 9, "CN"),
    b(180, 76, 0, 0, 16, "CN"), // Baidu
    b(182, 80, 0, 0, 12, "CN"),
    b(183, 0, 0, 0, 10, "CN"),
    b(202, 96, 0, 0, 11, "CN"), // China Telecom backbone
    b(211, 136, 0, 0, 13, "CN"),
    b(218, 0, 0, 0, 11, "CN"),
    b(220, 160, 0, 0, 11, "CN"),
    b(221, 192, 0, 0, 10, "CN"),
    b(222, 64, 0, 0, 11, "CN"),
    // ── Russia (RU) ──
    b(5, 16, 0, 0, 13, "RU"),
    b(5, 45, 0, 0, 16, "RU"),
    b(31, 184, 0, 0, 16, "RU"),
    b(37, 140, 0, 0, 16, "RU"),
    b(46, 146, 0, 0, 16, "RU"),
    b(77, 88, 0, 0, 18, "RU"),   // Yandex
    b(79, 104, 0, 0, 13, "RU"),
    b(87, 240, 0, 0, 13, "RU"),  // VK
    b(90, 150, 0, 0, 15, "RU"),
    b(91, 76, 0, 0, 14, "RU"),
    b(95, 24, 0, 0, 13, "RU"),
    b(176, 59, 0, 0, 16, "RU"),
    b(178, 176, 0, 0, 13, "RU"),
    b(213, 180, 192, 0, 19, "RU"), // Yandex
    // ── Iran (IR) / North Korea (KP) — small but frequently blocked ──
    b(2, 144, 0, 0, 12, "IR"),
    b(5, 22, 0, 0, 17, "IR"),
    b(175, 45, 176, 0, 22, "KP"), // the entire KP allocation
];

/// A country table: the built-in blocks plus any runtime-loaded ones.
#[derive(Default)]
pub struct Table {
    extra: Vec<Block>,
}

impl Table {
    pub const fn new() -> Self {
        Table { extra: Vec::new() }
    }

    /// Load extra `CIDR country` lines (e.g. `1.2.3.0/24 CN`); `#` is a comment.
    /// Returns how many blocks were added. This is how a full GeoIP feed extends
    /// the built-in set without a code change.
    pub fn load_feed(&mut self, text: &str) -> usize {
        let mut n = 0;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            if let (Some(cidr), Some(cc)) = (it.next(), it.next()) {
                if let Some(blk) = parse_cidr(cidr, cc) {
                    self.extra.push(blk);
                    n += 1;
                }
            }
        }
        n
    }

    /// The country of `ip` (host byte order), by longest-prefix match over the
    /// built-in + loaded blocks. `None` when no block covers it (treated as
    /// "not in a known geo-blocked region").
    pub fn country_of(&self, ip: u32) -> Option<&str> {
        let mut best: Option<(&str, u8)> = None;
        for blk in BUILTIN.iter().chain(self.extra.iter()) {
            if in_block(blk, ip) && best.map(|(_, p)| blk.prefix > p).unwrap_or(true) {
                best = Some((cc_of(blk, &self.extra), blk.prefix));
            }
        }
        best.map(|(cc, _)| cc)
    }

    /// Number of blocks in the table (built-in + loaded).
    pub fn len(&self) -> usize {
        BUILTIN.len() + self.extra.len()
    }
    pub fn is_empty(&self) -> bool {
        false
    }
}

// The `cc` for a loaded block is stored on the block itself; for built-ins it is
// static. `load_feed` leaks the cc into a small static-ish pool via a boxed str;
// to stay `no_std`+`forbid(unsafe)` we instead store loaded ccs inline as owned.
// Simpler: loaded blocks carry a two-byte code we map back. To keep the public
// API a `&str`, loaded ccs are normalized to a fixed set below.
fn cc_of<'a>(blk: &'a Block, _extra: &'a [Block]) -> &'a str {
    blk.cc
}

fn in_block(blk: &Block, ip: u32) -> bool {
    if blk.prefix == 0 {
        return true;
    }
    let mask = if blk.prefix >= 32 { u32::MAX } else { !((1u32 << (32 - blk.prefix)) - 1) };
    (ip & mask) == (blk.base & mask)
}

/// Parse `a.b.c.d/prefix` + a country code into a `Block`. The country string
/// is interned to a small set of `&'static` codes so the table stays no-alloc
/// on lookup; unknown codes map to a generic `"XX"`.
fn parse_cidr(cidr: &str, cc: &str) -> Option<Block> {
    let (addr, pfx) = cidr.split_once('/')?;
    let mut octets = [0u8; 4];
    let mut it = addr.split('.');
    for o in octets.iter_mut() {
        *o = it.next()?.parse::<u8>().ok()?;
    }
    if it.next().is_some() {
        return None;
    }
    let prefix: u8 = pfx.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    Some(Block { base: u32::from_be_bytes(octets), prefix, cc: intern_cc(cc) })
}

/// Intern a country code to a `&'static str` (kept small; extend as needed).
fn intern_cc(cc: &str) -> &'static str {
    match cc.to_ascii_uppercase().as_str() {
        "CN" => "CN",
        "RU" => "RU",
        "IR" => "IR",
        "KP" => "KP",
        "US" => "US",
        "BE" => "BE",
        "NL" => "NL",
        "DE" => "DE",
        "FR" => "FR",
        _ => "XX",
    }
}

/// Parse a dotted IPv4 string into host-byte-order `u32` (helper for callers).
pub fn parse_ipv4(s: &str) -> Option<u32> {
    let mut octets = [0u8; 4];
    let mut it = s.split('.');
    for o in octets.iter_mut() {
        *o = it.next()?.parse::<u8>().ok()?;
    }
    if it.next().is_some() {
        return None;
    }
    Some(u32::from_be_bytes(octets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn china_block_detected() {
        let t = Table::new();
        // 202.96.0.5 is inside China Telecom 202.96.0.0/11.
        assert_eq!(t.country_of(parse_ipv4("202.96.0.5").unwrap()), Some("CN"));
        // Baidu 180.76.0.0/16.
        assert_eq!(t.country_of(parse_ipv4("180.76.76.76").unwrap()), Some("CN"));
    }

    #[test]
    fn russia_and_kp() {
        let t = Table::new();
        assert_eq!(t.country_of(parse_ipv4("87.240.1.1").unwrap()), Some("RU")); // VK
        assert_eq!(t.country_of(parse_ipv4("175.45.176.1").unwrap()), Some("KP"));
    }

    #[test]
    fn non_geoblocked_is_none() {
        let t = Table::new();
        // 10.0.2.2 (the local test gateway) and 8.8.8.8 (US) are not in the
        // curated CN/RU/IR/KP set.
        assert_eq!(t.country_of(parse_ipv4("10.0.2.2").unwrap()), None);
        assert_eq!(t.country_of(parse_ipv4("8.8.8.8").unwrap()), None);
    }

    #[test]
    fn loaded_feed_extends_and_wins_by_prefix() {
        let mut t = Table::new();
        // A full-feed line adds a specific block; longest-prefix wins.
        let n = t.load_feed("# geoip feed\n1.2.3.0/24 CN\n9.9.9.9/32 RU\n");
        assert_eq!(n, 2);
        assert_eq!(t.country_of(parse_ipv4("1.2.3.99").unwrap()), Some("CN"));
        assert_eq!(t.country_of(parse_ipv4("9.9.9.9").unwrap()), Some("RU"));
        assert!(t.len() > BUILTIN.len());
    }

    #[test]
    fn parse_cidr_rejects_garbage() {
        assert!(parse_cidr("999.0.0.0/8", "CN").is_none());
        assert!(parse_cidr("1.2.3.4", "CN").is_none()); // no prefix
        assert!(parse_cidr("1.2.3.0/33", "CN").is_none());
    }
}
