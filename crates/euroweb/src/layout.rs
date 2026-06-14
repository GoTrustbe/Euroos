//! EuroWeb layout engine (Sprint AB-B3): the CSS box model.
//!
//! Turns the [`Dom`] + computed styles ([`crate::css::ComputedStyle`]) into a
//! **layout tree** of positioned boxes. Implements a real block
//! formatting context (vertical stacking, margin/border/padding/width/height)
//! plus inline text flow with **line breaking** for the height calculation. Follows
//! the classic CSS box-model algorithm (à la "robinson").
//!
//! Deliberately scoped: floats, flex and grid come later (B4); inline elements
//! are in this first version included as text in the parent flow. Pure
//! `no_std` logic, host-tested.

use alloc::string::String;
use alloc::vec::Vec;

use crate::css::ComputedStyle;
use crate::dom::{Dom, NodeId, NodeKind};

/// A rectangle (content box), in px.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Edge values (margin/border/padding) per side.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeSizes {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

/// The dimensions of a box: content + padding + border + margin.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Dimensions {
    pub content: Rect,
    pub padding: EdgeSizes,
    pub border: EdgeSizes,
    pub margin: EdgeSizes,
}

impl Dimensions {
    /// The margin box: content + padding + border + margin.
    pub fn margin_box(&self) -> Rect {
        let p = self.padding;
        let b = self.border;
        let m = self.margin;
        Rect {
            x: self.content.x - p.left - b.left - m.left,
            y: self.content.y - p.top - b.top - m.top,
            width: self.content.width + p.left + p.right + b.left + b.right + m.left + m.right,
            height: self.content.height + p.top + p.bottom + b.top + b.bottom + m.top + m.bottom,
        }
    }
    fn margin_box_height(&self) -> f32 {
        self.content.height
            + self.padding.top
            + self.padding.bottom
            + self.border.top
            + self.border.bottom
            + self.margin.top
            + self.margin.bottom
    }
}

/// A replaced/control element (intrinsic size instead of text flow): an
/// image or a form control. Sizes in px (u32 → usable with `Eq`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replaced {
    /// `<img>` — the kernel fetches `src` and blits the pixels.
    Image { src: String, w: u32, h: u32 },
    /// `<input type=text>`/`textarea` — name + (initial) value.
    Field { name: String, value: String, w: u32, h: u32 },
    /// `<button>`/`<input type=submit>` — button label.
    Button { label: String, w: u32, h: u32 },
}

/// The kind of layout box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoxType {
    Block,
    /// A text fragment (inline content), with the text itself.
    Text(String),
    /// Anonymous block box that groups inline content.
    Anonymous,
    /// A replaced element (image or form control) with fixed size.
    Replaced(Replaced),
}

/// Determine whether an (inline) element becomes a replaced/control box, and with
/// which intrinsic size. Reads the relevant attributes from the DOM. `None` = ordinary
/// element (no replaced box).
fn replaced_for(name: &str, dom: &Dom, node: NodeId) -> Option<Replaced> {
    let attr = |k: &str| dom.attr(node, k).map(String::from);
    let px = |k: &str, d: u32| dom.attr(node, k).and_then(|v| v.trim().trim_end_matches("px").trim().parse::<u32>().ok()).unwrap_or(d);
    match name {
        "img" => Some(Replaced::Image {
            src: attr("src").unwrap_or_default(),
            w: px("width", 160).clamp(1, 4096),
            h: px("height", 120).clamp(1, 4096),
        }),
        "input" => {
            let ty = attr("type").unwrap_or_else(|| String::from("text"));
            if ty == "submit" || ty == "button" {
                let label = attr("value").unwrap_or_else(|| String::from("Submit"));
                let w = (label.chars().count() as u32 * 9 + 28).clamp(64, 320);
                Some(Replaced::Button { label, w, h: 30 })
            } else if ty == "hidden" {
                None
            } else {
                Some(Replaced::Field {
                    name: attr("name").unwrap_or_default(),
                    value: attr("value").unwrap_or_default(),
                    w: px("size", 0).checked_mul(9).filter(|&w| w > 0).unwrap_or(220).clamp(80, 480),
                    h: 30,
                })
            }
        }
        "button" => {
            let label = {
                let t = dom.text_content(node);
                if t.trim().is_empty() { String::from("Button") } else { String::from(t.trim()) }
            };
            let w = (label.chars().count() as u32 * 9 + 28).clamp(64, 320);
            Some(Replaced::Button { label, w, h: 30 })
        }
        "textarea" => Some(Replaced::Field {
            name: attr("name").unwrap_or_default(),
            value: String::from(dom.text_content(node).trim()),
            w: 360,
            h: 90,
        }),
        _ => None,
    }
}

/// One node in the layout tree.
#[derive(Debug, Clone)]
pub struct LayoutBox {
    pub box_type: BoxType,
    pub node: Option<NodeId>,
    pub dimensions: Dimensions,
    pub children: Vec<LayoutBox>,
    /// Number of lines after breaking (only for [`BoxType::Text`]).
    pub line_count: usize,
    /// Effective font size (px) of this box.
    pub font_size: f32,
}

/// Text measuring function: (text, font size) → width in px.
pub type Measure = fn(&str, f32) -> f32;

/// Default metric: monospace approximation (advance ≈ 0.5·font-size per character).
pub fn monospace_measure(text: &str, font_size: f32) -> f32 {
    text.chars().count() as f32 * 0.5 * font_size
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "html" | "body" | "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            | "ul" | "ol" | "li" | "section" | "article" | "header" | "footer"
            | "main" | "nav" | "aside" | "blockquote" | "pre" | "figure"
            | "figcaption" | "table" | "form" | "fieldset" | "hr" | "address"
            | "dl" | "dt" | "dd" | "details" | "summary" | "menu"
    )
}

/// Non-rendering elements (no box).
fn is_non_visual(tag: &str) -> bool {
    matches!(tag, "head" | "script" | "style" | "title" | "meta" | "link" | "base")
}

fn parse_px(style: &ComputedStyle, prop: &str) -> Option<f32> {
    let v = style.get(prop)?;
    let v = v.trim();
    if let Some(num) = v.strip_suffix("px") {
        num.trim().parse::<f32>().ok()
    } else if v == "0" {
        Some(0.0)
    } else {
        // "auto"/percentages/keywords → not as fixed px.
        v.parse::<f32>().ok()
    }
}

fn font_size_of(style: &ComputedStyle, parent: f32) -> f32 {
    parse_px(style, "font-size").unwrap_or(parent)
}

fn display_none(style: &ComputedStyle) -> bool {
    style.get("display").map(|d| d == "none").unwrap_or(false)
}

/// Maximum nesting depth that we build up. Beyond this point we stop
/// descending: this prevents a stack overflow on malicious/deeply nested pages
/// (e.g. thousands of `<div>` inside each other) — a stability bound, not a cosmetic one.
pub const MAX_DEPTH: usize = 80;

/// Build the layout tree (without positions) from DOM + styles, starting at `<body>`.
fn build_box(
    dom: &Dom,
    styles: &[ComputedStyle],
    node: NodeId,
    parent_font: f32,
    depth: usize,
) -> Option<LayoutBox> {
    match &dom.nodes[node].kind {
        NodeKind::Element { name, .. } => {
            if is_non_visual(name) || display_none(&styles[node]) {
                return None;
            }
            let fs = font_size_of(&styles[node], parent_font);
            let mut bx = LayoutBox {
                box_type: BoxType::Block,
                node: Some(node),
                dimensions: Dimensions::default(),
                children: Vec::new(),
                line_count: 0,
                font_size: fs,
            };
            // Stop descending beyond the safe depth (anti stack-overflow).
            if depth < MAX_DEPTH {
                collect_children(dom, styles, node, fs, &mut bx.children, depth + 1);
            }
            Some(bx)
        }
        NodeKind::Text(t) => {
            if t.trim().is_empty() {
                return None;
            }
            Some(LayoutBox {
                box_type: BoxType::Text(t.clone()),
                node: Some(node),
                dimensions: Dimensions::default(),
                children: Vec::new(),
                line_count: 0,
                font_size: parent_font,
            })
        }
        _ => None,
    }
}

/// Collect the children of `node`; inline elements are flattened (their
/// text flows along in the parent flow).
fn collect_children(
    dom: &Dom,
    styles: &[ComputedStyle],
    node: NodeId,
    font: f32,
    out: &mut Vec<LayoutBox>,
    depth: usize,
) {
    // Safety bound: do not descend deeper than this (anti stack-overflow).
    if depth >= MAX_DEPTH {
        return;
    }
    for &child in &dom.nodes[node].children {
        match &dom.nodes[child].kind {
            NodeKind::Element { name, .. } => {
                if is_non_visual(name) || display_none(&styles[child]) {
                    continue;
                }
                if let Some(rep) = replaced_for(name, dom, child) {
                    // Replaced element (img/input/button): own box with fixed size.
                    out.push(LayoutBox {
                        box_type: BoxType::Replaced(rep),
                        node: Some(child),
                        dimensions: Dimensions::default(),
                        children: Vec::new(),
                        line_count: 0,
                        font_size: font_size_of(&styles[child], font),
                    });
                } else if is_block_tag(name) {
                    if let Some(b) = build_box(dom, styles, child, font, depth + 1) {
                        out.push(b);
                    }
                } else {
                    // Inline element: flatten into the same flow.
                    let fs = font_size_of(&styles[child], font);
                    collect_children(dom, styles, child, fs, out, depth + 1);
                }
            }
            NodeKind::Text(_) => {
                if let Some(b) = build_box(dom, styles, child, font, depth + 1) {
                    out.push(b);
                }
            }
            _ => {}
        }
    }
}

/// Break text into lines within `width`; returns the number of lines.
fn wrap_lines(text: &str, width: f32, font_size: f32, measure: Measure) -> usize {
    let space = measure(" ", font_size);
    let mut lines = 1usize;
    let mut cur = 0f32;
    let mut any = false;
    for word in text.split_whitespace() {
        any = true;
        let wlen = measure(word, font_size);
        if cur > 0.0 && cur + space + wlen > width && width > 0.0 {
            lines += 1;
            cur = wlen;
        } else {
            cur += if cur > 0.0 { space + wlen } else { wlen };
        }
    }
    if any {
        lines
    } else {
        0
    }
}

impl LayoutBox {
    fn layout(&mut self, containing: Dimensions, styles: &[ComputedStyle], measure: Measure) {
        match &self.box_type {
            BoxType::Block | BoxType::Anonymous => self.layout_block(containing, styles, measure),
            BoxType::Text(_) => self.layout_text(containing, measure),
            BoxType::Replaced(_) => self.layout_replaced(containing),
        }
    }

    /// Place a replaced element (img/input/button): fixed intrinsic size,
    /// below the height of the containing block filled so far.
    fn layout_replaced(&mut self, containing: Dimensions) {
        let (w, h) = match &self.box_type {
            BoxType::Replaced(Replaced::Image { w, h, .. })
            | BoxType::Replaced(Replaced::Field { w, h, .. })
            | BoxType::Replaced(Replaced::Button { w, h, .. }) => (*w as f32, *h as f32),
            _ => (0.0, 0.0),
        };
        let d = &mut self.dimensions;
        d.content.width = w.min(containing.content.width.max(1.0));
        d.content.x = containing.content.x;
        d.content.y = containing.content.y + containing.content.height;
        d.content.height = h;
    }

    fn style<'a>(&self, styles: &'a [ComputedStyle]) -> Option<&'a ComputedStyle> {
        self.node.map(|n| &styles[n])
    }

    fn layout_block(&mut self, containing: Dimensions, styles: &[ComputedStyle], measure: Measure) {
        self.calculate_width(containing, styles);
        self.calculate_position(containing, styles);
        // Layout children, height accumulates in self.dimensions.content.height.
        for child in &mut self.children {
            child.layout(self.dimensions, styles, measure);
            self.dimensions.content.height += child.dimensions.margin_box_height();
        }
        // Explicit height overrides.
        if let Some(s) = self.style(styles) {
            if let Some(h) = parse_px(s, "height") {
                self.dimensions.content.height = h;
            }
        }
    }

    fn calculate_width(&mut self, containing: Dimensions, styles: &[ComputedStyle]) {
        let d = &mut self.dimensions;
        let s = self.node.map(|n| &styles[n]);
        let get = |p: &str| s.and_then(|s| parse_px(s, p)).unwrap_or(0.0);
        d.padding.left = get("padding-left").max(self_or(s, "padding"));
        d.padding.right = get("padding-right").max(self_or(s, "padding"));
        d.border.left = get("border-left-width").max(border_shorthand(s));
        d.border.right = get("border-right-width").max(border_shorthand(s));
        d.margin.left = get("margin-left").max(self_or(s, "margin"));
        d.margin.right = get("margin-right").max(self_or(s, "margin"));

        let total_extra =
            d.padding.left + d.padding.right + d.border.left + d.border.right + d.margin.left + d.margin.right;
        let width = s.and_then(|s| parse_px(s, "width"));
        d.content.width = match width {
            Some(w) => w,
            None => (containing.content.width - total_extra).max(0.0),
        };
    }

    fn calculate_position(&mut self, containing: Dimensions, styles: &[ComputedStyle]) {
        let d = &mut self.dimensions;
        let s = self.node.map(|n| &styles[n]);
        let get = |p: &str| s.and_then(|s| parse_px(s, p)).unwrap_or(0.0);
        d.padding.top = get("padding-top").max(self_or(s, "padding"));
        d.padding.bottom = get("padding-bottom").max(self_or(s, "padding"));
        d.border.top = get("border-top-width").max(border_shorthand(s));
        d.border.bottom = get("border-bottom-width").max(border_shorthand(s));
        d.margin.top = get("margin-top").max(self_or(s, "margin"));
        d.margin.bottom = get("margin-bottom").max(self_or(s, "margin"));

        d.content.x = containing.content.x + d.margin.left + d.border.left + d.padding.left;
        // Stack below the height of the containing block filled so far.
        d.content.y = containing.content.y + containing.content.height + d.margin.top + d.border.top + d.padding.top;
        d.content.height = 0.0;
    }

    fn layout_text(&mut self, containing: Dimensions, measure: Measure) {
        let text = match &self.box_type {
            BoxType::Text(t) => t.clone(),
            _ => return,
        };
        let d = &mut self.dimensions;
        d.content.width = containing.content.width;
        d.content.x = containing.content.x;
        d.content.y = containing.content.y + containing.content.height;
        let fs = self.font_size;
        let lines = wrap_lines(&text, d.content.width, fs, measure);
        self.line_count = lines;
        let line_height = 1.2 * fs;
        d.content.height = lines as f32 * line_height;
    }
}

fn self_or(s: Option<&ComputedStyle>, shorthand: &str) -> f32 {
    s.and_then(|s| parse_px(s, shorthand)).unwrap_or(0.0)
}
fn border_shorthand(s: Option<&ComputedStyle>) -> f32 {
    // "border: 1px ..." → take the first px number.
    let v = match s.and_then(|s| s.get("border")) {
        Some(v) => v,
        None => return 0.0,
    };
    for tok in v.split_whitespace() {
        if let Some(num) = tok.strip_suffix("px") {
            if let Ok(n) = num.parse::<f32>() {
                return n;
            }
        }
    }
    0.0
}

/// Determine the root (`<body>`, otherwise `<html>`, otherwise document root).
fn root_node(dom: &Dom) -> NodeId {
    (0..dom.len())
        .find(|&i| dom.tag(i) == Some("body"))
        .or_else(|| (0..dom.len()).find(|&i| dom.tag(i) == Some("html")))
        .unwrap_or_else(|| dom.root())
}

/// Compute the layout tree for a viewport width with the default metric.
pub fn layout(dom: &Dom, styles: &[ComputedStyle], viewport_width: f32) -> LayoutBox {
    layout_with(dom, styles, viewport_width, monospace_measure)
}

/// Like [`layout`], but with a custom text measuring function (font rasterizer).
pub fn layout_with(dom: &Dom, styles: &[ComputedStyle], viewport_width: f32, measure: Measure) -> LayoutBox {
    let root = root_node(dom);
    let base_font = font_size_of(&styles[root], 16.0);
    let mut root_box =
        build_box(dom, styles, root, base_font, 0).unwrap_or(LayoutBox {
            box_type: BoxType::Block,
            node: Some(root),
            dimensions: Dimensions::default(),
            children: Vec::new(),
            line_count: 0,
            font_size: base_font,
        });

    let viewport = Dimensions {
        content: Rect { x: 0.0, y: 0.0, width: viewport_width, height: 0.0 },
        ..Default::default()
    };
    root_box.layout(viewport, styles, measure);
    root_box
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{compute, parse_stylesheet};
    use crate::parser::parse;

    fn build(html: &str, css: &str, vw: f32) -> LayoutBox {
        let dom = parse(html);
        let ss = parse_stylesheet(css);
        let styles = compute(&dom, &[&ss]);
        layout(&dom, &styles, vw)
    }

    #[test]
    fn blocks_stack_vertically() {
        let lb = build(
            "<body><div></div><div></div></body>",
            "div { height: 50px }",
            800.0,
        );
        assert_eq!(lb.children.len(), 2);
        assert_eq!(lb.children[0].dimensions.content.y, 0.0);
        // Second div starts below the first (50px tall).
        assert_eq!(lb.children[1].dimensions.content.y, 50.0);
    }

    #[test]
    fn block_fills_containing_width() {
        let lb = build("<body><div></div></body>", "", 800.0);
        assert_eq!(lb.children[0].dimensions.content.width, 800.0);
    }

    #[test]
    fn padding_and_margin_offset_content() {
        let lb = build(
            "<body><div></div></body>",
            "div { padding-left: 10px; margin-left: 20px; height: 30px }",
            800.0,
        );
        let d = &lb.children[0].dimensions;
        // content.x = margin.left + padding.left
        assert_eq!(d.content.x, 30.0);
        // content.width = 800 - (padding.left + margin.left)
        assert_eq!(d.content.width, 770.0);
    }

    #[test]
    fn explicit_height_respected() {
        let lb = build("<body><div></div></body>", "div { height: 123px }", 600.0);
        assert_eq!(lb.children[0].dimensions.content.height, 123.0);
    }

    #[test]
    fn text_wraps_into_multiple_lines() {
        // 10 words of 4 characters; monospace 16px → 8px/char → ~32px/word +8px space.
        // At 100px wide ~2 words fit per line → multiple lines.
        let lb = build(
            "<body><p>aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa</p></body>",
            "p { width: 100px }",
            800.0,
        );
        let p = &lb.children[0];
        // The <p> contains one Text child.
        let text = &p.children[0];
        assert!(matches!(text.box_type, BoxType::Text(_)));
        assert!(text.line_count >= 3, "expected multiple lines, got {}", text.line_count);
        // Height = lines × 1.2 × font-size(16) = lines × 19.2.
        let expected = text.line_count as f32 * 1.2 * 16.0;
        assert!((text.dimensions.content.height - expected).abs() < 0.01);
    }

    #[test]
    fn single_line_text_one_line() {
        let lb = build("<body><p>kort</p></body>", "", 800.0);
        let text = &lb.children[0].children[0];
        assert_eq!(text.line_count, 1);
    }

    #[test]
    fn display_none_removes_box() {
        let lb = build(
            "<body><div></div><div class=\"hide\"></div></body>",
            ".hide { display: none } div { height: 10px }",
            800.0,
        );
        assert_eq!(lb.children.len(), 1);
    }

    #[test]
    fn parent_height_sums_children() {
        let lb = build(
            "<body><section><div></div><div></div></section></body>",
            "div { height: 40px }",
            800.0,
        );
        let section = &lb.children[0];
        // Two divs of 40px → section content height 80px.
        assert_eq!(section.dimensions.content.height, 80.0);
    }
}
