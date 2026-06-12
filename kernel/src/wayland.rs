//! H5: EuroWL — de ECHTE Wayland-wire-protocol-server in de kernel, vóór EuroDisplay.
//!
//! Een in-kernel Wayland-test-client doorloopt de echte handshake (`get_registry`
//! → `bind` → `create_surface` → `xdg_wm_base.get_xdg_surface` → `get_toplevel` →
//! `set_title` → `commit`); de [`eurowl::Server`] verwerkt de echte protocol-bytes
//! en levert een getiteld venster, dat de EuroDesktop-compositor tekent. Dit is het
//! fundament waarop (met libwayland over een AF_UNIX-socket, H1) een ONGEWIJZIGDE
//! Wayland-client tegen EuroOS kan draaien.

use alloc::string::String;
use eurowl::{encode, Arg, Server};

const G_COMPOSITOR: u32 = 1;
const G_XDG_WM_BASE: u32 = 2;

/// Draai een complete, ECHTE Wayland-handshake door de server en geef
/// (surface_id, titel) van het resulterende top-level-venster.
pub fn run_handshake(title: &str) -> Option<(u32, String)> {
    let mut srv = Server::new();
    let mut buf = alloc::vec::Vec::new();
    buf.extend(encode(1, 1, &[Arg::U(2)])); // wl_display.get_registry(new_id=2)
    buf.extend(encode(2, 0, &[Arg::U(G_COMPOSITOR), Arg::S("wl_compositor"), Arg::U(4), Arg::U(3)])); // bind→3
    buf.extend(encode(2, 0, &[Arg::U(G_XDG_WM_BASE), Arg::S("xdg_wm_base"), Arg::U(1), Arg::U(4)])); // bind→4
    buf.extend(encode(3, 0, &[Arg::U(5)])); // wl_compositor.create_surface(new_id=5)
    buf.extend(encode(4, 2, &[Arg::U(6), Arg::U(5)])); // xdg_wm_base.get_xdg_surface(6, surface=5)
    buf.extend(encode(6, 1, &[Arg::U(7)])); // xdg_surface.get_toplevel(new_id=7)
    buf.extend(encode(7, 2, &[Arg::S(title)])); // xdg_toplevel.set_title
    buf.extend(encode(5, 6, &[])); // wl_surface.commit
    let events = srv.handle(&buf);
    crate::serial_println!(
        "[h5] echt Wayland-protocol: handshake verwerkt, {} bytes events terug (registry-globals + xdg-configure), {} venster(s)",
        events.len(),
        srv.windows().len()
    );
    srv.windows().first().map(|w| (w.surface, w.title.clone()))
}

/// H5-zelftest: verifieer dat de echte Wayland-handshake een getiteld venster maakt.
pub fn selftest() {
    match run_handshake("EuroOS — echt Wayland-protocol") {
        Some((sid, title)) => crate::serial_println!(
            "[h5] Wayland-server: surface {} gecommit → venster \"{}\" ✓",
            sid,
            title
        ),
        None => crate::serial_println!("[h5] FOUT: geen venster uit de Wayland-handshake"),
    }
}
