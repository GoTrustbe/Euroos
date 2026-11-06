//! **Policy learning** (2026-10-09): turn what an agent was DENIED into a
//! proposed manifest a person can approve. Enforcement stays on while learning
//! (a denied call is still denied, exactly like Microsoft Execution Containers'
//! learning mode); the only thing learning adds is a record of the wants.
//!
//! Rules of the proposal, so learning can never widen a boundary silently:
//! - a wanted capability that is NOT elevated goes to `required`;
//! - an ELEVATED capability (exec, vault write, global read, spawn, listen)
//!   only ever goes to `optional`: it still needs a JIT grant per action;
//! - a wanted tool goes to `tools.allowed`, unless it is in `tools.denied`
//!   (an explicit denial in the manifest is never undone by learning);
//! - nothing is ever removed from the base manifest;
//! - the proposal is a plain manifest: the person approves it by installing it.
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::caps::{self, AgentCaps};
use crate::manifest::AgentManifest;
use crate::mcp::{builtin_tools, AuditRecord};

/// One thing the agent wanted and did not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// A tool call refused for a missing capability (`reason` was
    /// `insufficient_capability`) or waiting on a JIT grant.
    Tool { tool: String, cap: u64, elevated: bool },
    /// A network destination outside `network_domains`.
    Domain(String),
}

/// Distil the wants from a gateway audit trail.
pub fn wants_from_audit(records: &[AuditRecord]) -> Vec<Want> {
    let tools = builtin_tools();
    let mut out: Vec<Want> = Vec::new();
    for r in records {
        if r.allowed {
            continue;
        }
        if r.reason != "insufficient_capability" && r.reason != "needs_jit_elevation" {
            continue;
        }
        let Some(t) = tools.iter().find(|t| t.name == r.tool) else { continue };
        let w = Want::Tool { tool: r.tool.clone(), cap: t.required_cap, elevated: t.required_cap & caps::ELEVATED != 0 };
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// The proposed manifest: `base` plus the wants, under the rules above.
pub fn propose(base: &AgentManifest, wants: &[Want]) -> AgentManifest {
    let mut m = base.clone();
    for w in wants {
        match w {
            Want::Tool { tool, cap, elevated } => {
                if base.tools_denied.iter().any(|d| d == tool) {
                    continue; // an explicit denial stands
                }
                if *elevated {
                    m.optional = AgentCaps(m.optional.0 | cap);
                } else {
                    m.required = AgentCaps(m.required.0 | cap);
                }
                if !m.tools_allowed.iter().any(|a| a == tool) {
                    m.tools_allowed.push(tool.clone());
                }
            }
            Want::Domain(d) => {
                if !m.network_domains.iter().any(|x| x == d) {
                    m.network_domains.push(d.clone());
                }
            }
        }
    }
    m
}

/// Human-readable difference `base` → `proposed`, one line per change.
pub fn diff(base: &AgentManifest, proposed: &AgentManifest) -> Vec<String> {
    let mut out = Vec::new();
    for bit in caps::ALL_BITS {
        if proposed.required.0 & bit != 0 && base.required.0 & bit == 0 {
            out.push(alloc::format!("+ required  {}", caps::to_name(bit)));
        }
        if proposed.optional.0 & bit != 0 && base.optional.0 & bit == 0 {
            out.push(alloc::format!("+ optional  {} (elevated: a JIT grant per action)", caps::to_name(bit)));
        }
    }
    for t in &proposed.tools_allowed {
        if !base.tools_allowed.contains(t) {
            out.push(alloc::format!("+ tool      {t}"));
        }
    }
    for d in &proposed.network_domains {
        if !base.network_domains.contains(d) {
            out.push(alloc::format!("+ domain    {d}"));
        }
    }
    if out.is_empty() {
        out.push("(nothing to add: every call was within the manifest)".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AgentManifest;

    fn base() -> AgentManifest {
        // The facilitator WITHOUT fs_write: the learning run will want it.
        let toml = crate::manifest::FACILITATOR_TOML.replace("    \"CAP_AGENT_FS_WRITE\",     # Save transcripts\n", "");
        AgentManifest::from_toml(&toml).unwrap()
    }
    fn rec(tool: &str, reason: &'static str) -> AuditRecord {
        AuditRecord { agent: "facilitator".into(), tool: tool.into(), allowed: false, succeeded: false, reason }
    }

    #[test]
    fn denied_fs_write_is_proposed_as_required() {
        let b = base();
        assert_eq!(b.required.0 & caps::FS_WRITE, 0);
        let w = wants_from_audit(&[rec("fs_write", "insufficient_capability")]);
        assert_eq!(w.len(), 1);
        let p = propose(&b, &w);
        assert_ne!(p.required.0 & caps::FS_WRITE, 0);
        assert!(p.tools_allowed.iter().any(|t| t == "fs_write"));
        assert!(diff(&b, &p).iter().any(|l| l.contains("CAP_AGENT_FS_WRITE")));
    }

    #[test]
    fn elevated_exec_only_becomes_optional_and_denied_tools_stay_denied() {
        let b = base(); // tools.denied contains "exec"
        let w = wants_from_audit(&[rec("exec", "insufficient_capability")]);
        let p = propose(&b, &w);
        assert_eq!(p.required.0 & caps::EXEC, 0, "exec must never land in required");
        assert_eq!(p.optional.0 & caps::EXEC, 0, "an explicitly denied tool is not proposed at all");
        assert!(!p.tools_allowed.iter().any(|t| t == "exec"));
        // an elevated tool that is NOT explicitly denied goes to optional only
        let w2 = wants_from_audit(&[rec("agent_spawn", "needs_jit_elevation")]);
        let p2 = propose(&b, &w2);
        assert_eq!(p2.required.0 & caps::AGENT_SPAWN, 0);
        assert_ne!(p2.optional.0 & caps::AGENT_SPAWN, 0);
    }

    #[test]
    fn allowed_and_unknown_records_are_ignored_and_nothing_is_removed() {
        let b = base();
        let mut ok = rec("fs_read", "ok"); ok.allowed = true;
        let w = wants_from_audit(&[ok, rec("nosuch", "tool_not_found")]);
        assert!(w.is_empty());
        let p = propose(&b, &w);
        assert_eq!(p, b);
    }
}
