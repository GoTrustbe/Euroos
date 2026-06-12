//! Capability-afleiding (Sprint AA, stap 2) — de *enige* plek waar bepaald wordt
//! wat een agent uiteindelijk mag. Pure logica, host-getest.
//!
//! De effectieve set is altijd een **subset**:
//! ```text
//! effective = (required ∪ granted_optional) ∩ user_caps  −  policy_denied
//! ```
//! Drie onafhankelijke grenzen, in volgorde:
//! 1. de agent krijgt nooit meer dan hij declareert (`required`/`optional`);
//! 2. nooit meer dan de bovenliggende gebruiker zelf bezit (`user_caps`);
//! 3. nooit iets dat het EuroPol-beleid (Sprint X) voor dit agent-type verbiedt.

use crate::caps::AgentCaps;
use crate::manifest::AgentManifest;

/// Het resultaat van een capability-afleiding — bevat genoeg om de beslissing
/// auditeerbaar te maken in P3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CapDecision {
    /// De daadwerkelijk verleende caps.
    pub effective: AgentCaps,
    /// Caps die gevraagd waren maar door de user-clamp wegvielen.
    pub dropped_by_user: AgentCaps,
    /// Caps die door EuroPol-beleid geweigerd zijn.
    pub dropped_by_policy: AgentCaps,
    /// Vereist de effectieve set user-bevestiging (verhoogde caps)?
    pub needs_confirmation: bool,
}

/// Leid de effectieve capability-set af voor een agent-instantie.
///
/// - `manifest`     — de gedeclareerde required/optional caps;
/// - `granted`      — welke *optional* caps de gebruiker expliciet toekende;
/// - `user_caps`    — de caps van de bovenliggende gebruiker (harde bovengrens);
/// - `policy_denied`— de door EuroPol verboden caps voor dit agent-type.
pub fn derive(
    manifest: &AgentManifest,
    granted: AgentCaps,
    user_caps: AgentCaps,
    policy_denied: AgentCaps,
) -> CapDecision {
    // Optionele caps tellen enkel mee voor zover ze ook echt toegekend zijn.
    let granted_optional = manifest.optional.intersect(granted);
    let requested = AgentCaps(manifest.required.0 | granted_optional.0);

    // Stap 2: clamp op de gebruiker.
    let after_user = requested.intersect(user_caps);
    let dropped_by_user = AgentCaps(requested.0 & !after_user.0);

    // Stap 3: trek het verboden beleid af.
    let effective = AgentCaps(after_user.0 & !policy_denied.0);
    let dropped_by_policy = AgentCaps(after_user.0 & policy_denied.0);

    CapDecision {
        effective,
        dropped_by_user,
        dropped_by_policy,
        needs_confirmation: effective.has_elevated(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::*;
    use crate::manifest::AgentManifest;

    fn manifest(req: &str, opt: &str) -> AgentManifest {
        let toml = alloc::format!(
            "[agent]\nname=\"a\"\nversion=\"1\"\nwasm=\"a.wasm\"\n[capabilities]\nrequired=[{req}]\noptional=[{opt}]\n"
        );
        AgentManifest::from_toml(&toml).unwrap()
    }

    #[test]
    fn optional_needs_grant() {
        let m = manifest("\"CAP_AGENT_FS_WRITE\"", "\"CAP_AGENT_NET_GET\"");
        let user = AgentCaps(ALL);
        // Zonder grant: alleen required.
        let d = derive(&m, AgentCaps::empty(), user, AgentCaps::empty());
        assert!(d.effective.contains(FS_WRITE));
        assert!(!d.effective.contains(NET_GET));
        // Met grant: ook de optional.
        let d2 = derive(&m, AgentCaps(NET_GET), user, AgentCaps::empty());
        assert!(d2.effective.contains(NET_GET));
    }

    #[test]
    fn user_clamp_drops() {
        let m = manifest("\"CAP_AGENT_FS_WRITE\",\"CAP_AGENT_EXEC\"", "");
        let user = AgentCaps(FS_WRITE); // gebruiker mág geen EXEC
        let d = derive(&m, AgentCaps::empty(), user, AgentCaps::empty());
        assert!(d.effective.contains(FS_WRITE));
        assert!(!d.effective.contains(EXEC));
        assert!(d.dropped_by_user.contains(EXEC));
    }

    #[test]
    fn policy_denies() {
        let m = manifest("\"CAP_AGENT_MIC\",\"CAP_AGENT_NET_GET\"", "");
        let user = AgentCaps(ALL);
        let policy = AgentCaps(NET_GET); // EuroPol verbiedt netwerk voor dit type
        let d = derive(&m, AgentCaps::empty(), user, policy);
        assert!(d.effective.contains(MIC));
        assert!(!d.effective.contains(NET_GET));
        assert!(d.dropped_by_policy.contains(NET_GET));
    }

    #[test]
    fn elevated_triggers_confirmation() {
        let m = manifest("\"CAP_AGENT_EXEC\"", "");
        let d = derive(&m, AgentCaps::empty(), AgentCaps(ALL), AgentCaps::empty());
        assert!(d.needs_confirmation);
    }
}
