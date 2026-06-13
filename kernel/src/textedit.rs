//! **EuroText** (Sprint 4) — een ECHTE platte-tekst-editor: typen, backspace,
//! nieuwe regels, en OPSLAAN naar het echte EuroFS (en terug inlezen na heropenen).
//! Geen mock: de inhoud wordt als bestand weggeschreven en overleeft een herstart.
//!
//! Beperking (eerlijk): de PS/2-driver levert geen pijltjes/Ctrl, dus de cursor zit
//! aan het invoegpunt (typemachine-stijl: typen/​backspace/​enter); opslaan gebeurt
//! via de "Opslaan"-knop in de werkbalk (muis). De bewerk-+-opslag-+-herlees-cyclus
//! is host-onafhankelijk geverifieerd via de `[edit]`-zelftest.

use crate::graphics::{Color, FrameBuffer};
use crate::{serial_println, text};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use eurofs::FileSystem;
use spin::Mutex;

const TITLEBAR_H: usize = 44;
const TOOLBAR_H: usize = 40;
const LINE_H: usize = 20;
const PAD: usize = 14;
const DEFAULT_PATH: &str = "/home/euro/euro.txt";

struct Editor {
    lines: Vec<String>,
    path: String,
    dirty: bool,
    status: String,
}

static ED: Mutex<Editor> = Mutex::new(Editor {
    lines: Vec::new(),
    path: String::new(),
    dirty: false,
    status: String::new(),
});

/// Open `path` (of het standaardbestand) in de editor; bestaat het niet, dan starten
/// we met een welkomsttekst. Vult de regelbuffer uit de ECHTE bestandsinhoud.
pub fn open(fs: &mut dyn FileSystem, path: &str) {
    let p = if path.is_empty() { DEFAULT_PATH } else { path };
    let content = match fs.read_file(p) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::from("Welkom in EuroText.\nTyp hier; klik 'Opslaan' om naar EuroFS te schrijven.\n"),
    };
    let mut lines: Vec<String> = content.split('\n').map(String::from).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let mut ed = ED.lock();
    ed.lines = lines;
    ed.path = String::from(p);
    ed.dirty = false;
    ed.status = alloc::format!("geopend: {p}");
}

/// Verwerk één toets aan het invoegpunt (einde van de buffer).
pub fn input(ch: char) {
    let mut ed = ED.lock();
    if ed.lines.is_empty() {
        ed.lines.push(String::new());
    }
    match ch {
        '\r' | '\n' => ed.lines.push(String::new()),
        '\u{8}' | '\u{7f}' => {
            // Backspace: laatste teken weg, of regels samenvoegen.
            if let Some(last) = ed.lines.last_mut() {
                if last.pop().is_none() && ed.lines.len() > 1 {
                    ed.lines.pop();
                }
            }
        }
        '\t' => {
            if let Some(last) = ed.lines.last_mut() {
                last.push_str("    ");
            }
        }
        c if !c.is_control() => {
            if let Some(last) = ed.lines.last_mut() {
                last.push(c);
            }
        }
        _ => return,
    }
    ed.dirty = true;
    ed.status = "niet-opgeslagen wijzigingen".to_string();
}

/// Schrijf de buffer naar EuroFS (echt, duurzaam). Geeft `true` bij succes.
pub fn save(fs: &mut dyn FileSystem) -> bool {
    let (path, body) = {
        let ed = ED.lock();
        (ed.path.clone(), ed.lines.join("\n"))
    };
    let path = if path.is_empty() { DEFAULT_PATH.to_string() } else { path };
    // Zorg dat de map bestaat.
    if let Some(slash) = path.rfind('/') {
        if slash > 0 {
            let _ = fs.create_dir(&path[..slash]);
        }
    }
    let ok = fs.write_file(&path, body.as_bytes()).is_ok();
    let mut ed = ED.lock();
    ed.dirty = !ok;
    ed.status = if ok {
        alloc::format!("opgeslagen → {path} ({} B)", body.len())
    } else {
        alloc::format!("OPSLAAN MISLUKT → {path}")
    };
    serial_println!("[edit] save {} → {}", path, if ok { "OK" } else { "MISLUKT" });
    ok
}

/// Rechthoek van de "Opslaan"-knop in de werkbalk (voor de muis-hittest).
fn save_button(x: usize, y: usize, w: usize) -> (usize, usize, usize, usize) {
    let bw = 96;
    let bx = x + w.saturating_sub(bw + PAD);
    let by = y + TITLEBAR_H + 6;
    (bx, by, bw, TOOLBAR_H - 12)
}

/// Werd op de "Opslaan"-knop geklikt?
pub fn save_button_at(x: usize, y: usize, w: usize, mx: usize, my: usize) -> bool {
    let (bx, by, bw, bh) = save_button(x, y, w);
    mx >= bx && mx < bx + bw && my >= by && my < by + bh
}

pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    let ed = ED.lock();
    let accent = Color::rgb(0x2B, 0x6C, 0xB0);
    let ink = Color::rgb(0x20, 0x24, 0x2C);

    // Werkbalk.
    fb.fill_rect(bx, by, bw, TOOLBAR_H, Color::rgb(0xF1, 0xF3, 0xF7));
    let dot = if ed.dirty { "● " } else { "" };
    text::draw_px(fb, bx + PAD, by + 11, &alloc::format!("{dot}{}", ed.path), ink, 15.0);
    // Opslaan-knop.
    let (sx, sy, sw, sh) = save_button(x, y, w);
    fb.fill_rounded_rect(sx, sy, sw, sh, 6, accent);
    text::draw_px(fb, sx + 16, sy + 5, "Opslaan", Color::rgb(0xFF, 0xFF, 0xFF), 14.0);

    // Tekstgebied.
    let tx = bx + PAD;
    let mut ty = by + TOOLBAR_H + 8;
    let maxrows = bh.saturating_sub(TOOLBAR_H + 8 + 24) / LINE_H;
    let total = ed.lines.len();
    let start = total.saturating_sub(maxrows);
    for (i, line) in ed.lines.iter().enumerate().skip(start) {
        let last = i + 1 == total;
        let shown = if last {
            alloc::format!("{line}_") // caret aan het invoegpunt
        } else {
            line.clone()
        };
        text::draw_px(fb, tx, ty, &shown, ink, 15.0);
        ty += LINE_H;
    }

    // Statusbalk.
    let sb_y = by + bh.saturating_sub(22);
    fb.fill_rect(bx, sb_y, bw, 22, Color::rgb(0xEC, 0xEF, 0xF4));
    text::draw_px(
        fb,
        bx + PAD,
        sb_y + 4,
        &alloc::format!("EuroText · {} regels · {}", total, ed.status),
        Color::rgb(0x55, 0x5C, 0x68),
        12.5,
    );
}

/// **[edit]** — bewijs de bewerk-→-opslaan-→-herlees-cyclus op het ECHTE EuroFS,
/// onafhankelijk van toetsenbord/muis: typ tekst in, sla op, herlees, vergelijk.
pub fn selftest(fs: &mut dyn FileSystem) {
    let path = "/tmp/edit-selftest.txt";
    open(fs, path); // verse buffer (bestand bestaat niet → welkomsttekst)
    // Vervang de buffer door een gecontroleerde inhoud via input().
    {
        let mut ed = ED.lock();
        ed.lines = vec![String::new()];
    }
    for ch in "Hallo".chars() {
        input(ch);
    }
    input('\r');
    for ch in "EuroOS 42".chars() {
        input(ch);
    }
    input('\u{8}'); // backspace: "42" → "4"
    let saved = save(fs);
    // Herlees uit een verse open() — bewijst dat het écht op schijf staat.
    open(fs, path);
    let reread = ED.lock().lines.join("\n");
    let expected = "Hallo\nEuroOS 4";
    let ok = saved && reread == expected;
    let _ = fs.remove_file(path);
    serial_println!(
        "[edit] EuroText bewerken+opslaan+herlezen: schrijf-OK={saved}, herlezen={:?} (verwacht {:?}) → {}",
        reread,
        expected,
        if ok { "OK ✓" } else { "MISLUKT ✗" }
    );
}
