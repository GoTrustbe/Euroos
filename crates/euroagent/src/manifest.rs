//! `AgentManifest` — declarative, Ed25519-signable description of an agent.
//!
//! An agent bundle (`*.euroa`) ships a TOML manifest that *fully* states
//! what the agent is and may do. The runtime never grants more than what is
//! declared here. This module contains a purpose-built TOML-subset parser
//! (sections, `key = value` with string/int/bool/array-of-strings, `#`
//! comments) — enough for the manifest and fully `no_std` + host-tested,
//! without an external TOML crate.

use crate::caps::{self, AgentCaps};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A parsed, validated agent manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub wasm: String,
    pub lang: String,
    /// Caps the agent strictly needs (granted at install time).
    pub required: AgentCaps,
    /// Caps that are optional (granted only after an explicit user grant).
    pub optional: AgentCaps,
    pub triggers_intent: Vec<String>,
    pub triggers_event: Vec<String>,
    pub tools_allowed: Vec<String>,
    pub tools_denied: Vec<String>,
    pub max_memory_mb: u64,
    pub max_runtime_ms: u64,
    pub network_domains: Vec<String>,
    pub log_tool_calls: bool,
    pub log_inputs: bool,
}

/// An error while parsing or validating a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// A required field is missing.
    MissingField(&'static str),
    /// A declared capability does not exist.
    UnknownCap(String),
    /// Syntax error on the given (1-based) line.
    Syntax(usize),
}

impl ManifestError {
    pub fn describe(&self) -> String {
        match self {
            ManifestError::MissingField(f) => {
                let mut s = String::from("required field missing: ");
                s.push_str(f);
                s
            }
            ManifestError::UnknownCap(c) => {
                let mut s = String::from("unknown capability: ");
                s.push_str(c);
                s
            }
            ManifestError::Syntax(line) => {
                let mut s = String::from("syntax error on line ");
                s.push_str(&line.to_string());
                s
            }
        }
    }
}

/// A single value from the TOML document.
enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Arr(Vec<String>),
}

/// Flat key→value map with `section.key` keys.
struct Doc {
    entries: Vec<(String, Value)>,
}

impl Doc {
    fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    fn str(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }
    fn int(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Value::Int(i)) => Some(*i),
            _ => None,
        }
    }
    fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    fn arr(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Value::Arr(a)) => a.clone(),
            _ => Vec::new(),
        }
    }
}

/// Strip `#` comments outside of string literals.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_str = !in_str,
            b'#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Parse a single scalar (`"str"`, int, true/false).
fn parse_scalar(s: &str) -> Option<Value> {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return Some(Value::Str(s[1..s.len() - 1].to_string()));
    }
    if s == "true" {
        return Some(Value::Bool(true));
    }
    if s == "false" {
        return Some(Value::Bool(false));
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(Value::Int(i));
    }
    None
}

/// Split the inside of an array on commas and take the string elements.
fn parse_array_body(body: &str, out: &mut Vec<String>) {
    for part in body.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if p.len() >= 2 && p.starts_with('"') && p.ends_with('"') {
            out.push(p[1..p.len() - 1].to_string());
        }
    }
}

/// Parse the TOML subset into a flat `Doc`.
fn parse_doc(input: &str) -> Result<Doc, ManifestError> {
    let mut entries: Vec<(String, Value)> = Vec::new();
    let mut section = String::new();
    let mut lines = input.lines().enumerate();
    while let Some((idx, raw)) = lines.next() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let end = line.find(']').ok_or(ManifestError::Syntax(idx + 1))?;
            section = line[1..end].trim().to_string();
            continue;
        }
        let eq = line.find('=').ok_or(ManifestError::Syntax(idx + 1))?;
        let key = line[..eq].trim();
        let mut val = line[eq + 1..].trim().to_string();
        let full_key = if section.is_empty() {
            key.to_string()
        } else {
            let mut k = section.clone();
            k.push('.');
            k.push_str(key);
            k
        };

        if val.starts_with('[') {
            // Possibly a multi-line array: read on until the closing ']'.
            while !val.contains(']') {
                match lines.next() {
                    Some((_, more)) => {
                        val.push(' ');
                        val.push_str(strip_comment(more).trim());
                    }
                    None => return Err(ManifestError::Syntax(idx + 1)),
                }
            }
            let open = val.find('[').unwrap();
            let close = val.rfind(']').unwrap();
            let mut arr = Vec::new();
            parse_array_body(&val[open + 1..close], &mut arr);
            entries.push((full_key, Value::Arr(arr)));
        } else {
            let scalar = parse_scalar(&val).ok_or(ManifestError::Syntax(idx + 1))?;
            entries.push((full_key, scalar));
        }
    }
    Ok(Doc { entries })
}

/// Convert a list of cap names into an `AgentCaps`, or fail on an unknown one.
fn caps_from(names: &[String]) -> Result<AgentCaps, ManifestError> {
    let mut c = AgentCaps::empty();
    for n in names {
        match caps::from_name(n) {
            Some(b) => c.insert(b),
            None => return Err(ManifestError::UnknownCap(n.clone())),
        }
    }
    Ok(c)
}

impl AgentManifest {
    /// Parse + validate a manifest from a TOML string.
    pub fn from_toml(input: &str) -> Result<AgentManifest, ManifestError> {
        let doc = parse_doc(input)?;

        let name = doc.str("agent.name").ok_or(ManifestError::MissingField("agent.name"))?;
        let version = doc.str("agent.version").ok_or(ManifestError::MissingField("agent.version"))?;
        let wasm = doc.str("agent.wasm").ok_or(ManifestError::MissingField("agent.wasm"))?;

        let required = caps_from(&doc.arr("capabilities.required"))?;
        let optional = caps_from(&doc.arr("capabilities.optional"))?;

        Ok(AgentManifest {
            name,
            version,
            description: doc.str("agent.description").unwrap_or_default(),
            author: doc.str("agent.author").unwrap_or_default(),
            wasm,
            lang: doc.str("agent.lang").unwrap_or_else(|| "en".to_string()),
            required,
            optional,
            triggers_intent: doc.arr("triggers.on_intent"),
            triggers_event: doc.arr("triggers.on_event"),
            tools_allowed: doc.arr("tools.allowed"),
            tools_denied: doc.arr("tools.denied"),
            max_memory_mb: doc.int("sandbox.max_memory_mb").unwrap_or(64).max(0) as u64,
            max_runtime_ms: doc.int("sandbox.max_runtime_ms").unwrap_or(0).max(0) as u64,
            network_domains: doc.arr("sandbox.network_domains"),
            log_tool_calls: doc.bool("audit.log_tool_calls").unwrap_or(true),
            log_inputs: doc.bool("audit.log_inputs").unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACILITATOR: &str = r#"
[agent]
name        = "facilitator"
version     = "1.0.0"
description = "Record, transcribe and summarize meetings"
author      = "GoTrust BV <agent@gotrust.eu>"
wasm        = "facilitator.wasm"
lang        = "nl-BE"

[capabilities]
required = [
    "CAP_AGENT_MIC",          # Microphone
    "CAP_AGENT_FS_WRITE",     # Save transcripts
    "CAP_AGENT_DISPLAY",
]
optional = ["CAP_AGENT_NET", "CAP_AGENT_CALENDAR"]

[triggers]
on_event  = ["calendar.meeting_start", "user.mic_hotkey"]
on_intent = ["record meeting", "start recording"]

[tools]
allowed = ["mic_record", "fs_write", "display_notify"]
denied  = ["exec", "vault_read"]

[sandbox]
max_memory_mb   = 64
max_runtime_ms  = 0
network_domains = []

[audit]
log_tool_calls = true
log_inputs     = false
"#;

    #[test]
    fn parse_valid() {
        let m = AgentManifest::from_toml(FACILITATOR).unwrap();
        assert_eq!(m.name, "facilitator");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.lang, "nl-BE");
        assert!(m.required.contains(caps::MIC));
        assert!(m.required.contains(caps::FS_WRITE));
        assert!(m.required.contains(caps::DISPLAY));
        assert!(m.optional.contains(caps::NET_GET));
        assert!(m.optional.contains(caps::CALENDAR));
        assert_eq!(m.triggers_intent.len(), 2);
        assert_eq!(m.tools_allowed.len(), 3);
        assert_eq!(m.max_memory_mb, 64);
        assert!(m.log_tool_calls);
        assert!(!m.log_inputs);
    }

    #[test]
    fn rejects_unknown_cap() {
        let bad = "[agent]\nname=\"x\"\nversion=\"1\"\nwasm=\"x.wasm\"\n[capabilities]\nrequired=[\"CAP_AGENT_KERNEL_PANIC\"]\n";
        assert_eq!(
            AgentManifest::from_toml(bad),
            Err(ManifestError::UnknownCap("CAP_AGENT_KERNEL_PANIC".into()))
        );
    }

    #[test]
    fn rejects_missing_required() {
        let incomplete = "[agent]\nname = \"test\"\n";
        assert_eq!(
            AgentManifest::from_toml(incomplete),
            Err(ManifestError::MissingField("agent.version"))
        );
    }

    #[test]
    fn comment_in_string_preserved() {
        let m = AgentManifest::from_toml("[agent]\nname=\"a#b\"\nversion=\"1\"\nwasm=\"w\"\n").unwrap();
        assert_eq!(m.name, "a#b");
    }
}
