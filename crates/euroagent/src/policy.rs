//! Capability derivation (Sprint AA, step 2) — the *only* place where it is
//! decided what an agent is ultimately allowed to do. Pure logic, host-tested.
//!
//! The effective set is always a **subset**:
//! ```text
//! effective = (required ∪ granted_optional) ∩ user_caps  −  policy_denied
//! ```
//! Three independent bounds, in order:
//! 1. the agent never gets more than it declares (`required`/`optional`);
//! 2. never more than the parent user themselves possesses (`user_caps`);
//! 3. never anything that the EuroPol policy (Sprint X) forbids for this agent type.

use crate::caps::AgentCaps;
use crate::manifest::AgentManifest;

/// The result of a capability derivation — contains enough to make the decision
/// auditable in P3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CapDecision {
    /// The actually granted caps.
    pub effective: AgentCaps,
    /// Caps that were requested but dropped by the user clamp.
    pub dropped_by_user: AgentCaps,
    /// Caps denied by EuroPol policy.
    pub dropped_by_policy: AgentCaps,
    /// Does the effective set require user confirmation (elevated caps)?
    pub needs_confirmation: bool,
}

/// Derive the effective capability set for an agent instance.
///
/// - `manifest`     — the declared required/optional caps;
/// - `granted`      — which *optional* caps the user explicitly granted;
/// - `user_caps`    — the caps of the parent user (hard upper bound);
/// - `policy_denied`— the caps forbidden by EuroPol for this agent type.
pub fn derive(
    manifest: &AgentManifest,
    granted: AgentCaps,
    user_caps: AgentCaps,
    policy_denied: AgentCaps,
) -> CapDecision {
    // Optional caps only count insofar as they are actually granted.
    let granted_optional = manifest.optional.intersect(granted);
    let requested = AgentCaps(manifest.required.0 | granted_optional.0);

    // Step 2: clamp to the user.
    let after_user = requested.intersect(user_caps);
    let dropped_by_user = AgentCaps(requested.0 & !after_user.0);

    // Step 3: subtract the forbidden policy.
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
        // Without grant: only required.
        let d = derive(&m, AgentCaps::empty(), user, AgentCaps::empty());
        assert!(d.effective.contains(FS_WRITE));
        assert!(!d.effective.contains(NET_GET));
        // With grant: also the optional.
        let d2 = derive(&m, AgentCaps(NET_GET), user, AgentCaps::empty());
        assert!(d2.effective.contains(NET_GET));
    }

    #[test]
    fn user_clamp_drops() {
        let m = manifest("\"CAP_AGENT_FS_WRITE\",\"CAP_AGENT_EXEC\"", "");
        let user = AgentCaps(FS_WRITE); // user may not have EXEC
        let d = derive(&m, AgentCaps::empty(), user, AgentCaps::empty());
        assert!(d.effective.contains(FS_WRITE));
        assert!(!d.effective.contains(EXEC));
        assert!(d.dropped_by_user.contains(EXEC));
    }

    #[test]
    fn policy_denies() {
        let m = manifest("\"CAP_AGENT_MIC\",\"CAP_AGENT_NET_GET\"", "");
        let user = AgentCaps(ALL);
        let policy = AgentCaps(NET_GET); // EuroPol forbids network for this type
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
