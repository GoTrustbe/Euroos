//! EuroDisplay server (H2) — the live link between the display protocol and the
//! real EuroDesktop compositor.
//!
//! An app process connects over a local **AF_UNIX socket** (H1), speaks the
//! **eurodisplay Request/Event** protocol (create/attach/commit surfaces + a
//! title & content lines), and this server translates every visible surface into a
//! draw-ready [`WindowView`] that the compositor renders as a real window. No
//! mockup: the window exists because another piece of code asked for it over a socket.
//!
//! The protocol + translation logic lives host-tested in `eurodisplay::server`; this
//! module is thin kernel glue: socket-accept + byte-read + frame forwarding.

use crate::net;
use alloc::string::String;
use alloc::vec::Vec;
use eurodisplay::server::{encode_frame, parse_frames, ServerMsg, ServerView, WindowView};
use eurodisplay::Request;

/// The default socket path the display server listens on.
pub const SOCK_PATH: &str = "/run/eurodisplay.sock";

/// The server state: the surface/window view + per-client an AF_UNIX endpoint with
/// a remainder buffer (for incomplete frames on the byte stream).
pub struct DispServer {
    sv: ServerView,
    path: &'static str,
    clients: Vec<(net::UnixEndpoint, Vec<u8>)>,
}

impl DispServer {
    pub fn new(path: &'static str) -> Self {
        DispServer {
            sv: ServerView::new(),
            path,
            clients: Vec::new(),
        }
    }

    /// Bind+listen on the socket path. `true` on success.
    pub fn bind(&self) -> bool {
        net::unix_bind_listen(self.path, 8).is_ok()
    }

    /// Accept new clients, read all their available bytes, parse complete
    /// frames and feed them into the view. Returns `true` if anything changed (new/
    /// modified/removed window) — then the compositor must redraw.
    pub fn pump(&mut self) -> bool {
        while let Some(ep) = net::unix_accept(self.path) {
            self.clients.push((ep, Vec::new()));
        }
        let mut changed = false;
        for (ep, buf) in &mut self.clients {
            loop {
                let chunk = net::unix_recv(*ep, 4096).unwrap_or_default();
                if chunk.is_empty() {
                    break;
                }
                buf.extend_from_slice(&chunk);
            }
            let (msgs, consumed) = parse_frames(buf);
            if consumed > 0 {
                buf.drain(..consumed);
            }
            if !msgs.is_empty() && self.sv.ingest(&msgs) {
                changed = true;
            }
        }
        changed
    }

    /// The visible app windows in z-order.
    pub fn windows(&self) -> Vec<WindowView> {
        self.sv.windows()
    }

    /// Number of connected clients.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }
}

/// An in-kernel demo app that connects over AF_UNIX and opens one window — proves
/// the full chain app → socket → server → compositor without a userspace
/// program. Returns the client endpoint (keep it open).
pub fn demo_app(path: &str) -> Option<net::UnixEndpoint> {
    let c = net::unix_connect(path).ok()?;
    let id = 100u32;
    let mut send = |m: ServerMsg| {
        let _ = net::unix_send(c, &encode_frame(&m));
    };
    send(ServerMsg::Req(Request::CreateSurface { id }));
    send(ServerMsg::Title(id, String::from("EuroApp  -  via AF_UNIX")));
    send(ServerMsg::Line(id, String::from("This window is NOT a mockup.")));
    send(ServerMsg::Line(id, String::from("An app process spoke the")));
    send(ServerMsg::Line(id, String::from("eurodisplay protocol (Request)")));
    send(ServerMsg::Line(id, String::from("over a local Unix socket (H1);")));
    send(ServerMsg::Line(id, String::from("the compositor drew it (H2).")));
    send(ServerMsg::Req(Request::Attach { id, width: 560, height: 360 }));
    send(ServerMsg::Req(Request::Commit { id }));
    Some(c)
}
