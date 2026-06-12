//! EuroDesign System (EDS) — design-tokens (Track 5 design-system).
//!
//! Vaste tokens uit de EDS-spec: de resolutie-onafhankelijke `eu`-eenheid, het
//! radius-systeem, en de **security-kleurtaal** (security is een eersteklas
//! semantische kleur). Componenten gebruiken deze tokens — nooit willekeurige waarden.

use crate::graphics::Color;

/// Euro Unit — basis-schaaleenheid (px bij 100% DPI). Grid-basis = 4.
pub const EU: usize = 4;
pub const fn eu(n: usize) -> usize {
    n * EU
}

// Radius-systeem (geen andere waarden toegestaan).
pub const RADIUS_S: usize = 8;
pub const RADIUS_M: usize = 12;
pub const RADIUS_L: usize = 20;
#[allow(dead_code)]
pub const RADIUS_XL: usize = 28;

// ── Security-kleurtaal (EDS) ──────────────────────────────────────────────
// Groen = Geverifieerd, Blauw = Beschermd, Geel = Aandacht, Rood = Gecompromitteerd, Grijs = Onbekend.
pub const SEC_VERIFIED: Color = Color::SUCCESS;
pub const SEC_PROTECTED: Color = Color::ACCENT;
pub const SEC_ATTENTION: Color = Color::YELLOW;
#[allow(dead_code)]
pub const SEC_COMPROMISED: Color = Color::RED;
#[allow(dead_code)]
pub const SEC_UNKNOWN: Color = Color::TEXT_DIM;

/// Security-status van een venster/app (zichtbaar in de titelbalk — "verberg
/// nooit security": versleuteling/sandbox/netwerk altijd tonen).
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
