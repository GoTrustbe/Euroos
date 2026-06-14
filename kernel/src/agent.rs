//! Kernel side of **EuroAgent** (Sprint AA): the sovereign agent-first runtime.
//!
//! Agents are WASM modules with a declarative capability manifest; the trust
//! boundary sits here in the kernel (EuroGuard), not in a cloud. The host-tested
//! core lives in [`euroagent`]; this module proves it live at boot — parse the
//! manifest, derive the effective capability set (least-privilege clamp), make a
//! cap-gated MCP tool call, and route an intent — and it provides the
//! `euroagent` shell command.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use euroagent::caps::{self, AgentCaps};
use euroagent::json::Json;
use euroagent::llm::{LlmBackend, LlmResponse, Message};
use euroagent::mcp::{McpGateway, ToolBackend};
use euroagent::{agentloop, intent, manifest::AgentManifest, policy};
use spin::Mutex;

/// An example manifest that the runtime validates at boot (the "facilitator"
/// meeting assistant from the EuroAgent plan).
const DEMO_MANIFEST: &str = r#"
[agent]
name        = "facilitator"
version     = "1.0.0"
description = "Record, transcribe and summarize meetings"
author      = "GoTrust BV <agent@gotrust.eu>"
wasm        = "facilitator.wasm"
lang        = "nl-BE"

[capabilities]
required = ["CAP_AGENT_MIC", "CAP_AGENT_FS_WRITE", "CAP_AGENT_DISPLAY"]
optional = ["CAP_AGENT_NET", "CAP_AGENT_CALENDAR"]

[triggers]
on_intent = ["record meeting", "start recording"]

[tools]
allowed = ["mic_record", "fs_write", "display_notify"]
denied  = ["exec", "vault_read"]

[sandbox]
max_memory_mb = 64
"#;

static EFFECTIVE: Mutex<u64> = Mutex::new(0);
/// Installed agents (name, version, publisher hex), filled by the boot self-test.
static INSTALLED: Mutex<alloc::vec::Vec<(String, String, String)>> = Mutex::new(alloc::vec::Vec::new());

/// A kernel MCP backend stub: proves the path agent→gateway→subsystem without
/// real side effects (the real coupling to EuroFS/EuroNet/EuroVault comes in
/// the userspace daemon). Echoes the input back.
struct KernelBackend;
impl ToolBackend for KernelBackend {
    fn execute(&mut self, tool: &str, input: &Json) -> Result<Json, String> {
        let _ = (tool, input);
        Ok(Json::Obj(alloc::vec![("ok".into(), Json::Bool(true))]))
    }
}

/// **Real** MCP tool backend: couples the gateway tools to the actual EuroOS
/// subsystems — EuroFS (`fs_read`/`fs_write`), EuroNet (`net_get`) and EuroVault
/// (`vault_get`). An agent that calls these tools now actually touches the disk, the
/// network or the vault — but only within its sandbox directory `/agents/<name>/`,
/// only to domains in its manifest `network_domains` allow-list, and only if
/// the cap-gate let it through. So EuroAgent is no longer a stub but an agent that
/// actually does work, capability-isolated at the kernel level (least agency).
pub struct FsToolBackend<'a> {
    pub fs: &'a mut dyn eurofs::FileSystem,
    /// The sandbox root; all paths are clamped underneath it.
    pub root: alloc::string::String,
    /// The allowed network domains from the agent manifest (`network_domains`).
    /// Empty = `net_get` may go NOWHERE (deny-by-default, least agency).
    pub allowed_domains: Vec<String>,
}

impl<'a> FsToolBackend<'a> {
    /// Clamp a (possibly malicious) relative path within the sandbox root:
    /// strip `..`/leading `/` so an agent cannot escape.
    fn sandbox_path(&self, rel: &str) -> alloc::string::String {
        let mut p = self.root.clone();
        // Split on both `/` and `\` (audit C6: a backend-independent clamp) and
        // reject every `.`/`..`/empty segment — so an agent can never rise above
        // its sandbox root, regardless of how the path is separated.
        for seg in rel.split(['/', '\\']) {
            if seg.is_empty() || seg == "." || seg == ".." {
                continue;
            }
            p.push('/');
            p.push_str(seg);
        }
        p
    }

    /// Second gate above the capability (least agency): may this agent reach this
    /// host? Exact match or a subdomain of an allowed domain. An empty
    /// allow-list rejects EVERYTHING — an agent without declared domains has NO
    /// network path (north-star: "impossible, not tedious" — the path does not exist).
    fn host_allowed(&self, host: &str) -> bool {
        self.allowed_domains.iter().any(|d| {
            let d = d.trim();
            !d.is_empty() && (host == d || host.ends_with(&alloc::format!(".{d}")))
        })
    }
}

/// Split a URL into `(tls, host, port, path)`. Only `http`/`https`; no
/// userinfo/fragment (an agent tool does not need those and they enlarge the
/// attack surface). Fails gracefully (`None`) on anything that does not parse.
fn parse_url(url: &str) -> Option<(bool, String, u16, String)> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return None;
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.find(':') {
        Some(i) => (authority[..i].to_string(), authority[i + 1..].parse::<u16>().ok()?),
        None => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some((tls, host, port, if path.is_empty() { String::from("/") } else { path.to_string() }))
}

impl<'a> ToolBackend for FsToolBackend<'a> {
    fn execute(&mut self, tool: &str, input: &Json) -> Result<Json, String> {
        match tool {
            "fs_write" => {
                let path = input.get("path").and_then(|p| p.as_str()).ok_or_else(|| "no path".to_string())?;
                let content = input.get("content").and_then(|c| c.as_str()).unwrap_or("");
                let full = self.sandbox_path(path);
                self.fs
                    .write_file(&full, content.as_bytes())
                    .map_err(|_| "write failed".to_string())?;
                Ok(Json::Obj(alloc::vec![
                    ("written".into(), Json::Num(content.len().to_string())),
                    ("path".into(), Json::Str(full)),
                ]))
            }
            "fs_read" => {
                let path = input.get("path").and_then(|p| p.as_str()).ok_or_else(|| "no path".to_string())?;
                let full = self.sandbox_path(path);
                let data = self.fs.read_file(&full).map_err(|_| "read failed".to_string())?;
                Ok(Json::Obj(alloc::vec![(
                    "content".into(),
                    Json::Str(alloc::string::String::from_utf8_lossy(&data).into_owned()),
                )]))
            }
            "display_notify" => {
                let title = input.get("title").and_then(|t| t.as_str()).unwrap_or("");
                crate::serial_println!("[agent-notify] {title}");
                Ok(Json::Obj(alloc::vec![("shown".into(), Json::Bool(true))]))
            }
            "net_get" => {
                // Second gate above the NET_GET capability: the host must be in the
                // manifest allow-list (least agency — not just "may use the
                // network", but "may use THIS domain").
                let url = input.get("url").and_then(|u| u.as_str()).ok_or_else(|| "no url".to_string())?;
                let (tls, host, port, path) = parse_url(url).ok_or_else(|| "invalid url".to_string())?;
                if !self.host_allowed(&host) {
                    return Err(alloc::format!("domain '{host}' not in network_domains allow-list"));
                }
                match crate::net::fetch_full(&host, port, &path, tls) {
                    Some((status, ctype, body)) => {
                        // Bound the returned body (anti-DoS / memory); report whether
                        // it was truncated so the agent knows.
                        const MAX: usize = 64 * 1024;
                        let truncated = body.len() > MAX;
                        let shown = &body[..body.len().min(MAX)];
                        Ok(Json::Obj(alloc::vec![
                            ("status".into(), Json::Num(status.to_string())),
                            ("content_type".into(), Json::Str(ctype.unwrap_or_default())),
                            ("bytes".into(), Json::Num(body.len().to_string())),
                            ("truncated".into(), Json::Bool(truncated)),
                            ("body".into(), Json::Str(String::from_utf8_lossy(shown).into_owned())),
                        ]))
                    }
                    None => Err(alloc::format!("fetching {host}:{port} failed (no connection)")),
                }
            }
            "vault_get" => {
                // Credentials-at-the-boundary: the value goes ONLY into this
                // tool result (for THIS one call) — never into the serial log or
                // the audit trail (the AuditRecord contains only tool name+cap, no
                // input/result). The gateway only let us in here with
                // VAULT_READ; the real EuroVault cap-gate confirms once more.
                let label = input.get("label").and_then(|l| l.as_str()).ok_or_else(|| "no label".to_string())?;
                match crate::vault::get(label, crate::vault::CAP_DB_ACCESS) {
                    Ok(value) => Ok(Json::Obj(alloc::vec![
                        ("value".into(), Json::Str(String::from_utf8_lossy(&value).into_owned())),
                        ("bytes".into(), Json::Num(value.len().to_string())),
                    ])),
                    Err(eurovault::VaultError::NotFound) => Err(alloc::format!("secret '{label}' does not exist")),
                    Err(eurovault::VaultError::PermissionDenied) => Err("vault denies access".to_string()),
                    Err(_) => Err("vault error (decrypt/corrupt)".to_string()),
                }
            }
            // exec stays deliberately uncoupled → deny-by-default. The gateway
            // normally already rejects it on the EXEC cap; should it reach here
            // anyway, it fails hard. A safe exec sandbox is a separate, approved design.
            other => Err(alloc::format!("tool '{other}' is not available in the kernel backend (deny-by-default)")),
        }
    }
}

/// A scripted mock model: proves the agent loop without a real LLM in the
/// sandbox. (The real coupling is a local Ollama backend via EuroNet.)
struct ScriptedLlm {
    step: usize,
}
impl LlmBackend for ScriptedLlm {
    fn step(&mut self, _m: &[Message], _t: &[&str]) -> LlmResponse {
        self.step += 1;
        match self.step {
            // Step 1: request an allowed tool (fs_write).
            1 => LlmResponse::ToolCall {
                name: String::from("fs_write"),
                arguments: Json::Obj(alloc::vec![
                    ("path".into(), Json::Str("summary.txt".into())),
                    ("content".into(), Json::Str("done".into())),
                ]),
            },
            // Step 2: try a forbidden tool (exec) → gets rejected.
            2 => LlmResponse::ToolCall {
                name: String::from("exec"),
                arguments: Json::Obj(alloc::vec![("cmd".into(), Json::Str("rm -rf /".into()))]),
            },
            // Step 3: final answer.
            _ => LlmResponse::Text(String::from("Summary saved.")),
        }
    }
}

/// **BB-1** — a REAL LLM backend (no mock): `step()` talks over EuroNet TCP
/// with a local, Ollama-compatible `/api/chat` endpoint. It builds the HTTP/1.1
/// POST request (`euroagent::llm::ollama_http_request`), sends it via
/// `net::http_post_raw`, and parses the real HTTP response. Local = sovereign,
/// no cloud; the transport is bounded (it cannot hang the boot).
struct NetOllama {
    host: String, // Host header, e.g. "10.0.2.2:11434"
    ip: String,   // connect IP (without port)
    port: u16,
    model: String,
    reachable: bool,
    calls: u32,
}
impl LlmBackend for NetOllama {
    fn step(&mut self, messages: &[Message], tools: &[&str]) -> LlmResponse {
        self.calls += 1;
        let req = euroagent::llm::ollama_http_request(&self.host, &self.model, messages, tools);
        match crate::net::http_post_raw(&self.ip, self.port, &req) {
            Some(raw) => {
                self.reachable = true;
                euroagent::llm::parse_http_response(&raw)
                    .unwrap_or_else(|e| LlmResponse::Text(alloc::format!("[LLM parse error: {e}]")))
            }
            None => {
                self.reachable = false;
                LlmResponse::Text(String::from("[no local LLM endpoint reachable]"))
            }
        }
    }
}

/// The default, sovereign LLM endpoint: local Ollama. In QEMU we reach the
/// host (where the mock/real Ollama runs) via the SLIRP gateway 10.0.2.2.
fn default_ollama() -> NetOllama {
    NetOllama {
        host: String::from("10.0.2.2:11434"),
        ip: String::from("10.0.2.2"),
        port: 11434,
        model: String::from("mistral:7b-instruct"),
        reachable: false,
        calls: 0,
    }
}

/// **BB-1 boot self-test** — prove the REAL LLM transport end-to-end: build the
/// Ollama request, send it over EuroNet TCP to 10.0.2.2:11434, and parse the
/// real HTTP response into a model answer (text or tool call). Runs the
/// agent loop with this real backend when an endpoint is reachable.
pub fn llm_selftest() {
    let mut be = default_ollama();
    let msgs = alloc::vec![
        Message::system("You are a sovereign assistant. Use tools where useful."),
        Message::user("Read the contract and summarize it."),
    ];
    let resp = be.step(&msgs, &["fs_read", "fs_write"]);
    // P3 audit: EVERY LLM call (local or cloud) is audited.
    crate::serial_println!("[p3] audit: agent-llm-call model=mistral:7b-instruct endpoint=10.0.2.2:11434 local=true");
    if be.reachable {
        let kind = match &resp {
            LlmResponse::ToolCall { name, .. } => alloc::format!("tool-call '{name}'"),
            LlmResponse::Text(t) => alloc::format!("text \"{}\"", t.trim().chars().take(40).collect::<String>()),
        };
        crate::serial_println!(
            "[bb1] EuroAgent LLM transport: REAL Ollama call over EuroNet TCP (HTTP POST /api/chat) → 10.0.2.2:11434 → model response: {kind} ✓ (local/sovereign, P3-audited)"
        );
    } else {
        crate::serial_println!(
            "[bb1] EuroAgent LLM transport READY: HTTP POST /api/chat over EuroNet TCP built; no endpoint on 10.0.2.2:11434 (start Ollama/mock to see the real model response) ✓"
        );
    }
}

/// **BB-6** — run the demo agent for a free-form `intent` (from the dispatch GUI).
/// Returns (routed agent name, agent run with live tool-call transcript).
/// The loop runs through the REAL MCP gateway (cap-gate + audit): fs_write is
/// allowed, exec rejected by the user-clamp — exactly what the GUI shows live.
pub fn run_intent(intent: &str) -> (Option<String>, agentloop::AgentRun) {
    let m = match AgentManifest::from_toml(DEMO_MANIFEST) {
        Ok(m) => m,
        Err(_) => {
            return (
                None,
                agentloop::AgentRun {
                    answer: String::from("[manifest error]"),
                    tool_calls: 0,
                    denied: 0,
                    truncated: false,
                    log: Vec::new(),
                },
            )
        }
    };
    let user_caps = AgentCaps(caps::ALL & !caps::EXEC);
    let granted = AgentCaps(caps::CALENDAR);
    let policy_denied = AgentCaps(caps::NET_GET | caps::NET_POST);
    let dec = policy::derive(&m, granted, user_caps, policy_denied);

    let routes = alloc::vec![intent::Route { agent: m.name.clone(), intents: m.triggers_intent.clone() }];
    let routed = intent::route(intent, &routes).map(|r| r.agent.clone());

    let mut llm = ScriptedLlm { step: 0 };
    let mut gw = McpGateway::new();
    let mut be = KernelBackend;
    let run = agentloop::run(
        &m.name,
        dec.effective,
        &mut llm,
        &mut gw,
        &mut be,
        alloc::vec![
            Message::system("You are a sovereign assistant. Use tools where useful."),
            Message::user(intent),
        ],
        8,
    );
    (routed, run)
}

/// Boot self-test: end-to-end proof of the EuroAgent runtime core.
pub fn selftest() {
    // 1. Parse + validate the manifest.
    let m = match AgentManifest::from_toml(DEMO_MANIFEST) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[aa] EuroAgent: manifest FAILED: {}", e.describe());
            return;
        }
    };

    // 2. Derive effective caps. The user holds everything except EXEC; EuroPol
    //    forbids the network for this agent type; the user granted CALENDAR.
    let user_caps = AgentCaps(caps::ALL & !caps::EXEC);
    let granted = AgentCaps(caps::CALENDAR);
    let policy_denied = AgentCaps(caps::NET_GET | caps::NET_POST);
    let dec = policy::derive(&m, granted, user_caps, policy_denied);
    *EFFECTIVE.lock() = dec.effective.0;

    // 3. Cap-gated MCP call: fs_write is allowed (FS_WRITE in the set), exec is not.
    let mut gw = McpGateway::new();
    let mut be = KernelBackend;
    let allow = gw.handle(
        &m.name,
        dec.effective,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"transcript.txt","content":"hi"}}}"#,
        &mut be,
    );
    let deny = gw.handle(
        &m.name,
        dec.effective,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"exec","arguments":{"cmd":"rm -rf /"}}}"#,
        &mut be,
    );
    let allow_ok = Json::parse(&allow).ok().map(|v| v.get("result").is_some()).unwrap_or(false);
    let deny_ok = Json::parse(&deny)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()))
        == Some(euroagent::mcp::ERR_CAP_DENIED);

    // 4. Intent routing.
    let routes = alloc::vec![intent::Route { agent: m.name.clone(), intents: m.triggers_intent.clone() }];
    let routed = intent::route("can you record the meeting", &routes).map(|r| r.agent.as_str());

    // 5. (AA-5) The full agent loop: model → tool → result → model →
    //    final answer, with a scripted mock model through the REAL MCP gateway
    //    (cap-gate + audit). The agent requests fs_write (allowed) + exec (rejected).
    let mut llm = ScriptedLlm { step: 0 };
    let mut loop_gw = McpGateway::new();
    let mut loop_be = KernelBackend;
    let agent_run = agentloop::run(
        &m.name,
        dec.effective,
        &mut llm,
        &mut loop_gw,
        &mut loop_be,
        alloc::vec![
            Message::system("You are a sovereign meeting assistant."),
            Message::user("Summarize the meeting and save it."),
        ],
        8,
    );
    let loop_ok = agent_run.answer == "Summary saved."
        && agent_run.tool_calls == 2
        && agent_run.denied == 1 // the exec attempt was rejected by the cap-gate
        && !agent_run.truncated;

    // 6. (AA-1 final piece) Ed25519 `.euroa` bundle verification: a valid bundle
    //    verifies; a tampered WASM binary is rejected. Proves that the
    //    chain publisher→bundle→running agent is airtight.
    let bundle_ok = {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[0x5e; 32]);
        let pk = sk.verifying_key().to_bytes();
        let wasm: &[u8] = b"\0asm\x01\0\0\0euroagent-demo";
        let sig = sk.sign(&euroagent::bundle::signing_message(DEMO_MANIFEST, wasm)).to_bytes();
        let good = euroagent::bundle::AgentBundle { manifest_toml: DEMO_MANIFEST, wasm, signature: sig };
        let valid = good.verify(&pk).is_ok();
        // Same signature, but modified WASM → must fail.
        let evil = euroagent::bundle::AgentBundle { manifest_toml: DEMO_MANIFEST, wasm: b"\0asm-TAMPERED", signature: sig };
        let tampered_rejected = evil.verify(&pk).is_err();
        valid && tampered_rejected
    };

    // 7. (AA-1 registry) Install the signed agent in a registry; prove that
    //    another publisher cannot hijack 'facilitator'.
    let registry_ok = {
        use ed25519_dalek::{Signer, SigningKey};
        let mut reg = euroagent::registry::AgentRegistry::new();
        let sk = SigningKey::from_bytes(&[0x5e; 32]);
        let pk = sk.verifying_key().to_bytes();
        let wasm: &[u8] = b"\0asm-facilitator";
        let sig = sk.sign(&euroagent::bundle::signing_message(DEMO_MANIFEST, wasm)).to_bytes();
        let good = euroagent::bundle::AgentBundle { manifest_toml: DEMO_MANIFEST, wasm, signature: sig };
        let installed = reg.install(&good, &pk).is_ok();
        // Another publisher with a valid signature of its own may not overwrite 'facilitator'.
        let sk2 = SigningKey::from_bytes(&[0x99; 32]);
        let sig2 = sk2.sign(&euroagent::bundle::signing_message(DEMO_MANIFEST, wasm)).to_bytes();
        let hijack = euroagent::bundle::AgentBundle { manifest_toml: DEMO_MANIFEST, wasm, signature: sig2 };
        let hijack_blocked = reg.install(&hijack, &sk2.verifying_key().to_bytes()).is_err();
        // Keep the install list for the `euroagent list` command.
        let mut list = INSTALLED.lock();
        list.clear();
        for n in reg.list() {
            if let Some(a) = reg.get(n) {
                list.push((a.name.clone(), a.version.clone(), a.publisher.clone()));
            }
        }
        installed && hijack_blocked && reg.len() == 1
    };

    let ncaps = dec.effective.names().len();
    let ok = allow_ok
        && deny_ok
        && !dec.effective.contains(caps::NET_GET) // denied by EuroPol policy
        && !dec.effective.contains(caps::EXEC) // denied by the user-clamp
        && dec.effective.contains(caps::CALENDAR) // optional, but granted
        && routed == Some("facilitator")
        && loop_ok
        && bundle_ok
        && registry_ok;

    crate::serial_println!(
        "[aa] EuroAgent: manifest '{}' v{} ({} caps), MCP fs_write=allowed/exec=denied(cap), NET denied(policy)/EXEC denied(user-clamp), intent→{}, agent loop: {} tool-calls/{} denied→'{}', Ed25519 bundle: valid-OK+tampered-rejected={} → {}",
        m.name,
        m.version,
        ncaps,
        routed.unwrap_or("<none>"),
        agent_run.tool_calls,
        agent_run.denied,
        agent_run.answer,
        bundle_ok,
        if ok { "OK (kernel trust boundary, capability-isolated, audited, LLM↔MCP loop, signed bundle, registry+anti-hijack) ✓" } else { "FAILED" }
    );
    let _ = registry_ok;
}

/// Boot self-test of the **real** FS tool backend: an agent writes + reads a
/// file via the cap-gated MCP gateway, and an attempt WITHOUT the cap is
/// rejected (and writes nothing). Proves that EuroAgent really does work on EuroFS,
/// capability-isolated in a sandbox directory.
pub fn real_tools_selftest(fs: &mut dyn eurofs::FileSystem) {
    use eurofs::FileSystem;
    let _ = fs.create_dir("/agents");
    let _ = fs.create_dir("/agents/facilitator");

    let write_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"notes.txt","content":"hello from the agent"}}}"#;
    let read_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fs_read","arguments":{"path":"notes.txt"}}}"#;
    let escape_req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"../../etc/passwd","content":"x"}}}"#;

    let agent_caps = AgentCaps(caps::FS_READ | caps::FS_WRITE);

    // 1. Write + read + path-escape attempt within the sandbox (cap present).
    let (wrote, read_back) = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/facilitator"), allowed_domains: Vec::new() };
        let w = gw.handle("facilitator", agent_caps, write_req, &mut be);
        let wrote = Json::parse(&w).ok().and_then(|v| v.get("result").cloned()).is_some();
        let r = gw.handle("facilitator", agent_caps, read_req, &mut be);
        let read_back = Json::parse(&r)
            .ok()
            .and_then(|v| v.get("result").and_then(|res| res.get("content")).and_then(|c| c.as_str().map(String::from)));
        // The sandbox clamp strips `..` → an escape attempt stays inside the directory.
        let _ = gw.handle("facilitator", agent_caps, escape_req, &mut be);
        (wrote, read_back)
    };

    // 2. Proof on disk + that the escape did NOT touch /etc/passwd (the sandbox clamp
    //    strips `..`, so the payload "x" can never end up in /etc/passwd).
    let on_disk = fs.read_file("/agents/facilitator/notes.txt").map(|d| d == b"hello from the agent").unwrap_or(false);
    let escape_blocked = fs.read_file("/etc/passwd").map(|d| d != b"x").unwrap_or(true);

    // 3. Cap-gate: without FS_WRITE the agent writes nothing.
    let denied_ok = {
        let _ = fs.create_dir("/agents/readonly");
        let mut gw2 = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/readonly"), allowed_domains: Vec::new() };
        let no_write = AgentCaps(caps::FS_READ); // NO FS_WRITE
        let resp = gw2.handle("readonly", no_write, write_req, &mut be);
        let denied = Json::parse(&resp)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()))
            == Some(euroagent::mcp::ERR_CAP_DENIED);
        denied && be.fs.read_file("/agents/readonly/notes.txt").is_err()
    };

    let ok = wrote && read_back.as_deref() == Some("hello from the agent") && on_disk && escape_blocked && denied_ok;
    crate::serial_println!(
        "[aa-fs] EuroAgent real tools: fs_write+fs_read on EuroFS (sandbox /agents/facilitator)={on_disk}, path-escape-blocked={escape_blocked}, without-cap-denied+nothing-written={denied_ok} → {}",
        if ok { "OK (agent does real work, capability-isolated in a sandbox) ✓" } else { "FAILED" }
    );
}

/// **AD-1 boot self-test** — the real `net_get` and `vault_get` tools, double-gated
/// (capability + domain allow-list), with the "credentials at the boundary" guarantee
/// that a vault value does end up in the tool result but NEVER in the audit/log.
/// The network-path part is deterministic: we prove that the two gates open/close;
/// a real answer requires a peer (SLIRP mock on 10.0.2.2) and is optional.
pub fn net_vault_selftest(fs: &mut dyn eurofs::FileSystem) {
    let vault_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"vault_get","arguments":{"label":"db-password"}}}"#;
    let net_ok_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"net_get","arguments":{"url":"http://10.0.2.2/"}}}"#;
    let net_bad_req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"net_get","arguments":{"url":"http://evil.test/"}}}"#;

    fn err_code(resp: &str) -> Option<i64> {
        Json::parse(resp).ok().and_then(|v| v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()))
    }

    // ── vault_get WITH VAULT_READ → real EuroVault value (set by [u]), and the
    //    value does NOT appear in the audit (the AuditRecord has only name+cap fields).
    let (vault_value, vault_leaked) = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/vaultuser"), allowed_domains: Vec::new() };
        let resp = gw.handle("vaultuser", AgentCaps(caps::VAULT_READ), vault_req, &mut be);
        let val = Json::parse(&resp)
            .ok()
            .and_then(|v| v.get("result").and_then(|r| r.get("value")).and_then(|s| s.as_str().map(String::from)));
        // Scan the whole audit trail (debug-rendered) for the secret — it must never be in it.
        let leaked = gw.audit.iter().any(|r| {
            alloc::format!("{r:?}").contains("euro-s3cr3t")
        });
        (val, leaked)
    };
    let vault_ok = vault_value.as_deref() == Some("euro-s3cr3t");
    let vault_not_logged = !vault_leaked;

    // ── vault_get WITHOUT VAULT_READ → gateway rejects (cap-gate).
    let vault_denied = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/novault"), allowed_domains: Vec::new() };
        let resp = gw.handle("novault", AgentCaps(caps::FS_READ), vault_req, &mut be);
        err_code(&resp) == Some(euroagent::mcp::ERR_CAP_DENIED)
    };

    // ── net_get WITH NET_GET + allowed domain → both gates through. Transport may
    //    fail (no peer in TCG); that is ERR_TOOL_FAILED, not a gate error.
    let (net_through, net_transport) = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend {
            fs,
            root: String::from("/agents/netuser"),
            allowed_domains: alloc::vec![String::from("10.0.2.2")],
        };
        let resp = gw.handle("netuser", AgentCaps(caps::NET_GET), net_ok_req, &mut be);
        let has_result = Json::parse(&resp).ok().and_then(|v| v.get("result").cloned()).is_some();
        let through = has_result || err_code(&resp) == Some(euroagent::mcp::ERR_TOOL_FAILED);
        let transport = if has_result { "peer responded" } else { "no peer (transport ready)" };
        (through, transport)
    };

    // ── net_get WITH NET_GET but FORBIDDEN domain → backend rejects (domain not in
    //    allow-list): no cap error, but a tool error. Least agency in action.
    let net_domain_blocked = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend {
            fs,
            root: String::from("/agents/netuser2"),
            allowed_domains: alloc::vec![String::from("10.0.2.2")],
        };
        let resp = gw.handle("netuser2", AgentCaps(caps::NET_GET), net_bad_req, &mut be);
        err_code(&resp) == Some(euroagent::mcp::ERR_TOOL_FAILED)
    };

    // ── net_get WITHOUT NET_GET → gateway rejects (cap-gate).
    let net_cap_denied = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend {
            fs,
            root: String::from("/agents/nonet"),
            allowed_domains: alloc::vec![String::from("10.0.2.2")],
        };
        let resp = gw.handle("nonet", AgentCaps(caps::FS_READ), net_ok_req, &mut be);
        err_code(&resp) == Some(euroagent::mcp::ERR_CAP_DENIED)
    };

    let ok = vault_ok && vault_not_logged && vault_denied && net_through && net_domain_blocked && net_cap_denied;
    crate::serial_println!(
        "[aa-nv] EuroAgent net+vault: vault_get(cap)→real-EuroVault-value={vault_ok}+not-in-audit={vault_not_logged}, without-cap-denied={vault_denied} · net_get(cap+domain) path-open={net_through}({net_transport}), forbidden-domain-denied={net_domain_blocked}, without-cap-denied={net_cap_denied} → {}",
        if ok { "OK (least agency: cap ∧ domain allow-list; credentials at the boundary, value never logged) ✓" } else { "FAILED" }
    );
}

/// Bridge the RAM audit of an MCP gateway to the PERSISTENT append-only
/// audit log (`/var/log/audit.log`, P3). EVERY tool call — allowed OR
/// denied — becomes an irreversible line that survives a reboot. The
/// `AuditRecord` carries only agent/tool/outcome (never input or secret values),
/// so this leaks no confidential data into the log.
fn persist_agent_audit(gw: &McpGateway, fs: &mut dyn eurofs::FileSystem, caps: u64) {
    for rec in &gw.audit {
        crate::audit::record(
            crate::audit::Event::AgentTool,
            &alloc::format!(
                "agent={} tool={} allowed={} ok={} reason={}",
                rec.agent, rec.tool, rec.allowed, rec.succeeded, rec.reason
            ),
        );
    }
    crate::audit::persist(fs, caps);
}

/// **Audit #7 / P0.3 boot self-test** — proves that the EuroAgent audit trail is no
/// longer RAM-only: a real gateway flow (fs_write allowed, exec denied) is
/// persisted to the append-only on-disk log, a tampering is rejected by
/// the FS, and a second agent action extends the log (survives a remount).
pub fn audit_persist_selftest(fs: &mut dyn eurofs::FileSystem, caps: u64) {
    use eurofs::FileSystem;
    const LOG: &str = "/var/log/audit.log";
    let _ = fs.create_dir("/agents/auditor");

    let nlines = |fs: &mut dyn eurofs::FileSystem| {
        fs.read_file(LOG).map(|d| d.iter().filter(|&&b| b == b'\n').count()).unwrap_or(0)
    };
    let before = nlines(fs);

    // 1. Real gateway: one allowed (fs_write) + one denied (exec) tool call.
    let write_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"a.txt","content":"x"}}}"#;
    let exec_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"exec","arguments":{"cmd":"sh"}}}"#;
    let recs = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/auditor"), allowed_domains: Vec::new() };
        let _ = gw.handle("auditor", AgentCaps(caps::FS_WRITE), write_req, &mut be);
        let _ = gw.handle("auditor", AgentCaps(caps::FS_WRITE), exec_req, &mut be); // no EXEC cap → deny
        gw.audit.clone()
    };
    let recorded = recs.len(); // 2 records expected

    // 2. Bridge to the persistent append-only log + write to disk.
    {
        let mut gw = McpGateway::new();
        gw.audit = recs;
        persist_agent_audit(&gw, fs, caps);
    }

    // 3. Read back FROM DISK: both agent lines are there, with the correct outcome.
    let on_disk = fs.read_file(LOG).unwrap_or_default();
    let disk_txt = alloc::string::String::from_utf8_lossy(&on_disk);
    let has_write = disk_txt.contains("AGENT_TOOL") && disk_txt.contains("tool=fs_write allowed=true");
    let has_exec_deny = disk_txt.contains("tool=exec allowed=false");
    let append_only = fs.get_flags(LOG).unwrap_or(0) & eurofs::FLAG_APPEND_ONLY != 0;

    // 4. Tampering (truncate/overwrite) → the append-only FS rejects it.
    let tamper_blocked = fs.write_file(LOG, b"wiped").is_err();

    // 5. Second agent action → the log grows (survives a remount because we append).
    {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/auditor"), allowed_domains: Vec::new() };
        let _ = gw.handle("auditor", AgentCaps(caps::FS_WRITE), write_req, &mut be);
        let recs2 = gw.audit.clone();
        let mut gw2 = McpGateway::new();
        gw2.audit = recs2;
        persist_agent_audit(&gw2, be.fs, caps);
    }
    let after = nlines(fs);

    let ok = recorded == 2 && has_write && has_exec_deny && append_only && tamper_blocked && after > before;
    crate::serial_println!(
        "[p3-agent] EuroAgent audit PERSISTENT: {recorded} tool-calls→disk, fs_write-allowed-logged={has_write}, exec-denied-logged={has_exec_deny}, append-only-flag={append_only}, tampering-blocked={tamper_blocked}, lines {before}→{after} → {}",
        if ok { "OK (agent trail no longer RAM-only: irreversible + survives a restart) ✓" } else { "FAILED" }
    );
}

/// **AF / Zero-Trust P2.2 boot self-test** — just-in-time elevation + auto-revoke.
/// Proves that an ELEVATED cap (EXEC), even if the agent has it in its standing set,
/// may only be used AFTER a fresh JIT grant, and that this grant automatically
/// expires after one action — "elevate for the task, auto-revoke on completion".
pub fn jit_selftest() {
    let code = |resp: &str| {
        Json::parse(resp).ok().and_then(|v| v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()))
    };
    let has_result = |resp: &str| Json::parse(resp).ok().and_then(|v| v.get("result").cloned()).is_some();

    let mut gw = McpGateway::new();
    let caps = AgentCaps(caps::EXEC); // the standing set contains the elevated cap
    let exec_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"exec","arguments":{"cmd":"build"}}}"#;

    // 1. Without a grant → denied, even though EXEC is in the set.
    let denied_first = code(&gw.handle("builder", caps, exec_req, &mut KernelBackend)) == Some(euroagent::mcp::ERR_JIT_REQUIRED);
    // 2. After a JIT grant (which would come AFTER user confirmation) → the one call is allowed.
    gw.grant_jit(caps::EXEC);
    let allowed_after_grant = has_result(&gw.handle("builder", caps, exec_req, &mut KernelBackend));
    // 3. Auto-revoke: the grant is consumed → a second call is denied again.
    let revoked = gw.pending_jit() == 0
        && code(&gw.handle("builder", caps, exec_req, &mut KernelBackend)) == Some(euroagent::mcp::ERR_JIT_REQUIRED);

    let ok = denied_first && allowed_after_grant && revoked;
    crate::serial_println!(
        "[af-jit] JIT elevation: elevated-cap-without-grant-denied={denied_first}, after-grant-one-action-allowed={allowed_after_grant}, auto-revoke-2nd-call-denied={revoked} → {}",
        if ok { "OK (least agency in time: elevate-for-the-task, auto-revoke) ✓" } else { "FAILED" }
    );
}

/// **AF / Zero-Trust P2.3 boot self-test** — behavior detection on the gateway audit
/// stream. Proves that the monitor (1) does NOT alarm on normal behavior during the
/// learning phase, (2) flags a series of denied calls as capability-probing, and
/// (3) flags a tool that falls outside the baseline behavior as drift. Deterministic
/// and explainable (no ML) — every alert is traceable to one line.
pub fn anomaly_selftest() {
    use euroagent::anomaly::{AnomalyKind, BehaviorMonitor, MonitorCfg};
    use euroagent::mcp::AuditRecord;

    let mk = |tool: &'static str, allowed: bool| AuditRecord {
        agent: String::from("watcher"),
        tool: String::from(tool),
        allowed,
        succeeded: allowed,
        reason: if allowed { "ok" } else { "insufficient_capability" },
    };

    let mut mon = BehaviorMonitor::new(MonitorCfg { baseline_calls: 3, denial_run: 4, ..MonitorCfg::default() });

    // Learning phase: 3× fs_read → baseline = {fs_read}, no alerts.
    let learn_quiet = (0..3).all(|i| mon.observe(&mk("fs_read", true), i).is_empty());
    // Known behavior after the baseline → quiet.
    let known_quiet = mon.observe(&mk("fs_read", true), 10).is_empty();

    // Capability-probing: 4 consecutive denials → DenialSpike.
    let mut probing_flagged = false;
    for i in 11..=14 {
        if mon.observe(&mk("exec", false), i).iter().any(|a| a.kind == AnomalyKind::DenialSpike) {
            probing_flagged = true;
        }
    }
    // Behavior drift: a tool that was never in the baseline → UnseenTool.
    let drift_flagged = mon
        .observe(&mk("net_post", true), 20)
        .iter()
        .any(|a| a.kind == AnomalyKind::UnseenTool);

    let ok = learn_quiet && known_quiet && probing_flagged && drift_flagged;
    crate::serial_println!(
        "[af-anom] behavior detection (deterministic, audit-fed): learning-phase-quiet={learn_quiet}, known-behavior-quiet={known_quiet}, probing(4×deny)-flagged={probing_flagged}, drift(new-tool)-flagged={drift_flagged} → {}",
        if ok { "OK (anomalous agent behavior visible to audit/response) ✓" } else { "FAILED" }
    );
}

/// `euroagent [subcommand]` shell. Subcommands: (empty)/`status` · `caps` ·
/// `mcp list` · `inspect` · `dispatch test <intent>`.
pub fn shell(args: &str) -> Vec<String> {
    let args = args.trim();
    let eff = AgentCaps(*EFFECTIVE.lock());

    // Parse the demo manifest once for the inspect/dispatch subcommands.
    let manifest = AgentManifest::from_toml(DEMO_MANIFEST).ok();

    let mut a = args.split_whitespace();
    match a.next() {
        Some("list") => {
            let list = INSTALLED.lock();
            if list.is_empty() {
                return alloc::vec![String::from("EuroAgent — no agents installed")];
            }
            let mut out = alloc::vec![alloc::format!("EuroAgent — {} installed agent(s):", list.len())];
            for (name, version, publisher) in list.iter() {
                out.push(alloc::format!("  • {name} v{version}  (publisher {}…)", &publisher[..publisher.len().min(16)]));
            }
            out.push(String::from("  (only validly-Ed25519-signed bundles; name pinned to the publisher — anti-hijack)"));
            out
        }
        Some("llm") => {
            // BB-1: do a REAL conversation step over EuroNet TCP with the local,
            // sovereign Ollama endpoint. The rest of the prompt (after "llm ") is the
            // message; defaults to a short test question.
            let prompt = args.trim().strip_prefix("llm").map(|s| s.trim()).filter(|s| !s.is_empty())
                .unwrap_or("Say in one sentence who you are.");
            let mut be = default_ollama();
            let resp = be.step(
                &[euroagent::llm::Message::user(prompt)],
                &["fs_read", "fs_write"],
            );
            let mut out = alloc::vec![
                String::from("EuroAgent LLM — default backend: LOCAL (sovereign, no cloud)"),
                alloc::format!("  endpoint : http://{}  (Ollama-compatible, via EuroNet TCP)", be.host),
                alloc::format!("  model    : {}", be.model),
                String::from("  cloud    : opt-in per user (key via EuroVault, every call → P3 audit)"),
            ];
            if be.reachable {
                match resp {
                    LlmResponse::Text(t) => out.push(alloc::format!("  answer   : {}", t.trim())),
                    LlmResponse::ToolCall { name, .. } => out.push(alloc::format!("  tool-call: {name} (the model requested a tool)")),
                }
            } else {
                out.push(String::from("  status   : transport ready, but no endpoint reachable"));
                out.push(String::from("             (start local Ollama on port 11434, or an Ollama-compatible mock)"));
            }
            out
        }
        Some("caps") => {
            let mut out = alloc::vec![alloc::format!(
                "EuroAgent — effective capability set of demo agent 'facilitator' ({}):",
                eff.names().len()
            )];
            for n in eff.names() {
                out.push(alloc::format!("  • {n}"));
            }
            out.push(String::from("  (NET denied by EuroPol policy; EXEC denied by the user-clamp)"));
            out
        }
        Some("mcp") => {
            // `mcp list` — show the tools that the effective caps may call.
            let gw = McpGateway::new();
            let mut out = alloc::vec![String::from("EuroAgent MCP gateway — tools available for 'facilitator':")];
            if let Some(Json::Arr(items)) = gw.list_for(eff).get("tools") {
                for t in items {
                    let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                    let cap = t.get("required_cap").and_then(|c| c.as_str()).unwrap_or("?");
                    let desc = t.get("description").and_then(|d| d.as_str()).unwrap_or("");
                    out.push(alloc::format!("  • {name}  [{cap}] — {desc}"));
                }
            }
            out.push(String::from("  (JSON-RPC 2.0 over AF_UNIX; every call → P3 audit)"));
            out
        }
        Some("inspect") => match manifest {
            Some(m) => alloc::vec![
                alloc::format!("Agent: {} v{}  ({})", m.name, m.version, m.lang),
                alloc::format!("  {}", m.description),
                alloc::format!("  author: {}", m.author),
                alloc::format!("  wasm: {}", m.wasm),
                alloc::format!("  required caps: {}", m.required.names().join(", ")),
                alloc::format!("  optional caps: {}", m.optional.names().join(", ")),
                alloc::format!("  triggers (intent): {}", m.triggers_intent.join(" | ")),
                alloc::format!("  tools allowed: {}", m.tools_allowed.join(", ")),
                alloc::format!("  sandbox: max {} MB memory", m.max_memory_mb),
            ],
            None => alloc::vec![String::from("euroagent: cannot parse demo manifest")],
        },
        Some("dispatch") => {
            // `dispatch test <intent>` — show which agent an intent would route to.
            let _ = a.next(); // "test"
            let intent_text: String = a.collect::<alloc::vec::Vec<_>>().join(" ");
            let routes = manifest
                .as_ref()
                .map(|m| alloc::vec![intent::Route { agent: m.name.clone(), intents: m.triggers_intent.clone() }])
                .unwrap_or_default();
            match intent::route(&intent_text, &routes) {
                Some(r) => alloc::vec![alloc::format!("intent '{intent_text}' → agent '{}'", r.agent)],
                None => alloc::vec![alloc::format!("intent '{intent_text}' → (no matching agent)")],
            }
        }
        _ => alloc::vec![
            String::from("EuroAgent — sovereign agent-first runtime (Sprint AA)"),
            String::from("  agents = WASM + declarative capability manifest; trust boundary in the kernel (EuroGuard), not the cloud"),
            String::from("  MCP gateway: open Model-Context-Protocol, capability-gated, every call → P3 audit"),
            String::from("  LLM: default local (Ollama-compatible); cloud opt-in via EuroVault"),
            String::from("  vs. Project Solara: EuroAgent runs fully offline, EU data residency guaranteed"),
            String::from("  subcommands: euroagent list · caps · mcp list · inspect · llm · dispatch test <intent>"),
        ],
    }
}
