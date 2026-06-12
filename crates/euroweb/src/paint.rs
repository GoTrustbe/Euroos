//! EuroWeb paint (Sprint AB-B6): layout-boom → **display-lijst**.
//!
//! Wandelt de [`crate::layout::LayoutBox`]-boom en zet hem om in een geordende
//! lijst teken-commando's ([`DisplayItem`]): achtergrond-rechthoeken (uit
//! `background`/`background-color`) en tekst (met kleur uit de overgeërfde
//! `color`). Een aparte rasterlaag (in de kernel) voert de display-lijst uit op
//! de EuroDisplay-framebuffer. Bevat een CSS-kleurparser (benoemd + `#hex` +
//! `rgb()`). Pure, host-geteste `no_std`-logica.

use alloc::string::String;
use alloc::vec::Vec;

use crate::css::ComputedStyle;
use crate::dom::Dom;
use crate::layout::{BoxType, LayoutBox, Replaced};

/// Eén teken-commando in document-volgorde (achter → voor).
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayItem {
    /// Gevulde rechthoek (achtergrond), kleur als 0xRRGGBB.
    Rect { x: f32, y: f32, w: f32, h: f32, color: u32 },
    /// Tekst op (x,y), kleur 0xRRGGBB, fontgrootte in px.
    Text { x: f32, y: f32, text: String, color: u32, size: f32 },
    /// `<img>`-vak: de kernel haalt `src` op (data:/http) en blit de pixels.
    Image { x: f32, y: f32, w: f32, h: f32, src: String },
    /// Tekst-invoerveld: `node` = DOM-knoop (voor focus/live waarde).
    Field { x: f32, y: f32, w: f32, h: f32, node: usize, name: String, value: String },
    /// Knop/verzendknop: `node` = DOM-knoop (voor klik → formulier-submit).
    Button { x: f32, y: f32, w: f32, h: f32, node: usize, label: String },
}

/// Bouw de display-lijst voor een gelayoute pagina.
pub fn paint(dom: &Dom, styles: &[ComputedStyle], root: &LayoutBox) -> Vec<DisplayItem> {
    let mut items = Vec::new();
    walk(dom, styles, root, &mut items);
    items
}

fn walk(dom: &Dom, styles: &[ComputedStyle], b: &LayoutBox, out: &mut Vec<DisplayItem>) {
    // 1) Achtergrond-rechthoek (padding-box) als er een achtergrondkleur is.
    if let Some(node) = b.node {
        if let Some(style) = styles.get(node) {
            if let Some(color) = bg_color(style) {
                let d = &b.dimensions;
                out.push(DisplayItem::Rect {
                    x: d.content.x - d.padding.left,
                    y: d.content.y - d.padding.top,
                    w: d.content.width + d.padding.left + d.padding.right,
                    h: d.content.height + d.padding.top + d.padding.bottom,
                    color,
                });
            }
        }
    }
    // 2) Tekst.
    if let BoxType::Text(t) = &b.box_type {
        let color = b
            .node
            .and_then(|n| styles.get(n))
            .and_then(|s| s.get("color"))
            .and_then(|c| parse_color(c))
            .unwrap_or(0x1A_1714); // standaard inkt
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            out.push(DisplayItem::Text {
                x: b.dimensions.content.x,
                y: b.dimensions.content.y,
                text: String::from(trimmed),
                color,
                size: b.font_size,
            });
        }
    }
    // 2b) Vervangen element (afbeelding / formulierbesturing).
    if let BoxType::Replaced(r) = &b.box_type {
        let d = &b.dimensions;
        let (x, y, w, h) = (d.content.x, d.content.y, d.content.width, d.content.height);
        let node = b.node.unwrap_or(0);
        match r {
            Replaced::Image { src, .. } => {
                out.push(DisplayItem::Image { x, y, w, h, src: src.clone() });
            }
            Replaced::Field { name, value, .. } => {
                out.push(DisplayItem::Field { x, y, w, h, node, name: name.clone(), value: value.clone() });
            }
            Replaced::Button { label, .. } => {
                out.push(DisplayItem::Button { x, y, w, h, node, label: label.clone() });
            }
        }
    }
    // 3) Kinderen (boven de achtergrond).
    for c in &b.children {
        walk(dom, styles, c, out);
    }
}

fn bg_color(style: &ComputedStyle) -> Option<u32> {
    style
        .get("background-color")
        .or_else(|| style.get("background"))
        .and_then(|v| parse_color(v))
}

/// Parse een CSS-kleur naar 0xRRGGBB. Ondersteunt `#rgb`, `#rrggbb`, `rgb(r,g,b)`
/// en een set benoemde kleuren.
pub fn parse_color(input: &str) -> Option<u32> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
                Some(((r as u32 * 17) << 16) | ((g as u32 * 17) << 8) | (b as u32 * 17))
            }
            6 => u32::from_str_radix(hex, 16).ok(),
            _ => None,
        };
    }
    if let Some(rest) = s.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<u32> = rest.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if parts.len() == 3 {
            return Some(((parts[0] & 0xFF) << 16) | ((parts[1] & 0xFF) << 8) | (parts[2] & 0xFF));
        }
        return None;
    }
    named_color(&s.to_ascii_lowercase())
}

fn named_color(name: &str) -> Option<u32> {
    let v = match name {
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        "red" => 0xFF0000,
        "green" => 0x008000,
        "lime" => 0x00FF00,
        "blue" => 0x0000FF,
        "navy" => 0x000080,
        "teal" => 0x008080,
        "gray" | "grey" => 0x808080,
        "silver" => 0xC0C0C0,
        "yellow" => 0xFFFF00,
        "orange" => 0xFFA500,
        "purple" => 0x800080,
        "gold" => 0xE2A33A,
        "transparent" => return None,
        _ => return None,
    };
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{compute, parse_stylesheet};
    use crate::layout::layout;
    use crate::parser::parse;

    #[test]
    fn color_parsing() {
        assert_eq!(parse_color("#fff"), Some(0xFFFFFF));
        assert_eq!(parse_color("#2D6BE0"), Some(0x2D6BE0));
        assert_eq!(parse_color("rgb(45, 107, 224)"), Some(0x2D6BE0));
        assert_eq!(parse_color("white"), Some(0xFFFFFF));
        assert_eq!(parse_color("navy"), Some(0x000080));
        assert_eq!(parse_color("nonsense"), None);
    }

    #[test]
    fn paints_background_and_text() {
        let dom = parse("<body><div><p>Hallo EuroOS</p></div></body>");
        let css = parse_stylesheet("div { background-color: #2D6BE0; height: 40px } p { color: white }");
        let styles = compute(&dom, &[&css]);
        let lb = layout(&dom, &styles, 800.0);
        let items = paint(&dom, &styles, &lb);

        // Er is een blauwe achtergrond-rechthoek (de div).
        assert!(items.iter().any(|i| matches!(i, DisplayItem::Rect { color, .. } if *color == 0x2D6BE0)));
        // En witte tekst "Hallo EuroOS".
        assert!(items.iter().any(|i| matches!(i, DisplayItem::Text { text, color, .. } if text == "Hallo EuroOS" && *color == 0xFFFFFF)));
    }

    #[test]
    fn renders_image_box() {
        let dom = parse(r#"<body><img src="/logo.qoi" width="80" height="60"></body>"#);
        let styles = compute(&dom, &[]);
        let lb = layout(&dom, &styles, 800.0);
        let items = paint(&dom, &styles, &lb);
        let img = items.iter().find_map(|i| match i {
            DisplayItem::Image { src, w, h, .. } => Some((src.clone(), *w, *h)),
            _ => None,
        });
        assert_eq!(img, Some((String::from("/logo.qoi"), 80.0, 60.0)));
    }

    #[test]
    fn renders_form_field_and_button() {
        let dom = parse(
            r#"<body><form action="/zoek" method="get">
               <input type="text" name="q" value="euro">
               <input type="submit" value="Zoek"></form></body>"#,
        );
        let styles = compute(&dom, &[]);
        let lb = layout(&dom, &styles, 800.0);
        let items = paint(&dom, &styles, &lb);
        let field = items.iter().find_map(|i| match i {
            DisplayItem::Field { name, value, .. } => Some((name.clone(), value.clone())),
            _ => None,
        });
        assert_eq!(field, Some((String::from("q"), String::from("euro"))));
        let btn = items.iter().find_map(|i| match i {
            DisplayItem::Button { label, .. } => Some(label.clone()),
            _ => None,
        });
        assert_eq!(btn, Some(String::from("Zoek")));
    }

    #[test]
    fn document_order_background_before_text() {
        let dom = parse("<body><div><span>tekst</span></div></body>");
        let css = parse_stylesheet("div { background: red }");
        let styles = compute(&dom, &[&css]);
        let lb = layout(&dom, &styles, 400.0);
        let items = paint(&dom, &styles, &lb);
        let rect_idx = items.iter().position(|i| matches!(i, DisplayItem::Rect { .. }));
        let text_idx = items.iter().position(|i| matches!(i, DisplayItem::Text { .. }));
        assert!(rect_idx < text_idx); // achtergrond vóór tekst
    }
}
