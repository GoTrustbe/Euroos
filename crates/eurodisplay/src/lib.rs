//! EuroDisplay — een Wayland-vormig display-protocol + de compositor-kant van het
//! surface-model (plan E2). Apps sturen render-commando's (`Request`), de compositor
//! beheert surfaces (z-order, damage) en stuurt `Event`s terug (configure/input/frame).
//!
//! Dit is de PROTOCOL- + STATE-kern, los van transport: de wire-encoding en het
//! surface-model zijn pure `no_std`-logica en host-getest. Het Unix-domain-socket-
//! transport + de live koppeling aan de EuroDesktop-compositor zijn de integratie
//! erbovenop (vereist Unix-sockets in EuroNet/EuroIPC).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

pub mod server;

/// App → compositor. Spiegelt `wl_surface`/`wl_buffer`-acties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// `wl_compositor.create_surface` — nieuwe (lege) surface met id.
    CreateSurface { id: u32 },
    /// `wl_surface.attach` — koppel een buffer (gedeeld geheugen) van w×h aan de surface.
    Attach { id: u32, width: u16, height: u16 },
    /// `wl_surface.commit` — maak de aangehechte buffer + positie zichtbaar.
    Commit { id: u32 },
    /// Verplaats de surface (compositor-beleid bepaalt of dit mag).
    Move { id: u32, x: i16, y: i16 },
    /// `wl_surface.destroy`.
    Destroy { id: u32 },
}

/// Compositor → app. Spiegelt `wl_surface`/`wl_seat`/`wl_output`-events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// `xdg_surface.configure` — voorgestelde grootte.
    Configure { id: u32, width: u16, height: u16 },
    /// `wl_keyboard.key` — toets naar de gefocuste surface.
    Key { id: u32, code: u16, pressed: bool },
    /// `wl_pointer.motion`.
    Pointer { id: u32, x: i16, y: i16 },
    /// `wl_surface.frame` done — klaar om de volgende frame te tekenen.
    FrameDone { id: u32 },
}

/// Eén surface in het compositor-model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    pub id: u32,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    /// Is er een gecommitte buffer (mag getekend worden)?
    pub mapped: bool,
}

/// De compositor-kant: een z-geordende lijst surfaces + damage-bijhouding.
#[derive(Debug, Default)]
pub struct Display {
    surfaces: Vec<Surface>, // achteraan = bovenop (hoogste z)
    damaged: bool,
}

impl Display {
    pub fn new() -> Self {
        Display { surfaces: Vec::new(), damaged: false }
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.surfaces.iter().position(|s| s.id == id)
    }

    /// Verwerk een app-request; geef optioneel een Event terug (bv. Configure na commit).
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
                    // Een commit met een geldige buffer maakt de surface zichtbaar +
                    // brengt hem naar voren (focus), en markeert damage.
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

    /// De zichtbare surfaces in z-order (onderaan → boven) voor het tekenen.
    pub fn scene(&self) -> Vec<Surface> {
        self.surfaces.iter().copied().filter(|s| s.mapped).collect()
    }

    /// De bovenste (gefocuste) surface — daar gaat keyboard-input heen.
    pub fn focused(&self) -> Option<u32> {
        self.surfaces.iter().rev().find(|s| s.mapped).map(|s| s.id)
    }

    /// Routeer een input-event naar de gefocuste surface (keyboard) of naar de
    /// surface onder de cursor (pointer, top-most hit).
    pub fn route_key(&self, code: u16, pressed: bool) -> Option<Event> {
        self.focused().map(|id| Event::Key { id, code, pressed })
    }
    pub fn route_pointer(&self, x: i16, y: i16) -> Option<Event> {
        // Top-most surface die (x,y) bevat.
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

// ── Wire-encoding (vast 12-byte bericht: opcode + 5×u16/i16-velden) ─────────
const REQ_CREATE: u8 = 1;
const REQ_ATTACH: u8 = 2;
const REQ_COMMIT: u8 = 3;
const REQ_MOVE: u8 = 4;
const REQ_DESTROY: u8 = 5;

/// Codeer een request tot 12 bytes (opcode, id, en tot 2 velden).
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

/// Decodeer een 12-byte bericht terug naar een request. `None` bij rommel.
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
        assert!(d.scene().is_empty()); // nog niet gemapt (geen buffer/commit)
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
        // De laatst-gecommitte (3) staat bovenop en heeft focus.
        assert_eq!(d.focused(), Some(3));
        // Een nieuwe commit van 1 brengt hem naar voren.
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
        // Keyboard → gefocuste surface.
        assert_eq!(d.route_key(30, true), Some(Event::Key { id: 1, code: 30, pressed: true }));
        // Pointer binnen de surface → surface-lokale coördinaten.
        assert_eq!(d.route_pointer(15, 20), Some(Event::Pointer { id: 1, x: 5, y: 10 }));
        // Pointer buiten elke surface → niets.
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
        // Rommel / te kort → None.
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
        assert!(d.take_damage()); // commit veroorzaakt damage
        assert!(!d.take_damage()); // daarna geveegd
    }
}
