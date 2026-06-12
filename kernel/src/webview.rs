//! EuroWeb-browser: een BRUIKBARE browser met tabbladen, een bewerkbare adresbalk
//! en redirect-volgende navigatie over de echte TCP/TLS-stack ([`crate::net`]).
//! Pagina's worden ECHT opgehaald (HTTP/HTTPS) en gerenderd door de eigen engine
//! ([`euroweb`]). De toestand leeft in een globale [`Browser`]; de desktop-loop
//! muteert 'm (toetsen/muis) en `render` toont 'm.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use euroweb::dom::{Dom, NodeKind};
use euroweb::DisplayItem;

const TITLEBAR_H: usize = 44;
const TABBAR_H: usize = 32;
const ADDR_H: usize = 38;

/// Eén tabblad.
pub struct Tab {
    pub url: String,
    pub html: String,
    pub title: String,
    pub status: String,
}

/// De browser-toestand (tabbladen + adresbalk-bewerking).
pub struct Browser {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub editing: bool,
    pub edit_buf: String,
}

static BROWSER: Mutex<Option<Browser>> = Mutex::new(None);

/// Initialiseer de browser met één tabblad op `start_url` (nog niet geladen).
pub fn init(start_url: &str) {
    *BROWSER.lock() = Some(Browser {
        tabs: alloc::vec![Tab {
            url: start_url.to_string(),
            html: String::new(),
            title: String::from("Nieuw tabblad"),
            status: String::from("nog niet geladen"),
        }],
        active: 0,
        editing: false,
        edit_buf: String::new(),
    });
}

// ── URL-parsing & navigatie ──────────────────────────────────────────────────

fn parse_url(url: &str) -> (bool, String, u16, String) {
    let url = url.trim();
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        (false, url)
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let default_port = if tls { 443 } else { 80 };
    let (host, port) = match hostport.find(':') {
        Some(i) => (hostport[..i].to_string(), hostport[i + 1..].parse().unwrap_or(default_port)),
        None => (hostport.to_string(), default_port),
    };
    let path = if path.is_empty() { "/".to_string() } else { path.to_string() };
    (tls, host, port, path)
}

fn resolve_redirect(base_host: &str, base_tls: bool, loc: &str) -> String {
    if loc.starts_with("http://") || loc.starts_with("https://") {
        loc.to_string()
    } else if let Some(p) = loc.strip_prefix('/') {
        let scheme = if base_tls { "https" } else { "http" };
        alloc::format!("{scheme}://{base_host}/{p}")
    } else {
        let scheme = if base_tls { "https" } else { "http" };
        alloc::format!("{scheme}://{base_host}/{loc}")
    }
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let s = lower.find("<title>")? + 7;
    let e = lower[s..].find("</title>")? + s;
    let t = html[s..e].trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Navigeer het actieve tabblad naar `input` (volgt redirects, HTTP→HTTPS).
/// Doet de (blokkerende) netwerk-fetch ZONDER de browser-lock vast te houden.
pub fn navigate(input: &str) {
    let mut cur = input.trim().to_string();
    if cur.is_empty() {
        return;
    }
    let mut final_html = String::new();
    let mut final_url = cur.clone();
    let mut final_status = String::from("fout");
    for _ in 0..6 {
        let (tls, host, port, path) = parse_url(&cur);
        crate::serial_println!("[web] GET {}://{host}:{port}{path}", if tls { "https" } else { "http" });
        match crate::net::fetch_full(&host, port, &path, tls) {
            Some((status, location, body)) => {
                if (300..400).contains(&status) {
                    if let Some(loc) = location {
                        cur = resolve_redirect(&host, tls, &loc);
                        continue;
                    }
                }
                final_html = String::from_utf8_lossy(&body).into_owned();
                final_url = cur.clone();
                final_status = alloc::format!("{} \u{00B7} {} bytes", status, body.len());
                break;
            }
            None => {
                final_html = alloc::format!(
                    "<body><h1>Kon niet laden</h1><p>{host}: geen verbinding of TLS-handshake mislukt.</p></body>"
                );
                final_url = cur.clone();
                final_status = String::from("mislukt");
                break;
            }
        }
    }
    // Redirect-lus uitgeput (6 hops, allemaal omleidingen): toon dit expliciet
    // i.p.v. een lege pagina — graceful failure, geen verwarrende blanco.
    if final_html.is_empty() {
        final_html = alloc::format!(
            "<body><h1>Te veel omleidingen</h1><p>{cur}: meer dan 6 redirects gevolgd, gestopt.</p></body>"
        );
        final_status = String::from("omleidingslus");
    }
    let title = extract_title(&final_html).unwrap_or_else(|| final_url.clone());
    if let Some(br) = BROWSER.lock().as_mut() {
        let a = br.active;
        br.tabs[a].url = final_url;
        br.tabs[a].html = final_html;
        br.tabs[a].title = title;
        br.tabs[a].status = final_status;
        br.editing = false;
    }
}

/// Laad het actieve tabblad opnieuw (of voor het eerst) van zijn huidige URL.
pub fn load_active() {
    let url = BROWSER.lock().as_ref().and_then(|b| b.tabs.get(b.active).map(|t| t.url.clone()));
    if let Some(u) = url {
        navigate(&u);
    }
}

// ── Bewerk-acties (vanuit de desktop-loop) ──────────────────────────────────

pub fn begin_edit() {
    if let Some(br) = BROWSER.lock().as_mut() {
        br.editing = true;
        br.edit_buf = String::new(); // leeg starten zodat typen meteen een nieuw adres bouwt
    }
}

/// Verwerk een toets in de adresbalk. Retourneert Some(url) als Enter → navigeren.
pub fn edit_key(ch: char) -> Option<String> {
    let mut b = BROWSER.lock();
    let br = b.as_mut()?;
    if !br.editing {
        return None;
    }
    match ch {
        '\r' => {
            let u = br.edit_buf.clone();
            br.editing = false;
            return Some(u);
        }
        '\u{1b}' => br.editing = false, // Esc
        '\u{8}' | '\u{7f}' => {
            br.edit_buf.pop();
        }
        c if !c.is_control() => br.edit_buf.push(c),
        _ => {}
    }
    None
}

pub fn editing() -> bool {
    BROWSER.lock().as_ref().map(|b| b.editing).unwrap_or(false)
}

pub fn new_tab() {
    if let Some(br) = BROWSER.lock().as_mut() {
        br.tabs.push(Tab {
            url: String::from("flowd.be"),
            html: String::new(),
            title: String::from("Nieuw tabblad"),
            status: String::from("nog niet geladen"),
        });
        br.active = br.tabs.len() - 1;
        br.editing = true;
        br.edit_buf = String::from("flowd.be");
    }
}

pub fn switch_tab(i: usize) {
    if let Some(br) = BROWSER.lock().as_mut() {
        if i < br.tabs.len() {
            br.active = i;
            br.editing = false;
        }
    }
}

// ── Klik-trefzones ──────────────────────────────────────────────────────────

/// Waar werd geklikt in het browservenster?
pub enum Hit {
    Tab(usize),
    NewTab,
    UrlBar,
    None,
}

pub fn hit_test(win_x: usize, win_y: usize, win_w: usize, mx: usize, my: usize) -> Hit {
    let bx = win_x;
    let by = win_y + TITLEBAR_H;
    // Tabstrip.
    if my >= by && my < by + TABBAR_H {
        let n = BROWSER.lock().as_ref().map(|b| b.tabs.len()).unwrap_or(0);
        let tab_w = 150usize;
        for i in 0..n {
            let tx = bx + 8 + i * (tab_w + 4);
            if mx >= tx && mx < tx + tab_w {
                return Hit::Tab(i);
            }
        }
        let plus_x = bx + 8 + n * (tab_w + 4);
        if mx >= plus_x && mx < plus_x + 30 {
            return Hit::NewTab;
        }
        return Hit::None;
    }
    // Adresbalk.
    let ay = by + TABBAR_H;
    if my >= ay && my < ay + ADDR_H {
        if mx > bx + 90 && mx < win_x + win_w - 90 {
            return Hit::UrlBar;
        }
    }
    Hit::None
}

// ── Render ───────────────────────────────────────────────────────────────────

fn extract_styles(dom: &Dom) -> String {
    let mut css = String::new();
    for i in 0..dom.len() {
        if let NodeKind::Element { name, .. } = &dom.nodes[i].kind {
            if name == "style" {
                css.push_str(&dom.text_content(i));
                css.push('\n');
            }
        }
    }
    css
}

pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize) {
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let w = win_w;
    let h = win_h.saturating_sub(TITLEBAR_H);

    let b = BROWSER.lock();
    let br = match b.as_ref() {
        Some(br) => br,
        None => return,
    };

    // ── Tabstrip ──
    fb.fill_rect(x, y, w, TABBAR_H, Color::SURFACE_3);
    let tab_w = 150usize;
    for (i, t) in br.tabs.iter().enumerate() {
        let tx = x + 8 + i * (tab_w + 4);
        let active = i == br.active;
        let bg = if active { Color::SURFACE } else { Color::CARD };
        fb.fill_rounded_rect(tx, y + 5, tab_w, TABBAR_H - 5, 8, bg);
        let label = clip(&t.title, 18);
        text::draw_px(fb, tx + 12, y + 11, &label, if active { Color::INK } else { Color::TEXT_SEC }, 12.5);
    }
    let plus_x = x + 8 + br.tabs.len() * (tab_w + 4);
    fb.fill_rounded_rect(plus_x, y + 5, 30, TABBAR_H - 5, 8, Color::CARD);
    text::draw_px(fb, plus_x + 9, y + 9, "+", Color::TEXT_SEC, 18.0);

    // ── Adresbalk ──
    let ay = y + TABBAR_H;
    fb.fill_rect(x, ay, w, ADDR_H, Color::CARD);
    fb.fill_rect(x, ay + ADDR_H - 1, w, 1, Color::BORDER);
    text::draw_px(fb, x + 14, ay + 11, "\u{2190}  \u{2192}  \u{21BB}", Color::TEXT_DIM, 15.0);
    let pill_x = x + 70;
    let pill_w = w.saturating_sub(70 + 96);
    let editing = br.editing;
    fb.fill_rounded_rect(pill_x, ay + 7, pill_w, ADDR_H - 14, 11, Color::SURFACE);
    fb.draw_border(pill_x, ay + 7, pill_w, ADDR_H - 14, if editing { 2 } else { 1 }, if editing { Color::ACCENT } else { Color::BORDER });
    let shown = if editing { &br.edit_buf } else { &br.tabs[br.active].url };
    let mut line = shown.clone();
    if editing {
        line.push('|'); // cursor
    }
    text::draw_px(fb, pill_x + 14, ay + 11, &line, Color::INK, 13.5);
    // Status-badge.
    let st = &br.tabs[br.active].status;
    let badge = clip(st, 12);
    let bw = text::width_px(&badge, 11.0) + 16;
    let bx2 = x + w - bw - 12;
    fb.fill_rounded_rect(bx2, ay + 8, bw, ADDR_H - 16, 10, Color::SUCCESS_SOFT);
    text::draw_px(fb, bx2 + 8, ay + 12, &badge, Color::SUCCESS, 11.0);

    // ── Pagina ──
    let py = ay + ADDR_H;
    let ph = h.saturating_sub(TABBAR_H + ADDR_H);
    fb.fill_rect(x, py, w, ph, Color::SURFACE);
    let html_full = &br.tabs[br.active].html;
    if html_full.trim().is_empty() {
        text::draw_px(fb, x + 24, py + 24, "Typ een adres en druk Enter om te laden.", Color::TEXT_SEC, 15.0);
        return;
    }
    // GUARD: kap heel grote pagina's af zodat de engine de kernel-heap niet
    // uitput (een browser mag het OS NOOIT laten crashen). Met de 96 MiB-heap
    // past ~150 KB ruim; grotere sites renderen we gedeeltelijk.
    const MAX_HTML: usize = 150_000;
    let html: &str = if html_full.len() > MAX_HTML {
        let mut e = MAX_HTML;
        while e > 0 && !html_full.is_char_boundary(e) {
            e -= 1;
        }
        &html_full[..e]
    } else {
        html_full
    };

    let margin = 22usize;
    let content_w = w.saturating_sub(margin * 2);
    let dom = euroweb::parse(html);
    let css = extract_styles(&dom);
    let ss = euroweb::parse_stylesheet(&css);
    let styles = euroweb::compute(&dom, &[&ss]);
    let lb = euroweb::layout(&dom, &styles, content_w as f32);
    let items: Vec<DisplayItem> = euroweb::paint(&dom, &styles, &lb);

    let ox = x + margin;
    let oy = py + margin;
    let max_y = y + h;
    for item in &items {
        match item {
            DisplayItem::Rect { x: rx, y: ry, w: rw, h: rh, color } => {
                let dx = ox + *rx as usize;
                let dy = oy + *ry as usize;
                if dy < max_y {
                    fb.fill_rect(dx, dy, *rw as usize, (*rh as usize).min(max_y - dy), col(*color));
                }
            }
            DisplayItem::Text { x: tx, y: ty, text: s, color, size } => {
                let dx = ox + *tx as usize;
                let dy = oy + *ty as usize;
                if dy + *size as usize <= max_y {
                    text::draw_px(fb, dx, dy, s, col(*color), *size);
                }
            }
        }
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    }
}

fn col(c: u32) -> Color {
    Color::rgb(((c >> 16) & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, (c & 0xFF) as u8)
}
