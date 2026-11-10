//! EuroUI forms (`euroui::form`): a scrollable column of labelled controls with
//! hit-testing, keyboard focus and events. This is what a settings screen is
//! built from; see `docs/SETTINGS.md` for a walk-through.
//!
//! The model is deliberately small:
//!
//! - The app describes its screen as a `Vec<Item>` built from its REAL state,
//!   every time it draws or handles input. An item is a label, an optional
//!   explanation (`hint`) and a control ([`Kind`]).
//! - The app keeps one [`FormState`] (focus, scroll position, an open dropdown,
//!   a text field being edited) between frames.
//! - [`draw`] paints, [`click`] and [`key`] return an [`Event`] naming the item
//!   that changed. The app applies it to its real state; the next draw shows
//!   the result. The form itself never owns a setting's value.
//!
//! Keyboard: Tab / Down / Up move the focus, Space or Enter activates, Left and
//! Right change a slider or dropdown, Enter applies a text field, Esc cancels,
//! PageUp / PageDown / Home / End scroll. Items that do not fit completely are
//! not drawn (the framebuffer has no clip rectangle), so scrolling moves by
//! whole items.

use alloc::string::String;
use alloc::vec::Vec;

use super::draw;
use crate::eds;
use crate::graphics::{Color, FrameBuffer};
use crate::ps2::Key;
use crate::text;

/// Identifies an item within one form; chosen by the app.
pub type Id = u16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, px: usize, py: usize) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn center(&self) -> (usize, usize) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// Sizes derived from the text scale (`euroui::text_scale`), so a larger text
/// setting grows rows and controls with it.
#[derive(Clone, Copy)]
pub struct Metrics {
    /// Label and control text size.
    pub label: f32,
    /// Explanation text size.
    pub hint: f32,
    pub line: usize,
    pub hint_line: usize,
    pub pad: usize,
    pub vpad: usize,
    pub gap: usize,
    /// Height of a field / dropdown / button.
    pub ctl_h: usize,
    /// Height of a list or dropdown row.
    pub row_h: usize,
    pct: usize,
}

impl Metrics {
    /// A pixel measure at the current text scale.
    pub fn u(&self, n: usize) -> usize {
        n * self.pct / 100
    }
}

pub fn metrics() -> Metrics {
    let pct = super::text_scale() as usize;
    let f = pct as f32 / 100.0;
    let u = |n: usize| n * pct / 100;
    Metrics {
        label: 13.5 * f,
        hint: 12.0 * f,
        line: u(19),
        hint_line: u(16),
        pad: eds::eu(3),
        vpad: eds::eu(2),
        gap: 6,
        ctl_h: u(28),
        row_h: u(24),
        pct,
    }
}

/// Width reserved for the scrollbar (always, so the layout does not jump).
const SCROLL_W: usize = 14;
/// A dropdown shows at most this many options at once.
const POPUP_MAX: usize = 8;

/// Break `s` into lines no wider than `max_w` at size `px` (word wrap; `\n`
/// forces a break; a single over-long word is cut).
pub fn wrap(s: &str, px: f32, max_w: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        for word in para.split(' ').filter(|w| !w.is_empty()) {
            let cand = if line.is_empty() { String::from(word) } else { alloc::format!("{line} {word}") };
            if text::width_px(&cand, px) <= max_w || line.is_empty() {
                line = cand;
            } else {
                out.push(core::mem::take(&mut line));
                line = String::from(word);
            }
            if text::width_px(&line, px) > max_w {
                line = fit(&line, px, max_w);
            }
        }
        out.push(line);
    }
    while out.len() > 1 && out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// `s`, or `s` cut with "..." so it is no wider than `max_w`.
pub fn fit(s: &str, px: f32, max_w: usize) -> String {
    if text::width_px(s, px) <= max_w {
        return String::from(s);
    }
    let mut t: Vec<char> = s.chars().collect();
    while !t.is_empty() {
        t.pop();
        let cand: String = t.iter().collect::<String>() + "...";
        if text::width_px(&cand, px) <= max_w {
            return cand;
        }
    }
    String::new()
}

/// What an item is.
#[derive(Clone)]
pub enum Kind {
    /// Sub-heading within a form.
    Heading,
    /// A paragraph of explanation (the label is the text; wrapped).
    Text,
    /// Read-only: label left, value right.
    Info(String),
    /// A block of ready-made lines (live kernel output). A line that starts
    /// with an em dash is drawn as a caption.
    Lines(Vec<String>),
    /// A usage bar with a value text.
    Meter { pct: u8, value: String },
    Toggle(bool),
    Checkbox(bool),
    /// Dropdown: one of `options`.
    Choice { options: Vec<String>, selected: usize },
    Slider { min: i32, max: i32, step: i32, value: i32, unit: &'static str },
    /// Single-line text field. `value` is the app's current value; Enter
    /// submits the edited text, Esc drops it.
    Field { value: String, placeholder: String, max_len: usize },
    /// Label (and hint) on the left, a button with `text` on the right.
    Button { text: String, primary: bool },
    /// Selection list showing `visible` rows at a time.
    List { rows: Vec<String>, selected: usize, visible: usize },
    #[allow(dead_code)]
    Divider,
    /// Vertical space in pixels.
    #[allow(dead_code)]
    Spacer(usize),
}

/// One row of a form.
#[derive(Clone)]
pub struct Item {
    pub id: Id,
    pub label: String,
    /// Explanation under the label: what the control does and its consequence.
    pub hint: String,
    pub kind: Kind,
    /// A disabled control is drawn dimmed and takes no focus or clicks.
    pub enabled: bool,
}

impl Item {
    pub fn new(id: Id, label: &str, kind: Kind) -> Self {
        Item { id, label: String::from(label), hint: String::new(), kind, enabled: true }
    }
    pub fn hint(mut self, hint: &str) -> Self {
        self.hint = String::from(hint);
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn heading(label: &str) -> Self {
        Item::new(0, label, Kind::Heading)
    }
    pub fn text(label: &str) -> Self {
        Item::new(0, label, Kind::Text)
    }
    pub fn info(label: &str, value: &str) -> Self {
        Item::new(0, label, Kind::Info(String::from(value)))
    }
    pub fn lines(lines: Vec<String>) -> Self {
        Item::new(0, "", Kind::Lines(lines))
    }
    pub fn meter(label: &str, pct: u8, value: &str) -> Self {
        Item::new(0, label, Kind::Meter { pct, value: String::from(value) })
    }
    pub fn toggle(id: Id, label: &str, on: bool) -> Self {
        Item::new(id, label, Kind::Toggle(on))
    }
    pub fn checkbox(id: Id, label: &str, on: bool) -> Self {
        Item::new(id, label, Kind::Checkbox(on))
    }
    pub fn choice(id: Id, label: &str, options: Vec<String>, selected: usize) -> Self {
        Item::new(id, label, Kind::Choice { options, selected })
    }
    pub fn slider(id: Id, label: &str, min: i32, max: i32, step: i32, value: i32, unit: &'static str) -> Self {
        Item::new(id, label, Kind::Slider { min, max, step: step.max(1), value, unit })
    }
    pub fn field(id: Id, label: &str, value: &str, placeholder: &str, max_len: usize) -> Self {
        Item::new(id, label, Kind::Field { value: String::from(value), placeholder: String::from(placeholder), max_len })
    }
    pub fn button(id: Id, label: &str, text: &str, primary: bool) -> Self {
        Item::new(id, label, Kind::Button { text: String::from(text), primary })
    }
    pub fn list(id: Id, label: &str, rows: Vec<String>, selected: usize, visible: usize) -> Self {
        Item::new(id, label, Kind::List { rows, selected, visible: visible.max(1) })
    }
    #[allow(dead_code)]
    pub fn divider() -> Self {
        Item::new(0, "", Kind::Divider)
    }
    #[allow(dead_code)]
    pub fn spacer(px: usize) -> Self {
        Item::new(0, "", Kind::Spacer(px))
    }

    /// Can this item take keyboard focus and clicks?
    pub fn focusable(&self) -> bool {
        self.enabled
            && matches!(
                self.kind,
                Kind::Toggle(_) | Kind::Checkbox(_) | Kind::Choice { .. } | Kind::Slider { .. } | Kind::Field { .. } | Kind::Button { .. } | Kind::List { .. }
            )
    }
}

/// What the user did. The app applies it to its real state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Toggle or checkbox flipped; the bool is the NEW state.
    Toggled(Id, bool),
    /// Dropdown option picked.
    Chosen(Id, usize),
    /// Slider moved to this value.
    Slid(Id, i32),
    /// Enter pressed in a text field.
    Submitted(Id, String),
    /// The text being edited changed (not applied yet).
    Edited(Id),
    Pressed(Id),
    /// List row selected.
    Selected(Id, usize),
}

/// The interaction state an app keeps per form.
pub struct FormState {
    /// The item with keyboard focus.
    pub focus: Option<Id>,
    /// Index of the first item shown (scroll position).
    pub top: usize,
    open: Option<Id>,
    open_hi: usize,
    open_top: usize,
    /// The field being edited: (id, text, cursor as a char index).
    edit: Option<(Id, String, usize)>,
}

impl FormState {
    pub const fn new() -> Self {
        FormState { focus: None, top: 0, open: None, open_hi: 0, open_top: 0, edit: None }
    }
    /// Forget focus, scroll and any edit (e.g. when the app shows another screen).
    pub fn reset(&mut self) {
        *self = FormState::new();
    }
    /// Is a text field being edited?
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }
    /// The uncommitted text of field `id`, if it is being edited.
    pub fn edit_text(&self, id: Id) -> Option<&str> {
        match &self.edit {
            Some((eid, text, _)) if *eid == id => Some(text.as_str()),
            _ => None,
        }
    }
    /// Is a dropdown open?
    pub fn popup_open(&self) -> bool {
        self.open.is_some()
    }
}

impl Default for FormState {
    fn default() -> Self {
        FormState::new()
    }
}

/// A visible item: its card, its control, and its wrapped texts.
pub struct Placed {
    pub idx: usize,
    /// The whole row (what a toggle or checkbox reacts to).
    pub row: Rect,
    /// The control itself.
    pub ctl: Rect,
    label_lines: Vec<String>,
    hint_lines: Vec<String>,
}

pub struct Layout {
    pub placed: Vec<Placed>,
    /// The scroll position actually used (clamped).
    pub top: usize,
    /// The largest useful scroll position.
    pub max_top: usize,
    pub track: Option<Rect>,
    pub thumb: Option<Rect>,
}

struct Measured {
    h: usize,
    ctl_w: usize,
    label_lines: Vec<String>,
    hint_lines: Vec<String>,
}

fn is_row_kind(k: &Kind) -> bool {
    matches!(k, Kind::Info(_) | Kind::Toggle(_) | Kind::Checkbox(_) | Kind::Choice { .. } | Kind::Slider { .. } | Kind::Field { .. } | Kind::Button { .. })
}

fn toggle_text_w(m: &Metrics) -> usize {
    text::width_px("Off", m.hint).max(text::width_px("On", m.hint)) + 8
}

fn measure(it: &Item, w: usize, m: &Metrics) -> Measured {
    let avail = w.saturating_sub(2 * m.pad);
    let none = Vec::new;
    match &it.kind {
        Kind::Heading => Measured { h: m.line + 12, ctl_w: 0, label_lines: none(), hint_lines: none() },
        Kind::Text => {
            let lines = wrap(&it.label, m.hint, w.saturating_sub(4));
            Measured { h: lines.len() * m.hint_line + 8, ctl_w: 0, label_lines: lines, hint_lines: none() }
        }
        Kind::Lines(l) => Measured { h: l.len() * m.u(18) + 2, ctl_w: 0, label_lines: none(), hint_lines: none() },
        Kind::Divider => Measured { h: 9, ctl_w: 0, label_lines: none(), hint_lines: none() },
        Kind::Spacer(n) => Measured { h: *n, ctl_w: 0, label_lines: none(), hint_lines: none() },
        Kind::Meter { .. } => {
            let hint_lines = if it.hint.is_empty() { none() } else { wrap(&it.hint, m.hint, avail) };
            let h = 2 * m.vpad + m.line + 6 + 8 + if hint_lines.is_empty() { 0 } else { 6 + hint_lines.len() * m.hint_line };
            Measured { h: h + m.gap, ctl_w: avail, label_lines: none(), hint_lines }
        }
        Kind::List { visible, .. } => {
            let label_lines = if it.label.is_empty() { none() } else { wrap(&it.label, m.label, avail) };
            let hint_lines = if it.hint.is_empty() { none() } else { wrap(&it.hint, m.hint, avail) };
            let head = label_lines.len() * m.line + hint_lines.len() * m.hint_line;
            let h = 2 * m.vpad + head + if head > 0 { 6 } else { 0 } + visible * m.row_h + 4;
            Measured { h: h + m.gap, ctl_w: avail, label_lines, hint_lines }
        }
        kind => {
            let ctl_w = match kind {
                Kind::Toggle(_) => m.u(40) + toggle_text_w(m),
                Kind::Checkbox(_) => m.u(20),
                Kind::Choice { options, .. } => {
                    let widest = options.iter().map(|o| text::width_px(o, m.label)).max().unwrap_or(0);
                    (widest + m.u(44)).clamp(m.u(150).min(avail), (avail * 62 / 100).max(1))
                }
                Kind::Slider { .. } => (m.u(150) + m.u(60)).min(avail * 62 / 100),
                Kind::Field { .. } => m.u(260).min(avail * 55 / 100),
                Kind::Button { text: t, .. } => (text::width_px(t, m.label) + m.u(28)).max(m.u(96)).min(avail * 62 / 100),
                Kind::Info(v) => text::width_px(v, m.label).min(avail * 62 / 100),
                _ => 0,
            };
            let left_w = avail.saturating_sub(ctl_w + m.pad).max(40);
            let label_lines = wrap(&it.label, m.label, left_w);
            let hint_lines = if it.hint.is_empty() { none() } else { wrap(&it.hint, m.hint, left_w) };
            let content = label_lines.len() * m.line + hint_lines.len() * m.hint_line + if hint_lines.is_empty() { 0 } else { 2 };
            let ctl_h = if matches!(kind, Kind::Info(_)) { m.line } else { m.ctl_h };
            Measured { h: content.max(ctl_h) + 2 * m.vpad + m.gap, ctl_w, label_lines, hint_lines }
        }
    }
}

/// Where every visible item sits within `area`.
pub fn layout(area: Rect, items: &[Item], st: &FormState) -> Layout {
    let m = metrics();
    let w = area.w.saturating_sub(SCROLL_W);
    let mut measured: Vec<Measured> = items.iter().map(|it| measure(it, w, &m)).collect();
    let total: usize = measured.iter().map(|g| g.h).sum();

    // The largest scroll position: the first index from which the rest fits.
    let mut max_top = items.len();
    let mut acc = 0usize;
    while max_top > 0 && acc + measured[max_top - 1].h <= area.h {
        acc += measured[max_top - 1].h;
        max_top -= 1;
    }
    if max_top == items.len() && max_top > 0 {
        max_top -= 1; // a single item taller than the area: still show from it
    }
    let top = st.top.min(max_top);

    let mut placed = Vec::new();
    let mut y = area.y;
    let mut shown_h = 0usize;
    for idx in top..items.len() {
        let h = measured[idx].h;
        if y + h > area.y + area.h {
            break;
        }
        let it = &items[idx];
        let g = &mut measured[idx];
        let gap = if is_row_kind(&it.kind) || matches!(it.kind, Kind::Meter { .. } | Kind::List { .. }) { m.gap } else { 0 };
        let row = Rect::new(area.x, y, w, h - gap);
        let ctl = match &it.kind {
            k if is_row_kind(k) => {
                let ch = if matches!(k, Kind::Info(_)) {
                    m.line
                } else if matches!(k, Kind::Checkbox(_)) {
                    m.u(20)
                } else {
                    m.ctl_h
                };
                Rect::new(row.x + row.w.saturating_sub(m.pad + g.ctl_w), row.y + row.h.saturating_sub(ch) / 2, g.ctl_w, ch)
            }
            Kind::Meter { .. } => Rect::new(row.x + m.pad, row.y + m.vpad + m.line + 6, row.w.saturating_sub(2 * m.pad), 8),
            Kind::List { visible, .. } => {
                let lh = visible * m.row_h + 4;
                Rect::new(row.x + m.pad, row.y + row.h.saturating_sub(m.vpad + lh), row.w.saturating_sub(2 * m.pad), lh)
            }
            _ => row,
        };
        placed.push(Placed {
            idx,
            row,
            ctl,
            label_lines: core::mem::take(&mut g.label_lines),
            hint_lines: core::mem::take(&mut g.hint_lines),
        });
        y += h;
        shown_h += h;
    }
    // A heading is not left alone at the bottom while what it introduces does
    // not fit: it comes with the next page instead.
    if placed.len() > 1 {
        if let Some(last) = placed.last() {
            if matches!(items[last.idx].kind, Kind::Heading) && last.idx + 1 < items.len() {
                shown_h -= measured[last.idx].h.min(shown_h);
                placed.pop();
            }
        }
    }

    let (track, thumb) = if max_top > 0 && total > 0 {
        let track = Rect::new(area.x + area.w.saturating_sub(SCROLL_W), area.y, SCROLL_W, area.h);
        let th = (area.h * shown_h.max(1) / total).clamp(24.min(area.h), area.h);
        let ty = area.y + (area.h - th) * top / max_top;
        (Some(track), Some(Rect::new(track.x, ty, SCROLL_W, th)))
    } else {
        (None, None)
    };
    Layout { placed, top, max_top, track, thumb }
}

/// The row and control rectangle of item `id`, if it is currently visible.
pub fn rect_of(area: Rect, items: &[Item], st: &FormState, id: Id) -> Option<(Rect, Rect)> {
    layout(area, items, st).placed.iter().find(|p| items[p.idx].id == id && id != 0).map(|p| (p.row, p.ctl))
}

/// The id of the interactive item under the pointer (for tooltips and tests).
pub fn hit(area: Rect, items: &[Item], st: &FormState, x: usize, y: usize) -> Option<Id> {
    layout(area, items, st)
        .placed
        .iter()
        .find(|p| items[p.idx].focusable() && p.row.contains(x, y))
        .map(|p| items[p.idx].id)
}

fn popup_rect(area: Rect, ctl: Rect, n: usize, m: &Metrics) -> (Rect, usize) {
    let vis = n.clamp(1, POPUP_MAX);
    let h = vis * m.row_h + 4;
    let below = ctl.y + ctl.h + 2;
    let y = if below + h <= area.y + area.h {
        below
    } else if ctl.y >= area.y + h + 2 {
        ctl.y - h - 2
    } else {
        (area.y + area.h).saturating_sub(h).max(area.y)
    };
    (Rect::new(ctl.x, y, ctl.w, h), vis)
}

/// Screen position of option `option` in the open dropdown `id` (for test
/// harnesses that click through a form); `None` if it is not open or the option
/// is scrolled out of the popup.
pub fn popup_option_center(area: Rect, items: &[Item], st: &FormState, id: Id, option: usize) -> Option<(usize, usize)> {
    if st.open != Some(id) {
        return None;
    }
    let m = metrics();
    let lay = layout(area, items, st);
    let p = lay.placed.iter().find(|p| items[p.idx].id == id)?;
    let Kind::Choice { options, .. } = &items[p.idx].kind else { return None };
    let (pr, vis) = popup_rect(area, p.ctl, options.len(), &m);
    let first = st.open_top.min(options.len().saturating_sub(vis));
    if option < first || option >= first + vis || option >= options.len() {
        return None;
    }
    Some((pr.x + pr.w / 3, pr.y + 2 + (option - first) * m.row_h + m.row_h / 2))
}

/// First row shown by a list so that the selection is visible.
fn list_first(selected: usize, visible: usize) -> usize {
    if selected < visible {
        0
    } else {
        selected + 1 - visible
    }
}

fn slider_track(ctl: Rect, m: &Metrics) -> Rect {
    Rect::new(ctl.x, ctl.y, ctl.w.saturating_sub(m.u(60)).max(20), ctl.h)
}

fn toggle_track(ctl: Rect, m: &Metrics) -> Rect {
    let th = m.u(22).min(ctl.h);
    Rect::new(ctl.x + ctl.w.saturating_sub(m.u(40)), ctl.y + (ctl.h - th) / 2, m.u(40), th)
}

// ── Drawing ────────────────────────────────────────────────────────────────

/// Paint the form into `area`.
pub fn draw(fb: &FrameBuffer, area: Rect, items: &[Item], st: &FormState) {
    let m = metrics();
    let lay = layout(area, items, st);
    for p in &lay.placed {
        draw_item(fb, &items[p.idx], p, st, &m);
    }
    if let (Some(track), Some(thumb)) = (lay.track, lay.thumb) {
        draw::scrollbar(fb, track, thumb);
    }
    // An open dropdown is drawn last, over the rows below it.
    if let Some(open) = st.open {
        if let Some(p) = lay.placed.iter().find(|p| items[p.idx].id == open) {
            if let Kind::Choice { options, selected } = &items[p.idx].kind {
                draw_popup(fb, area, p.ctl, options, *selected, st, &m);
            }
        }
    }
}

fn draw_popup(fb: &FrameBuffer, area: Rect, ctl: Rect, options: &[String], selected: usize, st: &FormState, m: &Metrics) {
    let n = options.len();
    let (pr, vis) = popup_rect(area, ctl, n, m);
    fb.fill_rounded_rect(pr.x, pr.y, pr.w, pr.h, eds::RADIUS_S, Color::TEXT_SEC);
    fb.fill_rounded_rect(pr.x + 1, pr.y + 1, pr.w - 2, pr.h - 2, eds::RADIUS_S - 1, Color::SURFACE);
    let first = st.open_top.min(n.saturating_sub(vis));
    let scroll = n > vis;
    let text_w = pr.w.saturating_sub(20 + if scroll { SCROLL_W } else { 0 });
    for (row, i) in (first..(first + vis).min(n)).enumerate() {
        let ry = pr.y + 2 + row * m.row_h;
        let hi = i == st.open_hi;
        if hi {
            fb.fill_rounded_rect(pr.x + 3, ry, pr.w.saturating_sub(6 + if scroll { SCROLL_W } else { 0 }), m.row_h, 6, Color::ACCENT_SOFT);
        }
        let c = if hi || i == selected { Color::ACCENT } else { Color::INK };
        text::draw_px(fb, pr.x + 10, draw::text_y(ry, m.row_h, m.label), &fit(&options[i], m.label, text_w), c, m.label);
    }
    if scroll {
        let track = Rect::new(pr.x + pr.w - SCROLL_W - 1, pr.y + 3, SCROLL_W, pr.h - 6);
        let th = (track.h * vis / n).max(16);
        let ty = track.y + (track.h - th) * first / (n - vis);
        draw::scrollbar(fb, track, Rect::new(track.x, ty, SCROLL_W, th));
    }
}

fn draw_item(fb: &FrameBuffer, it: &Item, p: &Placed, st: &FormState, m: &Metrics) {
    let focused = st.focus == Some(it.id) && it.focusable();
    let ink = if it.enabled { Color::INK } else { Color::TEXT_DIM };
    let (row, ctl) = (p.row, p.ctl);
    match &it.kind {
        Kind::Heading => {
            text::draw_px_styled(fb, row.x + 2, row.y + 10, &it.label, Color::ACCENT, m.label, text::STYLE_BOLD, 0);
        }
        Kind::Text => {
            for (i, l) in p.label_lines.iter().enumerate() {
                text::draw_px(fb, row.x + 2, row.y + 2 + i * m.hint_line, l, Color::TEXT_SEC, m.hint);
            }
        }
        Kind::Lines(lines) => {
            for (i, l) in lines.iter().enumerate() {
                let c = if l.starts_with('\u{2014}') { Color::ACCENT } else { Color::TEXT_SEC };
                text::draw_px(fb, row.x + 2, row.y + 1 + i * m.u(18), &fit(l.trim_end(), m.hint, row.w.saturating_sub(4)), c, m.hint);
            }
        }
        Kind::Divider => fb.fill_rect(row.x, row.y + 4, row.w, 1, Color::BORDER),
        Kind::Spacer(_) => {}
        Kind::Meter { pct, value } => {
            fb.fill_rounded_rect(row.x, row.y, row.w, row.h, eds::RADIUS_S, Color::CARD);
            let vw = text::width_px(value, m.hint);
            text::draw_px(fb, row.x + m.pad, row.y + m.vpad, &fit(&it.label, m.label, row.w.saturating_sub(3 * m.pad + vw)), ink, m.label);
            text::draw_px(fb, row.x + row.w.saturating_sub(m.pad + vw), row.y + m.vpad + 1, value, Color::TEXT_SEC, m.hint);
            draw::meter(fb, ctl, *pct);
            for (i, l) in p.hint_lines.iter().enumerate() {
                text::draw_px(fb, row.x + m.pad, ctl.y + ctl.h + 6 + i * m.hint_line, l, Color::TEXT_SEC, m.hint);
            }
        }
        Kind::List { rows, selected, visible } => {
            fb.fill_rounded_rect(row.x, row.y, row.w, row.h, eds::RADIUS_S, Color::CARD);
            let mut y = row.y + m.vpad;
            for l in &p.label_lines {
                text::draw_px(fb, row.x + m.pad, y, l, ink, m.label);
                y += m.line;
            }
            for l in &p.hint_lines {
                text::draw_px(fb, row.x + m.pad, y, l, Color::TEXT_SEC, m.hint);
                y += m.hint_line;
            }
            draw::input_box(fb, ctl, focused, it.enabled);
            let first = list_first(*selected, *visible);
            let more = rows.len() > *visible;
            let text_w = ctl.w.saturating_sub(20 + if more { SCROLL_W } else { 0 });
            for (r, i) in (first..(first + visible).min(rows.len())).enumerate() {
                let ry = ctl.y + 2 + r * m.row_h;
                let sel = i == *selected;
                if sel {
                    fb.fill_rounded_rect(ctl.x + 3, ry, ctl.w.saturating_sub(6 + if more { SCROLL_W } else { 0 }), m.row_h, 6, Color::ACCENT_SOFT);
                }
                let c = if sel { Color::ACCENT } else { ink };
                text::draw_px(fb, ctl.x + 10, draw::text_y(ry, m.row_h, m.label), &fit(&rows[i], m.label, text_w), c, m.label);
            }
            if rows.is_empty() {
                text::draw_px(fb, ctl.x + 10, draw::text_y(ctl.y + 2, m.row_h, m.label), "(none)", Color::TEXT_DIM, m.label);
            }
            if more {
                let track = Rect::new(ctl.x + ctl.w - SCROLL_W - 1, ctl.y + 3, SCROLL_W, ctl.h - 6);
                let th = (track.h * visible / rows.len()).max(16);
                let ty = track.y + (track.h - th) * first / (rows.len() - visible);
                draw::scrollbar(fb, track, Rect::new(track.x, ty, SCROLL_W, th));
            }
        }
        kind => {
            // A labelled row: card, label + explanation left, control right.
            fb.fill_rounded_rect(row.x, row.y, row.w, row.h, eds::RADIUS_S, Color::CARD);
            let content = p.label_lines.len() * m.line + p.hint_lines.len() * m.hint_line + if p.hint_lines.is_empty() { 0 } else { 2 };
            let mut y = row.y + row.h.saturating_sub(content) / 2;
            let label_color = if matches!(kind, Kind::Info(_)) { Color::TEXT_SEC } else { ink };
            for l in &p.label_lines {
                text::draw_px(fb, row.x + m.pad, y, l, label_color, m.label);
                y += m.line;
            }
            y += 2;
            for l in &p.hint_lines {
                text::draw_px(fb, row.x + m.pad, y, l, Color::TEXT_SEC, m.hint);
                y += m.hint_line;
            }
            match kind {
                Kind::Info(v) => {
                    let shown = fit(v, m.label, ctl.w);
                    let vw = text::width_px(&shown, m.label);
                    text::draw_px(fb, ctl.x + ctl.w.saturating_sub(vw), ctl.y, &shown, Color::INK, m.label);
                }
                Kind::Toggle(on) => {
                    let track = toggle_track(ctl, m);
                    let word = if *on { "On" } else { "Off" };
                    let c = if !it.enabled { Color::TEXT_DIM } else if *on { eds::SEC_VERIFIED } else { Color::TEXT_SEC };
                    let ww = text::width_px(word, m.hint);
                    text::draw_px(fb, track.x.saturating_sub(ww + 8), draw::text_y(ctl.y, ctl.h, m.hint), word, c, m.hint);
                    draw::toggle(fb, track, *on, it.enabled);
                    if focused {
                        draw::focus_ring(fb, track);
                    }
                }
                Kind::Checkbox(on) => {
                    draw::checkbox(fb, ctl, *on, it.enabled);
                    if focused {
                        draw::focus_ring(fb, ctl);
                    }
                }
                Kind::Choice { options, selected } => {
                    let open = st.open == Some(it.id);
                    draw::input_box(fb, ctl, focused || open, it.enabled);
                    let cur = options.get(*selected).map(String::as_str).unwrap_or("");
                    text::draw_px(fb, ctl.x + 10, draw::text_y(ctl.y, ctl.h, m.label), &fit(cur, m.label, ctl.w.saturating_sub(36)), ink, m.label);
                    draw::chevron(fb, ctl.x + ctl.w.saturating_sub(15), ctl.y + ctl.h / 2, if it.enabled { Color::TEXT_SEC } else { Color::TEXT_DIM });
                }
                Kind::Slider { min, max, value, unit, .. } => {
                    let track = slider_track(ctl, m);
                    let den = (*max - *min).max(1) as usize;
                    let num = (*value - *min).clamp(0, *max - *min) as usize;
                    draw::slider(fb, track, num, den, it.enabled);
                    let vt = alloc::format!("{value}{unit}");
                    let vw = text::width_px(&vt, m.label);
                    text::draw_px(fb, ctl.x + ctl.w.saturating_sub(vw), draw::text_y(ctl.y, ctl.h, m.label), &vt, ink, m.label);
                    if focused {
                        draw::focus_ring(fb, Rect::new(track.x, track.y + 2, track.w, track.h.saturating_sub(4)));
                    }
                }
                Kind::Field { value, placeholder, .. } => {
                    let editing = st.edit_text(it.id);
                    draw::input_box(fb, ctl, focused, it.enabled);
                    let inner = ctl.w.saturating_sub(20);
                    let ty = draw::text_y(ctl.y, ctl.h, m.label);
                    match editing {
                        Some(buf) => {
                            // Show the part up to the cursor that fits, cursor at its end.
                            let cur = st.edit.as_ref().map(|e| e.2).unwrap_or(0);
                            let head: Vec<char> = buf.chars().take(cur).collect();
                            let mut start = 0;
                            while start < head.len() && text::width_px(&head[start..].iter().collect::<String>(), m.label) > inner {
                                start += 1;
                            }
                            let before: String = head[start..].iter().collect();
                            let after: String = buf.chars().skip(cur).collect();
                            let bw = text::width_px(&before, m.label);
                            text::draw_px(fb, ctl.x + 10, ty, &before, Color::INK, m.label);
                            if !after.is_empty() && inner > bw + 8 {
                                text::draw_px(fb, ctl.x + 10 + bw, ty, &fit(&after, m.label, inner - bw), Color::INK, m.label);
                            }
                            fb.fill_rect(ctl.x + 10 + bw, ctl.y + 5, 1, ctl.h.saturating_sub(10), Color::ACCENT);
                        }
                        None if value.is_empty() => text::draw_px(fb, ctl.x + 10, ty, &fit(placeholder, m.label, inner), Color::TEXT_DIM, m.label),
                        None => text::draw_px(fb, ctl.x + 10, ty, &fit(value, m.label, inner), ink, m.label),
                    }
                }
                Kind::Button { text: caption, primary } => {
                    draw::button(fb, ctl, caption, *primary, it.enabled, m);
                    if focused {
                        draw::focus_ring(fb, ctl);
                    }
                }
                _ => {}
            }
        }
    }
}

// ── Input ──────────────────────────────────────────────────────────────────

fn begin_edit(st: &mut FormState, it: &Item) {
    if let Kind::Field { value, .. } = &it.kind {
        if st.edit_text(it.id).is_none() {
            st.edit = Some((it.id, value.clone(), value.chars().count()));
        }
    }
}

/// A left click at (x, y). Sets the focus and returns what changed, if anything.
pub fn click(area: Rect, items: &[Item], st: &mut FormState, x: usize, y: usize) -> Option<Event> {
    let m = metrics();
    let lay = layout(area, items, st);
    st.top = lay.top;

    // An open dropdown takes the click: pick an option, page its list, or close.
    if let Some(open) = st.open {
        let hit = lay.placed.iter().find(|p| items[p.idx].id == open).and_then(|p| match &items[p.idx].kind {
            Kind::Choice { options, .. } => Some((p.ctl, options.len())),
            _ => None,
        });
        if let Some((ctl, n)) = hit {
            let (pr, vis) = popup_rect(area, ctl, n, &m);
            if pr.contains(x, y) {
                if n > vis && x >= pr.x + pr.w.saturating_sub(SCROLL_W + 1) {
                    let max = n - vis;
                    st.open_top = if y < pr.y + pr.h / 2 { st.open_top.saturating_sub(vis) } else { (st.open_top + vis).min(max) };
                    return None;
                }
                let i = st.open_top.min(n.saturating_sub(vis)) + y.saturating_sub(pr.y + 2) / m.row_h.max(1);
                if i < n {
                    st.open = None;
                    return Some(Event::Chosen(open, i));
                }
                return None;
            }
        }
        st.open = None;
        return None;
    }

    // The scrollbar: a click above or below the thumb pages.
    if let Some(track) = lay.track {
        if track.contains(x, y) {
            if let Some(thumb) = lay.thumb {
                if y < thumb.y {
                    page(area, items, st, false);
                } else if y >= thumb.y + thumb.h {
                    page(area, items, st, true);
                }
            }
            return None;
        }
    }

    for p in &lay.placed {
        let it = &items[p.idx];
        if !it.focusable() {
            continue;
        }
        let inside = match it.kind {
            Kind::Toggle(_) | Kind::Checkbox(_) => p.row.contains(x, y),
            _ => p.ctl.contains(x, y),
        };
        if !inside {
            continue;
        }
        if st.edit.as_ref().map(|e| e.0) != Some(it.id) {
            st.edit = None;
        }
        st.focus = Some(it.id);
        return match &it.kind {
            Kind::Toggle(on) | Kind::Checkbox(on) => Some(Event::Toggled(it.id, !on)),
            Kind::Choice { options, selected } => {
                st.open = Some(it.id);
                st.open_hi = (*selected).min(options.len().saturating_sub(1));
                let vis = options.len().clamp(1, POPUP_MAX);
                st.open_top = list_first(st.open_hi, vis);
                None
            }
            Kind::Slider { min, max, step, .. } => {
                let track = slider_track(p.ctl, &m);
                let run = track.w.saturating_sub(16).max(1) as i64;
                let pos = (x.saturating_sub(track.x + 8) as i64).clamp(0, run);
                let span = (*max - *min).max(0) as i64;
                let step = (*step).max(1) as i64;
                let raw = pos * span / run;
                let snapped = ((raw + step / 2) / step * step).clamp(0, span);
                Some(Event::Slid(it.id, *min + snapped as i32))
            }
            Kind::Field { .. } => {
                begin_edit(st, it);
                None
            }
            Kind::Button { .. } => Some(Event::Pressed(it.id)),
            Kind::List { rows, selected, visible } => {
                let i = list_first(*selected, *visible) + y.saturating_sub(p.ctl.y + 2) / m.row_h.max(1);
                if i < rows.len() && x < p.ctl.x + p.ctl.w.saturating_sub(if rows.len() > *visible { SCROLL_W } else { 0 }) {
                    Some(Event::Selected(it.id, i))
                } else {
                    None
                }
            }
            _ => None,
        };
    }
    // A click on nothing drops the focus and any uncommitted edit.
    st.focus = None;
    st.edit = None;
    None
}

/// Scroll by one page (whole items).
pub fn page(area: Rect, items: &[Item], st: &mut FormState, down: bool) {
    let lay = layout(area, items, st);
    if down {
        let last = lay.placed.last().map(|p| p.idx).unwrap_or(lay.top);
        st.top = last.max(lay.top + 1).min(lay.max_top);
    } else {
        // Walk back until one page of items is above the current top.
        let m = metrics();
        let w = area.w.saturating_sub(SCROLL_W);
        let mut top = lay.top;
        let mut acc = 0usize;
        while top > 0 {
            let h = measure(&items[top - 1], w, &m).h;
            if acc + h > area.h && acc > 0 {
                break;
            }
            acc += h;
            top -= 1;
        }
        st.top = top;
    }
}

/// Scroll by `delta` items (negative = up). For a mouse wheel.
pub fn scroll(area: Rect, items: &[Item], st: &mut FormState, delta: i32) {
    let lay = layout(area, items, st);
    let top = lay.top as i64 + delta as i64;
    st.top = top.clamp(0, lay.max_top as i64) as usize;
}

fn ensure_visible(area: Rect, items: &[Item], st: &mut FormState, idx: usize) {
    if idx < st.top {
        // Bring a heading directly above it along.
        st.top = if idx > 0 && matches!(items[idx - 1].kind, Kind::Heading) { idx - 1 } else { idx };
        return;
    }
    let mut guard = 0;
    while guard <= items.len() {
        let lay = layout(area, items, st);
        st.top = lay.top;
        if lay.placed.iter().any(|p| p.idx == idx) || st.top >= idx || st.top >= lay.max_top {
            break;
        }
        st.top += 1;
        guard += 1;
    }
}

fn step_choice(it: &Item, forward: bool) -> Option<Event> {
    if let Kind::Choice { options, selected } = &it.kind {
        let n = options.len();
        if n == 0 {
            return None;
        }
        let next = if forward { (*selected + 1).min(n - 1) } else { selected.saturating_sub(1) };
        if next != *selected {
            return Some(Event::Chosen(it.id, next));
        }
    }
    None
}

/// A key press. Returns what changed, if anything. When no item has the focus
/// and the key is Up/Down/Esc, nothing happens here and the app may use the key
/// itself (e.g. to move between screens) — check `st.focus` before calling.
pub fn key(area: Rect, items: &[Item], st: &mut FormState, k: Key) -> Option<Event> {
    // 1. An open dropdown has the keyboard.
    if let Some(open) = st.open {
        let n = match items.iter().find(|i| i.id == open).map(|i| &i.kind) {
            Some(Kind::Choice { options, .. }) if !options.is_empty() => options.len(),
            _ => {
                st.open = None;
                return None;
            }
        };
        let vis = n.clamp(1, POPUP_MAX);
        match k {
            Key::Up => st.open_hi = st.open_hi.saturating_sub(1),
            Key::Down => st.open_hi = (st.open_hi + 1).min(n - 1),
            Key::PageUp => st.open_hi = st.open_hi.saturating_sub(vis),
            Key::PageDown => st.open_hi = (st.open_hi + vis).min(n - 1),
            Key::Home => st.open_hi = 0,
            Key::End => st.open_hi = n - 1,
            Key::Enter | Key::Char(' ') => {
                st.open = None;
                return Some(Event::Chosen(open, st.open_hi.min(n - 1)));
            }
            Key::Esc | Key::Tab => st.open = None,
            _ => {}
        }
        st.open_hi = st.open_hi.min(n - 1);
        if st.open_hi < st.open_top {
            st.open_top = st.open_hi;
        }
        if st.open_hi >= st.open_top + vis {
            st.open_top = st.open_hi + 1 - vis;
        }
        return None;
    }

    // 2. A text field being edited has the keyboard (except focus movement).
    if let Some((id, mut buf, mut cur)) = st.edit.take() {
        let max_len = match items.iter().find(|i| i.id == id).map(|i| &i.kind) {
            Some(Kind::Field { max_len, .. }) => *max_len,
            _ => 0,
        };
        let count = buf.chars().count();
        cur = cur.min(count);
        let byte_at = |s: &str, ci: usize| s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len());
        // Tab / Up / Down move on and drop the uncommitted text (falls through).
        if !matches!(k, Key::Tab | Key::Up | Key::Down) {
            match k {
                Key::Char(c) if !c.is_control() => {
                    if count < max_len {
                        let b = byte_at(&buf, cur);
                        buf.insert(b, c);
                        cur += 1;
                    }
                }
                Key::Backspace => {
                    if cur > 0 {
                        let b = byte_at(&buf, cur - 1);
                        buf.remove(b);
                        cur -= 1;
                    }
                }
                Key::Delete => {
                    if cur < count {
                        let b = byte_at(&buf, cur);
                        buf.remove(b);
                    }
                }
                Key::Left => cur = cur.saturating_sub(1),
                Key::Right => cur = (cur + 1).min(count),
                Key::Home => cur = 0,
                Key::End => cur = count,
                Key::Enter => return Some(Event::Submitted(id, buf)),
                Key::Esc => return None,
                _ => {}
            }
            st.edit = Some((id, buf, cur));
            return Some(Event::Edited(id));
        }
    }

    // 3. Focus movement, activation, scrolling.
    let order: Vec<usize> = items.iter().enumerate().filter(|(_, i)| i.focusable() && i.id != 0).map(|(i, _)| i).collect();
    let pos = st.focus.and_then(|id| order.iter().position(|&i| items[i].id == id));
    let cur = pos.map(|p| &items[order[p]]);

    // A focused list uses Up/Down for its selection.
    if let (Some(it), Key::Up | Key::Down) = (cur, k) {
        if let Kind::List { rows, selected, .. } = &it.kind {
            let next = if k == Key::Down { (*selected + 1).min(rows.len().saturating_sub(1)) } else { selected.saturating_sub(1) };
            return if next != *selected && !rows.is_empty() { Some(Event::Selected(it.id, next)) } else { None };
        }
    }

    match k {
        Key::Tab | Key::Down => {
            if order.is_empty() {
                return None;
            }
            let next = match pos {
                Some(p) if p + 1 < order.len() => p + 1,
                Some(p) if k == Key::Down => p,
                _ => 0,
            };
            st.focus = Some(items[order[next]].id);
            ensure_visible(area, items, st, order[next]);
        }
        Key::Up => match pos {
            Some(0) | None => {
                st.focus = None;
                st.top = 0;
            }
            Some(p) => {
                st.focus = Some(items[order[p - 1]].id);
                ensure_visible(area, items, st, order[p - 1]);
            }
        },
        Key::Esc => st.focus = None,
        Key::PageDown => page(area, items, st, true),
        Key::PageUp => page(area, items, st, false),
        Key::Home => st.top = 0,
        Key::End => st.top = layout(area, items, st).max_top,
        Key::Enter | Key::Char(' ') => {
            let it = cur?;
            return match &it.kind {
                Kind::Toggle(on) | Kind::Checkbox(on) => Some(Event::Toggled(it.id, !on)),
                Kind::Button { .. } => Some(Event::Pressed(it.id)),
                Kind::Choice { options, selected } => {
                    st.open = Some(it.id);
                    st.open_hi = (*selected).min(options.len().saturating_sub(1));
                    st.open_top = list_first(st.open_hi, options.len().clamp(1, POPUP_MAX));
                    None
                }
                Kind::Field { .. } => {
                    begin_edit(st, it);
                    None
                }
                _ => None,
            };
        }
        Key::Left | Key::Right => {
            let it = cur?;
            let fwd = k == Key::Right;
            return match &it.kind {
                Kind::Slider { min, max, step, value, .. } => {
                    let next = if fwd { (*value + *step).min(*max) } else { (*value - *step).max(*min) };
                    if next != *value {
                        Some(Event::Slid(it.id, next))
                    } else {
                        None
                    }
                }
                Kind::Choice { .. } => step_choice(it, fwd),
                _ => None,
            };
        }
        // Typing on a focused field starts editing it.
        Key::Char(c) if !c.is_control() => {
            let it = cur?;
            if let Kind::Field { max_len, .. } = &it.kind {
                begin_edit(st, it);
                if let Some((_, buf, cur)) = st.edit.as_mut() {
                    if buf.chars().count() < *max_len {
                        buf.push(c);
                        *cur = buf.chars().count();
                    }
                }
                return Some(Event::Edited(it.id));
            }
        }
        Key::Backspace => {
            let it = cur?;
            if matches!(it.kind, Kind::Field { .. }) {
                begin_edit(st, it);
                if let Some((_, buf, cur)) = st.edit.as_mut() {
                    buf.pop();
                    *cur = buf.chars().count();
                }
                return Some(Event::Edited(it.id));
            }
        }
        _ => {}
    }
    None
}

/// `[uikit]` boot self-test — the form logic without a framebuffer: hit-testing,
/// Tab order, dropdown, slider, text field, checkbox, scrolling, disabled items,
/// word wrap.
pub fn selftest() {
    let build = |tog: bool, chk: bool, sel: usize, val: i32| -> Vec<Item> {
        let mut v = alloc::vec![
            Item::heading("Test form"),
            Item::toggle(1, "A switch", tog).hint("What it does and what follows from it."),
            Item::checkbox(2, "A checkbox", chk),
            Item::choice(3, "A dropdown", (0..12).map(|i| alloc::format!("Option {i}")).collect(), sel),
            Item::slider(4, "A slider", 90, 130, 10, val, "%"),
            Item::field(5, "A field", "", "type here", 16),
            Item::button(6, "An action", "Run", true),
            Item::button(8, "Unavailable", "Run", false).enabled(false),
        ];
        for i in 0..30 {
            v.push(Item::info(&alloc::format!("Row {i}"), "value"));
        }
        v.push(Item::list(7, "A list", alloc::vec![String::from("one"), String::from("two"), String::from("three")], 0, 2));
        v
    };
    let area = Rect::new(100, 100, 520, 520);
    let mut st = FormState::new();
    let items = build(false, false, 0, 100);
    let at = |st: &FormState, items: &[Item], id: Id| rect_of(area, items, st, id).map(|(_, c)| c.center()).unwrap_or((0, 0));

    // Hit-testing: a click on the toggle's control flips it; the row counts too.
    let (tx, ty) = at(&st, &items, 1);
    let hit_ok = click(area, &items, &mut st, tx, ty) == Some(Event::Toggled(1, true))
        && st.focus == Some(1)
        && hit(area, &items, &st, area.x + 20, ty) == Some(1)
        && click(area, &items, &mut st, area.x + 20, ty) == Some(Event::Toggled(1, true));

    // Tab order follows the items and skips the disabled one.
    st.reset();
    let mut order = Vec::new();
    for _ in 0..8 {
        key(area, &items, &mut st, Key::Tab);
        order.push(st.focus.unwrap_or(0));
    }
    let tab_ok = order == [1, 2, 3, 4, 5, 6, 7, 1];

    // Checkbox by keyboard.
    st.reset();
    st.focus = Some(2);
    let check_ok = key(area, &items, &mut st, Key::Char(' ')) == Some(Event::Toggled(2, true));

    // Dropdown: click opens, a click on the second row picks option 1; by
    // keyboard End + Enter picks the last (scrolled) option.
    st.reset();
    let (cx, cy) = at(&st, &items, 3);
    let opened = click(area, &items, &mut st, cx, cy).is_none() && st.popup_open();
    let m = metrics();
    let (_, ctl) = rect_of(area, &items, &st, 3).unwrap_or_default();
    let (pr, _) = popup_rect(area, ctl, 12, &m);
    let picked = click(area, &items, &mut st, pr.x + 20, pr.y + 2 + m.row_h + m.row_h / 2) == Some(Event::Chosen(3, 1)) && !st.popup_open();
    st.focus = Some(3);
    key(area, &items, &mut st, Key::Enter);
    key(area, &items, &mut st, Key::End);
    let last = key(area, &items, &mut st, Key::Enter) == Some(Event::Chosen(3, 11));
    let outside_closes = {
        key(area, &items, &mut st, Key::Enter);
        click(area, &items, &mut st, area.x + 5, area.y + 5).is_none() && !st.popup_open()
    };
    let choice_ok = opened && picked && last && outside_closes;

    // Slider: a click at the far right gives the maximum, Left steps down.
    st.reset();
    let (_, sctl) = rect_of(area, &items, &st, 4).unwrap_or_default();
    let track = slider_track(sctl, &m);
    let max_ok = click(area, &items, &mut st, track.x + track.w - 1, track.y + track.h / 2) == Some(Event::Slid(4, 130));
    let mid_ok = click(area, &items, &mut st, track.x + track.w / 2, track.y + track.h / 2) == Some(Event::Slid(4, 110));
    let step_ok = key(area, &items, &mut st, Key::Left) == Some(Event::Slid(4, 90));
    let slider_ok = max_ok && mid_ok && step_ok;

    // Text field: click, type, edit, Enter submits; Esc drops the text.
    st.reset();
    let (fx, fy) = at(&st, &items, 5);
    click(area, &items, &mut st, fx, fy);
    for k in [Key::Char('a'), Key::Char('b'), Key::Backspace, Key::Char('c'), Key::Left, Key::Char('x'), Key::Home, Key::Delete] {
        key(area, &items, &mut st, k);
    }
    let live = st.edit_text(5) == Some("xc");
    let submit = key(area, &items, &mut st, Key::Enter) == Some(Event::Submitted(5, String::from("xc"))) && !st.editing();
    key(area, &items, &mut st, Key::Char('z'));
    let esc = st.editing() && key(area, &items, &mut st, Key::Esc).is_none() && !st.editing();
    for _ in 0..40 {
        key(area, &items, &mut st, Key::Char('m'));
    }
    let capped = st.edit_text(5).map(|t| t.len()) == Some(16);
    let field_ok = live && submit && esc && capped;

    // Scrolling: the form is taller than the area; PageDown moves on, End shows
    // the last item, Home returns; focus scrolls its item into view.
    st.reset();
    let lay0 = layout(area, &items, &st);
    let scrollable = lay0.max_top > 0 && lay0.thumb.is_some() && rect_of(area, &items, &st, 7).is_none();
    key(area, &items, &mut st, Key::PageDown);
    let moved = st.top > 0 && rect_of(area, &items, &st, 1).is_none();
    key(area, &items, &mut st, Key::End);
    let end = rect_of(area, &items, &st, 7).is_some();
    key(area, &items, &mut st, Key::Home);
    let home = st.top == 0;
    st.focus = Some(6);
    key(area, &items, &mut st, Key::Tab);
    let follows = st.focus == Some(7) && rect_of(area, &items, &st, 7).is_some();
    let list_ok = key(area, &items, &mut st, Key::Down) == Some(Event::Selected(7, 1));
    let inside = layout(area, &items, &st).placed.iter().all(|p| p.row.y >= area.y && p.row.y + p.row.h <= area.y + area.h);
    let scroll_ok = scrollable && moved && end && home && follows && list_ok && inside;

    // A disabled control takes no click.
    st.reset();
    let (dx, dy) = at(&st, &items, 8);
    let disabled_ok = dx != 0 && click(area, &items, &mut st, dx, dy).is_none() && st.focus.is_none();

    // Word wrap keeps every line within the width.
    let lines = wrap("Reads every file and verifies its checksum; the desktop pauses while it runs.", m.hint, 180);
    let wrap_ok = lines.len() > 1 && lines.iter().all(|l| text::width_px(l, m.hint) <= 180) && fit("a very long value indeed", m.label, 60).ends_with("...");

    let ok = hit_ok && tab_ok && check_ok && choice_ok && slider_ok && field_ok && scroll_ok && disabled_ok && wrap_ok;
    crate::serial_println!(
        "[uikit] EuroUI forms: hit-test={hit_ok}, tab-order={tab_ok}, checkbox={check_ok}, dropdown={choice_ok}, slider={slider_ok}, text-field={field_ok}, scroll={scroll_ok}, disabled-inert={disabled_ok}, wrap={wrap_ok} → {}",
        if ok { "OK (interactive toolkit: focus, input, events) ✓" } else { "FAILED ✗" }
    );
}
