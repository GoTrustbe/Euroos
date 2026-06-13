//! EuroDesktop compositor (Track 5).
//!
//! Een software-compositor die een desktop in de huisstijl van het UI-prototype
//! tekent: donkere achtergrond, linker-sidebar, overlappende vensters met
//! titelbalken (traffic-light-knoppen) in z-volgorde, en een muiscursor bovenop.
//! Tekent rechtstreeks naar de GOP-framebuffer (software-rendering); een
//! Vulkan-backend is een latere fase.

use alloc::string::String;
use alloc::vec::Vec;

use crate::eds::{self, SecState};
use crate::font::CHAR_HEIGHT;
use crate::graphics::{Color, FrameBuffer};

/// Linkermarge die de zwevende dock inneemt (dock zelf is 62px @ x=14, +marge).
/// Vensters beginnen rechts hiervan.
pub const SIDEBAR_W: usize = 90;
const TITLEBAR_H: usize = 44;
// Zwevende dock-geometrie (EDS: left/top/bottom 14, breedte 62).
const DOCK_X: usize = 14;
const DOCK_W: usize = 62;
const DOCK_M: usize = 14;
// Rechter statuspaneel.
const PANEL_W: usize = 284;

// Dock-tegelmetriek + de app-volgorde (index = `dock_targets`-index in main.rs).
const TILE: usize = 42;
const TILE_GAP: usize = 8;
const TILE_TOP: usize = DOCK_M + 64;
/// De dock-app-tegels, van boven naar onder. Honest mapping: elk icoon opent de
/// app die het voorstelt (AG-1 voegde files/notes/clock toe).
pub const DOCK_APPS: [&str; 11] =
    ["files", "notes", "clock", "browser", "terminal", "settings", "store", "star", "text", "monitor", "log"];
/// Welke dock-tegel is geopend/actief (usize::MAX = geen) — voor het accentbalkje.
static ACTIVE_DOCK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(usize::MAX);
/// Markeer welke dock-tegel actief is (de kernel zet dit bij open/sluiten).
pub fn set_active_dock(i: Option<usize>) {
    ACTIVE_DOCK.store(i.unwrap_or(usize::MAX), core::sync::atomic::Ordering::Relaxed);
}

/// Een venster (surface) met titel, inhoud en security-status (EDS). De body toont
/// `content` als monospace tekstregels (terminal / live systeemstatus) of, als `ui`
/// niet leeg is, een EuroUI-widgetpaneel.
pub struct Window {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub title: String,
    pub content: Vec<String>,
    /// EuroUI-widgetpaneel; als niet leeg, getekend i.p.v. `content` (tekst).
    pub ui: Vec<crate::euroui::Widget>,
    pub active: bool,
    pub accent: Color,
    pub sec: SecState,
    /// EuroSuite-app (Writer/Calc/Impress) — getekend i.p.v. tekst/widgets.
    pub app: crate::suite_ui::SuiteApp,
    /// Zichtbaar? `false` na sluiten/minimaliseren (niet getekend, niet aanklikbaar).
    pub visible: bool,
    /// Vorige geometrie (x,y,w,h) als het venster gemaximaliseerd is; `None` = normaal.
    pub restore: Option<(usize, usize, usize, usize)>,
}

/// Welke titelbalk-knop (verkeerslicht) is aangeklikt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TitleButton {
    Close,
    Minimize,
    Maximize,
}

impl Window {
    pub fn titlebar_contains(&self, mx: usize, my: usize) -> bool {
        mx >= self.x && mx < self.x + self.w && my >= self.y && my < self.y + TITLEBAR_H
    }
    /// Ligt (mx,my) ergens binnen dit venster (voor focus/raise op klik)?
    pub fn contains(&self, mx: usize, my: usize) -> bool {
        mx >= self.x && mx < self.x + self.w && my >= self.y && my < self.y + self.h
    }
    /// Welke verkeerslicht-knop ligt onder (mx,my)? De drie stippen staan op
    /// x+14/34/54, verticaal gecentreerd in de titelbalk, 13px — met een iets
    /// ruimere trefzone zodat ze makkelijk te raken zijn.
    pub fn title_button_at(&self, mx: usize, my: usize) -> Option<TitleButton> {
        let cy = self.y + (TITLEBAR_H - 13) / 2;
        // Verticaal binnen de stip-rij (met marge)?
        if my + 3 < cy || my > cy + 16 {
            return None;
        }
        let mxi = mx as i32;
        for (i, base) in [14i32, 34, 54].into_iter().enumerate() {
            let cx = self.x as i32 + base + 6; // stip-midden
            if (mxi - cx).abs() <= 9 {
                return Some(match i {
                    0 => TitleButton::Close,
                    1 => TitleButton::Minimize,
                    _ => TitleButton::Maximize,
                });
            }
        }
        None
    }
}

/// Welke dock-tegel-index (zie [`DOCK_APPS`]) ligt onder (px,py)? None als de
/// klik niet op een tegel valt.
pub fn dock_icon_at(px: usize, py: usize) -> Option<usize> {
    if px < DOCK_X || px >= DOCK_X + DOCK_W {
        return None;
    }
    if py < TILE_TOP {
        return None;
    }
    let i = (py - TILE_TOP) / (TILE + TILE_GAP);
    if i < DOCK_APPS.len() && py < TILE_TOP + i * (TILE + TILE_GAP) + TILE {
        Some(i)
    } else {
        None
    }
}

/// Het werkgebied-rechthoek (x,y,w,h) voor een gemaximaliseerd venster: tussen de
/// dock (links) en het statuspaneel (rechts), met de EDS-marge rondom.
pub fn work_area(screen_w: usize, screen_h: usize) -> (usize, usize, usize, usize) {
    let x = SIDEBAR_W;
    let y = DOCK_M;
    let w = screen_w.saturating_sub(SIDEBAR_W + DOCK_M + PANEL_W + DOCK_M);
    let h = screen_h.saturating_sub(DOCK_M * 2);
    (x, y, w, h)
}

pub fn draw_window(fb: &FrameBuffer, win: &Window) {
    // Zachte slagschaduw — sterker voor het actieve venster (diepte/EDS).
    let (spread, off) = if win.active { (16, 7) } else { (9, 4) };
    fb.drop_shadow(win.x, win.y, win.w, win.h, spread, off, Color::rgb(0x1A, 0x22, 0x2C));
    // Vensterlichaam (EDS radius-L token).
    fb.fill_rounded_rect(win.x, win.y, win.w, win.h, eds::RADIUS_L, Color::SURFACE);

    // Titelbalk: surface-2, onderkant recht zodat hij aansluit (EDS).
    let tb = Color::CARD;
    fb.fill_rounded_rect(win.x, win.y, win.w, TITLEBAR_H, eds::RADIUS_L, tb);
    fb.fill_rect(win.x, win.y + 18, win.w, TITLEBAR_H - 18, tb);

    // Traffic-light-knoppen (EDS-kleuren).
    let cy = win.y + (TITLEBAR_H - 13) / 2;
    fb.fill_rounded_rect(win.x + 14, cy, 13, 13, 7, Color::rgb(0xEC, 0x6A, 0x5E));
    fb.fill_rounded_rect(win.x + 34, cy, 13, 13, 7, Color::rgb(0xF4, 0xBF, 0x50));
    fb.fill_rounded_rect(win.x + 54, cy, 13, 13, 7, Color::rgb(0x61, 0xC5, 0x54));

    // Titel links na de knoppen (icoon-accent + naam), niet gecentreerd.
    let ty = win.y + (TITLEBAR_H - 14) / 2;
    crate::text::draw_px(fb, win.x + 82, ty, &win.title, Color::INK, 13.0);

    // "Protected"-pill rechts (groen) — sandboxed & encrypted, EDS-security.
    if win.sec.sandboxed {
        let label = "Protected";
        let lw = crate::text::width_px(label, 11.5);
        let pill_w = lw + 32;
        let pillx = win.x + win.w - 14 - pill_w;
        let pilly = win.y + (TITLEBAR_H - 22) / 2;
        fb.fill_rounded_rect(pillx, pilly, pill_w, 22, 11, Color::SUCCESS_SOFT);
        crate::icons::draw(fb, "shieldCheck", pillx + 7, pilly + 4, 14, Color::SUCCESS);
        crate::text::draw_px(fb, pillx + 25, pilly + 5, label, Color::SUCCESS, 11.5);
    }

    // Hairline onder de titelbalk.
    fb.fill_rect(win.x, win.y + TITLEBAR_H, win.w, 1, Color::BORDER);

    // Inset-glans langs de bovenrand (CSS: inset 0 .5px 0 rgba(255,255,255,.9)) —
    // een subtiele witte lijn net binnen de afgeronde bovenrand die het glas-
    // effect geeft dat de referentie heeft.
    let r = eds::RADIUS_L;
    let mut col = win.x + r;
    while col + 1 < win.x + win.w - r {
        fb.blend(col, win.y, Color::WHITE, 190);
        col += 1;
    }

    // Inhoud (body) — apart zodat hij goedkoop alleen-zichzelf kan hertekenen.
    draw_window_body(fb, win);
}

/// Herteken ALLEEN de body (inhoud) van een venster — voor goedkope live updates
/// (bv. het System-venster per klok-tick) zónder de slagschaduw opnieuw te tekenen
/// (die zou anders stapelen). Wist eerst de oude tekst en tekent dan de inhoud.
pub fn draw_window_body(fb: &FrameBuffer, win: &Window) {
    // EuroReken: ECHTE rekenmachine — render de LIVE toestand uit `win.content`.
    if win.app == crate::suite_ui::SuiteApp::Reken {
        crate::calc_ui::render(fb, win.x, win.y, win.w, win.h, &win.content);
        return;
    }
    // EuroWeb: bruikbare browser (tabbladen + adresbalk) — leest de globale toestand.
    if win.app == crate::suite_ui::SuiteApp::Browser {
        crate::webview::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroBeheer: instellingen — toont de LIVE kernel-toestand (euroguard/net/systeem).
    if win.app == crate::suite_ui::SuiteApp::Settings {
        crate::settings_ui::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroAgent: dispatch-paneel — intent + live cap-gated agent-lus.
    if win.app == crate::suite_ui::SuiteApp::Agent {
        crate::agent_ui::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroInstall: begeleide grafische installer (plan + live FDE).
    if win.app == crate::suite_ui::SuiteApp::Installer {
        crate::installer::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroFiles: bestandsbeheerder — toont het LIVE EuroFS.
    if win.app == crate::suite_ui::SuiteApp::Files {
        crate::files::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroNotes: notitie-app — echte Markdown via de euronotes-engine.
    if win.app == crate::suite_ui::SuiteApp::Notes {
        crate::notes::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroClock: wereldklokken + lokale tijd uit de ECHTE RTC.
    if win.app == crate::suite_ui::SuiteApp::Clock {
        crate::clockapp::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    if win.app == crate::suite_ui::SuiteApp::Text {
        crate::textedit::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    if win.app == crate::suite_ui::SuiteApp::Monitor {
        crate::monitor::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    if win.app == crate::suite_ui::SuiteApp::Log {
        crate::logview::render(fb, win.x, win.y, win.w, win.h);
        return;
    }
    // EuroSuite-app? Render de rijke Writer/Calc/Impress-GUI.
    if win.app != crate::suite_ui::SuiteApp::None {
        crate::suite_ui::render(fb, win.x, win.y, win.w, win.h, win.app);
        return;
    }
    // Inhoud: EuroUI-widgetpaneel of platte (monospace) tekstregels.
    if !win.ui.is_empty() {
        crate::euroui::draw_panel(fb, win.x, win.y + TITLEBAR_H, win.w, &win.ui);
        return;
    }
    // Body-achtergrond wissen (oude regels weg) — laat de onderste afgeronde hoeken
    // met rust zodat ze niet vierkant worden.
    let by = win.y + TITLEBAR_H + 1;
    let bh = win.h.saturating_sub(TITLEBAR_H + 1 + eds::RADIUS_L);
    fb.fill_rect(win.x + 1, by, win.w - 2, bh, Color::SURFACE);
    // Scroll naar ONDEREN: toon de LAATSTE regels (incl. de live prompt) i.p.v.
    // de top van de buffer — zo zie je echt wat de shell nú doet.
    let visible = (win.h.saturating_sub(TITLEBAR_H + 24)) / 16;
    let start = win.content.len().saturating_sub(visible);
    let mut ty = win.y + TITLEBAR_H + 12;
    for line in &win.content[start..] {
        if ty + CHAR_HEIGHT > win.y + win.h {
            break;
        }
        // Terminal/systeemstatus: monospace zodat kolommen uitlijnen.
        crate::text::draw_mono(fb, win.x + 16, ty, line, Color::TEXT_SEC, 1);
        ty += 16;
    }
}

/// Het body-rechthoekgebied (x,y,w,h) van een venster — voor `present_rect` na een
/// `draw_window_body`.
pub fn window_body_rect(win: &Window) -> (usize, usize, usize, usize) {
    (win.x, win.y + TITLEBAR_H, win.w, win.h - TITLEBAR_H)
}

/// 12 EU-sterren in een ring (vaste offsets — geen trig in no_std nodig).
const STAR_RING: [(i8, i8); 12] = [
    (10, 0), (9, 5), (5, 9), (0, 10), (-5, 9), (-9, 5),
    (-10, 0), (-9, -5), (-5, -9), (0, -10), (5, -9), (9, -5),
];

/// Het EU-embleem: blauwe schijf + ring van 12 gouden sterren, gecentreerd op
/// (cx,cy) met straal `r`.
fn draw_eu_mark(fb: &FrameBuffer, cx: usize, cy: usize, r: usize) {
    fb.fill_rounded_rect(cx - r, cy - r, r * 2, r * 2, r, Color::ACCENT);
    let star_r = (r as i32 * 13) / 18; // sterren dicht bij de rand
    for &(dx, dy) in &STAR_RING {
        let sx = cx as i32 + (dx as i32 * star_r) / 10;
        let sy = cy as i32 + (dy as i32 * star_r) / 10;
        fb.fill_rect((sx - 1) as usize, (sy - 1) as usize, 2, 2, Color::GOLD);
    }
}

/// Zwevende dock links: glas-kaart met EU-merk, kleurrijke app-tegels, actief-
/// balkje en gebruiker-avatar onderaan (EDS `v3-dock`).
fn draw_sidebar(fb: &FrameBuffer, h: usize) {
    let dh = h - DOCK_M * 2;
    // Kaart + zachte schaduw.
    fb.drop_shadow(DOCK_X, DOCK_M, DOCK_W, dh, 14, 6, Color::rgb(0x1A, 0x22, 0x2C));
    fb.fill_rounded_rect(DOCK_X, DOCK_M, DOCK_W, dh, eds::RADIUS_L, Color::SURFACE);
    fb.draw_border(DOCK_X, DOCK_M, DOCK_W, dh, 1, Color::BORDER);
    let cx = DOCK_X + DOCK_W / 2;

    // EU-merk bovenaan (size 36 → r 18).
    draw_eu_mark(fb, cx, DOCK_M + 28, 18);
    fb.fill_rect(cx - 15, DOCK_M + 54, 30, 1, Color::BORDER);

    // Kleurrijke app-tegels.
    let tile = TILE;
    let tx = cx - tile / 2;
    let active = ACTIVE_DOCK.load(core::sync::atomic::Ordering::Relaxed);
    let mut iy = TILE_TOP;
    for (i, id) in DOCK_APPS.iter().enumerate() {
        crate::appicons::draw_tile(fb, tx, iy, tile, id);
        // Actief/geopend: accent-balkje aan de linkerrand van de dock.
        if i == active {
            fb.fill_rounded_rect(DOCK_X + 1, iy + tile / 2 - 6, 4, 12, 2, Color::ACCENT);
        }
        iy += tile + TILE_GAP;
    }

    // Gebruiker-avatar onderaan: accent-ring + initialen van de INGELOGDE gebruiker
    // (afgeleid van de EuroID-sessie — nooit hardcoded persoonsgegevens).
    let av = 40usize;
    let ax = cx - av / 2;
    let ay = DOCK_M + dh - av - 12;
    fb.fill_rounded_rect(ax, ay, av, av, av / 2, Color::ACCENT); // 2px accent-ring
    fb.fill_rounded_rect(ax + 2, ay + 2, av - 4, av - 4, (av - 4) / 2, Color::ACCENT_SOFT);
    let initials = crate::auth::session_initials();
    let iw = crate::text::width_px(&initials, 14.0);
    crate::text::draw_px(fb, ax + (av - iw) / 2, ay + 12, &initials, Color::ACCENT, 14.0);
}

/// Wallpaper in de "desktop.html"-referentielook, in software-pixels:
/// 1) een koel→warm verticale gradient, 2) een zachte EU-blauwe radiale gloed
/// links-midden, 3) een subtiel stippelraster. Vervangt de effen `clear()`.
fn draw_wallpaper(fb: &FrameBuffer) {
    let w = fb.width();
    let h = fb.height().max(1);
    // Diagonale gradient: koel blauwgrijs (linksboven) → warm zandbeige
    // (rechtsonder). Iets sterker contrast dan de referentie-CSS zodat het ook
    // op een fototoestel/schermdump zichtbaar is, maar nog steeds rustig.
    let cool = Color::rgb(0xE6, 0xEC, 0xF4); // licht blauwgrijs
    let warm = Color::rgb(0xED, 0xE4, 0xD4); // warm zandbeige
    let denom = (w + h).max(1) as f32;
    for row in 0..h {
        // Per rij één basiskleur op de diagonaal (x=0); de horizontale drift is
        // klein genoeg om per-rij te benaderen → 1 lerp + rij-fill (goedkoop).
        let t0 = row as f32 / denom;
        // Twee segmenten zodat de horizontale component toch meekleurt: links- en
        // rechterhelft licht verschillend ingekleurd.
        let left = cool.lerp(warm, t0);
        let right = cool.lerp(warm, (row as f32 + w as f32 * 0.5) / denom);
        fb.fill_rect(0, row, w / 2, 1, left);
        fb.fill_rect(w / 2, row, w - w / 2, 1, right);
    }

    // 2) Zachte radiale gloed (EU-blauw), links-midden. Coarse: stap 2px + 2×2-blok.
    let gcx = (w / 6) as i32;
    let gcy = (h / 2) as i32;
    let gr = (w / 4).max(1) as i32;
    let grf = gr as f32;
    let mut py = (gcy - gr).max(0);
    let py_end = (gcy + gr).min(h as i32);
    while py < py_end {
        let mut px = (gcx - gr).max(0);
        let px_end = (gcx + gr).min(w as i32);
        while px < px_end {
            let dx = (px - gcx) as f32;
            let dy = (py - gcy) as f32;
            let d2 = dx * dx + dy * dy;
            if d2 < grf * grf {
                let t = 1.0 - crate::graphics::sqrtf(d2) / grf;
                let a = (30.0 * t * t) as u8; // ~12% in het hart, vloeiend uit
                if a > 0 {
                    let (ux, uy) = (px as usize, py as usize);
                    fb.blend(ux, uy, Color::ACCENT, a);
                    fb.blend(ux + 1, uy, Color::ACCENT, a);
                    fb.blend(ux, uy + 1, Color::ACCENT, a);
                    fb.blend(ux + 1, uy + 1, Color::ACCENT, a);
                }
            }
            px += 2;
        }
        py += 2;
    }

    // 3) Stippelraster (24px-grid) — fijne, zichtbare textuur. 2×2-stip zodat hij
    //    ook na schaling leesbaar blijft; warm-donkere tint, lage dekking.
    let step = 24usize;
    let dot = Color::rgb(0x6B, 0x60, 0x52);
    let mut y = 8;
    while y < h {
        let mut x = 8;
        while x < w {
            fb.blend(x, y, dot, 34);
            fb.blend(x + 1, y, dot, 22);
            fb.blend(x, y + 1, dot, 22);
            x += step;
        }
        y += step;
    }
}

/// Live systeemcijfers voor het statuspaneel (echte, veranderende waarden).
#[derive(Clone, Copy, Default)]
pub struct SysStats {
    pub free_mb: u64,
    pub total_mb: u64,
    pub uptime_s: u64,
    pub cores: u32,
    pub procs: u32,
}

/// Rechter statuspaneel — de blikvanger: echte klok + "device safe"-kaart +
/// een LIVE systeemkaart (RAM/uptime/cores/processen — verandert terwijl het OS draait).
/// `with_shadow=false` bij tick-updates zodat de schaduw niet stapelt.
/// Begrenzend rechthoek (x,y,w,h) van het statuspaneel incl. schaduw-marge — voor
/// DIRTY-RECT-rendering: bij een klok-tick blitten we enkel dit gebied i.p.v. het
/// hele scherm (SPERF). Klok-kaart (150) + gap (12) + systeemkaart (168) + marge.
pub fn status_panel_rect(w: usize) -> (usize, usize, usize, usize) {
    let px = w.saturating_sub(DOCK_M + PANEL_W);
    let py = DOCK_M;
    let total_h = 150 + 12 + 168 + 22; // panelinhoud + schaduw onder
    (px.saturating_sub(2), py.saturating_sub(2), PANEL_W + 22, total_h)
}

pub fn draw_status_panel(fb: &FrameBuffer, w: usize, _h: usize, hm: &str, date: &str, stats: &SysStats, with_shadow: bool) {
    let px = w - DOCK_M - PANEL_W;
    let py = DOCK_M;
    let ch = 150usize; // hoogte klok-kaart
    if with_shadow {
        fb.drop_shadow(px, py, PANEL_W, ch, 12, 5, Color::rgb(0x1A, 0x22, 0x2C));
    }
    fb.fill_rounded_rect(px, py, PANEL_W, ch, eds::RADIUS_L, Color::SURFACE);
    fb.draw_border(px, py, PANEL_W, ch, 1, Color::BORDER);

    // Grote klok (44px) + datum (14px).
    crate::text::draw_px(fb, px + 18, py + 16, hm, Color::INK, 44.0);
    crate::text::draw_px(fb, px + 18, py + 66, date, Color::TEXT_SEC, 14.0);

    // Thema-toggle (maan) rechtsboven.
    let tb = 34usize;
    let tbx = px + PANEL_W - 16 - tb;
    let tby = py + 16;
    fb.fill_rounded_rect(tbx, tby, tb, tb, tb / 2, Color::BORDER); // ronde 1px-rand
    fb.fill_rounded_rect(tbx + 1, tby + 1, tb - 2, tb - 2, (tb - 2) / 2, Color::SURFACE);
    crate::icons::draw(fb, "moon", tbx + 8, tby + 8, 18, Color::TEXT_SEC);

    // "Your device is safe"-kaart (groen).
    let gy = py + 90;
    let gx = px + 14;
    let gw = PANEL_W - 28;
    let gh = 46usize;
    fb.fill_rounded_rect(gx, gy, gw, gh, eds::RADIUS_M, Color::SUCCESS_SOFT);
    crate::icons::draw(fb, "shieldCheck", gx + 12, gy + 12, 22, Color::SUCCESS);
    crate::text::draw_px(fb, gx + 44, gy + 9, "Your device is safe", Color::INK, 13.0);
    crate::text::draw_px(fb, gx + 44, gy + 26, "Verified boot \u{00B7} encrypted \u{00B7} sandboxed", Color::TEXT_SEC, 11.5);

    // ── Live systeemkaart (echte, veranderende cijfers) ──
    let sy = py + ch + 12;
    let sh = 168usize;
    if with_shadow {
        fb.drop_shadow(px, sy, PANEL_W, sh, 12, 5, Color::rgb(0x1A, 0x22, 0x2C));
    }
    fb.fill_rounded_rect(px, sy, PANEL_W, sh, eds::RADIUS_L, Color::SURFACE);
    fb.draw_border(px, sy, PANEL_W, sh, 1, Color::BORDER);
    let lx = px + 18;
    crate::text::draw_px(fb, lx, sy + 14, "SYSTEM", Color::TEXT_DIM, 10.5);

    // Geheugenbalk (gebruikt = totaal - vrij).
    let used = stats.total_mb.saturating_sub(stats.free_mb);
    crate::text::draw_px(fb, lx, sy + 34, "Memory", Color::TEXT_SEC, 13.0);
    let mr = alloc::format!("{} / {} MiB", used, stats.total_mb);
    let mrw = crate::text::width_px(&mr, 12.0);
    crate::text::draw_px(fb, px + PANEL_W - 18 - mrw, sy + 35, &mr, Color::INK, 12.0);
    let barw = PANEL_W - 36;
    let bary = sy + 54;
    fb.fill_rounded_rect(lx, bary, barw, 6, 3, Color::SURFACE_3);
    let frac = if stats.total_mb > 0 { (used * barw as u64 / stats.total_mb) as usize } else { 0 };
    if frac > 0 {
        fb.fill_rounded_rect(lx, bary, frac.max(3), 6, 3, Color::ACCENT);
    }

    // Tekstrijen: uptime / cores / processen.
    let rows = [
        ("Uptime", fmt_uptime(stats.uptime_s)),
        ("CPU cores", alloc::format!("{} online", stats.cores)),
        ("Processes", alloc::format!("{}", stats.procs)),
    ];
    let mut ry = sy + 74;
    for (label, val) in rows {
        crate::text::draw_px(fb, lx, ry, label, Color::TEXT_SEC, 12.5);
        let vw = crate::text::width_px(&val, 12.5);
        crate::text::draw_px(fb, px + PANEL_W - 18 - vw, ry, &val, Color::INK, 12.5);
        ry += 22;
    }
}

fn fmt_uptime(s: u64) -> alloc::string::String {
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        alloc::format!("{}h {:02}m {:02}s", h, m, sec)
    } else if m > 0 {
        alloc::format!("{}m {:02}s", m, sec)
    } else {
        alloc::format!("{}s", sec)
    }
}

/// Klassieke pijl-cursor (X=rand, .=vulling, spatie=transparant).
const CURSOR: [&str; 16] = [
    "X          ",
    "XX         ",
    "X.X        ",
    "X..X       ",
    "X...X      ",
    "X....X     ",
    "X.....X    ",
    "X......X   ",
    "X.......X  ",
    "X........X ",
    "X.....XXXXX",
    "X..X..X    ",
    "X.X X..X   ",
    "XX  X..X   ",
    "X    X..X  ",
    "     XXX   ",
];

pub fn draw_cursor(fb: &FrameBuffer, mx: usize, my: usize) {
    for (row, line) in CURSOR.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            let c = match ch {
                b'X' => Color::rgb(0x0A, 0x0F, 0x1A),
                b'.' => Color::WHITE,
                _ => continue,
            };
            fb.put_pixel(mx + col, my + row, c);
        }
    }
}

/// Teken de volledige desktop: warme achtergrond, zwevende dock, vensters (z-
/// volgorde `order`, back-to-front), en het rechter statuspaneel bovenop.
/// De cursor beheert de desktop-loop apart.
pub fn render(fb: &FrameBuffer, windows: &[Window], order: &[usize], clock: &str, date: &str, stats: &SysStats) {
    let w = fb.width();
    let h = fb.height();
    draw_wallpaper(fb);
    draw_sidebar(fb, h);
    for &i in order {
        if windows[i].visible {
            draw_window(fb, &windows[i]);
        }
    }
    draw_status_panel(fb, w, h, clock, date, stats, true);
}

pub const CURSOR_W: usize = 11;
pub const CURSOR_H: usize = 16;

/// Bewaar de pixels onder de cursor (save-under) zodat we 'm vlot kunnen wissen.
pub fn save_cursor_bg(fb: &FrameBuffer, x: usize, y: usize, buf: &mut [Color]) {
    for r in 0..CURSOR_H {
        for c in 0..CURSOR_W {
            buf[r * CURSOR_W + c] = fb.get_pixel(x + c, y + r);
        }
    }
}

/// Herstel de bewaarde pixels (wis de cursor).
pub fn restore_cursor_bg(fb: &FrameBuffer, x: usize, y: usize, buf: &[Color]) {
    for r in 0..CURSOR_H {
        for c in 0..CURSOR_W {
            fb.put_pixel(x + c, y + r, buf[r * CURSOR_W + c]);
        }
    }
}
