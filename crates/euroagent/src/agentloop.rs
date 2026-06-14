//! The agent execution loop (Sprint AA, step 5) — the heart of the runtime.
//!
//! Deterministic and auditable: given a user intent the loop runs
//! `model → (tool-call → MCP-gateway → result → model)* → final answer`. Each
//! tool call passes through the [`McpGateway`] (capability gate + P3 audit), so the
//! loop can never let an agent do more than its `AgentCaps` allow. The LLM
//! sits behind the [`LlmBackend`] trait — host-tested with a scripted mock model.

use crate::caps::AgentCaps;
use crate::json::Json;
use crate::llm::{LlmBackend, LlmResponse, Message, Role};
use crate::mcp::{McpGateway, ToolBackend};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// The result of a full agent run.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRun {
    /// The model's final answer.
    pub answer: String,
    /// The number of tool calls made along the way.
    pub tool_calls: usize,
    /// How many of those were denied by the capability gate.
    pub denied: usize,
    /// Did the loop reach the step limit without a final answer?
    pub truncated: bool,
    /// Per-step transcript (for the live audit view in the dispatch GUI):
    /// each tool call + whether the capability gate allowed or denied it.
    pub log: Vec<String>,
}

/// Run the agent loop until a final answer or until `max_steps` is reached.
///
/// - `name`     — agent identity (for the audit trail);
/// - `caps`     — the effective capability set (gate for each tool);
/// - `llm`      — the language model;
/// - `gateway`  — the MCP gateway (cap gate + audit);
/// - `tools`    — the backend that executes authorized tools;
/// - `messages` — the starting conversation (system + user intent).
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
                // Execute the tool via the gateway (cap gate + audit) as JSON-RPC.
                let req = jsonrpc_call(&tool, &arguments, tool_calls as i64);
                let raw = gateway.handle(name, caps, &req, tools);
                let (content, was_denied) = summarize(&raw);
                if was_denied {
                    denied += 1;
                    log.push(alloc::format!("tool {tool} → DENIED by the capability gate (permission required)"));
                } else {
                    log.push(alloc::format!("tool {tool} → allowed, executed, audited"));
                }
                // Feed the result back into the model.
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

/// The names of the tools this cap set is allowed to call.
fn gateway_tool_names(gateway: &McpGateway, caps: AgentCaps) -> Vec<&'static str> {
    // `list_for` returns exactly the visible tools; extract the names.
    match gateway.list_for(caps).get("tools") {
        Some(Json::Arr(items)) => items
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .map(static_name)
            .collect(),
        _ => Vec::new(),
    }
}

/// Map a tool name to its `'static` variant (the gateway tools are fixed).
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

/// Summarize a JSON-RPC response into text for the model + whether it was a
/// capability denial.
fn summarize(raw: &str) -> (String, bool) {
    match Json::parse(raw) {
        Ok(v) => {
            if let Some(err) = v.get("error") {
                let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("error");
                (
                    alloc::format!("ERROR: {msg}"),
                    code == crate::mcp::ERR_CAP_DENIED,
                )
            } else if let Some(res) = v.get("result") {
                (res.to_string(), false)
            } else {
                (String::from("(empty)"), false)
            }
        }
        Err(_) => (String::from("(unreadable response)"), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::*;

    /// A scripted mock model: walks through a fixed list of answers.
    struct ScriptedLlm {
        script: Vec<LlmResponse>,
        idx: usize,
    }
    impl LlmBackend for ScriptedLlm {
        fn step(&mut self, _m: &[Message], _t: &[&str]) -> LlmResponse {
            let r = self.script.get(self.idx).cloned().unwrap_or(LlmResponse::Text("(end)".into()));
            self.idx += 1;
            r
        }
    }

    /// Tool backend that returns a fixed result.
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
        // Model: call fs_read, then give a final answer.
        let mut llm = ScriptedLlm {
            script: vec![
                tool_call("fs_read", "path", "notes.txt"),
                LlmResponse::Text("The note says: hello.".to_string()),
            ],
            idx: 0,
        };
        let mut gw = McpGateway::new();
        let mut tools = EchoTools;
        let caps = AgentCaps(FS_READ);
        let run = run(
            "assistant",
            caps,
            &mut llm,
            &mut gw,
            &mut tools,
            vec![Message::user("what is in notes.txt?")],
            8,
        );
        assert_eq!(run.answer, "The note says: hello.");
        assert_eq!(run.tool_calls, 1);
        assert_eq!(run.denied, 0);
        assert!(!run.truncated);
        // The tool call is audited in the gateway.
        assert_eq!(gw.audit.len(), 1);
        assert!(gw.audit[0].allowed);
    }

    #[test]
    fn capability_denied_is_recorded() {
        // Model tries exec without the EXEC cap → denied, then gives up.
        let mut llm = ScriptedLlm {
            script: vec![
                tool_call("exec", "cmd", "rm -rf /"),
                LlmResponse::Text("Not allowed, I'll stop.".to_string()),
            ],
            idx: 0,
        };
        let mut gw = McpGateway::new();
        let mut tools = EchoTools;
        let caps = AgentCaps(FS_READ); // NO exec
        let run = run("assistant", caps, &mut llm, &mut gw, &mut tools, vec![Message::user("delete everything")], 8);
        assert_eq!(run.tool_calls, 1);
        assert_eq!(run.denied, 1);
        assert_eq!(run.answer, "Not allowed, I'll stop.");
        assert!(!gw.audit[0].allowed);
    }

    #[test]
    fn loop_truncates_at_step_limit() {
        // Model keeps calling tools forever → the loop truncates.
        let mut llm = ScriptedLlm {
            script: vec![tool_call("fs_read", "path", "a"); 20],
            idx: 0,
        };
        let mut gw = McpGateway::new();
        let mut tools = EchoTools;
        let run = run("assistant", AgentCaps(FS_READ), &mut llm, &mut gw, &mut tools, vec![Message::user("loop")], 3);
        assert!(run.truncated);
        assert_eq!(run.tool_calls, 3);
        assert!(run.answer.is_empty());
    }
}
