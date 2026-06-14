//! **EuroAgent MCP daemon** (AA-3 capstone): the MCP gateway served over a
//! real **AF_UNIX socket** (`/run/euroagent/mcp.sock`), no longer a direct
//! in-process call. An agent client connects, sends JSON-RPC, and the daemon
//! processes it via the cap-gated [`McpGateway`] on the real EuroFS backend and sends
//! the response back. This is the socket layer that turns the gateway into a sovereign
//! agent service. (Hosted in-kernel, like the H2 display server; a ring-3
//! daemon is a further step — the socket route is complete here.)

use alloc::string::String;

use crate::agent::FsToolBackend;
use euroagent::caps::{self, AgentCaps};
use euroagent::json::Json;
use euroagent::mcp::McpGateway;

/// The MCP socket path (as in the EuroAgent plan).
pub const SOCK: &str = "/run/euroagent/mcp.sock";

/// Boot self-test: bind the socket, have a client send a tool call, serve it
/// via the gateway on EuroFS, and prove the round-trip + that the file was actually written.
pub fn selftest(fs: &mut dyn eurofs::FileSystem) {
    use eurofs::FileSystem;
    let _ = fs.create_dir("/agents");
    let _ = fs.create_dir("/agents/daemon");

    // 1. Daemon binds the MCP socket.
    let bound = crate::net::unix_bind_listen(SOCK, 8).is_ok();

    // 2. An agent client connects.
    let client = crate::net::unix_connect(SOCK).ok();

    // 3. Daemon accepts the connection.
    let server_ep = crate::net::unix_accept(SOCK);

    // 4. Client sends a JSON-RPC tool call over the socket.
    let req = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fs_write","arguments":{"path":"daemon.txt","content":"via the socket"}}}"#;
    if let Some(c) = client {
        let _ = crate::net::unix_send(c, req);
    }

    // 5. Daemon reads the request, processes it via the cap-gated gateway on EuroFS,
    //    and sends the response back.
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

    // 6. Client reads the response and checks that it is a result.
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

    // 7. Prove that the tool call actually wrote a file to EuroFS via the socket route.
    let on_disk = fs.read_file("/agents/daemon/daemon.txt").map(|d| d == b"via the socket").unwrap_or(false);

    let ok = bound && served && client_ok && on_disk;
    crate::serial_println!(
        "[aa-mcp] EuroAgent MCP daemon: socket-bind={bound}, served-via-AF_UNIX={served}, client-got-result={client_ok}, file-on-EuroFS-via-socket={on_disk} → {}",
        if ok { "OK (MCP gateway as sovereign agent service over AF_UNIX) ✓" } else { "FAILED" }
    );
}
