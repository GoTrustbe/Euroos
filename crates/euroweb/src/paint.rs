//! EuroWeb paint (Sprint AB-B6): layout tree → **display list**.
//!
//! Walks the [`crate::layout::LayoutBox`] tree and turns it into an ordered
//! list of draw commands ([`DisplayItem`]): background rectangles (from
//! `background`/`background-color`) and text (with color from the inherited
//! `color`). A separate raster layer (in the kernel) executes the display list on
//! the EuroDisplay framebuffer. Includes a CSS color parser (named + `#hex` +
//! `rgb()`). Pure, host-tested `no_std` logic.

use alloc::string::String;
use alloc::vec::Vec;

use crate::css::ComputedStyle;
use crate::dom::Dom;
use crate::layout::{BoxType, LayoutBox, Replaced};

/// A single draw command in document order (back → front).
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayItem {
    /// Filled rectangle (background), color as 0xRRGGBB.
    Rect { x: f32, y: f32, w: f32, h: f32, color: u32 },
    /// Text at (x,y), color 0xRRGGBB, font size in px.
    Text { x: f32, y: f32, text: String, color: u32, size: f32 },
    /// `<img>` box: the kernel fetches `src` (data:/http) and blits the pixels.
    Image { x: f32, y: f32, w: f32, h: f32, src: String },
    /// Text input field: `node` = DOM node (for focus/live value).
    Field { x: f32, y: f32, w: f32, h: f32, node: usize, name: String, value: String },
    /// Button/submit button: `node` = DOM node (for click → form submit).
    Button { x: f32, y: f32, w: f32, h: f32, node: usize, label: String },
}

/// Build the display list for a laid-out page.
pub fn paint(dom: &Dom, styles: &[ComputedStyle], root: &LayoutBox) -> Vec<DisplayItem> {
    let mut items = Vec::new();
    walk(dom, styles, root, &mut items);
    items
}

fn walk(dom: &Dom, styles: &[ComputedStyle], b: &LayoutBox, out: &mut Vec<DisplayItem>) {
    // 1) Background rectangle (padding box) if there is a background color.
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
    // 2) Text.
    if let BoxType::Text(t) = &b.box_type {
        let color = b
            .node
            .and_then(|n| styles.get(n))
            .and_then(|s| s.get("color"))
            .and_then(|c| parse_color(c))
            .unwrap_or(0x1A_1714); // default ink
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
    // 2b) Replaced element (image / form control).
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
    // 3) Children (above the background).
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

/// Parse a CSS color into 0xRRGGBB. Supports `#rgb`, `#rrggbb`, `rgb(r,g,b)`
/// and a set of named colors.
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
        let dom = parse("<body><div><p>Hello EuroOS</p></div></body>");
        let css = parse_stylesheet("div { background-color: #2D6BE0; height: 40px } p { color: white }");
        let styles = compute(&dom, &[&css]);
        let lb = layout(&dom, &styles, 800.0);
        let items = paint(&dom, &styles, &lb);

        // There is a blue background rectangle (the div).
        assert!(items.iter().any(|i| matches!(i, DisplayItem::Rect { color, .. } if *color == 0x2D6BE0)));
        // And white text "Hello EuroOS".
        assert!(items.iter().any(|i| matches!(i, DisplayItem::Text { text, color, .. } if text == "Hello EuroOS" && *color == 0xFFFFFF)));
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
            r#"<body><form action="/search" method="get">
               <input type="text" name="q" value="euro">
               <input type="submit" value="Search"></form></body>"#,
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
        assert_eq!(btn, Some(String::from("Search")));
    }

    #[test]
    fn document_order_background_before_text() {
        let dom = parse("<body><div><span>text</span></div></body>");
        let css = parse_stylesheet("div { background: red }");
        let styles = compute(&dom, &[&css]);
        let lb = layout(&dom, &styles, 400.0);
        let items = paint(&dom, &styles, &lb);
        let rect_idx = items.iter().position(|i| matches!(i, DisplayItem::Rect { .. }));
        let text_idx = items.iter().position(|i| matches!(i, DisplayItem::Text { .. }));
        assert!(rect_idx < text_idx); // background before text
    }
}
