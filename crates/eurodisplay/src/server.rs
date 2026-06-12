//! Display-server laag (H2): draagt de [`Request`]/[`Event`]-protocol-objecten
//! over een **byte-stroom** (in de kernel: een AF_UNIX-socket, H1) en vertaalt de
//! resulterende surfaces naar concrete **venster-views** die de EuroDesktop-
//! compositor kan tekenen.
//!
//! De surface-model van [`crate::Display`] is puur geometrie (id/x/y/w/h/mapped).
//! De compositor tekent vensters met een **titel** + **tekstregels** (monospace),
//! geen pixel-buffers. Deze laag voegt die compositor-only metadata toe via twee
//! extra berichten ([`ServerMsg::Title`]/[`ServerMsg::Line`]) bovenop het normale
//! surface-lifecycle-verkeer, en levert per zichtbare surface een [`WindowView`].
//!
//! Frame-formaat op de stroom (uniform, lengte-geprefixt — zodat vaste én
//! variabele berichten op één stroom ondubbelzinnig te demuxen zijn):
//! ```text
//! [op:u8][id:u32 LE][a:i16 LE][b:i16 LE][len:u16 LE][payload: len bytes]
//! ```
//! Header = 11 bytes. `a`/`b` dragen geometrie (w/h of x/y); `payload` draagt
//! UTF-8 tekst voor Title/Line. `no_std`+alloc, volledig host-testbaar.

use crate::{Display, Request};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

const OP_CREATE: u8 = 1;
const OP_ATTACH: u8 = 2;
const OP_COMMIT: u8 = 3;
const OP_MOVE: u8 = 4;
const OP_DESTROY: u8 = 5;
const OP_TITLE: u8 = 0x10;
const OP_LINE: u8 = 0x11;
const OP_CLEAR: u8 = 0x12;

const HDR: usize = 11;

/// Eén bericht op de display-server-stroom: óf een surface-[`Request`], óf
/// compositor-metadata (venstertitel / inhoudsregel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerMsg {
    Req(Request),
    /// Zet de venstertitel voor surface `id`.
    Title(u32, String),
    /// Voeg een inhoudsregel toe aan surface `id`.
    Line(u32, String),
    /// Wis alle inhoudsregels van surface `id`.
    ClearLines(u32),
}

/// Codeer één bericht tot een lengte-geprefixt frame.
pub fn encode_frame(msg: &ServerMsg) -> Vec<u8> {
    let mut b = Vec::with_capacity(HDR);
    let (op, id, a, bb, payload): (u8, u32, i16, i16, &[u8]) = match msg {
        ServerMsg::Req(Request::CreateSurface { id }) => (OP_CREATE, *id, 0, 0, &[]),
        ServerMsg::Req(Request::Attach { id, width, height }) => {
            (OP_ATTACH, *id, *width as i16, *height as i16, &[])
        }
        ServerMsg::Req(Request::Commit { id }) => (OP_COMMIT, *id, 0, 0, &[]),
        ServerMsg::Req(Request::Move { id, x, y }) => (OP_MOVE, *id, *x, *y, &[]),
        ServerMsg::Req(Request::Destroy { id }) => (OP_DESTROY, *id, 0, 0, &[]),
        ServerMsg::Title(id, t) => (OP_TITLE, *id, 0, 0, t.as_bytes()),
        ServerMsg::Line(id, l) => (OP_LINE, *id, 0, 0, l.as_bytes()),
        ServerMsg::ClearLines(id) => (OP_CLEAR, *id, 0, 0, &[]),
    };
    b.push(op);
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&a.to_le_bytes());
    b.extend_from_slice(&bb.to_le_bytes());
    b.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    b.extend_from_slice(payload);
    b
}

/// Parse zoveel volledige frames als in `buf` staan. Geeft de berichten + het
/// aantal verbruikte bytes (een onvolledig staart-frame blijft staan voor de
/// volgende aanroep — veilig voor een stroom-protocol).
pub fn parse_frames(buf: &[u8]) -> (Vec<ServerMsg>, usize) {
    let mut msgs = Vec::new();
    let mut off = 0;
    while off + HDR <= buf.len() {
        let op = buf[off];
        let id = u32::from_le_bytes([buf[off + 1], buf[off + 2], buf[off + 3], buf[off + 4]]);
        let a = i16::from_le_bytes([buf[off + 5], buf[off + 6]]);
        let bb = i16::from_le_bytes([buf[off + 7], buf[off + 8]]);
        let len = u16::from_le_bytes([buf[off + 9], buf[off + 10]]) as usize;
        if off + HDR + len > buf.len() {
            break; // onvolledig frame — wacht op meer bytes
        }
        let payload = &buf[off + HDR..off + HDR + len];
        let msg = match op {
            OP_CREATE => ServerMsg::Req(Request::CreateSurface { id }),
            OP_ATTACH => ServerMsg::Req(Request::Attach {
                id,
                width: a as u16,
                height: bb as u16,
            }),
            OP_COMMIT => ServerMsg::Req(Request::Commit { id }),
            OP_MOVE => ServerMsg::Req(Request::Move { id, x: a, y: bb }),
            OP_DESTROY => ServerMsg::Req(Request::Destroy { id }),
            OP_TITLE => ServerMsg::Title(id, String::from_utf8_lossy(payload).into_owned()),
            OP_LINE => ServerMsg::Line(id, String::from_utf8_lossy(payload).into_owned()),
            OP_CLEAR => ServerMsg::ClearLines(id),
            _ => {
                off += HDR + len; // onbekend opcode — sla over
                continue;
            }
        };
        msgs.push(msg);
        off += HDR + len;
    }
    (msgs, off)
}

/// Een teken-klare venster-view: surface-geometrie + compositor-metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowView {
    pub id: u32,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub title: String,
    pub content: Vec<String>,
}

/// De server-state: de surface-[`Display`] + per-surface compositor-metadata.
/// Voedt berichten in via [`ingest`](Self::ingest) en levert teken-klare
/// [`WindowView`]s via [`windows`](Self::windows).
#[derive(Default)]
pub struct ServerView {
    disp: Display,
    meta: BTreeMap<u32, (String, Vec<String>)>,
}

impl ServerView {
    pub fn new() -> Self {
        ServerView {
            disp: Display::new(),
            meta: BTreeMap::new(),
        }
    }

    /// Verwerk een batch berichten. Geeft `true` als er iets veranderde dat een
    /// hertekening vereist (nieuwe/gewijzigde/verdwenen vensters).
    pub fn ingest(&mut self, msgs: &[ServerMsg]) -> bool {
        let mut changed = false;
        for m in msgs {
            match m {
                ServerMsg::Req(r) => {
                    if let Request::CreateSurface { id } = r {
                        self.meta.entry(*id).or_default();
                    }
                    if let Request::Destroy { id } = r {
                        self.meta.remove(id);
                    }
                    if self.disp.handle(r.clone()).is_some()
                        || matches!(r, Request::Move { .. } | Request::Destroy { .. })
                    {
                        changed = true;
                    }
                }
                ServerMsg::Title(id, t) => {
                    self.meta.entry(*id).or_default().0 = t.clone();
                    changed = true;
                }
                ServerMsg::Line(id, l) => {
                    self.meta.entry(*id).or_default().1.push(l.clone());
                    changed = true;
                }
                ServerMsg::ClearLines(id) => {
                    self.meta.entry(*id).or_default().1.clear();
                    changed = true;
                }
            }
        }
        // De damage-vlag van de Display dekt Commit-hertekeningen.
        changed | self.disp.take_damage()
    }

    /// De zichtbare vensters in z-order (onderaan → boven), elk met titel +
    /// inhoud. Surfaces zonder expliciete titel krijgen `App <id>`.
    pub fn windows(&self) -> Vec<WindowView> {
        self.disp
            .scene()
            .into_iter()
            .map(|s| {
                let (title, content) = self
                    .meta
                    .get(&s.id)
                    .cloned()
                    .unwrap_or_else(|| (alloc::format!("App {}", s.id), Vec::new()));
                let title = if title.is_empty() {
                    alloc::format!("App {}", s.id)
                } else {
                    title
                };
                WindowView {
                    id: s.id,
                    x: s.x,
                    y: s.y,
                    width: s.width,
                    height: s.height,
                    title,
                    content,
                }
            })
            .collect()
    }

    /// Aantal zichtbare vensters.
    pub fn window_count(&self) -> usize {
        self.disp.scene().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_window_bytes(id: u32, w: u16, h: u16, title: &str, lines: &[&str]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(encode_frame(&ServerMsg::Req(Request::CreateSurface { id })));
        b.extend(encode_frame(&ServerMsg::Title(id, String::from(title))));
        for l in lines {
            b.extend(encode_frame(&ServerMsg::Line(id, String::from(*l))));
        }
        b.extend(encode_frame(&ServerMsg::Req(Request::Attach {
            id,
            width: w,
            height: h,
        })));
        b.extend(encode_frame(&ServerMsg::Req(Request::Commit { id })));
        b
    }

    #[test]
    fn frame_roundtrip_all_kinds() {
        let msgs = [
            ServerMsg::Req(Request::CreateSurface { id: 7 }),
            ServerMsg::Req(Request::Attach { id: 7, width: 320, height: 200 }),
            ServerMsg::Req(Request::Move { id: 7, x: -5, y: 40 }),
            ServerMsg::Req(Request::Commit { id: 7 }),
            ServerMsg::Req(Request::Destroy { id: 7 }),
            ServerMsg::Title(7, String::from("Hallo")),
            ServerMsg::Line(7, String::from("regel één")),
            ServerMsg::ClearLines(7),
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            buf.extend(encode_frame(m));
        }
        let (got, consumed) = parse_frames(&buf);
        assert_eq!(consumed, buf.len());
        assert_eq!(got, msgs);
    }

    #[test]
    fn partial_trailing_frame_is_left_for_later() {
        let mut buf = encode_frame(&ServerMsg::Req(Request::CreateSurface { id: 1 }));
        let full = buf.len();
        buf.extend_from_slice(&[OP_TITLE, 1, 0, 0, 0]); // truncated header
        let (got, consumed) = parse_frames(&buf);
        assert_eq!(consumed, full);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn full_open_produces_one_window() {
        let bytes = open_window_bytes(1, 640, 480, "EuroApp", &["Hallo van", "een AF_UNIX-app"]);
        let (msgs, _) = parse_frames(&bytes);
        let mut sv = ServerView::new();
        assert!(sv.ingest(&msgs));
        let wins = sv.windows();
        assert_eq!(wins.len(), 1);
        assert_eq!(wins[0].id, 1);
        assert_eq!(wins[0].width, 640);
        assert_eq!(wins[0].height, 480);
        assert_eq!(wins[0].title, "EuroApp");
        assert_eq!(wins[0].content, alloc::vec!["Hallo van", "een AF_UNIX-app"]);
    }

    #[test]
    fn unmapped_surface_is_not_a_window() {
        // Create + Attach maar GEEN Commit → niet zichtbaar.
        let mut b = encode_frame(&ServerMsg::Req(Request::CreateSurface { id: 1 }));
        b.extend(encode_frame(&ServerMsg::Req(Request::Attach { id: 1, width: 10, height: 10 })));
        let (msgs, _) = parse_frames(&b);
        let mut sv = ServerView::new();
        sv.ingest(&msgs);
        assert_eq!(sv.window_count(), 0);
    }

    #[test]
    fn destroy_removes_window_and_meta() {
        let mut bytes = open_window_bytes(1, 100, 100, "X", &["a"]);
        bytes.extend(encode_frame(&ServerMsg::Req(Request::Destroy { id: 1 })));
        let (msgs, _) = parse_frames(&bytes);
        let mut sv = ServerView::new();
        sv.ingest(&msgs);
        assert_eq!(sv.window_count(), 0);
        assert!(sv.windows().is_empty());
    }

    #[test]
    fn untitled_surface_gets_default_title() {
        let mut b = encode_frame(&ServerMsg::Req(Request::CreateSurface { id: 3 }));
        b.extend(encode_frame(&ServerMsg::Req(Request::Attach { id: 3, width: 50, height: 50 })));
        b.extend(encode_frame(&ServerMsg::Req(Request::Commit { id: 3 })));
        let (msgs, _) = parse_frames(&b);
        let mut sv = ServerView::new();
        sv.ingest(&msgs);
        assert_eq!(sv.windows()[0].title, "App 3");
    }

    #[test]
    fn zorder_two_windows_topmost_last() {
        let mut b = open_window_bytes(1, 100, 100, "first", &[]);
        b.extend(open_window_bytes(2, 100, 100, "second", &[]));
        let (msgs, _) = parse_frames(&b);
        let mut sv = ServerView::new();
        sv.ingest(&msgs);
        let wins = sv.windows();
        assert_eq!(wins.len(), 2);
        // laatst-gecommit = bovenaan = laatste in z-order
        assert_eq!(wins[0].title, "first");
        assert_eq!(wins[1].title, "second");
    }
}
