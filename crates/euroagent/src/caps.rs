//! `AgentCaps` — fine-grained, per-agent capabilities.
//!
//! These are *subsets* of the EuroGuard capabilities, split up further so that
//! an agent can respect the principle of least privilege: an agent gets
//! exactly what it declares in its manifest, never more. The set is a simple
//! `u64` bitset so that it is `no_std` and trivial to (de)serialize to P3.

use alloc::vec::Vec;

/// A bitset of agent capabilities (subset of EuroGuard).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct AgentCaps(pub u64);

// ── Storage ─────────────────────────────────────────────────────────────────
pub const FS_READ: u64 = 1 << 0; // read EuroFS within the sandbox
pub const FS_WRITE: u64 = 1 << 1; // write EuroFS within the sandbox
pub const FS_READ_GLOBAL: u64 = 1 << 2; // read EuroFS outside the sandbox (privileged)
pub const VAULT_READ: u64 = 1 << 3; // read EuroVault secrets
pub const VAULT_WRITE: u64 = 1 << 4; // write EuroVault secrets
// ── Network ─────────────────────────────────────────────────────────────────
pub const NET_GET: u64 = 1 << 8; // HTTP/HTTPS GET
pub const NET_POST: u64 = 1 << 9; // HTTP/HTTPS POST/PUT/DELETE
pub const NET_LISTEN: u64 = 1 << 10; // accept incoming connections
// ── Hardware ────────────────────────────────────────────────────────────────
pub const MIC: u64 = 1 << 16; // microphone
pub const CAMERA: u64 = 1 << 17; // camera
pub const SPEAKER: u64 = 1 << 18; // speaker
// ── System ──────────────────────────────────────────────────────────────────
pub const DISPLAY: u64 = 1 << 24; // EuroDisplay notifications + windows
pub const CALENDAR: u64 = 1 << 25; // read/write calendar
pub const EXEC: u64 = 1 << 26; // start subprocesses (highly privileged)
pub const AGENT_SPAWN: u64 = 1 << 27; // spawn other agents
pub const IPC_SEND: u64 = 1 << 28; // messages to other agents

/// The caps that count as "elevated" — granting one of them requires user confirmation.
pub const ELEVATED: u64 = EXEC | VAULT_WRITE | FS_READ_GLOBAL | AGENT_SPAWN | NET_LISTEN;

/// All known caps (for validation of unknown names).
pub const ALL: u64 = FS_READ
    | FS_WRITE
    | FS_READ_GLOBAL
    | VAULT_READ
    | VAULT_WRITE
    | NET_GET
    | NET_POST
    | NET_LISTEN
    | MIC
    | CAMERA
    | SPEAKER
    | DISPLAY
    | CALENDAR
    | EXEC
    | AGENT_SPAWN
    | IPC_SEND;

/// Map a manifest capability name (`CAP_AGENT_*`) to its bit. `None` =
/// unknown cap → the manifest is rejected.
pub fn from_name(name: &str) -> Option<u64> {
    let bit = match name.trim() {
        "CAP_AGENT_FS_READ" => FS_READ,
        "CAP_AGENT_FS_WRITE" => FS_WRITE,
        "CAP_AGENT_FS_READ_GLOBAL" => FS_READ_GLOBAL,
        "CAP_AGENT_VAULT_READ" => VAULT_READ,
        "CAP_AGENT_VAULT_WRITE" => VAULT_WRITE,
        "CAP_AGENT_NET_GET" | "CAP_AGENT_NET" => NET_GET,
        "CAP_AGENT_NET_POST" => NET_POST,
        "CAP_AGENT_NET_LISTEN" => NET_LISTEN,
        "CAP_AGENT_MIC" => MIC,
        "CAP_AGENT_CAMERA" => CAMERA,
        "CAP_AGENT_SPEAKER" => SPEAKER,
        "CAP_AGENT_DISPLAY" => DISPLAY,
        "CAP_AGENT_CALENDAR" => CALENDAR,
        "CAP_AGENT_EXEC" => EXEC,
        "CAP_AGENT_SPAWN" => AGENT_SPAWN,
        "CAP_AGENT_IPC_SEND" => IPC_SEND,
        _ => return None,
    };
    Some(bit)
}

/// The canonical name of a single cap bit.
pub fn to_name(bit: u64) -> &'static str {
    match bit {
        FS_READ => "CAP_AGENT_FS_READ",
        FS_WRITE => "CAP_AGENT_FS_WRITE",
        FS_READ_GLOBAL => "CAP_AGENT_FS_READ_GLOBAL",
        VAULT_READ => "CAP_AGENT_VAULT_READ",
        VAULT_WRITE => "CAP_AGENT_VAULT_WRITE",
        NET_GET => "CAP_AGENT_NET_GET",
        NET_POST => "CAP_AGENT_NET_POST",
        NET_LISTEN => "CAP_AGENT_NET_LISTEN",
        MIC => "CAP_AGENT_MIC",
        CAMERA => "CAP_AGENT_CAMERA",
        SPEAKER => "CAP_AGENT_SPEAKER",
        DISPLAY => "CAP_AGENT_DISPLAY",
        CALENDAR => "CAP_AGENT_CALENDAR",
        EXEC => "CAP_AGENT_EXEC",
        AGENT_SPAWN => "CAP_AGENT_SPAWN",
        IPC_SEND => "CAP_AGENT_IPC_SEND",
        _ => "CAP_AGENT_UNKNOWN",
    }
}

impl AgentCaps {
    pub const fn empty() -> Self {
        AgentCaps(0)
    }
    pub const fn contains(self, bits: u64) -> bool {
        (self.0 & bits) == bits
    }
    pub fn insert(&mut self, bits: u64) {
        self.0 |= bits;
    }
    pub fn remove(&mut self, bits: u64) {
        self.0 &= !bits;
    }
    /// The intersection with another set (used to clamp against user caps).
    pub fn intersect(self, other: AgentCaps) -> AgentCaps {
        AgentCaps(self.0 & other.0)
    }
    /// Does this set contain elevated (confirmation-requiring) caps?
    pub fn has_elevated(self) -> bool {
        self.0 & ELEVATED != 0
    }
    /// Build a set from a list of `CAP_AGENT_*` names. `Err(name)` on an
    /// unknown name.
    pub fn from_names(names: &[&str]) -> Result<AgentCaps, alloc::string::String> {
        use alloc::string::ToString;
        let mut c = AgentCaps(0);
        for n in names {
            match from_name(n) {
                Some(b) => c.0 |= b,
                None => return Err(n.to_string()),
            }
        }
        Ok(c)
    }
    /// The names of all set caps, ascending by bit.
    pub fn names(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        let mut bit = 1u64;
        while bit != 0 {
            if self.0 & bit != 0 {
                out.push(to_name(bit));
            }
            bit <<= 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_roundtrip() {
        for bit in [FS_READ, NET_GET, MIC, EXEC, DISPLAY] {
            assert_eq!(from_name(to_name(bit)), Some(bit));
        }
    }

    #[test]
    fn unknown_cap_rejected() {
        assert_eq!(from_name("CAP_AGENT_KERNEL_PANIC"), None);
        assert!(AgentCaps::from_names(&["CAP_AGENT_MIC", "CAP_AGENT_BOGUS"]).is_err());
    }

    #[test]
    fn elevated_detection() {
        let mut c = AgentCaps::from_names(&["CAP_AGENT_FS_WRITE", "CAP_AGENT_DISPLAY"]).unwrap();
        assert!(!c.has_elevated());
        c.insert(EXEC);
        assert!(c.has_elevated());
    }

    #[test]
    fn intersect_clamps() {
        let req = AgentCaps::from_names(&["CAP_AGENT_FS_WRITE", "CAP_AGENT_NET_GET", "CAP_AGENT_EXEC"]).unwrap();
        let user = AgentCaps::from_names(&["CAP_AGENT_FS_WRITE", "CAP_AGENT_NET_GET"]).unwrap();
        let eff = req.intersect(user);
        assert!(eff.contains(FS_WRITE | NET_GET));
        assert!(!eff.contains(EXEC));
    }
}
