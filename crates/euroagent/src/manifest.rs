//! `AgentManifest` — declaratieve, Ed25519-tekenbare beschrijving van een agent.
//!
//! Een agent-bundle (`*.euroa`) levert een TOML-manifest mee dat *volledig* zegt
//! wat de agent is en mag. De runtime verleent nooit meer dan hier staat. Dit
//! module bevat een doelgerichte TOML-subset-parser (secties, `key = value` met
//! string/int/bool/array-van-strings, `#`-commentaar) — genoeg voor het manifest
//! en volledig `no_std` + host-getest, zonder een externe TOML-crate.

use crate::caps::{self, AgentCaps};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Een geparseerd, gevalideerd agent-manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub wasm: String,
    pub lang: String,
    /// Caps die de agent strikt nodig heeft (verleend bij installatie).
    pub required: AgentCaps,
    /// Caps die optioneel zijn (pas verleend na expliciete user-grant).
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

/// Een fout bij het parsen of valideren van een manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// Een verplicht veld ontbreekt.
    MissingField(&'static str),
    /// Een gedeclareerde capability bestaat niet.
    UnknownCap(String),
    /// Syntaxfout op de gegeven (1-gebaseerde) regel.
    Syntax(usize),
}

impl ManifestError {
    pub fn describe(&self) -> String {
        match self {
            ManifestError::MissingField(f) => {
                let mut s = String::from("verplicht veld ontbreekt: ");
                s.push_str(f);
                s
            }
            ManifestError::UnknownCap(c) => {
                let mut s = String::from("onbekende capability: ");
                s.push_str(c);
                s
            }
            ManifestError::Syntax(line) => {
                let mut s = String::from("syntaxfout op regel ");
                s.push_str(&line.to_string());
                s
            }
        }
    }
}

/// Eén waarde uit het TOML-document.
enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Arr(Vec<String>),
}

/// Platte sleutel→waarde map met `sectie.key`-sleutels.
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

/// Strip `#`-commentaar buiten string-literals.
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

/// Parse één scalar (`"str"`, int, true/false).
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

/// Splits de binnenkant van een array op komma's en pak de string-elementen.
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

/// Parse de TOML-subset naar een platte `Doc`.
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
            // Mogelijk multi-line array: lees door tot de afsluitende ']'.
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

/// Zet een lijst cap-namen om naar een `AgentCaps`, of faal op onbekende.
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
    /// Parse + valideer een manifest uit een TOML-string.
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
description = "Vergaderingen opnemen, transcriberen en samenvatten"
author      = "GoTrust BV <agent@gotrust.eu>"
wasm        = "facilitator.wasm"
lang        = "nl-BE"

[capabilities]
required = [
    "CAP_AGENT_MIC",          # Microfoon
    "CAP_AGENT_FS_WRITE",     # Transcripties opslaan
    "CAP_AGENT_DISPLAY",
]
optional = ["CAP_AGENT_NET", "CAP_AGENT_CALENDAR"]

[triggers]
on_event  = ["calendar.meeting_start", "user.mic_hotkey"]
on_intent = ["vergadering opnemen", "start recording"]

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
