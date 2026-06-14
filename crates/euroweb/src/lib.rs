//! EuroWeb — the sovereign browser engine of EuroOS (Track B, see
//! `docs/EUROBROWSER-PLAN.md`).
//!
//! From scratch in Rust, `no_std`, no foreign engine, no ICU/NSS. This is the
//! **foundation layer**: HTML5 tokenizer + tree construction → DOM. On top of that,
//! later sprints add CSS (cascade/selectors), layout (block/inline/flex) and paint to
//! the EuroDisplay framebuffer; JavaScript arrives as a tree-walking interpreter with
//! per-tab EuroGuard capabilities.
//!
//! Architecture choices:
//! - **A single `Vec<Node>` arena** for the DOM (no `Rc`/`RefCell`), `#![forbid(unsafe_code)]`.
//! - **Spec-faithful tokenizer state machine** (WHATWG), including RAWTEXT/RCDATA
//!   and character references — host-tested against HTML5lib-like cases.
//! - **Pragmatic tree construction** (open-element stack, void elements,
//!   implicit closing) — enough for static pages; the full
//!   insertion-mode machine is a later refinement.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod css;
pub mod dom;
pub mod entities;
pub mod flex;
pub mod layout;
pub mod paint;
pub mod parser;
pub mod tokenizer;

pub use css::{compute, parse_stylesheet, ComputedStyle, Rule, Selector, Stylesheet};
pub use dom::{Attr, Dom, Node, NodeId, NodeKind};
pub use layout::{layout, layout_with, BoxType, Dimensions, LayoutBox, Rect, Replaced};
pub use paint::{paint, parse_color, DisplayItem};
pub use parser::parse;
pub use tokenizer::{tokenize, Token};

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    fn chars_of(tokens: &[Token]) -> String {
        tokens
            .iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    // ---- Tokenizer ----

    #[test]
    fn tokenize_simple_tag_and_text() {
        let t = tokenize("<p>Hello</p>");
        assert_eq!(t[0], Token::StartTag { name: "p".into(), attrs: Vec::new(), self_closing: false });
        assert_eq!(chars_of(&t), "Hello");
        assert_eq!(t[t.len() - 2], Token::EndTag { name: "p".into() });
        assert_eq!(t[t.len() - 1], Token::Eof);
    }

    #[test]
    fn tokenize_attributes_all_quoting_styles() {
        let t = tokenize(r#"<a href="x" title='y' data=z disabled>"#);
        if let Token::StartTag { name, attrs, .. } = &t[0] {
            assert_eq!(name, "a");
            assert_eq!(attrs.len(), 4);
            assert_eq!(attrs[0], Attr { name: "href".into(), value: "x".into() });
            assert_eq!(attrs[1], Attr { name: "title".into(), value: "y".into() });
            assert_eq!(attrs[2], Attr { name: "data".into(), value: "z".into() });
            assert_eq!(attrs[3], Attr { name: "disabled".into(), value: String::new() });
        } else {
            panic!("expected StartTag, got {:?}", t[0]);
        }
    }

    #[test]
    fn tokenize_self_closing_and_case_fold() {
        let t = tokenize("<IMG SRC='a.png'/>");
        assert_eq!(
            t[0],
            Token::StartTag {
                name: "img".into(),
                attrs: alloc::vec![Attr { name: "src".into(), value: "a.png".into() }],
                self_closing: true
            }
        );
    }

    #[test]
    fn tokenize_comment_and_doctype() {
        let t = tokenize("<!DOCTYPE html><!-- hi -->");
        assert_eq!(t[0], Token::Doctype { name: "html".into(), force_quirks: false });
        assert_eq!(t[1], Token::Comment(" hi ".into()));
    }

    #[test]
    fn tokenize_entities_in_text() {
        let t = tokenize("a &amp; b &lt;tag&gt; &#169; &#x20AC; &euro;");
        assert_eq!(chars_of(&t), "a & b <tag> © € €");
    }

    #[test]
    fn tokenize_entity_without_semicolon_legacy() {
        // &copy without ; is a legacy entity in text.
        let t = tokenize("\u{A9}: &copy 2026");
        assert_eq!(chars_of(&t), "©: © 2026");
    }

    #[test]
    fn tokenize_ambiguous_ampersand_left_literal() {
        // &notanentity; → '&' stays literal (no match).
        let t = tokenize("x &zzz; y");
        assert_eq!(chars_of(&t), "x &zzz; y");
    }

    #[test]
    fn tokenize_rawtext_script_keeps_markup() {
        let t = tokenize("<script>if (a<b && c>d) x()</script>after");
        // Inside <script> '<b' stays text; only </script> closes it.
        let txt = chars_of(&t);
        assert!(txt.contains("if (a<b && c>d) x()"), "script content lost: {txt:?}");
        assert!(txt.ends_with("after"));
        assert!(t.iter().any(|x| *x == Token::EndTag { name: "script".into() }));
        // No stray <b> start tag inside the script rawtext.
        assert!(!t.iter().any(|x| matches!(x, Token::StartTag { name, .. } if name == "b")));
    }

    #[test]
    fn tokenize_rcdata_title_decodes_entities() {
        let t = tokenize("<title>A &amp; B</title>");
        assert_eq!(chars_of(&t), "A & B");
    }

    #[test]
    fn tokenize_duplicate_attr_first_wins() {
        let t = tokenize(r#"<x a="1" a="2">"#);
        if let Token::StartTag { attrs, .. } = &t[0] {
            assert_eq!(attrs.len(), 1);
            assert_eq!(attrs[0].value, "1");
        } else {
            panic!();
        }
    }

    // ---- DOM / parser ----

    #[test]
    fn parse_nested_structure() {
        let dom = parse("<html><body><h1>Title</h1><p>Text</p></body></html>");
        assert_eq!(dom.count_tag("html"), 1);
        assert_eq!(dom.count_tag("body"), 1);
        assert_eq!(dom.count_tag("h1"), 1);
        assert_eq!(dom.count_tag("p"), 1);
        assert_eq!(dom.text_content(dom.root()), "TitleText");
    }

    #[test]
    fn parse_implicit_p_close() {
        // <p>a<p>b → two independent paragraphs, not nested.
        let dom = parse("<p>a<p>b");
        assert_eq!(dom.count_tag("p"), 2);
        // No p inside a p.
        for (i, n) in dom.nodes.iter().enumerate() {
            if dom.tag(i) == Some("p") {
                for &c in &n.children {
                    assert_ne!(dom.tag(c), Some("p"), "p nested in p");
                }
            }
        }
    }

    #[test]
    fn parse_list_items_autoclose() {
        let dom = parse("<ul><li>one<li>two<li>three</ul>");
        assert_eq!(dom.count_tag("li"), 3);
        // Each li is a direct child of ul (not nested).
        let ul = (0..dom.len()).find(|&i| dom.tag(i) == Some("ul")).unwrap();
        let li_children = dom.nodes[ul].children.iter().filter(|&&c| dom.tag(c) == Some("li")).count();
        assert_eq!(li_children, 3);
    }

    #[test]
    fn parse_void_elements_have_no_children() {
        let dom = parse("<div><img src='a'><br>text</div>");
        let img = (0..dom.len()).find(|&i| dom.tag(i) == Some("img")).unwrap();
        assert!(dom.nodes[img].children.is_empty());
        // 'text' hangs under div, not under br.
        let div = (0..dom.len()).find(|&i| dom.tag(i) == Some("div")).unwrap();
        assert_eq!(dom.text_content(div), "text");
    }

    #[test]
    fn parse_attributes_reachable() {
        let dom = parse(r#"<a href="https://euro-os.eu" class="btn">link</a>"#);
        let a = (0..dom.len()).find(|&i| dom.tag(i) == Some("a")).unwrap();
        assert_eq!(dom.attr(a, "href"), Some("https://euro-os.eu"));
        assert_eq!(dom.attr(a, "class"), Some("btn"));
        assert_eq!(dom.text_content(a), "link");
    }

    #[test]
    fn parse_mismatched_end_tag_ignored() {
        // </span> without an open span must not break the tree.
        let dom = parse("<div>x</span>y</div>");
        assert_eq!(dom.count_tag("div"), 1);
        let div = (0..dom.len()).find(|&i| dom.tag(i) == Some("div")).unwrap();
        assert_eq!(dom.text_content(div), "xy");
    }

    #[test]
    fn parse_realistic_fragment() {
        let html = r#"
            <!DOCTYPE html>
            <html lang="nl">
              <head><meta charset="utf-8"><title>EuroOS</title></head>
              <body>
                <header><h1>EuroOS &mdash; sovereign</h1></header>
                <main>
                  <p class="lead">Built from scratch in <strong>Rust</strong>.</p>
                  <ul><li>HTML</li><li>CSS</li><li>Layout</li></ul>
                </main>
                <!-- footer comes later -->
              </body>
            </html>"#;
        let dom = parse(html);
        assert_eq!(dom.count_tag("html"), 1);
        assert_eq!(dom.count_tag("title"), 1);
        assert_eq!(dom.count_tag("li"), 3);
        assert_eq!(dom.count_tag("strong"), 1);
        let h1 = (0..dom.len()).find(|&i| dom.tag(i) == Some("h1")).unwrap();
        assert_eq!(dom.text_content(h1), "EuroOS — sovereign");
        let html_el = (0..dom.len()).find(|&i| dom.tag(i) == Some("html")).unwrap();
        assert_eq!(dom.attr(html_el, "lang"), Some("nl"));
    }

    // ---- Robustness / stability: malicious & malformed input ----
    //
    // The engine runs in the kernel; IT MUST NEVER crash on bad input.
    // These tests drive the FULL pipeline (parse → compute → layout → paint)
    // through pathological pages and require: no panic, bounded output.

    fn full_pipeline(html: &str, css: &str) -> usize {
        let dom = parse(html);
        let sheet = parse_stylesheet(css);
        let styles = compute(&dom, &[&sheet]);
        let root = layout(&dom, &styles, 1280.0);
        let items = paint(&dom, &styles, &root);
        items.len()
    }

    /// Deeply nested `<div>` (would blow the kernel stack without a depth bound).
    #[test]
    fn robust_deeply_nested_does_not_overflow() {
        let mut html = String::new();
        for _ in 0..20_000 {
            html.push_str("<div>");
        }
        html.push_str("deep");
        for _ in 0..20_000 {
            html.push_str("</div>");
        }
        // Must not panic/overflow; the layout tree is bounded by MAX_DEPTH.
        let _ = full_pipeline(&html, "div{color:#222;padding:1px}");
        // The DOM does contain all nodes (the parser is iterative), but build_box
        // does not descend deeper than MAX_DEPTH.
        let dom = parse(&html);
        assert!(dom.len() >= 20_000, "parser must create all nodes");
    }

    /// Deeply nested text node via text_content (own depth bound).
    #[test]
    fn robust_text_content_deep_no_overflow() {
        let mut html = String::new();
        for _ in 0..5_000 {
            html.push_str("<span>");
        }
        html.push('x');
        let dom = parse(&html);
        let root = 0;
        let _ = dom.text_content(root); // must not overflow
    }

    /// Malformed / unclosed / junk markup.
    #[test]
    fn robust_malformed_markup() {
        let cases = [
            "<<<>>><p<<<",
            "<div class=\"unterminated",
            "<p>text</nonexistent></p></p></p>",
            "<a href=>&&&;&#;&#x;&zzz;",
            "</div></div></div>",
            "<!-- unclosed comment",
            "<script>unterminated",
            "<style>body{color: ; ;;; } broken",
            "",
            "<>",
            "&#999999999;&#xFFFFFFFF;", // out-of-range code points
        ];
        for c in cases {
            let _ = full_pipeline(c, "* { margin: 1px }");
        }
    }

    /// Malformed CSS must not let the cascade crash.
    #[test]
    fn robust_malformed_css() {
        let css = "}}}{{{ : ; color color color ;; @媒体 { x } .a..b###{} div > > p {} *{padding:-99999px}";
        let _ = full_pipeline("<div class='a'><p>hi</p></div>", css);
    }

    /// Lots of siblings (width instead of depth) — bounded, but large, input.
    #[test]
    fn robust_many_siblings() {
        let mut html = String::from("<body>");
        for i in 0..10_000 {
            html.push_str("<p>item ");
            // vary the content so they are not identical strings
            html.push_str(if i % 2 == 0 { "even" } else { "odd" });
            html.push_str("</p>");
        }
        html.push_str("</body>");
        let n = full_pipeline(&html, "p{font-size:14px}");
        assert!(n > 0, "must produce display items");
    }

    /// Long unsplit text and odd unicode must not panic.
    #[test]
    fn robust_long_unicode_text() {
        let mut html = String::from("<p>");
        for _ in 0..2_000 {
            html.push_str("word\u{200B}\u{00A0}€中文🇪🇺 ");
        }
        html.push_str("</p>");
        let _ = full_pipeline(&html, "p{}");
    }
}
