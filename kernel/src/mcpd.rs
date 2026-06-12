//! **EuroAgent MCP-daemon** (AA-3 sluitstuk): de MCP-gateway geserveerd over een
//! échte **AF_UNIX-socket** (`/run/euroagent/mcp.sock`), niet langer een directe
//! in-proces-aanroep. Een agent-client verbindt, stuurt JSON-RPC, en de daemon
//! verwerkt het via de cap-gated [`McpGateway`] op de echte EuroFS-backend en stuurt
//! het antwoord terug. Dit is de socket-laag die de gateway tot een soevereine
//! agent-dienst maakt. (In-kernel gehost, net als de H2-displayserver; een ring-3-
//! daemon is een verdere stap — de socket-route is hier volledig.)

use alloc::string::String;

use crate::agent::FsToolBackend;
use euroagent::caps::{self, AgentCaps};
use euroagent::json::Json;
use euroagent::mcp::McpGateway;

/// Het MCP-socketpad (zoals in het EuroAgent-plan).
pub const SOCK: &str = "/run/euroagent/mcp.sock";

/// Boot-zelftest: bind de socket, laat een client een tool-call sturen, serveer 'm
/// via de gateway op EuroFS, en bewijs de round-trip + dat het bestand écht geschreven is.
pub fn selftest(fs: &mut dyn eurofs::FileSystem) {
    use eurofs::FileSystem;
    let _ = fs.create_dir("/agents");
    let _ = fs.create_dir("/agents/daemon");

    // 1. Daemon bindt de MCP-socket.
    let bound = crate::net::unix_bind_listen(SOCK, 8).is_ok();

    // 2. Een agent-client verbindt.
    let client = crate::net::unix_connect(SOCK).ok();

    // 3. Daemon accepteert de verbinding.
    let server_ep = crate::net::unix_accept(SOCK);

    // 4. Client stuurt een JSON-RPC tool-call over de socket.
    let req = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"daemon.txt","content":"via de socket"}}}"#;
    if let Some(c) = client {
        let _ = crate::net::unix_send(c, req);
    }

    // 5. Daemon leest het verzoek, verwerkt het via de cap-gated gateway op EuroFS,
    //    en stuurt het antwoord terug.
    let mut served = false;
    if let Some(s) = server_ep {
        let data = crate::net::unix_recv(s, 8192).unwrap_or_default();
        let mut gw = McpGateway::new();
        let mut be = FsToolBackend { fs, root: String::from("/agents/daemon"), allowed_domains: alloc::vec::Vec::new() };
        let resp = gw.handle(
            "daemon",
            AgentCaps(caps::FS_READ | caps::FS_WRITE),
            core::str::from_utf8(&data).unwrap_or(""),
            &mut be,
        );
        let _ = crate::net::unix_send(s, resp.as_bytes());
        served = true;
    }

    // 6. Client leest het antwoord en controleert dat het een resultaat is.
    let mut client_ok = false;
    if let Some(c) = client {
        let resp = crate::net::unix_recv(c, 8192).unwrap_or_default();
        client_ok = Json::parse(core::str::from_utf8(&resp).unwrap_or(""))
            .ok()
            .map(|v| v.get("result").is_some())
            .unwrap_or(false);
        crate::net::unix_close(c);
    }
    if let Some(s) = server_ep {
        crate::net::unix_close(s);
    }

    // 7. Bewijs dat de tool-call écht een bestand op EuroFS schreef via de socket-route.
    let on_disk = fs.read_file("/agents/daemon/daemon.txt").map(|d| d == b"via de socket").unwrap_or(false);

    let ok = bound && served && client_ok && on_disk;
    crate::serial_println!(
        "[aa-mcp] EuroAgent MCP-daemon: socket-bind={bound}, geserveerd-via-AF_UNIX={served}, client-kreeg-resultaat={client_ok}, bestand-op-EuroFS-via-socket={on_disk} → {}",
        if ok { "OK (MCP-gateway als soevereine agent-dienst over AF_UNIX) ✓" } else { "MISLUKT" }
    );
}
