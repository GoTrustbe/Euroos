//! EuroAgent — sovereign agent-first runtime for EuroOS (Sprint AA).
//!
//! Microsoft's Project Solara (Build 2026) makes agents the primary interaction
//! unit, but places the trust boundary in the Microsoft cloud (Entra ID, Azure LLM).
//! EuroAgent does the same agent-first model with the trust boundary **in the kernel**:
//! agents are WASM modules with a declarative capability manifest, capability-
//! isolated at the kernel level (EuroGuard), with an open MCP gateway and a full
//! P3 audit trail — and fully offline (a local LLM is the default, cloud is
//! opt-in via EuroVault).
//!
//! This crate contains the host-tested, `no_std` core:
//! - [`caps`]     — `AgentCaps`: fine-grained per-agent capabilities (subset of EuroGuard);
//! - [`manifest`] — `AgentManifest`: TOML parser + validator of the agent bundle;
//! - [`policy`]   — derivation of the effective capability set (least-privilege);
//! - [`json`]     — minimal JSON for the MCP layer;
//! - [`mcp`]      — `McpGateway`: JSON-RPC tool dispatch with capability gating + audit;
//! - [`intent`]   — deterministic intent→agent routing (EuroDispatch core).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod agentloop;
pub mod anomaly;
pub mod bundle;
pub mod caps;
pub mod intent;
pub mod json;
pub mod llm;
pub mod manifest;
pub mod mcp;
pub mod policy;
pub mod registry;

pub use caps::AgentCaps;
pub use json::Json;
pub use manifest::{AgentManifest, ManifestError};
pub use mcp::McpGateway;
pub use policy::{derive, CapDecision};
