//! EuroDisplay-server (H2) — de live koppeling tussen het display-protocol en de
//! echte EuroDesktop-compositor.
//!
//! Een app-proces verbindt over een lokale **AF_UNIX-socket** (H1), spreekt het
//! **eurodisplay Request/Event**-protocol (surfaces aanmaken/attach/commit + een
//! titel & inhoudsregels), en deze server vertaalt elke zichtbare surface naar een
//! teken-klare [`WindowView`] die de compositor als een écht venster rendert. Geen
//! mockup: het venster bestaat omdat een ander stuk code er via een socket om vroeg.
//!
//! De protocol- + vertaal-logica zit host-getest in `eurodisplay::server`; deze
//! module is dunne kernel-lijm: socket-accept + bytes-lezen + frames doorgeven.

use crate::net;
use alloc::string::String;
use alloc::vec::Vec;
use eurodisplay::server::{encode_frame, parse_frames, ServerMsg, ServerView, WindowView};
use eurodisplay::Request;

/// Het standaard-socketpad waar de display-server op luistert.
pub const SOCK_PATH: &str = "/run/eurodisplay.sock";

/// De server-state: de surface/venster-view + per-client een AF_UNIX-endpoint met
/// een rest-buffer (voor onvolledige frames op de byte-stroom).
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

    /// Bind+luister op het socketpad. `true` bij succes.
    pub fn bind(&self) -> bool {
        net::unix_bind_listen(self.path, 8).is_ok()
    }

    /// Accepteer nieuwe clients, lees al hun beschikbare bytes, parse complete
    /// frames en voer ze in de view. Geeft `true` als er iets veranderde (nieuw/
    /// gewijzigd/verdwenen venster) — dan moet de compositor hertekenen.
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

    /// De zichtbare app-vensters in z-order.
    pub fn windows(&self) -> Vec<WindowView> {
        self.sv.windows()
    }

    /// Aantal verbonden clients.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }
}

/// Een in-kernel demo-app die over AF_UNIX verbindt en één venster opent — bewijst
/// de volledige keten app → socket → server → compositor zonder een userspace-
/// programma. Geeft het client-endpoint terug (open te houden).
pub fn demo_app(path: &str) -> Option<net::UnixEndpoint> {
    let c = net::unix_connect(path).ok()?;
    let id = 100u32;
    let mut send = |m: ServerMsg| {
        let _ = net::unix_send(c, &encode_frame(&m));
    };
    send(ServerMsg::Req(Request::CreateSurface { id }));
    send(ServerMsg::Title(id, String::from("EuroApp  -  via AF_UNIX")));
    send(ServerMsg::Line(id, String::from("Dit venster is GEEN mockup.")));
    send(ServerMsg::Line(id, String::from("Een app-proces sprak het")));
    send(ServerMsg::Line(id, String::from("eurodisplay-protocol (Request)")));
    send(ServerMsg::Line(id, String::from("over een lokale Unix-socket (H1);")));
    send(ServerMsg::Line(id, String::from("de compositor tekende het (H2).")));
    send(ServerMsg::Req(Request::Attach { id, width: 560, height: 360 }));
    send(ServerMsg::Req(Request::Commit { id }));
    Some(c)
}
