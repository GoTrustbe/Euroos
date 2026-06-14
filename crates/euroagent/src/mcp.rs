//! MCP gateway (Sprint AA, step 3) — the Model-Context-Protocol layer.
//!
//! Agents call EuroOS subsystems as *tools* via JSON-RPC 2.0. This module
//! is the protocol and authorization core, fully host-tested: it parses a
//! JSON-RPC request, looks up the tool, checks the required capability against the
//! `AgentCaps` of the caller, and produces a JSON-RPC response. The *execution*
//! of a tool sits behind a trait so the kernel binds it to real EuroFS/
//! EuroNet/EuroVault, while tests use a stub.

use crate::caps::{self, AgentCaps};
use crate::json::Json;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// JSON-RPC standard error codes + MCP-specific extensions.
pub const ERR_PARSE: i64 = -32700;
pub const ERR_INVALID_REQUEST: i64 = -32600;
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
pub const ERR_INVALID_PARAMS: i64 = -32602;
/// MCP: the agent lacks the capability for this tool.
pub const ERR_CAP_DENIED: i64 = -32001;
/// MCP: the tool does not exist.
pub const ERR_TOOL_NOT_FOUND: i64 = -32002;
/// MCP: the tool failed during execution.
pub const ERR_TOOL_FAILED: i64 = -32003;
/// An ELEVATED capability requires a just-in-time grant that is not present
/// right now: the call is denied until the user grants it for this one
/// action. (AF / Zero-Trust P2.2: elevate-for-the-task, auto-revoke.)
pub const ERR_JIT_REQUIRED: i64 = -32004;

/// A tool that the gateway offers.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub required_cap: u64,
}

/// The standard EuroOS toolset (see EUROAGENT-PLAN §MCP-gateway).
pub fn builtin_tools() -> Vec<ToolDef> {
    vec![
        ToolDef { name: "fs_read", description: "Read a file from EuroFS within the agent sandbox", required_cap: caps::FS_READ },
        ToolDef { name: "fs_write", description: "Write data to EuroFS within the agent sandbox", required_cap: caps::FS_WRITE },
        ToolDef { name: "net_get", description: "HTTP/HTTPS GET via EuroNet + EuroTLS", required_cap: caps::NET_GET },
        ToolDef { name: "net_post", description: "HTTP/HTTPS POST via EuroNet + EuroTLS", required_cap: caps::NET_POST },
        ToolDef { name: "vault_get", description: "Read a secret from EuroVault (value never logged)", required_cap: caps::VAULT_READ },
        ToolDef { name: "display_notify", description: "Send a notification to EuroDisplay", required_cap: caps::DISPLAY },
        ToolDef { name: "calendar_read", description: "Read calendar events (EuroIDM)", required_cap: caps::CALENDAR },
        ToolDef { name: "mic_record", description: "Record audio via the microphone", required_cap: caps::MIC },
        ToolDef { name: "agent_spawn", description: "Spawn a sub-agent for delegation", required_cap: caps::AGENT_SPAWN },
        ToolDef { name: "exec", description: "Run a command (highly privileged)", required_cap: caps::EXEC },
    ]
}

/// The backend that actually executes an authorized tool call.
/// The kernel implements this; tests use a stub.
pub trait ToolBackend {
    /// Run `tool` with `input`. `Ok(Json)` = result, `Err(msg)` = failed.
    fn execute(&mut self, tool: &str, input: &Json) -> Result<Json, String>;
}

/// An audit record of one tool call (goes to P3).
#[derive(Clone, Debug, PartialEq)]
pub struct AuditRecord {
    pub agent: String,
    pub tool: String,
    pub allowed: bool,
    pub succeeded: bool,
    pub reason: &'static str,
}

/// The MCP gateway: tool registry + cap gating + audit.
pub struct McpGateway {
    tools: Vec<ToolDef>,
    pub audit: Vec<AuditRecord>,
    /// AF / Zero-Trust P2.2 — **just-in-time elevation**: a bitset of ELEVATED
    /// caps (`caps::ELEVATED`) that are granted for exactly ONE upcoming tool
    /// call. After use the bit is cleared (auto-revoke). Elevated caps are
    /// NEVER in the standing set: they must be freshly granted per action.
    jit_grants: u64,
}

impl Default for McpGateway {
    fn default() -> Self {
        McpGateway { tools: builtin_tools(), audit: Vec::new(), jit_grants: 0 }
    }
}

impl McpGateway {
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant an ELEVATED capability just-in-time for the very next call that
    /// needs it (in practice after explicit user confirmation). The grant is
    /// automatically revoked on use — one action, then gone again.
    pub fn grant_jit(&mut self, cap: u64) {
        self.jit_grants |= cap & caps::ELEVATED;
    }

    /// The caps that currently have an outstanding JIT grant (for inspection/audit).
    pub fn pending_jit(&self) -> u64 {
        self.jit_grants
    }

    fn find(&self, name: &str) -> Option<&ToolDef> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// The MCP `tools/list` payload (only the tools that `caps` may see/call).
    pub fn list_for(&self, caps_set: AgentCaps) -> Json {
        let arr = self
            .tools
            .iter()
            .filter(|t| caps_set.contains(t.required_cap))
            .map(|t| {
                Json::Obj(vec![
                    ("name".to_string(), Json::Str(t.name.to_string())),
                    ("description".to_string(), Json::Str(t.description.to_string())),
                    ("required_cap".to_string(), Json::Str(caps::to_name(t.required_cap).to_string())),
                ])
            })
            .collect();
        Json::Obj(vec![("tools".to_string(), Json::Arr(arr))])
    }

    /// Process a raw JSON-RPC request string from an agent with capability set
    /// `caps_set`. Returns the JSON-RPC response string. `agent` is only for
    /// the audit trail.
    pub fn handle(
        &mut self,
        agent: &str,
        caps_set: AgentCaps,
        request: &str,
        backend: &mut dyn ToolBackend,
    ) -> String {
        let req = match Json::parse(request) {
            Ok(v) => v,
            Err(_) => return error_response(&Json::Null, ERR_PARSE, "parse error"),
        };
        let id = req.get("id").cloned().unwrap_or(Json::Null);
        let method = match req.get("method").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => return error_response(&id, ERR_INVALID_REQUEST, "missing method"),
        };

        match method {
            "tools/list" => result_response(&id, self.list_for(caps_set)),
            "tools/call" => self.handle_call(agent, caps_set, &id, &req, backend),
            _ => error_response(&id, ERR_METHOD_NOT_FOUND, "unknown method"),
        }
    }

    fn handle_call(
        &mut self,
        agent: &str,
        caps_set: AgentCaps,
        id: &Json,
        req: &Json,
        backend: &mut dyn ToolBackend,
    ) -> String {
        let params = match req.get("params") {
            Some(p) => p,
            None => return error_response(id, ERR_INVALID_PARAMS, "missing params"),
        };
        let name = match params.get("name").and_then(|n| n.as_str()) {
            Some(n) => n,
            None => return error_response(id, ERR_INVALID_PARAMS, "missing tool name"),
        };
        let empty = Json::Obj(Vec::new());
        let input = params.get("arguments").unwrap_or(&empty);

        let required_cap = match self.find(name) {
            Some(t) => t.required_cap,
            None => {
                self.audit.push(AuditRecord {
                    agent: agent.to_string(),
                    tool: name.to_string(),
                    allowed: false,
                    succeeded: false,
                    reason: "tool_not_found",
                });
                return error_response(id, ERR_TOOL_NOT_FOUND, "unknown tool");
            }
        };

        // ── Capability gate ──────────────────────────────────────────────
        if !caps_set.contains(required_cap) {
            self.audit.push(AuditRecord {
                agent: agent.to_string(),
                tool: name.to_string(),
                allowed: false,
                succeeded: false,
                reason: "insufficient_capability",
            });
            return error_response(id, ERR_CAP_DENIED, "capability denied");
        }

        // ── JIT elevation gate (P2.2) ─────────────────────────────────────
        // Elevated caps (EXEC, VAULT_WRITE, …) are admittedly in the standing set,
        // but may ONLY be used with a fresh just-in-time grant. If there is no
        // outstanding grant → deny (the UI then asks for confirmation). If there
        // is one → consume it now (auto-revoke), so that a NEXT call again
        // requires confirmation. This way the elevation is bounded to one concrete action.
        if required_cap & caps::ELEVATED != 0 {
            if self.jit_grants & required_cap != 0 {
                self.jit_grants &= !required_cap; // auto-revoke after this one action
            } else {
                self.audit.push(AuditRecord {
                    agent: agent.to_string(),
                    tool: name.to_string(),
                    allowed: false,
                    succeeded: false,
                    reason: "needs_jit_elevation",
                });
                return error_response(id, ERR_JIT_REQUIRED, "just-in-time elevation required");
            }
        }

        // Authorized → execute.
        let outcome = backend.execute(name, input);
        let succeeded = outcome.is_ok();
        self.audit.push(AuditRecord {
            agent: agent.to_string(),
            tool: name.to_string(),
            allowed: true,
            succeeded,
            reason: if succeeded { "ok" } else { "tool_failed" },
        });
        match outcome {
            Ok(result) => result_response(id, result),
            Err(_) => error_response(id, ERR_TOOL_FAILED, "tool execution failed"),
        }
    }
}

fn result_response(id: &Json, result: Json) -> String {
    Json::Obj(vec![
        ("jsonrpc".to_string(), Json::Str("2.0".to_string())),
        ("id".to_string(), id.clone()),
        ("result".to_string(), result),
    ])
    .to_string()
}

fn error_response(id: &Json, code: i64, message: &str) -> String {
    Json::Obj(vec![
        ("jsonrpc".to_string(), Json::Str("2.0".to_string())),
        ("id".to_string(), id.clone()),
        (
            "error".to_string(),
            Json::Obj(vec![
                ("code".to_string(), Json::Num(code.to_string())),
                ("message".to_string(), Json::Str(message.to_string())),
            ]),
        ),
    ])
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::*;

    /// Stub backend: returns the input under "echo", or fails for "net_get".
    struct Stub;
    impl ToolBackend for Stub {
        fn execute(&mut self, tool: &str, input: &Json) -> Result<Json, String> {
            if tool == "net_get" {
                return Err("network down".to_string());
            }
            Ok(Json::Obj(vec![("echo".to_string(), input.clone())]))
        }
    }

    fn call(name: &str, args: &str) -> String {
        alloc::format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{args}}}}}"#
        )
    }

    #[test]
    fn authorized_call_succeeds() {
        let mut g = McpGateway::new();
        let caps = AgentCaps(FS_WRITE);
        let resp = g.handle("a", caps, &call("fs_write", r#"{"path":"x","content":"hi"}"#), &mut Stub);
        let v = Json::parse(&resp).unwrap();
        assert!(v.get("result").is_some());
        assert_eq!(g.audit.last().unwrap().reason, "ok");
        assert!(g.audit.last().unwrap().allowed);
    }

    #[test]
    fn missing_cap_denied() {
        let mut g = McpGateway::new();
        let caps = AgentCaps(FS_READ); // no EXEC
        let resp = g.handle("a", caps, &call("exec", r#"{"cmd":"rm -rf /"}"#), &mut Stub);
        let v = Json::parse(&resp).unwrap();
        assert_eq!(v.get("error").unwrap().get("code").unwrap().as_i64(), Some(ERR_CAP_DENIED));
        assert_eq!(g.audit.last().unwrap().reason, "insufficient_capability");
        assert!(!g.audit.last().unwrap().allowed);
    }

    #[test]
    fn unknown_tool() {
        let mut g = McpGateway::new();
        let resp = g.handle("a", AgentCaps(ALL), &call("teleport", "{}"), &mut Stub);
        let v = Json::parse(&resp).unwrap();
        assert_eq!(v.get("error").unwrap().get("code").unwrap().as_i64(), Some(ERR_TOOL_NOT_FOUND));
    }

    #[test]
    fn tool_failure_reported() {
        let mut g = McpGateway::new();
        let resp = g.handle("a", AgentCaps(NET_GET), &call("net_get", r#"{"url":"https://eu"}"#), &mut Stub);
        let v = Json::parse(&resp).unwrap();
        assert_eq!(v.get("error").unwrap().get("code").unwrap().as_i64(), Some(ERR_TOOL_FAILED));
        assert!(g.audit.last().unwrap().allowed); // allowed, but failed
        assert!(!g.audit.last().unwrap().succeeded);
    }

    #[test]
    fn elevated_cap_requires_jit_grant_and_auto_revokes() {
        // AF / P2.2: even WITH EXEC in the standing set, an elevated tool call
        // is denied until there is a fresh JIT grant — and that applies to ONE action.
        let mut g = McpGateway::new();
        let caps = AgentCaps(EXEC); // standing set contains the elevated cap...
        let req = call("exec", r#"{"cmd":"build"}"#);

        // 1. Without grant → denied with ERR_JIT_REQUIRED.
        let r1 = g.handle("a", caps, &req, &mut Stub);
        let v1 = Json::parse(&r1).unwrap();
        assert_eq!(v1.get("error").unwrap().get("code").unwrap().as_i64(), Some(ERR_JIT_REQUIRED));
        assert_eq!(g.audit.last().unwrap().reason, "needs_jit_elevation");

        // 2. JIT grant (after confirmation) → the very next call is allowed.
        g.grant_jit(EXEC);
        assert_eq!(g.pending_jit(), EXEC);
        let r2 = g.handle("a", caps, &req, &mut Stub);
        assert!(Json::parse(&r2).unwrap().get("result").is_some());
        assert!(g.audit.last().unwrap().allowed);

        // 3. Auto-revoke: the grant is consumed → a second call is again denied.
        assert_eq!(g.pending_jit(), 0);
        let r3 = g.handle("a", caps, &req, &mut Stub);
        let v3 = Json::parse(&r3).unwrap();
        assert_eq!(v3.get("error").unwrap().get("code").unwrap().as_i64(), Some(ERR_JIT_REQUIRED));
    }

    #[test]
    fn grant_jit_ignores_non_elevated_caps() {
        // grant_jit may only take ELEVATED caps; an ordinary cap stays 0.
        let mut g = McpGateway::new();
        g.grant_jit(FS_READ); // non-elevated → ignored
        assert_eq!(g.pending_jit(), 0);
        g.grant_jit(VAULT_WRITE); // elevated → taken
        assert_eq!(g.pending_jit(), VAULT_WRITE);
    }

    /// Credentials at the boundary: a tool (such as `vault_get`) may return a secret
    /// value in the RESULT, but that value may NEVER end up in the
    /// audit trail. (The `AuditRecord` only carries name+cap+outcome —
    /// this test is the regression guarantee that it stays that way.)
    #[test]
    fn audit_never_contains_the_secret_value() {
        struct SecretBackend;
        impl ToolBackend for SecretBackend {
            fn execute(&mut self, _tool: &str, _input: &Json) -> Result<Json, String> {
                Ok(Json::Obj(vec![("value".to_string(), Json::Str("euro-s3cr3t".to_string()))]))
            }
        }
        let mut g = McpGateway::new();
        let resp = g.handle("a", AgentCaps(VAULT_READ), &call("vault_get", r#"{"label":"db-password"}"#), &mut SecretBackend);
        // The value IS in the tool result (the agent with the cap receives it)...
        let v = Json::parse(&resp).unwrap();
        assert_eq!(
            v.get("result").and_then(|r| r.get("value")).and_then(|s| s.as_str()),
            Some("euro-s3cr3t")
        );
        // ...but NEVER in the audit trail.
        let rec = g.audit.last().unwrap();
        assert!(rec.allowed && rec.succeeded);
        let rendered = alloc::format!("{:?}", g.audit);
        assert!(!rendered.contains("euro-s3cr3t"), "secret must never be in the audit trail");
    }

    #[test]
    fn list_filters_by_caps() {
        let g = McpGateway::new();
        let v = g.list_for(AgentCaps(FS_READ | DISPLAY));
        let tools = v.get("tools").unwrap();
        if let Json::Arr(items) = tools {
            assert_eq!(items.len(), 2);
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn parse_error_handled() {
        let mut g = McpGateway::new();
        let resp = g.handle("a", AgentCaps(ALL), "{not json", &mut Stub);
        let v = Json::parse(&resp).unwrap();
        assert_eq!(v.get("error").unwrap().get("code").unwrap().as_i64(), Some(ERR_PARSE));
    }
}
