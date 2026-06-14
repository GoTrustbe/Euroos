//! Boot self-test for **EuroShot** (AC-1): screenshot region + signed manifest.
//! Core: [`euroshot`].

use crate::serial_println;
use euroshot::{capture_region, fnv1a_64, Region, ShotManifest, Target};

pub fn selftest() {
    // Full screen + clamped selection.
    let full = capture_region(Target::FullScreen, Region::default(), 1920, 1080) == Region::new(0, 0, 1920, 1080);
    let clamped = Region::new(1900, 1000, 200, 200).clamp_to(1920, 1080) == Region::new(1900, 1000, 20, 80);

    // Manifest: equal pixels → equal hash; changed pixels → different payload.
    let m = ShotManifest::from_pixels(1920, 1080, 1_700_000_000, b"pixels", false);
    let tamper = ShotManifest::from_pixels(1920, 1080, 1_700_000_000, b"PIXELS", false);
    let hash_detects = m.content_hash != tamper.content_hash && m.canonical_bytes() != tamper.canonical_bytes();
    let payload_ok = alloc::string::String::from_utf8(m.canonical_bytes())
        .map(|s| s.starts_with("EuroShot-v1\n") && s.contains("size=1920x1080\n"))
        .unwrap_or(false);
    let fnv_ok = fnv1a_64(b"") == 0xcbf2_9ce4_8422_2325;

    let ok = full && clamped && hash_detects && payload_ok && fnv_ok;
    serial_println!(
        "[st] EuroShot: full-screen={}, region-clamp={}, manifest-detects-change={}, Ed25519-payload={}, FNV={} {}",
        full, clamped, hash_detects, payload_ok, fnv_ok,
        if ok { "✓" } else { "✗ ERROR" }
    );
}
