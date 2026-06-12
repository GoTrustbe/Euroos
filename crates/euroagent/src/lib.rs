//! EuroAgent — sovereign agent-first runtime voor EuroOS (Sprint AA).
//!
//! Microsoft's Project Solara (Build 2026) maakt agents de primaire interactie-
//! eenheid, maar legt de trust boundary in de Microsoft-cloud (Entra ID, Azure-LLM).
//! EuroAgent doet hetzelfde agent-first model met de trust boundary **in de kernel**:
//! agents zijn WASM-modules met een declaratief capability-manifest, capability-
//! geïsoleerd op kernelniveau (EuroGuard), met een open MCP-gateway en volledige
//! P3-audittrail — en volledig offline (een lokale LLM is de standaard, cloud is
//! opt-in via EuroVault).
//!
//! Dit crate bevat de host-geteste, `no_std` kern:
//! - [`caps`]     — `AgentCaps`: fijnmazige per-agent capabilities (subset EuroGuard);
//! - [`manifest`] — `AgentManifest`: TOML-parser + validator van de agent-bundle;
//! - [`policy`]   — afleiding van de effectieve capability-set (least-privilege);
//! - [`json`]     — minimale JSON voor de MCP-laag;
//! - [`mcp`]      — `McpGateway`: JSON-RPC tool-dispatch met capability-gating + audit;
//! - [`intent`]   — deterministische intent→agent routing (EuroDispatch-kern).

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
