//! LLM backend (Sprint AA, step 5) — the sovereign default is **local**.
//!
//! An agent talks to a language model via the [`LlmBackend`] trait. The default
//! implementation speaks the **Ollama-compatible** `/api/chat` protocol (JSON over
//! HTTP to `localhost:11434`) — fully local, no cloud. A cloud backend is
//! opt-in per user (key via EuroVault, every call → P3). This module contains the
//! trait + the host-tested request/response (de)serialization; the real HTTP transport
//! is wired up by the kernel/userspace daemon via EuroNet.

use crate::json::Json;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// The role of a message in the conversation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    System,
    User,
    Assistant,
    /// The result of a tool call, fed back to the model.
    Tool,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// A single message in the conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Message { role, content: content.into() }
    }
    pub fn system(c: impl Into<String>) -> Self {
        Message::new(Role::System, c)
    }
    pub fn user(c: impl Into<String>) -> Self {
        Message::new(Role::User, c)
    }
}

/// What the model returns: either a final answer, or a tool call.
#[derive(Clone, Debug, PartialEq)]
pub enum LlmResponse {
    /// The model gives a final answer (no more tools needed).
    Text(String),
    /// The model wants to call an MCP tool with these arguments.
    ToolCall { name: String, arguments: Json },
}

/// A language-model backend (local or cloud — transparent to the agent).
pub trait LlmBackend {
    /// Execute one conversation step: given the history + the available
    /// tool names, return the model's next response.
    fn step(&mut self, messages: &[Message], tools: &[&str]) -> LlmResponse;
}

/// Build an Ollama `/api/chat` request body (JSON) for a local model.
/// `tools` is passed as system context (function names) — models without
/// a native tool API are still given them this way.
pub fn ollama_request(model: &str, messages: &[Message], tools: &[&str], stream: bool) -> String {
    let msgs: Vec<Json> = messages
        .iter()
        .map(|m| {
            Json::Obj(vec![
                ("role".to_string(), Json::Str(m.role.as_str().to_string())),
                ("content".to_string(), Json::Str(m.content.clone())),
            ])
        })
        .collect();
    let tool_arr: Vec<Json> = tools.iter().map(|t| Json::Str(t.to_string())).collect();
    Json::Obj(vec![
        ("model".to_string(), Json::Str(model.to_string())),
        ("messages".to_string(), Json::Arr(msgs)),
        ("tools".to_string(), Json::Arr(tool_arr)),
        ("stream".to_string(), Json::Bool(stream)),
    ])
    .to_string()
}

/// Parse an Ollama `/api/chat` response body into an [`LlmResponse`].
///
/// Ollama returns `{"message":{"role":"assistant","content":"...","tool_calls":[
/// {"function":{"name":"fs_read","arguments":{...}}}]}}`. If there is a `tool_calls`,
/// then it is an [`LlmResponse::ToolCall`]; otherwise the text `content`.
pub fn parse_ollama_response(body: &str) -> Result<LlmResponse, &'static str> {
    let v = Json::parse(body).map_err(|_| "json")?;
    let msg = v.get("message").ok_or("no message")?;
    if let Some(Json::Arr(calls)) = msg.get("tool_calls") {
        if let Some(first) = calls.first() {
            let f = first.get("function").ok_or("no function")?;
            let name = f.get("name").and_then(|n| n.as_str()).ok_or("no name")?;
            let args = f.get("arguments").cloned().unwrap_or(Json::Obj(Vec::new()));
            return Ok(LlmResponse::ToolCall { name: name.to_string(), arguments: args });
        }
    }
    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
    Ok(LlmResponse::Text(content.to_string()))
}

/// Build a full **HTTP/1.1 POST `/api/chat`** request to a local Ollama
/// (`host` e.g. `"localhost:11434"`). The kernel only needs to send these bytes over an
/// EuroNet TCP socket; the protocol is host-tested here.
pub fn ollama_http_request(host: &str, model: &str, messages: &[Message], tools: &[&str]) -> alloc::vec::Vec<u8> {
    let body = ollama_request(model, messages, tools, false);
    let mut req = String::new();
    req.push_str("POST /api/chat HTTP/1.1\r\n");
    req.push_str("Host: ");
    req.push_str(host);
    req.push_str("\r\n");
    req.push_str("Content-Type: application/json\r\n");
    req.push_str("Connection: close\r\n");
    req.push_str("Content-Length: ");
    req.push_str(&body.len().to_string());
    req.push_str("\r\n\r\n");
    req.push_str(&body);
    req.into_bytes()
}

/// Parse a raw HTTP response (headers + body) from Ollama into an [`LlmResponse`].
/// Splits on the blank line between headers and body and parses the JSON body.
pub fn parse_http_response(raw: &[u8]) -> Result<LlmResponse, &'static str> {
    // Find the end of the headers (`\r\n\r\n`).
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no header end")?;
    let body = &raw[split + 4..];
    let text = core::str::from_utf8(body).map_err(|_| "body not utf-8")?;
    parse_ollama_response(text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_request_framing() {
        let req = ollama_http_request("localhost:11434", "mistral:7b", &[Message::user("hi")], &["fs_read"]);
        let s = alloc::string::String::from_utf8(req).unwrap();
        assert!(s.starts_with("POST /api/chat HTTP/1.1\r\n"));
        assert!(s.contains("Host: localhost:11434\r\n"));
        assert!(s.contains("Content-Type: application/json\r\n"));
        // Content-Length must be the real body length.
        let body = s.split("\r\n\r\n").nth(1).unwrap();
        assert!(s.contains(&alloc::format!("Content-Length: {}\r\n", body.len())));
        assert!(body.contains("\"model\":\"mistral:7b\""));
    }

    #[test]
    fn http_response_parsing() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 48\r\n\r\n{\"message\":{\"role\":\"assistant\",\"content\":\"Done.\"}}";
        assert_eq!(parse_http_response(raw).unwrap(), LlmResponse::Text("Done.".to_string()));
    }

    #[test]
    fn request_shape() {
        let req = ollama_request(
            "mistral:7b-instruct",
            &[Message::system("you are an assistant"), Message::user("hi")],
            &["fs_read", "fs_write"],
            false,
        );
        let v = Json::parse(&req).unwrap();
        assert_eq!(v.get("model").unwrap().as_str(), Some("mistral:7b-instruct"));
        assert_eq!(v.get("stream").unwrap().as_bool(), Some(false));
        if let Json::Arr(m) = v.get("messages").unwrap() {
            assert_eq!(m.len(), 2);
            assert_eq!(m[0].get("role").unwrap().as_str(), Some("system"));
            assert_eq!(m[1].get("content").unwrap().as_str(), Some("hi"));
        } else {
            panic!();
        }
    }

    #[test]
    fn parse_text_response() {
        let body = r#"{"message":{"role":"assistant","content":"Done."}}"#;
        assert_eq!(parse_ollama_response(body).unwrap(), LlmResponse::Text("Done.".to_string()));
    }

    #[test]
    fn parse_tool_call() {
        let body = r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"fs_read","arguments":{"path":"notes.txt"}}}]}}"#;
        match parse_ollama_response(body).unwrap() {
            LlmResponse::ToolCall { name, arguments } => {
                assert_eq!(name, "fs_read");
                assert_eq!(arguments.get("path").unwrap().as_str(), Some("notes.txt"));
            }
            _ => panic!("expected tool call"),
        }
    }
}
