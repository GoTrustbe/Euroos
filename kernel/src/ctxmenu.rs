//! Right-click context menus: the overlay widget plus the action model. This is
//! the plumbing a desktop needs before "right-click and pick an option" exists
//! at all. The compositor decides *what* menu to show for the object under the
//! cursor (a file, the desktop, a dock tile, a text field); this module owns the
//! menu's geometry, drawing, hit-testing and dismissal, and hands the chosen
//! [`Action`] back for the compositor to carry out.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

/// What a menu item does when chosen. The compositor matches on this.
#[derive(Clone)]
pub enum Action {
    /// Open a file with its default handler.
    OpenFile(String),
    /// Navigate into a directory in the file manager.
    OpenDir(String),
    /// Put arbitrary text (e.g. a file path) on the system clipboard.
    CopyText(String),
    /// Create a new folder inside the given directory.
    NewFolder(String),
    /// Move a file to the trash (recoverable delete).
    Trash(String),
    /// Restore the most recently trashed item (undo delete).
    RestoreTrash,
    /// Paste the clipboard into the focused text field.
    Paste,
    /// Open (or focus) the terminal.
    OpenTerminal,
    /// Open the settings panel on its System section.
    OpenDisplaySettings,
    /// Open (or focus) the dock app at this tile index.
    OpenApp(usize),
    /// Repaint the desktop.
    Refresh,
}

/// One row in a menu. `action: None` renders as a disabled (greyed) row.
pub struct Item {
    pub label: String,
    pub shortcut: String,
    pub action: Option<Action>,
    pub sep_after: bool,
}

impl Item {
    pub fn new(label: &str, shortcut: &str, action: Action) -> Self {
        Item { label: label.to_string(), shortcut: shortcut.to_string(), action: Some(action), sep_after: false }
    }
    pub fn disabled(label: &str) -> Self {
        Item { label: label.to_string(), shortcut: String::new(), action: None, sep_after: false }
    }
    pub fn sep(mut self) -> Self {
        self.sep_after = true;
        self
    }
}

/// `[ctx]` boot self-test: a menu opens, an item is chosen with the right
/// action, the menu closes after a choice, and a click outside dismisses it.
pub fn selftest() {
    open(100, 100, alloc::vec![
        Item::new("Copy", "Ctrl C", Action::CopyText(String::from("proof"))),
        Item::disabled("Disabled"),
    ], 1920, 1080);
    let opened = is_open();
    let chosen = matches!(click_at(120, 120), Hit::Chosen(Action::CopyText(s)) if s == "proof");
    let closed = !is_open();
    open(100, 100, alloc::vec![Item::new("Refresh", "F5", Action::Refresh)], 1920, 1080);
    let dismissed = matches!(click_at(900, 900), Hit::Dismiss) && !is_open();
    let ok = opened && chosen && closed && dismissed;
    crate::serial_println!(
        "[ctx] Context menus: opens={opened}, item-chosen={chosen}, closes-after-choice={closed}, click-outside-dismisses={dismissed} → {}",
        if ok { "OK (right-click menus with real actions) ✓" } else { "FAILED ✗" }
    );
}

struct Menu {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    items: Vec<Item>,
}

static MENU: Mutex<Option<Menu>> = Mutex::new(None);

const ITEM_H: usize = 30;
const PAD_X: usize = 14;
const SEP_H: usize = 9;
const FONT: f32 = 13.0;

fn row_height(it: &Item) -> usize {
    ITEM_H + if it.sep_after { SEP_H } else { 0 }
}

/// Open a menu with these items, anchored at the click point and kept on screen.
pub fn open(x: usize, y: usize, items: Vec<Item>, screen_w: usize, screen_h: usize) {
    if items.is_empty() {
        return;
    }
    // Width = widest (label + gap + shortcut), clamped to a sensible range.
    let mut inner = 0usize;
    for it in &items {
        let lw = crate::text::width_px(&it.label, FONT);
        let sw = if it.shortcut.is_empty() { 0 } else { crate::text::width_px(&it.shortcut, FONT) + 28 };
        inner = inner.max(lw + sw);
    }
    let w = (inner + PAD_X * 2 + 18).clamp(176, 320);
    let h: usize = items.iter().map(row_height).sum::<usize>() + 10;
    // Keep the whole card on screen.
    let x = x.min(screen_w.saturating_sub(w + 4));
    let y = y.min(screen_h.saturating_sub(h + 4));
    *MENU.lock() = Some(Menu { x, y, w, h, items });
}

pub fn is_open() -> bool {
    MENU.lock().is_some()
}

pub fn close() {
    *MENU.lock() = None;
}

/// Result of a click while a menu is open.
pub enum Hit {
    /// The user chose an item; carry out this action.
    Chosen(Action),
    /// The click dismissed the menu (outside, or on a disabled row/separator).
    Dismiss,
}

/// Route a click at (mx,my). Always consumes the click and closes the menu.
pub fn click_at(mx: usize, my: usize) -> Hit {
    let taken = MENU.lock().take();
    let Some(m) = taken else { return Hit::Dismiss };
    if mx < m.x || mx >= m.x + m.w || my < m.y + 5 || my >= m.y + m.h {
        return Hit::Dismiss;
    }
    let mut cy = m.y + 5;
    for it in &m.items {
        if my >= cy && my < cy + ITEM_H {
            return match &it.action {
                Some(a) => Hit::Chosen(a.clone()),
                None => Hit::Dismiss,
            };
        }
        cy += row_height(it);
    }
    Hit::Dismiss
}

/// Draw the menu overlay (call after windows/dock, before the cursor). `mx,my`
/// is the live cursor position so the row under it highlights.
pub fn render(fb: &FrameBuffer, mx: usize, my: usize) {
    let g = MENU.lock();
    let Some(m) = g.as_ref() else { return };
    // A subtle offset backdrop reads as a drop shadow, then the card on top.
    fb.fill_rounded_rect(m.x + 1, m.y + 3, m.w, m.h, crate::eds::RADIUS_M, Color::BORDER);
    fb.fill_rounded_rect(m.x, m.y, m.w, m.h, crate::eds::RADIUS_M, Color::CARD);
    fb.draw_border(m.x, m.y, m.w, m.h, 1, Color::BORDER);

    let mut cy = m.y + 5;
    let over = mx >= m.x && mx < m.x + m.w;
    for it in &m.items {
        let hovered = over && my >= cy && my < cy + ITEM_H && it.action.is_some();
        if hovered {
            fb.fill_rounded_rect(m.x + 5, cy + 2, m.w - 10, ITEM_H - 4, crate::eds::RADIUS_S, Color::ACCENT_SOFT);
        }
        let label_c = if it.action.is_some() {
            if hovered { Color::ACCENT } else { Color::INK }
        } else {
            Color::TEXT_DIM
        };
        crate::text::draw_px(fb, m.x + PAD_X, cy + 7, &it.label, label_c, FONT);
        if !it.shortcut.is_empty() {
            let sw = crate::text::width_px(&it.shortcut, FONT);
            crate::text::draw_px(fb, m.x + m.w - PAD_X - sw, cy + 7, &it.shortcut, Color::TEXT_DIM, FONT);
        }
        cy += ITEM_H;
        if it.sep_after {
            fb.fill_rect(m.x + 10, cy + SEP_H / 2, m.w - 20, 1, Color::BORDER);
            cy += SEP_H;
        }
    }
}
