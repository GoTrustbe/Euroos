//! EuroWL — the REAL Wayland wire-protocol server core (plan H5).
//!
//! Where EuroDisplay (H2) carried a Wayland-*shaped* custom frame protocol, this
//! module speaks the **real Wayland wire protocol**: object ids, opcodes, the
//! standardized 8-byte header `[obj_id:u32][ (size<<16)|opcode :u32 ]` +
//! word-aligned arguments, and the core interfaces `wl_display`/`wl_registry`/
//! `wl_compositor`/`wl_surface` plus `xdg_wm_base`/`xdg_surface`/`xdg_toplevel`.
//!
//! This way (eventually) an UNMODIFIED Wayland client (via libwayland over an
//! AF_UNIX socket, H1) can talk to EuroDisplay. This core processes the client
//! requests, sends the right events back (registry globals, xdg-configure), and
//! delivers per committed top-level surface a draw-ready [`Window`] to the
//! EuroDesktop compositor. `no_std`+alloc, parser + server fully host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

// Fixed object ids + globals.
const WL_DISPLAY: u32 = 1;

// Our advertised globals (registry `name` → interface).
const G_COMPOSITOR: u32 = 1;
const G_XDG_WM_BASE: u32 = 2;
const G_SHM: u32 = 3;

/// The kind of object behind a Wayland id (so we know which opcodes apply).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Obj {
    Display,
    Registry,
    Compositor,
    Shm,
    XdgWmBase,
    Surface,
    XdgSurface { surface: u32 },
    XdgToplevel { xdg_surface: u32 },
    Other,
}

/// A draw-ready window: the committed top-level surface + its title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub surface: u32,
    pub title: String,
}

/// The Wayland server core: an object table + surface/toplevel state. Feed client
/// bytes in via [`handle`](Self::handle) (returns the event bytes to send back), and
/// read the visible windows via [`windows`](Self::windows).
pub struct Server {
    objects: BTreeMap<u32, Obj>,
    /// xdg_surface id → surface id.
    xdg_to_surface: BTreeMap<u32, u32>,
    /// toplevel id → (xdg_surface id, title).
    toplevels: BTreeMap<u32, (u32, String)>,
    /// Committed top-level windows.
    windows: Vec<Window>,
    serial: u32,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        let mut objects = BTreeMap::new();
        objects.insert(WL_DISPLAY, Obj::Display); // id 1 = wl_display, always present
        Server {
            objects,
            xdg_to_surface: BTreeMap::new(),
            toplevels: BTreeMap::new(),
            windows: Vec::new(),
            serial: 0,
        }
    }

    pub fn windows(&self) -> &[Window] {
        &self.windows
    }

    fn next_serial(&mut self) -> u32 {
        self.serial += 1;
        self.serial
    }

    /// Process all complete Wayland messages in `input`; return the event bytes
    /// that must be sent to the client.
    pub fn handle(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut p = 0;
        while p + 8 <= input.len() {
            let obj = rd_u32(input, p);
            let w2 = rd_u32(input, p + 4);
            let size = (w2 >> 16) as usize;
            let opcode = (w2 & 0xffff) as u16;
            if size < 8 || p + size > input.len() {
                break; // incomplete message
            }
            let args = &input[p + 8..p + size];
            self.dispatch(obj, opcode, args, &mut out);
            p += (size + 3) & !3; // word-aligned
        }
        out
    }

    fn dispatch(&mut self, obj: u32, opcode: u16, args: &[u8], out: &mut Vec<u8>) {
        let kind = self.objects.get(&obj).cloned().unwrap_or(Obj::Other);
        match kind {
            Obj::Display => match opcode {
                0 => {
                    // wl_display.sync(callback) → callback.done(serial) + delete_id.
                    let cb = rd_u32(args, 0);
                    let s = self.next_serial();
                    write_msg(out, cb, 0, &[Arg::U(s)]); // wl_callback.done(serial)
                    write_msg(out, WL_DISPLAY, 1, &[Arg::U(cb)]); // wl_display.delete_id
                }
                1 => {
                    // wl_display.get_registry(registry) → advertise the globals.
                    let reg = rd_u32(args, 0);
                    self.objects.insert(reg, Obj::Registry);
                    self.advertise(reg, out);
                }
                _ => {}
            },
            Obj::Registry => {
                if opcode == 0 {
                    // wl_registry.bind(name, interface, version, new_id).
                    let name = rd_u32(args, 0);
                    let (iface, after) = rd_string(args, 4);
                    let _version = rd_u32(args, after);
                    let new_id = rd_u32(args, after + 4);
                    let kind = match (name, iface.as_str()) {
                        (G_COMPOSITOR, _) => Obj::Compositor,
                        (G_XDG_WM_BASE, _) => Obj::XdgWmBase,
                        (G_SHM, _) => Obj::Shm,
                        _ => Obj::Other,
                    };
                    self.objects.insert(new_id, kind);
                }
            }
            Obj::Compositor => {
                if opcode == 0 {
                    // wl_compositor.create_surface(new_id).
                    let sid = rd_u32(args, 0);
                    self.objects.insert(sid, Obj::Surface);
                }
            }
            Obj::XdgWmBase => {
                if opcode == 2 {
                    // xdg_wm_base.get_xdg_surface(new_id, surface).
                    let xid = rd_u32(args, 0);
                    let sid = rd_u32(args, 4);
                    self.objects.insert(xid, Obj::XdgSurface { surface: sid });
                    self.xdg_to_surface.insert(xid, sid);
                }
            }
            Obj::XdgSurface { .. } => match opcode {
                1 => {
                    // xdg_surface.get_toplevel(new_id) + send a configure.
                    let tid = rd_u32(args, 0);
                    self.objects.insert(tid, Obj::XdgToplevel { xdg_surface: obj });
                    self.toplevels.insert(tid, (obj, String::new()));
                    let s = self.next_serial();
                    write_msg(out, obj, 0, &[Arg::U(s)]); // xdg_surface.configure(serial)
                }
                4 => { /* ack_configure — accepted */ }
                _ => {}
            },
            Obj::XdgToplevel { .. } => {
                if opcode == 2 {
                    // xdg_toplevel.set_title(title).
                    let (title, _) = rd_string(args, 0);
                    if let Some(t) = self.toplevels.get_mut(&obj) {
                        t.1 = title;
                    }
                }
            }
            Obj::Surface
                if opcode == 6 => {
                    // wl_surface.commit() → if the surface has a top-level with a
                    // title, the window is now ready to draw.
                    self.commit_surface(obj);
                }
            _ => {}
        }
    }

    fn advertise(&self, registry: u32, out: &mut Vec<u8>) {
        // wl_registry.global(name, interface, version).
        write_msg(out, registry, 0, &[Arg::U(G_COMPOSITOR), Arg::S("wl_compositor"), Arg::U(4)]);
        write_msg(out, registry, 0, &[Arg::U(G_XDG_WM_BASE), Arg::S("xdg_wm_base"), Arg::U(1)]);
        write_msg(out, registry, 0, &[Arg::U(G_SHM), Arg::S("wl_shm"), Arg::U(1)]);
    }

    fn commit_surface(&mut self, sid: u32) {
        // Find a top-level whose xdg_surface refers to this surface.
        let found = self
            .toplevels
            .values()
            .find(|(xid, _)| self.xdg_to_surface.get(xid) == Some(&sid))
            .map(|(_, title)| title.clone());
        if let Some(title) = found {
            // Replace an existing window for the same surface (re-commit) or add one.
            if let Some(w) = self.windows.iter_mut().find(|w| w.surface == sid) {
                w.title = title;
            } else {
                self.windows.push(Window {
                    surface: sid,
                    title: if title.is_empty() { alloc::format!("Wayland surface {sid}") } else { title },
                });
            }
        }
    }
}

// ── Wire helpers ───────────────────────────────────────────────────────────
fn rd_u32(b: &[u8], o: usize) -> u32 {
    if o + 4 > b.len() {
        return 0;
    }
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Read a Wayland string at offset `o`: `[len:u32][len bytes incl null][pad→4]`.
/// Returns (string-without-null, offset after the padded string).
fn rd_string(b: &[u8], o: usize) -> (String, usize) {
    let len = rd_u32(b, o) as usize;
    let start = o + 4;
    if len == 0 || start + len > b.len() {
        return (String::new(), start + ((len + 3) & !3));
    }
    let bytes = &b[start..start + len - 1]; // last byte = null
    let s = String::from_utf8_lossy(bytes).into_owned();
    (s, start + ((len + 3) & !3))
}

/// A Wayland argument (for building requests/events).
pub enum Arg<'a> {
    U(u32),
    S(&'a str),
}

/// Encode one Wayland message (header + word-aligned args) to bytes — handy
/// for building client requests (e.g. an in-kernel test client).
pub fn encode(obj: u32, opcode: u16, args: &[Arg]) -> Vec<u8> {
    let mut out = Vec::new();
    write_msg(&mut out, obj, opcode, args);
    out
}

/// Write one Wayland message (header + word-aligned args) to `out`.
fn write_msg(out: &mut Vec<u8>, obj: u32, opcode: u16, args: &[Arg]) {
    let start = out.len();
    out.extend_from_slice(&obj.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]); // size+opcode (filled in later)
    for a in args {
        match a {
            Arg::U(v) => out.extend_from_slice(&v.to_le_bytes()),
            Arg::S(s) => {
                let len = s.len() as u32 + 1; // incl null
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(s.as_bytes());
                out.push(0);
                while !(out.len() - start).is_multiple_of(4) {
                    out.push(0);
                }
            }
        }
    }
    let size = (out.len() - start) as u32;
    let w2 = (size << 16) | opcode as u32;
    out[start + 4..start + 8].copy_from_slice(&w2.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Client-side helper to build real Wayland requests ──
    fn req(obj: u32, opcode: u16, args: &[Arg]) -> Vec<u8> {
        let mut v = Vec::new();
        write_msg(&mut v, obj, opcode, args);
        v
    }

    #[test]
    fn wire_roundtrip_header() {
        let m = req(1, 1, &[Arg::U(2)]);
        // header: obj=1, size=12, opcode=1
        assert_eq!(rd_u32(&m, 0), 1);
        let w2 = rd_u32(&m, 4);
        assert_eq!(w2 & 0xffff, 1); // opcode
        assert_eq!(w2 >> 16, 12); // size (8 header + 4 arg)
        assert_eq!(rd_u32(&m, 8), 2);
    }

    #[test]
    fn string_arg_padding() {
        // "wl_shm" = 6 chars + null = 7 → padded to 8 bytes after the len-u32.
        let m = req(1, 0, &[Arg::S("wl_shm")]);
        // size = 8 (header) + 4 (len) + 8 (padded string) = 20
        assert_eq!(rd_u32(&m, 4) >> 16, 20);
        let (s, _) = rd_string(&m, 8);
        assert_eq!(s, "wl_shm");
    }

    #[test]
    fn get_registry_advertises_globals() {
        let mut s = Server::new();
        // wl_display.get_registry(new_id=2)
        let out = s.handle(&req(WL_DISPLAY, 1, &[Arg::U(2)]));
        // Expect 3 global events on registry object 2.
        let mut p = 0;
        let mut globals = Vec::new();
        while p + 8 <= out.len() {
            let obj = rd_u32(&out, p);
            let w2 = rd_u32(&out, p + 4);
            let size = (w2 >> 16) as usize;
            assert_eq!(obj, 2); // registry
            assert_eq!(w2 & 0xffff, 0); // global event
            let (iface, _) = rd_string(&out, p + 8 + 4);
            globals.push(iface);
            p += (size + 3) & !3;
        }
        assert!(globals.iter().any(|g| g == "wl_compositor"));
        assert!(globals.iter().any(|g| g == "xdg_wm_base"));
    }

    /// Full handshake that ends in a titled window.
    fn handshake(title: &str) -> Server {
        let mut s = Server::new();
        let mut buf = Vec::new();
        buf.extend(req(WL_DISPLAY, 1, &[Arg::U(2)])); // get_registry → reg=2
        buf.extend(req(2, 0, &[Arg::U(G_COMPOSITOR), Arg::S("wl_compositor"), Arg::U(4), Arg::U(3)])); // bind compositor→3
        buf.extend(req(2, 0, &[Arg::U(G_XDG_WM_BASE), Arg::S("xdg_wm_base"), Arg::U(1), Arg::U(4)])); // bind xdg_wm_base→4
        buf.extend(req(3, 0, &[Arg::U(5)])); // compositor.create_surface → surface=5
        buf.extend(req(4, 2, &[Arg::U(6), Arg::U(5)])); // xdg_wm_base.get_xdg_surface(xdg=6, surface=5)
        buf.extend(req(6, 1, &[Arg::U(7)])); // xdg_surface.get_toplevel → toplevel=7
        buf.extend(req(7, 2, &[Arg::S(title)])); // xdg_toplevel.set_title
        buf.extend(req(5, 6, &[])); // wl_surface.commit
        let _ = s.handle(&buf);
        s
    }

    #[test]
    fn full_handshake_creates_titled_window() {
        let s = handshake("Hello Wayland");
        assert_eq!(s.windows().len(), 1);
        assert_eq!(s.windows()[0].surface, 5);
        assert_eq!(s.windows()[0].title, "Hello Wayland");
    }

    #[test]
    fn no_window_before_commit() {
        let mut s = Server::new();
        let mut buf = Vec::new();
        buf.extend(req(WL_DISPLAY, 1, &[Arg::U(2)]));
        buf.extend(req(2, 0, &[Arg::U(G_COMPOSITOR), Arg::S("wl_compositor"), Arg::U(4), Arg::U(3)]));
        buf.extend(req(3, 0, &[Arg::U(5)])); // create_surface, but NO commit
        s.handle(&buf);
        assert_eq!(s.windows().len(), 0);
    }

    #[test]
    fn surface_without_toplevel_is_not_a_window() {
        let mut s = Server::new();
        let mut buf = Vec::new();
        buf.extend(req(WL_DISPLAY, 1, &[Arg::U(2)]));
        buf.extend(req(2, 0, &[Arg::U(G_COMPOSITOR), Arg::S("wl_compositor"), Arg::U(4), Arg::U(3)]));
        buf.extend(req(3, 0, &[Arg::U(5)])); // surface 5
        buf.extend(req(5, 6, &[])); // commit without xdg_toplevel
        s.handle(&buf);
        assert_eq!(s.windows().len(), 0); // no top-level → no window
    }

    #[test]
    fn get_toplevel_sends_configure() {
        let mut s = Server::new();
        let mut buf = Vec::new();
        buf.extend(req(WL_DISPLAY, 1, &[Arg::U(2)]));
        buf.extend(req(2, 0, &[Arg::U(G_XDG_WM_BASE), Arg::S("xdg_wm_base"), Arg::U(1), Arg::U(4)]));
        buf.extend(req(2, 0, &[Arg::U(G_COMPOSITOR), Arg::S("wl_compositor"), Arg::U(4), Arg::U(3)]));
        buf.extend(req(3, 0, &[Arg::U(5)]));
        buf.extend(req(4, 2, &[Arg::U(6), Arg::U(5)]));
        let out = s.handle(&buf);
        let pre = out.len();
        let out2 = s.handle(&req(6, 1, &[Arg::U(7)])); // get_toplevel
        let _ = pre;
        // Expect an xdg_surface.configure(serial) on object 6, opcode 0.
        assert!(out2.len() >= 12);
        assert_eq!(rd_u32(&out2, 0), 6);
        assert_eq!(rd_u32(&out2, 4) & 0xffff, 0);
    }
}
