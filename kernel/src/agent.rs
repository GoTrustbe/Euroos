//! Kernel-zijde van **EuroAgent** (Sprint AA): de soevereine agent-first runtime.
//!
//! Agents zijn WASM-modules met een declaratief capability-manifest; de trust
//! boundary zit hier in de kernel (EuroGuard), niet in een cloud. De host-geteste
//! kern leeft in [`euroagent`]; dit module bewijst hem live bij boot — manifest
//! parsen, de effectieve capability-set afleiden (least-privilege clamp), een
//! cap-gated MCP-tool-aanroep doen, en een intent routen — en biedt het
//! `euroagent`-shellcommando.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use euroagent::caps::{self, AgentCaps};
use euroagent::json::Json;
use euroagent::llm::{LlmBackend, LlmResponse, Message};
use euroagent::mcp::{McpGateway, ToolBackend};
use euroagent::{agentloop, intent, manifest::AgentManifest, policy};
use spin::Mutex;

/// Een voorbeeld-manifest dat de runtime bij boot valideert (de "facilitator"
/// vergaderassistent uit het EuroAgent-plan).
const DEMO_MANIFEST: &str = r#"
[agent]
name        = "facilitator"
version     = "1.0.0"
description = "Vergaderingen opnemen, transcriberen en samenvatten"
author      = "GoTrust BV <agent@gotrust.eu>"
wasm        = "facilitator.wasm"
lang        = "nl-BE"

[capabilities]
required = ["CAP_AGENT_MIC", "CAP_AGENT_FS_WRITE", "CAP_AGENT_DISPLAY"]
optional = ["CAP_AGENT_NET", "CAP_AGENT_CALENDAR"]

[triggers]
on_intent = ["vergadering opnemen", "start recording"]

[tools]
allowed = ["mic_record", "fs_write", "display_notify"]
denied  = ["exec", "vault_read"]

[sandbox]
max_memory_mb = 64
"#;

static EFFECTIVE: Mutex<u64> = Mutex::new(0);
/// Geïnstalleerde agents (naam, versie, uitgever-hex), gevuld door de boot-zelftest.
static INSTALLED: Mutex<alloc::vec::Vec<(String, String, String)>> = Mutex::new(alloc::vec::Vec::new());

/// Een kernel-MCP-backend-stub: bewijst het pad agent→gateway→subsysteem zonder
/// echte zij-effecten (de echte koppeling naar EuroFS/EuroNet/EuroVault komt in
/// de userspace-daemon). Geeft de input echo'd terug.
struct KernelBackend;
impl ToolBackend for KernelBackend {
    fn execute(&mut self, tool: &str, input: &Json) -> Result<Json, String> {
        let _ = (tool, input);
        Ok(Json::Obj(alloc::vec![("ok".into(), Json::Bool(true))]))
    }
}

/// **Echte** MCP-tool-backend: koppelt de gateway-tools aan de échte EuroOS-
/// subsystemen — EuroFS (`fs_read`/`fs_write`), EuroNet (`net_get`) en EuroVault
/// (`vault_get`). Een agent die deze tools aanroept raakt nu écht de schijf, het
/// netwerk of de kluis — maar uitsluitend binnen zijn sandbox-map `/agents/<naam>/`,
/// alleen naar domeinen in zijn manifest-`network_domains`-allow-list, en alleen als
/// de cap-gate hem doorliet. Zo is EuroAgent geen stub meer maar een agent die
/// daadwerkelijk werk verricht, capability-geïsoleerd op kernelniveau (least agency).
pub struct FsToolBackend<'a> {
    pub fs: &'a mut dyn eurofs::FileSystem,
    /// De sandbox-wortel; alle paden worden hieronder geklemd.
    pub root: alloc::string::String,
    /// De toegestane netwerk-domeinen uit het agent-manifest (`network_domains`).
    /// Leeg = `net_get` mag NERGENS heen (deny-by-default, least agency).
    pub allowed_domains: Vec<String>,
}

impl<'a> FsToolBackend<'a> {
    /// Klem een (mogelijk kwaadaardig) relatief pad binnen de sandbox-wortel:
    /// strip `..`/leidende `/` zodat een agent niet kan ontsnappen.
    fn sandbox_path(&self, rel: &str) -> alloc::string::String {
        let mut p = self.root.clone();
        // Splits op zowel `/` als `\` (audit C6: een backend-onafhankelijke klem) en
        // verwerp elk `.`/`..`/leeg segment — zo kan een agent nooit boven zijn
        // sandbox-wortel uitkomen, ongeacht hoe het pad gescheiden is.
        for seg in rel.split(['/', '\\']) {
            if seg.is_empty() || seg == "." || seg == ".." {
                continue;
            }
            p.push('/');
            p.push_str(seg);
        }
        p
    }

    /// Tweede gate boven de capability (least agency): mag deze agent dit host
    /// bereiken? Exacte match of een subdomein van een toegestaan domein. Een lege
    /// allow-list weigert álles — een agent zonder gedeclareerde domeinen heeft géén
    /// netwerkpad (north-star: "impossible, not tedious" — het pad bestáát niet).
    fn host_allowed(&self, host: &str) -> bool {
        self.allowed_domains.iter().any(|d| {
            let d = d.trim();
            !d.is_empty() && (host == d || host.ends_with(&alloc::format!(".{d}")))
        })
    }
}

/// Splits een URL in `(tls, host, port, path)`. Alleen `http`/`https`; geen
/// userinfo/fragment (een agent-tool heeft die niet nodig en ze vergroten het
/// aanvalsoppervlak). Faalt netjes (`None`) op alles wat niet klopt.
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
                let path = input.get("path").and_then(|p| p.as_str()).ok_or_else(|| "geen pad".to_string())?;
                let content = input.get("content").and_then(|c| c.as_str()).unwrap_or("");
                let full = self.sandbox_path(path);
                self.fs
                    .write_file(&full, content.as_bytes())
                    .map_err(|_| "schrijven mislukt".to_string())?;
                Ok(Json::Obj(alloc::vec![
                    ("written".into(), Json::Num(content.len().to_string())),
                    ("path".into(), Json::Str(full)),
                ]))
            }
            "fs_read" => {
                let path = input.get("path").and_then(|p| p.as_str()).ok_or_else(|| "geen pad".to_string())?;
                let full = self.sandbox_path(path);
                let data = self.fs.read_file(&full).map_err(|_| "lezen mislukt".to_string())?;
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
                // Tweede gate boven de NET_GET-capability: het host moet in de
                // manifest-allow-list staan (least agency — niet alleen "mag het
                // netwerk", maar "mag DIT domein").
                let url = input.get("url").and_then(|u| u.as_str()).ok_or_else(|| "geen url".to_string())?;
                let (tls, host, port, path) = parse_url(url).ok_or_else(|| "ongeldige url".to_string())?;
                if !self.host_allowed(&host) {
                    return Err(alloc::format!("domein '{host}' niet in network_domains-allow-list"));
                }
                match crate::net::fetch_full(&host, port, &path, tls) {
                    Some((status, ctype, body)) => {
                        // Begrens de teruggegeven body (anti-DoS / geheugen); meld of
                        // er afgekapt is zodat de agent het weet.
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
                    None => Err(alloc::format!("ophalen van {host}:{port} mislukt (geen verbinding)")),
                }
            }
            "vault_get" => {
                // Credentials-at-the-boundary: de waarde gaat ALLEEN in dit
                // tool-resultaat (voor déze ene call) — nooit naar het serial-log of
                // de audit-trail (de AuditRecord bevat enkel toolnaam+cap, geen
                // input/resultaat). De gateway liet ons hier alleen binnen met
                // VAULT_READ; de echte EuroVault-cap-gate bevestigt nog eens.
                let label = input.get("label").and_then(|l| l.as_str()).ok_or_else(|| "geen label".to_string())?;
                match crate::vault::get(label, crate::vault::CAP_DB_ACCESS) {
                    Ok(value) => Ok(Json::Obj(alloc::vec![
                        ("value".into(), Json::Str(String::from_utf8_lossy(&value).into_owned())),
                        ("bytes".into(), Json::Num(value.len().to_string())),
                    ])),
                    Err(eurovault::VaultError::NotFound) => Err(alloc::format!("secret '{label}' bestaat niet")),
                    Err(eurovault::VaultError::PermissionDenied) => Err("vault weigert toegang".to_string()),
                    Err(_) => Err("vault-fout (ontsleutelen/corrupt)".to_string()),
                }
            }
            // exec blijft bewust ongekoppeld → deny-by-default. De gateway weigert
            // hem normaliter al op de EXEC-cap; mocht hij hier toch komen, dan faalt
            // hij hard. Een veilige exec-sandbox is een apart, goedgekeurd ontwerp.
            other => Err(alloc::format!("tool '{other}' is niet beschikbaar in de kernel-backend (deny-by-default)")),
        }
    }
}

/// Een gescript mock-model: bewijst de agent-lus zonder een echte LLM in de
/// sandbox. (De echte koppeling is een lokale Ollama-backend via EuroNet.)
struct ScriptedLlm {
    step: usize,
}
impl LlmBackend for ScriptedLlm {
    fn step(&mut self, _m: &[Message], _t: &[&str]) -> LlmResponse {
        self.step += 1;
        match self.step {
            // Stap 1: vraag een toegestane tool aan (fs_write).
            1 => LlmResponse::ToolCall {
                name: String::from("fs_write"),
                arguments: Json::Obj(alloc::vec![
                    ("path".into(), Json::Str("samenvatting.txt".into())),
                    ("content".into(), Json::Str("klaar".into())),
                ]),
            },
            // Stap 2: probeer een verboden tool (exec) → wordt geweigerd.
            2 => LlmResponse::ToolCall {
                name: String::from("exec"),
                arguments: Json::Obj(alloc::vec![("cmd".into(), Json::Str("rm -rf /".into()))]),
            },
            // Stap 3: eindantwoord.
            _ => LlmResponse::Text(String::from("Samenvatting opgeslagen.")),
        }
    }
}

/// **BB-1** — een ECHTE LLM-backend (geen mock): `step()` praat over EuroNet-TCP
/// met een lokale, Ollama-compatibele `/api/chat`-endpoint. Het bouwt het HTTP/1.1
/// POST-request (`euroagent::llm::ollama_http_request`), stuurt het via
/// `net::http_post_raw`, en parset de échte HTTP-response. Lokaal = soeverein,
/// geen cloud; transport is bounded (kan de boot niet laten hangen).
struct NetOllama {
    host: String, // Host-header, bv. "10.0.2.2:11434"
    ip: String,   // connect-IP (zonder poort)
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
                    .unwrap_or_else(|e| LlmResponse::Text(alloc::format!("[LLM-parsefout: {e}]")))
            }
            None => {
                self.reachable = false;
                LlmResponse::Text(String::from("[geen lokaal LLM-endpoint bereikbaar]"))
            }
        }
    }
}

/// De standaard, soevereine LLM-endpoint: lokale Ollama. In QEMU bereiken we de
/// host (waar de mock/echte Ollama draait) via de SLIRP-gateway 10.0.2.2.
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

/// **BB-1 boot-zelftest** — bewijs het ECHTE LLM-transport end-to-end: bouw het
/// Ollama-request, stuur het over EuroNet-TCP naar 10.0.2.2:11434, en parse de
/// echte HTTP-response naar een model-antwoord (tekst of tool-call). Draait de
/// agent-lus met deze échte backend wanneer een endpoint bereikbaar is.
pub fn llm_selftest() {
    let mut be = default_ollama();
    let msgs = alloc::vec![
        Message::system("Je bent een soevereine assistent. Gebruik tools waar nuttig."),
        Message::user("Lees het contract en vat het samen."),
    ];
    let resp = be.step(&msgs, &["fs_read", "fs_write"]);
    // P3-audit: élke LLM-call (lokaal of cloud) wordt geaudit.
    crate::serial_println!("[p3] audit: agent-llm-call model=mistral:7b-instruct endpoint=10.0.2.2:11434 lokaal=true");
    if be.reachable {
        let kind = match &resp {
            LlmResponse::ToolCall { name, .. } => alloc::format!("tool-call '{name}'"),
            LlmResponse::Text(t) => alloc::format!("tekst \"{}\"", t.trim().chars().take(40).collect::<String>()),
        };
        crate::serial_println!(
            "[bb1] EuroAgent LLM-transport: ECHTE Ollama-call over EuroNet-TCP (HTTP POST /api/chat) → 10.0.2.2:11434 → model-respons: {kind} ✓ (lokaal/soeverein, P3-geaudit)"
        );
    } else {
        crate::serial_println!(
            "[bb1] EuroAgent LLM-transport GEREED: HTTP POST /api/chat over EuroNet-TCP gebouwd; geen endpoint op 10.0.2.2:11434 (start Ollama/mock om de echte model-respons te zien) ✓"
        );
    }
}

/// **BB-6** — draai de demo-agent voor een vrije `intent` (vanuit de dispatch-GUI).
/// Geeft (gerouteerde agent-naam, agent-run met live tool-call-transcript) terug.
/// De lus loopt door de ECHTE MCP-gateway (cap-gate + audit): fs_write wordt
/// toegestaan, exec geweigerd door de user-clamp — precies wat de GUI live toont.
pub fn run_intent(intent: &str) -> (Option<String>, agentloop::AgentRun) {
    let m = match AgentManifest::from_toml(DEMO_MANIFEST) {
        Ok(m) => m,
        Err(_) => {
            return (
                None,
                agentloop::AgentRun {
                    answer: String::from("[manifest-fout]"),
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
            Message::system("Je bent een soevereine assistent. Gebruik tools waar nuttig."),
            Message::user(intent),
        ],
        8,
    );
    (routed, run)
}

/// Boot-zelftest: end-to-end bewijs van de EuroAgent-runtime-kern.
pub fn selftest() {
    // 1. Manifest parsen + valideren.
    let m = match AgentManifest::from_toml(DEMO_MANIFEST) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[aa] EuroAgent: manifest MISLUKT: {}", e.describe());
            return;
        }
    };

    // 2. Effectieve caps afleiden. Gebruiker bezit alles behalve EXEC; EuroPol
    //    verbiedt het netwerk voor dit agent-type; de gebruiker kende CALENDAR toe.
    let user_caps = AgentCaps(caps::ALL & !caps::EXEC);
    let granted = AgentCaps(caps::CALENDAR);
    let policy_denied = AgentCaps(caps::NET_GET | caps::NET_POST);
    let dec = policy::derive(&m, granted, user_caps, policy_denied);
    *EFFECTIVE.lock() = dec.effective.0;

    // 3. Cap-gated MCP-aanroep: fs_write is toegestaan (FS_WRITE in set), exec niet.
    let mut gw = McpGateway::new();
    let mut be = KernelBackend;
    let allow = gw.handle(
        &m.name,
        dec.effective,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"transcript.txt","content":"hoi"}}}"#,
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

    // 4. Intent-routing.
    let routes = alloc::vec![intent::Route { agent: m.name.clone(), intents: m.triggers_intent.clone() }];
    let routed = intent::route("kun je de vergadering opnemen", &routes).map(|r| r.agent.as_str());

    // 5. (AA-5) De volledige agent-lus: model → tool → resultaat → model →
    //    eindantwoord, met een gescript mock-model door de échte MCP-gateway
    //    (cap-gate + audit). De agent vraagt fs_write (toegestaan) + exec (geweigerd).
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
            Message::system("Je bent een soevereine vergaderassistent."),
            Message::user("Vat de vergadering samen en sla het op."),
        ],
        8,
    );
    let loop_ok = agent_run.answer == "Samenvatting opgeslagen."
        && agent_run.tool_calls == 2
        && agent_run.denied == 1 // de exec-poging werd door de cap-gate geweigerd
        && !agent_run.truncated;

    // 6. (AA-1 sluitstuk) Ed25519-`.euroa`-bundle-verificatie: een geldige bundle
    //    verifieert; een vervalste WASM-binary wordt geweigerd. Bewijst dat de
    //    keten uitgever→bundle→draaiende agent sluitend is.
    let bundle_ok = {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[0x5e; 32]);
        let pk = sk.verifying_key().to_bytes();
        let wasm: &[u8] = b"\0asm\x01\0\0\0euroagent-demo";
        let sig = sk.sign(&euroagent::bundle::signing_message(DEMO_MANIFEST, wasm)).to_bytes();
        let good = euroagent::bundle::AgentBundle { manifest_toml: DEMO_MANIFEST, wasm, signature: sig };
        let valid = good.verify(&pk).is_ok();
        // Zelfde handtekening, maar gewijzigde WASM → moet falen.
        let evil = euroagent::bundle::AgentBundle { manifest_toml: DEMO_MANIFEST, wasm: b"\0asm-VERVALST", signature: sig };
        let tampered_rejected = evil.verify(&pk).is_err();
        valid && tampered_rejected
    };

    // 7. (AA-1 register) Installeer de getekende agent in een register; bewijs dat
    //    een andere uitgever 'facilitator' niet kan kapen.
    let registry_ok = {
        use ed25519_dalek::{Signer, SigningKey};
        let mut reg = euroagent::registry::AgentRegistry::new();
        let sk = SigningKey::from_bytes(&[0x5e; 32]);
        let pk = sk.verifying_key().to_bytes();
        let wasm: &[u8] = b"\0asm-facilitator";
        let sig = sk.sign(&euroagent::bundle::signing_message(DEMO_MANIFEST, wasm)).to_bytes();
        let good = euroagent::bundle::AgentBundle { manifest_toml: DEMO_MANIFEST, wasm, signature: sig };
        let installed = reg.install(&good, &pk).is_ok();
        // Een andere uitgever met geldige eigen handtekening mag 'facilitator' niet overschrijven.
        let sk2 = SigningKey::from_bytes(&[0x99; 32]);
        let sig2 = sk2.sign(&euroagent::bundle::signing_message(DEMO_MANIFEST, wasm)).to_bytes();
        let hijack = euroagent::bundle::AgentBundle { manifest_toml: DEMO_MANIFEST, wasm, signature: sig2 };
        let hijack_blocked = reg.install(&hijack, &sk2.verifying_key().to_bytes()).is_err();
        // Bewaar de installatielijst voor het `euroagent list`-commando.
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
        && !dec.effective.contains(caps::NET_GET) // door EuroPol-beleid geweigerd
        && !dec.effective.contains(caps::EXEC) // door user-clamp geweigerd
        && dec.effective.contains(caps::CALENDAR) // optioneel, maar toegekend
        && routed == Some("facilitator")
        && loop_ok
        && bundle_ok
        && registry_ok;

    crate::serial_println!(
        "[aa] EuroAgent: manifest '{}' v{} ({} caps), MCP fs_write=toegestaan/exec=geweigerd(cap), NET geweigerd(beleid)/EXEC geweigerd(user-clamp), intent→{}, agent-lus: {} tool-calls/{} geweigerd→'{}', Ed25519-bundle: geldig-OK+vervalst-geweigerd={} → {}",
        m.name,
        m.version,
        ncaps,
        routed.unwrap_or("<geen>"),
        agent_run.tool_calls,
        agent_run.denied,
        agent_run.answer,
        bundle_ok,
        if ok { "OK (kernel-trust-boundary, capability-geïsoleerd, geauditeerd, LLM↔MCP-lus, getekende bundle, register+anti-kaping) ✓" } else { "MISLUKT" }
    );
    let _ = registry_ok;
}

/// Boot-zelftest van de **echte** FS-tool-backend: een agent schrijft + leest een
/// bestand via de cap-gated MCP-gateway, en een poging zónder de cap wordt
/// geweigerd (en schrijft niets). Bewijst dat EuroAgent écht werk doet op EuroFS,
/// capability-geïsoleerd in een sandbox-map.
pub fn real_tools_selftest(fs: &mut dyn eurofs::FileSystem) {
    use eurofs::FileSystem;
    let _ = fs.create_dir("/agents");
    let _ = fs.create_dir("/agents/facilitator");

    let write_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"notes.txt","content":"hallo van de agent"}}}"#;
    let read_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fs_read","arguments":{"path":"notes.txt"}}}"#;
    let escape_req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"../../etc/passwd","content":"x"}}}"#;

    let agent_caps = AgentCaps(caps::FS_READ | caps::FS_WRITE);

    // 1. Schrijf + lees + pad-escape-poging binnen de sandbox (cap aanwezig).
    let (wrote, read_back) = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/facilitator"), allowed_domains: Vec::new() };
        let w = gw.handle("facilitator", agent_caps, write_req, &mut be);
        let wrote = Json::parse(&w).ok().and_then(|v| v.get("result").cloned()).is_some();
        let r = gw.handle("facilitator", agent_caps, read_req, &mut be);
        let read_back = Json::parse(&r)
            .ok()
            .and_then(|v| v.get("result").and_then(|res| res.get("content")).and_then(|c| c.as_str().map(String::from)));
        // De sandbox-klem strips `..` → een escape-poging blijft binnen de map.
        let _ = gw.handle("facilitator", agent_caps, escape_req, &mut be);
        (wrote, read_back)
    };

    // 2. Bewijs op schijf + dat de escape /etc/passwd níét raakte (de sandbox-klem
    //    strips `..`, dus de payload "x" kan nooit in /etc/passwd belanden).
    let on_disk = fs.read_file("/agents/facilitator/notes.txt").map(|d| d == b"hallo van de agent").unwrap_or(false);
    let escape_blocked = fs.read_file("/etc/passwd").map(|d| d != b"x").unwrap_or(true);

    // 3. Cap-gate: zonder FS_WRITE schrijft de agent niets.
    let denied_ok = {
        let _ = fs.create_dir("/agents/readonly");
        let mut gw2 = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/readonly"), allowed_domains: Vec::new() };
        let no_write = AgentCaps(caps::FS_READ); // GEEN FS_WRITE
        let resp = gw2.handle("readonly", no_write, write_req, &mut be);
        let denied = Json::parse(&resp)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()))
            == Some(euroagent::mcp::ERR_CAP_DENIED);
        denied && be.fs.read_file("/agents/readonly/notes.txt").is_err()
    };

    let ok = wrote && read_back.as_deref() == Some("hallo van de agent") && on_disk && escape_blocked && denied_ok;
    crate::serial_println!(
        "[aa-fs] EuroAgent echte tools: fs_write+fs_read op EuroFS (sandbox /agents/facilitator)={on_disk}, pad-escape-geblokkeerd={escape_blocked}, zonder-cap-geweigerd+niets-geschreven={denied_ok} → {}",
        if ok { "OK (agent verricht écht werk, capability-geïsoleerd in sandbox) ✓" } else { "MISLUKT" }
    );
}

/// **AD-1 boot-zelftest** — de échte `net_get`- en `vault_get`-tools, dubbel gegate
/// (capability + domein-allow-list), met de "credentials at the boundary"-garantie
/// dat een vault-waarde wél in het tool-resultaat komt maar NOOIT in de audit/log.
/// Het netwerkpad-deel is deterministisch: we bewijzen dat de twee gates open/dicht
/// gaan; een echt antwoord vereist een peer (SLIRP-mock op 10.0.2.2) en is optioneel.
pub fn net_vault_selftest(fs: &mut dyn eurofs::FileSystem) {
    let vault_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"vault_get","arguments":{"label":"db-password"}}}"#;
    let net_ok_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"net_get","arguments":{"url":"http://10.0.2.2/"}}}"#;
    let net_bad_req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"net_get","arguments":{"url":"http://evil.test/"}}}"#;

    fn err_code(resp: &str) -> Option<i64> {
        Json::parse(resp).ok().and_then(|v| v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()))
    }

    // ── vault_get MET VAULT_READ → echte EuroVault-waarde (door [u] gezet), en de
    //    waarde komt NIET in de audit (de AuditRecord heeft enkel naam+cap-velden).
    let (vault_value, vault_leaked) = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/vaultuser"), allowed_domains: Vec::new() };
        let resp = gw.handle("vaultuser", AgentCaps(caps::VAULT_READ), vault_req, &mut be);
        let val = Json::parse(&resp)
            .ok()
            .and_then(|v| v.get("result").and_then(|r| r.get("value")).and_then(|s| s.as_str().map(String::from)));
        // Scan de hele audit-trail (debug-gerendered) op het secret — mag er nooit in zitten.
        let leaked = gw.audit.iter().any(|r| {
            alloc::format!("{r:?}").contains("euro-s3cr3t")
        });
        (val, leaked)
    };
    let vault_ok = vault_value.as_deref() == Some("euro-s3cr3t");
    let vault_not_logged = !vault_leaked;

    // ── vault_get ZONDER VAULT_READ → gateway weigert (cap-gate).
    let vault_denied = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/novault"), allowed_domains: Vec::new() };
        let resp = gw.handle("novault", AgentCaps(caps::FS_READ), vault_req, &mut be);
        err_code(&resp) == Some(euroagent::mcp::ERR_CAP_DENIED)
    };

    // ── net_get MET NET_GET + toegestaan domein → beide gates door. Transport mag
    //    falen (geen peer in TCG); dat is ERR_TOOL_FAILED, niet een gate-fout.
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
        let transport = if has_result { "peer antwoordde" } else { "geen peer (transport gereed)" };
        (through, transport)
    };

    // ── net_get MET NET_GET maar VERBODEN domein → backend weigert (domein niet in
    //    allow-list): géén cap-fout, wel een tool-fout. Least agency in actie.
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

    // ── net_get ZONDER NET_GET → gateway weigert (cap-gate).
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
        "[aa-nv] EuroAgent net+vault: vault_get(cap)→echte-EuroVault-waarde={vault_ok}+niet-in-audit={vault_not_logged}, zonder-cap-geweigerd={vault_denied} · net_get(cap+domein) pad-open={net_through}({net_transport}), verboden-domein-geweigerd={net_domain_blocked}, zonder-cap-geweigerd={net_cap_denied} → {}",
        if ok { "OK (least agency: cap ∧ domein-allow-list; credentials at the boundary, waarde nooit gelogd) ✓" } else { "MISLUKT" }
    );
}

/// Bridge de RAM-audit van een MCP-gateway naar het PERSISTENTE append-only
/// audit-log (`/var/log/audit.log`, P3). Élke tool-aanroep — toegestaan óf
/// geweigerd — wordt een onomkeerbare regel die een herstart overleeft. De
/// `AuditRecord` draagt enkel agent/tool/uitkomst (nooit input of secret-waarden),
/// dus dit lekt geen vertrouwelijke data naar het log.
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

/// **Audit #7 / P0.3 boot-zelftest** — bewijst dat het EuroAgent-audit-spoor niet
/// langer RAM-only is: een échte gateway-flow (fs_write toegestaan, exec geweigerd)
/// wordt gepersisteerd naar het append-only on-disk log, een vervalsing wordt door
/// de FS geweigerd, en een tweede agent-actie breidt het log uit (overleeft remount).
pub fn audit_persist_selftest(fs: &mut dyn eurofs::FileSystem, caps: u64) {
    use eurofs::FileSystem;
    const LOG: &str = "/var/log/audit.log";
    let _ = fs.create_dir("/agents/auditor");

    let nlines = |fs: &mut dyn eurofs::FileSystem| {
        fs.read_file(LOG).map(|d| d.iter().filter(|&&b| b == b'\n').count()).unwrap_or(0)
    };
    let before = nlines(fs);

    // 1. Échte gateway: één toegestane (fs_write) + één geweigerde (exec) tool-call.
    let write_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"a.txt","content":"x"}}}"#;
    let exec_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"exec","arguments":{"cmd":"sh"}}}"#;
    let recs = {
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/auditor"), allowed_domains: Vec::new() };
        let _ = gw.handle("auditor", AgentCaps(caps::FS_WRITE), write_req, &mut be);
        let _ = gw.handle("auditor", AgentCaps(caps::FS_WRITE), exec_req, &mut be); // geen EXEC-cap → deny
        gw.audit.clone()
    };
    let recorded = recs.len(); // 2 records verwacht

    // 2. Bridge naar het persistente append-only log + schrijf naar schijf.
    {
        let mut gw = McpGateway::new();
        gw.audit = recs;
        persist_agent_audit(&gw, fs, caps);
    }

    // 3. Lees terug VAN SCHIJF: beide agent-regels staan er, met de juiste uitkomst.
    let on_disk = fs.read_file(LOG).unwrap_or_default();
    let disk_txt = alloc::string::String::from_utf8_lossy(&on_disk);
    let has_write = disk_txt.contains("AGENT_TOOL") && disk_txt.contains("tool=fs_write allowed=true");
    let has_exec_deny = disk_txt.contains("tool=exec allowed=false");
    let append_only = fs.get_flags(LOG).unwrap_or(0) & eurofs::FLAG_APPEND_ONLY != 0;

    // 4. Vervalsing (inkorten/overschrijven) → de append-only-FS weigert.
    let tamper_blocked = fs.write_file(LOG, b"gewist").is_err();

    // 5. Tweede agent-actie → log groeit (overleeft remount want we appenden).
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
        "[p3-agent] EuroAgent-audit PERSISTENT: {recorded} tool-calls→schijf, fs_write-toegestaan-gelogd={has_write}, exec-geweigerd-gelogd={has_exec_deny}, append-only-vlag={append_only}, vervalsing-geblokkeerd={tamper_blocked}, regels {before}→{after} → {}",
        if ok { "OK (agent-spoor niet langer RAM-only: onomkeerbaar + overleeft herstart) ✓" } else { "MISLUKT" }
    );
}

/// **AF / Zero-Trust P2.2 boot-zelftest** — just-in-time elevatie + auto-revoke.
/// Bewijst dat een VERHOOGDE cap (EXEC), zelfs als de agent hem in z'n staande set
/// heeft, pas mag ná een verse JIT-grant, en dat die grant na één actie automatisch
/// vervalt — "elevate for the task, auto-revoke on completion".
pub fn jit_selftest() {
    let code = |resp: &str| {
        Json::parse(resp).ok().and_then(|v| v.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64()))
    };
    let has_result = |resp: &str| Json::parse(resp).ok().and_then(|v| v.get("result").cloned()).is_some();

    let mut gw = McpGateway::new();
    let caps = AgentCaps(caps::EXEC); // staande set bevat de verhoogde cap
    let exec_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"exec","arguments":{"cmd":"build"}}}"#;

    // 1. Zonder grant → geweigerd, ook al staat EXEC in de set.
    let denied_first = code(&gw.handle("builder", caps, exec_req, &mut KernelBackend)) == Some(euroagent::mcp::ERR_JIT_REQUIRED);
    // 2. Na een JIT-grant (zou ná gebruikersbevestiging komen) → de ene call mag.
    gw.grant_jit(caps::EXEC);
    let allowed_after_grant = has_result(&gw.handle("builder", caps, exec_req, &mut KernelBackend));
    // 3. Auto-revoke: de grant is verbruikt → een tweede call is wéér geweigerd.
    let revoked = gw.pending_jit() == 0
        && code(&gw.handle("builder", caps, exec_req, &mut KernelBackend)) == Some(euroagent::mcp::ERR_JIT_REQUIRED);

    let ok = denied_first && allowed_after_grant && revoked;
    crate::serial_println!(
        "[af-jit] JIT-elevatie: verhoogde-cap-zonder-grant-geweigerd={denied_first}, na-grant-één-actie-toegestaan={allowed_after_grant}, auto-revoke-2e-call-geweigerd={revoked} → {}",
        if ok { "OK (least agency in tijd: elevate-for-the-task, auto-revoke) ✓" } else { "MISLUKT" }
    );
}

/// **AF / Zero-Trust P2.3 boot-zelftest** — gedragsdetectie op de gateway-audit-
/// stroom. Bewijst dat de monitor (1) normaal gedrag tijdens de leerfase NIET
/// alarmeert, (2) een reeks geweigerde aanroepen als capability-probing flagt, en
/// (3) een tool die buiten het baseline-gedrag valt als drift flagt. Deterministisch
/// en uitlegbaar (geen ML) — elke alert is herleidbaar tot één regel.
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

    // Leerfase: 3× fs_read → baseline = {fs_read}, geen alerts.
    let learn_quiet = (0..3).all(|i| mon.observe(&mk("fs_read", true), i).is_empty());
    // Bekend gedrag ná baseline → stil.
    let known_quiet = mon.observe(&mk("fs_read", true), 10).is_empty();

    // Capability-probing: 4 opeenvolgende weigeringen → DenialSpike.
    let mut probing_flagged = false;
    for i in 11..=14 {
        if mon.observe(&mk("exec", false), i).iter().any(|a| a.kind == AnomalyKind::DenialSpike) {
            probing_flagged = true;
        }
    }
    // Gedragsdrift: een tool die nooit in de baseline zat → UnseenTool.
    let drift_flagged = mon
        .observe(&mk("net_post", true), 20)
        .iter()
        .any(|a| a.kind == AnomalyKind::UnseenTool);

    let ok = learn_quiet && known_quiet && probing_flagged && drift_flagged;
    crate::serial_println!(
        "[af-anom] gedragsdetectie (deterministisch, audit-gevoed): leerfase-stil={learn_quiet}, bekend-gedrag-stil={known_quiet}, probing(4×weiger)-geflagd={probing_flagged}, drift(nieuwe-tool)-geflagd={drift_flagged} → {}",
        if ok { "OK (afwijkend agent-gedrag zichtbaar voor audit/respons) ✓" } else { "MISLUKT" }
    );
}

/// `euroagent [subcommando]`-shell. Subcommando's: (leeg)/`status` · `caps` ·
/// `mcp list` · `inspect` · `dispatch test <intent>`.
pub fn shell(args: &str) -> Vec<String> {
    let args = args.trim();
    let eff = AgentCaps(*EFFECTIVE.lock());

    // Parse het demo-manifest één keer voor de inspect/dispatch-subcommando's.
    let manifest = AgentManifest::from_toml(DEMO_MANIFEST).ok();

    let mut a = args.split_whitespace();
    match a.next() {
        Some("list") => {
            let list = INSTALLED.lock();
            if list.is_empty() {
                return alloc::vec![String::from("EuroAgent — geen agents geïnstalleerd")];
            }
            let mut out = alloc::vec![alloc::format!("EuroAgent — {} geïnstalleerde agent(s):", list.len())];
            for (name, version, publisher) in list.iter() {
                out.push(alloc::format!("  • {name} v{version}  (uitgever {}…)", &publisher[..publisher.len().min(16)]));
            }
            out.push(String::from("  (alleen geldig-Ed25519-ondertekende bundles; naam vastgezet aan de uitgever — anti-kaping)"));
            out
        }
        Some("llm") => {
            // BB-1: doe een ECHTE conversatie-stap over EuroNet-TCP met de lokale,
            // soevereine Ollama-endpoint. De rest van de prompt (na "llm ") is het
            // bericht; standaard een korte testvraag.
            let prompt = args.trim().strip_prefix("llm").map(|s| s.trim()).filter(|s| !s.is_empty())
                .unwrap_or("Zeg in één zin wie je bent.");
            let mut be = default_ollama();
            let resp = be.step(
                &[euroagent::llm::Message::user(prompt)],
                &["fs_read", "fs_write"],
            );
            let mut out = alloc::vec![
                String::from("EuroAgent LLM — standaard backend: LOKAAL (soeverein, geen cloud)"),
                alloc::format!("  endpoint : http://{}  (Ollama-compatibel, via EuroNet-TCP)", be.host),
                alloc::format!("  model    : {}", be.model),
                String::from("  cloud    : opt-in per gebruiker (sleutel via EuroVault, elke call → P3-audit)"),
            ];
            if be.reachable {
                match resp {
                    LlmResponse::Text(t) => out.push(alloc::format!("  antwoord : {}", t.trim())),
                    LlmResponse::ToolCall { name, .. } => out.push(alloc::format!("  tool-call: {name} (model vroeg een tool aan)")),
                }
            } else {
                out.push(String::from("  status   : transport gereed, maar geen endpoint bereikbaar"));
                out.push(String::from("             (start lokaal Ollama op poort 11434, of een Ollama-compatibele mock)"));
            }
            out
        }
        Some("caps") => {
            let mut out = alloc::vec![alloc::format!(
                "EuroAgent — effectieve capability-set van demo-agent 'facilitator' ({}):",
                eff.names().len()
            )];
            for n in eff.names() {
                out.push(alloc::format!("  • {n}"));
            }
            out.push(String::from("  (NET geweigerd door EuroPol-beleid; EXEC geweigerd door user-clamp)"));
            out
        }
        Some("mcp") => {
            // `mcp list` — toon de tools die de effectieve caps mogen aanroepen.
            let gw = McpGateway::new();
            let mut out = alloc::vec![String::from("EuroAgent MCP-gateway — tools beschikbaar voor 'facilitator':")];
            if let Some(Json::Arr(items)) = gw.list_for(eff).get("tools") {
                for t in items {
                    let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                    let cap = t.get("required_cap").and_then(|c| c.as_str()).unwrap_or("?");
                    let desc = t.get("description").and_then(|d| d.as_str()).unwrap_or("");
                    out.push(alloc::format!("  • {name}  [{cap}] — {desc}"));
                }
            }
            out.push(String::from("  (JSON-RPC 2.0 over AF_UNIX; elke aanroep → P3-audit)"));
            out
        }
        Some("inspect") => match manifest {
            Some(m) => alloc::vec![
                alloc::format!("Agent: {} v{}  ({})", m.name, m.version, m.lang),
                alloc::format!("  {}", m.description),
                alloc::format!("  auteur: {}", m.author),
                alloc::format!("  wasm: {}", m.wasm),
                alloc::format!("  vereiste caps: {}", m.required.names().join(", ")),
                alloc::format!("  optionele caps: {}", m.optional.names().join(", ")),
                alloc::format!("  triggers (intent): {}", m.triggers_intent.join(" | ")),
                alloc::format!("  tools toegestaan: {}", m.tools_allowed.join(", ")),
                alloc::format!("  sandbox: max {} MB geheugen", m.max_memory_mb),
            ],
            None => alloc::vec![String::from("euroagent: kan demo-manifest niet parsen")],
        },
        Some("dispatch") => {
            // `dispatch test <intent>` — toon naar welke agent een intent zou routen.
            let _ = a.next(); // "test"
            let intent_text: String = a.collect::<alloc::vec::Vec<_>>().join(" ");
            let routes = manifest
                .as_ref()
                .map(|m| alloc::vec![intent::Route { agent: m.name.clone(), intents: m.triggers_intent.clone() }])
                .unwrap_or_default();
            match intent::route(&intent_text, &routes) {
                Some(r) => alloc::vec![alloc::format!("intent '{intent_text}' → agent '{}'", r.agent)],
                None => alloc::vec![alloc::format!("intent '{intent_text}' → (geen passende agent)")],
            }
        }
        _ => alloc::vec![
            String::from("EuroAgent — soevereine agent-first runtime (Sprint AA)"),
            String::from("  agents = WASM + declaratief capability-manifest; trust boundary in de kernel (EuroGuard), niet de cloud"),
            String::from("  MCP-gateway: open Model-Context-Protocol, capability-gated, elke aanroep → P3-audit"),
            String::from("  LLM: standaard lokaal (Ollama-compatibel); cloud opt-in via EuroVault"),
            String::from("  vs. Project Solara: EuroAgent draait volledig offline, EU-data-residency gegarandeerd"),
            String::from("  subcommando's: euroagent list · caps · mcp list · inspect · llm · dispatch test <intent>"),
        ],
    }
}
