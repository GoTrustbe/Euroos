# EuroDesktop — Visuele Upgrade Gids
*Van de huidige UI naar de desktop.html referentie look — in Rust pixels*

**Context:** EuroDesktop is een software renderer die pixels schrijft naar een UEFI GOP
framebuffer. Er is geen CSS, geen HTML, geen GPU. Elke visuele verbetering is een
aanpassing van draw calls in de compositor Rust code.

De referentie is de `desktop.html` demo — een React/CSS mockup van hoe de echte
compositor er zou moeten uitzien. Dit document vertaalt die CSS naar equivalente
Rust draw primitives.

---

## Huidig vs gewenst

| Element | Nu (screenshots) | Gewenst (desktop.html) |
|---------|-----------------|----------------------|
| Vensterkaders | Scherpe rechthoek, dunne rand | Rounded corners (r=16), zachte slagschaduw |
| Achtergrond vensters | Wit/grijs solid | Semi-transparant met blur-achtig frosted effect |
| Titelbalk | Effen kleur, dikke balk | Dun, licht transparant over de vensterinhoud |
| Wallpaper | Effen kleur | Zachte radiale gradient + subtiel stippelpatroon |
| Sidebar iconen | Vlak, geen context | Actieve indicator: blauwe lijn links |
| Protected badge | Tekst in titelbalk | Groene pill badge met pulserende dot |
| Systeempanel | Basic tekst layout | Frosted glass sidebar met gestructureerde secties |
| Systeembalk kleur | Div-kleuren (blauw/groen/rood) | Zachte status bars met progress fill |

---

## Design tokens (Rust constanten)

Dit zijn de exacte waarden uit de `desktop.html` CSS, vertaald naar Rust constanten.
Voeg toe aan `src/display/design.rs` of equivalent:

```rust
// ═══════════════════════════════════════════════════════
// EURO DESIGN SYSTEM — Design tokens
// Afgeleid van desktop.html CSS variabelen
// ═══════════════════════════════════════════════════════

pub mod colors {
    // Wallpaper
    pub const PAPER: u32          = 0xFF_EEE9DF;  // var(--paper)

    // EU Brand
    pub const EU_BLUE: u32        = 0xFF_2D6BE0;  // var(--eu-blue)
    pub const EU_BLUE_DIM: u32    = 0x1C_2D6BE0;  // rgba(45,107,224, 0.11)
    pub const EU_GOLD: u32        = 0xFF_E2A33A;  // var(--eu-gold) — sterren

    // Glas oppervlakken
    // "Frosted glass" = semi-transparant + lichte blur-simulatie
    // In software rendering: ARGB met alpha blending over de achtergrond
    pub const GLASS_BG: u32       = 0xCC_F8F5EF;  // rgba(248,245,239, 0.80)
    pub const GLASS_BORDER: u32   = 0xC2_FFFFFF;  // rgba(255,255,255, 0.76)
    pub const SIDEBAR_BG: u32     = 0xD6_F0ECE5;  // rgba(240,236,229, 0.84)

    // Tekst
    pub const TEXT_900: u32       = 0xFF_1A1714;  // primaire tekst
    pub const TEXT_600: u32       = 0xFF_5A5550;  // secundaire tekst
    pub const TEXT_400: u32       = 0xFF_9A958F;  // muted/hint tekst

    // Status
    pub const SAFE_GREEN: u32     = 0xFF_1E9B5F;  // var(--safe)
    pub const SAFE_BG: u32        = 0x17_1E9B5F;  // rgba(30,155,95, 0.09)
    pub const SAFE_BORDER: u32    = 0x38_1E9B5F;  // rgba(30,155,95, 0.22)
    pub const WARN_AMBER: u32     = 0xFF_D97706;  // var(--warn)

    // Randen
    pub const BORDER_SUBTLE: u32  = 0x12_000000;  // rgba(0,0,0, 0.07)
    pub const BORDER_MEDIUM: u32  = 0x24_000000;  // rgba(0,0,0, 0.14)
    pub const HOVER_BG: u32       = 0x0D_000000;  // rgba(0,0,0, 0.05)

    // Vensterdecoratie
    pub const DOT_RED: u32        = 0xFF_FF5F57;
    pub const DOT_AMBER: u32      = 0xFF_FEBC2E;
    pub const DOT_GREEN: u32      = 0xFF_28C840;
}

pub mod spacing {
    pub const SIDEBAR_W: u32      = 60;
    pub const PANEL_W: u32        = 272;
    pub const WIN_RADIUS: u32     = 16;  // border-radius van vensters
    pub const WIN_TITLEBAR_H: u32 = 44;  // hoogte van de titelbalk
    pub const WIN_TAB_H: u32      = 34;  // hoogte van tabbladen
    pub const WIN_TOOLBAR_H: u32  = 40;  // hoogte van de werkbalk
    pub const WIN_STATUS_H: u32   = 24;  // hoogte van de statusbalk
    pub const SIDEBAR_BTN_W: u32  = 42;  // breedte sidebar knoppen
    pub const SIDEBAR_BTN_H: u32  = 42;  // hoogte sidebar knoppen
    pub const SIDEBAR_BTN_R: u32  = 11;  // radius sidebar knoppen
    pub const LOGO_W: u32         = 38;  // sidebar logo breedte
    pub const LOGO_R: u32         = 11;  // sidebar logo radius
}

pub mod shadow {
    // Slagschaduw voor vensters
    // In software rendering: gaussiaans gesimuleerd via meerdere alpha-lagen
    pub const WIN_BLUR_RADIUS: u32   = 28;   // hoe breed de schaduw uitwaaiert
    pub const WIN_SHADOW_ALPHA: u8   = 33;   // 0x21 = 13% opacity voor outer shadow
    pub const WIN_SHADOW_OFFSET_Y: i32 = 6;  // schaduw valt licht naar beneden
    pub const WIN_INSET_TOP: u32     = 1;    // inset highlight bovenrand
}
```

---

## De vijf grootste visuele verbeteringen

### 1. Wallpaper — radiale gradient + stippelpatroon

**Nu:** effen kleur (`fill_rect(0, 0, W, H, SOLID_COLOR)`)

**Gewenst:**
```rust
pub fn draw_wallpaper(fb: &mut Framebuffer) {
    let w = fb.width();
    let h = fb.height();

    // Stap 1: basis gradient — licht blauwgrijs naar warm beige
    for y in 0..h {
        for x in 0..w {
            // Lineaire interpolatie: linksbovenhoek iets koeler
            let t = (x + y) as f32 / (w + h) as f32;
            let color = lerp_color(0xFF_E8ECF2, 0xFF_EEE9DF, t);
            fb.put(x, y, color);
        }
    }

    // Stap 2: subtiele radiale gloed links (EU blauw, 7% opacity)
    draw_radial_gradient(fb,
        x: w / 6,           // 15% van links
        y: h / 2,           // verticaal gecentreerd
        radius: w / 2,      // groot uitwaaieren
        color: colors::EU_BLUE,
        max_alpha: 18,       // 0x12 = ~7%
    );

    // Stap 3: stippelpatroon (26x26 grid, 1px dots, 35% opacity)
    let dot_spacing = 26u32;
    let dot_alpha: u8 = 89;  // 35% van 255
    let dot_color = blend_alpha(0xFF_000000, dot_alpha);
    for y in (0..h).step_by(dot_spacing as usize) {
        for x in (0..w).step_by(dot_spacing as usize) {
            fb.put(x, y, dot_color);
        }
    }
}
```

---

### 2. Vensters — rounded corners + slagschaduw

**Nu:** rechthoekige vensters zonder schaduw

**Gewenst:**
```rust
pub fn draw_window_frame(fb: &mut Framebuffer, rect: Rect, focused: bool) {
    let r = spacing::WIN_RADIUS;

    // ── Stap 1: slagschaduw (gesimuleerd via meerdere lagen) ──
    // Echte gaussiaanse blur is te zwaar voor software rendering.
    // Simulatie: 4-5 lagen steeds groter rect, steeds lagere alpha.
    let shadow_layers = [
        (2, 8,  0x08_000000u32),   // dichtste, sterkste
        (4, 12, 0x06_000000u32),
        (6, 18, 0x04_000000u32),
        (8, 24, 0x02_000000u32),   // buitenste, zwakste
    ];
    for (dx, dy, color) in shadow_layers {
        let shadow_rect = rect.expand(dx).translate(0, dy);
        fill_rounded_rect(fb, shadow_rect, r + dx, color);
    }

    // ── Stap 2: vensteroppervlak (frosted glass simulatie) ──
    // Echte backdrop-filter blur is GPU-only.
    // Simulatie: semi-transparant over de achtergrond (alpha blending).
    // Het effect is subtieler dan CSS blur maar geeft wel de doorzichtigheid.
    fill_rounded_rect(fb, rect, r, colors::GLASS_BG);

    // ── Stap 3: rand (glass border) ──
    // Buitenrand: witte glans (1px, 75% opacity)
    stroke_rounded_rect(fb, rect, r, colors::GLASS_BORDER, 1);

    // ── Stap 4: inset highlight bovenaan ──
    // CSS: inset 0 0.5px 0 rgba(255,255,255,0.9)
    // = een 1px witte lijn net onder de bovenrand
    let highlight_rect = Rect {
        x: rect.x + r as i32,
        y: rect.y,
        w: rect.w - 2 * r,
        h: 1,
    };
    fill_rect(fb, highlight_rect, 0xE6_FFFFFF);  // 90% wit
}
```

---

### 3. Titelbalk — modern, dun, licht

**Nu:** dikke balk met effen kleur

**Gewenst:**
```rust
pub fn draw_titlebar(fb: &mut Framebuffer, win: &Window) {
    let bar = Rect {
        x: win.rect.x,
        y: win.rect.y,
        w: win.rect.w,
        h: spacing::WIN_TITLEBAR_H as i32,
    };

    // Licht transparante overlay over het glasoppervlak
    // CSS: background: rgba(248,245,239, 0.45)
    fill_rect(fb, bar, 0x73_F8F5EF);

    // Onderlijn — subtiel (rgba(0,0,0,0.07))
    let divider = Rect { y: bar.y + bar.h - 1, h: 1, ..bar };
    fill_rect(fb, divider, colors::BORDER_SUBTLE);

    // Traffic light knoppen (12px, gap 6px)
    let dot_y = bar.y + (bar.h - 12) / 2;
    let dot_x = bar.x + 14;
    fill_circle(fb, dot_x,      dot_y, 6, colors::DOT_RED);
    fill_circle(fb, dot_x + 18, dot_y, 6, colors::DOT_AMBER);
    fill_circle(fb, dot_x + 36, dot_y, 6, colors::DOT_GREEN);

    // Venstericon (16x16, muted kleur)
    let icon_x = dot_x + 56;
    let icon_y = dot_y;
    draw_icon(fb, win.icon, icon_x, icon_y, 15, colors::TEXT_400);

    // Venstertitel (13px, font-weight 500, TEXT_600)
    let title_x = icon_x + 22;
    let title_y = bar.y + (bar.h - 13) / 2;
    draw_text(fb, &win.title, title_x, title_y, 13, colors::TEXT_600, FontWeight::Medium);

    // Protected badge (rechts, alleen als win.is_protected)
    if win.is_protected {
        draw_protected_badge(fb, bar.x + bar.w - 14, title_y);
    }
}

fn draw_protected_badge(fb: &mut Framebuffer, right_x: i32, center_y: i32) {
    // Groene pill: "● Protected"
    // CSS: background: rgba(30,155,95,0.09), border: 0.5px rgba(30,155,95,0.22)
    let text = "Protected";
    let text_w = measure_text(text, 11) as i32;
    let badge_w = text_w + 22;  // 9px padding links (dot+gap) + 9px rechts
    let badge_h = 20;
    let badge_x = right_x - badge_w;
    let badge_y = center_y - badge_h / 2;

    fill_rounded_rect(fb, Rect::new(badge_x, badge_y, badge_w, badge_h),
                      badge_h / 2, colors::SAFE_BG);
    stroke_rounded_rect(fb, Rect::new(badge_x, badge_y, badge_w, badge_h),
                        badge_h / 2, colors::SAFE_BORDER, 1);

    // Pulserende groene dot (6px)
    let dot_x = badge_x + 9;
    let dot_y = badge_y + badge_h / 2;
    fill_circle(fb, dot_x, dot_y, 3, colors::SAFE_GREEN);

    // Tekst
    draw_text(fb, text, dot_x + 9, badge_y + (badge_h - 11) / 2,
              11, colors::SAFE_GREEN, FontWeight::Medium);
}
```

---

### 4. Sidebar — actieve indicator lijn

**Nu:** vierkante highlight achter actief icoon

**Gewenst:**
```rust
pub fn draw_sidebar(fb: &mut Framebuffer, sidebar: &Sidebar) {
    // Achtergrond — glaseffect
    let rect = Rect::new(0, 0, spacing::SIDEBAR_W as i32, fb.height() as i32);
    fill_rect(fb, rect, colors::SIDEBAR_BG);
    // Rechterrand — glass border
    fill_rect(fb, Rect::new(rect.w - 1, 0, 1, rect.h), colors::GLASS_BORDER);

    // Logo
    let logo_x = (spacing::SIDEBAR_W - spacing::LOGO_W) as i32 / 2;
    let logo_y = 14;
    fill_rounded_rect(fb,
        Rect::new(logo_x, logo_y, spacing::LOGO_W as i32, spacing::LOGO_W as i32),
        spacing::LOGO_R,
        colors::EU_BLUE,
    );
    draw_eu_stars(fb, logo_x + 7, logo_y + 7, 24, colors::EU_GOLD);

    // Iconen
    let mut icon_y = logo_y + spacing::LOGO_W as i32 + 12;
    for item in &sidebar.items {
        draw_sidebar_button(fb, item, icon_y);
        icon_y += spacing::SIDEBAR_BTN_H as i32 + 2;

        if item.is_separator_after {
            let sep_y = icon_y + 5;
            fill_rect(fb, Rect::new(15, sep_y, 30, 1), colors::BORDER_MEDIUM);
            icon_y += 13;
        }
    }

    // Avatar (onderin)
    let av_y = fb.height() as i32 - 12 - 34;
    let av_x = (spacing::SIDEBAR_W - 34) as i32 / 2;
    fill_circle(fb, av_x + 17, av_y + 17, 17, colors::EU_BLUE);
    // Initialen
    draw_text(fb, "EU", av_x + 6, av_y + 11, 12, 0xFF_FFFFFF, FontWeight::SemiBold);
}

fn draw_sidebar_button(fb: &mut Framebuffer, item: &SidebarItem, y: i32) {
    let btn_x = (spacing::SIDEBAR_W - spacing::SIDEBAR_BTN_W) as i32 / 2;
    let btn_rect = Rect::new(btn_x, y, spacing::SIDEBAR_BTN_W as i32, spacing::SIDEBAR_BTN_H as i32);

    if item.is_active {
        // Blauwe achtergrond (rgba(45,107,224, 0.11))
        fill_rounded_rect(fb, btn_rect, spacing::SIDEBAR_BTN_R, colors::EU_BLUE_DIM);

        // Actieve indicator: 3px blauwe lijn links van de sidebar
        // CSS: ::before { left: -8px; width: 3px; height: 22px; background: --eu-blue }
        fill_rounded_rect(fb,
            Rect::new(0, y + (spacing::SIDEBAR_BTN_H as i32 - 22) / 2, 3, 22),
            2,
            colors::EU_BLUE,
        );

        draw_icon(fb, item.icon, btn_x + 11, y + 11, 20, colors::EU_BLUE);
    } else if item.is_hovered {
        fill_rounded_rect(fb, btn_rect, spacing::SIDEBAR_BTN_R, colors::HOVER_BG);
        draw_icon(fb, item.icon, btn_x + 11, y + 11, 20, colors::TEXT_600);
    } else {
        draw_icon(fb, item.icon, btn_x + 11, y + 11, 20, colors::TEXT_400);
    }
}
```

---

### 5. Systeempanel — rechts

**Nu:** eenvoudige tekst layout

**Gewenst structuur:**
```rust
pub fn draw_system_panel(fb: &mut Framebuffer, state: &SystemState) {
    let panel_x = fb.width() as i32 - spacing::PANEL_W as i32;
    let panel_rect = Rect::new(panel_x, 0, spacing::PANEL_W as i32, fb.height() as i32);

    // Achtergrond
    fill_rect(fb, panel_rect, colors::SIDEBAR_BG);
    fill_rect(fb, Rect::new(panel_x, 0, 1, panel_rect.h), colors::GLASS_BORDER);

    let mut cursor_y = panel_rect.y + 18;
    let pad_x = panel_x + 14;
    let inner_w = spacing::PANEL_W as i32 - 28;

    // ── Klok ──
    // Grote dunne klok (42px, font-weight 280)
    // Nota: aparte lichte variant van het font — of gewoon 300
    let time_str = state.time_string();  // "15:42"
    draw_text(fb, &time_str, pad_x, cursor_y, 42, colors::TEXT_900, FontWeight::Light);
    cursor_y += 50;

    // Datum
    let date_str = state.date_string();  // "Sat 6 June"
    draw_text(fb, &date_str, pad_x, cursor_y, 13, colors::TEXT_600, FontWeight::Regular);
    cursor_y += 28;

    // ── Safe card ──
    cursor_y = draw_safe_card(fb, pad_x, cursor_y, inner_w, state.is_safe);

    // ── System sectie ──
    cursor_y = draw_section_label(fb, pad_x, cursor_y, "SYSTEM");
    cursor_y = draw_stat_row(fb, pad_x, cursor_y, inner_w, "Memory",
                             state.mem_used, state.mem_total, "MB", colors::EU_BLUE);
    cursor_y = draw_stat_row(fb, pad_x, cursor_y, inner_w, "Uptime",
                             100, 100, &state.uptime_str, colors::SAFE_GREEN);
    cursor_y = draw_stat_row(fb, pad_x, cursor_y, inner_w, "CPU cores",
                             state.cpu_online, state.cpu_total, "online", colors::EU_BLUE);
    cursor_y = draw_stat_row(fb, pad_x, cursor_y, inner_w, "Processes",
                             state.processes, 80, &state.processes.to_string(), colors::EU_BLUE);

    // ── Quick Settings ──
    cursor_y = draw_section_label(fb, pad_x, cursor_y, "QUICK SETTINGS");
    cursor_y = draw_qs_grid(fb, pad_x, cursor_y, inner_w, &state.quick_settings);

    // ── Workspace ──
    cursor_y = draw_section_label(fb, pad_x, cursor_y, "WORKSPACE");
    cursor_y = draw_workspace_tabs(fb, pad_x, cursor_y, inner_w, &state.workspace);

    // ── In Use Right Now ──
    cursor_y = draw_section_label(fb, pad_x, cursor_y, "IN USE RIGHT NOW");
    for item in &state.in_use {
        cursor_y = draw_in_use_row(fb, pad_x, cursor_y, inner_w, item);
    }

    // ── Notifications ──
    cursor_y = draw_section_with_count(fb, pad_x, cursor_y, "NOTIFICATIONS",
                                        state.notifications.len() as u32);
    for notif in &state.notifications {
        cursor_y = draw_notification(fb, pad_x, cursor_y, inner_w, notif);
    }
}

fn draw_stat_row(fb: &mut Framebuffer, x: i32, y: i32, w: i32,
                 label: &str, value: u32, max: u32,
                 display: &str, fill_color: u32) -> i32 {
    // Label (68px breed)
    draw_text(fb, label, x, y + 2, 12, colors::TEXT_600, FontWeight::Regular);

    // Progress track (flex:1, height 4px)
    let track_x = x + 68 + 8;
    let track_w = w - 68 - 8 - 58 - 8;  // minus label, minus value
    let track_rect = Rect::new(track_x, y + 7, track_w, 4);
    fill_rounded_rect(fb, track_rect, 2, 0x12_000000);  // rgba(0,0,0,0.07)

    let fill_w = (track_w * value as i32 / max.max(1) as i32).min(track_w);
    fill_rounded_rect(fb, Rect::new(track_x, y + 7, fill_w, 4), 2, fill_color);

    // Waarde (rechts, monospace)
    let val_x = track_x + track_w + 8;
    draw_text(fb, display, val_x, y + 2, 10, colors::TEXT_400, FontWeight::Regular);

    y + 22  // volgende rij
}

fn draw_qs_grid(fb: &mut Framebuffer, x: i32, y: i32, w: i32,
                tiles: &[QuickSettingTile]) -> i32 {
    // 2x2 grid, gap 6px
    let tile_w = (w - 6) / 2;
    let tile_h = 60;

    for (i, tile) in tiles.iter().enumerate() {
        let col = (i % 2) as i32;
        let row = (i / 2) as i32;
        let tx = x + col * (tile_w + 6);
        let ty = y + row * (tile_h + 6);
        let tile_rect = Rect::new(tx, ty, tile_w, tile_h);

        if tile.is_active {
            // Ingeschakeld: solid EU blue
            fill_rounded_rect(fb, tile_rect, 11, colors::EU_BLUE);
            draw_icon(fb, tile.icon, tx + 10, ty + 10, 18, 0xE6_FFFFFF);
            draw_text(fb, &tile.name, tx + 10, ty + 32, 12, 0xFF_FFFFFF, FontWeight::Medium);
            draw_text(fb, &tile.sub,  tx + 10, ty + 46, 10, 0xA6_FFFFFF, FontWeight::Regular);
        } else {
            // Uitgeschakeld: zachte neutrale achtergrond
            fill_rounded_rect(fb, tile_rect, 11, 0x0A_000000);
            stroke_rounded_rect(fb, tile_rect, 11, 0x14_000000, 1);
            draw_icon(fb, tile.icon, tx + 10, ty + 10, 18, colors::TEXT_400);
            draw_text(fb, &tile.name, tx + 10, ty + 32, 12, colors::TEXT_900, FontWeight::Medium);
            draw_text(fb, &tile.sub,  tx + 10, ty + 46, 10, colors::TEXT_400, FontWeight::Regular);
        }
    }

    y + (((tiles.len() + 1) / 2) as i32) * (tile_h + 6) + 4
}
```

---

## Alpha blending helper (kern van het frosted glass effect)

Het grootste verschil tussen de huidige UI en de referentie is **semi-transparantie**.
De sidebar, vensterkaders, en titelbalken zijn allemaal half-doorschijnend.

In software rendering doe je dit met een alpha blend functie:

```rust
/// Blend foreground color OVER background color.
/// fg_alpha: 0 = volledig transparant, 255 = volledig opaque
#[inline]
pub fn alpha_blend(bg: u32, fg: u32, fg_alpha: u8) -> u32 {
    let a = fg_alpha as u32;
    let inv_a = 255 - a;

    let bg_r = (bg >> 16) & 0xFF;
    let bg_g = (bg >>  8) & 0xFF;
    let bg_b =  bg        & 0xFF;

    let fg_r = (fg >> 16) & 0xFF;
    let fg_g = (fg >>  8) & 0xFF;
    let fg_b =  fg        & 0xFF;

    let r = (fg_r * a + bg_r * inv_a) / 255;
    let g = (fg_g * a + bg_g * inv_a) / 255;
    let b = (fg_b * a + bg_b * inv_a) / 255;

    0xFF_000000 | (r << 16) | (g << 8) | b
}

/// Gebruik: sidebar achtergrond tekenen over wallpaper
pub fn fill_rect_alpha(fb: &mut Framebuffer, rect: Rect, color: u32) {
    let alpha = ((color >> 24) & 0xFF) as u8;
    let rgb = color & 0x00_FFFFFF;

    if alpha == 255 {
        // Volledig opaque: sneller pad
        fill_rect(fb, rect, color);
        return;
    }

    for y in rect.y..rect.y + rect.h {
        for x in rect.x..rect.x + rect.w {
            let bg = fb.get(x as u32, y as u32);
            let blended = alpha_blend(bg, rgb, alpha);
            fb.put(x as u32, y as u32, blended);
        }
    }
}
```

---

## Rounded rect helper

```rust
/// Vul een rechthoek met afgeronde hoeken.
/// In software rendering: volledige rij voor het midden,
/// per-pixel circle test voor de hoeken.
pub fn fill_rounded_rect(fb: &mut Framebuffer, rect: Rect, radius: u32, color: u32) {
    let r = radius as i32;
    let alpha = ((color >> 24) & 0xFF) as u8;

    for py in rect.y..rect.y + rect.h {
        for px in rect.x..rect.x + rect.w {
            let in_bounds = is_in_rounded_rect(px, py, rect, r);
            if in_bounds {
                if alpha < 255 {
                    let bg = fb.get(px as u32, py as u32);
                    let blended = alpha_blend(bg, color & 0x00_FFFFFF, alpha);
                    fb.put(px as u32, py as u32, blended);
                } else {
                    fb.put(px as u32, py as u32, color);
                }
            }
        }
    }
}

#[inline]
fn is_in_rounded_rect(px: i32, py: i32, rect: Rect, r: i32) -> bool {
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.w;
    let y1 = rect.y + rect.h;

    // Buiten de rechthoek?
    if px < x0 || px >= x1 || py < y0 || py >= y1 { return false; }

    // In de hoeken: circle test
    let corners = [
        (x0 + r, y0 + r),  // linksboven
        (x1 - r, y0 + r),  // rechtsboven
        (x0 + r, y1 - r),  // linksonder
        (x1 - r, y1 - r),  // rechtsonder
    ];

    for (cx, cy) in corners {
        if px < cx && py < cy || px >= cx && py < cy ||
           px < cx && py >= cy || px >= cx && py >= cy {
            // In de hoekzone — circle test
            let dx = px - cx;
            let dy = py - cy;
            if dx * dx + dy * dy > r * r { return false; }
        }
    }

    true
}
```

---

## EU Sterren logo (sidebar)

```rust
/// Tekent de EU-sterren cirkel (12 gouden sterren in een ring).
pub fn draw_eu_stars(fb: &mut Framebuffer, center_x: i32, center_y: i32,
                      diameter: i32, color: u32) {
    let n_stars = 12u32;
    let ring_r = (diameter as f32 / 2.0 * 0.72) as i32;  // iets kleiner dan de cirkel
    let star_r = (diameter as f32 / 2.0 * 0.13) as i32;  // straal van elke ster

    for i in 0..n_stars {
        let angle = (i as f32 / n_stars as f32) * core::f32::consts::TAU
                    - core::f32::consts::FRAC_PI_2;  // start bovenaan
        let sx = center_x + (ring_r as f32 * libm::cosf(angle)) as i32;
        let sy = center_y + (ring_r as f32 * libm::sinf(angle)) as i32;
        fill_circle(fb, sx, sy, star_r, color);
    }
}
```

---

## Claude Code implementatie volgorde

De veiligste volgorde om dit te implementeren zonder het huidige systeem te breken:

**Sessie 1 — Design tokens + helpers:**
- Voeg `src/display/design.rs` toe met alle constanten
- Implementeer `alpha_blend()`, `fill_rect_alpha()`, `fill_rounded_rect()`
- Voeg `fill_circle()` toe als die nog niet bestaat
- Host tests: verify dat de math klopt voor de hoekgevallen

**Sessie 2 — Wallpaper:**
- Vervang de effen wallpaper door de gradient + dot grid
- Boot verify: ziet er goed uit in QEMU

**Sessie 3 — Vensterframe:**
- `draw_window_frame()` met rounded corners en schaduwlagen
- `draw_titlebar()` met de nieuwe traffic lights en dunne balk
- `draw_protected_badge()` als groene pill
- Boot verify: vensters zien er modern uit

**Sessie 4 — Sidebar:**
- `draw_sidebar()` met de actieve indicator lijn
- `draw_eu_stars()` voor het logo
- Avatar cirkel
- Boot verify: sidebar ziet er correct uit

**Sessie 5 — Systeempanel:**
- `draw_system_panel()` met alle secties
- `draw_stat_row()` met progress bars
- `draw_qs_grid()` met active/inactive states
- Boot verify: panel toont alle info correct

---

## Belangrijk: geen CSS, geen HTML

De bovenstaande Rust functies zijn de **directe vertaling** van de CSS in `desktop.html`
naar software pixel rendering. De relatie is:

| CSS | Rust equivalent |
|-----|----------------|
| `border-radius: 16px` | `fill_rounded_rect(..., 16, ...)` |
| `background: rgba(248,245,239,0.80)` | `fill_rect_alpha(..., 0xCC_F8F5EF)` |
| `box-shadow: 0 20px 60px rgba(0,0,0,0.13)` | Gesimuleerde schaduwlagen |
| `backdrop-filter: blur(28px)` | Semi-transparante alpha blend (geen echte blur) |
| `border: 0.5px solid rgba(255,255,255,0.76)` | `stroke_rounded_rect(..., 1, 0xC2_FFFFFF)` |
| `font-weight: 300` | `FontWeight::Light` variant van de AA rasterizer |
| `color: #2D6BE0` | `colors::EU_BLUE` constante |

*Claude Code sprint commando: `"implementeer EuroDesktop design tokens sessie 1"`*
*of `"verbeter de wallpaper rendering naar de gradient + dot grid versie"`*
