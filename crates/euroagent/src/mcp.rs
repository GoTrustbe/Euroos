//! MCP-gateway (Sprint AA, stap 3) — de Model-Context-Protocol-laag.
//!
//! Agents roepen EuroOS-subsystemen aan als *tools* via JSON-RPC 2.0. Deze module
//! is de protocol- en autorisatiekern, volledig host-getest: hij parseert een
//! JSON-RPC-verzoek, zoekt de tool op, controleert de vereiste capability tegen de
//! `AgentCaps` van de aanroeper, en produceert een JSON-RPC-antwoord. De *uitvoering*
//! van een tool zit achter een trait zodat de kernel hem koppelt aan echte EuroFS/
//! EuroNet/EuroVault, terwijl tests een stub gebruiken.

use crate::caps::{self, AgentCaps};
use crate::json::Json;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// JSON-RPC standaard foutcodes + MCP-specifieke uitbreidingen.
pub const ERR_PARSE: i64 = -32700;
pub const ERR_INVALID_REQUEST: i64 = -32600;
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
pub const ERR_INVALID_PARAMS: i64 = -32602;
/// MCP: de agent mist de capability voor deze tool.
pub const ERR_CAP_DENIED: i64 = -32001;
/// MCP: de tool bestaat niet.
pub const ERR_TOOL_NOT_FOUND: i64 = -32002;
/// MCP: de tool faalde tijdens uitvoering.
pub const ERR_TOOL_FAILED: i64 = -32003;
/// Een verhoogde (ELEVATED) capability vereist een just-in-time grant die er nu
/// niet is: de aanroep wordt geweigerd tot de gebruiker hem voor déze ene actie
/// toekent. (AF / Zero-Trust P2.2: elevate-for-the-task, auto-revoke.)
pub const ERR_JIT_REQUIRED: i64 = -32004;

/// Een tool die de gateway aanbiedt.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub required_cap: u64,
}

/// De standaard EuroOS-toolset (zie EUROAGENT-PLAN §MCP-gateway).
pub fn builtin_tools() -> Vec<ToolDef> {
    vec![
        ToolDef { name: "fs_read", description: "Lees een bestand van EuroFS binnen de agent-sandbox", required_cap: caps::FS_READ },
        ToolDef { name: "fs_write", description: "Schrijf data naar EuroFS binnen de agent-sandbox", required_cap: caps::FS_WRITE },
        ToolDef { name: "net_get", description: "HTTP/HTTPS GET via EuroNet + EuroTLS", required_cap: caps::NET_GET },
        ToolDef { name: "net_post", description: "HTTP/HTTPS POST via EuroNet + EuroTLS", required_cap: caps::NET_POST },
        ToolDef { name: "vault_get", description: "Lees een secret uit EuroVault (waarde nooit gelogd)", required_cap: caps::VAULT_READ },
        ToolDef { name: "display_notify", description: "Stuur een notificatie naar EuroDisplay", required_cap: caps::DISPLAY },
        ToolDef { name: "calendar_read", description: "Lees agenda-events (EuroIDM)", required_cap: caps::CALENDAR },
        ToolDef { name: "mic_record", description: "Neem audio op via de microfoon", required_cap: caps::MIC },
        ToolDef { name: "agent_spawn", description: "Spawn een sub-agent voor delegatie", required_cap: caps::AGENT_SPAWN },
        ToolDef { name: "exec", description: "Voer een commando uit (zeer privileged)", required_cap: caps::EXEC },
    ]
}

/// De backend die een geautoriseerde tool-aanroep daadwerkelijk uitvoert.
/// De kernel implementeert dit; tests gebruiken een stub.
pub trait ToolBackend {
    /// Voer `tool` uit met `input`. `Ok(Json)` = resultaat, `Err(msg)` = mislukt.
    fn execute(&mut self, tool: &str, input: &Json) -> Result<Json, String>;
}

/// Een audit-record van één tool-aanroep (gaat naar P3).
#[derive(Clone, Debug, PartialEq)]
pub struct AuditRecord {
    pub agent: String,
    pub tool: String,
    pub allowed: bool,
    pub succeeded: bool,
    pub reason: &'static str,
}

/// De MCP-gateway: tool-registry + cap-gating + audit.
pub struct McpGateway {
    tools: Vec<ToolDef>,
    pub audit: Vec<AuditRecord>,
    /// AF / Zero-Trust P2.2 — **just-in-time elevatie**: een bitset van VERHOOGDE
    /// caps (`caps::ELEVATED`) die voor exact ÉÉN volgende tool-aanroep zijn
    /// toegekend. Na gebruik wordt de bit gewist (auto-revoke). Verhoogde caps
    /// zitten NOOIT in de staande set: ze moeten per actie vers toegekend worden.
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

    /// Ken een VERHOOGDE capability just-in-time toe voor de éérstvolgende aanroep
    /// die hem nodig heeft (in de praktijk ná expliciete gebruikersbevestiging). De
    /// grant wordt bij gebruik automatisch ingetrokken — één actie, dan weer weg.
    pub fn grant_jit(&mut self, cap: u64) {
        self.jit_grants |= cap & caps::ELEVATED;
    }

    /// De caps die momenteel een openstaande JIT-grant hebben (voor inspectie/audit).
    pub fn pending_jit(&self) -> u64 {
        self.jit_grants
    }

    fn find(&self, name: &str) -> Option<&ToolDef> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// De MCP `tools/list`-payload (alleen de tools die `caps` mag zien/aanroepen).
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

    /// Verwerk een ruwe JSON-RPC-verzoekstring van een agent met capability-set
    /// `caps_set`. Geeft de JSON-RPC-antwoordstring terug. `agent` is enkel voor
    /// de audit-trail.
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

        // ── Capability-gate ──────────────────────────────────────────────
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

        // ── JIT-elevatie-gate (P2.2) ─────────────────────────────────────
        // Verhoogde caps (EXEC, VAULT_WRITE, …) zitten weliswaar in de staande set,
        // maar mogen alléén met een verse just-in-time grant gebruikt worden. Is er
        // geen openstaande grant → weiger (de UI vraagt dan om bevestiging). Is er
        // wél één → verbruik hem nu (auto-revoke), zodat een vólgende aanroep weer
        // bevestiging vereist. Zo is de elevatie begrensd tot één concrete actie.
        if required_cap & caps::ELEVATED != 0 {
            if self.jit_grants & required_cap != 0 {
                self.jit_grants &= !required_cap; // auto-revoke na deze ene actie
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

        // Geautoriseerd → uitvoeren.
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

    /// Stub-backend: geeft de input terug onder "echo", of faalt voor "net_get".
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
        let caps = AgentCaps(FS_READ); // geen EXEC
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
        assert!(g.audit.last().unwrap().allowed); // wél toegestaan, maar mislukt
        assert!(!g.audit.last().unwrap().succeeded);
    }

    #[test]
    fn elevated_cap_requires_jit_grant_and_auto_revokes() {
        // AF / P2.2: zelfs mét EXEC in de staande set is een verhoogde tool-call
        // geweigerd tot er een verse JIT-grant is — en die geldt voor ÉÉN actie.
        let mut g = McpGateway::new();
        let caps = AgentCaps(EXEC); // staande set bevat de verhoogde cap...
        let req = call("exec", r#"{"cmd":"build"}"#);

        // 1. Zonder grant → geweigerd met ERR_JIT_REQUIRED.
        let r1 = g.handle("a", caps, &req, &mut Stub);
        let v1 = Json::parse(&r1).unwrap();
        assert_eq!(v1.get("error").unwrap().get("code").unwrap().as_i64(), Some(ERR_JIT_REQUIRED));
        assert_eq!(g.audit.last().unwrap().reason, "needs_jit_elevation");

        // 2. JIT-grant (na bevestiging) → de éérstvolgende call mag.
        g.grant_jit(EXEC);
        assert_eq!(g.pending_jit(), EXEC);
        let r2 = g.handle("a", caps, &req, &mut Stub);
        assert!(Json::parse(&r2).unwrap().get("result").is_some());
        assert!(g.audit.last().unwrap().allowed);

        // 3. Auto-revoke: de grant is verbruikt → een tweede call is wéér geweigerd.
        assert_eq!(g.pending_jit(), 0);
        let r3 = g.handle("a", caps, &req, &mut Stub);
        let v3 = Json::parse(&r3).unwrap();
        assert_eq!(v3.get("error").unwrap().get("code").unwrap().as_i64(), Some(ERR_JIT_REQUIRED));
    }

    #[test]
    fn grant_jit_ignores_non_elevated_caps() {
        // grant_jit mag alleen VERHOOGDE caps opnemen; een gewone cap blijft 0.
        let mut g = McpGateway::new();
        g.grant_jit(FS_READ); // niet-verhoogd → genegeerd
        assert_eq!(g.pending_jit(), 0);
        g.grant_jit(VAULT_WRITE); // verhoogd → opgenomen
        assert_eq!(g.pending_jit(), VAULT_WRITE);
    }

    /// Credentials at the boundary: een tool (zoals `vault_get`) mag een geheime
    /// waarde teruggeven in het RESULTAAT, maar die waarde mag NOOIT in de
    /// audit-trail belanden. (De `AuditRecord` draagt enkel naam+cap+uitkomst —
    /// deze test is de regressie-waarborg dat dat zo blijft.)
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
        // De waarde zit WÉL in het tool-resultaat (de agent met de cap krijgt hem)...
        let v = Json::parse(&resp).unwrap();
        assert_eq!(
            v.get("result").and_then(|r| r.get("value")).and_then(|s| s.as_str()),
            Some("euro-s3cr3t")
        );
        // ...maar NOOIT in de audit-trail.
        let rec = g.audit.last().unwrap();
        assert!(rec.allowed && rec.succeeded);
        let rendered = alloc::format!("{:?}", g.audit);
        assert!(!rendered.contains("euro-s3cr3t"), "secret mag nooit in de audit-trail zitten");
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
