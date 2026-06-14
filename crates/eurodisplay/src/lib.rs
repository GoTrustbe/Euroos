//! EuroDisplay — a Wayland-shaped display protocol + the compositor side of the
//! surface model (plan E2). Apps send render commands (`Request`), the compositor
//! manages surfaces (z-order, damage) and sends `Event`s back (configure/input/frame).
//!
//! This is the PROTOCOL + STATE core, decoupled from transport: the wire encoding and the
//! surface model are pure `no_std` logic and host-tested. The Unix-domain-socket
//! transport + the live binding to the EuroDesktop compositor are the integration
//! on top (requires Unix sockets in EuroNet/EuroIPC).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

pub mod server;

/// App → compositor. Mirrors `wl_surface`/`wl_buffer` actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// `wl_compositor.create_surface` — new (empty) surface with id.
    CreateSurface { id: u32 },
    /// `wl_surface.attach` — attach a buffer (shared memory) of w×h to the surface.
    Attach { id: u32, width: u16, height: u16 },
    /// `wl_surface.commit` — make the attached buffer + position visible.
    Commit { id: u32 },
    /// Move the surface (compositor policy decides whether this is allowed).
    Move { id: u32, x: i16, y: i16 },
    /// `wl_surface.destroy`.
    Destroy { id: u32 },
}

/// Compositor → app. Mirrors `wl_surface`/`wl_seat`/`wl_output` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// `xdg_surface.configure` — proposed size.
    Configure { id: u32, width: u16, height: u16 },
    /// `wl_keyboard.key` — key to the focused surface.
    Key { id: u32, code: u16, pressed: bool },
    /// `wl_pointer.motion`.
    Pointer { id: u32, x: i16, y: i16 },
    /// `wl_surface.frame` done — ready to draw the next frame.
    FrameDone { id: u32 },
}

/// One surface in the compositor model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    pub id: u32,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    /// Is there a committed buffer (allowed to be drawn)?
    pub mapped: bool,
}

/// The compositor side: a z-ordered list of surfaces + damage tracking.
#[derive(Debug, Default)]
pub struct Display {
    surfaces: Vec<Surface>, // last = on top (highest z)
    damaged: bool,
}

impl Display {
    pub fn new() -> Self {
        Display { surfaces: Vec::new(), damaged: false }
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.surfaces.iter().position(|s| s.id == id)
    }

    /// Process an app request; optionally return an Event (e.g. Configure after commit).
    pub fn handle(&mut self, req: Request) -> Option<Event> {
        match req {
            Request::CreateSurface { id } => {
                if self.index(id).is_none() {
                    self.surfaces.push(Surface { id, x: 0, y: 0, width: 0, height: 0, mapped: false });
                }
                None
            }
            Request::Attach { id, width, height } => {
                if let Some(i) = self.index(id) {
                    self.surfaces[i].width = width;
                    self.surfaces[i].height = height;
                    return Some(Event::Configure { id, width, height });
                }
                None
            }
            Request::Commit { id } => {
                if let Some(i) = self.index(id) {
                    // A commit with a valid buffer makes the surface visible +
                    // brings it to the front (focus), and marks damage.
                    if self.surfaces[i].width > 0 && self.surfaces[i].height > 0 {
                        let s = self.surfaces.remove(i);
                        self.surfaces.push(Surface { mapped: true, ..s });
                        self.damaged = true;
                        return Some(Event::FrameDone { id });
                    }
                }
                None
            }
            Request::Move { id, x, y } => {
                if let Some(i) = self.index(id) {
                    self.surfaces[i].x = x;
                    self.surfaces[i].y = y;
                    self.damaged = true;
                }
                None
            }
            Request::Destroy { id } => {
                if let Some(i) = self.index(id) {
                    self.surfaces.remove(i);
                    self.damaged = true;
                }
                None
            }
        }
    }

    /// The visible surfaces in z-order (bottom → top) for drawing.
    pub fn scene(&self) -> Vec<Surface> {
        self.surfaces.iter().copied().filter(|s| s.mapped).collect()
    }

    /// The top (focused) surface — that is where keyboard input goes.
    pub fn focused(&self) -> Option<u32> {
        self.surfaces.iter().rev().find(|s| s.mapped).map(|s| s.id)
    }

    /// Route an input event to the focused surface (keyboard) or to the
    /// surface under the cursor (pointer, top-most hit).
    pub fn route_key(&self, code: u16, pressed: bool) -> Option<Event> {
        self.focused().map(|id| Event::Key { id, code, pressed })
    }
    pub fn route_pointer(&self, x: i16, y: i16) -> Option<Event> {
        // Top-most surface that contains (x,y).
        self.surfaces.iter().rev().find(|s| {
            s.mapped
                && x >= s.x
                && y >= s.y
                && (x as i32) < s.x as i32 + s.width as i32
                && (y as i32) < s.y as i32 + s.height as i32
        }).map(|s| Event::Pointer { id: s.id, x: x - s.x, y: y - s.y })
    }

    pub fn take_damage(&mut self) -> bool {
        core::mem::take(&mut self.damaged)
    }
}

// ── Wire encoding (fixed 12-byte message: opcode + 5×u16/i16 fields) ─────────
const REQ_CREATE: u8 = 1;
const REQ_ATTACH: u8 = 2;
const REQ_COMMIT: u8 = 3;
const REQ_MOVE: u8 = 4;
const REQ_DESTROY: u8 = 5;

/// Encode a request into 12 bytes (opcode, id, and up to 2 fields).
pub fn encode(req: Request) -> [u8; 12] {
    let mut b = [0u8; 12];
    let (op, id, f0, f1) = match req {
        Request::CreateSurface { id } => (REQ_CREATE, id, 0u16, 0u16),
        Request::Attach { id, width, height } => (REQ_ATTACH, id, width, height),
        Request::Commit { id } => (REQ_COMMIT, id, 0, 0),
        Request::Move { id, x, y } => (REQ_MOVE, id, x as u16, y as u16),
        Request::Destroy { id } => (REQ_DESTROY, id, 0, 0),
    };
    b[0] = op;
    b[4..8].copy_from_slice(&id.to_le_bytes());
    b[8..10].copy_from_slice(&f0.to_le_bytes());
    b[10..12].copy_from_slice(&f1.to_le_bytes());
    b
}

/// Decode a 12-byte message back into a request. `None` on garbage.
pub fn decode(b: &[u8]) -> Option<Request> {
    if b.len() < 12 {
        return None;
    }
    let id = u32::from_le_bytes(b[4..8].try_into().ok()?);
    let f0 = u16::from_le_bytes(b[8..10].try_into().ok()?);
    let f1 = u16::from_le_bytes(b[10..12].try_into().ok()?);
    Some(match b[0] {
        REQ_CREATE => Request::CreateSurface { id },
        REQ_ATTACH => Request::Attach { id, width: f0, height: f1 },
        REQ_COMMIT => Request::Commit { id },
        REQ_MOVE => Request::Move { id, x: f0 as i16, y: f1 as i16 },
        REQ_DESTROY => Request::Destroy { id },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_lifecycle() {
        let mut d = Display::new();
        d.handle(Request::CreateSurface { id: 1 });
        assert!(d.scene().is_empty()); // not yet mapped (no buffer/commit)
        assert_eq!(d.handle(Request::Attach { id: 1, width: 100, height: 50 }), Some(Event::Configure { id: 1, width: 100, height: 50 }));
        assert_eq!(d.handle(Request::Commit { id: 1 }), Some(Event::FrameDone { id: 1 }));
        assert_eq!(d.scene().len(), 1);
        assert_eq!(d.focused(), Some(1));
        d.handle(Request::Destroy { id: 1 });
        assert!(d.scene().is_empty());
    }

    #[test]
    fn commit_raises_to_front_and_focuses() {
        let mut d = Display::new();
        for id in [1u32, 2, 3] {
            d.handle(Request::CreateSurface { id });
            d.handle(Request::Attach { id, width: 80, height: 80 });
            d.handle(Request::Commit { id });
        }
        // The last-committed (3) is on top and has focus.
        assert_eq!(d.focused(), Some(3));
        // A new commit of 1 brings it to the front.
        d.handle(Request::Commit { id: 1 });
        assert_eq!(d.focused(), Some(1));
        assert_eq!(d.scene().len(), 3);
    }

    #[test]
    fn input_routing() {
        let mut d = Display::new();
        d.handle(Request::CreateSurface { id: 1 });
        d.handle(Request::Attach { id: 1, width: 100, height: 100 });
        d.handle(Request::Move { id: 1, x: 10, y: 10 });
        d.handle(Request::Commit { id: 1 });
        // Keyboard → focused surface.
        assert_eq!(d.route_key(30, true), Some(Event::Key { id: 1, code: 30, pressed: true }));
        // Pointer inside the surface → surface-local coordinates.
        assert_eq!(d.route_pointer(15, 20), Some(Event::Pointer { id: 1, x: 5, y: 10 }));
        // Pointer outside every surface → nothing.
        assert_eq!(d.route_pointer(5, 5), None);
    }

    #[test]
    fn wire_roundtrip() {
        for r in [
            Request::CreateSurface { id: 7 },
            Request::Attach { id: 7, width: 640, height: 480 },
            Request::Commit { id: 7 },
            Request::Move { id: 7, x: -5, y: 12 },
            Request::Destroy { id: 7 },
        ] {
            assert_eq!(decode(&encode(r)), Some(r));
        }
        // Garbage / too short → None.
        assert_eq!(decode(&[0xFF; 12]), None);
        assert_eq!(decode(&[1, 2, 3]), None);
    }

    #[test]
    fn damage_tracking() {
        let mut d = Display::new();
        assert!(!d.take_damage());
        d.handle(Request::CreateSurface { id: 1 });
        d.handle(Request::Attach { id: 1, width: 10, height: 10 });
        d.handle(Request::Commit { id: 1 });
        assert!(d.take_damage()); // commit causes damage
        assert!(!d.take_damage()); // cleared afterwards
    }
}
