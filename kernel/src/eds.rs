//! EuroDesign System (EDS) — design tokens (Track 5 design system).
//!
//! Fixed tokens from the EDS spec: the resolution-independent `eu` unit, the
//! radius system, and the **security color language** (security is a first-class
//! semantic color). Components use these tokens — never arbitrary values.

use crate::graphics::Color;

/// Euro Unit — base scale unit (px at 100% DPI). Grid base = 4.
pub const EU: usize = 4;
pub const fn eu(n: usize) -> usize {
    n * EU
}

// Radius system (no other values allowed).
pub const RADIUS_S: usize = 8;
pub const RADIUS_M: usize = 12;
pub const RADIUS_L: usize = 20;
#[allow(dead_code)]
pub const RADIUS_XL: usize = 28;

// ── Security color language (EDS) ──────────────────────────────────────────────
// Green = Verified, Blue = Protected, Yellow = Attention, Red = Compromised, Gray = Unknown.
pub const SEC_VERIFIED: Color = Color::SUCCESS;
pub const SEC_PROTECTED: Color = Color::ACCENT;
pub const SEC_ATTENTION: Color = Color::YELLOW;
#[allow(dead_code)]
pub const SEC_COMPROMISED: Color = Color::RED;
#[allow(dead_code)]
pub const SEC_UNKNOWN: Color = Color::TEXT_DIM;

/// Security status of a window/app (visible in the title bar — "never hide
/// security": always show encryption/sandbox/network).
#[derive(Clone, Copy)]
pub struct SecState {
    pub sandboxed: bool,
    pub encrypted: bool,
    pub network: bool,
}

impl SecState {
    pub const fn new(sandboxed: bool, encrypted: bool, network: bool) -> Self {
        Self {
            sandboxed,
            encrypted,
            network,
        }
    }
}
