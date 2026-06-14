//! EuroWeb browser: a USABLE browser with tabs, an editable address bar
//! and redirect-following navigation over the real TCP/TLS stack ([`crate::net`]).
//! Pages are REALLY fetched (HTTP/HTTPS) and rendered by the own engine
//! ([`euroweb`]). The state lives in a global [`Browser`]; the desktop loop
//! mutates it (keys/mouse) and `render` shows it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use euromedia::Image;
use euroweb::dom::{Dom, NodeKind};
use euroweb::DisplayItem;

const TITLEBAR_H: usize = 44;
const TABBAR_H: usize = 32;
const ADDR_H: usize = 38;
const MARGIN: usize = 22; // page margin (equal in render + hit_test)

/// A decoded (or failed) image in the page image cache.
enum CachedImg {
    Ok(Image),
    Bad,
}

/// Image cache per `src` (filled on navigation — NEVER per frame in `render`,
/// so that drawing never blocks on a network fetch).
static IMG_CACHE: Mutex<Vec<(String, CachedImg)>> = Mutex::new(Vec::new());
/// Live form field values per DOM node (override the `value` attr).
static FORM_STATE: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());
/// Which page field (DOM node) has focus for keyboard input.
static FOCUSED_FIELD: Mutex<Option<usize>> = Mutex::new(None);

/// A single tab.
pub struct Tab {
    pub url: String,
    pub html: String,
    pub title: String,
    pub status: String,
}

/// The browser state (tabs + address bar editing).
pub struct Browser {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub editing: bool,
    pub edit_buf: String,
}

static BROWSER: Mutex<Option<Browser>> = Mutex::new(None);

/// Initialize the browser with one tab on `start_url` (not loaded yet).
pub fn init(start_url: &str) {
    *BROWSER.lock() = Some(Browser {
        tabs: alloc::vec![Tab {
            url: start_url.to_string(),
            html: String::new(),
            title: String::from("New tab"),
            status: String::from("not loaded yet"),
        }],
        active: 0,
        editing: false,
        edit_buf: String::new(),
    });
}

// ── URL parsing & navigation ──────────────────────────────────────────────────

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

/// Navigate the active tab to `input` (follows redirects, HTTP→HTTPS).
/// Performs the (blocking) network fetch WITHOUT holding the browser lock.
pub fn navigate(input: &str) {
    let mut cur = input.trim().to_string();
    if cur.is_empty() {
        return;
    }
    let mut final_html = String::new();
    let mut final_url = cur.clone();
    let mut final_status = String::from("error");
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
                    "<body><h1>Could not load</h1><p>{host}: no connection or TLS handshake failed.</p></body>"
                );
                final_url = cur.clone();
                final_status = String::from("failed");
                break;
            }
        }
    }
    // Redirect loop exhausted (6 hops, all redirects): show this explicitly
    // instead of a blank page — graceful failure, no confusing blank.
    if final_html.is_empty() {
        final_html = alloc::format!(
            "<body><h1>Too many redirects</h1><p>{cur}: more than 6 redirects followed, stopped.</p></body>"
        );
        final_status = String::from("redirect loop");
    }
    // New page: clear old form state + focus and prefetch/decode the images
    // (once, here — not per frame in `render`).
    FORM_STATE.lock().clear();
    *FOCUSED_FIELD.lock() = None;
    preload_images(&final_html, &final_url);

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

// ── Images: fetch (data:/http) + decode (QOI/PPM), cached ───────

/// Parse, find all `<img src>` and fill the image cache. `data:` URIs are
/// decoded inline; http(s) is REALLY fetched via the TCP/TLS stack.
fn preload_images(html: &str, base_url: &str) {
    let mut cache = IMG_CACHE.lock();
    cache.clear();
    let dom = euroweb::parse(html);
    for i in 0..dom.len() {
        if dom.tag(i) == Some("img") {
            if let Some(src) = dom.attr(i, "src") {
                if cache.iter().any(|(s, _)| s == src) {
                    continue;
                }
                let entry = match resolve_image(src, base_url) {
                    Some(img) => CachedImg::Ok(img),
                    None => CachedImg::Bad,
                };
                cache.push((String::from(src), entry));
            }
        }
    }
}

/// Fetch the bytes of one image and decode them (QOI first, then PPM).
fn resolve_image(src: &str, base_url: &str) -> Option<Image> {
    let bytes: Vec<u8> = if let Some(rest) = src.strip_prefix("data:") {
        // data:[<mime>][;base64],<data>
        let comma = rest.find(',')?;
        let meta = &rest[..comma];
        let data = &rest[comma + 1..];
        if meta.contains("base64") {
            euromail::base64_decode(data)
        } else {
            data.as_bytes().to_vec()
        }
    } else {
        // Relative/absolute http(s): resolve against the page URL and fetch.
        let abs = if src.starts_with("http://") || src.starts_with("https://") {
            String::from(src)
        } else {
            let (tls, host, _port, _path) = parse_url(base_url);
            resolve_redirect(&host, tls, src)
        };
        let (tls, host, port, path) = parse_url(&abs);
        crate::serial_println!("[web] IMG {host}:{port}{path}");
        let (_status, _loc, body) = crate::net::fetch_full(&host, port, &path, tls)?;
        body
    };
    euromedia::decode(&bytes).ok().or_else(|| euromedia::decode_ppm(&bytes).ok())
}

/// Reload the active tab (or for the first time) from its current URL.
pub fn load_active() {
    let url = BROWSER.lock().as_ref().and_then(|b| b.tabs.get(b.active).map(|t| t.url.clone()));
    if let Some(u) = url {
        navigate(&u);
    }
}

// ── Edit actions (from the desktop loop) ──────────────────────────────────

pub fn begin_edit() {
    *FOCUSED_FIELD.lock() = None; // address bar takes over from a page field
    if let Some(br) = BROWSER.lock().as_mut() {
        br.editing = true;
        br.edit_buf = String::new(); // start empty so typing immediately builds a new address
    }
}

/// Handle a key in the address bar. Returns Some(url) on Enter → navigate.
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

/// The (after script execution amended) HTML of the active tab — for self-tests.
pub fn active_html() -> String {
    BROWSER.lock().as_ref().map(|b| b.tabs[b.active].html.clone()).unwrap_or_default()
}

pub fn new_tab() {
    if let Some(br) = BROWSER.lock().as_mut() {
        br.tabs.push(Tab {
            url: String::from("flowd.be"),
            html: String::new(),
            title: String::from("New tab"),
            status: String::from("not loaded yet"),
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

// ── Click hit zones ──────────────────────────────────────────────────────────

/// Where was the click in the browser window?
pub enum Hit {
    Tab(usize),
    NewTab,
    UrlBar,
    /// A text input field in the page (DOM node) → focus for typing.
    Field(usize),
    /// A button/submit button in the page (DOM node) → submit form.
    Submit(usize),
    None,
}

/// Does a page field have focus (keys then go to the field instead of the address bar)?
pub fn field_focused() -> bool {
    FOCUSED_FIELD.lock().is_some()
}

/// Give a page field focus (and stop address bar editing).
pub fn focus_field(node: usize) {
    *FOCUSED_FIELD.lock() = Some(node);
    if let Some(br) = BROWSER.lock().as_mut() {
        br.editing = false;
    }
    // Make sure a live-value slot exists (seeded from the value attr).
    let mut fs = FORM_STATE.lock();
    if !fs.iter().any(|(n, _)| *n == node) {
        let seed = active_field_value(node);
        fs.push((node, seed));
    }
}

/// Handle a key in the focused page field. `true` = something changed.
pub fn field_key(ch: char) -> bool {
    let node = match *FOCUSED_FIELD.lock() {
        Some(n) => n,
        None => return false,
    };
    let mut fs = FORM_STATE.lock();
    let slot = match fs.iter_mut().find(|(n, _)| *n == node) {
        Some(s) => &mut s.1,
        None => {
            fs.push((node, String::new()));
            &mut fs.last_mut().unwrap().1
        }
    };
    match ch {
        '\r' => {
            drop(fs);
            *FOCUSED_FIELD.lock() = None;
        }
        '\u{1b}' => {
            drop(fs);
            *FOCUSED_FIELD.lock() = None;
        }
        '\u{8}' | '\u{7f}' => {
            slot.pop();
        }
        c if !c.is_control() => slot.push(c),
        _ => {}
    }
    true
}

/// The current (initial) value of a field from the `value` attr of the active page.
fn active_field_value(node: usize) -> String {
    let b = BROWSER.lock();
    if let Some(br) = b.as_ref() {
        let dom = euroweb::parse(&br.tabs[br.active].html);
        if node < dom.len() {
            return dom.attr(node, "value").map(String::from).unwrap_or_default();
        }
    }
    String::new()
}

/// Submit the form that contains node `btn_node`: build the target URL and do a
/// REAL HTTP GET (via [`navigate`]).
pub fn submit_form(btn_node: usize) {
    let (method, action_abs, pairs) = match collect_form(btn_node) {
        Some(f) => f,
        None => return,
    };
    let body = pairs
        .iter()
        .map(|(k, v)| alloc::format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    if method == "post" {
        // REAL POST: urlencoded body via the same EuroTLS/TCP stack as GET.
        crate::serial_println!("[web] FORM POST \u{2192} {action_abs} ({} B body: {body})", body.len());
        let (tls, host, port, path) = parse_url(&action_abs);
        match crate::net::post_full(&host, port, &path, tls, "application/x-www-form-urlencoded", body.as_bytes()) {
            Some((code, _loc, resp)) => {
                let html = String::from_utf8_lossy(&resp).into_owned();
                load_inline(&action_abs, &html);
                crate::serial_println!("[web] POST → HTTP {code}, {} B response", resp.len());
            }
            None => {
                // No external server in this environment: honestly show the
                // REALLY built POST request (method + body) instead of faking anything.
                let echo = alloc::format!(
                    "<html><head><title>POST sent</title></head><body>\
                     <h1>POST request built</h1>\
                     <p>Target: {action_abs}</p>\
                     <p>Content-Type: application/x-www-form-urlencoded</p>\
                     <p>Body ({} bytes): {body}</p>\
                     <p>(No external server reachable in this environment — this is the \
                     request that the EuroWeb engine sent.)</p></body></html>",
                    body.len()
                );
                load_inline(&action_abs, &echo);
            }
        }
    } else if let Some(target) = submit_target(btn_node) {
        crate::serial_println!("[web] FORM GET \u{2192} {target}");
        navigate(&target);
    }
}

/// Collect a form: (method "get"/"post", absolute action URL, name/value
/// pairs of the fields). Shared by GET (`submit_target`) and POST (`submit_form`).
fn collect_form(btn_node: usize) -> Option<(String, String, Vec<(String, String)>)> {
    let (method, action, base_url, pairs) = {
        let b = BROWSER.lock();
        let br = b.as_ref()?;
        let base_url = br.tabs[br.active].url.clone();
        let dom = euroweb::parse(&br.tabs[br.active].html);
        let form = enclosing_form(&dom, btn_node);
        let method = form
            .and_then(|f| dom.attr(f, "method").map(|m| m.to_ascii_lowercase()))
            .filter(|m| m == "post")
            .unwrap_or_else(|| String::from("get"));
        let action = form
            .and_then(|f| dom.attr(f, "action").map(String::from))
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| {
                let (_t, _h, _p, path) = parse_url(&base_url);
                path
            });
        let scope = form.unwrap_or(0);
        let mut pairs: Vec<(String, String)> = Vec::new();
        for i in 0..dom.len() {
            if !is_descendant(&dom, scope, i) && form.is_some() {
                continue;
            }
            if dom.tag(i) == Some("input") {
                let ty = dom.attr(i, "type").unwrap_or("text");
                if ty == "submit" || ty == "button" {
                    continue;
                }
                if let Some(name) = dom.attr(i, "name") {
                    let val = FORM_STATE
                        .lock()
                        .iter()
                        .find(|(n, _)| *n == i)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_else(|| dom.attr(i, "value").map(String::from).unwrap_or_default());
                    pairs.push((String::from(name), val));
                }
            }
        }
        (method, action, base_url, pairs)
    };
    let (tls, host, _port, _path) = parse_url(&base_url);
    let action_abs = if action.starts_with("http://") || action.starts_with("https://") {
        action
    } else {
        resolve_redirect(&host, tls, &action)
    };
    Some((method, action_abs, pairs))
}

/// Build — WITHOUT navigating — the absolute GET URL for the form that
/// `btn_node` contains (action + query from the fields). For the self-test + GET submit.
pub fn submit_target(btn_node: usize) -> Option<String> {
    let (_method, action_abs, pairs) = collect_form(btn_node)?;
    let query = pairs
        .iter()
        .map(|(k, v)| alloc::format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let target = if query.is_empty() {
        action_abs
    } else if action_abs.contains('?') {
        alloc::format!("{action_abs}&{query}")
    } else {
        alloc::format!("{action_abs}?{query}")
    };
    Some(target)
}

/// Build the POST request (absolute action URL + urlencoded body) for the form
/// that `btn_node` contains — for the `[post]` self-test (no network required).
pub fn post_request(btn_node: usize) -> Option<(String, String, String)> {
    let (method, action_abs, pairs) = collect_form(btn_node)?;
    let body = pairs
        .iter()
        .map(|(k, v)| alloc::format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Some((method, action_abs, body))
}

/// Set the active tab to ready-made HTML (without network) and preload the
/// images. For the AG-2 demo/self-test page.
pub fn load_inline(url: &str, html: &str) {
    FORM_STATE.lock().clear();
    *FOCUSED_FIELD.lock() = None;
    // Run page scripts ONCE on load (not on every render).
    let html = run_page_scripts(html);
    preload_images(&html, url);
    if let Some(br) = BROWSER.lock().as_mut() {
        let a = br.active;
        br.tabs[a].url = String::from(url);
        br.tabs[a].title = extract_title(&html).unwrap_or_else(|| String::from("EuroWeb"));
        br.tabs[a].html = html;
        br.tabs[a].status = String::from("inline");
        br.editing = false;
    }
}

/// Run the `<script>` content of a page via the EuroJS engine (tree-walking,
/// no JIT, step-/depth-bounded). `document.write(...)` output is added as REAL
/// page content; `console.log` goes to the serial log + a small
/// JS console panel at the bottom of the page. Returns the amended HTML. Scripts
/// run here once (on load) — not again on every frame render.
fn run_page_scripts(html: &str) -> String {
    let dom = euroweb::parse(html);
    let mut logs: Vec<String> = Vec::new();
    let mut writes: Vec<String> = Vec::new();
    let mut ran = 0;
    for i in 0..dom.len() {
        if dom.tag(i) == Some("script") {
            let src = dom.text_content(i);
            if src.trim().is_empty() {
                continue;
            }
            ran += 1;
            let (res, l, w) = eurojs::run_page(&src);
            logs.extend(l);
            writes.extend(w);
            if let Err(e) = res {
                logs.push(alloc::format!("[error] {e}"));
            }
        }
    }
    if ran == 0 {
        return String::from(html);
    }
    crate::serial_println!("[js] EuroJS: {ran} script(s) executed · {} console line(s) · {} document.write", logs.len(), writes.len());
    // Build an injection block: document.write output as page content + JS console.
    let mut inject = String::new();
    if !writes.is_empty() {
        inject.push_str("<div>");
        inject.push_str(&writes.join(""));
        inject.push_str("</div>");
    }
    if !logs.is_empty() {
        inject.push_str("<p>JS console:</p>");
        for l in &logs {
            inject.push_str(&alloc::format!("<p>&gt; {l}</p>"));
        }
    }
    if inject.is_empty() {
        return String::from(html);
    }
    match html.rfind("</body>") {
        Some(pos) => alloc::format!("{}{}{}", &html[..pos], inject, &html[pos..]),
        None => alloc::format!("{html}{inject}"),
    }
}

/// The AG-2 demo page: a REALLY generated PPM image (as data: URI,
/// decoded by euromedia) + a search form (GET). No mock pixels.
pub fn ag2_demo_html() -> String {
    let (w, h) = (24u32, 16u32);
    let mut ppm = alloc::format!("P3 {w} {h} 255");
    for y in 0..h {
        for x in 0..w {
            // EU blue field with a grid of golden "stars".
            let star = (x % 6 == 3) && (y % 5 == 2);
            let (r, g, b) = if star { (226u32, 163, 58) } else { (45, 107, 224) };
            ppm.push_str(&alloc::format!(" {r} {g} {b}"));
        }
    }
    alloc::format!(
        "<html><head><title>EuroWeb \u{2014} images and forms</title></head>\
         <body><h1>Images and forms</h1>\
         <p>A PPM image, decoded by the own euromedia engine:</p>\
         <img src=\"data:image/x-portable-pixmap,{ppm}\" width=\"192\" height=\"128\">\
         <form action=\"/zoek\" method=\"get\">\
         <p>Search the sovereign web:</p>\
         <input type=\"text\" name=\"q\" value=\"sovereignty\">\
         <input type=\"submit\" value=\"Search\"></form></body></html>"
    )
}

/// First `<form>` ancestor of `node` (or None).
fn enclosing_form(dom: &Dom, node: usize) -> Option<usize> {
    (0..dom.len()).find(|&f| dom.tag(f) == Some("form") && is_descendant(dom, f, node))
}

/// Is `node` a (transitive) descendant of `anc` (or `anc` itself)?
fn is_descendant(dom: &Dom, anc: usize, node: usize) -> bool {
    if anc == node {
        return true;
    }
    dom.nodes[anc].children.iter().any(|&c| is_descendant(dom, c, node))
}

/// Minimal URL encoding for query values (RFC 3986 unreserved stays, rest %HH).
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&alloc::format!("%{:02X}", b)),
        }
    }
    out
}

pub fn hit_test(win_x: usize, win_y: usize, win_w: usize, mx: usize, my: usize) -> Hit {
    let bx = win_x;
    let by = win_y + TITLEBAR_H;
    // Tab strip.
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
    // Address bar.
    let ay = by + TABBAR_H;
    if my >= ay && my < ay + ADDR_H {
        if mx > bx + 90 && mx < win_x + win_w - 90 {
            return Hit::UrlBar;
        }
    }
    // Page area: test against the laid-out form controls (re-layout).
    let py = ay + ADDR_H;
    if my >= py {
        if let Some(hit) = hit_page_control(win_x, win_y, win_w, mx, my) {
            return hit;
        }
    }
    Hit::None
}

/// Re-layout the active page and find whether (mx,my) lands on a field/button.
fn hit_page_control(win_x: usize, win_y: usize, win_w: usize, mx: usize, my: usize) -> Option<Hit> {
    let b = BROWSER.lock();
    let br = b.as_ref()?;
    let html = br.tabs.get(br.active)?.html.clone();
    drop(b);
    if html.trim().is_empty() {
        return None;
    }
    let content_w = win_w.saturating_sub(MARGIN * 2);
    let dom = euroweb::parse(&html);
    let css = extract_styles(&dom);
    let ss = euroweb::parse_stylesheet(&css);
    let styles = euroweb::compute(&dom, &[&ss]);
    let lb = euroweb::layout(&dom, &styles, content_w as f32);
    let items = euroweb::paint(&dom, &styles, &lb);
    let ox = win_x + MARGIN;
    let oy = win_y + TITLEBAR_H + TABBAR_H + ADDR_H + MARGIN;
    for item in &items {
        let (ix, iy, iw, ih, hit) = match item {
            DisplayItem::Field { x, y, w, h, node, .. } => (*x, *y, *w, *h, Hit::Field(*node)),
            DisplayItem::Button { x, y, w, h, node, .. } => (*x, *y, *w, *h, Hit::Submit(*node)),
            _ => continue,
        };
        let dx = ox + ix as usize;
        let dy = oy + iy as usize;
        if mx >= dx && mx < dx + iw as usize && my >= dy && my < dy + ih as usize {
            return Some(hit);
        }
    }
    None
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

    // ── Tab strip ──
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

    // ── Address bar ──
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
    // Status badge.
    let st = &br.tabs[br.active].status;
    let badge = clip(st, 12);
    let bw = text::width_px(&badge, 11.0) + 16;
    let bx2 = x + w - bw - 12;
    fb.fill_rounded_rect(bx2, ay + 8, bw, ADDR_H - 16, 10, Color::SUCCESS_SOFT);
    text::draw_px(fb, bx2 + 8, ay + 12, &badge, Color::SUCCESS, 11.0);

    // ── Page ──
    let py = ay + ADDR_H;
    let ph = h.saturating_sub(TABBAR_H + ADDR_H);
    fb.fill_rect(x, py, w, ph, Color::SURFACE);
    let html_full = &br.tabs[br.active].html;
    if html_full.trim().is_empty() {
        text::draw_px(fb, x + 24, py + 24, "Type an address and press Enter to load.", Color::TEXT_SEC, 15.0);
        return;
    }
    // GUARD: truncate very large pages so the engine does not exhaust the
    // kernel heap (a browser must NEVER let the OS crash). With the 96 MiB heap
    // ~150 KB fits comfortably; larger sites we render partially.
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
    let focused = *FOCUSED_FIELD.lock();
    let cache = IMG_CACHE.lock();
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
            DisplayItem::Image { x: ix, y: iy, w: iw, h: ih, src } => {
                let dx = ox + *ix as usize;
                let dy = oy + *iy as usize;
                let dw = *iw as usize;
                let dh = (*ih as usize).min(max_y.saturating_sub(dy));
                if dy < max_y && dh > 0 {
                    let img = cache.iter().find(|(s, _)| s == src).map(|(_, e)| e);
                    match img {
                        Some(CachedImg::Ok(image)) => blit_image(fb, dx, dy, dw, dh, image),
                        _ => {
                            // Placeholder for a missing/broken image.
                            fb.fill_rect(dx, dy, dw, dh, Color::SURFACE_3);
                            fb.draw_border(dx, dy, dw, dh, 1, Color::BORDER);
                            text::draw_px(fb, dx + 8, dy + dh / 2 - 7, "\u{1F5BC} image", Color::TEXT_DIM, 12.0);
                        }
                    }
                }
            }
            DisplayItem::Field { x: fx, y: fy, w: fw, h: fh, node, value, .. } => {
                let dx = ox + *fx as usize;
                let dy = oy + *fy as usize;
                if dy + *fh as usize <= max_y {
                    // Live value from FORM_STATE (no BROWSER lock: we already hold that here).
                    let live = FORM_STATE.lock().iter().find(|(n, _)| n == node).map(|(_, v)| v.clone());
                    let shown = live.unwrap_or_else(|| value.clone());
                    let foc = focused == Some(*node);
                    fb.fill_rounded_rect(dx, dy, *fw as usize, *fh as usize, 7, Color::SURFACE);
                    fb.draw_border(dx, dy, *fw as usize, *fh as usize, if foc { 2 } else { 1 }, if foc { Color::ACCENT } else { Color::BORDER });
                    let mut t = clip(&shown, (*fw as usize / 8).max(1));
                    if foc {
                        t.push('|');
                    }
                    text::draw_px(fb, dx + 9, dy + (*fh as usize).saturating_sub(18) / 2 + 2, &t, Color::INK, 13.5);
                }
            }
            DisplayItem::Button { x: bx, y: by_, w: bw, h: bh, label, .. } => {
                let dx = ox + *bx as usize;
                let dy = oy + *by_ as usize;
                if dy + *bh as usize <= max_y {
                    fb.fill_rounded_rect(dx, dy, *bw as usize, *bh as usize, 8, Color::ACCENT);
                    let lw = text::width_px(label, 13.5);
                    let lx = dx + (*bw as usize).saturating_sub(lw) / 2;
                    text::draw_px(fb, lx, dy + (*bh as usize).saturating_sub(18) / 2 + 2, label, Color::SURFACE, 13.5);
                }
            }
        }
    }
}

/// Blit an image into a dest box (w×h) with nearest-neighbor scaling.
fn blit_image(fb: &FrameBuffer, dx: usize, dy: usize, dw: usize, dh: usize, img: &Image) {
    if img.width == 0 || img.height == 0 {
        return;
    }
    for ry in 0..dh {
        let sy = ry * img.height as usize / dh;
        for rx in 0..dw {
            let sx = rx * img.width as usize / dw;
            if let Some(p) = img.get(sx as u32, sy as u32) {
                if p[3] >= 8 {
                    fb.put_pixel(dx + rx, dy + ry, Color::rgb(p[0], p[1], p[2]));
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
