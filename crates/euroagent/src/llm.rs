//! LLM-backend (Sprint AA, stap 5) — de soevereine standaard is **lokaal**.
//!
//! Een agent praat met een taalmodel via de [`LlmBackend`]-trait. De standaard-
//! implementatie spreekt het **Ollama-compatibele** `/api/chat`-protocol (JSON over
//! HTTP naar `localhost:11434`) — volledig lokaal, geen cloud. Een cloud-backend is
//! opt-in per gebruiker (sleutel via EuroVault, elke call → P3). Dit module bevat de
//! trait + de host-geteste request/response-(de)serialisatie; de echte HTTP-transport
//! koppelt de kernel/userspace-daemon via EuroNet.

use crate::json::Json;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// De rol van een bericht in de conversatie.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    System,
    User,
    Assistant,
    /// Het resultaat van een tool-aanroep, teruggevoerd naar het model.
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

/// Eén bericht in de conversatie.
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

/// Wat het model terugkomt: ofwel een eindantwoord, ofwel een tool-aanroep.
#[derive(Clone, Debug, PartialEq)]
pub enum LlmResponse {
    /// Het model geeft een eindantwoord (geen tool meer nodig).
    Text(String),
    /// Het model wil een MCP-tool aanroepen met deze argumenten.
    ToolCall { name: String, arguments: Json },
}

/// Een taalmodel-backend (lokaal of cloud — transparant voor de agent).
pub trait LlmBackend {
    /// Voer een conversatie-stap uit: gegeven de geschiedenis + de beschikbare
    /// tool-namen, geef het volgende antwoord van het model.
    fn step(&mut self, messages: &[Message], tools: &[&str]) -> LlmResponse;
}

/// Bouw een Ollama-`/api/chat`-request-body (JSON) voor een lokaal model.
/// `tools` wordt als systeem-context meegegeven (functie-namen) — modellen zonder
/// native tool-API krijgen ze zo toch aangereikt.
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

/// Parse een Ollama-`/api/chat`-response-body naar een [`LlmResponse`].
///
/// Ollama geeft `{"message":{"role":"assistant","content":"...","tool_calls":[
/// {"function":{"name":"fs_read","arguments":{...}}}]}}`. Is er een `tool_calls`,
/// dan is het een [`LlmResponse::ToolCall`]; anders de tekst-`content`.
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

/// Bouw een volledige **HTTP/1.1 POST `/api/chat`**-request naar een lokale Ollama
/// (`host` bv. `"localhost:11434"`). De kernel hoeft enkel deze bytes over een
/// EuroNet-TCP-socket te sturen; het protocol is hier host-getest.
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

/// Parse een rauwe HTTP-respons (headers + body) van Ollama naar een [`LlmResponse`].
/// Splitst op de lege regel tussen headers en body en parseert de JSON-body.
pub fn parse_http_response(raw: &[u8]) -> Result<LlmResponse, &'static str> {
    // Vind het einde van de headers (`\r\n\r\n`).
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("geen header-einde")?;
    let body = &raw[split + 4..];
    let text = core::str::from_utf8(body).map_err(|_| "body niet-utf8")?;
    parse_ollama_response(text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_request_framing() {
        let req = ollama_http_request("localhost:11434", "mistral:7b", &[Message::user("hoi")], &["fs_read"]);
        let s = alloc::string::String::from_utf8(req).unwrap();
        assert!(s.starts_with("POST /api/chat HTTP/1.1\r\n"));
        assert!(s.contains("Host: localhost:11434\r\n"));
        assert!(s.contains("Content-Type: application/json\r\n"));
        // Content-Length moet de echte body-lengte zijn.
        let body = s.split("\r\n\r\n").nth(1).unwrap();
        assert!(s.contains(&alloc::format!("Content-Length: {}\r\n", body.len())));
        assert!(body.contains("\"model\":\"mistral:7b\""));
    }

    #[test]
    fn http_response_parsing() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 48\r\n\r\n{\"message\":{\"role\":\"assistant\",\"content\":\"Klaar.\"}}";
        assert_eq!(parse_http_response(raw).unwrap(), LlmResponse::Text("Klaar.".to_string()));
    }

    #[test]
    fn request_shape() {
        let req = ollama_request(
            "mistral:7b-instruct",
            &[Message::system("je bent een assistent"), Message::user("hoi")],
            &["fs_read", "fs_write"],
            false,
        );
        let v = Json::parse(&req).unwrap();
        assert_eq!(v.get("model").unwrap().as_str(), Some("mistral:7b-instruct"));
        assert_eq!(v.get("stream").unwrap().as_bool(), Some(false));
        if let Json::Arr(m) = v.get("messages").unwrap() {
            assert_eq!(m.len(), 2);
            assert_eq!(m[0].get("role").unwrap().as_str(), Some("system"));
            assert_eq!(m[1].get("content").unwrap().as_str(), Some("hoi"));
        } else {
            panic!();
        }
    }

    #[test]
    fn parse_text_response() {
        let body = r#"{"message":{"role":"assistant","content":"Klaar."}}"#;
        assert_eq!(parse_ollama_response(body).unwrap(), LlmResponse::Text("Klaar.".to_string()));
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
