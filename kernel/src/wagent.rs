//! **WASM agent host** (AA-5 capstone): the agent *code* now really runs as a
//! WASM module in the EuroWASM interpreter, and its host import is routed by the
//! cap-gated MCP gateway to EuroFS. This closes the chain: WASM agent code →
//! host import → MCP gateway (capability gate + audit) → real EuroFS. Without the
//! capability the tool call is denied — the sandbox boundary is proven.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::agent::FsToolBackend;
use euroagent::caps::{self, AgentCaps};
use euroagent::json::Json;
use euroagent::mcp::McpGateway;
use eurowasm::{HostImports, Instance, Module, Val, WasmError};

// ── Mini WASM assembler (same encoding as the H4 self-test) ────────────────
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

/// The message that the WASM agent writes out via its tool.
const MSG: &[u8] = b"written-by-wasm-agent";

/// Build a WASM agent module: imports `agent.fs_write : (i32,i32)->i32` and
/// exports `run()->i32` which calls `fs_write(0, len)` and returns the result.
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
    w.extend(section(3, vec![1, 1])); // 1 function, type 1
    w.extend(section(5, vec![1, 0x00, 1])); // 1 mem page
    let mut ex = vec![1u8, 3];
    ex.extend_from_slice(b"run");
    ex.extend_from_slice(&[0x00, 1]); // export "run" = func index 1
    w.extend(section(7, ex));
    // code: fs_write(0, len); end (returns the import result).
    let len = MSG.len() as u8;
    let body = vec![
        0u8, // no locals
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

/// The host that routes `agent.fs_write` to the cap-gated MCP gateway on EuroFS.
struct AgentWasmHost<'a> {
    fs: &'a mut dyn eurofs::FileSystem,
    caps: AgentCaps,
    gw: McpGateway,
    granted: bool, // was the last tool call allowed by the gateway?
}

impl<'a> HostImports for AgentWasmHost<'a> {
    fn call(&mut self, m: &str, n: &str, args: &[Val], mem: &mut [u8]) -> Result<Vec<Val>, WasmError> {
        if m == "agent" && n == "fs_write" {
            // Do NOT trust the arity declared by the module (audit H7): a
            // crafted module can declare the import with too few parameters.
            if args.len() < 2 {
                return Err(WasmError::HostError(String::from("fs_write expects 2 args")));
            }
            let ptr = args[0] as usize;
            let len = args[1] as usize;
            let content = if ptr + len <= mem.len() {
                core::str::from_utf8(&mem[ptr..ptr + len]).unwrap_or("")
            } else {
                ""
            };
            // Build a JSON-RPC tool call and let the gateway cap-gate + audit it.
            let req = alloc::format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"fs_write","arguments":{{"path":"wagent.txt","content":"{content}"}}}}}}"#
            );
            let gw = &mut self.gw;
            let mut be = FsToolBackend { fs: &mut *self.fs, root: String::from("/agents/wasm"), allowed_domains: Vec::new() };
            let resp = gw.handle("wasm-agent", self.caps, &req, &mut be);
            self.granted = Json::parse(&resp).ok().map(|v| v.get("result").is_some()).unwrap_or(false);
            return Ok(vec![if self.granted { 1 } else { 0 }]);
        }
        Err(WasmError::HostError(String::from("unknown import")))
    }
}

/// Boot self-test: run the WASM agent with and without the FS_WRITE cap.
pub fn selftest(fs: &mut dyn eurofs::FileSystem) {
    use eurofs::FileSystem;
    let _ = fs.create_dir("/agents");
    let _ = fs.create_dir("/agents/wasm");

    let bytes = build_agent_wasm();
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[aa-wasm] WASM agent parse failed: {:?}", e);
            return;
        }
    };

    // (1) WITH FS_WRITE: the WASM agent really writes a file via the MCP gateway.
    let granted_ok;
    {
        let mut inst = Instance::new(&module);
        let _ = inst.write_mem(0, MSG);
        let mut host = AgentWasmHost { fs, caps: AgentCaps(caps::FS_READ | caps::FS_WRITE), gw: McpGateway::new(), granted: false };
        let r = inst.invoke("run", &[], &mut host);
        granted_ok = r.ok().and_then(|v| v.first().copied()) == Some(1) && host.granted;
    }
    let on_disk = fs.read_file("/agents/wasm/wagent.txt").map(|d| d == MSG).unwrap_or(false);

    // (2) WITHOUT FS_WRITE: the gateway denies (capability gate) → status 0.
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
        "[aa-wasm] WASM agent host: WASM code→host import→MCP gateway→EuroFS, with-cap-written={granted_ok}, file-on-disk={on_disk}, without-cap-denied={denied_ok} → {}",
        if ok { "OK (agent code runs in WASM sandbox, capability-gated at kernel level) ✓" } else { "FAILED" }
    );
}
