# EuroOS — Sprint AA: EuroAgent
*Sovereign Agent-First Runtime*

**Status:** ⬜ niet gestart  
**Prioriteit:** hoog — strategisch differentiator  
**Afhankelijkheden:** H4 (WASM/WASI), L1 (immutable flags), P3 (audit log), U (EuroVault), X (EuroPol)  
**Geschatte sessies:** 4–5  
**Kind:** `N 🔒 🏗️` — nieuw subsysteem, security-critical, grote architecturale lever

---

## Context: waarom dit nu strategisch relevant is

Microsoft kondigde op Build 2026 **Project Solara** aan: een Android-gebaseerd platform specifiek gebouwd voor *agent-first devices* — apparaten waarop AI-agents de primaire interactie-eenheid zijn in plaats van traditionele apps. De kerngedachte: agents worden de nieuwe eenheid van programmering én de nieuwe eenheid van mens-machine interactie. Apparaten worden niet meer gebouwd rondom apps, maar rondom agents.

Solara's aanpak: een Android (AOSP) basis + cloud identity (Entra ID/Microsoft) + agent shell bovenop. De sovereignty is een marketingclaim, niet een architecturale garantie: de trust boundary ligt bij Microsoft's cloud, niet bij het apparaat zelf.

**EuroOS heeft een unieke positie:** de architecturale bouwstenen voor een sovereigner agent model zijn al aanwezig of gepland — EuroGuard capabilities, WASM/WASI sandbox (H4), EuroVault (U), EuroPol (X), TPM (O1). Sprint AA assembleert deze tot een coherente **EuroAgent runtime** waarbij de trust boundary in de kernel zit, niet in een Amerikaanse cloud.

Het positioneringsverhaal: **EuroOS is het enige OS waar AI-agents by design capability-isolated draaien op kernelniveau, met volledige audit trail, zonder afhankelijkheid van externe identity providers.**

---

## Doelstelling

Een volledige **EuroAgent runtime** die:

1. Agents definieert als WASM modules met een declaratief capability manifest
2. Capability-isolatie op kernelniveau afdwingt via EuroGuard (agents krijgen alleen wat ze declareren)
3. Een native **MCP gateway** exposeert zodat agents tools kunnen aanroepen op EuroFS, EuroNet, EuroVault en EuroDisplay
4. Een **EuroDispatch** orchestrator biedt voor multi-agent coördinatie
5. Elke agent-actie logt naar P3 (audit trail) met agent identity + gebruikte capability
6. Volledig offline werkt — geen cloud dependency voor basisgedrag

---

## Architectuur

```
kernel/euroagent/
├── mod.rs              // EuroAgentManager: publieke kernel API
├── manifest.rs         // AgentManifest: TOML parser + validator
├── loader.rs           // Agent laden: WASM module + manifest verifiëren
├── caps.rs             // AgentCapabilitySet: subset van EuroGuard caps
├── sandbox.rs          // AgentSandbox: WASM instantie + capability enforcement
├── audit.rs            // AgentAuditWriter: elke actie → P3 audit log
└── ipc.rs              // Agent ↔ kernel IPC via EuroIPC bridge (syscall 500–502)

userspace/euroagent/
├── dispatch/
│   ├── mod.rs          // EuroDispatch: agent orchestrator daemon
│   ├── scheduler.rs    // Agent scheduling: triggers, timers, events
│   ├── router.rs       // Intent routing: welke agent handelt welk verzoek af
│   └── context.rs      // AgentContext: gedeelde toestand tussen agents
├── mcp/
│   ├── mod.rs          // MCP gateway server (Model Context Protocol)
│   ├── server.rs       // MCP JSON-RPC over AF_UNIX socket
│   ├── tools/
│   │   ├── fs.rs       // Tool: lees/schrijf EuroFS (cap: CAP_AGENT_FS_*)
│   │   ├── net.rs      // Tool: HTTP/HTTPS requests (cap: CAP_AGENT_NET)
│   │   ├── vault.rs    // Tool: EuroVault secrets (cap: CAP_AGENT_VAULT)
│   │   ├── display.rs  // Tool: EuroDisplay notificaties (cap: CAP_AGENT_DISPLAY)
│   │   ├── calendar.rs // Tool: agenda lezen/schrijven (cap: CAP_AGENT_CALENDAR)
│   │   └── exec.rs     // Tool: commando uitvoeren (cap: CAP_AGENT_EXEC)
│   └── registry.rs     // Tool registry: welke tools zijn beschikbaar
├── registry/
│   ├── mod.rs          // AgentRegistry: geïnstalleerde agents beheren
│   ├── store.rs        // On-disk agent store (EuroFS, content-addressed)
│   └── verify.rs       // Ed25519 handtekening verificatie van agent bundles
└── cli/
    └── euroagent.rs    // Shell: euroagent install/list/run/stop/logs/inspect
```

---

## Kerndata-structuren

### AgentManifest

Het manifest beschrijft een agent volledig declaratief. Het is een TOML-bestand meegeleverd in de agent bundle, Ed25519-gesigneerd samen met de WASM binary.

```toml
# Voorbeeld: vergaderassistent agent
[agent]
name        = "facilitator"
version     = "1.0.0"
description = "Vergaderingen opnemen, transcriberen en samenvatten"
author      = "GoTrust BV <agent@gotrust.eu>"
wasm        = "facilitator.wasm"
lang        = "nl-BE"                       # P1 locale binding

[capabilities]
# Declaratief: agent vraagt exact wat hij nodig heeft
required = [
    "CAP_AGENT_MIC",          # Microfoon toegang
    "CAP_AGENT_FS_WRITE",     # Opslaan van transcripties
    "CAP_AGENT_DISPLAY",      # Notificaties sturen
]
optional = [
    "CAP_AGENT_NET",          # Optioneel: cloud transcriptie API
    "CAP_AGENT_CALENDAR",     # Optioneel: vergadering metadata
]

[triggers]
# Wanneer wordt de agent automatisch geactiveerd?
on_event    = ["calendar.meeting_start", "user.mic_hotkey"]
on_schedule = []                            # Geen geplande runs
on_intent   = ["vergadering opnemen", "start recording"]

[tools]
# Welke MCP tools mag deze agent aanroepen?
allowed = ["mic_record", "fs_write", "display_notify", "calendar_read"]
denied  = ["exec", "vault_read", "net_post"]  # Expliciet verboden

[sandbox]
max_memory_mb   = 64
max_runtime_ms  = 0                         # 0 = onbeperkt (long-running)
max_fs_write_mb = 512
network_domains = []                        # Leeg = geen netwerk (tenzij CAP_AGENT_NET)

[audit]
log_tool_calls  = true                      # Elke MCP tool call → P3 log
log_inputs      = false                     # Privacy: inputs niet loggen
log_outputs     = false                     # Privacy: outputs niet loggen
```

### AgentCapabilitySet

```rust
/// Fijnmazige capabilities specifiek voor agents.
/// Dit zijn subsets van EuroGuard capabilities, verder opgesplitst
/// voor het principle of least privilege op agent-niveau.
bitflags! {
    pub struct AgentCaps: u64 {
        // Opslag
        const FS_READ       = 1 << 0;  // EuroFS lezen binnen agent sandbox
        const FS_WRITE      = 1 << 1;  // EuroFS schrijven binnen agent sandbox
        const FS_READ_GLOBAL= 1 << 2;  // EuroFS lezen buiten sandbox (privileged)
        const VAULT_READ    = 1 << 3;  // EuroVault secrets lezen
        const VAULT_WRITE   = 1 << 4;  // EuroVault secrets schrijven

        // Netwerk
        const NET_GET       = 1 << 8;  // HTTP/HTTPS GET
        const NET_POST      = 1 << 9;  // HTTP/HTTPS POST/PUT/DELETE
        const NET_LISTEN    = 1 << 10; // Inkomende verbindingen aanvaarden

        // Hardware
        const MIC           = 1 << 16; // Microfoon
        const CAMERA        = 1 << 17; // Camera
        const SPEAKER       = 1 << 18; // Luidspreker output

        // Systeem
        const DISPLAY       = 1 << 24; // EuroDisplay notificaties + windows
        const CALENDAR      = 1 << 25; // Agenda lezen/schrijven
        const EXEC          = 1 << 26; // Subprocessen starten (zeer privileged)
        const AGENT_SPAWN   = 1 << 27; // Andere agents spawnen
        const IPC_SEND      = 1 << 28; // Berichten sturen naar andere agents
    }
}

/// Een actieve agent instantie in de kernel.
pub struct AgentInstance {
    pub id: AgentId,
    pub manifest: AgentManifest,
    pub caps: AgentCaps,                    // Verleende capabilities (≤ requested)
    pub wasm_instance: WasmInstance,        // WASM sandbox (H4)
    pub state: AgentState,                  // Idle | Running | Suspended | Stopped
    pub spawned_by: Option<AgentId>,        // Welke agent of gebruiker spawnte deze
    pub user_id: UserId,                    // Owner (K1 user model)
    pub audit_ctx: AuditContext,            // P3 audit context
}

pub enum AgentState {
    Idle,
    Running { started_at: Timestamp, tool_call: Option<McpToolId> },
    Suspended { reason: SuspendReason },
    Stopped { exit_code: i32, stopped_at: Timestamp },
}
```

### AgentSandbox (integratie met H4 WASM)

```rust
/// De WASM sandbox voor een agent.
/// Wraps de H4 WASM instantie met capability enforcement.
pub struct AgentSandbox {
    wasm: WasmInstance,
    caps: AgentCaps,
    fs_root: PathBuf,           // /agents/<agent_id>/ — eigen sandbox directory
    net_filter: NetFilter,      // Welke domeinen zijn bereikbaar
    mem_limit: u64,             // Harde memory limiet via page fault handler
    call_log: Vec<McpToolCall>, // Recente tool calls (voor audit)
}

impl AgentSandbox {
    /// Roept een MCP tool aan na capability verificatie.
    /// Elke aanroep wordt gelogd naar P3 audit log.
    pub fn call_tool(
        &mut self,
        tool: &McpToolId,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, AgentError> {
        // 1. Controleer of agent de capability heeft voor deze tool
        let required_cap = tool.required_cap();
        if !self.caps.contains(required_cap) {
            // Log de geweigerde aanroep naar P3
            audit::log_denied(self, tool, "insufficient_capability");
            return Err(AgentError::CapabilityDenied(required_cap));
        }

        // 2. Log de aanroep (voor audit trail)
        audit::log_tool_call(self, tool, input);

        // 3. Voer de tool uit via de MCP gateway
        let result = MCP_GATEWAY.execute(tool, input, &self.caps)?;

        // 4. Log het resultaat (metadata, niet de waarde zelf)
        audit::log_tool_result(self, tool, result.is_ok());

        result
    }
}
```

---

## MCP Gateway — tools

De MCP gateway exposeert EuroOS subsystemen als tools die agents kunnen aanroepen via JSON-RPC over een AF_UNIX socket. Dit is het standaard Model Context Protocol (MCP) — compatibel met elke MCP-client (Claude, lokale LLMs via Ollama, etc.).

### MCP socket locatie
`/run/euroagent/mcp.sock` — alleen bereikbaar voor processen met `CAP_AGENT_*`

### Beschikbare tools

```json
// Toollijst zoals gerapporteerd door de MCP server
{
  "tools": [
    {
      "name": "fs_read",
      "description": "Lees een bestand van EuroFS binnen de agent sandbox",
      "required_cap": "CAP_AGENT_FS_READ",
      "input_schema": {
        "type": "object",
        "properties": {
          "path": { "type": "string", "description": "Pad relatief aan agent sandbox" },
          "encoding": { "type": "string", "enum": ["utf8", "base64"], "default": "utf8" }
        },
        "required": ["path"]
      }
    },
    {
      "name": "fs_write",
      "description": "Schrijf data naar EuroFS binnen de agent sandbox",
      "required_cap": "CAP_AGENT_FS_WRITE",
      "input_schema": {
        "type": "object",
        "properties": {
          "path": { "type": "string" },
          "content": { "type": "string" },
          "encoding": { "type": "string", "enum": ["utf8", "base64"], "default": "utf8" },
          "append": { "type": "boolean", "default": false }
        },
        "required": ["path", "content"]
      }
    },
    {
      "name": "net_get",
      "description": "HTTP/HTTPS GET request via EuroNet + EuroTLS",
      "required_cap": "CAP_AGENT_NET_GET",
      "input_schema": {
        "type": "object",
        "properties": {
          "url": { "type": "string" },
          "headers": { "type": "object" },
          "timeout_ms": { "type": "integer", "default": 30000 }
        },
        "required": ["url"]
      }
    },
    {
      "name": "vault_get",
      "description": "Lees een secret uit EuroVault (waarde nooit in audit log)",
      "required_cap": "CAP_AGENT_VAULT_READ",
      "input_schema": {
        "type": "object",
        "properties": {
          "secret_id": { "type": "string" }
        },
        "required": ["secret_id"]
      }
    },
    {
      "name": "display_notify",
      "description": "Stuur een notificatie naar EuroDisplay",
      "required_cap": "CAP_AGENT_DISPLAY",
      "input_schema": {
        "type": "object",
        "properties": {
          "title": { "type": "string" },
          "body": { "type": "string" },
          "priority": { "type": "string", "enum": ["low", "normal", "high", "urgent"] },
          "action": {
            "type": "object",
            "properties": {
              "label": { "type": "string" },
              "intent": { "type": "string" }
            }
          }
        },
        "required": ["title", "body"]
      }
    },
    {
      "name": "agent_spawn",
      "description": "Spawn een andere agent (sub-agent voor delegatie)",
      "required_cap": "CAP_AGENT_SPAWN",
      "input_schema": {
        "type": "object",
        "properties": {
          "agent_name": { "type": "string" },
          "intent": { "type": "string" },
          "caps_requested": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["agent_name", "intent"]
      }
    },
    {
      "name": "calendar_read",
      "description": "Lees agenda events (EuroIDM calendar backend)",
      "required_cap": "CAP_AGENT_CALENDAR",
      "input_schema": {
        "type": "object",
        "properties": {
          "from": { "type": "string", "format": "date-time" },
          "to": { "type": "string", "format": "date-time" },
          "include_details": { "type": "boolean", "default": false }
        }
      }
    }
  ]
}
```

---

## EuroDispatch — agent orchestrator

EuroDispatch is een userspace daemon die agents coördineert. Het is bewust **geen AI-systeem zelf** — het is een deterministisch, auditeerbaar routingsysteem.

```rust
/// EuroDispatch: centrale agent orchestrator.
/// Draait als geprivilegieerde userspace process met CAP_AGENT_SPAWN.
pub struct EuroDispatch {
    registry: AgentRegistry,        // Alle geïnstalleerde agents
    active: HashMap<AgentId, AgentInstance>,
    intent_router: IntentRouter,    // Intent → agent mapping
    event_bus: EventBus,            // Systeem events (calendar, mic hotkey, ...)
    audit: AuditWriter,             // P3 audit log
}

impl EuroDispatch {
    /// Verwerk een user intent (spraak, tekst, of systeemevent).
    /// Selecteert de beste agent en delegeert.
    pub fn handle_intent(&mut self, intent: &Intent) -> Result<AgentId, DispatchError> {
        // 1. Bepaal welke agent dit intent kan afhandelen
        let candidates = self.intent_router.route(intent);

        // 2. Kies de beste kandidaat (score op basis van declareerde triggers)
        let best = candidates.into_iter()
            .max_by_key(|a| a.intent_match_score(intent))
            .ok_or(DispatchError::NoAgentFound)?;

        // 3. Vraag gebruikersbevestiging als agent elevated caps wil
        if best.caps.contains(AgentCaps::EXEC | AgentCaps::VAULT_WRITE) {
            self.request_user_confirmation(&best, intent)?;
        }

        // 4. Start de agent
        let id = self.spawn_agent(&best, intent)?;
        audit::log_dispatch(intent, &best, id);
        Ok(id)
    }

    /// Multi-agent workflow: orchestreer meerdere agents sequentieel of parallel.
    pub fn run_workflow(&mut self, workflow: &AgentWorkflow) -> Result<WorkflowResult, DispatchError> {
        let mut context = AgentContext::new();

        for step in &workflow.steps {
            match step.execution {
                Execution::Sequential => {
                    let result = self.run_step(step, &mut context)?;
                    context.store_result(&step.id, result);
                }
                Execution::Parallel(ref steps) => {
                    // Parallel steps: elk in eigen sandbox, resultaten samenvoegen
                    let results: Vec<_> = steps.iter()
                        .map(|s| self.run_step(s, &mut context))
                        .collect();
                    context.merge_parallel_results(results)?;
                }
            }
        }

        Ok(context.into_result())
    }
}
```

### Intent routing

```toml
# /etc/euroagent/dispatch.toml — intent routing configuratie
# Operators kunnen dit aanpassen per deployment

[routes]
# Intent pattern → agent naam (regex op intent tekst)
"vergader.*opnemen|start.*recording|record.*meeting" = "facilitator"
"agenda|kalender|calendar|meetings vandaag"          = "calendar-assistant"
"incident.*melden|nis2.*incident|security.*breach"   = "incident-reporter"
"password|wachtwoord|secret|api.key"                 = "vault-assistant"
"systeem.*status|health|gezondheid.*server"          = "health-monitor"

[priority_agent]
# Welke agent heeft altijd toegang tot de "wat nu?" context?
name = "priority"
always_active = true
max_background_memory_mb = 16
```

---

## Sovereign LLM integratie

EuroOS koppelt agents aan **lokale LLMs** via een gestandaardiseerde backend interface — geen verplichte cloud dependency.

```rust
/// LLM backend trait: lokaal of cloud, transparant voor agents.
pub trait LlmBackend: Send + Sync {
    fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[McpTool],             // MCP tools doorgeven aan LLM
        caps: &AgentCaps,              // Capability filter voor tool calls
    ) -> Result<LlmResponse, LlmError>;
}

/// Lokale LLM via Ollama-compatible API (sovereign, geen cloud).
pub struct LocalLlmBackend {
    endpoint: String,           // bijv. "http://localhost:11434"
    model: String,              // bijv. "mistral:7b", "llama3:8b"
    context_window: usize,
}

/// Cloud LLM als optionele backend (user kiest bewust).
pub struct CloudLlmBackend {
    provider: CloudProvider,    // Anthropic, OpenAI, Mistral, ...
    api_key_secret: SecretId,   // Via EuroVault — nooit plaintext
    endpoint: String,
    model: String,
}

/// EuroAgent gebruikt standaard de lokale backend.
/// Cloud backend is opt-in per agent, per gebruiker.
pub struct AgentLlm {
    primary: Box<dyn LlmBackend>,       // Standaard: LocalLlmBackend
    fallback: Option<Box<dyn LlmBackend>>, // Optioneel: cloud fallback
}
```

### Standaard lokale modellen

```toml
# /etc/euroagent/llm.toml
[llm]
default_backend = "local"               # Sovereign default: altijd lokaal

[llm.local]
endpoint    = "http://localhost:11434"  # Ollama-compatible
model       = "mistral:7b-instruct"    # Aanbevolen: snel, efficient, EU-getraind
context     = 32768
gpu_layers  = 0                         # 0 = CPU only; K4 (GPU) verhoogt dit later

[llm.cloud]
# Opt-in per gebruiker: standaard uitgeschakeld
enabled     = false
provider    = "anthropic"
model       = "claude-sonnet-4-20250514"
api_key     = { vault = "anthropic-api-key" }  # Via EuroVault
audit_cloud_calls = true               # Elke cloud call → P3 audit
```

---

## Vergelijking met Project Solara

| Eigenschap | Project Solara (Microsoft) | EuroAgent (EuroOS) |
|------------|---------------------------|-------------------|
| OS fundament | Android (AOSP) — niet zelf gebouwd | EuroOS kernel — volledig eigen code |
| Trust boundary | Microsoft cloud (Entra ID) | EuroOS kernel (EuroGuard) |
| Capability model | Android permissions + Intune MDM | Kernel-level AgentCaps, subsets van EuroGuard |
| Identity provider | Verplicht: Microsoft Entra ID | Optioneel: EuroIDM (lokaal LDAP/OIDC of standalone) |
| LLM backend | Verplicht: Azure/Microsoft cloud | Standaard: lokale Ollama; cloud opt-in via EuroVault |
| Audit trail | Microsoft cloud logs | EuroOS P3 kernel audit log, lokaal + tamper-evident |
| Agent sandboxing | Android process isolation | WASM/WASI + EuroGuard capability enforcement |
| MCP gateway | Proprietary agent SDK | Open MCP protocol — elke MCP client werkt |
| Offline gebruik | Beperkt (cloud-first architectuur) | Volledig — cloud is een optionele backend |
| EU data residency | Afhankelijk van Azure regio keuze | Gegarandeerd — alles lokaal tenzij gebruiker kiest voor cloud |
| Open source | Gesloten | EUPL-1.2 (Q3 governance) |
| Primaire doelgroep | Enterprise, grote organisaties | Europese overheden, KMO's, developers |

---

## Shell commando's

```bash
# Agent beheer
euroagent install <bundle.euroa>        # Installeer agent bundle (Ed25519 geverifieerd)
euroagent list                          # Alle geïnstalleerde agents + status
euroagent inspect <name>                # Toon manifest + capabilities + runtime stats
euroagent remove <name>                 # Verwijder agent + sandbox data
euroagent update <name> <bundle.euroa>  # Update agent (nieuw manifest verificatie)

# Agent uitvoering
euroagent run <name> [--intent "..."]   # Start agent met optioneel intent
euroagent stop <name>                   # Stop agent graceful
euroagent logs <name> [--follow]        # Toon agent output log
euroagent audit <name> [--last 1h]      # P3 audit trail voor deze agent

# Capability beheer
euroagent grant <name> CAP_AGENT_NET    # Verleen extra capability (vereist user confirm)
euroagent revoke <name> CAP_AGENT_MIC   # Trek capability in (directe werking)
euroagent caps <name>                   # Toon effectieve capability set

# MCP gateway
euroagent mcp list                      # Beschikbare MCP tools
euroagent mcp call <tool> <json>        # Handmatig een tool aanroepen (debug)
euroagent mcp inspect <tool>            # Toon tool schema + required caps

# LLM configuratie
euroagent llm status                    # Actieve backend + model info
euroagent llm test                      # Test lokale LLM connectie
euroagent llm set-cloud --provider anthropic  # Cloud backend instellen

# Dispatch
euroagent dispatch list                 # Actieve intent routes
euroagent dispatch test "vergadering opnemen"  # Simuleer intent routing
euroagent dispatch reload               # Herlaad dispatch configuratie
```

---

## Implementatiestappen

### Stap 1 — AgentManifest + registry (host-testbaar)

```rust
// Volledig host-testbaar: geen kernel of WASM nodig
// src: userspace/euroagent/registry/

#[test]
fn manifest_parse_valid() {
    let toml = include_str!("fixtures/facilitator.toml");
    let manifest = AgentManifest::from_toml(toml).unwrap();
    assert_eq!(manifest.name, "facilitator");
    assert!(manifest.capabilities.required.contains(&AgentCap::Mic));
}

#[test]
fn manifest_rejects_unknown_cap() {
    let bad = r#"[capabilities]
    required = ["CAP_AGENT_KERNEL_PANIC"]  # Niet bestaand
    "#;
    assert!(AgentManifest::from_toml(bad).is_err());
}

#[test]
fn manifest_rejects_missing_required_fields() {
    let incomplete = r#"[agent]
    name = "test"
    # version ontbreekt
    "#;
    assert!(AgentManifest::from_toml(incomplete).is_err());
}
```

### Stap 2 — AgentCapabilitySet + EuroGuard integratie

Voeg `AgentCaps` toe aan EuroGuard als sub-namespace van de bestaande capability flags. Een agent-process krijgt:
- De capabilities die in zijn manifest staan (alleen `required`, `optional` pas na user grant)
- Minus alle caps die de parent user niet zelf heeft
- Minus alle caps die EuroPol (Sprint X) heeft verboden voor dit agent-type

```rust
// kernel/euroguard/agent_caps.rs
impl EuroGuard {
    /// Maak een capability set voor een nieuwe agent instantie.
    /// Altijd een SUBSET van de parent user's capabilities.
    pub fn agent_capability_set(
        &self,
        manifest: &AgentManifest,
        parent_user: UserId,
        granted_optional: &[AgentCap],
    ) -> Result<AgentCaps, GuardError> {
        let user_caps = self.user_caps(parent_user)?;
        let requested = manifest.capabilities.required_as_set();
        let optional = AgentCaps::from_slice(granted_optional);

        // Veiligheidsregel: agent kan nooit meer dan parent user
        let effective = (requested | optional) & user_caps.to_agent_caps();

        // Controleer EuroPol restricties
        if let Some(denied) = EUROPOL.check_agent_policy(manifest, effective) {
            return Err(GuardError::PolicyDenied(denied));
        }

        Ok(effective)
    }
}
```

### Stap 3 — MCP gateway server

```rust
// userspace/euroagent/mcp/server.rs
// JSON-RPC 2.0 over AF_UNIX socket

pub struct McpServer {
    socket_path: PathBuf,       // /run/euroagent/mcp.sock
    tools: Vec<McpTool>,
    gateway: Arc<McpGateway>,
}

impl McpServer {
    pub fn run(&self) -> ! {
        let listener = UnixListener::bind(&self.socket_path).unwrap();

        loop {
            let (conn, _) = listener.accept().unwrap();
            let gateway = self.gateway.clone();
            let tools = self.tools.clone();

            // Elke agent-verbinding in eigen thread
            // Capability check gebeurt per tool call, niet per verbinding
            spawn(move || handle_mcp_connection(conn, gateway, tools));
        }
    }
}

fn handle_mcp_connection(conn: UnixStream, gateway: Arc<McpGateway>, tools: Vec<McpTool>) {
    let agent_id = authenticate_agent(&conn); // Via SO_PEERCRED

    loop {
        let request: JsonRpcRequest = read_json(&conn);

        let response = match request.method.as_str() {
            "tools/list"    => JsonRpcResponse::ok(request.id, json!({"tools": tools})),
            "tools/call"    => {
                let tool_name = request.params["name"].as_str().unwrap();
                let input = &request.params["arguments"];
                match gateway.call(agent_id, tool_name, input) {
                    Ok(result)  => JsonRpcResponse::ok(request.id, result),
                    Err(e)      => JsonRpcResponse::err(request.id, e.to_json()),
                }
            }
            unknown => JsonRpcResponse::err(request.id, format!("unknown method: {}", unknown)),
        };

        write_json(&conn, &response);
    }
}
```

### Stap 4 — EuroDispatch orchestrator

Zie architectuur hierboven. Focus op:
1. Intent routing via regex-tabel (TOML configuratie)
2. Agent spawnen via kernel EuroAgentManager
3. Sequentiële workflows (parallel na verificatie)
4. User confirmation dialog voor elevated cap requests

### Stap 5 — Lokale LLM integratie + WASM agent loop

```rust
// De agent loop: LLM ↔ MCP tools ↔ sandbox
pub async fn run_agent_loop(
    llm: &AgentLlm,
    mcp: &McpServer,
    manifest: &AgentManifest,
    initial_intent: &str,
) -> AgentResult {
    let mut messages = vec![
        Message::system(&manifest.system_prompt()),
        Message::user(initial_intent),
    ];

    let tools = mcp.tools_for_caps(&manifest.capabilities.all());

    loop {
        // LLM genereer response
        let response = llm.complete(&manifest.system_prompt(), &messages, &tools).await?;

        match response {
            LlmResponse::Text(text) => {
                // Agent is klaar, geef resultaat terug
                return AgentResult::Done(text);
            }
            LlmResponse::ToolCall(call) => {
                // LLM wil een tool aanroepen
                let result = mcp.execute_tool_call(&call).await;
                messages.push(Message::tool_result(call.id, result));
                // Loop verder: LLM verwerkt tool resultaat
            }
            LlmResponse::Error(e) => return AgentResult::Error(e),
        }
    }
}
```

---

## Agent bundle formaat

Een agent bundle (`.euroa`) is een ondertekend archief dat alle nodige bestanden bevat:

```
facilitator.euroa (tar.zst + Ed25519 signature)
├── MANIFEST.toml           // AgentManifest
├── MANIFEST.sig            // Ed25519 handtekening van MANIFEST.toml
├── facilitator.wasm        // WASM binary (gecompileerd vanuit Rust/Python/JS)
├── facilitator.wasm.sig    // Ed25519 handtekening van de WASM binary
├── assets/
│   ├── icon.svg            // Agent icoon voor EuroDisplay
│   └── nl-BE/
│       └── strings.toml    // Gelokaliseerde strings (P1 locale support)
└── CHANGELOG.md
```

Installatie:
```bash
# euroagent install verifieert:
# 1. Ed25519 handtekening van MANIFEST.toml
# 2. Ed25519 handtekening van .wasm binary
# 3. Alle requested capabilities zijn geldig AgentCaps
# 4. Geen capability escalation (cap subset check)
# 5. EuroPol policy check

euroagent install facilitator.euroa
# [euro/agent] Verifying bundle signature... OK
# [euro/agent] Parsing manifest... OK
# [euro/agent] Checking capabilities: CAP_AGENT_MIC, CAP_AGENT_FS_WRITE, CAP_AGENT_DISPLAY
# [euro/agent] EuroPol policy check... OK
# [euro/agent] Agent 'facilitator' installed successfully.
# [euro/agent] Optional capabilities not granted: CAP_AGENT_NET, CAP_AGENT_CALENDAR
# [euro/agent] Use 'euroagent grant facilitator <cap>' to enable optional capabilities.
```

---

## Referentie-agents (meegeleverd met EuroOS)

Vier agents die standaard meegeleverd worden als referentie-implementatie en direct nuttig zijn:

### 1. `priority` — Prioriteitsagent (altijd actief)
Toont wat nu aandacht nodig heeft: agenda, ongelezen berichten, systeem alerts. Inspired door Solara's Priority Agent maar volledig lokaal, geen cloud.
- Capabilities: `CAP_AGENT_CALENDAR`, `CAP_AGENT_DISPLAY`
- Trigger: `on_schedule = ["*/5 * * * *"]` (elke 5 minuten)

### 2. `facilitator` — Vergaderassistent
Neemt vergaderingen op, transcribeert lokaal (Whisper.cpp WASM), maakt samenvattingen.
- Capabilities: `CAP_AGENT_MIC`, `CAP_AGENT_FS_WRITE`, `CAP_AGENT_DISPLAY`
- Optioneel: `CAP_AGENT_CALENDAR` voor vergadering metadata

### 3. `incident-reporter` — NIS2 incident rapportage
Begeleidt de gebruiker door NIS2/CyFun incident meldingsprocedure. Slaat rapport op in EuroFS, optioneel versturen via EuroNet.
- Capabilities: `CAP_AGENT_FS_WRITE`, `CAP_AGENT_DISPLAY`
- Optioneel: `CAP_AGENT_NET_POST` voor melding aan CCB

### 4. `sys-monitor` — Systeembewaking
Integreert met Sprint Z (EuroHealth): geeft proactieve meldingen bij disk health issues, hoog geheugengebruik, of mislukte updates.
- Capabilities: `CAP_AGENT_DISPLAY`
- Trigger: `on_event = ["health.warning", "health.critical"]`

---

## Verify

**Host tests (geen QEMU):**
- `AgentManifest::from_toml` parset correcte manifests en weigert ongeldige
- `AgentCapabilitySet` berekening: agent krijgt nooit meer dan parent user
- MCP tool schema validatie: onbekende tools retourneren JSON-RPC error
- Intent router: correcte agent geselecteerd op basis van regex match score
- Bundle verificatie: tampered WASM binary wordt geweigerd

**QEMU boot tests:**
- `euroagent install facilitator.euroa` → agent verschijnt in `euroagent list`
- `euroagent run facilitator --intent "vergadering opnemen"` → agent start, MCP socket actief
- Agent zonder `CAP_AGENT_NET` roept `net_get` aan → `EPERM`, audit log toont geweigerde aanroep
- `euroagent revoke facilitator CAP_AGENT_MIC` → direct effect, lopende tool call gestopt
- P3 audit log bevat volledige trace van elke tool call

**Sovereignteitstest:**
- Netwerk volledig afgesloten (QEMU zonder NIC) → agent blijft volledig functioneel met lokale LLM
- Geen DNS queries naar externe servers tijdens normaal agent gebruik
- `euroagent audit` toont: 0 cloud calls in volledige sessie

---

## Haalbaarheid

**Hoog** — elk component heeft een duidelijke Rust implementatie en bouwt op bestaande EuroOS primitieven.

| Component | Afhankelijkheid | Complexiteit |
|-----------|----------------|-------------|
| AgentManifest + registry | Geen kernel | ⭐ Laag — TOML parser, host-testbaar |
| AgentCaps + EuroGuard integratie | EuroGuard (✅ aanwezig) | ⭐⭐ Middel |
| MCP gateway server | EuroNet AF_UNIX (✅ aanwezig) | ⭐⭐ Middel |
| EuroDispatch orchestrator | AgentManifest + EuroIPC | ⭐⭐ Middel |
| WASM agent loop | H4 (WASM/WASI) | ⭐⭐ Middel |
| Lokale LLM integratie | EuroNet HTTP (✅ aanwezig) | ⭐ Laag — Ollama API is simpel |
| Bundle formaat + installer | Ed25519 (✅ in eurotls) | ⭐ Laag |
| Referentie-agents | Alle bovenstaande | ⭐⭐ Middel |

**Totaal geschatte sessies:** 4–5 gerichte sessies met Claude Code.

**Kritisch pad:** AgentManifest → AgentCaps → MCP gateway → EuroDispatch → WASM loop → referentie-agents.

---

## Positionering in de hoofdroadmap

Sprint AA past na de volgende sprints:
- H4 (WASM/WASI) — de sandbox basis
- U (EuroVault) — voor `vault_get` tool en LLM API key opslag
- X (EuroPol) — voor policy enforcement op agents

En parallel aan:
- V (EuroIDM) — `calendar_read` tool gebruikt EuroIDM als backend
- W (EuroObserve) — agent metrics via OpenMetrics endpoint
- Z (EuroHealth) — `sys-monitor` referentie-agent

**Claude Code sprint commando:** `"implementeer Sprint AA stap 1"` of `"bouw de MCP gateway voor EuroAgent"` of `"schrijf de facilitator referentie-agent"`

---

*Live site: <https://euro-os.eu> · technisch overzicht: `docs/TECHNICAL-OVERVIEW.md` · sprint board: `NEXT-SPRINTS.md`*
