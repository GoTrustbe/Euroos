//! H5: EuroWL — the REAL Wayland wire-protocol server in the kernel, ahead of EuroDisplay.
//!
//! An in-kernel Wayland test client runs through the real handshake (`get_registry`
//! → `bind` → `create_surface` → `xdg_wm_base.get_xdg_surface` → `get_toplevel` →
//! `set_title` → `commit`); the [`eurowl::Server`] processes the real protocol bytes
//! and produces a titled window, which the EuroDesktop compositor draws. This is the
//! foundation on which (with libwayland over an AF_UNIX socket, H1) an UNMODIFIED
//! Wayland client can run against EuroOS.

use alloc::string::String;
use eurowl::{encode, Arg, Server};

const G_COMPOSITOR: u32 = 1;
const G_XDG_WM_BASE: u32 = 2;

/// Run a complete, REAL Wayland handshake through the server and return
/// (surface_id, title) of the resulting top-level window.
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
        "[h5] real Wayland protocol: handshake processed, {} bytes of events back (registry globals + xdg-configure), {} window(s)",
        events.len(),
        srv.windows().len()
    );
    srv.windows().first().map(|w| (w.surface, w.title.clone()))
}

/// H5 self-test: verify that the real Wayland handshake produces a titled window.
pub fn selftest() {
    match run_handshake("EuroOS — real Wayland protocol") {
        Some((sid, title)) => crate::serial_println!(
            "[h5] Wayland server: surface {} committed → window \"{}\" ✓",
            sid,
            title
        ),
        None => crate::serial_println!("[h5] ERROR: no window from the Wayland handshake"),
    }
}
