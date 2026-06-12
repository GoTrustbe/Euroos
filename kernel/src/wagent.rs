//! **WASM-agent-host** (AA-5 sluitstuk): de agent-*code* draait nu écht als een
//! WASM-module in de EuroWASM-interpreter, en zijn host-import wordt door de
//! cap-gated MCP-gateway naar EuroFS geleid. Dit sluit de keten: WASM-agentcode →
//! host-import → MCP-gateway (capability-poort + audit) → echte EuroFS. Zonder de
//! capability wordt de tool-aanroep geweigerd — de sandbox-grens is bewezen.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::agent::FsToolBackend;
use euroagent::caps::{self, AgentCaps};
use euroagent::json::Json;
use euroagent::mcp::McpGateway;
use eurowasm::{HostImports, Instance, Module, Val, WasmError};

// ── Mini-WASM-assembler (zelfde codering als de H4-zelftest) ────────────────
fn uleb(mut n: u32) -> Vec<u8> {
    let mut o = Vec::new();
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            o.push(b | 0x80);
        } else {
            o.push(b);
            break;
        }
    }
    o
}
fn section(id: u8, content: Vec<u8>) -> Vec<u8> {
    let mut s = vec![id];
    s.extend(uleb(content.len() as u32));
    s.extend(content);
    s
}

/// Het bericht dat de WASM-agent via zijn tool wegschrijft.
const MSG: &[u8] = b"geschreven-door-wasm-agent";

/// Bouw een WASM-agentmodule: importeert `agent.fs_write : (i32,i32)->i32` en
/// exporteert `run()->i32` die `fs_write(0, len)` aanroept en het resultaat teruggeeft.
fn build_agent_wasm() -> Vec<u8> {
    let mut w = vec![0u8, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
    // types: 0 = (i32,i32)->i32 (fs_write), 1 = ()->i32 (run)
    w.extend(section(1, vec![2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f]));
    // import agent.fs_write : type 0
    let mut im = vec![1u8, 5];
    im.extend_from_slice(b"agent");
    im.push(8);
    im.extend_from_slice(b"fs_write");
    im.extend_from_slice(&[0x00, 0]);
    w.extend(section(2, im));
    w.extend(section(3, vec![1, 1])); // 1 functie, type 1
    w.extend(section(5, vec![1, 0x00, 1])); // 1 mem-pagina
    let mut ex = vec![1u8, 3];
    ex.extend_from_slice(b"run");
    ex.extend_from_slice(&[0x00, 1]); // export "run" = func-index 1
    w.extend(section(7, ex));
    // code: fs_write(0, len); end (geeft het import-resultaat terug).
    let len = MSG.len() as u8;
    let body = vec![
        0u8, // geen locals
        0x41, 0x00, // i32.const 0  (ptr)
        0x41, len, // i32.const len
        0x10, 0x00, // call 0 (fs_write)
        0x0b, // end
    ];
    let mut code = vec![1u8];
    code.extend(uleb(body.len() as u32));
    code.extend(body);
    w.extend(section(10, code));
    w
}

/// De host die `agent.fs_write` naar de cap-gated MCP-gateway op EuroFS leidt.
struct AgentWasmHost<'a> {
    fs: &'a mut dyn eurofs::FileSystem,
    caps: AgentCaps,
    gw: McpGateway,
    granted: bool, // werd de laatste tool-call door de gateway toegestaan?
}

impl<'a> HostImports for AgentWasmHost<'a> {
    fn call(&mut self, m: &str, n: &str, args: &[Val], mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        if m == "agent" && n == "fs_write" {
            // Vertrouw de door de module gedeclareerde arity NIET (audit H7): een
            // gemaakte module kan de import met te weinig parameters declareren.
            if args.len() < 2 {
                return Err(WasmError::HostError(String::from("fs_write verwacht 2 args")));
            }
            let ptr = args[0] as usize;
            let len = args[1] as usize;
            let content = if ptr + len <= mem.len() {
                core::str::from_utf8(&mem[ptr..ptr + len]).unwrap_or("")
            } else {
                ""
            };
            // Bouw een JSON-RPC tool-call en laat de gateway 'm cap-gaten + auditen.
            let req = alloc::format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"fs_write","arguments":{{"path":"wagent.txt","content":"{content}"}}}}}}"#
            );
            let gw = &mut self.gw;
            let mut be = FsToolBackend { fs: &mut *self.fs, root: String::from("/agents/wasm"), allowed_domains: Vec::new() };
            let resp = gw.handle("wasm-agent", self.caps, &req, &mut be);
            self.granted = Json::parse(&resp).ok().map(|v| v.get("result").is_some()).unwrap_or(false);
            return Ok(vec![if self.granted { 1 } else { 0 }]);
        }
        Err(WasmError::HostError(String::from("onbekende import")))
    }
}

/// Boot-zelftest: draai de WASM-agent mét en zonder de FS_WRITE-cap.
pub fn selftest(fs: &mut dyn eurofs::FileSystem) {
    use eurofs::FileSystem;
    let _ = fs.create_dir("/agents");
    let _ = fs.create_dir("/agents/wasm");

    let bytes = build_agent_wasm();
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[aa-wasm] WASM-agent-parse mislukt: {:?}", e);
            return;
        }
    };

    // (1) MÉT FS_WRITE: de WASM-agent schrijft écht een bestand via de MCP-gateway.
    let granted_ok;
    {
        let mut inst = Instance::new(&module);
        let _ = inst.write_mem(0, MSG);
        let mut host = AgentWasmHost { fs, caps: AgentCaps(caps::FS_READ | caps::FS_WRITE), gw: McpGateway::new(), granted: false };
        let r = inst.invoke("run", &[], &mut host);
        granted_ok = r.ok().and_then(|v| v.first().copied()) == Some(1) && host.granted;
    }
    let on_disk = fs.read_file("/agents/wasm/wagent.txt").map(|d| d == MSG).unwrap_or(false);

    // (2) ZONDER FS_WRITE: de gateway weigert (capability-poort) → status 0.
    let denied_ok;
    {
        let mut inst = Instance::new(&module);
        let _ = inst.write_mem(0, MSG);
        let mut host = AgentWasmHost { fs, caps: AgentCaps(caps::FS_READ), gw: McpGateway::new(), granted: true };
        let r = inst.invoke("run", &[], &mut host);
        denied_ok = r.ok().and_then(|v| v.first().copied()) == Some(0) && !host.granted;
    }

    let ok = granted_ok && on_disk && denied_ok;
    crate::serial_println!(
        "[aa-wasm] WASM-agent-host: WASM-code→host-import→MCP-gateway→EuroFS, mét-cap-geschreven={granted_ok}, bestand-op-schijf={on_disk}, zonder-cap-geweigerd={denied_ok} → {}",
        if ok { "OK (agentcode draait in WASM-sandbox, capability-gated op kernelniveau) ✓" } else { "MISLUKT" }
    );
}
