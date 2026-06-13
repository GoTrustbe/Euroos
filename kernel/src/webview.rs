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
use euromedia::Image;
use euroweb::dom::{Dom, NodeKind};
use euroweb::DisplayItem;

const TITLEBAR_H: usize = 44;
const TABBAR_H: usize = 32;
const ADDR_H: usize = 38;
const MARGIN: usize = 22; // paginarand (gelijk in render + hit_test)

/// Een gedecodeerde (of mislukte) afbeelding in de paginabeeldcache.
enum CachedImg {
    Ok(Image),
    Bad,
}

/// Beeldcache per `src` (gevuld bij navigatie — NOOIT per frame in `render`,
/// zodat tekenen nooit blokkeert op een netwerk-fetch).
static IMG_CACHE: Mutex<Vec<(String, CachedImg)>> = Mutex::new(Vec::new());
/// Live formulier-veldwaarden per DOM-knoop (overschrijven het `value`-attr).
static FORM_STATE: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());
/// Welk paginaveld (DOM-knoop) heeft focus voor toetsinvoer.
static FOCUSED_FIELD: Mutex<Option<usize>> = Mutex::new(None);

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
    // Nieuwe pagina: oude formuliertoestand + focus wissen en de afbeeldingen
    // vooraf ophalen/decoderen (één keer, hier — niet per frame in `render`).
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

// ── Afbeeldingen: ophalen (data:/http) + decoderen (QOI/PPM), gecachet ───────

/// Parse, vind alle `<img src>` en vul de beeldcache. `data:`-URI's worden
/// inline gedecodeerd; http(s) wordt ECHT opgehaald via de TCP/TLS-stack.
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

/// Haal de bytes van één afbeelding op en decodeer ze (QOI eerst, dan PPM).
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
        // Relatief/absoluut http(s): los op tegen de pagina-URL en fetch.
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

/// Laad het actieve tabblad opnieuw (of voor het eerst) van zijn huidige URL.
pub fn load_active() {
    let url = BROWSER.lock().as_ref().and_then(|b| b.tabs.get(b.active).map(|t| t.url.clone()));
    if let Some(u) = url {
        navigate(&u);
    }
}

// ── Bewerk-acties (vanuit de desktop-loop) ──────────────────────────────────

pub fn begin_edit() {
    *FOCUSED_FIELD.lock() = None; // adresbalk neemt over van een paginaveld
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

/// De (na scriptuitvoering aangevulde) HTML van het actieve tabblad — voor zelftests.
pub fn active_html() -> String {
    BROWSER.lock().as_ref().map(|b| b.tabs[b.active].html.clone()).unwrap_or_default()
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
    /// Een tekst-invoerveld in de pagina (DOM-knoop) → focus voor typen.
    Field(usize),
    /// Een knop/verzendknop in de pagina (DOM-knoop) → formulier verzenden.
    Submit(usize),
    None,
}

/// Heeft een paginaveld focus (toetsen gaan dan naar het veld i.p.v. de adresbalk)?
pub fn field_focused() -> bool {
    FOCUSED_FIELD.lock().is_some()
}

/// Geef een paginaveld focus (en stop adresbalk-bewerking).
pub fn focus_field(node: usize) {
    *FOCUSED_FIELD.lock() = Some(node);
    if let Some(br) = BROWSER.lock().as_mut() {
        br.editing = false;
    }
    // Zorg dat er een live-waarde-slot bestaat (geseed uit het value-attr).
    let mut fs = FORM_STATE.lock();
    if !fs.iter().any(|(n, _)| *n == node) {
        let seed = active_field_value(node);
        fs.push((node, seed));
    }
}

/// Verwerk een toets in het gefocuste paginaveld. `true` = er veranderde iets.
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

/// De huidige (begin)waarde van een veld uit het `value`-attr van de actieve pagina.
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

/// Verzend het formulier dat knoop `btn_node` bevat: bouw de doel-URL en doe een
/// ECHTE HTTP-GET (via [`navigate`]).
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
        // ECHTE POST: urlencoded body via dezelfde EuroTLS/TCP-stack als GET.
        crate::serial_println!("[web] FORM POST \u{2192} {action_abs} ({} B body: {body})", body.len());
        let (tls, host, port, path) = parse_url(&action_abs);
        match crate::net::post_full(&host, port, &path, tls, "application/x-www-form-urlencoded", body.as_bytes()) {
            Some((code, _loc, resp)) => {
                let html = String::from_utf8_lossy(&resp).into_owned();
                load_inline(&action_abs, &html);
                crate::serial_println!("[web] POST → HTTP {code}, {} B antwoord", resp.len());
            }
            None => {
                // Geen externe server in deze omgeving: toon eerlijk het ECHT
                // opgebouwde POST-verzoek (methode + body) i.p.v. iets te faken.
                let echo = alloc::format!(
                    "<html><head><title>POST verzonden</title></head><body>\
                     <h1>POST-verzoek opgebouwd</h1>\
                     <p>Doel: {action_abs}</p>\
                     <p>Content-Type: application/x-www-form-urlencoded</p>\
                     <p>Body ({} bytes): {body}</p>\
                     <p>(Geen externe server bereikbaar in deze omgeving — dit is het \
                     verzoek dat de EuroWeb-engine verzond.)</p></body></html>",
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

/// Verzamel een formulier: (methode "get"/"post", absolute action-URL, naam/waarde-
/// paren van de velden). Gedeeld door GET (`submit_target`) en POST (`submit_form`).
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

/// Bouw — ZONDER te navigeren — de absolute GET-URL voor het formulier dat
/// `btn_node` bevat (action + query uit de velden). Voor de zelftest + GET-submit.
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

/// Bouw het POST-verzoek (absolute action-URL + urlencoded body) voor het formulier
/// dat `btn_node` bevat — voor de `[post]`-zelftest (geen netwerk vereist).
pub fn post_request(btn_node: usize) -> Option<(String, String, String)> {
    let (method, action_abs, pairs) = collect_form(btn_node)?;
    let body = pairs
        .iter()
        .map(|(k, v)| alloc::format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Some((method, action_abs, body))
}

/// Zet de actieve tab op kant-en-klare HTML (zónder netwerk) en preload de
/// afbeeldingen. Voor de AG-2 demo/zelftest-pagina.
pub fn load_inline(url: &str, html: &str) {
    FORM_STATE.lock().clear();
    *FOCUSED_FIELD.lock() = None;
    // Voer paginascripts ÉÉN keer uit bij het laden (niet bij elke render).
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

/// Voer de `<script>`-inhoud van een pagina uit via de EuroJS-engine (tree-walking,
/// geen JIT, stap-/diepte-begrensd). `document.write(...)`-uitvoer wordt als ECHTE
/// pagina-inhoud toegevoegd; `console.log` gaat naar het serieel logboek + een klein
/// JS-consolepaneel onderaan de pagina. Geeft de aangevulde HTML terug. Scripts
/// draaien hier eenmalig (bij laden) — niet opnieuw bij elke frame-render.
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
                logs.push(alloc::format!("[fout] {e}"));
            }
        }
    }
    if ran == 0 {
        return String::from(html);
    }
    crate::serial_println!("[js] EuroJS: {ran} script(s) uitgevoerd · {} console-regel(s) · {} document.write", logs.len(), writes.len());
    // Bouw een injectieblok: document.write-uitvoer als pagina-inhoud + JS-console.
    let mut inject = String::new();
    if !writes.is_empty() {
        inject.push_str("<div>");
        inject.push_str(&writes.join(""));
        inject.push_str("</div>");
    }
    if !logs.is_empty() {
        inject.push_str("<p>JS-console:</p>");
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

/// De AG-2 demopagina: een ECHT gegenereerde PPM-afbeelding (als data:-URI,
/// gedecodeerd door euromedia) + een zoekformulier (GET). Geen mock-pixels.
pub fn ag2_demo_html() -> String {
    let (w, h) = (24u32, 16u32);
    let mut ppm = alloc::format!("P3 {w} {h} 255");
    for y in 0..h {
        for x in 0..w {
            // EU-blauw veld met een raster gouden "sterren".
            let star = (x % 6 == 3) && (y % 5 == 2);
            let (r, g, b) = if star { (226u32, 163, 58) } else { (45, 107, 224) };
            ppm.push_str(&alloc::format!(" {r} {g} {b}"));
        }
    }
    alloc::format!(
        "<html><head><title>EuroWeb \u{2014} afbeeldingen en formulieren</title></head>\
         <body><h1>Afbeeldingen en formulieren</h1>\
         <p>Een PPM-afbeelding, gedecodeerd door de eigen euromedia-engine:</p>\
         <img src=\"data:image/x-portable-pixmap,{ppm}\" width=\"192\" height=\"128\">\
         <form action=\"/zoek\" method=\"get\">\
         <p>Zoek het soevereine web:</p>\
         <input type=\"text\" name=\"q\" value=\"soevereiniteit\">\
         <input type=\"submit\" value=\"Zoeken\"></form></body></html>"
    )
}

/// Eerste `<form>`-voorouder van `node` (of None).
fn enclosing_form(dom: &Dom, node: usize) -> Option<usize> {
    (0..dom.len()).find(|&f| dom.tag(f) == Some("form") && is_descendant(dom, f, node))
}

/// Is `node` een (transitieve) afstammeling van `anc` (of `anc` zelf)?
fn is_descendant(dom: &Dom, anc: usize, node: usize) -> bool {
    if anc == node {
        return true;
    }
    dom.nodes[anc].children.iter().any(|&c| is_descendant(dom, c, node))
}

/// Minimale URL-encoding voor query-waarden (RFC 3986 unreserved blijft, rest %HH).
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
    // Paginagebied: test tegen de gelayoute formulierbesturingen (her-layout).
    let py = ay + ADDR_H;
    if my >= py {
        if let Some(hit) = hit_page_control(win_x, win_y, win_w, mx, my) {
            return hit;
        }
    }
    Hit::None
}

/// Her-layout de actieve pagina en zoek of (mx,my) op een veld/knop valt.
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
                            // Placeholder voor een ontbrekende/kapotte afbeelding.
                            fb.fill_rect(dx, dy, dw, dh, Color::SURFACE_3);
                            fb.draw_border(dx, dy, dw, dh, 1, Color::BORDER);
                            text::draw_px(fb, dx + 8, dy + dh / 2 - 7, "\u{1F5BC} afbeelding", Color::TEXT_DIM, 12.0);
                        }
                    }
                }
            }
            DisplayItem::Field { x: fx, y: fy, w: fw, h: fh, node, value, .. } => {
                let dx = ox + *fx as usize;
                let dy = oy + *fy as usize;
                if dy + *fh as usize <= max_y {
                    // Live waarde uit FORM_STATE (géén BROWSER-lock: die houden we hier al vast).
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

/// Blit een afbeelding in een dest-vak (w×h) met nearest-neighbor-schaling.
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
