//! De agent-uitvoeringslus (Sprint AA, stap 5) — het hart van de runtime.
//!
//! Deterministisch en auditeerbaar: gegeven een gebruikers-intent draait de lus
//! `model → (tool-call → MCP-gateway → resultaat → model)* → eindantwoord`. Elke
//! tool-aanroep gaat door de [`McpGateway`] (capability-gate + P3-audit), dus de
//! lus kan een agent nooit meer laten doen dan zijn `AgentCaps` toestaan. De LLM
//! zit achter de [`LlmBackend`]-trait — host-getest met een gescript mock-model.

use crate::caps::AgentCaps;
use crate::json::Json;
use crate::llm::{LlmBackend, LlmResponse, Message, Role};
use crate::mcp::{McpGateway, ToolBackend};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// Het resultaat van een volledige agent-run.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRun {
    /// Het eindantwoord van het model.
    pub answer: String,
    /// Het aantal tool-aanroepen dat onderweg gedaan is.
    pub tool_calls: usize,
    /// Hoeveel daarvan door de capability-gate geweigerd zijn.
    pub denied: usize,
    /// Bereikt de lus de step-limiet zonder eindantwoord?
    pub truncated: bool,
    /// Per-stap transcript (voor de live audit-weergave in de dispatch-GUI):
    /// elke tool-aanroep + of de capability-gate ze toestond of weigerde.
    pub log: Vec<String>,
}

/// Draai de agent-lus tot een eindantwoord of tot `max_steps` bereikt is.
///
/// - `name`     — agent-identiteit (voor de audit-trail);
/// - `caps`     — de effectieve capability-set (gate voor elke tool);
/// - `llm`      — het taalmodel;
/// - `gateway`  — de MCP-gateway (cap-gate + audit);
/// - `tools`    — de backend die geautoriseerde tools uitvoert;
/// - `messages` — de start-conversatie (system + user-intent).
pub fn run(
    name: &str,
    caps: AgentCaps,
    llm: &mut dyn LlmBackend,
    gateway: &mut McpGateway,
    tools: &mut dyn ToolBackend,
    mut messages: Vec<Message>,
    max_steps: usize,
) -> AgentRun {
    let tool_names: Vec<&str> = gateway_tool_names(gateway, caps);
    let mut tool_calls = 0usize;
    let mut denied = 0usize;
    let mut log: Vec<String> = Vec::new();

    for _ in 0..max_steps {
        let resp = llm.step(&messages, &tool_names);
        match resp {
            LlmResponse::Text(answer) => {
                return AgentRun { answer, tool_calls, denied, truncated: false, log };
            }
            LlmResponse::ToolCall { name: tool, arguments } => {
                tool_calls += 1;
                // Voer de tool uit via de gateway (cap-gate + audit) als JSON-RPC.
                let req = jsonrpc_call(&tool, &arguments, tool_calls as i64);
                let raw = gateway.handle(name, caps, &req, tools);
                let (content, was_denied) = summarize(&raw);
                if was_denied {
                    denied += 1;
                    log.push(alloc::format!("tool {tool} → GEWEIGERD door de capability-gate (toestemming vereist)"));
                } else {
                    log.push(alloc::format!("tool {tool} → toegestaan, uitgevoerd, geaudit"));
                }
                // Voer het resultaat terug naar het model.
                messages.push(Message::new(Role::Assistant, alloc::format!("[tool {tool}]")));
                messages.push(Message::new(Role::Tool, content));
            }
        }
    }

    AgentRun {
        answer: String::new(),
        tool_calls,
        denied,
        truncated: true,
        log,
    }
}

/// De namen van de tools die deze cap-set mag aanroepen.
fn gateway_tool_names(gateway: &McpGateway, caps: AgentCaps) -> Vec<&'static str> {
    // `list_for` levert exact de zichtbare tools; haal de namen eruit.
    match gateway.list_for(caps).get("tools") {
        Some(Json::Arr(items)) => items
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .map(static_name)
            .collect(),
        _ => Vec::new(),
    }
}

/// Map een tool-naam naar zijn `'static`-variant (de gateway-tools zijn vast).
fn static_name(n: &str) -> &'static str {
    for t in crate::mcp::builtin_tools() {
        if t.name == n {
            return t.name;
        }
    }
    "unknown"
}

fn jsonrpc_call(tool: &str, args: &Json, id: i64) -> String {
    Json::Obj(vec![
        ("jsonrpc".to_string(), Json::Str("2.0".to_string())),
        ("id".to_string(), Json::Num(id.to_string())),
        ("method".to_string(), Json::Str("tools/call".to_string())),
        (
            "params".to_string(),
            Json::Obj(vec![
                ("name".to_string(), Json::Str(tool.to_string())),
                ("arguments".to_string(), args.clone()),
            ]),
        ),
    ])
    .to_string()
}

/// Vat een JSON-RPC-antwoord samen tot tekst voor het model + of het een
/// capability-weigering was.
fn summarize(raw: &str) -> (String, bool) {
    match Json::parse(raw) {
        Ok(v) => {
            if let Some(err) = v.get("error") {
                let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("error");
                (
                    alloc::format!("FOUT: {msg}"),
                    code == crate::mcp::ERR_CAP_DENIED,
                )
            } else if let Some(res) = v.get("result") {
                (res.to_string(), false)
            } else {
                (String::from("(leeg)"), false)
            }
        }
        Err(_) => (String::from("(onleesbaar antwoord)"), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::*;

    /// Een gescript mock-model: doorloopt een vaste lijst antwoorden.
    struct ScriptedLlm {
        script: Vec<LlmResponse>,
        idx: usize,
    }
    impl LlmBackend for ScriptedLlm {
        fn step(&mut self, _m: &[Message], _t: &[&str]) -> LlmResponse {
            let r = self.script.get(self.idx).cloned().unwrap_or(LlmResponse::Text("(einde)".into()));
            self.idx += 1;
            r
        }
    }

    /// Tool-backend die een vast resultaat teruggeeft.
    struct EchoTools;
    impl ToolBackend for EchoTools {
        fn execute(&mut self, _tool: &str, input: &Json) -> Result<Json, String> {
            Ok(Json::Obj(vec![("read".to_string(), input.clone())]))
        }
    }

    fn tool_call(name: &str, key: &str, val: &str) -> LlmResponse {
        LlmResponse::ToolCall {
            name: name.to_string(),
            arguments: Json::Obj(vec![(key.to_string(), Json::Str(val.to_string()))]),
        }
    }

    #[test]
    fn loop_runs_tool_then_answers() {
        // Model: roep fs_read aan, geef daarna een eindantwoord.
        let mut llm = ScriptedLlm {
            script: vec![
                tool_call("fs_read", "path", "notes.txt"),
                LlmResponse::Text("De notitie zegt: hallo.".to_string()),
            ],
            idx: 0,
        };
        let mut gw = McpGateway::new();
        let mut tools = EchoTools;
        let caps = AgentCaps(FS_READ);
        let run = run(
            "assistent",
            caps,
            &mut llm,
            &mut gw,
            &mut tools,
            vec![Message::user("wat staat er in notes.txt?")],
            8,
        );
        assert_eq!(run.answer, "De notitie zegt: hallo.");
        assert_eq!(run.tool_calls, 1);
        assert_eq!(run.denied, 0);
        assert!(!run.truncated);
        // De tool-aanroep is geauditeerd in de gateway.
        assert_eq!(gw.audit.len(), 1);
        assert!(gw.audit[0].allowed);
    }

    #[test]
    fn capability_denied_is_recorded() {
        // Model probeert exec zonder de EXEC-cap → geweigerd, dan geeft het op.
        let mut llm = ScriptedLlm {
            script: vec![
                tool_call("exec", "cmd", "rm -rf /"),
                LlmResponse::Text("Mag niet, ik stop.".to_string()),
            ],
            idx: 0,
        };
        let mut gw = McpGateway::new();
        let mut tools = EchoTools;
        let caps = AgentCaps(FS_READ); // GEEN exec
        let run = run("assistent", caps, &mut llm, &mut gw, &mut tools, vec![Message::user("verwijder alles")], 8);
        assert_eq!(run.tool_calls, 1);
        assert_eq!(run.denied, 1);
        assert_eq!(run.answer, "Mag niet, ik stop.");
        assert!(!gw.audit[0].allowed);
    }

    #[test]
    fn loop_truncates_at_step_limit() {
        // Model blijft eindeloos tools aanroepen → de lus kapt af.
        let mut llm = ScriptedLlm {
            script: vec![tool_call("fs_read", "path", "a"); 20],
            idx: 0,
        };
        let mut gw = McpGateway::new();
        let mut tools = EchoTools;
        let run = run("assistent", AgentCaps(FS_READ), &mut llm, &mut gw, &mut tools, vec![Message::user("loop")], 3);
        assert!(run.truncated);
        assert_eq!(run.tool_calls, 3);
        assert!(run.answer.is_empty());
    }
}
